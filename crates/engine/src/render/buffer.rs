use crate::content::BlendMode;
use crate::images::decoder::RawImage;
use crate::render::path::{FillRule, FlatPath};

/// RGBA color: [R, G, B, A] each 0-255.
pub type PixelColor = [u8; 4];

pub const BLACK: PixelColor = [0, 0, 0, 255];
pub const WHITE: PixelColor = [255, 255, 255, 255];
pub const TRANSPARENT: PixelColor = [0, 0, 0, 0];
pub const RED: PixelColor = [255, 0, 0, 255];
pub const GREEN: PixelColor = [0, 255, 0, 255];
pub const BLUE: PixelColor = [0, 0, 255, 255];

/// Create a PixelColor with full alpha.
pub fn rgb(r: u8, g: u8, b: u8) -> PixelColor {
    [r, g, b, 255]
}

/// Create a PixelColor with specified alpha.
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> PixelColor {
    [r, g, b, a]
}

#[derive(Debug, Clone)]
pub struct ClipMask {
    pub width: u32,
    pub height: u32,
    mask: Vec<u8>,
}

impl ClipMask {
    /// All-visible mask: every pixel is inside the clip.
    pub fn all_visible(width: u32, height: u32) -> Self {
        let len = (width as usize).checked_mul(height as usize).unwrap_or(0);
        Self {
            width,
            height,
            mask: vec![255u8; len],
        }
    }

    /// Query whether pixel (x, y) is inside the clip.
    #[inline]
    pub fn is_visible(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return true;
        }
        let idx = match (y as usize)
            .checked_mul(self.width as usize)
            .and_then(|row| row.checked_add(x as usize))
        {
            Some(idx) => idx,
            None => return true,
        };
        self.mask.get(idx).copied().unwrap_or(255) > 0
    }

    /// Set pixel (x, y) to visible or clipped.
    pub fn set(&mut self, x: i32, y: i32, visible: bool) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let Some(idx) = (y as usize)
            .checked_mul(self.width as usize)
            .and_then(|row| row.checked_add(x as usize))
        else {
            return;
        };
        if let Some(value) = self.mask.get_mut(idx) {
            *value = if visible { 255 } else { 0 };
        }
    }

    /// Intersect this mask with another mask.
    pub fn intersect(&mut self, other: &ClipMask) {
        if self.width != other.width || self.height != other.height {
            log::warn!(
                "ClipMask::intersect size mismatch: {}x{} vs {}x{}",
                self.width,
                self.height,
                other.width,
                other.height
            );
            return;
        }
        for (a, b) in self.mask.iter_mut().zip(other.mask.iter()) {
            *a = (*a).min(*b);
        }
    }

    /// Fill a rectangular mask region with visible or clipped.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, visible: bool) {
        if w <= 0 || h <= 0 {
            return;
        }
        let value = if visible { 255u8 } else { 0u8 };
        let x0 = x.max(0).min(self.width as i32);
        let y0 = y.max(0).min(self.height as i32);
        let x1 = x.saturating_add(w).max(0).min(self.width as i32);
        let y1 = y.saturating_add(h).max(0).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        for row in y0..y1 {
            let start = row as usize * self.width as usize + x0 as usize;
            let end = row as usize * self.width as usize + x1 as usize;
            if let Some(slice) = self.mask.get_mut(start..end) {
                slice.fill(value);
            }
        }
    }

    /// Build a ClipMask from a flattened path using scanline fill.
    pub fn from_path(flat: &FlatPath, width: u32, height: u32, fill_rule: FillRule) -> Self {
        Self::scanline_fill(flat, width, height, fill_rule)
    }

    fn scanline_fill(flat: &FlatPath, width: u32, height: u32, rule: FillRule) -> Self {
        let mut clip = Self::all_visible(width, height);
        clip.mask.fill(0);

        let mut edges = Vec::new();
        for subpath in &flat.subpaths {
            for segment in subpath.windows(2) {
                let (x0, y0) = segment[0];
                let (x1, y1) = segment[1];
                if (y0 - y1).abs() < 1e-10 {
                    continue;
                }
                let (x_start, y_start, x_end, y_end) = if y0 < y1 {
                    (x0, y0, x1, y1)
                } else {
                    (x1, y1, x0, y0)
                };
                let winding = if y0 < y1 { 1 } else { -1 };
                edges.push(ClipEdge {
                    y_min: y_start,
                    y_max: y_end,
                    x_at_ymin: x_start,
                    slope: (x_end - x_start) / (y_end - y_start),
                    winding,
                });
            }
        }

        if edges.is_empty() || width == 0 || height == 0 {
            return clip;
        }

        let y_min = edges
            .iter()
            .map(|edge| floor_i32(edge.y_min))
            .min()
            .unwrap_or(0)
            .max(0);
        let y_max = edges
            .iter()
            .map(|edge| ceil_i32(edge.y_max))
            .max()
            .unwrap_or(0)
            .min(height as i32 - 1);

        for y in y_min..=y_max {
            let y_f = y as f64 + 0.5;
            let mut intersections: Vec<(f64, i32)> = edges
                .iter()
                .filter(|edge| edge.y_min <= y_f && y_f < edge.y_max)
                .map(|edge| {
                    (
                        edge.x_at_ymin + edge.slope * (y_f - edge.y_min),
                        edge.winding,
                    )
                })
                .collect();
            intersections
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            for (x0, x1) in fill_spans(&intersections, rule) {
                let px0 = ceil_i32(x0).max(0);
                let px1 = floor_i32(x1).min(width as i32 - 1);
                for px in px0..=px1 {
                    clip.set(px, y, true);
                }
            }
        }

        clip
    }
}

#[derive(Debug, Clone)]
pub struct AlphaMask {
    pub width: u32,
    pub height: u32,
    data: Vec<u8>,
}

impl AlphaMask {
    pub fn all_opaque(width: u32, height: u32) -> Self {
        let len = (width as usize).checked_mul(height as usize).unwrap_or(0);
        Self {
            width,
            height,
            data: vec![255u8; len],
        }
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> f32 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 1.0;
        }
        let Some(idx) = (y as usize)
            .checked_mul(self.width as usize)
            .and_then(|row| row.checked_add(x as usize))
        else {
            return 1.0;
        };
        self.data.get(idx).copied().unwrap_or(255) as f32 / 255.0
    }

    pub fn set(&mut self, x: i32, y: i32, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let Some(idx) = (y as usize)
            .checked_mul(self.width as usize)
            .and_then(|row| row.checked_add(x as usize))
        else {
            return;
        };
        if let Some(value) = self.data.get_mut(idx) {
            *value = alpha;
        }
    }

    /// Build a luminosity soft mask from a rendered buffer (ExtGState
    /// `/SMask /S /Luminosity`). The mask value for each pixel is the
    /// perceptual luminance of its RGB. We use Rec. 601 weights
    /// (0.299/0.587/0.114), which is what Poppler's `SplashBitmap` uses for
    /// luminosity soft masks; matching it keeps our masks PSNR-comparable.
    pub fn from_luminosity(buf: &PixelBuffer) -> Self {
        let len = (buf.width as usize)
            .checked_mul(buf.height as usize)
            .unwrap_or(0);
        let mut mask = Self {
            width: buf.width,
            height: buf.height,
            data: vec![0u8; len],
        };
        for y in 0..buf.height as i32 {
            for x in 0..buf.width as i32 {
                let p = buf.get_pixel(x, y);
                let lum = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
                mask.set(x, y, lum.round().clamp(0.0, 255.0) as u8);
            }
        }
        mask
    }

    /// Build an alpha soft mask from a rendered buffer (ExtGState
    /// `/SMask /S /Alpha`). The mask value for each pixel is the buffer's own
    /// alpha channel — no luminosity conversion.
    pub fn from_alpha_channel(buf: &PixelBuffer) -> Self {
        let len = (buf.width as usize)
            .checked_mul(buf.height as usize)
            .unwrap_or(0);
        let mut mask = Self {
            width: buf.width,
            height: buf.height,
            data: vec![0u8; len],
        };
        for y in 0..buf.height as i32 {
            for x in 0..buf.width as i32 {
                mask.set(x, y, buf.get_pixel(x, y)[3]);
            }
        }
        mask
    }

    /// Remap every mask value through a transfer-function lookup table
    /// (256 entries, input index -> output byte). Used for ExtGState SMask
    /// `/TR` transfer functions.
    pub fn apply_transfer_lut(&mut self, lut: &[u8; 256]) {
        for v in self.data.iter_mut() {
            *v = lut[*v as usize];
        }
    }
}

#[derive(Debug, Clone)]
struct ClipEdge {
    y_min: f64,
    y_max: f64,
    x_at_ymin: f64,
    slope: f64,
    winding: i32,
}

fn fill_spans(intersections: &[(f64, i32)], rule: FillRule) -> Vec<(f64, f64)> {
    let mut spans = Vec::new();
    match rule {
        FillRule::EvenOdd => {
            let mut iter = intersections.iter();
            while let Some((x_start, _)) = iter.next() {
                if let Some((x_end, _)) = iter.next() {
                    spans.push((*x_start, *x_end));
                }
            }
        }
        FillRule::NonZero => {
            let mut winding = 0i32;
            let mut span_start = None;
            for &(x, w) in intersections {
                let was_nonzero = winding != 0;
                winding += w;
                let is_nonzero = winding != 0;
                if !was_nonzero && is_nonzero {
                    span_start = Some(x);
                } else if was_nonzero && !is_nonzero {
                    if let Some(start) = span_start.take() {
                        spans.push((start, x));
                    }
                }
            }
        }
    }
    spans
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

#[derive(Debug, Clone)]
pub struct PixelBuffer {
    pub width: u32,
    pub height: u32,
    pub blend_mode: BlendMode,
    data: Vec<u8>,
    clip: Option<ClipMask>,
    smask: Option<AlphaMask>,
}

impl PixelBuffer {
    /// Allocate a new transparent buffer.
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .unwrap_or(0);
        Self {
            width,
            height,
            blend_mode: BlendMode::Normal,
            data: vec![0u8; len],
            clip: None,
            smask: None,
        }
    }

    /// Allocate a fully transparent buffer. Used for off-screen transparency groups.
    pub fn new_transparent(width: u32, height: u32) -> Self {
        Self::new(width, height)
    }

    /// Allocate and fill with the given color.
    pub fn new_filled(width: u32, height: u32, color: PixelColor) -> Self {
        let mut buf = Self::new(width, height);
        buf.fill(color);
        buf
    }

    fn pixel_index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        let idx = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?
            .checked_mul(4)?;
        if idx + 3 < self.data.len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Get the RGBA value of pixel (x, y). Returns transparent if out of bounds.
    pub fn get_pixel(&self, x: i32, y: i32) -> PixelColor {
        match self.pixel_index(x, y) {
            Some(idx) => [
                self.data[idx],
                self.data[idx + 1],
                self.data[idx + 2],
                self.data[idx + 3],
            ],
            None => TRANSPARENT,
        }
    }

    /// Set the RGBA value of pixel (x, y). No-op if out of bounds.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: PixelColor) {
        if let Some(clip) = &self.clip {
            if !clip.is_visible(x, y) {
                return;
            }
        }
        if let Some(idx) = self.pixel_index(x, y) {
            self.data[idx] = color[0];
            self.data[idx + 1] = color[1];
            self.data[idx + 2] = color[2];
            self.data[idx + 3] = color[3];
        }
    }

    /// Alpha-composite a color with coverage [0.0, 1.0] over the existing pixel.
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: PixelColor, coverage: f32) {
        if coverage <= 0.0 {
            return;
        }
        if let Some(clip) = &self.clip {
            if !clip.is_visible(x, y) {
                return;
            }
        }
        let idx = match self.pixel_index(x, y) {
            Some(idx) => idx,
            None => return,
        };

        let smask_alpha = self.smask.as_ref().map_or(1.0, |mask| mask.get(x, y));
        let eff_a = (color[3] as f32 / 255.0 * coverage * smask_alpha).clamp(0.0, 1.0);
        if eff_a <= 0.0 {
            return;
        }

        let dst_r = self.data[idx] as f32 / 255.0;
        let dst_g = self.data[idx + 1] as f32 / 255.0;
        let dst_b = self.data[idx + 2] as f32 / 255.0;
        let dst_a = self.data[idx + 3] as f32 / 255.0;
        let src_r = color[0] as f32 / 255.0;
        let src_g = color[1] as f32 / 255.0;
        let src_b = color[2] as f32 / 255.0;

        let (blend_r, blend_g, blend_b) = if dst_a <= 1e-6 {
            (src_r, src_g, src_b)
        } else {
            let bm = self.blend_mode;
            if bm.is_separable() {
                (
                    bm.blend_channel(src_r, dst_r),
                    bm.blend_channel(src_g, dst_g),
                    bm.blend_channel(src_b, dst_b),
                )
            } else {
                // Non-separable modes (Hue/Saturation/Color/Luminosity) operate
                // on the whole RGB triple, not per channel.
                let [r, g, b] = bm.blend_rgb([src_r, src_g, src_b], [dst_r, dst_g, dst_b]);
                (r, g, b)
            }
        };
        let out_a = eff_a + dst_a * (1.0 - eff_a);

        if out_a < 1e-6 {
            self.data[idx] = 0;
            self.data[idx + 1] = 0;
            self.data[idx + 2] = 0;
            self.data[idx + 3] = 0;
            return;
        }

        let inv_a = 1.0 / out_a;
        self.data[idx] = ((blend_r * eff_a + dst_r * dst_a * (1.0 - eff_a)) * inv_a * 255.0)
            .clamp(0.0, 255.0) as u8;
        self.data[idx + 1] = ((blend_g * eff_a + dst_g * dst_a * (1.0 - eff_a)) * inv_a * 255.0)
            .clamp(0.0, 255.0) as u8;
        self.data[idx + 2] = ((blend_b * eff_a + dst_b * dst_a * (1.0 - eff_a)) * inv_a * 255.0)
            .clamp(0.0, 255.0) as u8;
        self.data[idx + 3] = (out_a * 255.0).clamp(0.0, 255.0) as u8;
    }

    /// Fill the entire buffer with a solid color.
    pub fn fill(&mut self, color: PixelColor) {
        for chunk in self.data.chunks_exact_mut(4) {
            chunk[0] = color[0];
            chunk[1] = color[1];
            chunk[2] = color[2];
            chunk[3] = color[3];
        }
    }

    /// Fill a rectangular region. Clips to buffer bounds.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: PixelColor) {
        if w <= 0 || h <= 0 {
            return;
        }
        if color[3] == 0 {
            return;
        }
        let x0 = x.max(0).min(self.width as i32);
        let y0 = y.max(0).min(self.height as i32);
        let x1 = x.saturating_add(w).max(0).min(self.width as i32);
        let y1 = y.saturating_add(h).max(0).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let should_blend =
            color[3] < 255 || self.blend_mode != BlendMode::Normal || self.smask.is_some();
        if should_blend {
            for row in y0..y1 {
                for col in x0..x1 {
                    self.blend_pixel(col, row, color, 1.0);
                }
            }
            return;
        }

        let Some(clip) = self.clip.as_ref() else {
            for row in y0..y1 {
                fill_opaque_run(&mut self.data, self.width, row, x0, x1, color);
            }
            return;
        };

        for row in y0..y1 {
            let mut run_start: Option<i32> = None;
            for col in x0..x1 {
                if clip.is_visible(col, row) {
                    if run_start.is_none() {
                        run_start = Some(col);
                    }
                } else if let Some(start) = run_start.take() {
                    fill_opaque_run(&mut self.data, self.width, row, start, col, color);
                }
            }
            if let Some(start) = run_start {
                fill_opaque_run(&mut self.data, self.width, row, start, x1, color);
            }
        }
    }

    /// Intersect the existing clip with `mask`, or install it as the first clip.
    pub fn set_clip(&mut self, mask: ClipMask) {
        if let Some(existing) = &mut self.clip {
            existing.intersect(&mask);
        } else {
            self.clip = Some(mask);
        }
    }

    /// Clear clipping; all pixels become paintable.
    pub fn clear_clip(&mut self) {
        self.clip = None;
    }

    /// Directly replace the current clip without intersecting.
    pub fn replace_clip(&mut self, clip: Option<ClipMask>) {
        self.clip = clip;
    }

    /// True if a clip mask is installed.
    pub fn has_clip(&self) -> bool {
        self.clip.is_some()
    }

    /// Borrow the current clip mask, if any.
    pub fn clip_mask(&self) -> Option<&ClipMask> {
        self.clip.as_ref()
    }

    pub fn set_smask(&mut self, mask: AlphaMask) {
        self.smask = Some(mask);
    }

    pub fn clear_smask(&mut self) {
        self.smask = None;
    }

    pub fn smask_mask(&self) -> Option<&AlphaMask> {
        self.smask.as_ref()
    }

    /// True if the pixel at (x, y) is paintable under the current clip. With no
    /// clip installed every in-bounds pixel is allowed. Used by the shading
    /// renderer to skip expensive colour evaluation for clipped pixels.
    pub fn clip_allows(&self, x: i32, y: i32) -> bool {
        match &self.clip {
            Some(clip) => clip.is_visible(x, y),
            None => true,
        }
    }

    pub(crate) fn restore_clip(&mut self, clip: Option<ClipMask>) {
        self.clip = clip;
    }

    pub(crate) fn restore_smask(&mut self, smask: Option<AlphaMask>) {
        self.smask = smask;
    }

    /// Return RGB bytes, discarding alpha.
    pub fn to_rgb_bytes(&self) -> Vec<u8> {
        let pixel_count = self.width as usize * self.height as usize;
        let mut out = Vec::with_capacity(pixel_count * 3);
        for chunk in self.data.chunks_exact(4) {
            out.push(chunk[0]);
            out.push(chunk[1]);
            out.push(chunk[2]);
        }
        out
    }

    /// Return RGBA bytes.
    pub fn to_rgba_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Convert to a RawImage for use with ImageEncoder.
    pub fn to_raw_image(&self) -> RawImage {
        RawImage {
            width: self.width,
            height: self.height,
            channels: 3,
            bits_per_sample: 8,
            pixels: self.to_rgb_bytes(),
        }
    }

    /// Convert to a RawImage with alpha channel.
    pub fn to_raw_image_rgba(&self) -> RawImage {
        RawImage {
            width: self.width,
            height: self.height,
            channels: 4,
            bits_per_sample: 8,
            pixels: self.to_rgba_bytes(),
        }
    }

    /// Composite a source RGBA buffer onto this buffer.
    ///
    /// This is the primitive used to flatten a transparency-group offscreen
    /// buffer onto its parent (the page buffer, or an enclosing group). The
    /// source's own per-pixel alpha is honored, then scaled by `group_alpha`
    /// (the `/ca` or `/CA` constant active at the `Do` operator) and, if
    /// present, by the per-pixel `soft_mask` value. Blending of color channels
    /// uses `blend_mode`. `self`'s own clip mask still applies.
    ///
    /// `self` and `src` must have the same dimensions (both are page-sized in
    /// the renderer), which keeps device coordinates aligned 1:1.
    pub fn composite_from(
        &mut self,
        src: &PixelBuffer,
        group_alpha: f32,
        blend_mode: BlendMode,
        soft_mask: Option<&AlphaMask>,
    ) {
        let alpha = group_alpha.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let saved_blend = self.blend_mode;
        self.blend_mode = blend_mode;
        let w = self.width.min(src.width) as i32;
        let h = self.height.min(src.height) as i32;
        for y in 0..h {
            for x in 0..w {
                let sp = src.get_pixel(x, y);
                if sp[3] == 0 {
                    continue;
                }
                let mask = soft_mask.map_or(1.0, |m| m.get(x, y));
                let coverage = alpha * mask;
                if coverage <= 0.0 {
                    continue;
                }
                // `blend_pixel` interprets the source's alpha (sp[3]) and the
                // coverage multiplier together, applying the buffer blend mode
                // and any installed clip; reusing it keeps a single compositing
                // code path for direct paints and group flattening.
                self.blend_pixel(x, y, sp, coverage);
            }
        }
        self.blend_mode = saved_blend;
    }

    /// Remove a backdrop's contribution from this buffer (a non-isolated
    /// transparency-group result that was seeded with `backdrop`).
    ///
    /// A non-isolated group is rendered starting from a copy of its backdrop so
    /// that blend modes inside the group can interact with what is already
    /// painted. Before the group is composited back onto that same backdrop, the
    /// backdrop's own contribution must be removed so it is not counted twice.
    /// Per PDF 32000-1 §11.4.8, with group result `(Cn, αn)` and initial
    /// backdrop `(C0, α0)`:
    ///
    /// ```text
    /// C = Cn + (Cn - C0) * (α0 / αn - α0)   (per color channel, when αn > 0)
    /// ```
    ///
    /// The result alpha is left as `αn`; compositing back with source-over then
    /// reproduces the correct final image.
    pub fn remove_backdrop(&mut self, backdrop: &PixelBuffer) {
        let w = self.width.min(backdrop.width) as i32;
        let h = self.height.min(backdrop.height) as i32;
        for y in 0..h {
            for x in 0..w {
                let Some(idx) = self.pixel_index(x, y) else {
                    continue;
                };
                let an = self.data[idx + 3] as f32 / 255.0;
                if an <= 1e-6 {
                    continue;
                }
                let bd = backdrop.get_pixel(x, y);
                let a0 = bd[3] as f32 / 255.0;
                if a0 <= 1e-6 {
                    // No backdrop here: nothing to remove.
                    continue;
                }
                let factor = a0 / an - a0;
                for (c, &c0_byte) in bd.iter().take(3).enumerate() {
                    let cn = self.data[idx + c] as f32 / 255.0;
                    let c0 = c0_byte as f32 / 255.0;
                    let out = cn + (cn - c0) * factor;
                    self.data[idx + c] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    /// Composite a source buffer onto this one using "knockout" semantics:
    /// each source pixel with non-zero alpha *replaces* the destination pixel
    /// (scaled by `group_alpha`/`soft_mask`) rather than blending with it. Used
    /// for knockout transparency groups (/K true), where group elements knock
    /// out the group backdrop instead of accumulating.
    pub fn knockout_from(
        &mut self,
        src: &PixelBuffer,
        group_alpha: f32,
        soft_mask: Option<&AlphaMask>,
    ) {
        let alpha = group_alpha.clamp(0.0, 1.0);
        for y in 0..self.height.min(src.height) as i32 {
            for x in 0..self.width.min(src.width) as i32 {
                if let Some(clip) = &self.clip {
                    if !clip.is_visible(x, y) {
                        continue;
                    }
                }
                let sp = src.get_pixel(x, y);
                if sp[3] == 0 {
                    continue;
                }
                let mask = soft_mask.map_or(1.0, |m| m.get(x, y));
                let eff = (sp[3] as f32 / 255.0 * alpha * mask).clamp(0.0, 1.0);
                if let Some(idx) = self.pixel_index(x, y) {
                    self.data[idx] = sp[0];
                    self.data[idx + 1] = sp[1];
                    self.data[idx + 2] = sp[2];
                    self.data[idx + 3] = (eff * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

fn fill_opaque_run(
    data: &mut [u8],
    width: u32,
    row: i32,
    x_start: i32,
    x_end_exclusive: i32,
    color: PixelColor,
) {
    if row < 0 || x_start < 0 || x_end_exclusive <= x_start {
        return;
    }
    let Some(start) = (row as usize)
        .checked_mul(width as usize)
        .and_then(|row_base| row_base.checked_add(x_start as usize))
        .and_then(|pixel| pixel.checked_mul(4))
    else {
        return;
    };
    let Some(end) = (row as usize)
        .checked_mul(width as usize)
        .and_then(|row_base| row_base.checked_add(x_end_exclusive as usize))
        .and_then(|pixel| pixel.checked_mul(4))
    else {
        return;
    };
    let Some(slice) = data.get_mut(start..end) else {
        return;
    };
    for chunk in slice.chunks_exact_mut(4) {
        chunk.copy_from_slice(&color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_transparent() {
        let buf = PixelBuffer::new(4, 4);
        assert_eq!(buf.get_pixel(0, 0), TRANSPARENT);
        assert_eq!(buf.get_pixel(3, 3), TRANSPARENT);
    }

    #[test]
    fn set_and_get_pixel() {
        let mut buf = PixelBuffer::new(10, 10);
        buf.set_pixel(5, 5, RED);
        assert_eq!(buf.get_pixel(5, 5), RED);
        assert_eq!(buf.get_pixel(0, 0), TRANSPARENT);
    }

    #[test]
    fn out_of_bounds_set_pixel_is_no_op() {
        let mut buf = PixelBuffer::new(4, 4);
        buf.set_pixel(-1, 0, RED);
        buf.set_pixel(4, 0, RED);
        buf.set_pixel(0, -1, RED);
        buf.set_pixel(0, 4, RED);
        assert_eq!(buf.get_pixel(0, 0), TRANSPARENT);
    }

    #[test]
    fn fill() {
        let mut buf = PixelBuffer::new(3, 3);
        buf.fill(WHITE);
        assert_eq!(buf.get_pixel(0, 0), WHITE);
        assert_eq!(buf.get_pixel(2, 2), WHITE);
    }

    #[test]
    fn blend_pixel_composites_correctly() {
        let mut buf = PixelBuffer::new(1, 1);
        buf.fill(WHITE);
        buf.blend_pixel(0, 0, RED, 0.5);
        let p = buf.get_pixel(0, 0);
        assert!(p[0] >= 200);
        assert!(p[1] <= 200);
    }

    #[test]
    fn blend_pixel_with_zero_coverage_is_no_op() {
        let mut buf = PixelBuffer::new(1, 1);
        buf.fill(WHITE);
        buf.blend_pixel(0, 0, RED, 0.0);
        assert_eq!(buf.get_pixel(0, 0), WHITE);
    }

    #[test]
    fn to_rgb_bytes_discards_alpha() {
        let mut buf = PixelBuffer::new(2, 1);
        buf.set_pixel(0, 0, [255, 0, 0, 128]);
        buf.set_pixel(1, 0, [0, 255, 0, 255]);
        assert_eq!(buf.to_rgb_bytes(), vec![255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn to_raw_image_has_correct_dimensions_and_channels() {
        let buf = PixelBuffer::new(100, 200);
        let raw = buf.to_raw_image();
        assert_eq!(raw.width, 100);
        assert_eq!(raw.height, 200);
        assert_eq!(raw.channels, 3);
        assert_eq!(raw.pixels.len(), 100 * 200 * 3);
    }

    #[test]
    fn fill_rect_clips_correctly() {
        let mut buf = PixelBuffer::new(10, 10);
        buf.fill_rect(-5, -5, 20, 20, RED);
        for y in 0..10i32 {
            for x in 0..10i32 {
                assert_eq!(buf.get_pixel(x, y), RED);
            }
        }
    }

    #[test]
    fn fill_rect_partial_overlap() {
        let mut buf = PixelBuffer::new(10, 10);
        buf.fill_rect(5, 5, 10, 10, RED);
        assert_eq!(buf.get_pixel(5, 5), RED);
        assert_eq!(buf.get_pixel(9, 9), RED);
        assert_eq!(buf.get_pixel(4, 4), TRANSPARENT);
        assert_eq!(buf.get_pixel(4, 5), TRANSPARENT);
    }

    #[test]
    fn blend_pixel_full_opacity_replaces_pixel() {
        let mut buf = PixelBuffer::new(1, 1);
        buf.fill(WHITE);
        buf.blend_pixel(0, 0, BLACK, 1.0);
        let p = buf.get_pixel(0, 0);
        assert!(p[0] < 10);
        assert!(p[1] < 10);
        assert!(p[2] < 10);
    }

    #[test]
    fn multiple_blend_operations_accumulate() {
        let mut buf = PixelBuffer::new(1, 1);
        buf.fill(WHITE);
        buf.blend_pixel(0, 0, RED, 0.5);
        buf.blend_pixel(0, 0, RED, 0.5);
        let p = buf.get_pixel(0, 0);
        assert!(p[0] >= 240);
    }

    #[test]
    fn to_raw_image_rgba_includes_alpha() {
        let mut buf = PixelBuffer::new(1, 1);
        buf.set_pixel(0, 0, [100, 150, 200, 128]);
        let raw = buf.to_raw_image_rgba();
        assert_eq!(raw.channels, 4);
        assert_eq!(&raw.pixels, &[100, 150, 200, 128]);
    }

    #[test]
    fn fill_rect_with_no_clip_uses_fast_path_correctly() {
        let mut buf = PixelBuffer::new_filled(50, 50, WHITE);
        buf.fill_rect(10, 10, 30, 30, RED);
        assert_eq!(buf.get_pixel(25, 25), RED);
        assert_eq!(buf.get_pixel(9, 10), WHITE);
        assert_eq!(buf.get_pixel(40, 10), WHITE);
    }

    #[test]
    fn fill_rect_with_clip_uses_span_path_correctly() {
        let mut buf = PixelBuffer::new_filled(20, 20, WHITE);
        let mut clip = ClipMask::all_visible(20, 20);
        clip.fill_rect(0, 0, 20, 5, false);
        clip.fill_rect(0, 15, 20, 5, false);
        buf.set_clip(clip);
        buf.fill_rect(0, 0, 20, 20, RED);
        assert_eq!(buf.get_pixel(10, 10), RED);
        assert_eq!(buf.get_pixel(10, 2), WHITE);
        assert_eq!(buf.get_pixel(10, 18), WHITE);
    }

    #[test]
    fn fill_rect_with_solid_clip_fills_entire_rect() {
        let mut buf = PixelBuffer::new_filled(20, 20, WHITE);
        let clip = ClipMask::all_visible(20, 20);
        buf.set_clip(clip);
        buf.fill_rect(5, 5, 10, 10, BLUE);
        assert_eq!(buf.get_pixel(10, 10), BLUE);
        assert_eq!(buf.get_pixel(0, 0), WHITE);
    }

    #[test]
    fn fill_rect_with_column_stripe_clip() {
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut clip = ClipMask::all_visible(10, 10);
        for x in (1..10).step_by(2) {
            clip.fill_rect(x, 0, 1, 10, false);
        }
        buf.set_clip(clip);
        buf.fill_rect(0, 0, 10, 10, RED);

        assert_eq!(buf.get_pixel(0, 5), RED);
        assert_eq!(buf.get_pixel(2, 5), RED);
        assert_eq!(buf.get_pixel(1, 5), WHITE);
        assert_eq!(buf.get_pixel(3, 5), WHITE);
    }

    #[test]
    fn fill_rect_zero_dimensions_are_noop() {
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        buf.fill_rect(5, 5, 0, 5, RED);
        buf.fill_rect(5, 5, 5, 0, RED);
        buf.fill_rect(5, 5, -1, 5, RED);

        for y in 0..10i32 {
            for x in 0..10i32 {
                assert_eq!(buf.get_pixel(x, y), WHITE);
            }
        }
    }

    #[test]
    fn fill_rect_run_merging_preserves_clipped_gap() {
        let mut buf = PixelBuffer::new_filled(100, 1, WHITE);
        let mut clip = ClipMask::all_visible(100, 1);
        clip.set(50, 0, false);
        buf.set_clip(clip);
        buf.fill_rect(0, 0, 100, 1, BLUE);

        assert_eq!(buf.get_pixel(0, 0), BLUE);
        assert_eq!(buf.get_pixel(49, 0), BLUE);
        assert_eq!(buf.get_pixel(50, 0), WHITE);
        assert_eq!(buf.get_pixel(51, 0), BLUE);
        assert_eq!(buf.get_pixel(99, 0), BLUE);
    }

    #[test]
    fn fill_rect_with_no_visible_pixels_does_nothing() {
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut clip = ClipMask::all_visible(10, 10);
        clip.fill_rect(0, 0, 10, 10, false);
        buf.set_clip(clip);
        buf.fill_rect(0, 0, 10, 10, RED);

        let any_red = (0..10i32)
            .flat_map(|y| (0..10i32).map(move |x| (x, y)))
            .any(|(x, y)| {
                let pixel = buf.get_pixel(x, y);
                pixel[0] == 255 && pixel[1] == 0
            });
        assert!(!any_red);
    }

    #[test]
    fn blend_mode_channel_math_matches_pdf_modes() {
        assert_eq!(BlendMode::Normal.blend_channel(0.8, 0.3), 0.8);
        assert!((BlendMode::Multiply.blend_channel(0.8, 0.5) - 0.4).abs() < 0.001);
        assert!((BlendMode::Screen.blend_channel(0.8, 0.5) - 0.9).abs() < 0.001);
        assert!((BlendMode::Overlay.blend_channel(0.6, 0.3) - 0.36).abs() < 0.001);
        assert!((BlendMode::Overlay.blend_channel(0.6, 0.8) - 0.84).abs() < 0.001);
        assert_eq!(BlendMode::Darken.blend_channel(0.3, 0.7), 0.3);
        assert_eq!(BlendMode::Darken.blend_channel(0.7, 0.3), 0.3);
        assert_eq!(BlendMode::Lighten.blend_channel(0.3, 0.7), 0.7);
        assert_eq!(BlendMode::Lighten.blend_channel(0.7, 0.3), 0.7);
    }

    #[test]
    fn blend_mode_from_name_parses_supported_modes() {
        assert_eq!(BlendMode::from_name("Multiply"), BlendMode::Multiply);
        assert_eq!(BlendMode::from_name("Screen"), BlendMode::Screen);
        assert_eq!(BlendMode::from_name("Overlay"), BlendMode::Overlay);
        assert_eq!(BlendMode::from_name("Normal"), BlendMode::Normal);
        assert_eq!(BlendMode::from_name("Compatible"), BlendMode::Normal);
        assert_eq!(BlendMode::from_name("Unknown"), BlendMode::Normal);
    }

    #[test]
    fn multiply_blend_pixel_darkens_destination() {
        let mut buf = PixelBuffer::new_filled(1, 1, [200, 200, 200, 255]);
        buf.blend_mode = BlendMode::Multiply;
        buf.blend_pixel(0, 0, [128, 128, 128, 255], 1.0);
        let result = buf.get_pixel(0, 0);
        assert!(result[0] < 170, "Multiply should darken: R={}", result[0]);
    }

    #[test]
    fn screen_blend_pixel_lightens_destination() {
        let mut buf = PixelBuffer::new_filled(1, 1, [100, 100, 100, 255]);
        buf.blend_mode = BlendMode::Screen;
        buf.blend_pixel(0, 0, [100, 100, 100, 255], 1.0);
        let result = buf.get_pixel(0, 0);
        let mid = 100.0 / 255.0_f32;
        let expected = ((mid + mid - mid * mid) * 255.0) as u8;
        assert!(
            (result[0] as i32 - expected as i32).abs() <= 3,
            "Screen blend result: {} expected: {}",
            result[0],
            expected
        );
    }

    #[test]
    fn transparent_paint_does_not_change_destination() {
        let mut buf = PixelBuffer::new_filled(1, 1, WHITE);
        buf.blend_pixel(0, 0, [0, 0, 0, 0], 1.0);
        assert_eq!(buf.get_pixel(0, 0), WHITE);
    }

    #[test]
    fn blend_pixel_uses_current_buffer_blend_mode() {
        let mut multiply = PixelBuffer::new_filled(1, 1, [200, 200, 200, 255]);
        multiply.blend_mode = BlendMode::Multiply;
        multiply.blend_pixel(0, 0, [128, 128, 128, 255], 1.0);
        let multiply_result = multiply.get_pixel(0, 0)[0];

        let mut normal = PixelBuffer::new_filled(1, 1, [200, 200, 200, 255]);
        normal.blend_mode = BlendMode::Normal;
        normal.blend_pixel(0, 0, [128, 128, 128, 255], 1.0);
        let normal_result = normal.get_pixel(0, 0)[0];

        assert!(
            multiply_result < normal_result,
            "Multiply({}) should be darker than Normal({})",
            multiply_result,
            normal_result
        );
    }

    #[test]
    fn alpha_mask_from_luminosity_handles_white_black_and_gray() {
        let white = AlphaMask::from_luminosity(&PixelBuffer::new_filled(1, 1, WHITE));
        assert_eq!(white.get(0, 0), 1.0);

        let black = AlphaMask::from_luminosity(&PixelBuffer::new_filled(1, 1, BLACK));
        assert!(black.get(0, 0).abs() < 0.01);

        let gray = AlphaMask::from_luminosity(&PixelBuffer::new_filled(1, 1, [128, 128, 128, 255]));
        assert!(
            (gray.get(0, 0) - 0.502).abs() < 0.01,
            "gray alpha: {}",
            gray.get(0, 0)
        );
    }

    #[test]
    fn smask_modulates_blend_pixel_alpha() {
        let mut mask = AlphaMask::all_opaque(1, 1);
        mask.set(0, 0, 128);

        let mut buf = PixelBuffer::new_filled(1, 1, WHITE);
        buf.set_smask(mask);
        buf.blend_pixel(0, 0, BLACK, 1.0);
        let result = buf.get_pixel(0, 0);
        assert!(
            result[0] > 100 && result[0] < 200,
            "50% soft mask over white should be gray-ish: {:?}",
            result
        );
    }

    #[test]
    fn composite_from_half_alpha_red_over_white_is_pink() {
        // A fully-opaque red source composited at 50% group alpha onto white
        // must produce the same pink as a 50%-alpha red paint: 255 / 127 / 127.
        let mut dst = PixelBuffer::new_filled(2, 2, WHITE);
        let src = PixelBuffer::new_filled(2, 2, RED);
        dst.composite_from(&src, 0.5, BlendMode::Normal, None);
        let p = dst.get_pixel(0, 0);
        assert_eq!(p[0], 255, "red channel stays max");
        assert!((p[1] as i32 - 127).abs() <= 2, "green ~127, got {}", p[1]);
        assert!((p[2] as i32 - 127).abs() <= 2, "blue ~127, got {}", p[2]);
    }

    #[test]
    fn composite_from_respects_per_pixel_soft_mask() {
        // Opaque red source, full group alpha, but a soft mask that is 0 at
        // pixel (0,0) and 255 at (1,0). The masked pixel must stay white; the
        // unmasked pixel must become red.
        let mut dst = PixelBuffer::new_filled(2, 1, WHITE);
        let src = PixelBuffer::new_filled(2, 1, RED);
        let mut mask = AlphaMask::all_opaque(2, 1);
        mask.set(0, 0, 0);
        mask.set(1, 0, 255);
        dst.composite_from(&src, 1.0, BlendMode::Normal, Some(&mask));
        assert_eq!(dst.get_pixel(0, 0), WHITE, "masked-out pixel unchanged");
        assert_eq!(dst.get_pixel(1, 0), RED, "unmasked pixel fully painted");
    }

    #[test]
    fn composite_from_skips_transparent_source_pixels() {
        let mut dst = PixelBuffer::new_filled(2, 1, WHITE);
        let mut src = PixelBuffer::new_transparent(2, 1);
        src.set_pixel(1, 0, RED);
        dst.composite_from(&src, 1.0, BlendMode::Normal, None);
        assert_eq!(dst.get_pixel(0, 0), WHITE, "transparent src leaves dst");
        assert_eq!(dst.get_pixel(1, 0), RED);
    }

    #[test]
    fn composite_from_uses_blend_mode() {
        let mut dst = PixelBuffer::new_filled(1, 1, [200, 200, 200, 255]);
        let src = PixelBuffer::new_filled(1, 1, [128, 128, 128, 255]);
        dst.composite_from(&src, 1.0, BlendMode::Multiply, None);
        // Multiply darkens: 200/255 * 128/255 ~= 100.
        assert!(dst.get_pixel(0, 0)[0] < 170, "multiply should darken");
        // The buffer's own blend mode is restored to Normal afterwards.
        assert_eq!(dst.blend_mode, BlendMode::Normal);
    }

    #[test]
    fn knockout_from_replaces_rather_than_blends() {
        // Knockout: a semi-transparent source replaces the destination's color
        // outright (alpha scaled), it does not composite over it.
        let mut dst = PixelBuffer::new_filled(1, 1, WHITE);
        let src = PixelBuffer::new_filled(1, 1, [10, 20, 30, 128]);
        dst.knockout_from(&src, 1.0, None);
        let p = dst.get_pixel(0, 0);
        assert_eq!([p[0], p[1], p[2]], [10, 20, 30], "color replaced outright");
        assert!((p[3] as i32 - 128).abs() <= 1, "alpha scaled, got {}", p[3]);
    }

    #[test]
    fn from_alpha_channel_reads_alpha_not_luminosity() {
        let mut buf = PixelBuffer::new_transparent(2, 1);
        buf.set_pixel(0, 0, [255, 255, 255, 64]); // white but low alpha
        buf.set_pixel(1, 0, [0, 0, 0, 200]); // black but high alpha
        let mask = AlphaMask::from_alpha_channel(&buf);
        assert!((mask.get(0, 0) - 64.0 / 255.0).abs() < 0.01);
        assert!((mask.get(1, 0) - 200.0 / 255.0).abs() < 0.01);
    }

    #[test]
    fn apply_transfer_lut_remaps_mask_values() {
        let mut mask = AlphaMask::all_opaque(1, 1);
        mask.set(0, 0, 100);
        // Inversion LUT: out = 255 - in.
        let mut lut = [0u8; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            *v = 255 - i as u8;
        }
        mask.apply_transfer_lut(&lut);
        assert!((mask.get(0, 0) - (155.0 / 255.0)).abs() < 0.01);
    }

    #[test]
    fn new_transparent_accumulates_alpha() {
        let mut buf = PixelBuffer::new_transparent(1, 1);
        buf.blend_pixel(0, 0, [255, 0, 0, 128], 1.0);
        let first = buf.get_pixel(0, 0);
        assert!(
            (first[3] as i32 - 128).abs() <= 1,
            "first alpha should be about 128: {:?}",
            first
        );

        buf.blend_pixel(0, 0, [255, 0, 0, 128], 1.0);
        let second = buf.get_pixel(0, 0);
        assert!(
            second[3] > first[3],
            "second semi-transparent paint should accumulate alpha: {:?} -> {:?}",
            first,
            second
        );
    }
}
