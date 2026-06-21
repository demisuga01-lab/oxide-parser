//! Table detection and extraction: rows/cells plus span-aware structure.
//!
//! This module keeps the original flat `rows` view for CSV/backwards
//! compatibility and adds a richer structural model:
//!
//! - origin cells with row/column coordinates, rowspan/colspan, bbox and text;
//! - deterministic header flags and a simple parent -> child header hierarchy;
//! - basic nested ruled tables inside a detected cell;
//! - HTML serialization that preserves spans and header cells.
//!
//! Detection still builds on the existing geometry paths:
//!
//! 1. Ruled tables use drawn horizontal/vertical rule segments. Boundary
//!    coordinates come from line coverage across the dominant table extent; a
//!    missing internal divider joins adjacent atomic grid slots into a spanning
//!    cell.
//! 2. Borderless tables use text alignment. Rows come from baselines, columns
//!    from left-edge clusters, and a text block whose bounds cross inferred
//!    gutters is treated as a colspan.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::graphics::{DrawnGraphics, Segment};
use crate::text::TextChunk;

const COORD_TOL: f64 = 2.0;
const LINE_COVERAGE_RATIO: f64 = 0.35;
const DIVIDER_COVERAGE_RATIO: f64 = 0.55;
const MAX_NESTED_DEPTH: usize = 1;

/// A detected table. `rows` is the flattened compatibility view; `cells`
/// carries the span/header/nesting structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    /// Flattened view as `rows[r][c]`. Spanning cells write their text only at
    /// the origin slot and leave covered slots blank.
    pub rows: Vec<Vec<String>>,
    /// Span-aware origin cells in reading order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<TableCell>,
    /// Parent header -> child header links for multi-level headers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_hierarchy: Vec<HeaderRelation>,
    /// How the table was detected.
    pub source: TableSource,
    /// Detection confidence in [0, 1]. Ruled/semantic tables are high
    /// confidence; borderless tables carry heuristic confidence.
    pub confidence: f64,
    /// The table's bounding box in user space [x0, y0, x1, y1].
    pub bbox: [f64; 4],
    /// Explicit limitations or fallback decisions for this table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub rowspan: usize,
    pub colspan: usize,
    pub text: String,
    pub bbox: [f64; 4],
    pub is_header: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_scope: Option<HeaderScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested_tables: Vec<Table>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeaderScope {
    Row,
    Column,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderRelation {
    pub parent: HeaderRef,
    pub children: Vec<HeaderRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderRef {
    pub row: usize,
    pub col: usize,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TableSource {
    /// Recovered directly from a tagged-PDF `/Table` structure tree.
    Semantic,
    /// Detected from drawn grid lines.
    Ruled,
    /// Inferred from text alignment (no lines).
    Borderless,
}

impl Table {
    pub fn num_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn num_cols(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Construct a tagged-PDF table from authored rows. TH cells become header
    /// cells; spans are not available unless producers expose attributes that
    /// the semantic parser preserves in a future pass.
    pub fn from_semantic_rows(rows: Vec<Vec<(String, bool)>>) -> Self {
        let n_rows = rows.len();
        let n_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut cells = Vec::new();
        for (r, row) in rows.into_iter().enumerate() {
            for (c, (text, is_header)) in row.into_iter().enumerate() {
                cells.push(TableCell {
                    row: r,
                    col: c,
                    rowspan: 1,
                    colspan: 1,
                    text,
                    bbox: [0.0; 4],
                    is_header,
                    header_scope: is_header.then_some(if r == 0 {
                        HeaderScope::Column
                    } else if c == 0 {
                        HeaderScope::Row
                    } else {
                        HeaderScope::Column
                    }),
                    nested_tables: Vec::new(),
                });
            }
        }
        finalize_table(
            TableSource::Semantic,
            1.0,
            [0.0; 4],
            n_rows,
            n_cols,
            cells,
            Vec::new(),
        )
    }

    /// Serialize to CSV (RFC 4180 quoting). Rows are padded to the column count.
    /// Spanning cells use the documented blank-fill convention for covered
    /// non-origin slots.
    pub fn to_csv(&self) -> String {
        let cols = self.num_cols();
        let mut out = String::new();
        for row in &self.rows {
            for c in 0..cols {
                if c > 0 {
                    out.push(',');
                }
                let cell = row.get(c).map(String::as_str).unwrap_or("");
                out.push_str(&csv_quote(cell));
            }
            out.push('\n');
        }
        out
    }

    /// Serialize as an HTML table fragment, preserving rowspan/colspan and
    /// header cells. Nested tables are emitted inside their containing cell.
    pub fn to_html(&self) -> String {
        let mut out = String::new();
        out.push_str("<table>\n");
        let header_rows = contiguous_header_rows(&self.cells);
        if header_rows > 0 {
            out.push_str("<thead>\n");
            for r in 0..header_rows {
                self.write_html_row(r, &mut out);
            }
            out.push_str("</thead>\n");
        }
        let start_body = header_rows.min(self.num_rows());
        if start_body < self.num_rows() {
            out.push_str("<tbody>\n");
            for r in start_body..self.num_rows() {
                self.write_html_row(r, &mut out);
            }
            out.push_str("</tbody>\n");
        }
        out.push_str("</table>\n");
        out
    }

    fn write_html_row(&self, row: usize, out: &mut String) {
        out.push_str("<tr>");
        let mut cells: Vec<&TableCell> = self.cells.iter().filter(|c| c.row == row).collect();
        cells.sort_by_key(|c| c.col);
        for cell in cells {
            let tag = if cell.is_header { "th" } else { "td" };
            out.push('<');
            out.push_str(tag);
            if cell.rowspan > 1 {
                out.push_str(&format!(" rowspan=\"{}\"", cell.rowspan));
            }
            if cell.colspan > 1 {
                out.push_str(&format!(" colspan=\"{}\"", cell.colspan));
            }
            if let Some(scope) = cell.header_scope {
                out.push_str(" scope=\"");
                out.push_str(match scope {
                    HeaderScope::Row => "row",
                    HeaderScope::Column => "col",
                    HeaderScope::Both => "colgroup",
                });
                out.push('"');
            }
            out.push('>');
            out.push_str(&html_escape(&cell.text));
            for nested in &cell.nested_tables {
                out.push('\n');
                out.push_str(&nested.to_html());
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        out.push_str("</tr>\n");
    }
}

/// Quote a CSV field per RFC 4180 when it contains a comma, quote, or newline.
fn csv_quote(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Detect all tables on a page from its text chunks and drawn graphics. Tries
/// ruled detection first (high confidence); if no ruled grid is found, falls
/// back to borderless inference.
pub fn detect_tables(chunks: &[TextChunk], graphics: &DrawnGraphics) -> Vec<Table> {
    let mut tables = detect_ruled(chunks, graphics);
    if tables.is_empty() {
        tables.extend(detect_borderless(chunks));
    }
    tables
}

/// Detect a ruled table from drawn grid lines. Returns at most one dominant
/// table; nested ruled tables inside cells are attached recursively for clear
/// cases.
pub fn detect_ruled(chunks: &[TextChunk], graphics: &DrawnGraphics) -> Vec<Table> {
    detect_ruled_with_depth(chunks, graphics, 0)
}

fn detect_ruled_with_depth(
    chunks: &[TextChunk],
    graphics: &DrawnGraphics,
    depth: usize,
) -> Vec<Table> {
    let h_lines: Vec<Segment> = graphics
        .segments
        .iter()
        .copied()
        .filter(Segment::is_horizontal)
        .collect();
    let v_lines: Vec<Segment> = graphics
        .segments
        .iter()
        .copied()
        .filter(Segment::is_vertical)
        .collect();

    if h_lines.len() < 2 || v_lines.len() < 2 {
        return Vec::new();
    }

    let Some(extent) = dominant_extent(&h_lines, &v_lines) else {
        return Vec::new();
    };
    let width = extent[2] - extent[0];
    let height = extent[3] - extent[1];
    if width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }

    let row_ys = covered_axis_coords(
        &h_lines,
        extent[0],
        extent[2],
        width * LINE_COVERAGE_RATIO,
        true,
    );
    let col_xs = covered_axis_coords(
        &v_lines,
        extent[1],
        extent[3],
        height * LINE_COVERAGE_RATIO,
        false,
    );

    if row_ys.len() < 2 || col_xs.len() < 2 {
        return Vec::new();
    }

    let n_rows = row_ys.len() - 1;
    let n_cols = col_xs.len() - 1;
    let mut dsu = Dsu::new(n_rows * n_cols);

    for row in 0..n_rows {
        for col in 0..n_cols {
            if col + 1 < n_cols
                && !vertical_divider_exists(&v_lines, &row_ys, &col_xs, row, col + 1)
            {
                dsu.union(slot(row, col, n_cols), slot(row, col + 1, n_cols));
            }
            if row + 1 < n_rows && !horizontal_divider_exists(&h_lines, &row_ys, &col_xs, row, col)
            {
                dsu.union(slot(row, col, n_cols), slot(row + 1, col, n_cols));
            }
        }
    }

    let mut components: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    for row in 0..n_rows {
        for col in 0..n_cols {
            let root = dsu.find(slot(row, col, n_cols));
            components.entry(root).or_default().push((row, col));
        }
    }

    let mut cells = Vec::new();
    for (_root, slots) in components {
        let min_row = slots.iter().map(|(r, _)| *r).min().unwrap();
        let max_row = slots.iter().map(|(r, _)| *r).max().unwrap();
        let min_col = slots.iter().map(|(_, c)| *c).min().unwrap();
        let max_col = slots.iter().map(|(_, c)| *c).max().unwrap();
        let row_span = max_row - min_row + 1;
        let col_span = max_col - min_col + 1;
        let rectangular = slots.len() == row_span * col_span;
        if !rectangular {
            for (row, col) in slots {
                cells.push(empty_ruled_cell(row, col, 1, 1, &row_ys, &col_xs));
            }
            continue;
        }
        cells.push(empty_ruled_cell(
            min_row, min_col, row_span, col_span, &row_ys, &col_xs,
        ));
    }

    assign_ruled_text(chunks, &mut cells, &row_ys, &col_xs, &mut dsu, n_cols);
    detect_geometric_headers(&mut cells, n_rows, n_cols);
    if depth < MAX_NESTED_DEPTH {
        attach_nested_tables(&mut cells, chunks, graphics, depth);
    }

    vec![finalize_table(
        TableSource::Ruled,
        1.0,
        [
            *col_xs.first().unwrap(),
            *row_ys.first().unwrap(),
            *col_xs.last().unwrap(),
            *row_ys.last().unwrap(),
        ],
        n_rows,
        n_cols,
        cells,
        Vec::new(),
    )]
}

fn empty_ruled_cell(
    row: usize,
    col: usize,
    rowspan: usize,
    colspan: usize,
    row_ys: &[f64],
    col_xs: &[f64],
) -> TableCell {
    let n_rows = row_ys.len() - 1;
    let bottom_band = n_rows - row - rowspan;
    let top_band = n_rows - row;
    TableCell {
        row,
        col,
        rowspan,
        colspan,
        text: String::new(),
        bbox: [
            col_xs[col],
            row_ys[bottom_band],
            col_xs[col + colspan],
            row_ys[top_band],
        ],
        is_header: false,
        header_scope: None,
        nested_tables: Vec::new(),
    }
}

fn assign_ruled_text(
    chunks: &[TextChunk],
    cells: &mut [TableCell],
    row_ys: &[f64],
    col_xs: &[f64],
    dsu: &mut Dsu,
    n_cols: usize,
) {
    let mut parts: Vec<Vec<TextPart>> = vec![Vec::new(); cells.len()];
    let mut origin_to_idx = HashMap::new();
    for (idx, cell) in cells.iter().enumerate() {
        for row in cell.row..cell.row + cell.rowspan {
            for col in cell.col..cell.col + cell.colspan {
                origin_to_idx.insert(slot(row, col, n_cols), idx);
            }
        }
    }

    for chunk in chunks {
        if chunk.is_vertical || chunk.text.trim().is_empty() {
            continue;
        }
        let cx = chunk.x + chunk.width / 2.0;
        let cy = chunk.y + chunk.font_size / 2.0;
        let Some(col) = band_index(col_xs, cx) else {
            continue;
        };
        let Some(band) = band_index(row_ys, cy) else {
            continue;
        };
        let row = row_ys.len() - 2 - band;
        let root = dsu.find(slot(row, col, n_cols));
        let Some(&idx) = origin_to_idx
            .get(&root)
            .or_else(|| origin_to_idx.get(&slot(row, col, n_cols)))
        else {
            continue;
        };
        parts[idx].push(TextPart {
            x: chunk.x,
            y: chunk.y,
            text: chunk.text.clone(),
        });
    }

    for (cell, mut parts) in cells.iter_mut().zip(parts) {
        cell.text = join_positioned_parts(&mut parts);
    }
}

fn vertical_divider_exists(
    lines: &[Segment],
    row_ys: &[f64],
    col_xs: &[f64],
    row: usize,
    boundary_col: usize,
) -> bool {
    let band = row_ys.len() - 2 - row;
    let y0 = row_ys[band];
    let y1 = row_ys[band + 1];
    let x = col_xs[boundary_col];
    axis_line_covers(lines, x, y0, y1, false)
}

fn horizontal_divider_exists(
    lines: &[Segment],
    row_ys: &[f64],
    col_xs: &[f64],
    row: usize,
    col: usize,
) -> bool {
    let boundary_band = row_ys.len() - 2 - row;
    let y = row_ys[boundary_band];
    let x0 = col_xs[col];
    let x1 = col_xs[col + 1];
    axis_line_covers(lines, y, x0, x1, true)
}

fn axis_line_covers(lines: &[Segment], coord: f64, start: f64, end: f64, horizontal: bool) -> bool {
    let mut intervals = Vec::new();
    for line in lines {
        let line_coord = if horizontal { line.y0 } else { line.x0 };
        if (line_coord - coord).abs() > COORD_TOL {
            continue;
        }
        let (a, b) = if horizontal {
            (line.x0.min(line.x1), line.x0.max(line.x1))
        } else {
            (line.y0.min(line.y1), line.y0.max(line.y1))
        };
        let clipped_start = a.max(start);
        let clipped_end = b.min(end);
        if clipped_end > clipped_start {
            intervals.push((clipped_start, clipped_end));
        }
    }
    covered_length(intervals) >= (end - start).abs() * DIVIDER_COVERAGE_RATIO
}

fn dominant_extent(h_lines: &[Segment], v_lines: &[Segment]) -> Option<[f64; 4]> {
    let x0 = h_lines
        .iter()
        .flat_map(|s| [s.x0, s.x1])
        .chain(v_lines.iter().flat_map(|s| [s.x0, s.x1]))
        .fold(f64::INFINITY, f64::min);
    let x1 = h_lines
        .iter()
        .flat_map(|s| [s.x0, s.x1])
        .chain(v_lines.iter().flat_map(|s| [s.x0, s.x1]))
        .fold(f64::NEG_INFINITY, f64::max);
    let y0 = h_lines
        .iter()
        .flat_map(|s| [s.y0, s.y1])
        .chain(v_lines.iter().flat_map(|s| [s.y0, s.y1]))
        .fold(f64::INFINITY, f64::min);
    let y1 = h_lines
        .iter()
        .flat_map(|s| [s.y0, s.y1])
        .chain(v_lines.iter().flat_map(|s| [s.y0, s.y1]))
        .fold(f64::NEG_INFINITY, f64::max);
    if x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite() {
        Some([x0, y0, x1, y1])
    } else {
        None
    }
}

fn covered_axis_coords(
    lines: &[Segment],
    clip_start: f64,
    clip_end: f64,
    min_coverage: f64,
    horizontal: bool,
) -> Vec<f64> {
    let mut clusters: Vec<LineCluster> = Vec::new();
    let mut sorted = lines.to_vec();
    sorted.sort_by(|a, b| {
        let ac = if horizontal { a.y0 } else { a.x0 };
        let bc = if horizontal { b.y0 } else { b.x0 };
        ac.partial_cmp(&bc).unwrap_or(std::cmp::Ordering::Equal)
    });

    for line in sorted {
        let coord = if horizontal { line.y0 } else { line.x0 };
        let interval = if horizontal {
            (
                line.x0.min(line.x1).max(clip_start),
                line.x0.max(line.x1).min(clip_end),
            )
        } else {
            (
                line.y0.min(line.y1).max(clip_start),
                line.y0.max(line.y1).min(clip_end),
            )
        };
        if interval.1 <= interval.0 {
            continue;
        }
        match clusters.last_mut() {
            Some(cluster) if (coord - cluster.coord).abs() <= COORD_TOL => {
                let n = cluster.count as f64;
                cluster.coord = (cluster.coord * n + coord) / (n + 1.0);
                cluster.count += 1;
                cluster.intervals.push(interval);
            }
            _ => clusters.push(LineCluster {
                coord,
                count: 1,
                intervals: vec![interval],
            }),
        }
    }

    let mut out: Vec<f64> = clusters
        .into_iter()
        .filter(|cluster| covered_length(cluster.intervals.clone()) >= min_coverage)
        .map(|cluster| cluster.coord)
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[derive(Clone)]
struct LineCluster {
    coord: f64,
    count: usize,
    intervals: Vec<(f64, f64)>,
}

fn covered_length(mut intervals: Vec<(f64, f64)>) -> f64 {
    if intervals.is_empty() {
        return 0.0;
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut total = 0.0;
    let mut current = intervals[0];
    for interval in intervals.into_iter().skip(1) {
        if interval.0 <= current.1 {
            current.1 = current.1.max(interval.1);
        } else {
            total += current.1 - current.0;
            current = interval;
        }
    }
    total + current.1 - current.0
}

fn attach_nested_tables(
    cells: &mut [TableCell],
    chunks: &[TextChunk],
    graphics: &DrawnGraphics,
    depth: usize,
) {
    for cell in cells {
        let inner_segments: Vec<Segment> = graphics
            .segments
            .iter()
            .copied()
            .filter(|s| segment_inside_cell(*s, cell.bbox, 3.0))
            .collect();
        let h = inner_segments.iter().filter(|s| s.is_horizontal()).count();
        let v = inner_segments.iter().filter(|s| s.is_vertical()).count();
        if h < 2 || v < 2 {
            continue;
        }
        let sub_chunks: Vec<TextChunk> = chunks
            .iter()
            .filter(|chunk| point_inside_bbox(chunk.x, chunk.y, cell.bbox, 1.0))
            .cloned()
            .collect();
        let sub_graphics = DrawnGraphics {
            segments: inner_segments,
            rects: Vec::new(),
            ..DrawnGraphics::default()
        };
        let nested = detect_ruled_with_depth(&sub_chunks, &sub_graphics, depth + 1);
        if !nested.is_empty() {
            cell.nested_tables = nested;
        }
    }
}

fn segment_inside_cell(seg: Segment, bbox: [f64; 4], margin: f64) -> bool {
    let x0 = bbox[0] + margin;
    let y0 = bbox[1] + margin;
    let x1 = bbox[2] - margin;
    let y1 = bbox[3] - margin;
    [seg.x0, seg.x1].iter().all(|x| *x >= x0 && *x <= x1)
        && [seg.y0, seg.y1].iter().all(|y| *y >= y0 && *y <= y1)
}

fn point_inside_bbox(x: f64, y: f64, bbox: [f64; 4], margin: f64) -> bool {
    x >= bbox[0] + margin && x <= bbox[2] - margin && y >= bbox[1] + margin && y <= bbox[3] - margin
}

/// Infer a borderless table from text alignment alone. Returns at most one
/// table with a confidence reflecting how grid-like the text is.
pub fn detect_borderless(chunks: &[TextChunk]) -> Vec<Table> {
    let items: Vec<TextItem> = chunks
        .iter()
        .filter(|c| !c.is_vertical && !c.text.trim().is_empty())
        .map(|c| TextItem {
            x0: c.x,
            x1: c.x + c.width.max(0.0),
            yc: c.y + c.font_size.max(1.0) / 2.0,
            fs: c.font_size.max(1.0),
            text: c.text.clone(),
        })
        .collect();
    if items.len() < 4 {
        return Vec::new();
    }

    let line_h = median(items.iter().map(|i| i.fs)).max(1.0);
    let mut rows = group_rows(items, line_h * 0.6);
    if rows.len() < 2 {
        return Vec::new();
    }
    rows.sort_by(|a, b| {
        row_yc(b)
            .partial_cmp(&row_yc(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let col_bounds = infer_columns(&rows, line_h);
    let n_cols = col_bounds.len().saturating_sub(1);
    if n_cols < 2 {
        return Vec::new();
    }

    let n_rows = rows.len();
    let mut cells_by_origin: BTreeMap<(usize, usize), TableCell> = BTreeMap::new();
    let mut multi_col_rows = 0usize;

    for (row_idx, row) in rows.iter().enumerate() {
        let mut filled_cols = HashSet::new();
        for item in row {
            let Some(start_col) = band_index(&col_bounds, item.x0 + 0.1) else {
                continue;
            };
            let mut end_col = band_index(&col_bounds, item.x1 - 0.1).unwrap_or(start_col);
            if end_col > start_col && item.x1 < col_bounds[start_col + 1] + line_h * 0.4 {
                end_col = start_col;
            }
            let colspan = end_col.saturating_sub(start_col) + 1;
            for col in start_col..=end_col {
                filled_cols.insert(col);
            }
            let entry = cells_by_origin
                .entry((row_idx, start_col))
                .or_insert(TableCell {
                    row: row_idx,
                    col: start_col,
                    rowspan: 1,
                    colspan,
                    text: String::new(),
                    bbox: [
                        col_bounds[start_col],
                        item.yc - line_h / 2.0,
                        col_bounds[end_col + 1],
                        item.yc + line_h / 2.0,
                    ],
                    is_header: false,
                    header_scope: None,
                    nested_tables: Vec::new(),
                });
            entry.colspan = entry.colspan.max(colspan);
            if !entry.text.is_empty() {
                entry.text.push(' ');
            }
            entry.text.push_str(item.text.trim());
        }
        if filled_cols.len() >= 2 {
            multi_col_rows += 1;
        }
    }

    let confidence = multi_col_rows as f64 / n_rows as f64;
    if confidence < 0.5 {
        return Vec::new();
    }

    let mut cells: Vec<TableCell> = cells_by_origin.into_values().collect();
    fill_uncovered_cells(&mut cells, n_rows, n_cols, &col_bounds, &rows, line_h);
    detect_geometric_headers(&mut cells, n_rows, n_cols);

    let x0 = rows
        .iter()
        .flatten()
        .map(|c| c.x0)
        .fold(f64::INFINITY, f64::min);
    let x1 = rows
        .iter()
        .flatten()
        .map(|c| c.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    let y_top = rows
        .iter()
        .flatten()
        .map(|c| c.yc)
        .fold(f64::NEG_INFINITY, f64::max);
    let y_bot = rows
        .iter()
        .flatten()
        .map(|c| c.yc)
        .fold(f64::INFINITY, f64::min);

    vec![finalize_table(
        TableSource::Borderless,
        confidence,
        [x0, y_bot, x1, y_top],
        n_rows,
        n_cols,
        cells,
        vec!["borderless rowspans are not inferred; clear colspans only".to_string()],
    )]
}

#[derive(Clone)]
struct TextItem {
    x0: f64,
    x1: f64,
    yc: f64,
    fs: f64,
    text: String,
}

fn row_yc(row: &[TextItem]) -> f64 {
    if row.is_empty() {
        0.0
    } else {
        row.iter().map(|c| c.yc).sum::<f64>() / row.len() as f64
    }
}

fn group_rows(mut items: Vec<TextItem>, tol: f64) -> Vec<Vec<TextItem>> {
    items.sort_by(|a, b| b.yc.partial_cmp(&a.yc).unwrap_or(std::cmp::Ordering::Equal));
    let mut rows: Vec<Vec<TextItem>> = Vec::new();
    for item in items {
        let placed = rows
            .last()
            .map(|r| (row_yc(r) - item.yc).abs() <= tol)
            .unwrap_or(false);
        if placed {
            rows.last_mut().unwrap().push(item);
        } else {
            rows.push(vec![item]);
        }
    }
    rows
}

fn infer_columns(rows: &[Vec<TextItem>], line_h: f64) -> Vec<f64> {
    let mut left_edges: Vec<f64> = rows.iter().flatten().map(|c| c.x0).collect();
    if left_edges.len() < 2 {
        return Vec::new();
    }
    let centers = cluster_coords(std::mem::take(&mut left_edges), line_h * 1.3);
    if centers.len() < 2 {
        return Vec::new();
    }
    let min_x = rows
        .iter()
        .flatten()
        .map(|c| c.x0)
        .fold(f64::INFINITY, f64::min);
    let max_x = rows
        .iter()
        .flatten()
        .map(|c| c.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut bounds = Vec::with_capacity(centers.len() + 1);
    bounds.push(min_x);
    for pair in centers.windows(2) {
        bounds.push((pair[0] + pair[1]) / 2.0);
    }
    bounds.push(max_x);
    bounds
}

fn fill_uncovered_cells(
    cells: &mut Vec<TableCell>,
    n_rows: usize,
    n_cols: usize,
    col_bounds: &[f64],
    rows: &[Vec<TextItem>],
    line_h: f64,
) {
    let mut covered = vec![vec![false; n_cols]; n_rows];
    for cell in cells.iter() {
        for row in covered
            .iter_mut()
            .take((cell.row + cell.rowspan).min(n_rows))
            .skip(cell.row)
        {
            for covered_cell in row
                .iter_mut()
                .take((cell.col + cell.colspan).min(n_cols))
                .skip(cell.col)
            {
                *covered_cell = true;
            }
        }
    }
    for row in 0..n_rows {
        let yc = row_yc(&rows[row]);
        for col in 0..n_cols {
            if !covered[row][col] {
                cells.push(TableCell {
                    row,
                    col,
                    rowspan: 1,
                    colspan: 1,
                    text: String::new(),
                    bbox: [
                        col_bounds[col],
                        yc - line_h / 2.0,
                        col_bounds[col + 1],
                        yc + line_h / 2.0,
                    ],
                    is_header: false,
                    header_scope: None,
                    nested_tables: Vec::new(),
                });
            }
        }
    }
}

fn detect_geometric_headers(cells: &mut [TableCell], n_rows: usize, n_cols: usize) {
    if n_rows == 0 || n_cols == 0 {
        return;
    }

    for cell in cells.iter_mut().filter(|cell| cell.row == 0) {
        cell.is_header = true;
        cell.header_scope = Some(if cell.colspan > 1 {
            HeaderScope::Both
        } else {
            HeaderScope::Column
        });
    }

    let top_spans: Vec<(usize, usize)> = cells
        .iter()
        .filter(|cell| cell.row == 0 && cell.colspan > 1)
        .map(|cell| (cell.col, cell.col + cell.colspan))
        .collect();
    if n_rows > 1 && !top_spans.is_empty() {
        for cell in cells.iter_mut().filter(|cell| cell.row == 1) {
            if top_spans
                .iter()
                .any(|(start, end)| cell.col >= *start && cell.col < *end)
            {
                cell.is_header = true;
                cell.header_scope = Some(HeaderScope::Column);
            }
        }
    }

    if looks_like_row_header_column(cells, n_rows, n_cols) {
        for cell in cells
            .iter_mut()
            .filter(|cell| cell.col == 0 && cell.row > 0 && cell.colspan == 1)
        {
            cell.is_header = true;
            cell.header_scope = Some(HeaderScope::Row);
        }
    }
}

fn looks_like_row_header_column(cells: &[TableCell], n_rows: usize, n_cols: usize) -> bool {
    if n_rows < 3 || n_cols < 2 {
        return false;
    }
    let mut body_rows = 0usize;
    let mut row_header_like = 0usize;
    for row in 1..n_rows {
        let first = cell_at(cells, row, 0)
            .map(|cell| cell.text.trim())
            .unwrap_or("");
        if first.is_empty() || is_numeric_like(first) {
            continue;
        }
        body_rows += 1;
        let numeric_others = (1..n_cols)
            .filter_map(|col| cell_at(cells, row, col))
            .filter(|cell| is_numeric_like(cell.text.trim()))
            .count();
        if numeric_others * 2 >= n_cols.saturating_sub(1).max(1) {
            row_header_like += 1;
        }
    }
    body_rows >= 2 && row_header_like * 2 >= body_rows
}

fn is_numeric_like(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|ch| {
            ch.is_ascii_digit() || matches!(ch, '.' | ',' | '-' | '+' | '$' | '%' | '(' | ')' | '/')
        })
        && trimmed.chars().any(|ch| ch.is_ascii_digit())
}

fn finalize_table(
    source: TableSource,
    confidence: f64,
    bbox: [f64; 4],
    n_rows: usize,
    n_cols: usize,
    mut cells: Vec<TableCell>,
    notes: Vec<String>,
) -> Table {
    cells.sort_by_key(|cell| (cell.row, cell.col));
    let rows = flatten_rows(n_rows, n_cols, &cells);
    let header_hierarchy = build_header_hierarchy(&cells);
    Table {
        rows,
        cells,
        header_hierarchy,
        source,
        confidence,
        bbox,
        notes,
    }
}

fn flatten_rows(n_rows: usize, n_cols: usize, cells: &[TableCell]) -> Vec<Vec<String>> {
    let mut rows = vec![vec![String::new(); n_cols]; n_rows];
    for cell in cells {
        if cell.row < n_rows && cell.col < n_cols {
            rows[cell.row][cell.col] = cell.text.clone();
        }
    }
    rows
}

fn build_header_hierarchy(cells: &[TableCell]) -> Vec<HeaderRelation> {
    let mut out = Vec::new();
    for parent in cells
        .iter()
        .filter(|cell| cell.is_header && cell.colspan > 1)
    {
        let child_row = parent.row + parent.rowspan;
        let mut children: Vec<HeaderRef> = cells
            .iter()
            .filter(|child| {
                child.is_header
                    && child.row == child_row
                    && child.col >= parent.col
                    && child.col < parent.col + parent.colspan
            })
            .map(header_ref)
            .collect();
        children.sort_by_key(|child| child.col);
        if !children.is_empty() {
            out.push(HeaderRelation {
                parent: header_ref(parent),
                children,
            });
        }
    }
    out
}

fn header_ref(cell: &TableCell) -> HeaderRef {
    HeaderRef {
        row: cell.row,
        col: cell.col,
        text: cell.text.clone(),
    }
}

fn contiguous_header_rows(cells: &[TableCell]) -> usize {
    let mut row = 0usize;
    loop {
        let row_cells: Vec<&TableCell> = cells.iter().filter(|cell| cell.row == row).collect();
        if row_cells.is_empty() || !row_cells.iter().any(|cell| cell.is_header) {
            return row;
        }
        row += 1;
    }
}

fn cell_at(cells: &[TableCell], row: usize, col: usize) -> Option<&TableCell> {
    cells.iter().find(|cell| {
        row >= cell.row
            && row < cell.row + cell.rowspan
            && col >= cell.col
            && col < cell.col + cell.colspan
    })
}

/// Cluster nearly-equal coordinates (within `tol`) into representative values.
fn cluster_coords(mut values: Vec<f64>, tol: f64) -> Vec<f64> {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<f64> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some(last) if (v - *last).abs() <= tol => {
                *last = (*last + v) / 2.0;
            }
            _ => out.push(v),
        }
    }
    out
}

fn band_index(bounds: &[f64], v: f64) -> Option<usize> {
    if bounds.len() < 2 || v < bounds[0] || v > *bounds.last().unwrap() {
        return None;
    }
    (0..bounds.len() - 1).find(|&i| v >= bounds[i] && v <= bounds[i + 1])
}

fn join_cell(parts: impl Iterator<Item = String>) -> String {
    parts
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone)]
struct TextPart {
    x: f64,
    y: f64,
    text: String,
}

fn join_positioned_parts(parts: &mut [TextPart]) -> String {
    parts.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    join_cell(parts.iter().map(|part| part.text.clone()))
}

fn median(iter: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = iter.collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn slot(row: usize, col: usize, n_cols: usize) -> usize {
    row * n_cols + col
}

#[derive(Clone)]
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
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::graphics::{DrawnGraphics, Rect, Segment};

    fn seg(x0: f64, y0: f64, x1: f64, y1: f64) -> Segment {
        Segment { x0, y0, x1, y1 }
    }

    fn chunk(text: &str, x: f64, y: f64, w: f64) -> TextChunk {
        TextChunk {
            text: text.into(),
            x,
            y,
            font_size: 10.0,
            font_name: "F".into(),
            width: w,
            is_rtl: false,
            is_vertical: false,
            is_invisible: false,
        }
    }

    fn ruled_2x2() -> (Vec<TextChunk>, DrawnGraphics) {
        let mut g = DrawnGraphics::default();
        for y in [600.0, 620.0, 640.0] {
            g.segments.push(seg(50.0, y, 250.0, y));
        }
        for x in [50.0, 150.0, 250.0] {
            g.segments.push(seg(x, 600.0, x, 640.0));
        }
        g.rects.push(Rect {
            x0: 50.0,
            y0: 600.0,
            x1: 250.0,
            y1: 640.0,
        });
        let chunks = vec![
            chunk("A", 60.0, 625.0, 20.0),
            chunk("B", 160.0, 625.0, 20.0),
            chunk("C", 60.0, 605.0, 20.0),
            chunk("D", 160.0, 605.0, 20.0),
        ];
        (chunks, g)
    }

    #[test]
    fn ruled_grid_extracts_correct_cells() {
        let (chunks, g) = ruled_2x2();
        let tables = detect_ruled(&chunks, &g);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.num_rows(), 2);
        assert_eq!(t.num_cols(), 2);
        assert_eq!(t.source, TableSource::Ruled);
        assert_eq!(t.rows[0], vec!["A".to_string(), "B".to_string()]);
        assert_eq!(t.rows[1], vec!["C".to_string(), "D".to_string()]);
        assert_eq!(t.cells.len(), 4);
        assert!(t.cells.iter().filter(|cell| cell.is_header).count() >= 2);
    }

    #[test]
    fn ruled_table_to_csv_is_correct() {
        let (chunks, g) = ruled_2x2();
        let t = &detect_ruled(&chunks, &g)[0];
        assert_eq!(t.to_csv(), "A,B\nC,D\n");
    }

    #[test]
    fn csv_quotes_fields_with_commas() {
        let t = finalize_table(
            TableSource::Ruled,
            1.0,
            [0.0; 4],
            2,
            2,
            vec![
                TableCell {
                    row: 0,
                    col: 0,
                    rowspan: 1,
                    colspan: 1,
                    text: "a,b".into(),
                    bbox: [0.0; 4],
                    is_header: false,
                    header_scope: None,
                    nested_tables: Vec::new(),
                },
                TableCell {
                    row: 0,
                    col: 1,
                    rowspan: 1,
                    colspan: 1,
                    text: "c".into(),
                    bbox: [0.0; 4],
                    is_header: false,
                    header_scope: None,
                    nested_tables: Vec::new(),
                },
                TableCell {
                    row: 1,
                    col: 0,
                    rowspan: 1,
                    colspan: 1,
                    text: "d\"e".into(),
                    bbox: [0.0; 4],
                    is_header: false,
                    header_scope: None,
                    nested_tables: Vec::new(),
                },
                TableCell {
                    row: 1,
                    col: 1,
                    rowspan: 1,
                    colspan: 1,
                    text: "f".into(),
                    bbox: [0.0; 4],
                    is_header: false,
                    header_scope: None,
                    nested_tables: Vec::new(),
                },
            ],
            Vec::new(),
        );
        assert_eq!(t.to_csv(), "\"a,b\",c\n\"d\"\"e\",f\n");
    }

    #[test]
    fn no_grid_lines_yields_no_ruled_table() {
        let g = DrawnGraphics::default();
        let chunks = vec![chunk("X", 10.0, 10.0, 10.0)];
        assert!(detect_ruled(&chunks, &g).is_empty());
    }

    #[test]
    fn ruled_missing_internal_vertical_rule_creates_colspan() {
        let mut g = DrawnGraphics::default();
        for y in [600.0, 625.0, 650.0] {
            g.segments.push(seg(50.0, y, 350.0, y));
        }
        for x in [50.0, 150.0, 250.0, 350.0] {
            g.segments.push(seg(x, 600.0, x, 650.0));
        }
        // Remove the vertical divider x=150 only across the top row by replacing
        // it with a lower segment.
        g.segments
            .retain(|s| !(s.is_vertical() && (s.x0 - 150.0).abs() < 0.1));
        g.segments.push(seg(150.0, 600.0, 150.0, 625.0));
        let chunks = vec![
            chunk("Merged", 60.0, 633.0, 95.0),
            chunk("H3", 260.0, 633.0, 20.0),
            chunk("A", 60.0, 608.0, 20.0),
            chunk("B", 160.0, 608.0, 20.0),
            chunk("C", 260.0, 608.0, 20.0),
        ];
        let table = detect_ruled(&chunks, &g).remove(0);
        let merged = table
            .cells
            .iter()
            .find(|cell| cell.text == "Merged")
            .unwrap();
        assert_eq!(
            (merged.row, merged.col, merged.rowspan, merged.colspan),
            (0, 0, 1, 2)
        );
        assert_eq!(table.rows[0], vec!["Merged", "", "H3"]);
        assert!(table.to_html().contains("<th colspan=\"2\""));
    }

    #[test]
    fn ruled_missing_internal_horizontal_rule_creates_rowspan() {
        let mut g = DrawnGraphics::default();
        for y in [600.0, 625.0, 650.0] {
            g.segments.push(seg(50.0, y, 350.0, y));
        }
        for x in [50.0, 150.0, 250.0, 350.0] {
            g.segments.push(seg(x, 600.0, x, 650.0));
        }
        // Replace the y=625 divider with partial segments, leaving column 0 open.
        g.segments
            .retain(|s| !(s.is_horizontal() && (s.y0 - 625.0).abs() < 0.1));
        g.segments.push(seg(150.0, 625.0, 350.0, 625.0));
        let chunks = vec![
            chunk("Span", 60.0, 633.0, 30.0),
            chunk("B", 160.0, 633.0, 20.0),
            chunk("C", 260.0, 633.0, 20.0),
            chunk("E", 160.0, 608.0, 20.0),
            chunk("F", 260.0, 608.0, 20.0),
        ];
        let table = detect_ruled(&chunks, &g).remove(0);
        let span = table.cells.iter().find(|cell| cell.text == "Span").unwrap();
        assert_eq!(
            (span.row, span.col, span.rowspan, span.colspan),
            (0, 0, 2, 1)
        );
        assert!(table.to_html().contains("rowspan=\"2\""));
    }

    #[test]
    fn borderless_table_recovered_from_alignment() {
        let cols = [50.0, 200.0, 350.0];
        let rows_y = [700.0, 680.0, 660.0];
        let labels = [
            ["Name", "Age", "City"],
            ["Alice", "30", "NYC"],
            ["Bob", "25", "LA"],
        ];
        let mut chunks = Vec::new();
        for (r, &y) in rows_y.iter().enumerate() {
            for (c, &x) in cols.iter().enumerate() {
                chunks.push(chunk(labels[r][c], x, y, 30.0));
            }
        }
        let tables = detect_borderless(&chunks);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.source, TableSource::Borderless);
        assert_eq!(t.num_rows(), 3);
        assert_eq!(t.num_cols(), 3);
        assert_eq!(t.rows[0], vec!["Name", "Age", "City"]);
        assert_eq!(t.rows[1], vec!["Alice", "30", "NYC"]);
        assert_eq!(t.rows[2], vec!["Bob", "25", "LA"]);
        assert!(t.confidence > 0.9);
    }

    #[test]
    fn borderless_text_crossing_gutters_creates_colspan() {
        let chunks = vec![
            chunk("Group", 50.0, 700.0, 205.0),
            chunk("Other", 350.0, 700.0, 40.0),
            chunk("A", 50.0, 680.0, 20.0),
            chunk("B", 200.0, 680.0, 20.0),
            chunk("C", 350.0, 680.0, 20.0),
        ];
        let table = detect_borderless(&chunks).remove(0);
        let group = table
            .cells
            .iter()
            .find(|cell| cell.text == "Group")
            .unwrap();
        assert_eq!(group.colspan, 2);
        assert_eq!(table.header_hierarchy[0].parent.text, "Group");
    }

    #[test]
    fn prose_is_not_detected_as_a_table() {
        let mut chunks = Vec::new();
        for i in 0..6 {
            chunks.push(chunk(
                "some normal prose line here",
                50.0,
                700.0 - i as f64 * 14.0,
                200.0,
            ));
        }
        let tables = detect_borderless(&chunks);
        assert!(tables.is_empty(), "single-column prose is not a table");
    }

    #[test]
    fn detect_tables_prefers_ruled_then_borderless() {
        let (chunks, g) = ruled_2x2();
        let tables = detect_tables(&chunks, &g);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].source, TableSource::Ruled);
    }

    #[test]
    fn semantic_rows_mark_th_cells_as_headers() {
        let table = Table::from_semantic_rows(vec![
            vec![("Name".to_string(), true), ("Age".to_string(), true)],
            vec![("Alice".to_string(), false), ("30".to_string(), false)],
        ]);
        assert_eq!(table.source, TableSource::Semantic);
        assert!(table.cells[0].is_header);
        assert!(table.to_html().contains("<th scope=\"col\">Name</th>"));
    }
}
