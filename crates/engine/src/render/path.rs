use crate::content::state::LineCap;
use crate::render::buffer::{PixelBuffer, PixelColor};
use crate::render::line::{DashState, WuLineRenderer};
use crate::render::transform::{Transform2D, Viewport};

#[derive(Debug, Clone, PartialEq)]
pub enum PathSegment {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CubicTo {
        cp1x: f64,
        cp1y: f64,
        cp2x: f64,
        cp2y: f64,
        x: f64,
        y: f64,
    },
    ClosePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Default)]
pub struct Path {
    pub segments: Vec<PathSegment>,
    pub current_point: Option<(f64, f64)>,
    subpath_start: Option<(f64, f64)>,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.segments.push(PathSegment::MoveTo(x, y));
        self.current_point = Some((x, y));
        self.subpath_start = Some((x, y));
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        if self.current_point.is_none() {
            self.move_to(x, y);
            return;
        }
        self.segments.push(PathSegment::LineTo(x, y));
        self.current_point = Some((x, y));
    }

    pub fn curve_to(&mut self, cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64) {
        if self.current_point.is_none() {
            self.move_to(x, y);
            return;
        }
        self.segments.push(PathSegment::CubicTo {
            cp1x,
            cp1y,
            cp2x,
            cp2y,
            x,
            y,
        });
        self.current_point = Some((x, y));
    }

    pub fn close(&mut self) {
        if self.subpath_start.is_some() {
            self.segments.push(PathSegment::ClosePath);
            self.current_point = self.subpath_start;
        }
    }

    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close();
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn clear(&mut self) {
        self.segments.clear();
        self.current_point = None;
        self.subpath_start = None;
    }
}

#[derive(Debug, Clone, Default)]
pub struct FlatPath {
    pub subpaths: Vec<Vec<(f64, f64)>>,
    pub closed: Vec<bool>,
}

/// Distance from point P to the line through A and B.
pub(crate) fn point_to_line_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-10 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    ((dy * p.0 - dx * p.1 + b.0 * a.1 - b.1 * a.0) / len).abs()
}

fn midpoint(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

/// Flatten a cubic Bezier curve into endpoint/intermediate points.
pub fn flatten_cubic(
    p0: (f64, f64),
    p1: (f64, f64),
    p2: (f64, f64),
    p3: (f64, f64),
    threshold: f64,
    max_depth: u32,
    out: &mut Vec<(f64, f64)>,
) {
    if max_depth == 0 {
        out.push(p3);
        return;
    }

    let threshold = threshold.max(0.01);
    let d1 = point_to_line_dist(p1, p0, p3);
    let d2 = point_to_line_dist(p2, p0, p3);
    if d1 <= threshold && d2 <= threshold {
        out.push(p3);
        return;
    }

    let q01 = midpoint(p0, p1);
    let q12 = midpoint(p1, p2);
    let q23 = midpoint(p2, p3);
    let q012 = midpoint(q01, q12);
    let q123 = midpoint(q12, q23);
    let q0123 = midpoint(q012, q123);

    flatten_cubic(p0, q01, q012, q0123, threshold, max_depth - 1, out);
    flatten_cubic(q0123, q123, q23, p3, threshold, max_depth - 1, out);
}

/// Flatten a path from PDF user space to pixel-space polylines.
pub fn flatten_path(
    path: &Path,
    ctm: &Transform2D,
    viewport: &Viewport,
    bezier_threshold: f64,
) -> FlatPath {
    let mut flat = FlatPath::default();
    let mut current_subpath = Vec::new();
    let mut current_start: Option<(f64, f64)> = None;
    let mut is_closed = false;
    let mut pen = (0.0, 0.0);

    let to_px = |x: f64, y: f64| -> (f64, f64) {
        let (ux, uy) = ctm.transform_point(x, y);
        viewport.page_to_pixel_f64(ux, uy)
    };

    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                if !current_subpath.is_empty() {
                    flat.subpaths.push(std::mem::take(&mut current_subpath));
                    flat.closed.push(is_closed);
                }
                is_closed = false;
                let px = to_px(x, y);
                pen = (x, y);
                current_start = Some(px);
                current_subpath.push(px);
            }
            PathSegment::LineTo(x, y) => {
                let px = to_px(x, y);
                current_subpath.push(px);
                pen = (x, y);
            }
            PathSegment::CubicTo {
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x,
                y,
            } => {
                let p0 = to_px(pen.0, pen.1);
                let p1 = to_px(cp1x, cp1y);
                let p2 = to_px(cp2x, cp2y);
                let p3 = to_px(x, y);
                flatten_cubic(p0, p1, p2, p3, bezier_threshold, 16, &mut current_subpath);
                pen = (x, y);
            }
            PathSegment::ClosePath => {
                if let Some(start) = current_start {
                    current_subpath.push(start);
                }
                is_closed = true;
            }
        }
    }

    if !current_subpath.is_empty() {
        flat.subpaths.push(current_subpath);
        flat.closed.push(is_closed);
    }

    flat
}

pub struct PathPainter;

impl PathPainter {
    pub fn stroke(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        stroke_width: f64,
        dash: &DashState,
    ) {
        Self::stroke_internal(buf, path, ctm, viewport, color, stroke_width, dash, None);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_with_cap(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        stroke_width: f64,
        dash: &DashState,
        cap: &LineCap,
    ) {
        Self::stroke_internal(
            buf,
            path,
            ctm,
            viewport,
            color,
            stroke_width,
            dash,
            Some(cap),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_internal(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        stroke_width: f64,
        dash: &DashState,
        cap: Option<&LineCap>,
    ) {
        if path.is_empty() {
            return;
        }

        let flat = flatten_path(path, ctm, viewport, 0.5);
        let width_px = (stroke_width * ctm.scale_factor() * viewport.scale).max(1.0);
        let half_width = width_px / 2.0;

        for (idx, subpath) in flat.subpaths.iter().enumerate() {
            if subpath.len() < 2 {
                continue;
            }

            for window in subpath.windows(2) {
                let (x0, y0) = window[0];
                let (x1, y1) = window[1];
                let dx = x1 - x0;
                let dy = y1 - y0;
                let seg_len = (dx * dx + dy * dy).sqrt();
                if seg_len < 1e-10 || !seg_len.is_finite() {
                    continue;
                }

                let ux = dx / seg_len;
                let uy = dy / seg_len;
                // TODO(dash): maintain continuous dash state across the entire path.
                let mut dash_copy = dash.clone();
                for (t0, t1, drawing) in dash_copy.advance(seg_len) {
                    if !drawing {
                        continue;
                    }
                    let sx0 = x0 + ux * t0;
                    let sy0 = y0 + uy * t0;
                    let sx1 = x0 + ux * t1;
                    let sy1 = y0 + uy * t1;
                    WuLineRenderer::draw_line(buf, sx0, sy0, sx1, sy1, color, width_px);
                }
            }

            if let Some(cap) = cap {
                let is_closed = flat.closed.get(idx).copied().unwrap_or(false);
                if !is_closed && subpath.len() >= 2 {
                    let (sx0, sy0) = subpath[0];
                    let (sx1, sy1) = subpath[subpath.len() - 1];
                    Self::draw_end_caps(buf, sx0, sy0, sx1, sy1, color, half_width, cap);
                }
            }
        }
    }

    pub fn fill(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        rule: FillRule,
    ) {
        if path.is_empty() {
            return;
        }

        let flat = flatten_path(path, ctm, viewport, 0.5);
        let mut edges = Vec::new();

        for subpath in &flat.subpaths {
            for window in subpath.windows(2) {
                let (x0, y0) = window[0];
                let (x1, y1) = window[1];
                if (y0 - y1).abs() < 1e-10 {
                    continue;
                }

                let (x_start, y_start, x_end, y_end) = if y0 < y1 {
                    (x0, y0, x1, y1)
                } else {
                    (x1, y1, x0, y0)
                };
                let winding = if y0 < y1 { 1 } else { -1 };
                edges.push(Edge {
                    y_min: y_start,
                    y_max: y_end,
                    x_at_ymin: x_start,
                    slope: (x_end - x_start) / (y_end - y_start),
                    winding,
                });
            }
        }

        if edges.is_empty() || buf.width == 0 || buf.height == 0 {
            return;
        }

        let y_min = edges
            .iter()
            .map(|e| safe_floor_i32(e.y_min))
            .min()
            .unwrap_or(0)
            .max(0);
        let y_max = edges
            .iter()
            .map(|e| safe_ceil_i32(e.y_max))
            .max()
            .unwrap_or(0)
            .min(buf.height as i32 - 1);

        for y in y_min..=y_max {
            let y_f = y as f64 + 0.5;
            let mut intersections: Vec<(f64, i32)> = edges
                .iter()
                .filter(|e| e.y_min <= y_f && y_f < e.y_max)
                .map(|e| {
                    let x = e.x_at_ymin + e.slope * (y_f - e.y_min);
                    (x, e.winding)
                })
                .collect();

            if intersections.is_empty() {
                continue;
            }

            intersections
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let spans = Self::compute_fill_spans(&intersections, rule);
            for (x_start, x_end) in spans {
                let px_start = safe_ceil_i32(x_start).max(0);
                let px_end = safe_floor_i32(x_end).min(buf.width as i32 - 1);
                if px_start > px_end {
                    continue;
                }
                buf.fill_rect(px_start, y, px_end - px_start + 1, 1, color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_rect(
        buf: &mut PixelBuffer,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
    ) {
        if !ctm.is_axis_aligned() {
            let mut path = Path::new();
            path.rect(x, y, w, h);
            Self::fill(buf, &path, ctm, viewport, color, FillRule::NonZero);
            return;
        }

        let (ux0, uy0) = ctm.transform_point(x, y);
        let (px0, py0) = viewport.page_to_pixel_f64(ux0, uy0);
        let (ux1, uy1) = ctm.transform_point(x + w, y + h);
        let (px1, py1) = viewport.page_to_pixel_f64(ux1, uy1);

        let rx_min = safe_ceil_i32(px0.min(px1));
        let ry_min = safe_ceil_i32(py0.min(py1));
        let rx_max = safe_floor_i32(px0.max(px1));
        let ry_max = safe_floor_i32(py0.max(py1));
        let rw = (rx_max - rx_min + 1).max(0);
        let rh = (ry_max - ry_min + 1).max(0);
        buf.fill_rect(rx_min, ry_min, rw, rh, color);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rect(
        buf: &mut PixelBuffer,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        stroke_width: f64,
    ) {
        let mut path = Path::new();
        path.rect(x, y, w, h);
        Self::stroke(
            buf,
            &path,
            ctm,
            viewport,
            color,
            stroke_width,
            &DashState::solid(),
        );
    }

    fn compute_fill_spans(intersections: &[(f64, i32)], rule: FillRule) -> Vec<(f64, f64)> {
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
                if let Some(start) = span_start {
                    log::warn!("PathPainter::fill: unclosed winding span at x={}", start);
                }
            }
        }
        spans
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_end_caps(
        buf: &mut PixelBuffer,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        color: PixelColor,
        half_width: f64,
        cap: &LineCap,
    ) {
        match cap {
            LineCap::Butt => {}
            LineCap::ProjectingSquare => {
                let dx = x1 - x0;
                let dy = y1 - y0;
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1e-10 || !len.is_finite() {
                    return;
                }
                let ux = dx / len * half_width;
                let uy = dy / len * half_width;
                WuLineRenderer::draw_line(buf, x0 - ux, y0 - uy, x0, y0, color, half_width * 2.0);
                WuLineRenderer::draw_line(buf, x1, y1, x1 + ux, y1 + uy, color, half_width * 2.0);
            }
            LineCap::Round => {
                Self::draw_round_cap(buf, x0, y0, color, half_width);
                Self::draw_round_cap(buf, x1, y1, color, half_width);
            }
        }
    }

    fn draw_round_cap(buf: &mut PixelBuffer, cx: f64, cy: f64, color: PixelColor, radius: f64) {
        if radius < 0.5 || !radius.is_finite() {
            return;
        }
        for i in 0..16 {
            let angle = i as f64 * std::f64::consts::TAU / 16.0;
            let ex = cx + radius * angle.cos();
            let ey = cy + radius * angle.sin();
            WuLineRenderer::draw_line(buf, cx, cy, ex, ey, color, 1.0);
        }
    }
}

#[derive(Debug, Clone)]
struct Edge {
    y_min: f64,
    y_max: f64,
    x_at_ymin: f64,
    slope: f64,
    winding: i32,
}

fn safe_floor_i32(value: f64) -> i32 {
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

fn safe_ceil_i32(value: f64) -> i32 {
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
    use crate::render::buffer::{BLACK, BLUE, GREEN, RED, TRANSPARENT, WHITE};

    #[test]
    fn empty_path_is_empty() {
        assert!(Path::new().is_empty());
    }

    #[test]
    fn move_to_and_line_to() {
        let mut p = Path::new();
        p.move_to(0.0, 0.0);
        p.line_to(100.0, 0.0);
        assert!(!p.is_empty());
        assert_eq!(p.segments.len(), 2);
        assert!(matches!(p.segments[0], PathSegment::MoveTo(0.0, 0.0)));
        assert!(matches!(p.segments[1], PathSegment::LineTo(100.0, 0.0)));
    }

    #[test]
    fn rect_adds_five_segments() {
        let mut p = Path::new();
        p.rect(10.0, 20.0, 50.0, 30.0);
        assert_eq!(p.segments.len(), 5);
        assert!(matches!(p.segments[0], PathSegment::MoveTo(..)));
        assert!(matches!(p.segments[4], PathSegment::ClosePath));
    }

    #[test]
    fn clear_resets_path() {
        let mut p = Path::new();
        p.rect(0.0, 0.0, 100.0, 100.0);
        p.clear();
        assert!(p.is_empty());
        assert!(p.current_point.is_none());
    }

    #[test]
    fn close_path_sets_current_point_to_subpath_start() {
        let mut p = Path::new();
        p.move_to(50.0, 50.0);
        p.line_to(100.0, 100.0);
        p.close();
        assert_eq!(p.current_point, Some((50.0, 50.0)));
    }

    #[test]
    fn straight_line_cubic_is_not_subdivided() {
        let mut out = Vec::new();
        flatten_cubic(
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 0.0),
            0.5,
            16,
            &mut out,
        );
        assert_eq!(out, vec![(3.0, 0.0)]);
    }

    #[test]
    fn curved_bezier_is_subdivided() {
        let mut out = Vec::new();
        flatten_cubic(
            (0.0, 1.0),
            (0.552, 1.0),
            (1.0, 0.552),
            (1.0, 0.0),
            0.05,
            16,
            &mut out,
        );
        assert!(out.len() > 1);
        let last = out.last().copied().unwrap_or((0.0, 0.0));
        assert!((last.0 - 1.0).abs() < 0.01);
        assert!(last.1.abs() < 0.01);
    }

    #[test]
    fn point_to_line_dist_for_known_geometry() {
        let d = point_to_line_dist((0.0, 1.0), (0.0, 0.0), (1.0, 0.0));
        assert!((d - 1.0).abs() < 1e-10);
        let d2 = point_to_line_dist((0.5, 0.0), (0.0, 0.0), (1.0, 0.0));
        assert!(d2 < 1e-10);
    }

    #[test]
    fn stroke_horizontal_line_produces_dark_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.move_to(10.0, 50.0);
        path.line_to(90.0, 50.0);
        PathPainter::stroke(&mut buf, &path, &ctm, &vp, BLACK, 2.0, &DashState::solid());
        let mid = buf.get_pixel(50, 50);
        assert!(mid[0] < 200, "midpoint pixel should be dark: {mid:?}");
    }

    #[test]
    fn hairline_stroke_renders_as_one_pixel() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.move_to(10.0, 50.0);
        path.line_to(90.0, 50.0);

        PathPainter::stroke(&mut buf, &path, &ctm, &vp, BLACK, 0.0, &DashState::solid());

        let mid = buf.get_pixel(50, 50);
        assert!(mid[0] < 200, "hairline stroke should be visible: {mid:?}");
    }

    #[test]
    fn fill_rectangle_produces_filled_region() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.rect(20.0, 20.0, 60.0, 60.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);
        let center = buf.get_pixel(50, 50);
        println!("nonzero fill center: {:?}", center);
        assert_eq!(center, RED);
        assert_eq!(buf.get_pixel(5, 5), WHITE);
    }

    #[test]
    fn fill_rect_fast_path_fills_correct_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        PathPainter::fill_rect(&mut buf, 10.0, 10.0, 80.0, 80.0, &ctm, &vp, BLUE);
        assert_eq!(buf.get_pixel(50, 50), BLUE);
        assert_eq!(buf.get_pixel(5, 5), WHITE);
    }

    #[test]
    fn evenodd_and_nonzero_fill_both_paint_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf_eo = PixelBuffer::new_filled(100, 100, WHITE);
        let mut buf_nz = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.rect(10.0, 10.0, 50.0, 50.0);
        path.rect(30.0, 30.0, 50.0, 50.0);
        PathPainter::fill(&mut buf_eo, &path, &ctm, &vp, RED, FillRule::EvenOdd);
        PathPainter::fill(&mut buf_nz, &path, &ctm, &vp, RED, FillRule::NonZero);
        let eo_has_red = (0..100i32)
            .flat_map(|y| (0..100i32).map(move |x| (x, y)))
            .any(|(x, y)| buf_eo.get_pixel(x, y) == RED);
        let nz_has_red = (0..100i32)
            .flat_map(|y| (0..100i32).map(move |x| (x, y)))
            .any(|(x, y)| buf_nz.get_pixel(x, y) == RED);
        println!(
            "overlap eo={:?} nz={:?}",
            buf_eo.get_pixel(40, 50),
            buf_nz.get_pixel(40, 50)
        );
        assert!(eo_has_red);
        assert!(nz_has_red);
        assert_eq!(buf_eo.get_pixel(40, 50), WHITE);
        assert_eq!(buf_nz.get_pixel(40, 50), RED);
    }

    #[test]
    fn empty_path_does_not_panic() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let path = Path::new();
        PathPainter::stroke(&mut buf, &path, &ctm, &vp, BLACK, 1.0, &DashState::solid());
        PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);
        assert_eq!(buf.get_pixel(5, 5), WHITE);
    }

    #[test]
    fn curve_to_adds_cubic_segment() {
        let mut p = Path::new();
        p.move_to(0.0, 0.0);
        p.curve_to(10.0, 20.0, 30.0, 20.0, 40.0, 0.0);
        assert_eq!(p.segments.len(), 2);
        assert!(matches!(
            p.segments[1],
            PathSegment::CubicTo {
                cp1x,
                cp2x,
                x,
                ..
            } if cp1x == 10.0 && cp2x == 30.0 && x == 40.0
        ));
    }

    #[test]
    fn line_to_without_move_to_creates_implicit_move() {
        let mut p = Path::new();
        p.line_to(50.0, 50.0);
        assert!(!p.is_empty());
        assert!(p.current_point.is_some());
    }

    #[test]
    fn multiple_subpaths_in_one_path() {
        let mut p = Path::new();
        p.move_to(0.0, 0.0);
        p.line_to(50.0, 0.0);
        p.move_to(0.0, 50.0);
        p.line_to(50.0, 50.0);
        assert_eq!(p.segments.len(), 4);
    }

    #[test]
    fn flatten_path_with_pure_line_segments() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut p = Path::new();
        p.move_to(0.0, 0.0);
        p.line_to(50.0, 0.0);
        p.line_to(50.0, 50.0);
        let flat = flatten_path(&p, &ctm, &vp, 0.5);
        assert_eq!(flat.subpaths.len(), 1);
        assert_eq!(flat.subpaths[0].len(), 3);
        assert!(!flat.closed[0]);
    }

    #[test]
    fn flatten_path_closed_rect_has_closed_subpath() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut p = Path::new();
        p.rect(0.0, 0.0, 50.0, 50.0);
        let flat = flatten_path(&p, &ctm, &vp, 0.5);
        assert_eq!(flat.subpaths.len(), 1);
        assert_eq!(flat.subpaths[0].len(), 5);
        assert!(flat.closed[0]);
    }

    #[test]
    fn flatten_path_with_ctm_scale_doubles_pixel_coords() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::scale(2.0, 2.0);
        let mut p = Path::new();
        p.move_to(10.0, 10.0);
        p.line_to(20.0, 10.0);
        let flat = flatten_path(&p, &ctm, &vp, 0.5);
        let (px0, _) = flat.subpaths[0][0];
        let (px1, _) = flat.subpaths[0][1];
        assert!((px0 - 20.0).abs() < 1.0);
        assert!(((px1 - px0).abs() - 20.0).abs() < 1.0);
    }

    #[test]
    fn stroke_does_not_modify_far_corners() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut p = Path::new();
        p.move_to(40.0, 40.0);
        p.line_to(60.0, 40.0);
        PathPainter::stroke(&mut buf, &p, &ctm, &vp, BLACK, 1.0, &DashState::solid());
        assert_eq!(buf.get_pixel(0, 0), WHITE);
        assert_eq!(buf.get_pixel(99, 99), WHITE);
    }

    #[test]
    fn fill_rect_with_identity_ctm_uses_fast_path() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        PathPainter::fill_rect(&mut buf, 25.0, 25.0, 50.0, 50.0, &ctm, &vp, GREEN);
        assert_eq!(buf.get_pixel(50, 50), GREEN);
        assert_eq!(buf.get_pixel(20, 20), WHITE);
    }

    #[test]
    fn stroke_rect_produces_border_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        PathPainter::stroke_rect(&mut buf, 20.0, 20.0, 60.0, 60.0, &ctm, &vp, BLACK, 1.0);
        let top_edge = buf.get_pixel(50, 20);
        assert!(top_edge[0] < 200, "top edge should be dark: {top_edge:?}");
    }

    #[test]
    fn stroke_with_round_cap_draws_endpoint_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut p = Path::new();
        p.move_to(30.0, 50.0);
        p.line_to(70.0, 50.0);
        PathPainter::stroke_with_cap(
            &mut buf,
            &p,
            &ctm,
            &vp,
            BLACK,
            6.0,
            &DashState::solid(),
            &LineCap::Round,
        );
        assert_ne!(buf.get_pixel(30, 50), TRANSPARENT);
    }
}
