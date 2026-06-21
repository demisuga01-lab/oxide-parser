//! Part B — spatial label→value pairing (the general KV engine).
//!
//! For documents without form fields (most invoices/receipts), find LABEL text
//! and pair it with the VALUE text spatially associated with it, using geometry
//! and patterns. Operates on the canonical [`crate::parse::Document`] blocks, so
//! it is **identical** for digital-born and OCR'd input.
//!
//! Pairing strategies, in priority order:
//! 1. **Inline** — `Total: $42.00` in one block: split at the colon.
//! 2. **Right-of** — the value block is on the same baseline, just to the right
//!    (or to the *left* for RTL).
//! 3. **Below** — the value block is the next line down, left-aligned with the
//!    label.
//!
//! Each pair is scored (label clarity × geometric strength × pattern match) and
//! low-scoring pairs are still emitted but flagged by a low `confidence`.

use crate::extract::value::{normalize, ValueHint};
use crate::extract::{Field, FieldSource};
use crate::parse::{Block, BlockKind, Document};

/// A flattened text fragment with geometry — one per block (plus colon-split
/// pieces). The spatial engine reasons entirely over these.
#[derive(Debug, Clone)]
struct Frag {
    text: String,
    page: u32,
    /// `[x0,y0,x1,y1]` user space, y-up.
    bbox: [f64; 4],
    confidence: f32,
}

impl Frag {
    fn cy(&self) -> f64 {
        (self.bbox[1] + self.bbox[3]) / 2.0
    }
    fn height(&self) -> f64 {
        (self.bbox[3] - self.bbox[1]).abs()
    }
    fn left(&self) -> f64 {
        self.bbox[0]
    }
    fn right(&self) -> f64 {
        self.bbox[2]
    }
}

/// Extract spatial label→value [`Field`]s from a document's body blocks.
///
/// Pairs come from two places: free text blocks (inline / right-of / below) and
/// **table cells** (a label cell pairs with the value cell in the same row, or
/// the cell below). Real invoices/receipts put most labeled fields inside an
/// (often borderless) table the layout engine recovered, so table-cell pairing
/// is essential — not an afterthought.
pub fn extract_spatial_fields(doc: &Document) -> Vec<Field> {
    let mut fields = extract_block_fields(doc);
    fields.extend(extract_table_fields(doc));
    fields
}

/// Label→value pairs from free (non-table) text blocks.
fn extract_block_fields(doc: &Document) -> Vec<Field> {
    let frags = collect_frags(doc);

    // First pass: inline "label: value" within a single fragment.
    let mut fields = Vec::new();
    let mut consumed = vec![false; frags.len()];

    for (i, f) in frags.iter().enumerate() {
        if let Some((label, value)) = split_inline_label_value(&f.text) {
            if is_label_text(&label) && !value.trim().is_empty() {
                fields.push(make_field(&label, &value, f, f, GeoKind::Inline));
                consumed[i] = true;
            }
        }
    }

    // Second pass: a fragment that is *just a label* paired with a neighbor.
    for (i, f) in frags.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        if !is_label_text(&f.text) {
            continue;
        }
        let label = strip_label(&f.text);
        if label.is_empty() {
            continue;
        }
        // Find the best value neighbor that is NOT itself a label.
        if let Some((j, kind)) = best_value_neighbor(&frags, i, &consumed) {
            let v = &frags[j];
            fields.push(make_field(&label, &v.text, f, v, kind));
            consumed[i] = true;
            consumed[j] = true;
        }
    }

    fields
}

/// Label→value pairs from inside table cells (Part B.2). For each row, a label
/// cell (`Total:` / a known label phrase) pairs with the next non-empty cell to
/// its right in the same row. This recovers invoice header fields and totals
/// that the layout engine grouped into a borderless table.
fn extract_table_fields(doc: &Document) -> Vec<Field> {
    let mut fields = Vec::new();
    for b in &doc.body {
        let BlockKind::Table { table, .. } = &b.kind else {
            continue;
        };
        for row in &table.rows {
            // Walk cells; when a label cell is found, the value is the next
            // non-empty cell in the same row (skipping blank grid slots).
            let mut ci = 0;
            while ci < row.len() {
                let cell = row[ci].trim();
                if !cell.is_empty() && is_label_text(cell) {
                    // Inline "label: value" inside one cell.
                    if let Some((label, value)) = split_inline_label_value(cell) {
                        if !value.trim().is_empty() {
                            fields.push(table_field(&label, &value, b.page, b.bbox, b.confidence));
                            ci += 1;
                            continue;
                        }
                    }
                    // Else: the value is the next non-empty cell to the right.
                    if let Some(vj) = (ci + 1..row.len()).find(|&j| !row[j].trim().is_empty()) {
                        let value = row[vj].trim();
                        if !is_label_text(value) {
                            fields.push(table_field(cell, value, b.page, b.bbox, b.confidence));
                            ci = vj + 1;
                            continue;
                        }
                    }
                }
                ci += 1;
            }
        }
    }
    fields
}

/// Build a table-derived field. The bbox is the table block's bbox (cell-level
/// geometry is not surfaced through `Table.rows`); confidence inherits the
/// block's, scaled by the geometric strength of an in-row pairing.
fn table_field(label: &str, raw_value: &str, page: u32, bbox: [f64; 4], block_conf: f32) -> Field {
    let key = strip_label(label);
    let raw = raw_value.trim().to_string();
    let hint = hint_for_label(&key);
    let value = normalize(&raw, hint);
    let label_clarity = if label.trim_end().ends_with(':') { 1.0 } else { 0.85 };
    let pattern_match = match (&value, hint) {
        (crate::extract::FieldValue::Text { .. }, ValueHint::Any) => 0.9,
        (crate::extract::FieldValue::Text { .. }, _) => 0.6,
        _ => 1.0,
    };
    let conf = (label_clarity * 0.9 * pattern_match * block_conf.clamp(0.1, 1.0)).clamp(0.0, 1.0);
    Field {
        key,
        value,
        raw,
        page,
        bbox,
        confidence: conf,
        source: FieldSource::Spatial,
    }
}

/// Flatten the document body into geometry-bearing fragments. Furniture and
/// figures contribute nothing useful; tables are handled by the profile layer.
fn collect_frags(doc: &Document) -> Vec<Frag> {
    let mut frags = Vec::new();
    for b in &doc.body {
        let Some(text) = block_text(b) else {
            continue;
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        frags.push(Frag {
            text,
            page: b.page,
            bbox: b.bbox,
            confidence: b.confidence,
        });
    }
    frags
}

/// The plain text of a block, for the block kinds that carry label/value text.
fn block_text(b: &Block) -> Option<String> {
    match &b.kind {
        BlockKind::Paragraph { text }
        | BlockKind::Text { text }
        | BlockKind::Heading { text, .. }
        | BlockKind::Title { text }
        | BlockKind::Caption { text, .. }
        | BlockKind::Header { text }
        | BlockKind::Footer { text } => Some(text.to_plain()),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum GeoKind {
    Inline,
    RightOf,
    Below,
    LeftOf, // RTL
}

impl GeoKind {
    /// Geometric strength component of the confidence.
    fn strength(self) -> f32 {
        match self {
            GeoKind::Inline => 1.0,
            GeoKind::RightOf | GeoKind::LeftOf => 0.9,
            GeoKind::Below => 0.75,
        }
    }
}

/// Split `"Total: $42.00"` → `("Total", "$42.00")`. Splits on the FIRST colon
/// that is followed by content. Returns `None` if there's no such colon.
fn split_inline_label_value(s: &str) -> Option<(String, String)> {
    let idx = s.find(':')?;
    let (l, r) = s.split_at(idx);
    let value = r[1..].trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some((l.trim().to_string(), value))
}

/// A short label lexicon of phrases that act as labels even without a colon.
const LABEL_LEXICON: &[&str] = &[
    "total", "subtotal", "tax", "amount due", "amount", "balance", "balance due",
    "invoice number", "invoice no", "invoice #", "invoice", "inv no", "inv #",
    "date", "invoice date", "due date", "order number", "order no", "order #",
    "po number", "po #", "purchase order", "bill to", "ship to", "sold to",
    "vendor", "customer", "account number", "account no", "reference", "ref",
    "quantity", "qty", "unit price", "price", "description", "phone", "email",
    "name", "address", "merchant", "store", "receipt number", "receipt no",
    "payment", "discount", "shipping", "grand total", "net", "gross",
];

/// Is this text a *label*? Either it ends with a colon (after a short-ish run),
/// or it matches a known label phrase. Labels are short; long prose is not.
fn is_label_text(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    let stripped = strip_label(t);
    let words = stripped.split_whitespace().count();
    // A trailing colon is the strongest cue.
    if t.trim_end().ends_with(':') && words <= 6 && !stripped.is_empty() {
        return true;
    }
    // Otherwise a known label phrase (case-insensitive), short.
    if words <= 4 {
        let low = stripped.to_ascii_lowercase();
        return LABEL_LEXICON.iter().any(|l| low == *l);
    }
    false
}

/// Remove a trailing colon and surrounding whitespace from a label.
fn strip_label(s: &str) -> String {
    s.trim().trim_end_matches(':').trim().to_string()
}

/// Find the best value fragment for the label at `label_idx`. Searches for a
/// right-of (same line), then below (aligned) neighbor, scoring by proximity.
/// Skips fragments that are themselves labels (so "Date:" never pairs with
/// "Total:") and fragments already consumed.
fn best_value_neighbor(
    frags: &[Frag],
    label_idx: usize,
    consumed: &[bool],
) -> Option<(usize, GeoKind)> {
    let label = &frags[label_idx];
    let line_tol = (label.height() * 0.6).max(2.0);

    let mut best: Option<(usize, GeoKind, f64)> = None; // (idx, kind, cost)

    for (j, f) in frags.iter().enumerate() {
        if j == label_idx || consumed[j] || f.page != label.page {
            continue;
        }
        if is_label_text(&f.text) {
            continue; // a value must not be another label
        }
        let same_line = (f.cy() - label.cy()).abs() <= line_tol;

        // Right-of (same line, starts after the label ends).
        if same_line && f.left() >= label.right() - line_tol {
            let cost = (f.left() - label.right()).max(0.0);
            consider(&mut best, j, GeoKind::RightOf, cost);
            continue;
        }
        // Left-of (RTL: value before the label on the same line).
        if same_line && f.right() <= label.left() + line_tol {
            let cost = (label.left() - f.right()).max(0.0) + 1000.0; // prefer right-of
            consider(&mut best, j, GeoKind::LeftOf, cost);
            continue;
        }
        // Below: next line down, left edges roughly aligned.
        let below = f.cy() < label.cy() - line_tol;
        let aligned = (f.left() - label.left()).abs() <= label.height().max(6.0) * 2.0;
        if below && aligned {
            let vgap = (label.cy() - f.cy()).max(0.0);
            // Only the *nearest* line below is a plausible value.
            let cost = vgap + 500.0; // prefer same-line neighbors over below
            consider(&mut best, j, GeoKind::Below, cost);
        }
    }
    best.map(|(j, k, _)| (j, k))
}

fn consider(best: &mut Option<(usize, GeoKind, f64)>, j: usize, kind: GeoKind, cost: f64) {
    match best {
        Some((_, _, c)) if *c <= cost => {}
        _ => *best = Some((j, kind, cost)),
    }
}

/// Build a [`Field`] from a label fragment, a value string, and the geometry of
/// the value's home fragment.
fn make_field(label: &str, raw_value: &str, label_frag: &Frag, value_frag: &Frag, kind: GeoKind) -> Field {
    let key = strip_label(label);
    let raw = raw_value.trim().to_string();
    let hint = hint_for_label(&key);
    let value = normalize(&raw, hint);

    // Confidence = label clarity × geometric strength × pattern match, scaled by
    // the value block's own (e.g. OCR) confidence.
    let label_clarity = if label.trim_end().ends_with(':') { 1.0 } else { 0.8 };
    let pattern_match = match (&value, hint) {
        (crate::extract::FieldValue::Text { .. }, ValueHint::Any) => 0.9, // text where text is fine
        (crate::extract::FieldValue::Text { .. }, _) => 0.6, // expected a type, got text
        _ => 1.0,                                            // typed value matched
    };
    let geo = kind.strength();
    let block_conf = value_frag.confidence.clamp(0.0, 1.0).max(0.1);
    let confidence = (label_clarity * geo * pattern_match * block_conf).clamp(0.0, 1.0);

    // The label fragment's geometry is not stored on the field (the value's
    // location is what matters for consumers), but it gates confidence above.
    let _ = label_frag;

    Field {
        key,
        value,
        raw,
        page: value_frag.page,
        bbox: value_frag.bbox,
        confidence,
        source: FieldSource::Spatial,
    }
}

/// Expected value type for a label, to bias normalization.
fn hint_for_label(key: &str) -> ValueHint {
    let k = key.to_ascii_lowercase();
    if k.contains("date") {
        ValueHint::Date
    } else if k.contains("email") {
        ValueHint::Email
    } else if k.contains("phone") || k.contains("tel") || k.contains("fax") {
        ValueHint::Phone
    } else if k.contains("total")
        || k.contains("amount")
        || k.contains("balance")
        || k.contains("subtotal")
        || k.contains("tax")
        || k.contains("price")
        || k.contains("due")
        || k.contains("payment")
        || k.contains("discount")
    {
        ValueHint::Amount
    } else if k == "qty" || k.contains("quantity") {
        ValueHint::Number
    } else {
        ValueHint::Any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_detection() {
        assert!(is_label_text("Total:"));
        assert!(is_label_text("Invoice Number:"));
        assert!(is_label_text("Total")); // lexicon
        assert!(!is_label_text("This is a long sentence of body prose, not a label"));
        assert!(!is_label_text(""));
    }

    #[test]
    fn inline_split() {
        assert_eq!(
            split_inline_label_value("Total: $42.00"),
            Some(("Total".into(), "$42.00".into()))
        );
        assert_eq!(split_inline_label_value("no colon here"), None);
        assert_eq!(split_inline_label_value("Trailing:"), None);
    }

    #[test]
    fn hint_mapping() {
        assert_eq!(hint_for_label("Invoice Date"), ValueHint::Date);
        assert_eq!(hint_for_label("Total"), ValueHint::Amount);
        assert_eq!(hint_for_label("Email"), ValueHint::Email);
        assert_eq!(hint_for_label("Vendor"), ValueHint::Any);
    }
}
