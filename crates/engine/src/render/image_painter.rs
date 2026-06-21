use crate::images::decoder::RawImage;
use crate::render::buffer::PixelBuffer;
use crate::render::transform::{Transform2D, Viewport};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmoothMode {
    None,
    Interpolate,
    LegacyBilinear,
}

pub struct ImagePainter;

impl ImagePainter {
    /// Paint a decoded image onto the buffer.
    pub fn paint_image(
        buf: &mut PixelBuffer,
        image: &RawImage,
        ctm: &Transform2D,
        viewport: &Viewport,
    ) {
        Self::paint_image_with_options(buf, image, ctm, viewport, false);
    }

    /// Paint with optional `/Interpolate` smoothing when the image is magnified.
    /// PDF's default is nearest-neighbour for magnification; when interpolation
    /// is requested, smooth photographic upscaling is used instead. Downscaling
    /// always integrates the source footprint with deterministic sRGB-space area
    /// averaging, matching the default proof renderer convention.
    pub fn paint_image_with_options(
        buf: &mut PixelBuffer,
        image: &RawImage,
        ctm: &Transform2D,
        viewport: &Viewport,
        interpolate: bool,
    ) {
        let mode = if interpolate {
            SmoothMode::Interpolate
        } else {
            SmoothMode::None
        };
        Self::paint_image_with_mode(buf, image, ctm, viewport, mode);
    }

    /// Preserve the older JPX compatibility path where Poppler smooths some
    /// magnified JPXDecode images even without an explicit `/Interpolate true`.
    pub(crate) fn paint_image_with_jpx_compat(
        buf: &mut PixelBuffer,
        image: &RawImage,
        ctm: &Transform2D,
        viewport: &Viewport,
    ) {
        Self::paint_image_with_mode(buf, image, ctm, viewport, SmoothMode::LegacyBilinear);
    }

    fn paint_image_with_mode(
        buf: &mut PixelBuffer,
        image: &RawImage,
        ctm: &Transform2D,
        viewport: &Viewport,
        smooth_mode: SmoothMode,
    ) {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return;
        }
        if ctm.determinant().abs() < 1e-10 {
            log::warn!("ImagePainter: singular transform, skipping image");
            return;
        }

        let vp_transform = viewport.to_transform();
        let combined = ctm.concat(&vp_transform);

        if ctm.is_axis_aligned() {
            Self::paint_axis_aligned(buf, image, &combined, smooth_mode);
        } else {
            Self::paint_affine(buf, image, &combined, smooth_mode);
        }
    }

    /// Decide whether a paint is magnifying the source. The PDF default
    /// (`/Interpolate false`) keeps magnification nearest-neighbour so small
    /// pixel art and masks remain crisp.
    fn magnifying(image: &RawImage, dst_w: f64, dst_h: f64) -> bool {
        // Magnifying when each source pixel covers more than one destination
        // pixel on either axis (dst extent exceeds source extent).
        dst_w >= image.width as f64 && dst_h >= image.height as f64
    }

    fn sample(
        image: &RawImage,
        u: f64,
        v: f64,
        footprint_x: f64,
        footprint_y: f64,
        smooth_mode: SmoothMode,
    ) -> [u8; 4] {
        if footprint_x > 1.0 || footprint_y > 1.0 {
            Self::area_average_sample(image, u, v, footprint_x.max(1.0), footprint_y.max(1.0))
        } else {
            match smooth_mode {
                SmoothMode::None => Self::nearest_sample(image, u, v),
                SmoothMode::Interpolate => Self::interpolated_sample(image, u, v),
                SmoothMode::LegacyBilinear => Self::bilinear_sample(image, u, v),
            }
        }
    }

    fn paint_axis_aligned(
        buf: &mut PixelBuffer,
        image: &RawImage,
        combined: &Transform2D,
        smooth_mode: SmoothMode,
    ) {
        let corners = [
            combined.transform_point(0.0, 0.0),
            combined.transform_point(1.0, 0.0),
            combined.transform_point(0.0, 1.0),
            combined.transform_point(1.0, 1.0),
        ];
        let (px_min, px_max, py_min, py_max) = bounding_box(&corners);
        if !px_min.is_finite() || !px_max.is_finite() || !py_min.is_finite() || !py_max.is_finite()
        {
            return;
        }

        let dst_w = (px_max - px_min).max(1.0);
        let dst_h = (py_max - py_min).max(1.0);
        let footprint_x = image.width as f64 / dst_w;
        let footprint_y = image.height as f64 / dst_h;
        let smooth = if Self::magnifying(image, dst_w, dst_h) {
            smooth_mode
        } else if matches!(smooth_mode, SmoothMode::None) {
            SmoothMode::LegacyBilinear
        } else {
            smooth_mode
        };
        let (x0, x1, y0, y1) = clipped_bounds(buf, px_min, px_max, py_min, py_max);
        if x0 > x1 || y0 > y1 {
            return;
        }

        for py in y0..=y1 {
            for px in x0..=x1 {
                let u = (px as f64 + 0.5 - px_min) / dst_w;
                let v = (py as f64 + 0.5 - py_min) / dst_h;
                let sample = Self::sample(image, u, v, footprint_x, footprint_y, smooth);
                let coverage = if image.channels == 4 {
                    sample[3] as f32 / 255.0
                } else {
                    1.0
                };
                buf.blend_pixel(px, py, [sample[0], sample[1], sample[2], 255], coverage);
            }
        }
    }

    fn paint_affine(
        buf: &mut PixelBuffer,
        image: &RawImage,
        combined: &Transform2D,
        smooth_mode: SmoothMode,
    ) {
        let inv = match combined.inverse() {
            Some(matrix) => matrix,
            None => {
                log::warn!("ImagePainter: singular transform, skipping image");
                return;
            }
        };

        let corners = [
            combined.transform_point(0.0, 0.0),
            combined.transform_point(1.0, 0.0),
            combined.transform_point(1.0, 1.0),
            combined.transform_point(0.0, 1.0),
        ];
        let (px_min, px_max, py_min, py_max) = bounding_box(&corners);
        if !px_min.is_finite() || !px_max.is_finite() || !py_min.is_finite() || !py_max.is_finite()
        {
            return;
        }

        let dst_w = (px_max - px_min).max(1.0);
        let dst_h = (py_max - py_min).max(1.0);
        let (footprint_x, footprint_y) = source_footprint_from_inverse(&inv, image);
        let smooth = if Self::magnifying(image, dst_w, dst_h) {
            smooth_mode
        } else if matches!(smooth_mode, SmoothMode::None) {
            SmoothMode::LegacyBilinear
        } else {
            smooth_mode
        };
        let (x0, x1, y0, y1) = clipped_bounds(buf, px_min, px_max, py_min, py_max);
        if x0 > x1 || y0 > y1 {
            return;
        }

        for py in y0..=y1 {
            for px in x0..=x1 {
                let (u, v) = inv.transform_point(px as f64 + 0.5, py as f64 + 0.5);
                if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                    continue;
                }

                let sample = Self::sample(image, u, v, footprint_x, footprint_y, smooth);
                let coverage = if image.channels == 4 {
                    sample[3] as f32 / 255.0
                } else {
                    1.0
                };
                buf.blend_pixel(px, py, [sample[0], sample[1], sample[2], 255], coverage);
            }
        }
    }

    /// Nearest-neighbour sample at normalized coords. Used when magnifying so a
    /// small source image renders as crisp blocks (the PDF/Poppler default).
    pub fn nearest_sample(image: &RawImage, u: f64, v: f64) -> [u8; 4] {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return [0, 0, 0, 0];
        }
        let w = image.width as f64;
        let h = image.height as f64;
        // Map [0,1) across the pixel grid and pick the covering source pixel.
        let x = (u * w).floor().clamp(0.0, w - 1.0) as usize;
        let y = (v * h).floor().clamp(0.0, h - 1.0) as usize;
        Self::get_pixel_channels(image, x, y)
    }

    /// Sample image at normalized coordinates using bilinear interpolation.
    pub fn bilinear_sample(image: &RawImage, u: f64, v: f64) -> [u8; 4] {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return [0, 0, 0, 0];
        }

        let w = image.width as f64;
        let h = image.height as f64;
        let sx = (u * (w - 1.0)).clamp(0.0, (w - 1.0).max(0.0));
        let sy = (v * (h - 1.0)).clamp(0.0, (h - 1.0).max(0.0));

        let x0 = sx.floor() as usize;
        let y0 = sy.floor() as usize;
        let x1 = (x0 + 1).min(image.width.saturating_sub(1) as usize);
        let y1 = (y0 + 1).min(image.height.saturating_sub(1) as usize);
        let fx = (sx - x0 as f64) as f32;
        let fy = (sy - y0 as f64) as f32;

        let p00 = Self::get_pixel_channels(image, x0, y0);
        let p10 = Self::get_pixel_channels(image, x1, y0);
        let p01 = Self::get_pixel_channels(image, x0, y1);
        let p11 = Self::get_pixel_channels(image, x1, y1);

        let lerp2 = |v00: u8, v10: u8, v01: u8, v11: u8| -> u8 {
            let top = v00 as f32 * (1.0 - fx) + v10 as f32 * fx;
            let bottom = v01 as f32 * (1.0 - fx) + v11 as f32 * fx;
            (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
        };

        [
            lerp2(p00[0], p10[0], p01[0], p11[0]),
            lerp2(p00[1], p10[1], p01[1], p11[1]),
            lerp2(p00[2], p10[2], p01[2], p11[2]),
            lerp2(p00[3], p10[3], p01[3], p11[3]),
        ]
    }

    /// Source-footprint area averaging in default sRGB byte space. This is used
    /// for minification so high-resolution scans and photos are integrated
    /// instead of undersampled by a single bilinear lookup.
    pub fn area_average_sample(
        image: &RawImage,
        u: f64,
        v: f64,
        footprint_x: f64,
        footprint_y: f64,
    ) -> [u8; 4] {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return [0, 0, 0, 0];
        }

        let w = image.width as f64;
        let h = image.height as f64;
        let cx = (u * w).clamp(0.0, w);
        let cy = (v * h).clamp(0.0, h);
        let half_w = (footprint_x.max(1e-6) * 0.5).min(w * 0.5);
        let half_h = (footprint_y.max(1e-6) * 0.5).min(h * 0.5);
        let x0 = (cx - half_w).clamp(0.0, w);
        let x1 = (cx + half_w).clamp(0.0, w);
        let y0 = (cy - half_h).clamp(0.0, h);
        let y1 = (cy + half_h).clamp(0.0, h);

        if x1 <= x0 || y1 <= y0 {
            return Self::nearest_sample(image, u, v);
        }

        let ix0 = x0.floor().max(0.0) as usize;
        let ix1 = x1.ceil().min(w) as usize;
        let iy0 = y0.floor().max(0.0) as usize;
        let iy1 = y1.ceil().min(h) as usize;

        let mut accum = [0.0_f64; 4];
        let mut total = 0.0_f64;
        for y in iy0..iy1 {
            let oy = ((y + 1) as f64).min(y1) - (y as f64).max(y0);
            if oy <= 0.0 {
                continue;
            }
            for x in ix0..ix1 {
                let ox = ((x + 1) as f64).min(x1) - (x as f64).max(x0);
                if ox <= 0.0 {
                    continue;
                }
                let weight = ox * oy;
                let px = Self::get_pixel_channels(image, x, y);
                for c in 0..4 {
                    accum[c] += px[c] as f64 * weight;
                }
                total += weight;
            }
        }

        if total <= 0.0 {
            return Self::nearest_sample(image, u, v);
        }

        [
            (accum[0] / total).round().clamp(0.0, 255.0) as u8,
            (accum[1] / total).round().clamp(0.0, 255.0) as u8,
            (accum[2] / total).round().clamp(0.0, 255.0) as u8,
            (accum[3] / total).round().clamp(0.0, 255.0) as u8,
        ]
    }

    /// Poppler-compatible smooth sample for `/Interpolate true` magnification.
    /// Uses edge-oriented bilinear coordinates, matching how Poppler spreads a
    /// 2-pixel image across the first two source-cell extents.
    pub fn interpolated_sample(image: &RawImage, u: f64, v: f64) -> [u8; 4] {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return [0, 0, 0, 0];
        }

        let w = image.width as f64;
        let h = image.height as f64;
        let sx = (u * w).clamp(0.0, (w - 1.0).max(0.0));
        let sy = (v * h).clamp(0.0, (h - 1.0).max(0.0));
        let x0 = sx.floor() as usize;
        let y0 = sy.floor() as usize;
        let x1 = (x0 + 1).min(image.width.saturating_sub(1) as usize);
        let y1 = (y0 + 1).min(image.height.saturating_sub(1) as usize);
        let fx = sx - x0 as f64;
        let fy = sy - y0 as f64;

        let p00 = Self::get_pixel_channels(image, x0, y0);
        let p10 = Self::get_pixel_channels(image, x1, y0);
        let p01 = Self::get_pixel_channels(image, x0, y1);
        let p11 = Self::get_pixel_channels(image, x1, y1);

        let lerp2 = |v00: u8, v10: u8, v01: u8, v11: u8| -> u8 {
            let top = v00 as f64 * (1.0 - fx) + v10 as f64 * fx;
            let bottom = v01 as f64 * (1.0 - fx) + v11 as f64 * fx;
            (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
        };

        [
            lerp2(p00[0], p10[0], p01[0], p11[0]),
            lerp2(p00[1], p10[1], p01[1], p11[1]),
            lerp2(p00[2], p10[2], p01[2], p11[2]),
            lerp2(p00[3], p10[3], p01[3], p11[3]),
        ]
    }

    fn get_pixel_channels(image: &RawImage, x: usize, y: usize) -> [u8; 4] {
        let channels = image.channels as usize;
        let stride = match (image.width as usize).checked_mul(channels) {
            Some(stride) => stride,
            None => return [0, 0, 0, 255],
        };
        let base = match y
            .checked_mul(stride)
            .and_then(|row| row.checked_add(x * channels))
        {
            Some(base) => base,
            None => return [0, 0, 0, 255],
        };

        match image.channels {
            1 => {
                let g = image.pixels.get(base).copied().unwrap_or(0);
                [g, g, g, 255]
            }
            3 => [
                image.pixels.get(base).copied().unwrap_or(0),
                image.pixels.get(base + 1).copied().unwrap_or(0),
                image.pixels.get(base + 2).copied().unwrap_or(0),
                255,
            ],
            4 => [
                image.pixels.get(base).copied().unwrap_or(0),
                image.pixels.get(base + 1).copied().unwrap_or(0),
                image.pixels.get(base + 2).copied().unwrap_or(0),
                image.pixels.get(base + 3).copied().unwrap_or(255),
            ],
            _ => [0, 0, 0, 255],
        }
    }
}

fn source_footprint_from_inverse(inv: &Transform2D, image: &RawImage) -> (f64, f64) {
    let dx = inv.transform_vector(1.0, 0.0);
    let dy = inv.transform_vector(0.0, 1.0);
    let footprint_x = (dx.0.abs() + dy.0.abs()) * image.width as f64;
    let footprint_y = (dx.1.abs() + dy.1.abs()) * image.height as f64;
    (footprint_x.max(1e-6), footprint_y.max(1e-6))
}

fn bounding_box(corners: &[(f64, f64); 4]) -> (f64, f64, f64, f64) {
    let px_min = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    let px_max = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let py_min = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::INFINITY, f64::min);
    let py_max = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max);
    (px_min, px_max, py_min, py_max)
}

fn clipped_bounds(
    buf: &PixelBuffer,
    px_min: f64,
    px_max: f64,
    py_min: f64,
    py_max: f64,
) -> (i32, i32, i32, i32) {
    if buf.width == 0 || buf.height == 0 {
        return (1, 0, 1, 0);
    }
    let x0 = floor_i32(px_min).max(0);
    let x1 = ceil_i32(px_max).min(buf.width as i32 - 1);
    let y0 = floor_i32(py_min).max(0);
    let y1 = ceil_i32(py_max).min(buf.height as i32 - 1);
    (x0, x1, y0, y1)
}

fn floor_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value.floor() as i32
    }
}

fn ceil_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value.ceil() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::{BLACK, WHITE};

    fn rgb_2x2_image() -> RawImage {
        RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        }
    }

    #[test]
    fn bilinear_sample_on_2x2_image_corners() {
        let image = rgb_2x2_image();
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 0.0, 0.0)[..3],
            &[255, 0, 0]
        );
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 1.0, 0.0)[..3],
            &[0, 255, 0]
        );
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 0.0, 1.0)[..3],
            &[0, 0, 255]
        );
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 1.0, 1.0)[..3],
            &[255, 255, 0]
        );
    }

    #[test]
    fn bilinear_sample_at_center_blends_all_corners() {
        let image = RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![200, 0, 0, 0, 200, 0, 0, 0, 200, 200, 200, 0],
        };
        let center = ImagePainter::bilinear_sample(&image, 0.5, 0.5);
        assert!((center[0] as i32 - 100).abs() <= 3);
    }

    #[test]
    fn bilinear_sample_gray_image_replicates_channels() {
        let image = RawImage {
            width: 2,
            height: 1,
            channels: 1,
            bits_per_sample: 8,
            pixels: vec![0, 255],
        };
        let left = ImagePainter::bilinear_sample(&image, 0.0, 0.5);
        let right = ImagePainter::bilinear_sample(&image, 1.0, 0.5);
        assert_eq!(&left[..3], &[0, 0, 0]);
        assert_eq!(&right[..3], &[255, 255, 255]);
        assert_eq!(left[3], 255);
        assert_eq!(right[3], 255);
    }

    #[test]
    fn bilinear_sample_rgba_image_preserves_alpha() {
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 128],
        };
        let sample = ImagePainter::bilinear_sample(&image, 0.5, 0.5);
        assert_eq!(sample, [255, 0, 0, 128]);
    }

    #[test]
    fn area_average_downscales_checkerboard_to_gray() {
        let mut pixels = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                pixels.push(if (x + y) % 2 == 0 { 0 } else { 255 });
            }
        }
        let image = RawImage {
            width: 4,
            height: 4,
            channels: 1,
            bits_per_sample: 8,
            pixels,
        };

        let sample = ImagePainter::area_average_sample(&image, 0.5, 0.5, 4.0, 4.0);
        for channel in sample.iter().take(3) {
            assert!(
                (*channel as i32 - 128).abs() <= 1,
                "checkerboard should integrate to gray, got {sample:?}"
            );
        }
        assert_eq!(sample[3], 255);
    }

    #[test]
    fn minified_image_paint_uses_area_average() {
        let mut pixels = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                pixels.extend_from_slice(&[v, v, v]);
            }
        }
        let image = RawImage {
            width: 4,
            height: 4,
            channels: 3,
            bits_per_sample: 8,
            pixels,
        };
        let vp = Viewport::new([0.0, 0.0, 2.0, 2.0], 72);
        let ctm = Transform2D::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0);
        let mut buf = PixelBuffer::new_filled(2, 2, WHITE);

        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);

        for y in 0..2 {
            for x in 0..2 {
                let pixel = buf.get_pixel(x, y);
                assert!(
                    (pixel[0] as i32 - 128).abs() <= 1,
                    "downscaled pixel ({x},{y}) should be gray, got {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn interpolate_true_smooths_magnified_image() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 10.0], 72);
        let ctm = Transform2D::new(100.0, 0.0, 0.0, 10.0, 0.0, 0.0);
        let image = RawImage {
            width: 2,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0, 0, 0, 255, 255, 255],
        };
        let mut crisp = PixelBuffer::new_filled(100, 10, WHITE);
        let mut smooth = PixelBuffer::new_filled(100, 10, WHITE);

        ImagePainter::paint_image(&mut crisp, &image, &ctm, &vp);
        ImagePainter::paint_image_with_options(&mut smooth, &image, &ctm, &vp, true);

        assert_eq!(
            crisp.get_pixel(49, 5)[0],
            0,
            "default /Interpolate false should stay nearest on the left of seam"
        );
        assert_eq!(
            crisp.get_pixel(50, 5)[0],
            255,
            "default /Interpolate false should stay nearest on the right of seam"
        );
        let edge = smooth.get_pixel(25, 5)[0];
        assert!(
            (32..=223).contains(&edge),
            "/Interpolate true should smooth the magnified seam, got {edge}"
        );
    }

    #[test]
    fn paint_image_places_pixels_in_correct_region() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0],
        };
        let ctm = Transform2D::new(50.0, 0.0, 0.0, 50.0, 25.0, 25.0);
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        let center = buf.get_pixel(50, 50);
        println!("paint_image center pixel: {:?}", center);
        assert!(center[0] > 200);
        assert_eq!(buf.get_pixel(1, 1), WHITE);
    }

    #[test]
    fn paint_image_with_zero_size_image_does_not_panic() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let empty = RawImage {
            width: 0,
            height: 0,
            channels: 3,
            bits_per_sample: 8,
            pixels: Vec::new(),
        };
        ImagePainter::paint_image(&mut buf, &empty, &ctm, &vp);
    }

    #[test]
    fn paint_image_rgba_uses_alpha_channel() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf_opaque = PixelBuffer::new_filled(100, 100, WHITE);
        let mut buf_transp = PixelBuffer::new_filled(100, 100, WHITE);
        let ctm = Transform2D::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0);
        let opaque = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 255],
        };
        let transparent = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 0],
        };
        ImagePainter::paint_image(&mut buf_opaque, &opaque, &ctm, &vp);
        ImagePainter::paint_image(&mut buf_transp, &transparent, &ctm, &vp);
        assert!(buf_opaque.get_pixel(50, 50)[0] > 200);
        assert_eq!(buf_transp.get_pixel(50, 50), WHITE);
    }

    #[test]
    fn bilinear_sample_empty_image_is_transparent() {
        let image = RawImage {
            width: 0,
            height: 0,
            channels: 0,
            bits_per_sample: 8,
            pixels: Vec::new(),
        };
        assert_eq!(
            ImagePainter::bilinear_sample(&image, 0.5, 0.5),
            [0, 0, 0, 0]
        );
    }

    #[test]
    fn paint_image_singular_transform_skips_gracefully() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0, 0, 0],
        };
        let ctm = Transform2D::new(0.0, 0.0, 0.0, 0.0, 10.0, 10.0);
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        assert_eq!(buf.get_pixel(10, 10), WHITE);
        assert_eq!(buf.get_pixel(0, 0), WHITE);
    }

    // Regression (Benchmark Fix B): a small image MAGNIFIED to a larger area
    // must render as crisp nearest-neighbour blocks (the PDF/Poppler default when
    // /Interpolate is absent), NOT a bilinearly-smoothed gradient. A 2x2
    // image scaled to fill a 100x100 page must have a sharp seam between cells,
    // with interior pixels exactly equal to a source pixel (no blend).
    #[test]
    fn magnified_image_uses_nearest_neighbour_blocks() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            // TL=red, TR=green, BL=blue, BR=yellow
            pixels: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        };
        let ctm = Transform2D::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0);
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        // Deep inside each quadrant the pixel must be exactly one source colour
        // (a smoothed gradient would blend toward neighbours). Sample near each
        // corner, away from the central seam. Device y is flipped (top = y small).
        let tl = buf.get_pixel(12, 12);
        let tr = buf.get_pixel(88, 12);
        let bl = buf.get_pixel(12, 88);
        let br = buf.get_pixel(88, 88);
        // Each must be a pure primary (one channel 255, others 0) or yellow — i.e.
        // NOT a blended intermediate. Check no channel is a mid value.
        for (label, p) in [("tl", tl), ("tr", tr), ("bl", bl), ("br", br)] {
            for (c, &v) in p.iter().take(3).enumerate() {
                assert!(
                    v == 0 || v == 255,
                    "{label} channel {c} = {v}: magnified image must be crisp (0 or 255), not blended"
                );
            }
        }
    }

    #[test]
    fn paint_rotated_image_affine_path_draws_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: BLACK[..3].to_vec(),
        };
        let ctm = Transform2D::scale(30.0, 30.0)
            .concat(&Transform2D::rotation(std::f64::consts::FRAC_PI_4))
            .concat(&Transform2D::translation(50.0, 50.0));
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        let dark = (0..100i32)
            .flat_map(|y| (0..100i32).map(move |x| (x, y)))
            .any(|(x, y)| buf.get_pixel(x, y)[0] < 200);
        assert!(dark);
    }
}
