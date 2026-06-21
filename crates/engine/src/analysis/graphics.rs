//! Lightweight drawn-graphics collector — extracts the axis-aligned line
//! segments and rectangles drawn by a page's content stream, in **PDF user
//! space**, for ruled-table detection ([`crate::analysis::tables`]).
//!
//! Like the SVG/PostScript sinks, this reuses the content interpreter's
//! [`GraphicsState`] for the CTM and `q`/`Q` stack, but instead of painting
//! pixels it records the geometry of stroked/filled paths. It only keeps
//! HORIZONTAL and VERTICAL segments (table rules are axis-aligned); skewed and
//! curved geometry is ignored. Thin filled rectangles (a common way to draw
//! rules without `stroke`) are also decomposed into their edges.

use std::collections::BTreeSet;

use crate::content::operation::ContentOperation;
use crate::content::state::GraphicsState;
use crate::render::transform::Transform2D;

/// A horizontal or vertical line segment in user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Segment {
    pub fn is_horizontal(&self) -> bool {
        (self.y0 - self.y1).abs() < AXIS_EPS && (self.x0 - self.x1).abs() >= MIN_LEN
    }
    pub fn is_vertical(&self) -> bool {
        (self.x0 - self.x1).abs() < AXIS_EPS && (self.y0 - self.y1).abs() >= MIN_LEN
    }
    pub fn length(&self) -> f64 {
        ((self.x1 - self.x0).powi(2) + (self.y1 - self.y0).powi(2)).sqrt()
    }
}

/// A rectangle in user space (from `re` or an axis-aligned closed quad).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn width(&self) -> f64 {
        (self.x1 - self.x0).abs()
    }
    pub fn height(&self) -> f64 {
        (self.y1 - self.y0).abs()
    }
}

/// The user-space placement of an image XObject drawn by a `Do` operator.
///
/// PDF paints an image into the unit square `[0,1]×[0,1]` transformed by the
/// current CTM; `bbox` is the axis-aligned bounding rectangle of that
/// transformed unit square, in user space (the same space as [`Segment`] and
/// [`Rect`]). Used by the document-model layer to place figures.
#[derive(Debug, Clone, PartialEq)]
pub struct ImagePlacement {
    pub bbox: Rect,
    /// The XObject resource name (the operand of `Do`).
    pub name: String,
}

/// All drawn graphics relevant to table/figure detection, in user space.
#[derive(Debug, Clone, Default)]
pub struct DrawnGraphics {
    /// Axis-aligned horizontal/vertical line segments (stroked or thin-fill).
    pub segments: Vec<Segment>,
    /// Rectangles (stroked or filled), e.g. table/cell borders.
    pub rects: Vec<Rect>,
    /// Image XObject placements (filled only by [`collect_graphics_with_images`];
    /// empty for the table-detection entry point [`collect_graphics`], which
    /// keeps its original signature for back-compatibility).
    pub images: Vec<ImagePlacement>,
}

/// Tolerance for treating a segment as axis-aligned (user-space units).
const AXIS_EPS: f64 = 0.6;
/// Minimum length (user-space units) for a segment to be kept.
const MIN_LEN: f64 = 2.0;
/// A filled rectangle thinner than this (in either dimension) is treated as a
/// drawn rule and decomposed into a single segment along its long axis.
const RULE_THICKNESS: f64 = 3.0;

/// Collect the drawn line segments and rectangles from a page's operations.
/// Image placements are NOT recorded (use [`collect_graphics_with_images`] for
/// those); the table detector only needs lines/rects.
pub fn collect_graphics(operations: &[ContentOperation]) -> DrawnGraphics {
    collect_graphics_with_images(operations, &BTreeSet::new())
}

/// Like [`collect_graphics`] but also records the user-space placement of every
/// `Do` whose operand names an image XObject in `image_names`. Form XObjects
/// (and any other name) are ignored, so the caller passes only the page's
/// top-level *image* XObject names. The line/rect output is identical to
/// [`collect_graphics`].
pub fn collect_graphics_with_images(
    operations: &[ContentOperation],
    image_names: &BTreeSet<String>,
) -> DrawnGraphics {
    let mut collector = GraphicsCollector::new(image_names);
    collector.run(operations);
    collector.out
}

struct GraphicsCollector<'a> {
    gs: GraphicsState,
    /// Current path, as a list of subpaths of user-space points, plus whether a
    /// subpath came from `re` (so we can recover rectangles exactly).
    subpaths: Vec<Subpath>,
    out: DrawnGraphics,
    /// Names of image XObjects whose `Do` placements should be recorded.
    image_names: &'a BTreeSet<String>,
}

#[derive(Default)]
struct Subpath {
    points: Vec<(f64, f64)>,
    /// True when this subpath was created by the `re` operator.
    from_rect: bool,
}

impl<'a> GraphicsCollector<'a> {
    fn new(image_names: &'a BTreeSet<String>) -> Self {
        Self {
            gs: GraphicsState::new(),
            subpaths: Vec::new(),
            out: DrawnGraphics::default(),
            image_names,
        }
    }

    fn ctm(&self) -> Transform2D {
        Transform2D::from(self.gs.ctm)
    }

    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        self.ctm().transform_point(x, y)
    }

    fn run(&mut self, operations: &[ContentOperation]) {
        for op in operations {
            self.dispatch(op);
        }
    }

    fn dispatch(&mut self, op: &ContentOperation) {
        match op.operator.as_str() {
            "m" => {
                if let (Some(x), Some(y)) = (op.number(0), op.number(1)) {
                    let p = self.map(x, y);
                    self.subpaths.push(Subpath {
                        points: vec![p],
                        from_rect: false,
                    });
                }
            }
            "l" => {
                if let (Some(x), Some(y)) = (op.number(0), op.number(1)) {
                    let p = self.map(x, y);
                    if let Some(sp) = self.subpaths.last_mut() {
                        sp.points.push(p);
                    } else {
                        self.subpaths.push(Subpath {
                            points: vec![p],
                            from_rect: false,
                        });
                    }
                }
            }
            "c" | "v" | "y" => {
                // Curves: keep only the endpoint (table rules are straight; a
                // curve endpoint preserves connectivity without false axes).
                let (ex, ey) = match op.operator.as_str() {
                    "c" | "y" => (op.number(4), op.number(5)),
                    _ => (op.number(2), op.number(3)), // v
                };
                if let (Some(x), Some(y)) = (ex, ey) {
                    let p = self.map(x, y);
                    if let Some(sp) = self.subpaths.last_mut() {
                        sp.points.push(p);
                    }
                }
            }
            "re" => {
                if let (Some(x), Some(y), Some(w), Some(h)) =
                    (op.number(0), op.number(1), op.number(2), op.number(3))
                {
                    let p0 = self.map(x, y);
                    let p1 = self.map(x + w, y);
                    let p2 = self.map(x + w, y + h);
                    let p3 = self.map(x, y + h);
                    self.subpaths.push(Subpath {
                        points: vec![p0, p1, p2, p3, p0],
                        from_rect: true,
                    });
                }
            }
            "h" => {
                if let Some(sp) = self.subpaths.last_mut() {
                    if let Some(&first) = sp.points.first() {
                        sp.points.push(first);
                    }
                }
            }
            // Painting operators flush the current path into the collector.
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                self.flush_path();
            }
            "n" => {
                // No-paint (often after a clip); discard the path.
                self.subpaths.clear();
            }
            "Do" => {
                // An image XObject is painted into the unit square transformed by
                // the current CTM. Record the axis-aligned bbox of that square,
                // but only for names known to be image XObjects (never Form
                // XObjects). The CTM is maintained by the catch-all `process`
                // for `cm`/`q`/`Q`, so it is current here.
                if let Some(name) = op.name(0) {
                    if self.image_names.contains(name) {
                        let p0 = self.map(0.0, 0.0);
                        let p1 = self.map(1.0, 0.0);
                        let p2 = self.map(1.0, 1.0);
                        let p3 = self.map(0.0, 1.0);
                        let xs = [p0.0, p1.0, p2.0, p3.0];
                        let ys = [p0.1, p1.1, p2.1, p3.1];
                        let x0 = xs.iter().cloned().fold(f64::INFINITY, f64::min);
                        let x1 = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                        let y0 = ys.iter().cloned().fold(f64::INFINITY, f64::min);
                        let y1 = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                        let (w, h) = (x1 - x0, y1 - y0);
                        if w.is_finite() && h.is_finite() && w >= MIN_LEN && h >= MIN_LEN {
                            self.out.images.push(ImagePlacement {
                                bbox: Rect { x0, y0, x1, y1 },
                                name: name.to_string(),
                            });
                        }
                    }
                }
            }
            _ => self.gs.process(op),
        }
    }

    /// Convert the current path's subpaths into segments/rects and clear it.
    fn flush_path(&mut self) {
        let subpaths = std::mem::take(&mut self.subpaths);
        for sp in subpaths {
            if sp.from_rect && sp.points.len() == 5 {
                self.emit_rect(&sp.points);
            } else {
                self.emit_polyline(&sp.points);
            }
        }
    }

    fn emit_rect(&mut self, pts: &[(f64, f64)]) {
        let xs = [pts[0].0, pts[1].0, pts[2].0, pts[3].0];
        let ys = [pts[0].1, pts[1].1, pts[2].1, pts[3].1];
        let x0 = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let x1 = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let y0 = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let y1 = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let w = x1 - x0;
        let h = y1 - y0;
        // A thin rectangle is a drawn rule -> emit a single centre-line segment.
        if h <= RULE_THICKNESS && w > h {
            let yc = (y0 + y1) / 2.0;
            self.push_segment((x0, yc), (x1, yc));
            return;
        }
        if w <= RULE_THICKNESS && h > w {
            let xc = (x0 + x1) / 2.0;
            self.push_segment((xc, y0), (xc, y1));
            return;
        }
        // A real rectangle: record it and also its four edges (so cell-boundary
        // detection sees the lines even when only rectangles are drawn).
        self.out.rects.push(Rect { x0, y0, x1, y1 });
        self.push_segment((x0, y0), (x1, y0));
        self.push_segment((x0, y1), (x1, y1));
        self.push_segment((x0, y0), (x0, y1));
        self.push_segment((x1, y0), (x1, y1));
    }

    fn emit_polyline(&mut self, pts: &[(f64, f64)]) {
        for w in pts.windows(2) {
            self.push_segment(w[0], w[1]);
        }
    }

    /// Keep a segment only if it is axis-aligned and long enough.
    fn push_segment(&mut self, a: (f64, f64), b: (f64, f64)) {
        let seg = Segment {
            x0: a.0,
            y0: a.1,
            x1: b.0,
            y1: b.1,
        };
        if seg.is_horizontal() || seg.is_vertical() {
            // Normalise orientation so x0<=x1 / y0<=y1.
            let norm = Segment {
                x0: seg.x0.min(seg.x1),
                y0: seg.y0.min(seg.y1),
                x1: seg.x0.max(seg.x1),
                y1: seg.y0.max(seg.y1),
            };
            self.out.segments.push(norm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentParser;

    fn collect(stream: &[u8]) -> DrawnGraphics {
        let ops = ContentParser::parse(stream).unwrap();
        collect_graphics(&ops)
    }

    #[test]
    fn stroked_horizontal_line_is_collected() {
        let g = collect(b"50 700 m 200 700 l S");
        assert_eq!(g.segments.len(), 1);
        assert!(g.segments[0].is_horizontal());
        assert!((g.segments[0].x0 - 50.0).abs() < 0.1);
        assert!((g.segments[0].x1 - 200.0).abs() < 0.1);
    }

    #[test]
    fn stroked_vertical_line_is_collected() {
        let g = collect(b"100 600 m 100 750 l S");
        assert_eq!(g.segments.len(), 1);
        assert!(g.segments[0].is_vertical());
    }

    #[test]
    fn diagonal_line_is_ignored() {
        let g = collect(b"50 50 m 200 200 l S");
        assert!(g.segments.is_empty(), "diagonal must not be kept");
    }

    #[test]
    fn rectangle_yields_four_edges_and_a_rect() {
        let g = collect(b"50 600 200 100 re S");
        assert_eq!(g.rects.len(), 1);
        // Four edges: 2 horizontal + 2 vertical.
        let h = g.segments.iter().filter(|s| s.is_horizontal()).count();
        let v = g.segments.iter().filter(|s| s.is_vertical()).count();
        assert_eq!(h, 2, "two horizontal edges");
        assert_eq!(v, 2, "two vertical edges");
    }

    #[test]
    fn thin_filled_rect_is_a_rule_segment() {
        // A 1pt-tall filled rectangle is a horizontal rule, not a box.
        let g = collect(b"50 700 150 1 re f");
        assert!(g.rects.is_empty(), "thin rect is a rule, not a box");
        assert_eq!(g.segments.len(), 1);
        assert!(g.segments[0].is_horizontal());
    }

    #[test]
    fn ctm_scale_is_applied_to_coordinates() {
        // cm scales by 2 -> a line from (10,10) to (60,10) maps to (20,20)-(120,20).
        let g = collect(b"2 0 0 2 0 0 cm 10 10 m 60 10 l S");
        assert_eq!(g.segments.len(), 1);
        assert!(
            (g.segments[0].x0 - 20.0).abs() < 0.1,
            "x0={}",
            g.segments[0].x0
        );
        assert!(
            (g.segments[0].x1 - 120.0).abs() < 0.1,
            "x1={}",
            g.segments[0].x1
        );
        assert!((g.segments[0].y0 - 20.0).abs() < 0.1);
    }

    fn collect_with_images(stream: &[u8], names: &[&str]) -> DrawnGraphics {
        let ops = ContentParser::parse(stream).unwrap();
        let set: BTreeSet<String> = names.iter().map(|s| s.to_string()).collect();
        collect_graphics_with_images(&ops, &set)
    }

    #[test]
    fn image_do_records_placement_rect() {
        // `cm` places the unit square as a 200x100 box at (50,600); /Im0 Do then
        // paints an image into it. With Im0 known as an image, its placement is
        // recorded as the user-space bbox [50,600,250,700].
        let g = collect_with_images(b"q 200 0 0 100 50 600 cm /Im0 Do Q", &["Im0"]);
        assert_eq!(g.images.len(), 1, "one image placement expected");
        let r = &g.images[0].bbox;
        assert!((r.x0 - 50.0).abs() < 0.1, "x0={}", r.x0);
        assert!((r.y0 - 600.0).abs() < 0.1, "y0={}", r.y0);
        assert!((r.x1 - 250.0).abs() < 0.1, "x1={}", r.x1);
        assert!((r.y1 - 700.0).abs() < 0.1, "y1={}", r.y1);
        assert_eq!(g.images[0].name, "Im0");
        // y1 > y0 (user space, y-up).
        assert!(r.y1 > r.y0);
    }

    #[test]
    fn form_do_is_ignored_without_image_name() {
        // The same Do, but the name is NOT in the image set (e.g. a Form XObject):
        // no placement is recorded. collect_graphics (empty set) likewise records
        // nothing, preserving back-compat for table detection.
        let g = collect_with_images(b"q 200 0 0 100 50 600 cm /Fm0 Do Q", &["Im0"]);
        assert!(g.images.is_empty(), "form xobject must not be recorded");
        let g2 = collect(b"q 200 0 0 100 50 600 cm /Im0 Do Q");
        assert!(g2.images.is_empty(), "collect_graphics records no images");
    }

    #[test]
    fn q_then_uppercase_q_restores_ctm() {
        // Inside q/Q the CTM is scaled; after Q it is restored, so the second
        // line is in the original coordinate space.
        let g = collect(b"q 3 0 0 3 0 0 cm 10 10 m 20 10 l S Q 10 50 m 20 50 l S");
        assert_eq!(g.segments.len(), 2);
        // First line scaled x: 30..60; second line unscaled x: 10..20.
        let first = g.segments.iter().find(|s| s.y0 > 25.0).unwrap();
        let second = g
            .segments
            .iter()
            .find(|s| (s.y0 - 50.0).abs() < 1.0)
            .unwrap();
        assert!((first.x1 - 60.0).abs() < 0.1, "scaled line x1={}", first.x1);
        assert!(
            (second.x1 - 20.0).abs() < 0.1,
            "restored line x1={}",
            second.x1
        );
    }
}
