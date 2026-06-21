use crate::content::state::{LineCap, LineJoin};
use crate::render::buffer::{PixelBuffer, PixelColor};
use crate::render::line::DashState;
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphHinting {
    pixel_size: f64,
}

impl GlyphHinting {
    pub fn disabled() -> Self {
        Self { pixel_size: 0.0 }
    }

    pub fn light(pixel_size: f64) -> Self {
        Self { pixel_size }
    }

    fn should_apply(self) -> bool {
        self.pixel_size.is_finite() && (7.0..=32.0).contains(&self.pixel_size)
    }
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
        Self::stroke_internal(
            buf,
            path,
            ctm,
            viewport,
            color,
            stroke_width,
            dash,
            LineCap::Butt,
            LineJoin::Miter,
            10.0,
        );
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
            cap.clone(),
            LineJoin::Miter,
            10.0,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_with_style(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        stroke_width: f64,
        dash: &DashState,
        cap: &LineCap,
        join: &LineJoin,
        miter_limit: f64,
    ) {
        Self::stroke_internal(
            buf,
            path,
            ctm,
            viewport,
            color,
            stroke_width,
            dash,
            cap.clone(),
            join.clone(),
            miter_limit,
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
        cap: LineCap,
        join: LineJoin,
        miter_limit: f64,
    ) {
        if path.is_empty() || buf.width == 0 || buf.height == 0 {
            return;
        }

        let flat = flatten_path(path, ctm, viewport, 0.2);
        let width_px = (stroke_width * ctm.scale_factor() * viewport.scale).max(1.0);
        let outline = stroke_flat_path(&flat, width_px, dash, cap, join, miter_limit);
        if !outline.subpaths.is_empty() {
            fill_flat_aa(buf, &outline, color, FillRule::NonZero);
        }
    }

    /// Fill a path with **analytic, coverage-based antialiasing**.
    ///
    /// Each edge contributes exact signed area+cover to a pixel-local
    /// accumulation buffer (the technique used by FreeType's smooth rasteriser
    /// and `font-rs`), giving true sub-pixel coverage in BOTH axes rather than
    /// the previous hard, integer-snapped scanline spans. Glyphs route through
    /// this path too (see [`crate::render::font_rasterizer`]), so text gains the
    /// same crisp AA. Coverage is composited via [`PixelBuffer::blend_pixel`],
    /// which uses the buffer's render mode: sRGB-space for Compat, linear light
    /// for opt-in HighQuality.
    pub fn fill(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        rule: FillRule,
    ) {
        if path.is_empty() || buf.width == 0 || buf.height == 0 {
            return;
        }
        let flat = flatten_path(path, ctm, viewport, 0.3);
        fill_flat_aa(buf, &flat, color, rule);
    }

    /// Fill a glyph outline using the shared analytic coverage rasterizer.
    ///
    /// Glyph curves use a tighter 0.2px flattening tolerance than general PDF
    /// paths. The default text path is neutral grayscale coverage composited by
    /// [`PixelBuffer::blend_pixel`] in Compat mode's sRGB byte space, matching
    /// Poppler/Splash's proof-rendering convention. Optional light grid-fitting
    /// is only applied when the caller explicitly supplies an enabled
    /// [`GlyphHinting`] value.
    pub fn fill_glyph(
        buf: &mut PixelBuffer,
        path: &Path,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        rule: FillRule,
        hinting: GlyphHinting,
    ) {
        if path.is_empty() || buf.width == 0 || buf.height == 0 {
            return;
        }
        let mut flat = flatten_path(path, ctm, viewport, 0.2);
        if hinting.should_apply() {
            let device_t = ctm.concat(&viewport.to_transform());
            light_grid_fit_flat_glyph(&mut flat, &device_t);
        }
        fill_flat_aa(buf, &flat, color, rule);
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
}

// ---------------------------------------------------------------------------
// Analytic coverage-based antialiased fill
// ---------------------------------------------------------------------------

/// Fill a flattened path into `buf` with analytic antialiasing.
///
/// This is a signed-area **accumulation-cell** rasteriser. For every edge it
/// walks the pixel rows the edge spans and deposits, per touched cell, two
/// quantities into a bounding-box-local scratch buffer — `area` (the exact
/// signed area the edge cuts out of that cell) and `cover` (the signed fraction
/// of the cell's height the edge crosses, which propagates to every cell to its
/// right on the same row).
///
/// A left-to-right prefix sum of `cover` plus the local `area` then gives, for
/// each cell, the exact signed coverage of the path — accurate to sub-pixel in
/// both axes. The accumulated winding number is mapped to an opacity in [0,1]
/// per the fill rule (nonzero or even-odd) and composited with `blend_pixel`.
fn fill_flat_aa(buf: &mut PixelBuffer, flat: &FlatPath, color: PixelColor, rule: FillRule) {
    let bw = buf.width as i32;
    let bh = buf.height as i32;

    // Device-space bounding box of all subpaths, clamped to the buffer.
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for sp in &flat.subpaths {
        for &(x, y) in sp {
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if !min_x.is_finite() || !max_x.is_finite() {
        return;
    }

    let x0 = safe_floor_i32(min_x).max(0);
    let y0 = safe_floor_i32(min_y).max(0);
    let x1 = (safe_ceil_i32(max_x) + 1).min(bw);
    let y1 = (safe_ceil_i32(max_y) + 1).min(bh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let w = (x1 - x0) as usize;
    let h = (y1 - y0) as usize;
    // Guard against pathological allocation on degenerate huge geometry.
    if w == 0 || h == 0 || w.saturating_mul(h) > 64 * 1024 * 1024 {
        return;
    }

    let mut acc = Accumulator::new(w, h, x0, y0);
    for (sp, &closed) in flat.subpaths.iter().zip(flat.closed.iter()) {
        if sp.len() < 2 {
            continue;
        }
        for win in sp.windows(2) {
            acc.add_edge(win[0], win[1]);
        }
        // Implicitly close every subpath for filling (PDF fills as if closed).
        if !closed {
            if let (Some(&first), Some(&last)) = (sp.first(), sp.last()) {
                if first != last {
                    acc.add_edge(last, first);
                }
            }
        }
    }

    acc.composite(buf, color, rule);
}

fn light_grid_fit_flat_glyph(flat: &mut FlatPath, device_t: &Transform2D) {
    if !device_t.is_axis_aligned() {
        return;
    }

    const MAX_BASELINE_SHIFT: f64 = 0.35;

    let (_, baseline_y) = device_t.transform_point(0.0, 0.0);
    let baseline_shift = baseline_y.round() - baseline_y;
    if baseline_shift.abs() <= MAX_BASELINE_SHIFT {
        translate_flat(flat, 0.0, baseline_shift);
    }
}

fn translate_flat(flat: &mut FlatPath, dx: f64, dy: f64) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    for subpath in &mut flat.subpaths {
        for point in subpath {
            point.0 += dx;
            point.1 += dy;
        }
    }
}

#[derive(Debug, Clone)]
struct StrokeStyle {
    half_width: f64,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f64,
}

#[derive(Debug, Clone, Copy)]
struct StrokeSegment {
    dir: (f64, f64),
    normal: (f64, f64),
}

fn stroke_flat_path(
    flat: &FlatPath,
    width_px: f64,
    dash: &DashState,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f64,
) -> FlatPath {
    if width_px <= 0.0 || !width_px.is_finite() {
        return FlatPath::default();
    }

    let style = StrokeStyle {
        half_width: width_px / 2.0,
        cap,
        join,
        miter_limit: miter_limit.max(1.0),
    };
    let mut outline = FlatPath::default();

    for (idx, subpath) in flat.subpaths.iter().enumerate() {
        let closed = flat.closed.get(idx).copied().unwrap_or(false);
        let Some(points) = normalize_stroke_points(subpath, closed) else {
            continue;
        };

        if closed && dash.is_solid() {
            if let Some(poly) = stroked_polyline_outline(&points, true, &style) {
                push_outline_subpath(&mut outline, poly);
            }
            continue;
        }

        let mut dashed_points = points.clone();
        if closed {
            dashed_points.push(points[0]);
        }
        for dash_piece in dash_polyline(&dashed_points, dash) {
            if let Some(poly) = stroked_polyline_outline(&dash_piece, false, &style) {
                push_outline_subpath(&mut outline, poly);
            }
        }
    }

    outline
}

fn push_outline_subpath(outline: &mut FlatPath, mut poly: Vec<(f64, f64)>) {
    if poly.len() < 3 {
        return;
    }
    if let (Some(first), Some(last)) = (poly.first().copied(), poly.last().copied()) {
        if distance(first, last) < 1e-8 {
            poly.pop();
        }
    }
    outline.subpaths.push(poly);
    // `fill_flat_aa` implicitly closes subpaths whose closed flag is false.
    outline.closed.push(false);
}

fn normalize_stroke_points(points: &[(f64, f64)], closed: bool) -> Option<Vec<(f64, f64)>> {
    let mut cleaned = Vec::with_capacity(points.len());
    for &p in points {
        if !p.0.is_finite() || !p.1.is_finite() {
            continue;
        }
        if cleaned.last().is_none_or(|&last| distance(last, p) > 1e-8) {
            cleaned.push(p);
        }
    }
    if closed && cleaned.len() >= 2 && distance(cleaned[0], *cleaned.last()?) < 1e-8 {
        cleaned.pop();
    }
    if cleaned.len() < 2 || (closed && cleaned.len() < 3) {
        None
    } else {
        Some(cleaned)
    }
}

fn dash_polyline(points: &[(f64, f64)], dash: &DashState) -> Vec<Vec<(f64, f64)>> {
    let mut pieces = Vec::new();
    let mut current = Vec::new();
    let mut dash_state = dash.clone();

    for window in points.windows(2) {
        let p0 = window[0];
        let p1 = window[1];
        let dx = p1.0 - p0.0;
        let dy = p1.1 - p0.1;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1e-10 || !seg_len.is_finite() {
            continue;
        }
        let ux = dx / seg_len;
        let uy = dy / seg_len;

        for (t0, t1, drawing) in dash_state.advance(seg_len) {
            if !drawing {
                finish_dash_piece(&mut pieces, &mut current);
                continue;
            }

            let q0 = (p0.0 + ux * t0, p0.1 + uy * t0);
            let q1 = (p0.0 + ux * t1, p0.1 + uy * t1);
            if current
                .last()
                .is_some_and(|&last| distance(last, q0) > 1e-7)
            {
                finish_dash_piece(&mut pieces, &mut current);
            }
            if current.is_empty() {
                current.push(q0);
            }
            if current.last().is_none_or(|&last| distance(last, q1) > 1e-8) {
                current.push(q1);
            }
        }
    }

    finish_dash_piece(&mut pieces, &mut current);
    pieces
}

fn finish_dash_piece(pieces: &mut Vec<Vec<(f64, f64)>>, current: &mut Vec<(f64, f64)>) {
    if current.len() >= 2 {
        pieces.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn stroked_polyline_outline(
    points: &[(f64, f64)],
    closed: bool,
    style: &StrokeStyle,
) -> Option<Vec<(f64, f64)>> {
    let segments = build_stroke_segments(points, closed)?;
    if closed {
        stroked_closed_outline(points, &segments, style)
    } else {
        stroked_open_outline(points, &segments, style)
    }
}

fn build_stroke_segments(points: &[(f64, f64)], closed: bool) -> Option<Vec<StrokeSegment>> {
    let count = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    let mut segments = Vec::with_capacity(count);
    for i in 0..count {
        let start = points[i];
        let end = points[(i + 1) % points.len()];
        let dx = end.0 - start.0;
        let dy = end.1 - start.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-10 || !len.is_finite() {
            continue;
        }
        let dir = (dx / len, dy / len);
        let normal = (-dir.1, dir.0);
        segments.push(StrokeSegment { dir, normal });
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

fn stroked_open_outline(
    points: &[(f64, f64)],
    segments: &[StrokeSegment],
    style: &StrokeStyle,
) -> Option<Vec<(f64, f64)>> {
    let first = *segments.first()?;
    let last = *segments.last()?;
    let start_center = match style.cap {
        LineCap::ProjectingSquare => sub(points[0], scale(first.dir, style.half_width)),
        _ => points[0],
    };
    let end_center = match style.cap {
        LineCap::ProjectingSquare => add(*points.last()?, scale(last.dir, style.half_width)),
        _ => *points.last()?,
    };

    let mut left = Vec::with_capacity(points.len() + 8);
    let mut right = Vec::with_capacity(points.len() + 8);
    let start_left = offset_point(start_center, first.normal, 1.0, style.half_width);
    let start_right = offset_point(start_center, first.normal, -1.0, style.half_width);
    let end_left = offset_point(end_center, last.normal, 1.0, style.half_width);
    let end_right = offset_point(end_center, last.normal, -1.0, style.half_width);

    left.push(start_left);
    right.push(start_right);
    for i in 1..(points.len() - 1) {
        left.extend(join_points(
            points[i],
            segments[i - 1],
            segments[i],
            1.0,
            style,
        ));
        right.extend(join_points(
            points[i],
            segments[i - 1],
            segments[i],
            -1.0,
            style,
        ));
    }
    left.push(end_left);
    right.push(end_right);

    let mut poly = left;
    if matches!(style.cap, LineCap::Round) {
        poly.extend(arc_points_towards(
            end_center,
            end_left,
            end_right,
            last.dir,
            style.half_width,
        ));
    }
    poly.extend(right.into_iter().rev());
    if matches!(style.cap, LineCap::Round) {
        poly.extend(arc_points_towards(
            start_center,
            start_right,
            start_left,
            scale(first.dir, -1.0),
            style.half_width,
        ));
    }

    Some(poly)
}

fn stroked_closed_outline(
    points: &[(f64, f64)],
    segments: &[StrokeSegment],
    style: &StrokeStyle,
) -> Option<Vec<(f64, f64)>> {
    if points.len() < 3 || segments.len() < 3 {
        return None;
    }

    let mut left = Vec::with_capacity(points.len() + 8);
    let mut right = Vec::with_capacity(points.len() + 8);
    for i in 0..points.len() {
        let prev = segments[(i + segments.len() - 1) % segments.len()];
        let next = segments[i % segments.len()];
        left.extend(join_points(points[i], prev, next, 1.0, style));
        right.extend(join_points(points[i], prev, next, -1.0, style));
    }

    let mut poly = left;
    poly.extend(right.into_iter().rev());
    Some(poly)
}

fn join_points(
    vertex: (f64, f64),
    prev: StrokeSegment,
    next: StrokeSegment,
    side: f64,
    style: &StrokeStyle,
) -> Vec<(f64, f64)> {
    let prev_offset = offset_point(vertex, prev.normal, side, style.half_width);
    let next_offset = offset_point(vertex, next.normal, side, style.half_width);
    let intersection = line_intersection(prev_offset, prev.dir, next_offset, next.dir);

    match style.join {
        LineJoin::Miter => {
            if let Some(p) = intersection {
                if distance(vertex, p) <= style.half_width * style.miter_limit + 1e-8 {
                    return vec![p];
                }
            }
            vec![prev_offset, next_offset]
        }
        LineJoin::Bevel => vec![prev_offset, next_offset],
        LineJoin::Round => {
            let mut arc = arc_points_shortest(vertex, prev_offset, next_offset, style.half_width);
            if arc.len() < 2 {
                if let Some(p) = intersection {
                    arc.push(p);
                }
            }
            arc
        }
    }
}

fn offset_point(p: (f64, f64), normal: (f64, f64), side: f64, half_width: f64) -> (f64, f64) {
    add(p, scale(normal, side * half_width))
}

fn line_intersection(
    p: (f64, f64),
    r: (f64, f64),
    q: (f64, f64),
    s: (f64, f64),
) -> Option<(f64, f64)> {
    let denom = cross(r, s);
    if denom.abs() < 1e-10 {
        return None;
    }
    let t = cross(sub(q, p), s) / denom;
    Some(add(p, scale(r, t)))
}

fn arc_points_shortest(
    center: (f64, f64),
    from: (f64, f64),
    to: (f64, f64),
    radius: f64,
) -> Vec<(f64, f64)> {
    let a0 = angle(center, from);
    let a1 = angle(center, to);
    let mut delta = a1 - a0;
    while delta <= -std::f64::consts::PI {
        delta += std::f64::consts::TAU;
    }
    while delta > std::f64::consts::PI {
        delta -= std::f64::consts::TAU;
    }
    arc_points_with_delta(center, a0, delta, radius)
}

fn arc_points_towards(
    center: (f64, f64),
    from: (f64, f64),
    to: (f64, f64),
    desired: (f64, f64),
    radius: f64,
) -> Vec<(f64, f64)> {
    let a0 = angle(center, from);
    let a1 = angle(center, to);
    let mut ccw = a1 - a0;
    while ccw < 0.0 {
        ccw += std::f64::consts::TAU;
    }
    while ccw >= std::f64::consts::TAU {
        ccw -= std::f64::consts::TAU;
    }
    let cw = ccw - std::f64::consts::TAU;
    let ccw_mid = (a0 + ccw / 2.0).cos() * desired.0 + (a0 + ccw / 2.0).sin() * desired.1;
    let cw_mid = (a0 + cw / 2.0).cos() * desired.0 + (a0 + cw / 2.0).sin() * desired.1;
    let delta = if ccw_mid >= cw_mid { ccw } else { cw };
    arc_points_with_delta(center, a0, delta, radius)
}

fn arc_points_with_delta(
    center: (f64, f64),
    start_angle: f64,
    delta: f64,
    radius: f64,
) -> Vec<(f64, f64)> {
    if radius <= 0.0 || !radius.is_finite() || !delta.is_finite() {
        return Vec::new();
    }
    let steps = ((delta.abs() / (std::f64::consts::PI / 12.0)).ceil() as usize).max(1);
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let a = start_angle + delta * t;
            (center.0 + radius * a.cos(), center.1 + radius * a.sin())
        })
        .collect()
}

fn angle(center: (f64, f64), p: (f64, f64)) -> f64 {
    (p.1 - center.1).atan2(p.0 - center.0)
}

fn add(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 + b.0, a.1 + b.1)
}

fn sub(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 - b.0, a.1 - b.1)
}

fn scale(a: (f64, f64), s: f64) -> (f64, f64) {
    (a.0 * s, a.1 * s)
}

fn cross(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    (dx * dx + dy * dy).sqrt()
}

/// Per-pixel signed-area accumulation buffer for one fill.
struct Accumulator {
    w: usize,
    h: usize,
    origin_x: i32,
    origin_y: i32,
    /// `area[y*w + x]` — exact area contribution local to cell (x, y).
    area: Vec<f32>,
    /// `cover[y*w + x]` — signed height crossed at cell (x, y); prefix-summed
    /// left-to-right at composite time.
    cover: Vec<f32>,
}

impl Accumulator {
    fn new(w: usize, h: usize, origin_x: i32, origin_y: i32) -> Self {
        Self {
            w,
            h,
            origin_x,
            origin_y,
            area: vec![0.0; w * h],
            cover: vec![0.0; w * h],
        }
    }

    /// Deposit one edge from p0 to p1 (device-space, buffer-global coords).
    fn add_edge(&mut self, p0: (f64, f64), p1: (f64, f64)) {
        let (mut x0, mut y0) = (p0.0 - self.origin_x as f64, p0.1 - self.origin_y as f64);
        let (mut x1, mut y1) = (p1.0 - self.origin_x as f64, p1.1 - self.origin_y as f64);
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return;
        }
        if (y0 - y1).abs() < 1e-12 {
            return; // Horizontal edges contribute no coverage.
        }

        // Winding sign: +1 for downward edges, -1 for upward; normalise so we
        // always walk increasing y and flip the sign accordingly.
        let dir = if y0 < y1 { 1.0 } else { -1.0 };
        if y0 > y1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
        }

        // Clip to the vertical extent of the accumulation buffer.
        let h = self.h as f64;
        let dxdy = (x1 - x0) / (y1 - y0);
        if y0 < 0.0 {
            x0 += dxdy * (0.0 - y0);
            y0 = 0.0;
        }
        if y1 > h {
            // `y1` is the clipped row extent; `x1` is not read again (per-row x
            // is recomputed from `x0`/`dxdy`), so only the y bound is clamped.
            y1 = h;
        }
        if y1 <= y0 {
            return;
        }

        // Walk each pixel row the edge crosses.
        let mut y = y0;
        while y < y1 {
            let row = y.floor();
            let row_idx = row as i32;
            let row_top = row;
            let row_bot = (row + 1.0).min(y1);
            let seg_y0 = y.max(row_top);
            let seg_y1 = row_bot;
            if seg_y1 <= seg_y0 {
                y = row_bot;
                continue;
            }
            if row_idx < 0 || row_idx as usize >= self.h {
                y = row_bot;
                continue;
            }

            let xa = x0 + dxdy * (seg_y0 - y0);
            let xb = x0 + dxdy * (seg_y1 - y0);
            let dy = (seg_y1 - seg_y0) as f32 * dir as f32;
            self.add_span(row_idx as usize, xa, xb, dy);

            y = row_bot;
        }
    }

    /// Add a single edge segment confined to one pixel row. `xa`/`xb` are the
    /// x-coordinates where the edge enters/leaves this row; `dy` is the signed
    /// covered height (already sign-adjusted for winding direction).
    fn add_span(&mut self, row: usize, xa: f64, xb: f64, dy: f32) {
        // Order so xl <= xr.
        let (xl, xr) = if xa <= xb { (xa, xb) } else { (xb, xa) };
        let base = row * self.w;
        let wf = self.w as f64;

        // Everything left of the buffer fully contributes `cover` to column 0.
        let xl = xl.clamp(0.0, wf);
        let xr = xr.clamp(0.0, wf);

        let cl = xl.floor() as usize;
        let cr = xr.floor() as usize;
        let cl = cl.min(self.w - 1);
        let cr = cr.min(self.w - 1);

        if cl == cr {
            // Edge stays within one cell: area is the rectangle to the right of
            // the edge's mean x within the cell.
            let mid = (xl + xr) * 0.5 - cl as f64;
            let cell_cover = dy;
            // `area` = portion of the cell NOT to the left of the edge, scaled
            // by the covered height.
            self.area[base + cl] += (1.0 - mid as f32) * cell_cover;
            self.cover[base + cl] += cell_cover;
            return;
        }

        // Edge crosses multiple cells in x. Distribute coverage proportionally
        // to the horizontal extent within each crossed cell.
        let inv_dx = 1.0 / (xr - xl);
        // First (leftmost) partial cell.
        let first_right = (cl + 1) as f64;
        let frac_first = ((first_right - xl) * inv_dx) as f32; // fraction of dy in this cell
        let mid_first = (xl + first_right) * 0.5 - cl as f64;
        self.area[base + cl] += (1.0 - mid_first as f32) * (dy * frac_first);
        self.cover[base + cl] += dy * frac_first;

        // Middle full cells.
        for cell in (cl + 1)..cr {
            let cell_left = cell as f64;
            let cell_right = (cell + 1) as f64;
            let frac = ((cell_right - cell_left) * inv_dx) as f32;
            let mid = 0.5; // edge crosses the whole cell width
            self.area[base + cell] += (1.0 - mid) * (dy * frac);
            self.cover[base + cell] += dy * frac;
        }

        // Last (rightmost) partial cell.
        let last_left = cr as f64;
        let frac_last = ((xr - last_left) * inv_dx) as f32;
        let mid_last = (last_left + xr) * 0.5 - cr as f64;
        self.area[base + cr] += (1.0 - mid_last as f32) * (dy * frac_last);
        self.cover[base + cr] += dy * frac_last;
    }

    /// Resolve accumulated winding into coverage and composite onto `buf`.
    fn composite(&self, buf: &mut PixelBuffer, color: PixelColor, rule: FillRule) {
        for ry in 0..self.h {
            let base = ry * self.w;
            let mut running = 0.0f32; // prefix sum of cover (winding to the left)
            for rx in 0..self.w {
                let cell_winding = running + self.area[base + rx];
                running += self.cover[base + rx];
                let coverage = coverage_from_winding(cell_winding, rule);
                if coverage <= 0.001 {
                    continue;
                }
                let px = self.origin_x + rx as i32;
                let py = self.origin_y + ry as i32;
                buf.blend_pixel(px, py, color, coverage);
            }
        }
    }
}

/// Map a signed winding/coverage accumulator value to an opacity in [0, 1]
/// under the given fill rule.
fn coverage_from_winding(winding: f32, rule: FillRule) -> f32 {
    match rule {
        FillRule::NonZero => winding.abs().min(1.0),
        FillRule::EvenOdd => {
            // Fold into [0, 2) then mirror around 1 -> triangular profile, which
            // gives correct even-odd coverage including antialiased edges.
            let mut v = winding.abs() % 2.0;
            if v > 1.0 {
                v = 2.0 - v;
            }
            v.clamp(0.0, 1.0)
        }
    }
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
    use crate::render::buffer::{RenderMode, BLACK, BLUE, GREEN, RED, TRANSPARENT, WHITE};

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
    fn stroke_outline_keeps_dash_state_across_segments() {
        let points = vec![(0.0, 0.0), (10.0, 0.0), (20.0, 0.0)];
        let pieces = dash_polyline(&points, &DashState::new(vec![12.0, 100.0], 0.0));
        assert_eq!(pieces.len(), 1);
        let last = pieces[0].last().copied().unwrap();
        assert!(
            (last.0 - 12.0).abs() < 1e-8 && last.1.abs() < 1e-8,
            "dash should continue across segment boundary, got {last:?}"
        );
    }

    #[test]
    fn stroke_outline_miter_and_bevel_join_geometry_differs() {
        let flat = FlatPath {
            subpaths: vec![vec![(10.0, 50.0), (50.0, 50.0), (50.0, 10.0)]],
            closed: vec![false],
        };
        let miter = stroke_flat_path(
            &flat,
            10.0,
            &DashState::solid(),
            LineCap::Butt,
            LineJoin::Miter,
            10.0,
        );
        let bevel = stroke_flat_path(
            &flat,
            10.0,
            &DashState::solid(),
            LineCap::Butt,
            LineJoin::Bevel,
            10.0,
        );

        let miter_points = &miter.subpaths[0];
        let bevel_points = &bevel.subpaths[0];
        assert!(
            miter_points
                .iter()
                .any(|&p| distance(p, (55.0, 55.0)) < 1e-8),
            "miter join should include the offset-line intersection"
        );
        assert!(
            !bevel_points
                .iter()
                .any(|&p| distance(p, (55.0, 55.0)) < 1e-8),
            "bevel join should use edge endpoints instead of the miter point"
        );
    }

    #[test]
    fn analytic_stroke_edge_has_fractional_coverage() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut p = Path::new();
        p.move_to(10.0, 49.75);
        p.line_to(90.0, 49.75);

        PathPainter::stroke(&mut buf, &p, &ctm, &vp, BLACK, 2.0, &DashState::solid());

        let center = buf.get_pixel(50, 50);
        let edge = buf.get_pixel(50, 49);
        assert!(center[0] < 20, "stroke center should be solid: {center:?}");
        assert!(
            edge[0] > 20 && edge[0] < 230,
            "fractional stroke edge should be antialiased gray: {edge:?}"
        );
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
    fn aa_fill_interior_is_fully_covered() {
        // A rect aligned to integer pixel boundaries: interior cells get 100%
        // coverage (exact colour), proving AA does not erode solid fills.
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.rect(20.0, 20.0, 40.0, 40.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);
        // Center is solid red.
        assert_eq!(buf.get_pixel(40, 50), RED);
    }

    #[test]
    fn aa_fill_partial_edge_produces_intermediate_coverage() {
        // A rectangle whose right edge falls on a half-pixel (x = 30.5) must
        // produce a partially-covered column (not a hard on/off jump). With the
        // old non-AA fill that boundary column was either full or empty.
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        // y in PDF space flips; use a tall rect with a fractional right edge.
        path.move_to(10.0, 10.0);
        path.line_to(30.5, 10.0);
        path.line_to(30.5, 90.0);
        path.line_to(10.0, 90.0);
        path.close();
        PathPainter::fill(&mut buf, &path, &ctm, &vp, BLACK, FillRule::NonZero);
        // Column 30 straddles the x=30.5 edge -> ~50% black over white -> gray.
        let edge = buf.get_pixel(30, 50);
        assert!(
            edge[0] > 60 && edge[0] < 210,
            "fractional edge column should be antialiased gray, got {edge:?}"
        );
        // Well inside is solid black; well outside is white.
        assert!(
            buf.get_pixel(20, 50)[0] < 30,
            "interior should be near-black"
        );
        assert_eq!(buf.get_pixel(40, 50), WHITE, "outside the rect stays white");
    }

    #[test]
    fn aa_fill_triangle_has_smooth_diagonal_edge() {
        // A diagonal edge should show a gradient of partial coverage along it,
        // not a staircase of full/empty pixels.
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.move_to(10.0, 10.0);
        path.line_to(90.0, 10.0);
        path.line_to(10.0, 90.0);
        path.close();
        PathPainter::fill(&mut buf, &path, &ctm, &vp, BLACK, FillRule::NonZero);
        // Sample the diagonal region: count pixels with intermediate (AA) values.
        let mut partial = 0;
        for y in 0..100i32 {
            for x in 0..100i32 {
                let v = buf.get_pixel(x, y)[0];
                if v > 20 && v < 235 {
                    partial += 1;
                }
            }
        }
        assert!(
            partial > 30,
            "diagonal edge should yield many antialiased pixels, got {partial}"
        );
    }

    #[test]
    fn glyph_fill_partial_edge_produces_intermediate_coverage() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut path = Path::new();
        path.move_to(10.0, 10.0);
        path.line_to(30.5, 10.0);
        path.line_to(30.5, 90.0);
        path.line_to(10.0, 90.0);
        path.close();

        PathPainter::fill_glyph(
            &mut buf,
            &path,
            &ctm,
            &vp,
            BLACK,
            FillRule::NonZero,
            GlyphHinting::disabled(),
        );

        let edge = buf.get_pixel(30, 50);
        assert!(
            edge[0] > 60 && edge[0] < 210,
            "glyph fractional edge column should be gray, got {edge:?}"
        );
        assert!(buf.get_pixel(20, 50)[0] < 30);
        assert_eq!(buf.get_pixel(40, 50), WHITE);
    }

    #[test]
    fn glyph_grayscale_default_uses_compat_srgb_coverage_weight() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut compat = PixelBuffer::new_filled(100, 100, WHITE);
        let mut high = PixelBuffer::new_filled_with_mode(100, 100, WHITE, RenderMode::HighQuality);
        let mut path = Path::new();
        path.move_to(10.5, 10.0);
        path.line_to(20.5, 10.0);
        path.line_to(20.5, 90.0);
        path.line_to(10.5, 90.0);
        path.close();

        PathPainter::fill_glyph(
            &mut compat,
            &path,
            &ctm,
            &vp,
            BLACK,
            FillRule::NonZero,
            GlyphHinting::disabled(),
        );
        PathPainter::fill_glyph(
            &mut high,
            &path,
            &ctm,
            &vp,
            BLACK,
            FillRule::NonZero,
            GlyphHinting::disabled(),
        );

        let compat_edge = compat.get_pixel(10, 50)[0];
        let high_edge = high.get_pixel(10, 50)[0];
        assert!(
            (110..=145).contains(&compat_edge),
            "Compat grayscale text uses Poppler-style sRGB coverage, got {compat_edge}"
        );
        assert!(
            high_edge > compat_edge + 30,
            "HighQuality remains an opt-in linear-light path, compat={compat_edge}, high={high_edge}"
        );
        assert!(
            compat.get_pixel(15, 50)[0] < 20,
            "stem interior should remain solid"
        );
    }

    #[test]
    fn glyph_curve_flattening_uses_tighter_tolerance() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut path = Path::new();
        path.move_to(10.0, 10.0);
        path.curve_to(10.0, 90.0, 90.0, 90.0, 90.0, 10.0);

        let loose = flatten_path(&path, &ctm, &vp, 0.5);
        let tight = flatten_path(&path, &ctm, &vp, 0.2);
        assert!(
            tight.subpaths[0].len() > loose.subpaths[0].len(),
            "0.2px glyph tolerance should keep more curve samples than 0.5px"
        );
    }

    #[test]
    fn light_grid_fit_snaps_small_axis_aligned_baseline() {
        let mut flat = FlatPath {
            subpaths: vec![vec![
                (10.28, 5.12),
                (12.74, 5.12),
                (12.74, 20.12),
                (10.28, 20.12),
                (10.28, 5.12),
            ]],
            closed: vec![true],
        };

        light_grid_fit_flat_glyph(&mut flat, &Transform2D::translation(0.0, 0.28));

        let sp = &flat.subpaths[0];
        assert!((sp[0].0 - 10.28).abs() < 1e-10);
        assert!((sp[0].1 - 4.84).abs() < 1e-10);
        assert!((sp[2].0 - 12.74).abs() < 1e-10);
        assert!((sp[2].1 - 19.84).abs() < 1e-10);
    }

    #[test]
    fn glyph_hinting_large_display_text_is_disabled() {
        assert!(!GlyphHinting::light(48.0).should_apply());
        assert!(!GlyphHinting::light(6.5).should_apply());
        assert!(GlyphHinting::light(18.0).should_apply());
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
