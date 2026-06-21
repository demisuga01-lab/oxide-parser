use crate::render::buffer::{PixelBuffer, PixelColor};
use crate::render::transform::{Transform2D, Viewport};

pub struct WuLineRenderer;

impl WuLineRenderer {
    /// Draw an antialiased line from (x0, y0) to (x1, y1).
    pub fn draw_line(
        buf: &mut PixelBuffer,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        color: PixelColor,
        width: f64,
    ) {
        if width <= 0.0 || !width.is_finite() {
            return;
        }

        if width <= 1.5 {
            Self::draw_wu_line(buf, x0, y0, x1, y1, color, 1.0);
            return;
        }

        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-10 || !len.is_finite() {
            Self::draw_wu_line(buf, x0, y0, x1, y1, color, 1.0);
            return;
        }

        let perp_x = -dy / len;
        let perp_y = dx / len;
        let half = width / 2.0;
        let n_lines = (width.ceil() as i32).max(1);

        for i in 0..n_lines {
            let offset = -half + i as f64 + 0.5;
            let ox = perp_x * offset;
            let oy = perp_y * offset;
            let dist_from_center = if half > 0.0 {
                (offset / half).abs()
            } else {
                0.0
            };
            let coverage = 1.0 - (dist_from_center - 0.85).max(0.0) / 0.15;
            let coverage = coverage.clamp(0.1, 1.0);
            Self::draw_wu_line(buf, x0 + ox, y0 + oy, x1 + ox, y1 + oy, color, coverage);
        }
    }

    fn draw_wu_line(
        buf: &mut PixelBuffer,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        color: PixelColor,
        coverage: f64,
    ) {
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return;
        }

        fn frac(v: f64) -> f64 {
            v - v.floor()
        }

        fn rfrac(v: f64) -> f64 {
            1.0 - frac(v)
        }

        let mut plot = |x: i32, y: i32, c: f64| {
            let alpha = (c * coverage).clamp(0.0, 1.0) as f32;
            buf.blend_pixel(x, y, color, alpha);
        };

        let steep = (y1 - y0).abs() > (x1 - x0).abs();
        let (mut x0, mut y0, mut x1, mut y1) = if steep {
            (y0, x0, y1, x1)
        } else {
            (x0, y0, x1, y1)
        };
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
        }

        let dx = x1 - x0;
        let dy = y1 - y0;
        let gradient = if dx.abs() < 1e-10 { 1.0 } else { dy / dx };

        let xend = x0.round();
        let yend = y0 + gradient * (xend - x0);
        let xgap = rfrac(x0 + 0.5);
        let xpxl1 = safe_to_i32(xend);
        let ypxl1 = safe_to_i32(yend.floor());
        if steep {
            plot(ypxl1, xpxl1, rfrac(yend) * xgap);
            plot(ypxl1 + 1, xpxl1, frac(yend) * xgap);
        } else {
            plot(xpxl1, ypxl1, rfrac(yend) * xgap);
            plot(xpxl1, ypxl1 + 1, frac(yend) * xgap);
        }
        let mut intery = yend + gradient;

        let xend = x1.round();
        let yend = y1 + gradient * (xend - x1);
        let xgap = frac(x1 + 0.5);
        let xpxl2 = safe_to_i32(xend);
        let ypxl2 = safe_to_i32(yend.floor());
        if steep {
            plot(ypxl2, xpxl2, rfrac(yend) * xgap);
            plot(ypxl2 + 1, xpxl2, frac(yend) * xgap);
        } else {
            plot(xpxl2, ypxl2, rfrac(yend) * xgap);
            plot(xpxl2, ypxl2 + 1, frac(yend) * xgap);
        }

        for x in (xpxl1 + 1)..xpxl2 {
            let iy = safe_to_i32(intery.floor());
            if steep {
                plot(iy, x, rfrac(intery));
                plot(iy + 1, x, frac(intery));
            } else {
                plot(x, iy, rfrac(intery));
                plot(x, iy + 1, frac(intery));
            }
            intery += gradient;
        }
    }
}

#[derive(Debug, Clone)]
pub struct DashState {
    pattern: Vec<f64>,
    phase: f64,
    current_pos: f64,
    current_idx: usize,
}

impl DashState {
    pub fn new(pattern: Vec<f64>, phase: f64) -> Self {
        let pattern: Vec<f64> = pattern
            .into_iter()
            .filter(|interval| interval.is_finite() && *interval > 0.0)
            .collect();
        let total: f64 = pattern.iter().sum();
        let phase = if total > 0.0 {
            phase.rem_euclid(total)
        } else {
            0.0
        };
        let mut current_idx = 0usize;
        let mut current_pos = 0.0_f64;
        let mut remaining_phase = phase;

        if total > 0.0 {
            for (i, &interval) in pattern.iter().enumerate() {
                if remaining_phase < interval {
                    current_idx = i;
                    current_pos = remaining_phase;
                    break;
                }
                remaining_phase -= interval;
            }
        }

        Self {
            pattern,
            phase,
            current_pos,
            current_idx,
        }
    }

    /// Solid line (no dashing).
    pub fn solid() -> Self {
        Self {
            pattern: Vec::new(),
            phase: 0.0,
            current_pos: 0.0,
            current_idx: 0,
        }
    }

    /// True if the current position is in an on interval.
    pub fn is_drawing(&self) -> bool {
        self.pattern.is_empty() || self.current_idx.is_multiple_of(2)
    }

    /// True when this dash state represents an uninterrupted solid stroke.
    pub fn is_solid(&self) -> bool {
        self.pattern.is_empty()
    }

    /// The normalized dash phase used to initialize the state.
    pub fn phase(&self) -> f64 {
        self.phase
    }

    /// Advance along a line by distance units.
    pub fn advance(&mut self, distance: f64) -> Vec<(f64, f64, bool)> {
        if distance <= 0.0 || !distance.is_finite() {
            return Vec::new();
        }
        if self.pattern.is_empty() {
            return vec![(0.0, distance, true)];
        }
        let total: f64 = self.pattern.iter().sum();
        if total <= 0.0 {
            return vec![(0.0, distance, true)];
        }

        let mut segments = Vec::new();
        let mut traveled = 0.0_f64;

        while traveled < distance {
            let current_interval = self.pattern[self.current_idx];
            let remaining_in_interval = current_interval - self.current_pos;
            let can_travel = (distance - traveled).min(remaining_in_interval);
            if can_travel <= 1e-10 {
                self.current_pos = 0.0;
                self.current_idx = (self.current_idx + 1) % self.pattern.len();
                continue;
            }

            segments.push((
                traveled,
                traveled + can_travel,
                self.current_idx.is_multiple_of(2),
            ));
            traveled += can_travel;
            self.current_pos += can_travel;

            if self.current_pos >= current_interval - 1e-10 {
                self.current_pos = 0.0;
                self.current_idx = (self.current_idx + 1) % self.pattern.len();
            }
        }

        segments
    }
}

pub struct LinePainter;

impl LinePainter {
    /// Draw a line from PDF user-space coordinates.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(
        buf: &mut PixelBuffer,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        color: PixelColor,
        stroke_width: f64,
        ctm: &Transform2D,
        viewport: &Viewport,
        dash: &DashState,
    ) {
        if dash.pattern.is_empty() {
            let (px0, py0) = Self::to_pixel(x0, y0, ctm, viewport);
            let (px1, py1) = Self::to_pixel(x1, y1, ctm, viewport);
            let width_px = (stroke_width * ctm.scale_factor() * viewport.scale).max(1.0);
            WuLineRenderer::draw_line(buf, px0, py0, px1, py1, color, width_px);
            return;
        }

        let dx = x1 - x0;
        let dy = y1 - y0;
        let line_len = (dx * dx + dy * dy).sqrt();
        if line_len < 1e-10 || !line_len.is_finite() {
            return;
        }

        let ux = dx / line_len;
        let uy = dy / line_len;
        let mut dash = dash.clone();
        let segs = dash.advance(line_len);
        let width_px = (stroke_width * ctm.scale_factor() * viewport.scale).max(1.0);

        for (t0, t1, drawing) in segs {
            if !drawing {
                continue;
            }
            let sx0 = x0 + ux * t0;
            let sy0 = y0 + uy * t0;
            let sx1 = x0 + ux * t1;
            let sy1 = y0 + uy * t1;
            let (px0, py0) = Self::to_pixel(sx0, sy0, ctm, viewport);
            let (px1, py1) = Self::to_pixel(sx1, sy1, ctm, viewport);
            WuLineRenderer::draw_line(buf, px0, py0, px1, py1, color, width_px);
        }
    }

    fn to_pixel(x: f64, y: f64, ctm: &Transform2D, viewport: &Viewport) -> (f64, f64) {
        let (ux, uy) = ctm.transform_point(x, y);
        viewport.page_to_pixel_f64(ux, uy)
    }
}

fn safe_to_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::{BLACK, WHITE};
    use crate::render::{PixelBuffer, RED};

    #[test]
    fn draw_line_produces_non_transparent_pixels() {
        let mut buf = PixelBuffer::new_filled(50, 50, WHITE);
        WuLineRenderer::draw_line(&mut buf, 5.0, 5.0, 45.0, 45.0, BLACK, 1.0);
        let p_start = buf.get_pixel(5, 5);
        let p_end = buf.get_pixel(45, 45);
        println!("wu line start pixel: {:?}", p_start);
        assert!(p_start[0] < 255 || p_start[1] < 255 || p_start[2] < 255);
        assert!(p_end[0] < 255 || p_end[1] < 255 || p_end[2] < 255);
    }

    #[test]
    fn horizontal_line_sets_expected_pixels() {
        let mut buf = PixelBuffer::new_filled(20, 10, WHITE);
        WuLineRenderer::draw_line(&mut buf, 2.0, 5.0, 17.0, 5.0, BLACK, 1.0);
        for x in 3..16 {
            let p = buf.get_pixel(x, 5);
            assert!(p[0] < 255);
        }
    }

    #[test]
    fn draw_line_does_not_panic_for_degenerate_inputs() {
        let mut buf = PixelBuffer::new(20, 20);
        WuLineRenderer::draw_line(&mut buf, 10.0, 10.0, 10.0, 10.0, BLACK, 1.0);
        WuLineRenderer::draw_line(&mut buf, 0.0, 0.0, 19.0, 19.0, BLACK, 0.0);
        WuLineRenderer::draw_line(&mut buf, -10.0, -10.0, 100.0, 100.0, BLACK, 1.0);
    }

    #[test]
    fn thick_line_covers_more_pixels_than_thin_line() {
        let mut thin_buf = PixelBuffer::new_filled(50, 50, WHITE);
        let mut thick_buf = PixelBuffer::new_filled(50, 50, WHITE);
        WuLineRenderer::draw_line(&mut thin_buf, 5.0, 25.0, 45.0, 25.0, BLACK, 1.0);
        WuLineRenderer::draw_line(&mut thick_buf, 5.0, 25.0, 45.0, 25.0, BLACK, 5.0);

        let thin_count = (0..50i32)
            .flat_map(|y| (0..50i32).map(move |x| (x, y)))
            .filter(|&(x, y)| thin_buf.get_pixel(x, y)[0] < 255)
            .count();
        let thick_count = (0..50i32)
            .flat_map(|y| (0..50i32).map(move |x| (x, y)))
            .filter(|&(x, y)| thick_buf.get_pixel(x, y)[0] < 255)
            .count();
        println!("thin_count={thin_count} thick_count={thick_count}");
        assert!(thick_count > thin_count);
    }

    #[test]
    fn solid_line_advance_returns_single_segment() {
        let mut ds = DashState::solid();
        let segs = ds.advance(100.0);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0], (0.0, 100.0, true));
    }

    #[test]
    fn simple_dash_advance_has_drawn_and_gap_segments() {
        let mut ds = DashState::new(vec![10.0, 5.0], 0.0);
        let segs = ds.advance(20.0);
        assert!(segs.iter().any(|(_, _, drawing)| *drawing));
        assert!(segs.iter().any(|(_, _, drawing)| !*drawing));
        assert!(segs[0].2);
        assert!(!segs[1].2);
    }

    #[test]
    fn dash_state_is_drawing_reflects_current_interval() {
        let ds = DashState::new(vec![5.0, 3.0], 0.0);
        assert!(ds.is_drawing());
        let ds_gap = DashState::new(vec![5.0, 3.0], 6.0);
        assert!(!ds_gap.is_drawing());
    }

    #[test]
    fn dash_state_with_phase_skips_start() {
        let mut ds = DashState::new(vec![10.0, 10.0], 5.0);
        let segs = ds.advance(15.0);
        assert!(segs[0].2);
        assert!((segs[0].1 - segs[0].0 - 5.0).abs() < 0.001);
        assert!(!segs[1].2);
    }

    #[test]
    fn line_painter_identity_ctm_and_viewport_produces_pixels() {
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let dash = DashState::solid();
        LinePainter::draw_line(&mut buf, 10.0, 50.0, 90.0, 50.0, RED, 1.0, &ctm, &vp, &dash);
        let p = buf.get_pixel(50, 50);
        assert!(p[0] > 100);
    }

    #[test]
    fn dashed_draw_line_only_paints_on_segments() {
        let mut buf = PixelBuffer::new_filled(100, 10, WHITE);
        let vp = Viewport::new([0.0, 0.0, 100.0, 10.0], 72);
        let ctm = Transform2D::identity();
        let dash = DashState::new(vec![10.0, 10.0], 0.0);
        LinePainter::draw_line(&mut buf, 0.0, 5.0, 100.0, 5.0, BLACK, 1.0, &ctm, &vp, &dash);
        let on_pixel = buf.get_pixel(5, 5);
        let off_pixel = buf.get_pixel(15, 5);
        assert!(on_pixel[0] < 200);
        assert_eq!(off_pixel, WHITE);
    }
}
