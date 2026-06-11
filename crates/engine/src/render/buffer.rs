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
            (
                bm.blend_channel(src_r, dst_r),
                bm.blend_channel(src_g, dst_g),
                bm.blend_channel(src_b, dst_b),
            )
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
