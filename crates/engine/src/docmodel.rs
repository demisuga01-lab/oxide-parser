//! Typed, ordered **document model**: classify and order recovered PDF blocks
//! into titles / headings / paragraphs / lists / figures / captions / tables,
//! with running header / footer / page-number detection. This is the richer
//! semantic layer built on top of the geometric layout analyzer
//! ([`crate::analysis::layout`]), the table detector
//! ([`crate::analysis::tables`]), and the tagged-PDF structure extractor
//! ([`crate::semantic`]).
//!
//! # Design properties
//!
//! - **ML-free.** Classification uses only geometric + typographic features and,
//!   when the PDF is tagged, authored structure roles. Font-size clustering is
//!   *document-relative* (the body size is discovered per document, not assumed).
//! - **Tags-first.** A tagged PDF's `/StructTreeRoot` order *is* the reading
//!   order and its roles *are* the types; the geometric precedence graph is the
//!   fallback for untagged documents.
//! - **Deterministic.** Every ordering/sort uses `f64::total_cmp` (no `<`/`>`
//!   on raw floats, no `partial_cmp().unwrap()`); no `HashMap` is ever iterated
//!   to produce output; all sorts end in a unique index tiebreak; loops are
//!   capped. Same PDF → byte-identical model.
//! - **Honest.** Every classification carries a confidence in `[0,1]` and a
//!   `basis` audit trail. A block whose best score is below the floor is labeled
//!   the generic [`ClassifiedType::Text`] rather than mis-typed.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::Serialize;

use crate::analysis::graphics::{collect_graphics_with_images, DrawnGraphics, ImagePlacement, Rect, Segment};
use crate::analysis::layout::{BBox, LayoutBlock, LayoutConfig, LayoutLine};
use crate::analysis::tables::{detect_tables, Table, TableSource};
use crate::engine::ContentEngine;
use crate::error::Result;
use crate::object::PdfObject;
use crate::semantic::{SemanticDocument, SemanticElement};
use crate::text::{TextChunk, TextCollector};

// ── thresholds (document-relative or scale-free) ────────────────────────────
const SIZE_BUCKET: f64 = 0.5; // pt histogram granularity
const HEAD_SIZE_RATIO: f64 = 1.15; // size >= body*1.15 => heading tier
const SLIGHTLY_LARGE: f64 = 1.05;
const CONF_FLOOR: f64 = 0.50; // below => ClassifiedType::Text
const MAX_LEVEL: u8 = 6;
const MARGIN_BAND: f64 = 0.12; // top/bottom 12% for header/footer
const BOLD_CHAR_MASS: f64 = 0.60; // >=60% bold char mass => is_bold
const ITALIC_CHAR_MASS: f64 = 0.50;
const CAPTION_HOVERLAP: f64 = 0.50;
const FIGURE_AREA_FRAC: f64 = 0.06; // vector cluster >= 6% page area
const MIN_VECTOR_SEGMENTS: usize = 6;
const MIN_VECTOR_RECTS: usize = 3;
const VECTOR_TEXT_FRAC: f64 = 0.18; // < 18% of cluster area covered by text
const TABLE_OVERLAP_IOU: f64 = 0.50;
const TABLE_CONTAIN_FRAC: f64 = 0.80;
const MAX_VECTOR_PRIMS: usize = 20_000;
const FIGURE_ADJ: f64 = 0.75; // image tile-merge gap, * line_h
const VECTOR_ADJ: f64 = 1.5; // vector primitive cluster gap, * line_h
const K_BELOW: f64 = 1.6; // caption-below gap, * line_h
const K_ABOVE: f64 = 1.2; // caption-above gap, * line_h

const BOLD_TOKENS: &[&str] = &[
    "bold", "black", "heavy", "semibold", "demibold", "extrabold", "ultrabold", "medium",
];
const ITALIC_TOKENS: &[&str] = &["italic", "oblique"];

// ── canonical box conversions (single chokepoint) ───────────────────────────
#[inline]
fn rect_to_bbox(r: &Rect) -> BBox {
    // graphics Rect is already normalized x0<=x1, y0<=y1.
    BBox {
        x0: r.x0,
        y0: r.y0,
        x1: r.x1,
        y1: r.y1,
    }
}
#[inline]
fn bbox_to_array(b: &BBox) -> [f64; 4] {
    [b.x0, b.y0, b.x1, b.y1]
}
#[inline]
fn array_to_bbox(a: [f64; 4]) -> BBox {
    BBox {
        x0: a[0],
        y0: a[1],
        x1: a[2],
        y1: a[3],
    }
}

/// Sanitize a box: non-finite coords → 0.0 (FIRST), then swap to enforce
/// `x0<=x1, y0<=y1` (SECOND). The order is pinned: replacing NaN before the swap
/// guarantees a well-formed box even from pathological CTMs.
fn sanitize(mut b: BBox) -> BBox {
    for v in [&mut b.x0, &mut b.y0, &mut b.x1, &mut b.y1] {
        if !v.is_finite() {
            *v = 0.0;
        }
    }
    if b.x0 > b.x1 {
        std::mem::swap(&mut b.x0, &mut b.x1);
    }
    if b.y0 > b.y1 {
        std::mem::swap(&mut b.y0, &mut b.y1);
    }
    b
}

/// Total-order `f64` newtype — the single NaN-safe comparison chokepoint.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedF64(f64);
impl Eq for OrderedF64 {}
impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for OrderedF64 {
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0)
    }
}

// ── geometry helpers (all total_cmp / NaN-safe) ─────────────────────────────
#[inline]
fn top(b: &BBox) -> f64 {
    b.y1
} // y-up: upper edge
#[inline]
fn bottom(b: &BBox) -> f64 {
    b.y0
}
#[inline]
fn cx(b: &BBox) -> f64 {
    (b.x0 + b.x1) * 0.5
}
fn area(b: &BBox) -> f64 {
    b.width() * b.height()
}
fn intersect(a: &BBox, b: &BBox) -> f64 {
    let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    w * h
}
fn iou(a: &BBox, b: &BBox) -> f64 {
    let i = intersect(a, b);
    let u = area(a) + area(b) - i;
    if u <= 0.0 {
        0.0
    } else {
        i / u
    }
}
fn contained_frac(inner: &BBox, outer: &BBox) -> f64 {
    let ai = area(inner);
    if ai <= 0.0 {
        0.0
    } else {
        intersect(inner, outer) / ai
    }
}
fn hoverlap_frac(a: &BBox, b: &BBox) -> f64 {
    let ow = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let denom = a.width().min(b.width());
    if denom <= 0.0 {
        0.0
    } else {
        ow / denom
    }
}
fn union_box(a: &BBox, b: &BBox) -> BBox {
    BBox {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Page-rotation normalization (/Rotate 90/180/270 → upright reading space)
// ════════════════════════════════════════════════════════════════════════════

/// Maps content-stream coordinates of a rotated page (`/Rotate` 90/180/270) into
/// upright reading orientation, so segmentation / table detection / reading-order
/// all run on an upright page. PDF user space is y-up; `/Rotate r` rotates the
/// page *clockwise* by `r` for display, so to normalize we rotate content
/// **counter-clockwise** by `r` about the crop origin and re-anchor into
/// `[0,nw] × [0,nh]` (the post-rotation page size).
struct PageRotation {
    rotate: i32,
    /// Crop-box origin (subtracted before rotating).
    cx0: f64,
    cy0: f64,
    /// Original (pre-rotation) page width/height.
    w0: f64,
    h0: f64,
}

impl PageRotation {
    /// `nw`/`nh` are the post-rotation page dimensions (w/h swapped for 90/270).
    fn new(rotate: i32, crop: [f64; 4], nw: f64, nh: f64) -> Self {
        let (w0, h0) = if rotate == 90 || rotate == 270 {
            (nh, nw) // undo the swap to recover the original page size
        } else {
            (nw, nh)
        };
        PageRotation {
            rotate,
            cx0: crop[0].min(crop[2]),
            cy0: crop[1].min(crop[3]),
            w0,
            h0,
        }
    }

    /// Map a single (x, y) user-space point into upright space.
    #[inline]
    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        let u = x - self.cx0;
        let v = y - self.cy0;
        match self.rotate {
            // 90° clockwise display → rotate content 90° CCW: (u,v) → (v, w0 - u).
            90 => (v, self.w0 - u),
            180 => (self.w0 - u, self.h0 - v),
            // 270° clockwise display → (h0 - v, u).
            270 => (self.h0 - v, u),
            _ => (u, v),
        }
    }

    /// Rotate a text chunk's anchor and orientation. A chunk's box is
    /// `[x, x+width] × [y, y+font_size]`; the anchor `(x, y)` is its lower-left.
    /// For 90/270 the advance direction and the glyph height swap roles, so after
    /// rotating we recompute the box from the rotated corners and re-anchor.
    fn rotate_chunk(&self, c: &mut TextChunk) {
        let fs = if c.font_size > 0.0 { c.font_size } else { 1.0 };
        let w = c.width.max(0.0);
        // Rotate the box's two defining corners and take the axis-aligned hull.
        let (ax, ay) = self.point(c.x, c.y);
        let (bx, by) = self.point(c.x + w, c.y + fs);
        let x0 = ax.min(bx);
        let y0 = ay.min(by);
        let x1 = ax.max(bx);
        let y1 = ay.max(by);
        c.x = x0;
        c.y = y0;
        // After a quarter turn the run reads along the upright x-axis: the box's
        // longer side is the run length (advance/width), the shorter side is the
        // glyph height (font_size). Half turns preserve the axes. Reconstructing
        // width/height from the hull this way keeps a rotated run looking like an
        // ordinary upright run to the segmenter (line grouping by y, advance by x).
        let bw = x1 - x0;
        let bh = y1 - y0;
        if self.rotate == 90 || self.rotate == 270 {
            c.width = bw.max(bh);
            c.font_size = bw.min(bh).max(1.0);
        } else {
            c.width = bw;
            c.font_size = bh.max(1.0);
        }
    }

    fn rotate_rect(&self, r: &Rect) -> Rect {
        let (ax, ay) = self.point(r.x0, r.y0);
        let (bx, by) = self.point(r.x1, r.y1);
        Rect {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        }
    }

    fn rotate_graphics(&self, g: &mut DrawnGraphics) {
        for s in &mut g.segments {
            let (ax, ay) = self.point(s.x0, s.y0);
            let (bx, by) = self.point(s.x1, s.y1);
            *s = Segment {
                x0: ax,
                y0: ay,
                x1: bx,
                y1: by,
            };
        }
        for r in &mut g.rects {
            *r = self.rotate_rect(r);
        }
        for im in &mut g.images {
            *im = ImagePlacement {
                bbox: self.rotate_rect(&im.bbox),
                name: im.name.clone(),
            };
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Public types
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    Tagged,
    Geometric,
}

/// The semantic type of a block. Serialized with an internal `"type"` tag, e.g.
/// `{"type":"heading","level":1}` or `{"type":"list","ordered":true}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClassifiedType {
    Title,
    Heading { level: u8 },
    Paragraph,
    List { ordered: bool },
    ListItem,
    Figure,
    Caption,
    Table,
    Header,
    Footer,
    PageNumber,
    /// Honest low-confidence fallback (better than a wrong label).
    Text,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListItem {
    pub text: String,
    pub bbox: [f64; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    pub ordered: bool,
}

/// One typed block in the recovered document order.
#[derive(Debug, Clone, Serialize)]
pub struct DocBlock {
    /// Stable identifier (survives the `--min-confidence` filter); referenced by
    /// `caption_id` / `figure_id`.
    pub id: usize,
    /// The classified type, flattened so heading levels / list ordering appear
    /// at the block's top level (`"type":"heading","level":1`).
    #[serde(flatten)]
    pub classified: ClassifiedType,
    /// 1-based PDF page number.
    pub page: usize,
    /// Bounding box in user space (y-up) `[x0,y0,x1,y1]`; `[0;4]` when unknown
    /// (a tagged element with no resolvable marked-content geometry).
    pub bbox: [f64; 4],
    pub reading_order_index: usize,
    pub text: String,
    pub confidence: f64,
    /// Sorted audit trail of the cues that fired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub basis: Vec<String>,
    /// Children of a `List` block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ListItem>,
    /// For a `Figure`/`Table`: the id of its linked caption block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption_id: Option<usize>,
    /// For a `Caption`: the id of the figure/table it describes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figure_id: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub header_footer: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub page_number: bool,
    /// Whether the block's text is predominantly bold (≥60% bold char mass).
    /// Block-level emphasis the inline-span layer ([`crate::parse`]) lifts into
    /// `InlineText` so it survives to Markdown/HTML. Geometric path only; tagged
    /// blocks leave this `false` (their emphasis is carried by the role/level).
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_bold: bool,
    /// Whether the block's text is predominantly italic (≥50% italic char mass).
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_italic: bool,
    /// For a `Table` block: the recovered structured table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<Table>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentModel {
    pub source: ModelSource,
    pub page_count: usize,
    pub body_font_size: f64,
    /// Blocks in recovered reading order (`reading_order_index` ascending).
    pub blocks: Vec<DocBlock>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ════════════════════════════════════════════════════════════════════════════
// Ordering layer — precedence graph + deterministic, cycle-safe topo sort
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Text,
    Figure,
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ColumnAssign {
    /// A full-width block (title, spanning header, footer) crossing columns.
    Spanning,
    /// Column band index; 0 = leftmost in visual order.
    Column(usize),
}

#[derive(Debug, Clone)]
pub struct OrderNode {
    pub bbox: BBox, // sanitized, user space y-up
    pub kind: RegionKind,
    pub original_index: usize, // unique; final tiebreak
    pub is_rtl: bool,
    column: ColumnAssign,
}

impl OrderNode {
    pub fn new(bbox: BBox, kind: RegionKind, original_index: usize, is_rtl: bool) -> Self {
        OrderNode {
            bbox: sanitize(bbox),
            kind,
            original_index,
            is_rtl,
            column: ColumnAssign::Column(0),
        }
    }

    /// Pre-assign this node's column band (or `None` for a full-width spanning
    /// node). Used when an upstream segmenter already knows the columns, so the
    /// ordering trusts that rather than re-deriving bands from block geometry.
    fn pre_columned(
        bbox: BBox,
        kind: RegionKind,
        original_index: usize,
        is_rtl: bool,
        column: Option<usize>,
    ) -> Self {
        let mut n = OrderNode::new(bbox, kind, original_index, is_rtl);
        n.column = match column {
            Some(c) => ColumnAssign::Column(c),
            None => ColumnAssign::Spanning,
        };
        n
    }
}

/// Assign each node to a column band (or `Spanning`), keyed off the document
/// line height so the column gutter threshold is resolution-independent.
fn assign_columns(nodes: &mut [OrderNode], line_height: f64) {
    // 1. Column unit = median width of TEXT nodes (figures/tables excluded).
    let mut text_w: Vec<f64> = nodes
        .iter()
        .filter(|n| n.kind == RegionKind::Text && area(&n.bbox) > 0.0)
        .map(|n| n.bbox.width())
        .collect();
    if text_w.is_empty() {
        for n in nodes.iter_mut() {
            n.column = ColumnAssign::Spanning;
        }
        return;
    }
    text_w.sort_by(|a, b| a.total_cmp(b));
    let single_col_w = text_w[text_w.len() / 2].max(1.0);

    // 2. Band the x-centres of text nodes by GUTTER gaps (a real column gutter is
    //    column_gap_factor * line_height — reuse the layout analyzer's factor).
    let gap_thresh = (LayoutConfig::default().column_gap_factor * line_height).max(1.0);
    let mut centres: Vec<(f64, usize)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == RegionKind::Text && area(&n.bbox) > 0.0)
        .map(|(i, n)| (cx(&n.bbox), i))
        .collect();
    centres.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut band_anchor: Vec<f64> = Vec::new(); // mean cx per band
    let mut band_xmin: Vec<f64> = Vec::new();
    let mut band_xmax: Vec<f64> = Vec::new();
    let mut last_cx = f64::NEG_INFINITY;
    let mut cur_sum = 0.0;
    let mut cur_n = 0.0;
    let mut cur_xmin = f64::INFINITY;
    let mut cur_xmax = f64::NEG_INFINITY;
    for &(c, idx) in &centres {
        if last_cx.is_finite() && (c - last_cx) > gap_thresh {
            if cur_n > 0.0 {
                band_anchor.push(cur_sum / cur_n);
                band_xmin.push(cur_xmin);
                band_xmax.push(cur_xmax);
            }
            cur_sum = 0.0;
            cur_n = 0.0;
            cur_xmin = f64::INFINITY;
            cur_xmax = f64::NEG_INFINITY;
        }
        cur_sum += c;
        cur_n += 1.0;
        cur_xmin = cur_xmin.min(nodes[idx].bbox.x0);
        cur_xmax = cur_xmax.max(nodes[idx].bbox.x1);
        last_cx = c;
    }
    if cur_n > 0.0 {
        band_anchor.push(cur_sum / cur_n);
        band_xmin.push(cur_xmin);
        band_xmax.push(cur_xmax);
    }

    let num_bands = band_anchor.len();

    // 3. Assign every node.
    for n in nodes.iter_mut() {
        if num_bands <= 1 {
            n.column = ColumnAssign::Column(0); // single column: nothing spans
            continue;
        }
        let spans_by_width =
            n.bbox.width().total_cmp(&(1.5 * single_col_w)) == Ordering::Greater;
        // A band is "hit" only if the node's x-range overlaps the band's x-extent
        // by a meaningful amount (≥ 25% of a column width). A column block that
        // merely touches its neighbour's edge does NOT count — that near-touch is
        // what previously mislabelled clean column blocks as spanning.
        let min_overlap = 0.25 * single_col_w;
        let bands_hit = (0..num_bands)
            .filter(|&b| {
                let ov = (n.bbox.x1.min(band_xmax[b]) - n.bbox.x0.max(band_xmin[b])).max(0.0);
                ov >= min_overlap
            })
            .count();
        if spans_by_width || bands_hit >= 2 {
            n.column = ColumnAssign::Spanning;
        } else {
            let mut best = 0usize;
            let mut best_d = f64::INFINITY;
            for (b, &anchor) in band_anchor.iter().enumerate() {
                let d = (cx(&n.bbox) - anchor).abs();
                if d.total_cmp(&best_d) == Ordering::Less {
                    best_d = d;
                    best = b;
                }
            }
            n.column = ColumnAssign::Column(best);
        }
    }
}

fn veps(line_height: f64) -> f64 {
    0.5 * line_height
}

fn vertically_overlap(a: &BBox, b: &BBox, eps: f64) -> bool {
    top(a).total_cmp(&(bottom(b) + eps)) != Ordering::Less
        && top(b).total_cmp(&(bottom(a) + eps)) != Ordering::Less
}

/// "`a` reads before `b`" from geometry. RTL flips only the *column traversal*
/// direction, never the within-column top-to-bottom order.
fn precedes(a: &OrderNode, b: &OrderNode, rtl: bool, line_height: f64) -> bool {
    use ColumnAssign::*;
    let eps = veps(line_height);
    match (a.column, b.column) {
        // Spanning vs Spanning: higher y first.
        (Spanning, Spanning) => top(&a.bbox).total_cmp(&top(&b.bbox)) == Ordering::Greater,

        // Spanning vs Column: only when clearly separated vertically. If they
        // overlap, emit no edge and let the band-first frontier key place them.
        (Spanning, Column(_)) => {
            if vertically_overlap(&a.bbox, &b.bbox, eps) {
                return false;
            }
            bottom(&a.bbox).total_cmp(&top(&b.bbox)) != Ordering::Less // a above b => a first
        }
        (Column(_), Spanning) => {
            if vertically_overlap(&a.bbox, &b.bbox, eps) {
                return false;
            }
            top(&a.bbox).total_cmp(&bottom(&b.bbox)) == Ordering::Greater // a above b => a first
        }

        // Same column: higher y first.
        (Column(ca), Column(cb)) if ca == cb => {
            top(&a.bbox).total_cmp(&top(&b.bbox)) == Ordering::Greater
        }

        // Different columns: edge only when vertically overlapping (same row band).
        (Column(ca), Column(cb)) => {
            if !vertically_overlap(&a.bbox, &b.bbox, eps) {
                return false;
            }
            if rtl {
                cb.cmp(&ca) == Ordering::Less
            } else {
                ca.cmp(&cb) == Ordering::Less
            }
        }
    }
}

/// Total-order sort key for the topo frontier: (band rank, -top, unique index).
type OrderKey = (OrderedF64, OrderedF64, usize);

/// Frontier key (smaller = earlier): **column-major** — band first, then higher
/// y first, then unique index. Column-major is the fix that stops the topo sort
/// from interleaving columns when one column is shorter than the other.
fn order_key(n: &OrderNode, rtl: bool) -> OrderKey {
    let band_rank: f64 = match n.column {
        ColumnAssign::Spanning => f64::NEG_INFINITY, // spanning sorts first among ready ties
        ColumnAssign::Column(c) if rtl => -(c as f64), // RTL: rightmost band first
        ColumnAssign::Column(c) => c as f64,
    };
    (OrderedF64(band_rank), OrderedF64(-top(&n.bbox)), n.original_index)
}

/// Reading order of `nodes`: build the precedence graph, then Kahn-sort it.
pub fn reading_order(nodes: &[OrderNode], rtl: bool, line_height: f64) -> Vec<usize> {
    let n = nodes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for i in 0..n {
        for j in 0..n {
            if i != j
                && precedes(&nodes[i], &nodes[j], rtl, line_height)
                && !precedes(&nodes[j], &nodes[i], rtl, line_height)
            {
                adj[i].push(j);
                indeg[j] += 1;
            }
        }
    }
    reading_order_from_edges(adj, indeg, nodes, rtl)
}

/// Kahn topological sort with the column-major frontier key. An internal seam so
/// cycle-safety is testable with a hand-built adjacency. On a cycle (no node
/// constructible through `precedes` produces one, but the guard is cheap
/// insurance), the remaining nodes are appended in `order_key` order.
fn reading_order_from_edges(
    adj: Vec<Vec<usize>>,
    mut indeg: Vec<usize>,
    nodes: &[OrderNode],
    rtl: bool,
) -> Vec<usize> {
    let n = nodes.len();
    // Min-heap (via Reverse) keyed by (order key, node index); the trailing index
    // makes ties total-ordered for determinism.
    let mut frontier: BinaryHeap<Reverse<(OrderKey, usize)>> = BinaryHeap::new();
    for i in 0..n {
        if indeg[i] == 0 {
            frontier.push(Reverse((order_key(&nodes[i], rtl), i)));
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut visited = vec![false; n];
    while let Some(Reverse((_, u))) = frontier.pop() {
        if visited[u] {
            continue;
        }
        visited[u] = true;
        out.push(u);
        for &v in &adj[u] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                frontier.push(Reverse((order_key(&nodes[v], rtl), v)));
            }
        }
    }
    if out.len() < n {
        let mut rest: Vec<usize> = (0..n).filter(|&i| !visited[i]).collect();
        rest.sort_by_key(|&i| order_key(&nodes[i], rtl));
        out.extend(rest);
    }
    out
}

// ════════════════════════════════════════════════════════════════════════════
// Document statistics (document-relative font sizing)
// ════════════════════════════════════════════════════════════════════════════

pub struct DocStats {
    pub body_size: f64,
    /// Distinct buckets `>= body*HEAD_SIZE_RATIO`, DESC; `[0]` is the H1 size.
    pub heading_tiers: Vec<f64>,
    pub median_line_height: f64,
    pub column_width: f64,
}

fn bucket(size: f64) -> f64 {
    (size / SIZE_BUCKET).round() * SIZE_BUCKET
}

fn compute_doc_stats(all_blocks: &[(usize, &LayoutBlock)]) -> DocStats {
    // Char-weighted histogram over 0.5pt buckets (BTreeMap => deterministic).
    let mut hist: BTreeMap<OrderedF64, usize> = BTreeMap::new();
    let mut sizes: Vec<f64> = Vec::new();
    for (_p, b) in all_blocks {
        let chars: usize = b.lines.iter().map(|l| l.text.chars().count()).sum();
        *hist.entry(OrderedF64(bucket(b.font_size))).or_insert(0) += chars.max(1);
        sizes.push(b.font_size);
    }
    // Body = max char-weight bucket; tie → SMALLER size (BTreeMap asc + strict >).
    let mut body = 1.0;
    let mut best_w = 0usize;
    for (k, &w) in &hist {
        if w > best_w {
            best_w = w;
            body = k.0;
        }
    }
    let body = body.max(1.0);
    let mut tiers: Vec<f64> = hist
        .keys()
        .map(|k| k.0)
        .filter(|&s| s >= body * HEAD_SIZE_RATIO)
        .collect();
    tiers.sort_by(|a, b| b.total_cmp(a)); // DESC
    tiers.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if tiers.len() > MAX_LEVEL as usize {
        tiers.truncate(MAX_LEVEL as usize);
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    let median_line_height = sizes.get(sizes.len() / 2).copied().unwrap_or(1.0).max(1.0);
    let mut bw: Vec<f64> = all_blocks
        .iter()
        .filter(|(_p, b)| (bucket(b.font_size) - body).abs() < 1e-9)
        .map(|(_p, b)| b.bbox.width())
        .collect();
    bw.sort_by(|a, b| a.total_cmp(b));
    let column_width = bw.get(bw.len() / 2).copied().unwrap_or(1.0).max(1.0);
    DocStats {
        body_size: body,
        heading_tiers: tiers,
        median_line_height,
        column_width,
    }
}

fn heading_level_for_size(size: f64, doc: &DocStats) -> Option<u8> {
    let b = bucket(size);
    for (rank, &t) in doc.heading_tiers.iter().enumerate() {
        if (b - t).abs() < 1e-9 || b >= t {
            return Some(((rank as u8) + 1).min(MAX_LEVEL));
        }
    }
    None
}
fn lowest_level(doc: &DocStats) -> u8 {
    ((doc.heading_tiers.len() as u8) + 1).clamp(1, MAX_LEVEL)
}

// ════════════════════════════════════════════════════════════════════════════
// Hand-written prefix scanners (no regex dependency; deterministic)
// ════════════════════════════════════════════════════════════════════════════

fn is_bullet_marker(line: &str) -> bool {
    let t = line.trim_start();
    let mut chars = t.chars();
    match chars.next() {
        Some(
            '\u{2022}' // •
            | '\u{25E6}' // ◦
            | '\u{2023}' // ‣
            | '\u{00B7}' // ·
            | '\u{2043}' // ⁃
            | '\u{2219}' // ∙
            | '\u{2013}' // –
            | '\u{2014}' // —
            | '-'
            | '*',
        ) => {
            // Must be followed by whitespace then a non-space (a real list, not a
            // hyphenated word or "*emphasis").
            matches!(chars.next(), Some(w) if w.is_whitespace())
                && t.chars().nth(2).map(|c| !c.is_whitespace()).unwrap_or(false)
        }
        _ => false,
    }
}

/// Returns `Some(ordered)` if the line starts with an enumerator marker;
/// `ordered=true` for numeric/alpha/roman sequences.
fn enum_marker(line: &str) -> Option<bool> {
    let t = line.trim_start();
    let bytes: Vec<char> = t.chars().collect();
    if bytes.is_empty() {
        return None;
    }
    // (1) / (a) / [1] forms.
    if let Some(&open) = bytes.first() {
        if open == '(' || open == '[' {
            let close = if open == '(' { ')' } else { ']' };
            if let Some(pos) = bytes.iter().position(|&c| c == close) {
                if (2..=9).contains(&pos) {
                    let inner: String = bytes[1..pos].iter().collect();
                    if is_enumerator_token(inner.trim()) {
                        // must be followed by whitespace + content
                        return bytes
                            .get(pos + 1)
                            .map(|c| c.is_whitespace())
                            .filter(|&w| w)
                            .map(|_| true);
                    }
                }
            }
        }
    }
    // 1. / a) / iv. forms: token then '.' or ')' then whitespace.
    for sep_pos in 1..bytes.len().min(9) {
        let c = bytes[sep_pos];
        if c == '.' || c == ')' {
            let token: String = bytes[..sep_pos].iter().collect();
            if is_enumerator_token(&token) {
                let after = bytes.get(sep_pos + 1);
                if after.map(|c| c.is_whitespace()).unwrap_or(false) {
                    return Some(true);
                }
            }
            break;
        }
        if !(c.is_ascii_alphanumeric()) {
            break;
        }
    }
    None
}

fn is_enumerator_token(t: &str) -> bool {
    if t.is_empty() || t.len() > 7 {
        return false;
    }
    // arabic
    if t.chars().all(|c| c.is_ascii_digit()) && t.len() <= 3 {
        return true;
    }
    // single latin letter
    if t.len() == 1 && t.chars().next().unwrap().is_ascii_alphabetic() {
        return true;
    }
    // roman numeral (lower or upper, not mixed)
    let lower = t.to_ascii_lowercase();
    if lower.chars().all(|c| matches!(c, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm')) {
        let all_upper = t.chars().all(|c| c.is_ascii_uppercase());
        let all_lower = t.chars().all(|c| c.is_ascii_lowercase());
        return all_upper || all_lower;
    }
    false
}

/// True if the line opens with a section number like "1.", "1.2", "1.2.3",
/// "Chapter 3", "Section 2", "Appendix A".
fn is_section_numbered(line: &str) -> bool {
    let t = line.trim_start();
    let lower = t.to_ascii_lowercase();
    for kw in ["chapter ", "section ", "appendix "] {
        if let Some(rest) = lower.strip_prefix(kw) {
            return rest
                .trim_start()
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
        }
    }
    // dotted numeric: digits ('.' digits)* optionally a trailing '.' then space/letter
    let mut chars = t.chars().peekable();
    let mut saw_digit = false;
    let mut consumed = 0usize;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            saw_digit = true;
            chars.next();
            consumed += 1;
        } else if c == '.' && saw_digit {
            chars.next();
            consumed += 1;
        } else {
            break;
        }
        if consumed > 12 {
            return false;
        }
    }
    if !saw_digit {
        return false;
    }
    // require a separator (space) after the number prefix, and some text after
    match chars.peek() {
        Some(&c) if c.is_whitespace() => true,
        None => false,
        _ => false,
    }
}

/// True if the line begins with a caption prefix like "Figure 1", "Fig. 2:",
/// "Table 3 —", "Exhibit A".
fn is_caption_prefixed(line: &str) -> bool {
    let t = line.trim_start();
    let lower = t.to_ascii_lowercase();
    // Each keyword must be followed by a word boundary (space or punctuation),
    // so "Figures show…" (no boundary after "figure") is NOT a caption, while
    // "Figure 1", "Fig. 2", "Table 3 —" are.
    const KWS: &[&str] = &[
        "figure", "fig", "table", "tbl", "exhibit", "listing", "plate", "chart", "scheme",
        "equation", "eq",
    ];
    for kw in KWS {
        let Some(rest_raw) = lower.strip_prefix(kw) else {
            continue;
        };
        // The char immediately after the keyword must be a boundary (a '.' as in
        // "Fig." or whitespace), never another letter ("Figures").
        let boundary = match rest_raw.chars().next() {
            Some('.') => true,
            Some(c) if c.is_whitespace() => true,
            _ => false,
        };
        if !boundary {
            continue;
        }
        // After the boundary, expect a figure/table number or letter/roman id.
        let rest = rest_raw.trim_start_matches('.').trim_start();
        if let Some(c) = rest.chars().next() {
            if c.is_ascii_digit() || c.is_ascii_alphabetic() {
                return true;
            }
        }
    }
    false
}

/// If the line is a standalone page-number marker, return its integer value
/// (arabic or roman). Used for both detection and sequence checking.
fn page_number_value(line: &str) -> Option<i64> {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    let core = lower.strip_prefix("page ").map(str::trim).unwrap_or(&lower);
    // strip a trailing "/ N" or "of N"
    let core = core
        .split('/')
        .next()
        .unwrap_or(core)
        .split(" of ")
        .next()
        .unwrap_or(core)
        .trim();
    if core.is_empty() || core.len() > 12 {
        return None;
    }
    if core.chars().all(|c| c.is_ascii_digit()) {
        return core.parse::<i64>().ok();
    }
    if core
        .chars()
        .all(|c| matches!(c, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
    {
        return roman_to_int(core);
    }
    None
}

fn roman_to_int(s: &str) -> Option<i64> {
    let val = |c: char| match c {
        'i' => 1,
        'v' => 5,
        'x' => 10,
        'l' => 50,
        'c' => 100,
        'd' => 500,
        'm' => 1000,
        _ => 0,
    };
    let chars: Vec<char> = s.chars().collect();
    let mut total = 0i64;
    for i in 0..chars.len() {
        let v = val(chars[i]);
        if v == 0 {
            return None;
        }
        if i + 1 < chars.len() && val(chars[i + 1]) > v {
            total -= v;
        } else {
            total += v;
        }
    }
    if total > 0 {
        Some(total)
    } else {
        None
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Block features + classifier
// ════════════════════════════════════════════════════════════════════════════

/// Typographic/geometric features the classifier scores. Header/footer/
/// page-number band membership is decided later from `DocBlock.bbox` in the
/// cross-page pass, so it is not duplicated here.
struct BlockFeatures {
    font_size: f64,
    size_ratio: f64,
    is_bold: bool,
    is_italic: bool,
    line_count: usize,
    word_count: usize,
    first_line_text: String,
    fill_ratio: f64,
    ends_with_sentence_punct: bool,
    gap_above: f64,
}

fn ends_sentence(t: &str) -> bool {
    matches!(t.trim_end().chars().last(), Some('.') | Some('!') | Some('?') | Some(':'))
}

fn score_heading(f: &BlockFeatures, doc: &DocStats) -> (f64, Vec<String>) {
    let mut s: f64 = 0.0;
    let mut basis = Vec::new();
    if f.size_ratio >= HEAD_SIZE_RATIO {
        s += 0.45;
        basis.push(format!("size:{:.2}x", f.size_ratio));
    } else if f.size_ratio >= SLIGHTLY_LARGE {
        s += 0.15;
        basis.push("size:slightly-large".into());
    }
    if f.line_count <= 2 {
        s += 0.10;
        basis.push("short:lines".into());
    }
    if f.word_count <= 12 {
        s += 0.10;
        basis.push("short:words".into());
    }
    if f.fill_ratio <= 0.6 {
        s += 0.10;
        basis.push("short:fill".into());
    }
    if f.is_bold {
        s += 0.15;
        basis.push("bold".into());
    }
    if f.is_italic && !f.is_bold {
        s += 0.03;
    }
    if f.gap_above >= 1.5 * doc.median_line_height {
        s += 0.08;
        basis.push("space-above".into());
    }
    if is_section_numbered(&f.first_line_text) {
        s += 0.20;
        basis.push("numbered".into());
    }
    if f.ends_with_sentence_punct && f.word_count > 12 {
        s -= 0.25;
        basis.push("-prose-punct".into());
    }
    if f.line_count >= 4 {
        s -= 0.30;
        basis.push("-multiline".into());
    }
    basis.sort();
    (s.clamp(0.0, 1.0), basis)
}

fn score_paragraph(f: &BlockFeatures) -> f64 {
    let mut s: f64 = 0.55;
    if f.line_count >= 2 {
        s += 0.10;
    }
    if f.ends_with_sentence_punct {
        s += 0.10;
    }
    if f.fill_ratio >= 0.6 {
        s += 0.10;
    }
    if (0.95..=1.10).contains(&f.size_ratio) {
        s += 0.10;
    }
    if f.is_bold && f.word_count <= 12 {
        s -= 0.20;
    }
    // A real paragraph is prose, not a stray fragment. A single short line of
    // only a word or two, at an unusual size and with no terminal punctuation,
    // is not confidently a paragraph — let it fall to the honest `Text` floor
    // rather than be mislabelled.
    if f.word_count <= 2 && f.line_count == 1 && !f.ends_with_sentence_punct {
        s -= 0.25;
    }
    s.clamp(0.0, 1.0)
}

fn classify_block(f: &BlockFeatures, doc: &DocStats) -> (ClassifiedType, f64, Vec<String>) {
    let (h_s, h_basis) = score_heading(f, doc);
    let p_s = score_paragraph(f);
    let heading_type = match heading_level_for_size(f.font_size, doc) {
        Some(level) => ClassifiedType::Heading { level },
        None => ClassifiedType::Heading {
            level: lowest_level(doc),
        },
    };
    let candidates: [(ClassifiedType, f64, Vec<String>); 2] = [
        (heading_type, h_s, h_basis),
        (ClassifiedType::Paragraph, p_s, vec!["prose".into()]),
    ];
    let mut best = 0usize;
    for i in 1..candidates.len() {
        if candidates[i].1.total_cmp(&candidates[best].1) == Ordering::Greater {
            best = i;
        }
    }
    let (bt, bs, bb) = candidates[best].clone();
    if bs < CONF_FLOOR {
        return (
            ClassifiedType::Text,
            bs,
            vec!["low-confidence;fallback".into()],
        );
    }
    (bt, bs, bb)
}

// ════════════════════════════════════════════════════════════════════════════
// Finer page segmentation (docmodel-local; the analyzer's `analyze_page` is
// left intact).
//
// The analyzer's XY-cut is tuned for clean Manhattan layouts and, on tight real-world
// columns, collapses a page into one or two giant blocks — merging headings into
// body and interleaving columns. Because this layer needs heading boundaries and
// per-column blocks to classify and order, it does its own segmentation from the
// raw chunks: cluster into lines, split into columns by x-projection gutters,
// then split each column's lines into blocks at large vertical gaps OR font-size
// changes. The output is ordinary `LayoutBlock`s, so the classifier and
// precedence graph downstream are unchanged.
// ════════════════════════════════════════════════════════════════════════════

struct SegLine {
    text: String,
    bbox: BBox,
    font_size: f64,
    is_rtl: bool,
}

/// Median font size across a page's chunks (the document line-height proxy).
fn page_line_height(chunks: &[TextChunk]) -> f64 {
    let mut sizes: Vec<f64> = chunks
        .iter()
        .filter(|c| !c.is_vertical && !c.text.trim().is_empty() && c.font_size > 0.0)
        .map(|c| c.font_size)
        .collect();
    if sizes.is_empty() {
        return 1.0;
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    sizes[sizes.len() / 2].max(1.0)
}

/// Cluster horizontal chunks into lines (by y-centre proximity), each line's
/// chunks joined left-to-right with inferred spacing. Vertical/empty chunks are
/// skipped (they carry no block structure here).
fn chunks_to_lines(chunks: &[TextChunk], line_h: f64) -> Vec<SegLine> {
    let mut items: Vec<&TextChunk> = chunks
        .iter()
        .filter(|c| !c.is_vertical && !c.text.trim().is_empty())
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    // Sort by descending y-centre (top first).
    items.sort_by(|a, b| {
        let ay = a.y + a.font_size.max(1.0) / 2.0;
        let by = b.y + b.font_size.max(1.0) / 2.0;
        by.total_cmp(&ay)
            .then(a.x.total_cmp(&b.x))
    });
    let tol = 0.6 * line_h;
    let mut groups: Vec<Vec<&TextChunk>> = Vec::new();
    for c in items {
        let cy = c.y + c.font_size.max(1.0) / 2.0;
        let same = groups
            .last()
            .map(|g| {
                let gy = g[0].y + g[0].font_size.max(1.0) / 2.0;
                (gy - cy).abs() <= tol
            })
            .unwrap_or(false);
        if same {
            groups.last_mut().unwrap().push(c);
        } else {
            groups.push(vec![c]);
        }
    }
    groups.into_iter().map(|g| build_seg_line(g, line_h)).collect()
}

fn build_seg_line(mut chunks: Vec<&TextChunk>, line_h: f64) -> SegLine {
    chunks.sort_by(|a, b| a.x.total_cmp(&b.x));
    let x0 = chunks.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
    let x1 = chunks
        .iter()
        .map(|c| c.x + c.width.max(0.0))
        .fold(f64::NEG_INFINITY, f64::max);
    let y0 = chunks.iter().map(|c| c.y).fold(f64::INFINITY, f64::min);
    let y1 = chunks
        .iter()
        .map(|c| c.y + c.font_size.max(1.0))
        .fold(f64::NEG_INFINITY, f64::max);
    let mut sizes: Vec<f64> = chunks.iter().map(|c| c.font_size.max(1.0)).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let font_size = sizes[sizes.len() / 2];
    let rtl_n = chunks.iter().filter(|c| c.is_rtl).count();
    let is_rtl = rtl_n * 2 > chunks.len();

    let word_gap = 0.25 * line_h;
    let mut text = String::new();
    let mut prev_right: Option<f64> = None;
    for c in &chunks {
        if let Some(pr) = prev_right {
            if c.x - pr > word_gap && !text.ends_with(' ') {
                text.push(' ');
            }
        }
        text.push_str(&c.text);
        prev_right = Some(c.x + c.width.max(0.0));
    }
    let text = if is_rtl {
        text.split(' ').rev().collect::<Vec<_>>().join(" ")
    } else {
        text
    };
    SegLine {
        text: text.trim().to_string(),
        bbox: sanitize(BBox { x0, y0, x1, y1 }),
        font_size,
        is_rtl,
    }
}

/// Split one column's (top-to-bottom) lines into blocks at large vertical gaps
/// OR font-size changes (so a heading line becomes its own block).
fn lines_to_blocks(mut lines: Vec<SegLine>, line_h: f64) -> Vec<LayoutBlock> {
    if lines.is_empty() {
        return Vec::new();
    }
    lines.sort_by(|a, b| (-top(&a.bbox)).total_cmp(&-top(&b.bbox)));

    // Typical line pitch (centre-to-centre of adjacent lines), low percentile so
    // a paragraph gap stands out from normal leading.
    let mut pitches: Vec<f64> = lines
        .windows(2)
        .map(|w| (cy_center(&w[0].bbox) - cy_center(&w[1].bbox)).abs())
        .filter(|p| *p > 0.0)
        .collect();
    pitches.sort_by(|a, b| a.total_cmp(b));
    let typical_pitch = if pitches.is_empty() {
        line_h * 1.2
    } else {
        pitches[pitches.len() / 4].max(line_h * 0.8)
    };
    let para_gap = typical_pitch * 1.6;

    let mut blocks: Vec<LayoutBlock> = Vec::new();
    let mut current: Vec<SegLine> = Vec::new();
    let mut prev_center: Option<f64> = None;
    let mut cur_size: Option<f64> = None;
    // Whether the current block so far holds a list marker, and the marker run's
    // left edge (for distinguishing a hanging-indent continuation line from a
    // left-aligned prose line that ends the list).
    let mut cur_has_marker = false;
    let mut marker_left: Option<f64> = None;
    let tol = 0.5 * line_h;
    for line in lines {
        let c = cy_center(&line.bbox);
        let is_marker = is_bullet_marker(&line.text) || enum_marker(&line.text).is_some();
        let size_break = match cur_size {
            // a meaningful font-size change ends the current block
            Some(s) => (bucket(line.font_size) - bucket(s)).abs() >= SIZE_BUCKET * 1.5,
            None => false,
        };
        let gap_break = match prev_center {
            Some(pc) => (pc - c).abs() > para_gap,
            None => false,
        };
        // List-marker transition breaks (so an intro paragraph, a bullet list, and
        // a trailing paragraph become THREE blocks even with normal leading):
        //  - entering a list: this line is a marker, but the current block has
        //    only non-marker prose so far;
        //  - leaving a list: the current block is a list, this line is NOT a
        //    marker, and it is left-aligned with the markers (a wrapped
        //    continuation line is indented further right, so it stays).
        let enter_list = is_marker && !current.is_empty() && !cur_has_marker;
        let leave_list = cur_has_marker
            && !is_marker
            && marker_left
                .map(|m| line.bbox.x0 <= m + tol)
                .unwrap_or(false);
        let marker_break = enter_list || leave_list;

        if (size_break || gap_break || marker_break) && !current.is_empty() {
            blocks.push(finish_seg_block(std::mem::take(&mut current)));
            cur_size = None;
            cur_has_marker = false;
            marker_left = None;
        }
        prev_center = Some(c);
        // a block's representative size tracks its first line (heading lines are
        // single-line blocks, so this stays stable for body paragraphs).
        if cur_size.is_none() {
            cur_size = Some(line.font_size);
        }
        if is_marker {
            cur_has_marker = true;
            if marker_left.is_none() {
                marker_left = Some(line.bbox.x0);
            }
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(finish_seg_block(current));
    }
    blocks
}

fn cy_center(b: &BBox) -> f64 {
    (b.y0 + b.y1) / 2.0
}

fn finish_seg_block(lines: Vec<SegLine>) -> LayoutBlock {
    let bbox = lines
        .iter()
        .map(|l| l.bbox)
        .reduce(|a, b| union_box(&a, &b))
        .unwrap_or(BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        });
    let mut sizes: Vec<f64> = lines.iter().map(|l| l.font_size).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let font_size = sizes[sizes.len() / 2];
    let layout_lines: Vec<LayoutLine> = lines
        .into_iter()
        .map(|l| LayoutLine {
            text: l.text,
            bbox: l.bbox,
            is_rtl: l.is_rtl,
        })
        .collect();
    LayoutBlock {
        bbox,
        lines: layout_lines,
        font_size,
    }
}

/// Detect column boundaries from chunk LEFT-EDGE clusters and split chunks into
/// columns BEFORE line grouping (so a left-column line and a right-column line
/// at the same y are never merged). Tight real-world columns have no empty
/// vertical gutter (words flow edge-to-edge within each line), but their line
/// *starts* cluster sharply at each column's left margin — that is the robust
/// signal (this mirrors the analyzer's `find_column_split_x` left-edge histogram).
/// Returns chunk groups in visual left-to-right order; one group when no
/// confident multi-column structure is found.
fn split_chunks_into_columns(chunks: Vec<&TextChunk>, line_h: f64) -> Vec<Vec<&TextChunk>> {
    if chunks.len() < 12 {
        return vec![chunks];
    }
    let x_min = chunks.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
    let x_max = chunks
        .iter()
        .map(|c| c.x + c.width.max(0.0))
        .fold(f64::NEG_INFINITY, f64::max);
    if !(x_min.is_finite() && x_max.is_finite()) {
        return vec![chunks];
    }
    let span = x_max - x_min;
    // A column gutter (and thus minimum spacing between two column starts) is at
    // least this wide; below it, two "peaks" are the same column.
    let min_col_sep = (span / 6.0).max(LayoutConfig::default().column_gap_factor * line_h * 3.0);

    // Left-edge histogram. Bin at roughly the line height so a column's start
    // margin lands in ONE bin (finer bins dilute the margin across neighbours and
    // hide it below the floor).
    let bw = line_h.max(8.0);
    let nbins = (((span) / bw).ceil() as usize).max(1);
    let mut hist = vec![0u32; nbins];
    for c in &chunks {
        let b = (((c.x - x_min) / bw) as usize).min(nbins - 1);
        hist[b] += 1;
    }
    // Column start-margins are the x-positions where many lines begin. Rather
    // than fragile strict local maxima (margins spread across adjacent bins),
    // greedily pick the highest-count bins that are each ≥ `min_col_sep` from
    // every margin already chosen. A bin must hold a meaningful share (≥4% of
    // line-starts) to be a margin at all.
    let total: u32 = hist.iter().sum();
    let peak_floor = ((total as f64) * 0.04).ceil() as u32;
    let mut order: Vec<usize> = (0..nbins).filter(|&i| hist[i] >= peak_floor).collect();
    order.sort_by(|&a, &b| hist[b].cmp(&hist[a]).then(a.cmp(&b))); // count desc, x asc
    let mut peaks: Vec<(f64, u32)> = Vec::new();
    for i in order {
        let x = x_min + i as f64 * bw;
        if peaks.iter().all(|&(px, _)| (x - px).abs() >= min_col_sep) {
            peaks.push((x, hist[i]));
        }
    }
    peaks.sort_by(|a, b| a.0.total_cmp(&b.0)); // left-to-right
    if peaks.len() < 2 {
        return vec![chunks];
    }
    // Assign each chunk to the LAST column whose start-margin (peak x) is at or
    // left of the chunk's left edge. This is range assignment, not nearest: a
    // mid-line word at x=250 in a column that starts at x=54 (next column at
    // x=314) correctly stays in column 0, because 54 ≤ 250 < 314. (Nearest-peak
    // would wrongly pull it right.) A small tolerance absorbs a word that starts
    // a hair left of its column margin (justified/indented first line).
    let ncols = peaks.len();
    let tol = 0.5 * line_h;
    let mut cols: Vec<Vec<&TextChunk>> = vec![Vec::new(); ncols];
    for c in chunks {
        let mut col = 0usize;
        for (k, &(px, _)) in peaks.iter().enumerate() {
            if c.x >= px - tol {
                col = k;
            }
        }
        cols[col].push(c);
    }
    // Require every column to hold a meaningful share, else treat as single
    // column (avoids over-splitting a stray right-aligned run).
    let total_chunks: usize = cols.iter().map(Vec::len).sum();
    let min_share = ((total_chunks as f64) * 0.12).ceil() as usize;
    if cols.iter().any(|c| c.len() < min_share.max(3)) {
        return vec![cols.into_iter().flatten().collect()];
    }
    cols
}

/// Full docmodel-local page segmentation: chunks → columns → lines → blocks.
/// Returns the column-tagged blocks, whether the page is RTL-dominant, and the
/// detected column count.
fn segment_page(chunks: &[TextChunk], line_h: f64) -> (Vec<SegBlock>, bool, usize) {
    let usable: Vec<&TextChunk> = chunks
        .iter()
        .filter(|c| !c.is_vertical && !c.text.trim().is_empty())
        .collect();
    if usable.is_empty() {
        return (Vec::new(), false, 1);
    }
    let rtl_n = usable.iter().filter(|c| c.is_rtl).count();
    let page_is_rtl = rtl_n * 2 > usable.len();

    // Columns first (at the chunk level), then lines within each column. Each
    // block is tagged with its column index so the precedence ordering uses the
    // segmentation's column structure directly (more reliable than re-deriving
    // bands from finished-block centres).
    let columns = split_chunks_into_columns(usable, line_h);
    let ncols = columns.len();
    let mut blocks: Vec<SegBlock> = Vec::new();
    for (col_idx, col) in columns.into_iter().enumerate() {
        let owned: Vec<TextChunk> = col.into_iter().cloned().collect();
        let lines = chunks_to_lines(&owned, line_h);
        for b in lines_to_blocks(lines, line_h) {
            blocks.push(SegBlock {
                block: b,
                column: col_idx,
            });
        }
    }
    (blocks, page_is_rtl, ncols)
}

/// A segmented block plus the column it was assigned to (0-based, visual L→R).
struct SegBlock {
    block: LayoutBlock,
    column: usize,
}

// ════════════════════════════════════════════════════════════════════════════
// Per-page assembly (geometric path)
// ════════════════════════════════════════════════════════════════════════════

/// Names of the page's top-level *image* XObjects (never Form XObjects). Used to
/// gate `Do`-placement recording in the graphics collector.
fn page_image_names(engine: &ContentEngine, page: usize) -> Result<BTreeSet<String>> {
    let resources = engine.get_page_resources(page)?;
    let reader = engine.document().reader();
    let mut names = BTreeSet::new();
    for (name, &(obj, gen)) in &resources.xobjects {
        if let Ok(PdfObject::Stream { dict, .. }) = reader.get_object(obj, gen) {
            if dict.get_name("Subtype") == Some("Image") {
                names.insert(name.clone());
            }
        }
    }
    Ok(names)
}

/// A figure region (image placement or text-free vector cluster), user space.
struct FigureRegion {
    bbox: BBox,
}

/// Merge image placements into figures via connected components (overlap or
/// near-adjacency). Order-independent.
fn merge_image_rects(images: &[Rect], line_h: f64) -> Vec<BBox> {
    let n = images.len();
    if n == 0 {
        return Vec::new();
    }
    let boxes: Vec<BBox> = images.iter().map(|r| sanitize(rect_to_bbox(r))).collect();
    let mut dsu = Dsu::new(n);
    let adj = FIGURE_ADJ * line_h;
    for i in 0..n {
        for j in (i + 1)..n {
            if boxes_adjacent(&boxes[i], &boxes[j], adj) {
                dsu.union(i, j);
            }
        }
    }
    components_to_boxes(&boxes, &mut dsu, n)
}

fn boxes_adjacent(a: &BBox, b: &BBox, gap: f64) -> bool {
    // Overlap, or a small gap on one axis while the orthogonal axis overlaps.
    if intersect(a, b) > 0.0 {
        return true;
    }
    let dx = (a.x0.max(b.x0) - a.x1.min(b.x1)).max(0.0);
    let dy = (a.y0.max(b.y0) - a.y1.min(b.y1)).max(0.0);
    let x_overlap = a.x1.min(b.x1) - a.x0.max(b.x0) > 0.0;
    let y_overlap = a.y1.min(b.y1) - a.y0.max(b.y0) > 0.0;
    (dy <= gap && x_overlap) || (dx <= gap && y_overlap)
}

fn components_to_boxes(boxes: &[BBox], dsu: &mut Dsu, n: usize) -> Vec<BBox> {
    let mut by_root: BTreeMap<usize, BBox> = BTreeMap::new();
    for (i, &b) in boxes.iter().enumerate().take(n) {
        let r = dsu.find(i);
        by_root
            .entry(r)
            .and_modify(|acc| *acc = union_box(acc, &b))
            .or_insert(b);
    }
    let mut out: Vec<BBox> = by_root.into_values().collect();
    out.sort_by(|a, b| {
        (-top(a))
            .total_cmp(&-top(b))
            .then(a.x0.total_cmp(&b.x0))
    });
    out
}

/// Vector figure regions: cluster drawn primitives (segments-as-boxes + rects)
/// by proximity; keep clusters that are big, primitive-dense, text-sparse, and
/// not coincident with a detected table.
fn vector_regions(
    graphics: &DrawnGraphics,
    chunks: &[TextChunk],
    tables: &[BBox],
    line_h: f64,
    page_area: f64,
) -> Vec<BBox> {
    let mut prims: Vec<(BBox, bool)> = Vec::new(); // (box, is_rect)
    for s in &graphics.segments {
        prims.push((
            sanitize(BBox {
                x0: s.x0,
                y0: s.y0,
                x1: s.x1,
                y1: s.y1,
            }),
            false,
        ));
        if prims.len() >= MAX_VECTOR_PRIMS {
            break;
        }
    }
    for r in &graphics.rects {
        prims.push((sanitize(rect_to_bbox(r)), true));
        if prims.len() >= MAX_VECTOR_PRIMS {
            break;
        }
    }
    let n = prims.len();
    if n == 0 {
        return Vec::new();
    }
    let mut dsu = Dsu::new(n);
    let adj = VECTOR_ADJ * line_h;
    for i in 0..n {
        for j in (i + 1)..n {
            if boxes_adjacent(&prims[i].0, &prims[j].0, adj) {
                dsu.union(i, j);
            }
        }
    }
    // Aggregate per component: union box, primitive count, rect count.
    let mut acc: BTreeMap<usize, (BBox, usize, usize)> = BTreeMap::new();
    for (i, &(pbox, is_rect)) in prims.iter().enumerate().take(n) {
        let r = dsu.find(i);
        let entry = acc.entry(r).or_insert((pbox, 0, 0));
        entry.0 = union_box(&entry.0, &pbox);
        entry.1 += 1;
        if is_rect {
            entry.2 += 1;
        }
    }

    let mut out: Vec<BBox> = Vec::new();
    for (_root, (bx, prim_count, rect_count)) in acc {
        if !(prim_count >= MIN_VECTOR_SEGMENTS || rect_count >= MIN_VECTOR_RECTS) {
            continue;
        }
        let a = area(&bx);
        if a < FIGURE_AREA_FRAC * page_area {
            continue;
        }
        // text-area fraction inside the cluster
        let mut text_area = 0.0;
        for c in chunks {
            if c.text.trim().is_empty() {
                continue;
            }
            let cb = sanitize(BBox {
                x0: c.x,
                y0: c.y,
                x1: c.x + c.width.max(0.0),
                y1: c.y + c.font_size.max(1.0),
            });
            text_area += intersect(&cb, &bx);
        }
        if text_area > VECTOR_TEXT_FRAC * a {
            continue;
        }
        if overlaps_any_table(&bx, tables) {
            continue;
        }
        out.push(bx);
    }
    out.sort_by(|a, b| (-top(a)).total_cmp(&-top(b)).then(a.x0.total_cmp(&b.x0)));
    out
}

fn overlaps_any_table(b: &BBox, tables: &[BBox]) -> bool {
    tables.iter().any(|t| {
        iou(b, t) >= TABLE_OVERLAP_IOU
            || contained_frac(b, t) >= TABLE_CONTAIN_FRAC
            || contained_frac(t, b) >= TABLE_CONTAIN_FRAC
    })
}

fn build_figures(
    graphics: &DrawnGraphics,
    chunks: &[TextChunk],
    tables: &[BBox],
    line_h: f64,
    page_area: f64,
) -> Vec<FigureRegion> {
    let image_rects: Vec<Rect> = graphics.images.iter().map(|p| p.bbox).collect();
    let mut figs: Vec<BBox> = merge_image_rects(&image_rects, line_h);
    // Drop image figures coincident with a detected table (e.g. scanned tables).
    figs.retain(|b| !overlaps_any_table(b, tables));

    let vecs = vector_regions(graphics, chunks, tables, line_h, page_area);
    for v in vecs {
        // De-dup against image figures: skip a vector region largely inside an
        // image figure (or containing one).
        let dup = figs.iter().any(|f| {
            contained_frac(&v, f) >= TABLE_CONTAIN_FRAC
                || contained_frac(f, &v) >= TABLE_CONTAIN_FRAC
        });
        if !dup {
            figs.push(v);
        }
    }
    figs.sort_by(|a, b| (-top(a)).total_cmp(&-top(b)).then(a.x0.total_cmp(&b.x0)));
    figs.into_iter().map(|bbox| FigureRegion { bbox }).collect()
}

// ── disjoint-set (union-find), path-compressed ──────────────────────────────
struct Dsu {
    parent: Vec<usize>,
}
impl Dsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let r = self.find(self.parent[x]);
            self.parent[x] = r;
        }
        self.parent[x]
    }
    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb.max(ra)] = rb.min(ra);
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// List grouping
// ════════════════════════════════════════════════════════════════════════════

/// A block recognised as a list of items (replaces the consumed layout block).
struct GroupedList {
    bbox: BBox,
    page: usize,
    ordered: bool,
    items: Vec<ListItem>,
}

/// Try to interpret a single layout block's lines as a list. Returns `None`
/// unless ≥2 lines start with a marker at a consistent left edge.
fn try_group_list(block: &LayoutBlock, page: usize, line_h: f64) -> Option<GroupedList> {
    if block.lines.len() < 2 {
        return None;
    }
    let tol = 0.5 * line_h;
    let mut items: Vec<ListItem> = Vec::new();
    let mut ordered_votes = 0i32;
    let mut bullet_votes = 0i32;
    let mut marker_left: Option<f64> = None;
    let mut started = false;

    for line in &block.lines {
        let is_bullet = is_bullet_marker(&line.text);
        let enm = enum_marker(&line.text);
        let is_item_start = is_bullet || enm.is_some();
        if is_item_start {
            // consistent hanging indent: marker left edges align
            match marker_left {
                Some(m) if (line.bbox.x0 - m).abs() > tol => {
                    // a marker at a different indent breaks consistency
                    return None;
                }
                _ => marker_left = Some(line.bbox.x0),
            }
            if is_bullet {
                bullet_votes += 1;
            } else {
                ordered_votes += 1;
            }
            let marker = first_token(&line.text);
            items.push(ListItem {
                text: line.text.trim().to_string(),
                bbox: bbox_to_array(&line.bbox),
                marker,
                ordered: enm.unwrap_or(false),
            });
            started = true;
        } else if started {
            // continuation line of the current item (hanging indent)
            if let Some(last) = items.last_mut() {
                last.text.push(' ');
                last.text.push_str(line.text.trim());
                last.bbox = bbox_to_array(&union_box(&array_to_bbox(last.bbox), &line.bbox));
            }
        } else {
            // leading non-marker line before any item: not a clean list
            return None;
        }
    }

    if items.len() < 2 {
        return None;
    }
    let ordered = ordered_votes >= bullet_votes;
    Some(GroupedList {
        bbox: block.bbox,
        page,
        ordered,
        items,
    })
}

fn first_token(line: &str) -> Option<String> {
    line.split_whitespace().next().map(str::to_string)
}

// ════════════════════════════════════════════════════════════════════════════
// build_document_model
// ════════════════════════════════════════════════════════════════════════════

pub fn build_document_model(engine: &ContentEngine, pages: &[usize]) -> Result<DocumentModel> {
    let total = engine.page_count()?;
    let page_list: Vec<usize> = if pages.is_empty() {
        (1..=total).collect()
    } else {
        pages.to_vec()
    };
    for &p in &page_list {
        if p == 0 || p > total {
            return Err(crate::error::OxideError::MalformedPdf(format!(
                "page {p} out of range (document has {total})"
            )));
        }
    }

    let semantic = engine.extract_semantic_document(&page_list)?;
    if semantic.tagged {
        Ok(build_tagged(&semantic, page_list.len()))
    } else {
        build_geometric(engine, &page_list)
    }
}

// ── geometric path ──────────────────────────────────────────────────────────

pub(crate) struct PageData {
    page: usize,
    page_width: f64,
    page_height: f64,
    blocks: Vec<LayoutBlock>,
    /// Column index per block (parallel to `blocks`), from segmentation.
    block_col: Vec<usize>,
    block_bold: Vec<bool>,
    block_italic: Vec<bool>,
    page_is_rtl: bool,
    /// Number of columns the segmenter found on this page (1 = single column).
    ncols: usize,
    figures: Vec<FigureRegion>,
    tables: Vec<Table>,
}

fn build_geometric(engine: &ContentEngine, page_list: &[usize]) -> Result<DocumentModel> {
    let mut pages_data: Vec<PageData> = Vec::new();

    for &page in page_list {
        let ops = engine.get_page_content(page)?;
        let resources = engine.get_page_resources(page)?;
        let mut collector = TextCollector::new(resources, engine.document().reader());
        let mut chunks = collector.collect(&ops);
        let image_names = page_image_names(engine, page)?;
        let mut graphics = collect_graphics_with_images(&ops, &image_names);
        let (raw_w, raw_h) = engine.page_dimensions(page)?;

        // ── rotation normalization ──
        // /Rotate 90/180/270 leaves the content stream's coordinates in the
        // *unrotated* user space, so on a landscape-rotated page the text and
        // rules are sideways and layout/reading-order would run along the wrong
        // axis. Map every chunk and graphic into upright reading orientation
        // here, and swap page width/height for the quarter turns, so everything
        // downstream (segmentation, tables, ordering) sees an upright page.
        let rotate = engine.page_rotation(page).unwrap_or(0);
        let crop = engine.page_crop_box(page).unwrap_or([0.0, 0.0, raw_w, raw_h]);
        let (page_width, page_height) = if rotate == 90 || rotate == 270 {
            (raw_h, raw_w)
        } else {
            (raw_w, raw_h)
        };
        if rotate != 0 {
            let rot = PageRotation::new(rotate, crop, page_width, page_height);
            for c in &mut chunks {
                rot.rotate_chunk(c);
            }
            rot.rotate_graphics(&mut graphics);
        }

        pages_data.push(page_data_from_chunks(
            page,
            &chunks,
            &graphics,
            page_width,
            page_height,
        ));
    }

    assemble_pages_data(pages_data, page_list.len())
}

/// Build one page's [`PageData`] from already-collected, already-upright
/// positioned text chunks plus its drawn graphics and page dimensions.
///
/// This is the **source-agnostic seam**: the digital-born path passes chunks
/// from the PDF content stream, the OCR path passes chunks synthesized from
/// recognized words (mapped into the same upright user space). Both then feed
/// the identical table/segment/figure machinery — there is no OCR-specific
/// layout code.
pub(crate) fn page_data_from_chunks(
    page: usize,
    chunks: &[TextChunk],
    graphics: &DrawnGraphics,
    page_width: f64,
    page_height: f64,
) -> PageData {
    let all_tables = detect_tables(chunks, graphics);
    let line_h = page_line_height(chunks);

    // docmodel-local fine segmentation (the analyzer's analyze_page is left intact;
    // it collapses tight columns into giant blocks, which the classifier and
    // precedence graph can't work with — see module docs).
    let (seg_blocks, page_is_rtl, ncols) = segment_page(chunks, line_h);

    // Step 0: drop blocks ~contained in a detected table (the table carries
    // that text as structured cells). Keep blocks and their column tags in
    // lockstep.
    let table_boxes: Vec<BBox> = all_tables.iter().map(|t| array_to_bbox(t.bbox)).collect();
    let mut kept_blocks: Vec<LayoutBlock> = Vec::new();
    let mut block_col: Vec<usize> = Vec::new();
    for sb in seg_blocks {
        let inside_table = table_boxes
            .iter()
            .any(|tb| contained_frac(&sb.block.bbox, tb) >= TABLE_CONTAIN_FRAC);
        if !inside_table {
            kept_blocks.push(sb.block);
            block_col.push(sb.column);
        }
    }

    // Re-associate chunks to kept blocks for bold/italic features.
    let (block_bold, block_italic) = block_font_flags(&kept_blocks, chunks);

    let page_area = (page_width * page_height).max(1.0);
    let figures = build_figures(graphics, chunks, &table_boxes, line_h, page_area);

    PageData {
        page,
        page_width,
        page_height,
        blocks: kept_blocks,
        block_col,
        block_bold,
        block_italic,
        page_is_rtl,
        ncols,
        figures,
        tables: all_tables,
    }
}

/// Order, classify, link, and detect running elements across already-built
/// per-page [`PageData`] — the page-source-agnostic tail of the geometric
/// pipeline, shared by the digital-born and OCR paths.
pub(crate) fn assemble_pages_data(
    pages_data: Vec<PageData>,
    page_count: usize,
) -> Result<DocumentModel> {
    let page_list_len = page_count;
    // Document-wide stats over all kept blocks.
    let all_blocks: Vec<(usize, &LayoutBlock)> = pages_data
        .iter()
        .flat_map(|pd| pd.blocks.iter().map(move |b| (pd.page, b)))
        .collect();
    let doc_stats = compute_doc_stats(&all_blocks);
    let line_h = doc_stats.median_line_height;

    // Assemble per-page ordered, typed blocks; concatenate in page order.
    let mut blocks: Vec<DocBlock> = Vec::new();
    let mut next_id = 0usize;
    let mut ro_index = 0usize;

    for pd in &pages_data {
        // A staged block: its geometry, region kind, the pre-assigned column
        // (None = full-width spanning), and the typed DocBlock.
        let mut staged: Vec<(BBox, RegionKind, Option<usize>, DocBlock)> = Vec::new();

        // Per-page column width (median block width within a column), to decide
        // which blocks are full-width spanning (title/caption across columns).
        let col_w = page_column_width(&pd.blocks, &pd.block_col, pd.ncols);
        let multi_col = pd.ncols >= 2;
        // Column band of a block: its segmentation column, unless it is wide
        // enough to span (then None → Spanning). On single-column pages every
        // text block is column 0 (nothing spans).
        let text_col = |bi: usize, b: &LayoutBlock| -> Option<usize> {
            if !multi_col {
                return Some(0);
            }
            if b.bbox.width() >= 1.5 * col_w {
                None
            } else {
                Some(pd.block_col.get(bi).copied().unwrap_or(0))
            }
        };
        // Column band of a figure/table from its x-centre against the page's
        // column centres (None when it spans).
        let region_col = |bx: &BBox| -> Option<usize> {
            region_column(bx, &pd.blocks, &pd.block_col, pd.ncols, col_w)
        };

        // Lists first (consume runs); then classify remaining blocks.
        // Sort blocks top-to-bottom within page so gap_above is meaningful.
        let mut idx_order: Vec<usize> = (0..pd.blocks.len()).collect();
        idx_order.sort_by(|&a, &b| {
            (-top(&pd.blocks[a].bbox))
                .total_cmp(&-top(&pd.blocks[b].bbox))
                .then(pd.blocks[a].bbox.x0.total_cmp(&pd.blocks[b].bbox.x0))
        });

        let mut prev_bottom: Option<f64> = None;
        for &bi in &idx_order {
            let block = &pd.blocks[bi];
            let gap_above = match prev_bottom {
                Some(pb) => (pb - top(&block.bbox)).max(0.0),
                None => f64::MAX,
            };
            prev_bottom = Some(bottom(&block.bbox));
            let column = text_col(bi, block);

            if let Some(list) = try_group_list(block, pd.page, line_h) {
                let id = next_id;
                next_id += 1;
                let text = list
                    .items
                    .iter()
                    .map(|it| it.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                staged.push((
                    list.bbox,
                    RegionKind::Text,
                    column,
                    DocBlock {
                        id,
                        classified: ClassifiedType::List {
                            ordered: list.ordered,
                        },
                        page: list.page,
                        bbox: bbox_to_array(&list.bbox),
                        reading_order_index: 0,
                        text,
                        confidence: 0.9,
                        basis: vec!["list:markers".into()],
                        items: list.items,
                        caption_id: None,
                        figure_id: None,
                        header_footer: false,
                        page_number: false,
                        is_bold: false,
                        is_italic: false,
                        table: None,
                    },
                ));
                continue;
            }

            let f = features_for(block, pd, &doc_stats, bi, gap_above);
            let (ctype, conf, basis) = classify_block(&f, &doc_stats);
            let id = next_id;
            next_id += 1;
            staged.push((
                block.bbox,
                RegionKind::Text,
                column,
                DocBlock {
                    id,
                    classified: ctype,
                    page: pd.page,
                    bbox: bbox_to_array(&block.bbox),
                    reading_order_index: 0,
                    text: block.text(),
                    confidence: conf,
                    basis,
                    items: Vec::new(),
                    caption_id: None,
                    figure_id: None,
                    header_footer: false,
                    page_number: false,
                    is_bold: pd.block_bold.get(bi).copied().unwrap_or(false),
                    is_italic: pd.block_italic.get(bi).copied().unwrap_or(false),
                    table: None,
                },
            ));
        }

        // Tables as blocks.
        for t in &pd.tables {
            let id = next_id;
            next_id += 1;
            let bx = array_to_bbox(t.bbox);
            let column = region_col(&bx);
            staged.push((
                bx,
                RegionKind::Table,
                column,
                DocBlock {
                    id,
                    classified: ClassifiedType::Table,
                    page: pd.page,
                    bbox: t.bbox,
                    reading_order_index: 0,
                    text: t.to_csv(),
                    confidence: match t.source {
                        TableSource::Borderless => t.confidence,
                        _ => t.confidence.max(0.9),
                    },
                    basis: vec![format!("table:{:?}", t.source).to_lowercase()],
                    items: Vec::new(),
                    caption_id: None,
                    figure_id: None,
                    header_footer: false,
                    page_number: false,
                    is_bold: false,
                    is_italic: false,
                    table: Some(t.clone()),
                },
            ));
        }

        // Figures as blocks.
        for fig in &pd.figures {
            let id = next_id;
            next_id += 1;
            let column = region_col(&fig.bbox);
            staged.push((
                fig.bbox,
                RegionKind::Figure,
                column,
                DocBlock {
                    id,
                    classified: ClassifiedType::Figure,
                    page: pd.page,
                    bbox: bbox_to_array(&fig.bbox),
                    reading_order_index: 0,
                    text: String::new(),
                    confidence: 0.75,
                    basis: vec!["figure:region".into()],
                    items: Vec::new(),
                    caption_id: None,
                    figure_id: None,
                    header_footer: false,
                    page_number: false,
                    is_bold: false,
                    is_italic: false,
                    table: None,
                },
            ));
        }

        // Order this page's staged blocks. On multi-column pages the precedence
        // ordering trusts the segmentation's column tags (pre_columned); on
        // single-column pages it falls back to band auto-detection (which also
        // correctly recovers spanning headers among single-column blocks).
        let node_staged_idx: Vec<usize> = staged
            .iter()
            .enumerate()
            .filter(|(_, (bx, _, _, _))| area(bx) > 0.0)
            .map(|(i, _)| i)
            .collect();
        let mut nodes: Vec<OrderNode> = node_staged_idx
            .iter()
            .map(|&i| {
                let (bx, kind, col, _) = &staged[i];
                if multi_col {
                    OrderNode::pre_columned(*bx, *kind, i, pd.page_is_rtl, *col)
                } else {
                    OrderNode::new(*bx, *kind, i, pd.page_is_rtl)
                }
            })
            .collect();
        if !multi_col {
            assign_columns(&mut nodes, line_h);
        }
        let order = reading_order(&nodes, pd.page_is_rtl, line_h);

        // Emit in order; zero-area staged blocks (rare) appended after, by index.
        let mut emitted = vec![false; staged.len()];
        for &node_pos in &order {
            let staged_idx = node_staged_idx[node_pos];
            let mut blk = staged[staged_idx].3.clone();
            blk.reading_order_index = ro_index;
            ro_index += 1;
            emitted[staged_idx] = true;
            blocks.push(blk);
        }
        for (i, was) in emitted.iter().enumerate() {
            if !was {
                let mut blk = staged[i].3.clone();
                blk.reading_order_index = ro_index;
                ro_index += 1;
                blocks.push(blk);
            }
        }
    }

    // Caption linkage (promotion in place; single ownership).
    link_captions(&mut blocks, line_h);

    // Cross-page header/footer/page-number pass.
    let page_dims: BTreeMap<usize, (f64, f64)> = pages_data
        .iter()
        .map(|pd| (pd.page, (pd.page_width, pd.page_height)))
        .collect();
    detect_running_elements(&mut blocks, &page_dims, page_list_len);

    Ok(DocumentModel {
        source: ModelSource::Geometric,
        page_count: page_list_len,
        body_font_size: doc_stats.body_size,
        blocks,
    })
}

/// Median width of single-column blocks — the page's column width. Falls back
/// to the median of all block widths when columns aren't tagged.
fn page_column_width(blocks: &[LayoutBlock], block_col: &[usize], ncols: usize) -> f64 {
    let mut widths: Vec<f64> = if ncols >= 2 {
        // Width of blocks that sit in a single column (not spanning), per the
        // segmentation tag — approximate by taking the narrower half.
        blocks.iter().map(|b| b.bbox.width()).collect()
    } else {
        blocks.iter().map(|b| b.bbox.width()).collect()
    };
    let _ = block_col;
    if widths.is_empty() {
        return 1.0;
    }
    widths.sort_by(|a, b| a.total_cmp(b));
    // Use the lower-median so a few full-width blocks don't inflate the column
    // width estimate.
    widths[widths.len() / 3].max(1.0)
}

/// Column band of a figure/table region from its x-centre against per-column
/// centroids derived from the tagged text blocks. `None` when the region is wide
/// enough to span columns.
fn region_column(
    bx: &BBox,
    blocks: &[LayoutBlock],
    block_col: &[usize],
    ncols: usize,
    col_w: f64,
) -> Option<usize> {
    if ncols < 2 {
        return Some(0);
    }
    if bx.width() >= 1.5 * col_w {
        return None;
    }
    // Mean x-centre per column from tagged blocks.
    let mut sum = vec![0.0f64; ncols];
    let mut cnt = vec![0.0f64; ncols];
    for (b, &c) in blocks.iter().zip(block_col.iter()) {
        if c < ncols {
            sum[c] += cx(&b.bbox);
            cnt[c] += 1.0;
        }
    }
    let cxr = cx(bx);
    let mut best = 0usize;
    let mut best_d = f64::INFINITY;
    for c in 0..ncols {
        if cnt[c] <= 0.0 {
            continue;
        }
        let d = (cxr - sum[c] / cnt[c]).abs();
        if d.total_cmp(&best_d) == Ordering::Less {
            best_d = d;
            best = c;
        }
    }
    Some(best)
}

/// Re-associate chunks to blocks (by centre-in-bbox) and compute bold/italic
/// flags per block from font-name char mass (`TextChunk` carries no weight bit,
/// so weight is inferred from the BaseFont name).
fn block_font_flags(blocks: &[LayoutBlock], chunks: &[TextChunk]) -> (Vec<bool>, Vec<bool>) {
    let mut bold_mass = vec![0.0f64; blocks.len()];
    let mut ital_mass = vec![0.0f64; blocks.len()];
    let mut total_mass = vec![0.0f64; blocks.len()];
    for c in chunks {
        if c.text.trim().is_empty() {
            continue;
        }
        let pcx = c.x + c.width.max(0.0) / 2.0;
        let pcy = c.y + c.font_size.max(1.0) / 2.0;
        // find block whose bbox contains the chunk centre (first match)
        let Some(bi) = blocks.iter().position(|b| {
            pcx >= b.bbox.x0 && pcx <= b.bbox.x1 && pcy >= b.bbox.y0 && pcy <= b.bbox.y1
        }) else {
            continue;
        };
        let m = c.text.chars().count() as f64;
        total_mass[bi] += m;
        let name = c.font_name.to_ascii_lowercase();
        if BOLD_TOKENS.iter().any(|t| name.contains(t)) {
            bold_mass[bi] += m;
        }
        if ITALIC_TOKENS.iter().any(|t| name.contains(t)) {
            ital_mass[bi] += m;
        }
    }
    let bold = (0..blocks.len())
        .map(|i| total_mass[i] > 0.0 && bold_mass[i] / total_mass[i] >= BOLD_CHAR_MASS)
        .collect();
    let italic = (0..blocks.len())
        .map(|i| total_mass[i] > 0.0 && ital_mass[i] / total_mass[i] >= ITALIC_CHAR_MASS)
        .collect();
    (bold, italic)
}

fn features_for(
    block: &LayoutBlock,
    pd: &PageData,
    doc: &DocStats,
    bi: usize,
    gap_above: f64,
) -> BlockFeatures {
    let first_line_text = block
        .lines
        .first()
        .map(|l| l.text.clone())
        .unwrap_or_default();
    let word_count: usize = block
        .lines
        .iter()
        .map(|l| l.text.split_whitespace().count())
        .sum();
    let fill_ratio = if doc.column_width > 0.0 {
        (block.bbox.width() / doc.column_width).min(1.0)
    } else {
        1.0
    };
    let last_text = block.lines.last().map(|l| l.text.clone()).unwrap_or_default();
    BlockFeatures {
        font_size: block.font_size,
        size_ratio: block.font_size / doc.body_size.max(1.0),
        is_bold: pd.block_bold.get(bi).copied().unwrap_or(false),
        is_italic: pd.block_italic.get(bi).copied().unwrap_or(false),
        line_count: block.lines.len(),
        word_count,
        first_line_text,
        fill_ratio,
        ends_with_sentence_punct: ends_sentence(&last_text),
        gap_above,
    }
}

// ── caption linkage ─────────────────────────────────────────────────────────

fn link_captions(blocks: &mut [DocBlock], line_h: f64) {
    // Indices of figure/table blocks (ordered by reading order already).
    let fig_indices: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            matches!(b.classified, ClassifiedType::Figure | ClassifiedType::Table)
        })
        .map(|(i, _)| i)
        .collect();
    let mut claimed = vec![false; blocks.len()];
    for &fi in &fig_indices {
        let fig_box = array_to_bbox(blocks[fi].bbox);
        if area(&fig_box) <= 0.0 {
            continue;
        }
        // Candidate caption blocks: short text blocks not already claimed/typed.
        let mut best: Option<(f64, usize)> = None; // (score, idx)
        for (ci, cand) in blocks.iter().enumerate() {
            if ci == fi || claimed[ci] {
                continue;
            }
            if !matches!(
                cand.classified,
                ClassifiedType::Paragraph
                    | ClassifiedType::Text
                    | ClassifiedType::Heading { .. }
            ) {
                continue;
            }
            if cand.page != blocks[fi].page {
                continue;
            }
            let cb = array_to_bbox(cand.bbox);
            if area(&cb) <= 0.0 {
                continue;
            }
            let line_count = cand.text.lines().count().max(1);
            if line_count > 3 {
                continue;
            }
            if cb.width() > 1.1 * fig_box.width().max(1.0) {
                continue;
            }
            if hoverlap_frac(&cb, &fig_box) < CAPTION_HOVERLAP {
                continue;
            }
            // below preferred, then above
            let gap_below = bottom(&fig_box) - top(&cb); // >0 if cand below figure
            let gap_above = bottom(&cb) - top(&fig_box); // >0 if cand above figure
            let mut score = if gap_below >= 0.0 && gap_below <= K_BELOW * line_h {
                2.0 - (gap_below / (K_BELOW * line_h))
            } else if gap_above >= 0.0 && gap_above <= K_ABOVE * line_h {
                1.0 - (gap_above / (K_ABOVE * line_h))
            } else {
                continue;
            };
            if is_caption_prefixed(&cand.text) {
                score += 1.0;
            }
            match best {
                Some((bs, _)) if score.total_cmp(&bs) != Ordering::Greater => {}
                _ => best = Some((score, ci)),
            }
        }
        if let Some((_, ci)) = best {
            claimed[ci] = true;
            let fig_id = blocks[fi].id;
            let cap_id = blocks[ci].id;
            blocks[ci].classified = ClassifiedType::Caption;
            blocks[ci].figure_id = Some(fig_id);
            blocks[ci].confidence = blocks[ci].confidence.max(0.7);
            if !blocks[ci].basis.iter().any(|s| s == "caption:adjacent") {
                blocks[ci].basis.push("caption:adjacent".into());
                blocks[ci].basis.sort();
            }
            blocks[fi].caption_id = Some(cap_id);
        }
    }
}

// ── cross-page running header/footer/page-number ────────────────────────────

fn normalize_running(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut prev_space = false;
    for ch in lower.chars() {
        if ch.is_ascii_digit() {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn detect_running_elements(
    blocks: &mut [DocBlock],
    page_dims: &BTreeMap<usize, (f64, f64)>,
    page_count: usize,
) {
    // Per-block band classification (skip unknown-geometry blocks).
    #[derive(Clone, Copy, PartialEq)]
    enum Band {
        Top,
        Bottom,
        Mid,
    }
    let band_of = |b: &DocBlock| -> Band {
        let bx = array_to_bbox(b.bbox);
        if area(&bx) <= 0.0 {
            return Band::Mid;
        }
        let Some(&(_, h)) = page_dims.get(&b.page) else {
            return Band::Mid;
        };
        if h <= 0.0 {
            return Band::Mid;
        }
        if top(&bx) >= h * (1.0 - MARGIN_BAND) {
            Band::Top
        } else if bottom(&bx) <= h * MARGIN_BAND {
            Band::Bottom
        } else {
            Band::Mid
        }
    };

    let x_bucket = |b: &DocBlock| -> i64 {
        let bx = array_to_bbox(b.bbox);
        let w = page_dims.get(&b.page).map(|d| d.0).unwrap_or(1.0).max(1.0);
        (bx.x0 / (w * 0.1)).floor() as i64
    };

    // ── running header/footer by repeated normalized text ──
    // key = (band, x_bucket, normalized_text) -> set of pages
    let mut groups: BTreeMap<(u8, i64, String), BTreeSet<usize>> = BTreeMap::new();
    let mut members: BTreeMap<(u8, i64, String), Vec<usize>> = BTreeMap::new();
    for (i, b) in blocks.iter().enumerate() {
        let band = band_of(b);
        if band == Band::Mid {
            continue;
        }
        let key_band = if band == Band::Top { 0u8 } else { 1u8 };
        let norm = normalize_running(&b.text);
        if norm.is_empty() {
            continue;
        }
        let key = (key_band, x_bucket(b), norm);
        groups.entry(key.clone()).or_default().insert(b.page);
        members.entry(key).or_default().push(i);
    }
    let need = ((page_count as f64 * 0.5).ceil() as usize).max(2);
    let mut running_idxs: BTreeSet<usize> = BTreeSet::new();
    for (key, pages) in &groups {
        if pages.len() >= need {
            let total = page_count;
            let conf = (0.5 + 0.4 * (pages.len() as f64 / total.max(1) as f64)).min(0.98);
            for &i in &members[key] {
                let is_top = key.0 == 0;
                blocks[i].classified = if is_top {
                    ClassifiedType::Header
                } else {
                    ClassifiedType::Footer
                };
                blocks[i].header_footer = true;
                blocks[i].confidence = conf;
                blocks[i].basis = vec![format!(
                    "running:{}of{};band:{}",
                    pages.len(),
                    total,
                    if is_top { "top" } else { "bottom" }
                )];
                running_idxs.insert(i);
            }
        }
    }

    // ── page numbers: short numeric/roman blocks in a band, clustered by
    //    (band, x_bucket), incrementing across pages ──
    // (band, x_bucket) -> [(block_idx, page, page-number value)]
    type PnEntry = (usize, usize, i64);
    let mut pn_groups: BTreeMap<(u8, i64), Vec<PnEntry>> = BTreeMap::new();
    for (i, b) in blocks.iter().enumerate() {
        let band = band_of(b);
        if band == Band::Mid {
            continue;
        }
        if let Some(v) = page_number_value(&b.text) {
            let key_band = if band == Band::Top { 0u8 } else { 1u8 };
            pn_groups
                .entry((key_band, x_bucket(b)))
                .or_default()
                .push((i, b.page, v));
        }
    }
    for (_key, mut members) in pn_groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|&(_, page, _)| page);
        // fraction of consecutive pairs with a constant +1 (or +k) increment
        let mut consistent = 0usize;
        let mut total_pairs = 0usize;
        for w in members.windows(2) {
            total_pairs += 1;
            if w[1].2 - w[0].2 == 1 && w[1].1 > w[0].1 {
                consistent += 1;
            }
        }
        let seq_ok = total_pairs > 0 && consistent * 2 >= total_pairs;
        let conf = if seq_ok { 0.95 } else { 0.6 };
        for (i, _page, _v) in members {
            // page numbers override header/footer
            blocks[i].classified = ClassifiedType::PageNumber;
            blocks[i].page_number = true;
            blocks[i].header_footer = false;
            blocks[i].confidence = conf;
            blocks[i].basis = vec![if seq_ok {
                "pagenum:sequence".into()
            } else {
                "pagenum:format".into()
            }];
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// tagged path
// ════════════════════════════════════════════════════════════════════════════

fn build_tagged(doc: &SemanticDocument, page_count: usize) -> DocumentModel {
    let mut blocks: Vec<DocBlock> = Vec::new();
    let mut next_id = 0usize;
    let mut order = 0usize;
    // table matching by walk order
    let mut table_iter = doc.tables.iter();

    // pre-order DFS; skip the synthetic Document root
    let roots: Vec<&SemanticElement> = doc
        .elements
        .iter()
        .flat_map(|e| {
            if e.element_type == "Document" {
                e.children.iter().collect::<Vec<_>>()
            } else {
                vec![e]
            }
        })
        .collect();

    // Track parent figure/table for caption sibling-linkage.
    fn walk<'a>(
        el: &'a SemanticElement,
        blocks: &mut Vec<DocBlock>,
        next_id: &mut usize,
        order: &mut usize,
        table_iter: &mut std::slice::Iter<'a, Table>,
        last_figure_id: &mut Option<usize>,
    ) {
        let mapped = map_tag(&el.element_type, el);
        if let Some((ctype, list_items)) = mapped {
            let id = *next_id;
            *next_id += 1;
            let bbox = el.bbox.unwrap_or([0.0; 4]);
            let text = match ctype {
                ClassifiedType::Figure => el
                    .actual_text
                    .clone()
                    .or_else(|| el.alt_text.clone())
                    .unwrap_or_else(|| el.text.clone()),
                _ => el.combined_text(),
            };
            let table = if matches!(ctype, ClassifiedType::Table) {
                table_iter.next().cloned()
            } else {
                None
            };
            let mut figure_id = None;
            if matches!(ctype, ClassifiedType::Caption) {
                if let Some(fid) = *last_figure_id {
                    figure_id = Some(fid);
                    // back-link the figure
                    if let Some(fb) = blocks.iter_mut().find(|b| b.id == fid) {
                        fb.caption_id = Some(id);
                    }
                }
            }
            let confidence = 0.95;
            blocks.push(DocBlock {
                id,
                classified: ctype,
                page: el.page.unwrap_or(0),
                bbox,
                reading_order_index: *order,
                text,
                confidence,
                basis: vec![format!("tagged:{}", el.element_type)],
                items: list_items,
                caption_id: None,
                figure_id,
                header_footer: false,
                page_number: false,
                is_bold: false,
                is_italic: false,
                table,
            });
            *order += 1;
            if matches!(ctype, ClassifiedType::Figure | ClassifiedType::Table) {
                *last_figure_id = Some(id);
            }
        }
        for child in &el.children {
            walk(child, blocks, next_id, order, table_iter, last_figure_id);
        }
    }

    let mut last_figure_id: Option<usize> = None;
    for r in roots {
        walk(
            r,
            &mut blocks,
            &mut next_id,
            &mut order,
            &mut table_iter,
            &mut last_figure_id,
        );
    }

    DocumentModel {
        source: ModelSource::Tagged,
        page_count,
        body_font_size: 0.0,
        blocks,
    }
}

/// Map a tag role to a typed block. Returns `None` for pure containers with no
/// own content. The second tuple element is the list items for `L` elements.
fn map_tag(tag: &str, el: &SemanticElement) -> Option<(ClassifiedType, Vec<ListItem>)> {
    let t = tag;
    if t == "Title" {
        return Some((ClassifiedType::Title, Vec::new()));
    }
    if t == "H" {
        return Some((ClassifiedType::Heading { level: 1 }, Vec::new()));
    }
    if let Some(rest) = t.strip_prefix('H') {
        if let Ok(n) = rest.parse::<u8>() {
            return Some((
                ClassifiedType::Heading {
                    level: n.clamp(1, MAX_LEVEL),
                },
                Vec::new(),
            ));
        }
    }
    match t {
        "P" | "Note" | "BlockQuote" | "Quote" => Some((ClassifiedType::Paragraph, Vec::new())),
        "L" => {
            let items = list_items_from_element(el);
            let ordered = items.iter().filter(|i| i.ordered).count() * 2 >= items.len().max(1);
            Some((ClassifiedType::List { ordered }, items))
        }
        "LI" => Some((ClassifiedType::ListItem, Vec::new())),
        "Figure" => Some((ClassifiedType::Figure, Vec::new())),
        "Caption" => Some((ClassifiedType::Caption, Vec::new())),
        "Table" => Some((ClassifiedType::Table, Vec::new())),
        "TOC" | "TOCI" | "Artifact" | "Lbl" | "LBody" | "TR" | "TH" | "TD" | "THead" | "TBody"
        | "TFoot" => None,
        // Span/unknown with text => paragraph; empty => skipped.
        _ => {
            if el.combined_text().trim().is_empty() {
                None
            } else {
                Some((ClassifiedType::Paragraph, Vec::new()))
            }
        }
    }
}

fn list_items_from_element(el: &SemanticElement) -> Vec<ListItem> {
    let mut items = Vec::new();
    for child in &el.children {
        if child.element_type == "LI" {
            // gather Lbl (marker) and LBody (text)
            let mut marker = None;
            let mut text = child.text.clone();
            for gc in &child.children {
                match gc.element_type.as_str() {
                    "Lbl" => marker = Some(gc.combined_text()),
                    "LBody" => {
                        let body = gc.combined_text();
                        if !body.trim().is_empty() {
                            text = body;
                        }
                    }
                    _ => {}
                }
            }
            if text.trim().is_empty() {
                text = child.combined_text();
            }
            let ordered = marker
                .as_deref()
                .map(|m| enum_marker(m).unwrap_or(false) || m.chars().any(|c| c.is_ascii_digit()))
                .unwrap_or(false);
            items.push(ListItem {
                text: text.trim().to_string(),
                bbox: child.bbox.unwrap_or([0.0; 4]),
                marker,
                ordered,
            });
        }
    }
    items
}

// ════════════════════════════════════════════════════════════════════════════
// Markdown rendering (human inspection)
// ════════════════════════════════════════════════════════════════════════════

/// Render a [`DocumentModel`] as readable markdown-ish text: headings as `#`,
/// lists as bullets/enumerators, figures as `![...]`, captions italicised,
/// tables as GitHub pipe grids. Deterministic (iterates blocks in order).
pub fn render_markdown(model: &DocumentModel) -> String {
    let mut out = String::new();
    for b in &model.blocks {
        match b.classified {
            ClassifiedType::Title => {
                out.push_str("# ");
                out.push_str(b.text.trim());
                out.push('\n');
            }
            ClassifiedType::Heading { level } => {
                let hashes = "#".repeat((level as usize + 1).clamp(2, 6));
                out.push_str(&hashes);
                out.push(' ');
                out.push_str(b.text.trim());
                out.push('\n');
            }
            ClassifiedType::Paragraph | ClassifiedType::Text => {
                out.push_str(b.text.trim());
                out.push('\n');
            }
            ClassifiedType::List { ordered } => {
                for (i, it) in b.items.iter().enumerate() {
                    if ordered {
                        out.push_str(&format!("{}. ", i + 1));
                    } else {
                        out.push_str("- ");
                    }
                    out.push_str(strip_list_marker(&it.text).trim());
                    out.push('\n');
                }
            }
            ClassifiedType::ListItem => {
                out.push_str("- ");
                out.push_str(strip_list_marker(&b.text).trim());
                out.push('\n');
            }
            ClassifiedType::Figure => {
                let alt = if b.text.trim().is_empty() {
                    "Figure"
                } else {
                    b.text.trim()
                };
                out.push_str(&format!("![{alt}]()\n"));
            }
            ClassifiedType::Caption => {
                out.push('*');
                out.push_str(b.text.trim());
                out.push_str("*\n");
            }
            ClassifiedType::Table => {
                if let Some(t) = &b.table {
                    out.push_str(&render_table_md(t));
                } else {
                    out.push_str(b.text.trim());
                    out.push('\n');
                }
            }
            ClassifiedType::Header | ClassifiedType::Footer => {
                out.push_str(&format!("<!-- {}: {} -->\n", running_label(b), b.text.trim()));
            }
            ClassifiedType::PageNumber => {
                out.push_str(&format!("<!-- page-number: {} -->\n", b.text.trim()));
            }
        }
        out.push('\n');
    }
    out
}

fn running_label(b: &DocBlock) -> &'static str {
    match b.classified {
        ClassifiedType::Header => "header",
        ClassifiedType::Footer => "footer",
        _ => "running",
    }
}

fn strip_list_marker(text: &str) -> String {
    // Drop a leading bullet/enumerator token for cleaner markdown.
    let t = text.trim_start();
    if is_bullet_marker(t) {
        return t.chars().skip(1).collect::<String>().trim_start().to_string();
    }
    if enum_marker(t).is_some() {
        if let Some(pos) = t.find(['.', ')', ']']) {
            return t[pos + 1..].trim_start().to_string();
        }
    }
    t.to_string()
}

fn render_table_md(t: &Table) -> String {
    let cols = t.num_cols();
    if cols == 0 || t.rows.is_empty() {
        let csv = t.to_csv();
        return format!("```csv\n{csv}```\n");
    }
    let mut out = String::new();
    for (r, row) in t.rows.iter().enumerate() {
        out.push('|');
        for c in 0..cols {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&cell.replace('|', "\\|").replace('\n', " "));
            out.push_str(" |");
        }
        out.push('\n');
        if r == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests;
