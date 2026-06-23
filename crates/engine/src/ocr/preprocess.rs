//! Pure-Rust image preprocessing — **the OCR quality lever**.
//!
//! Tesseract (and any OCR engine) recognizes a *deskewed, binarized, denoised*
//! image far better than a raw scan. Every step here is pure Rust over the
//! single-channel [`OcrImage`] luminance buffer, deterministic, and individually
//! skippable via [`PreprocessConfig`].
//!
//! Pipeline order (each optional):
//! 1. **Deskew** — find the page's skew angle by maximizing the variance of the
//!    horizontal ink-projection profile over candidate angles, then rotate the
//!    image flat. Skewed text lines wreck line segmentation; this is high-impact.
//! 2. **Binarize** — collapse to black/white. [`Binarization::Otsu`] is a global
//!    threshold (fast, good on clean scans); [`Binarization::Sauvola`] is a local
//!    adaptive threshold (good on uneven lighting / phone-photo shadows).
//! 3. **Denoise** — remove isolated speckle so noise is not read as punctuation.
//!
//! DPI normalization is handled *upstream* by rendering the page at the target
//! DPI directly (the renderer rasterizes at any DPI), which is higher quality
//! than upscaling a small embedded image — so there is no resampling step here.
//!
//! # Determinism
//!
//! No `Date`/random; every angle scan and threshold is a pure function of the
//! input bytes. Same image → byte-identical output.

use super::OcrImage;

/// Which binarization to apply.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Binarization {
    /// No binarization (leave grayscale). Some engines binarize internally; this
    /// lets a caller defer to them.
    None,
    /// Global Otsu threshold from the luminance histogram. Best on clean,
    /// evenly-lit scans.
    #[default]
    Otsu,
    /// Local adaptive Sauvola threshold over a window. Best on uneven lighting,
    /// shadows, and phone photos. `window` is the side length (odd, in pixels);
    /// `k` is the Sauvola sensitivity (typically 0.2–0.5; higher → more
    /// aggressive thresholding toward white).
    Sauvola { window: u32, k: f64 },
}

/// Preprocessing knobs. Defaults are a sane scan pipeline (deskew + Otsu +
/// light denoise).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreprocessConfig {
    /// Detect and correct page skew before binarization.
    pub deskew: bool,
    /// Largest absolute skew angle (degrees) searched during deskew.
    pub max_skew_deg: f64,
    /// Angular step (degrees) of the deskew search.
    pub skew_step_deg: f64,
    pub binarization: Binarization,
    /// Remove isolated speckle after binarization (3×3 majority on B/W).
    pub denoise: bool,
}

impl Default for PreprocessConfig {
    fn default() -> Self {
        PreprocessConfig {
            deskew: true,
            max_skew_deg: 10.0,
            skew_step_deg: 0.5,
            binarization: Binarization::Otsu,
            denoise: true,
        }
    }
}

impl PreprocessConfig {
    /// A config that performs no work (used to OCR a raw image, or to test the
    /// pipeline is a no-op when disabled).
    pub fn passthrough() -> Self {
        PreprocessConfig {
            deskew: false,
            max_skew_deg: 0.0,
            skew_step_deg: 1.0,
            binarization: Binarization::None,
            denoise: false,
        }
    }
}

/// Run the configured preprocessing pipeline, returning the cleaned image and
/// the detected skew angle (degrees, `0.0` when deskew is off).
pub fn preprocess(img: &OcrImage, cfg: &PreprocessConfig) -> (OcrImage, f64) {
    let mut out = img.clone();
    let mut angle = 0.0;

    if cfg.deskew && img.is_valid() {
        angle = detect_skew(&out, cfg.max_skew_deg, cfg.skew_step_deg);
        if angle.abs() > f64::EPSILON {
            out = rotate(&out, angle);
        }
    }

    out = match cfg.binarization {
        Binarization::None => out,
        Binarization::Otsu => binarize_otsu(&out),
        Binarization::Sauvola { window, k } => binarize_sauvola(&out, window, k),
    };

    if cfg.denoise {
        out = denoise_speckle(&out);
    }

    (out, angle)
}

// ── deskew ───────────────────────────────────────────────────────────────────

/// Detect the skew angle (degrees, positive = the image is rotated clockwise and
/// must be rotated counter-clockwise to correct) by maximizing the variance of
/// the row ink-projection profile over candidate angles.
///
/// Intuition: when text lines are horizontal, each row is either mostly ink (a
/// line) or mostly blank (a gap), so the row-sum profile has high variance. Skew
/// smears ink across rows and flattens the profile. The angle whose profile has
/// the highest variance is the deskew angle.
pub fn detect_skew(img: &OcrImage, max_deg: f64, step_deg: f64) -> f64 {
    if !img.is_valid() || max_deg <= 0.0 {
        return 0.0;
    }
    // Work on a coarse ink mask: dark pixels (below mid-gray) are "ink".
    let thresh = otsu_threshold(&histogram(img));
    let step = if step_deg > 0.0 { step_deg } else { 0.5 };
    let mut best_angle = 0.0f64;
    let mut best_score = f64::NEG_INFINITY;

    let mut a = -max_deg;
    while a <= max_deg + 1e-9 {
        let score = projection_variance(img, thresh, a);
        // Prefer the smaller absolute angle on a tie for stability/determinism.
        if score > best_score || (score == best_score && a.abs() < best_angle.abs()) {
            best_score = score;
            best_angle = a;
        }
        a += step;
    }
    best_angle
}

/// Variance of the per-row ink count when the image is conceptually rotated by
/// `angle_deg`. We do not rotate the buffer; we accumulate each ink pixel into
/// the destination row it would map to, which is cheap and exact enough for
/// profile variance.
fn projection_variance(img: &OcrImage, ink_thresh: u8, angle_deg: f64) -> f64 {
    let w = img.width as i64;
    let h = img.height as i64;
    let theta = angle_deg.to_radians();
    let (sin, cos) = theta.sin_cos();
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let mut rows = vec![0u32; img.height as usize];

    // Sub-sample for speed on large pages: stride keeps ~512 rows/cols max.
    let sx = (w / 512).max(1);
    let sy = (h / 512).max(1);

    let mut y = 0i64;
    while y < h {
        let mut x = 0i64;
        while x < w {
            if img.get(x, y) <= ink_thresh {
                // The destination row of (x,y) after rotating the content by
                // +theta about the centre (same convention as `rotate`): the
                // angle that maximizes this profile's variance is exactly the
                // correction angle to hand to `rotate`.
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                let ry = dx * sin + dy * cos + cy;
                let ri = ry.round();
                if ri >= 0.0 && (ri as i64) < h {
                    rows[ri as usize] = rows[ri as usize].saturating_add(1);
                }
            }
            x += sx;
        }
        y += sy;
    }

    // Variance of the row-ink profile.
    let n = rows.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mean = rows.iter().map(|&v| v as f64).sum::<f64>() / n;
    rows.iter()
        .map(|&v| {
            let d = v as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n
}

/// Rotate the image content by `angle_deg` about its centre (same sign
/// convention as [`detect_skew`], so a detected angle can be passed straight
/// here to correct the skew), sampling nearest-neighbor and filling exposed
/// corners with white. Output keeps the same dimensions.
pub fn rotate(img: &OcrImage, angle_deg: f64) -> OcrImage {
    if !img.is_valid() || angle_deg.abs() <= f64::EPSILON {
        return img.clone();
    }
    let w = img.width as i64;
    let h = img.height as i64;
    // Forward-rotate content by +angle ⇒ inverse-sample each destination from
    // the source rotated by −angle (R(−a)·dest).
    let a = angle_deg.to_radians();
    let (sin, cos) = a.sin_cos();
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let mut out = vec![255u8; (w * h) as usize];

    for dy in 0..h {
        for dx in 0..w {
            let ox = dx as f64 - cx;
            let oy = dy as f64 - cy;
            // R(−a): [ cos  sin; −sin  cos ]
            let sx = ox * cos + oy * sin + cx;
            let sy = -ox * sin + oy * cos + cy;
            let srx = sx.round() as i64;
            let sry = sy.round() as i64;
            out[(dy * w + dx) as usize] = img.get(srx, sry);
        }
    }
    OcrImage {
        width: img.width,
        height: img.height,
        gray: out,
    }
}

// ── binarization ───────────────────────────────────────────────────────────

fn histogram(img: &OcrImage) -> [u32; 256] {
    let mut h = [0u32; 256];
    for &v in &img.gray {
        h[v as usize] += 1;
    }
    h
}

/// Otsu's method: the threshold maximizing between-class variance of the
/// luminance histogram. Returned as the cut value `t` (pixels `<= t` are ink).
pub fn otsu_threshold(hist: &[u32; 256]) -> u8 {
    let total: u64 = hist.iter().map(|&c| c as u64).sum();
    if total == 0 {
        return 127;
    }
    let sum_all: u64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as u64 * c as u64)
        .sum();

    let mut w_back = 0u64;
    let mut sum_back = 0u64;
    let mut best_t = 0usize;
    let mut best_var = -1.0f64;

    for (t, &count) in hist.iter().enumerate() {
        w_back += count as u64;
        if w_back == 0 {
            continue;
        }
        let w_fore = total - w_back;
        if w_fore == 0 {
            break;
        }
        sum_back += t as u64 * count as u64;
        let mean_back = sum_back as f64 / w_back as f64;
        let mean_fore = (sum_all - sum_back) as f64 / w_fore as f64;
        let between = w_back as f64 * w_fore as f64 * (mean_back - mean_fore).powi(2);
        if between > best_var {
            best_var = between;
            best_t = t;
        }
    }
    best_t as u8
}

/// Global Otsu binarization → pure black (`0`, ink) / white (`255`).
pub fn binarize_otsu(img: &OcrImage) -> OcrImage {
    let t = otsu_threshold(&histogram(img));
    let gray = img
        .gray
        .iter()
        .map(|&v| if v <= t { 0 } else { 255 })
        .collect();
    OcrImage {
        width: img.width,
        height: img.height,
        gray,
    }
}

/// Sauvola local adaptive binarization. For each pixel, the threshold is
/// `mean * (1 + k * (stddev / 128 - 1))` over a `window×window` neighborhood,
/// computed in O(1) per pixel via integral images of the values and their
/// squares. Robust to uneven illumination.
pub fn binarize_sauvola(img: &OcrImage, window: u32, k: f64) -> OcrImage {
    if !img.is_valid() {
        return img.clone();
    }
    let w = img.width as usize;
    let h = img.height as usize;
    let win = (window.max(3) | 1) as usize; // force odd, >=3
    let r = (win / 2) as i64;

    // Integral images (sum and sum of squares), size (w+1) x (h+1), i64/f64.
    let iw = w + 1;
    let mut isum = vec![0i64; iw * (h + 1)];
    let mut isq = vec![0f64; iw * (h + 1)];
    for y in 0..h {
        let mut row_sum = 0i64;
        let mut row_sq = 0f64;
        for x in 0..w {
            let v = img.gray[y * w + x] as i64;
            row_sum += v;
            row_sq += (v * v) as f64;
            let idx = (y + 1) * iw + (x + 1);
            isum[idx] = isum[y * iw + (x + 1)] + row_sum;
            isq[idx] = isq[y * iw + (x + 1)] + row_sq;
        }
    }
    let rect = |arr_sum: &[i64], x0: i64, y0: i64, x1: i64, y1: i64| -> i64 {
        let x0 = x0.clamp(0, w as i64) as usize;
        let y0 = y0.clamp(0, h as i64) as usize;
        let x1 = (x1 + 1).clamp(0, w as i64) as usize;
        let y1 = (y1 + 1).clamp(0, h as i64) as usize;
        arr_sum[y1 * iw + x1] - arr_sum[y0 * iw + x1] - arr_sum[y1 * iw + x0]
            + arr_sum[y0 * iw + x0]
    };
    let rectf = |arr: &[f64], x0: i64, y0: i64, x1: i64, y1: i64| -> f64 {
        let x0 = x0.clamp(0, w as i64) as usize;
        let y0 = y0.clamp(0, h as i64) as usize;
        let x1 = (x1 + 1).clamp(0, w as i64) as usize;
        let y1 = (y1 + 1).clamp(0, h as i64) as usize;
        arr[y1 * iw + x1] - arr[y0 * iw + x1] - arr[y1 * iw + x0] + arr[y0 * iw + x0]
    };

    let mut gray = vec![255u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let x0 = x as i64 - r;
            let y0 = y as i64 - r;
            let x1 = x as i64 + r;
            let y1 = y as i64 + r;
            let cx0 = x0.clamp(0, w as i64 - 1);
            let cy0 = y0.clamp(0, h as i64 - 1);
            let cx1 = x1.clamp(0, w as i64 - 1);
            let cy1 = y1.clamp(0, h as i64 - 1);
            let count = ((cx1 - cx0 + 1) * (cy1 - cy0 + 1)) as f64;
            let s = rect(&isum, cx0, cy0, cx1, cy1) as f64;
            let sq = rectf(&isq, cx0, cy0, cx1, cy1);
            let mean = s / count;
            let var = (sq / count - mean * mean).max(0.0);
            let std = var.sqrt();
            let t = mean * (1.0 + k * (std / 128.0 - 1.0));
            let v = img.gray[y * w + x] as f64;
            gray[y * w + x] = if v <= t { 0 } else { 255 };
        }
    }
    OcrImage {
        width: img.width,
        height: img.height,
        gray,
    }
}

// ── denoise ──────────────────────────────────────────────────────────────────

/// Remove isolated speckle from a (binary) image with a 3×3 majority filter:
/// a black pixel surrounded by mostly white neighbors is flipped to white, and
/// vice-versa. On a clean binary image this erases single-pixel noise without
/// eroding strokes (which have a majority of same-colored neighbors).
pub fn denoise_speckle(img: &OcrImage) -> OcrImage {
    if !img.is_valid() {
        return img.clone();
    }
    let w = img.width as i64;
    let h = img.height as i64;
    let mut gray = img.gray.clone();
    for y in 0..h {
        for x in 0..w {
            let mut black = 0;
            let mut total = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    total += 1;
                    if img.get(x + dx, y + dy) < 128 {
                        black += 1;
                    }
                }
            }
            let center_black = img.get(x, y) < 128;
            // Majority of the 8 neighbors.
            let neighbor_black_majority = black * 2 > total;
            let idx = (y * w + x) as usize;
            if center_black && !neighbor_black_majority && black <= 1 {
                gray[idx] = 255; // isolated black speckle → white
            } else if !center_black && neighbor_black_majority && black >= 7 {
                gray[idx] = 0; // isolated white hole → black
            }
        }
    }
    OcrImage {
        width: img.width,
        height: img.height,
        gray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a synthetic page: black horizontal text lines on white, optionally
    /// skewed by `skew_deg`. Returns the image.
    fn synthetic_lines(w: u32, h: u32, skew_deg: f64) -> OcrImage {
        let mut g = vec![255u8; (w * h) as usize];
        let theta = skew_deg.to_radians();
        let slope = theta.tan();
        // Draw a black "line" of text every 20 px, 6 px tall.
        for line_top in (20..h.saturating_sub(20)).step_by(20) {
            for x in 10..(w - 10) {
                let yshift = (slope * (x as f64 - w as f64 / 2.0)).round() as i64;
                for ty in 0..6i64 {
                    let yy = line_top as i64 + ty + yshift;
                    if yy >= 0 && yy < h as i64 {
                        g[(yy as u32 * w + x) as usize] = 0;
                    }
                }
            }
        }
        OcrImage {
            width: w,
            height: h,
            gray: g,
        }
    }

    #[test]
    fn deskew_detects_known_angle() {
        // Lines tilted by +4° need a −4° correction; `detect_skew` returns the
        // correction angle (the value to hand to `rotate`), so ~−4° is expected.
        let img = synthetic_lines(300, 300, 4.0);
        let angle = detect_skew(&img, 10.0, 0.5);
        assert!(
            (angle + 4.0).abs() <= 1.0,
            "expected ~−4° correction, got {angle}"
        );
        // Sanity: a −3° tilt needs a +3° correction.
        let img2 = synthetic_lines(300, 300, -3.0);
        let angle2 = detect_skew(&img2, 10.0, 0.5);
        assert!(
            (angle2 - 3.0).abs() <= 1.0,
            "expected ~+3° correction, got {angle2}"
        );
    }

    #[test]
    fn deskew_zero_on_straight_lines() {
        let img = synthetic_lines(300, 300, 0.0);
        let angle = detect_skew(&img, 10.0, 0.5);
        assert!(angle.abs() <= 1.0, "expected ~0°, got {angle}");
    }

    #[test]
    fn rotate_is_near_inverse_of_skew() {
        // Skew by 4°, deskew, and confirm the projection variance improves
        // (lines become more horizontal).
        let img = synthetic_lines(300, 300, 4.0);
        let t = otsu_threshold(&histogram(&img));
        let before = projection_variance(&img, t, 0.0);
        let angle = detect_skew(&img, 10.0, 0.5);
        let fixed = rotate(&img, angle);
        let after = projection_variance(&fixed, t, 0.0);
        assert!(
            after > before,
            "deskew should raise horizontal-profile variance: {before} -> {after}"
        );
    }

    #[test]
    fn otsu_threshold_on_bimodal_gradient() {
        // Half the pixels dark (~40), half bright (~210): threshold should land
        // between the two clusters.
        let mut g = vec![40u8; 5000];
        g.extend(vec![210u8; 5000]);
        let img = OcrImage {
            width: 100,
            height: 100,
            gray: g,
        };
        let t = otsu_threshold(&histogram(&img));
        assert!((40..210).contains(&t), "threshold {t} not between clusters");
    }

    #[test]
    fn otsu_binarize_produces_pure_bw() {
        let img = synthetic_lines(100, 100, 0.0);
        let b = binarize_otsu(&img);
        assert!(b.gray.iter().all(|&v| v == 0 || v == 255));
        // Some ink and some background must survive.
        assert!(b.gray.contains(&0));
        assert!(b.gray.contains(&255));
    }

    #[test]
    fn sauvola_handles_uneven_lighting() {
        // A page with a dark gradient (left dark, right bright) and a constant-
        // contrast text stroke. A global threshold would lose the dark side;
        // Sauvola should keep ink visible across the whole width.
        let w = 120u32;
        let h = 60u32;
        let mut g = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                // Background ramp 90..230 across x: the left half is darker than
                // a *global* mid-gray threshold would tolerate, the right half
                // brighter — so one global cut must lose ink on one side.
                let bg = 90 + (x * 140 / w) as u8;
                // A horizontal stroke with strong *local* contrast (much darker
                // than its surrounding background), as real ink-on-paper is.
                let ink = (10..50).contains(&y);
                let v = if ink { bg.saturating_sub(80) } else { bg };
                g[(y * w + x) as usize] = v;
            }
        }
        let img = OcrImage {
            width: w,
            height: h,
            gray: g,
        };
        let b = binarize_sauvola(&img, 25, 0.2);
        let has_ink = |g: &[u8], xr: std::ops::Range<u32>| {
            (10..50).any(|y| xr.clone().any(|x| g[(y * w + x) as usize] == 0))
        };
        // Sauvola keeps the stroke on BOTH the dark (left) and bright (right)
        // sides — the property a single global threshold cannot achieve here.
        assert!(has_ink(&b.gray, 0..20), "no ink recovered on the dark side");
        assert!(
            has_ink(&b.gray, 100..120),
            "no ink recovered on the bright side"
        );

        // Demonstrate the failure mode Sauvola fixes: a global Otsu threshold
        // loses the stroke on at least one side of this uneven-lit page.
        let global = binarize_otsu(&img);
        let global_both = has_ink(&global.gray, 0..20) && has_ink(&global.gray, 100..120);
        assert!(
            !global_both,
            "global Otsu unexpectedly recovered ink on both sides; test no longer \
             exercises the adaptive advantage"
        );
    }

    #[test]
    fn denoise_removes_injected_speckle() {
        let mut img = OcrImage::white(50, 50);
        // Inject isolated black speckles on a white field.
        for &(x, y) in &[(5u32, 5u32), (20, 31), (44, 10), (12, 40)] {
            img.gray[(y * 50 + x) as usize] = 0;
        }
        let before = img.gray.iter().filter(|&&v| v == 0).count();
        assert_eq!(before, 4);
        let cleaned = denoise_speckle(&img);
        let after = cleaned.gray.iter().filter(|&&v| v == 0).count();
        assert_eq!(after, 0, "all isolated speckle should be removed");
    }

    #[test]
    fn denoise_preserves_strokes() {
        // A solid 10x10 black block must survive denoise (its interior pixels
        // have a black-neighbor majority).
        let w = 30u32;
        let mut img = OcrImage::white(w, 30);
        for y in 10..20 {
            for x in 10..20 {
                img.gray[(y * w + x) as usize] = 0;
            }
        }
        let cleaned = denoise_speckle(&img);
        // The centre of the block stays black.
        assert_eq!(cleaned.gray[(15 * w + 15) as usize], 0);
    }

    #[test]
    fn passthrough_is_noop_modulo_clone() {
        let img = synthetic_lines(64, 64, 0.0);
        let (out, angle) = preprocess(&img, &PreprocessConfig::passthrough());
        assert_eq!(out, img);
        assert_eq!(angle, 0.0);
    }

    #[test]
    fn full_pipeline_is_deterministic() {
        let img = synthetic_lines(120, 120, 3.0);
        let cfg = PreprocessConfig::default();
        let (a, aa) = preprocess(&img, &cfg);
        let (b, bb) = preprocess(&img, &cfg);
        assert_eq!(a, b);
        assert_eq!(aa, bb);
    }
}
