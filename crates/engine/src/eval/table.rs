//! Table-extraction quality metrics: cell-level precision/recall/F1 and a
//! TEDS (Tree-Edit-Distance-based Similarity) approximation — the table metric
//! Docling and the table-extraction literature report, so results are comparable.

use crate::eval::text::edit_distance;

/// A simple grid table for scoring: `rows[r][c]` cell text (already
/// span-flattened). Both the reference and the system output are reduced to this
/// shape before scoring.
#[derive(Debug, Clone, PartialEq)]
pub struct GridTable {
    pub rows: Vec<Vec<String>>,
}

impl GridTable {
    pub fn new(rows: Vec<Vec<String>>) -> Self {
        GridTable { rows }
    }

    pub fn n_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn n_cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }

    /// Cell at `(r, c)`, normalized (trimmed, internal whitespace collapsed).
    fn cell(&self, r: usize, c: usize) -> String {
        normalize_cell(
            self.rows
                .get(r)
                .and_then(|row| row.get(c))
                .map(String::as_str)
                .unwrap_or(""),
        )
    }
}

fn normalize_cell(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Precision / recall / F1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prf {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

impl Prf {
    pub fn from_counts(true_pos: usize, pred: usize, gold: usize) -> Prf {
        let precision = if pred == 0 {
            if gold == 0 {
                1.0
            } else {
                0.0
            }
        } else {
            true_pos as f64 / pred as f64
        };
        let recall = if gold == 0 {
            if pred == 0 {
                1.0
            } else {
                0.0
            }
        } else {
            true_pos as f64 / gold as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        Prf {
            precision,
            recall,
            f1,
        }
    }
}

/// Cell-level precision/recall/F1: a predicted non-empty cell counts as a true
/// positive when the cell at the SAME `(row, col)` in the reference has matching
/// normalized text. Non-empty cells are the unit (empty grid slots aren't
/// scored), matching how cell-F1 is usually reported.
pub fn cell_f1(reference: &GridTable, predicted: &GridTable) -> Prf {
    let rows = reference.n_rows().max(predicted.n_rows());
    let cols = reference.n_cols().max(predicted.n_cols());
    let mut tp = 0usize;
    let mut pred_cells = 0usize;
    let mut gold_cells = 0usize;
    for r in 0..rows {
        for c in 0..cols {
            let g = reference.cell(r, c);
            let p = predicted.cell(r, c);
            if !g.is_empty() {
                gold_cells += 1;
            }
            if !p.is_empty() {
                pred_cells += 1;
            }
            if !g.is_empty() && g == p {
                tp += 1;
            }
        }
    }
    Prf::from_counts(tp, pred_cells, gold_cells)
}

/// TEDS approximation in `[0,1]` (1.0 = identical). True TEDS is the tree edit
/// distance between the two tables' HTML DOM trees normalized by tree size; here
/// we approximate it with a structure term (how close the row/col shape is) and
/// a content term (normalized cell-content edit distance over the aligned grid),
/// which tracks TEDS closely for flat grid tables (the common case) without an
/// HTML-tree dependency. Documented as an approximation.
///
/// `teds ≈ structure_score * content_score` where:
/// - `structure_score = 1 - |Δrows|+|Δcols| / (rows+cols)` (shape agreement),
/// - `content_score = 1 - sum(cell edit distance) / sum(reference cell length)`.
pub fn teds_approx(reference: &GridTable, predicted: &GridTable) -> f64 {
    let r_rows = reference.n_rows();
    let r_cols = reference.n_cols();
    let p_rows = predicted.n_rows();
    let p_cols = predicted.n_cols();

    if r_rows == 0 && p_rows == 0 {
        return 1.0;
    }
    if r_rows == 0 || p_rows == 0 {
        return 0.0;
    }

    // Structure term.
    let shape_denom = (r_rows + r_cols) as f64;
    let shape_diff = ((r_rows as i64 - p_rows as i64).unsigned_abs()
        + (r_cols as i64 - p_cols as i64).unsigned_abs()) as f64;
    let structure_score = (1.0 - shape_diff / shape_denom).max(0.0);

    // Content term over the union grid.
    let rows = r_rows.max(p_rows);
    let cols = r_cols.max(p_cols);
    let mut dist_sum = 0usize;
    let mut len_sum = 0usize;
    for r in 0..rows {
        for c in 0..cols {
            let g = reference.cell(r, c);
            let p = predicted.cell(r, c);
            let gc: Vec<char> = g.chars().collect();
            let pc: Vec<char> = p.chars().collect();
            dist_sum += edit_distance(&gc, &pc);
            len_sum += gc.len().max(pc.len());
        }
    }
    let content_score = if len_sum == 0 {
        1.0
    } else {
        (1.0 - dist_sum as f64 / len_sum as f64).max(0.0)
    };

    structure_score * content_score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(rows: &[&[&str]]) -> GridTable {
        GridTable::new(
            rows.iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
        )
    }

    #[test]
    fn prf_from_counts() {
        let p = Prf::from_counts(3, 4, 5);
        assert!((p.precision - 0.75).abs() < 1e-9);
        assert!((p.recall - 0.6).abs() < 1e-9);
        assert!((p.f1 - 2.0 * 0.75 * 0.6 / 1.35).abs() < 1e-9);
        // empty/empty is perfect.
        let e = Prf::from_counts(0, 0, 0);
        assert_eq!(e.f1, 1.0);
    }

    #[test]
    fn cell_f1_perfect() {
        let r = t(&[&["A", "B"], &["1", "2"]]);
        let p = r.clone();
        let prf = cell_f1(&r, &p);
        assert_eq!(prf.f1, 1.0);
        assert_eq!(prf.precision, 1.0);
        assert_eq!(prf.recall, 1.0);
    }

    #[test]
    fn cell_f1_one_wrong_cell() {
        let r = t(&[&["A", "B"], &["1", "2"]]);
        let p = t(&[&["A", "B"], &["1", "9"]]); // last cell wrong
        let prf = cell_f1(&r, &p);
        // 3 of 4 non-empty cells correct, all 4 predicted, all 4 gold.
        assert!((prf.precision - 0.75).abs() < 1e-9);
        assert!((prf.recall - 0.75).abs() < 1e-9);
    }

    #[test]
    fn cell_f1_normalizes_whitespace() {
        let r = t(&[&["hello world"]]);
        let p = t(&[&["hello   world"]]);
        assert_eq!(cell_f1(&r, &p).f1, 1.0);
    }

    #[test]
    fn cell_f1_missing_cells_hurt_recall() {
        let r = t(&[&["A", "B"], &["1", "2"]]);
        let p = t(&[&["A", "B"]]); // missing second row
        let prf = cell_f1(&r, &p);
        // 2 of 4 gold recovered; precision over 2 predicted is 1.0.
        assert!((prf.recall - 0.5).abs() < 1e-9);
        assert!((prf.precision - 1.0).abs() < 1e-9);
    }

    #[test]
    fn teds_identical_is_one() {
        let r = t(&[&["A", "B"], &["1", "2"]]);
        assert!((teds_approx(&r, &r.clone()) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn teds_degrades_with_content_and_shape() {
        let r = t(&[&["Name", "Qty"], &["Widget", "10"]]);
        // same shape, one cell changed → high but < 1.
        let p1 = t(&[&["Name", "Qty"], &["Widget", "99"]]);
        let s1 = teds_approx(&r, &p1);
        assert!(s1 < 1.0 && s1 > 0.7, "got {s1}");
        // wrong shape (missing column) → lower.
        let p2 = t(&[&["Name"], &["Widget"]]);
        let s2 = teds_approx(&r, &p2);
        assert!(s2 < s1, "shape error should score lower: {s2} vs {s1}");
    }

    #[test]
    fn teds_empty_cases() {
        let empty = GridTable::new(vec![]);
        assert_eq!(teds_approx(&empty, &empty), 1.0);
        let r = t(&[&["A"]]);
        assert_eq!(teds_approx(&r, &empty), 0.0);
    }
}
