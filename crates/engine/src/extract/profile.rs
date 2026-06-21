//! Part C — document-type detection, field profiles, and line-item extraction.
//!
//! Many high-value documents have a *known* field set. A lightweight, pure-Rust,
//! **data-driven** profile (not ML) boosts accuracy: it declares which canonical
//! fields to seek, their label synonyms, and each field's expected value type.
//! The profile then re-keys the spatial pairs to canonical names and assembles a
//! clean, typed result; for invoices/receipts it also maps the line-item table
//! to structured rows.
//!
//! Profiles are **data** (the [`FieldProfile`] tables below), so adding a new
//! document type or tuning labels is a data edit — no new code paths. The
//! invoice / receipt / generic-form profiles ship built in.

use crate::analysis::tables::Table;
use crate::extract::value::{normalize, ValueHint};
use crate::extract::{DocType, Field, FieldSource, LineItem};
use crate::parse::{BlockKind, Document};

/// One canonical field a profile seeks.
pub struct ProfileField {
    /// Canonical output key (e.g. `"invoice_number"`).
    pub key: &'static str,
    /// Label synonyms to match (case-insensitive, against spatially-found
    /// labels). The first synonym is also the "pretty" label.
    pub labels: &'static [&'static str],
    pub hint: ValueHint,
}

/// A document-type profile: the fields to seek.
pub struct FieldProfile {
    pub doc_type: DocType,
    pub fields: &'static [ProfileField],
}

/// The built-in invoice profile.
pub static INVOICE: FieldProfile = FieldProfile {
    doc_type: DocType::Invoice,
    fields: &[
        ProfileField {
            key: "invoice_number",
            labels: &["invoice number", "invoice no", "invoice #", "inv no", "inv #", "invoice"],
            hint: ValueHint::Any,
        },
        ProfileField {
            key: "invoice_date",
            labels: &["invoice date", "date", "issued", "issue date"],
            hint: ValueHint::Date,
        },
        ProfileField {
            key: "due_date",
            labels: &["due date", "payment due", "due"],
            hint: ValueHint::Date,
        },
        ProfileField {
            key: "po_number",
            labels: &["po number", "po #", "purchase order", "order number", "order no"],
            hint: ValueHint::Any,
        },
        ProfileField {
            key: "vendor",
            labels: &["vendor", "from", "seller", "supplier", "bill from"],
            hint: ValueHint::Any,
        },
        ProfileField {
            key: "bill_to",
            labels: &["bill to", "billed to", "customer", "sold to"],
            hint: ValueHint::Any,
        },
        ProfileField {
            key: "subtotal",
            labels: &["subtotal", "sub total", "net"],
            hint: ValueHint::Amount,
        },
        ProfileField {
            key: "tax",
            labels: &["tax", "vat", "gst", "sales tax"],
            hint: ValueHint::Amount,
        },
        ProfileField {
            key: "total",
            labels: &["total", "amount due", "balance due", "grand total", "total due"],
            hint: ValueHint::Amount,
        },
    ],
};

/// The built-in receipt profile.
pub static RECEIPT: FieldProfile = FieldProfile {
    doc_type: DocType::Receipt,
    fields: &[
        ProfileField {
            key: "merchant",
            labels: &["merchant", "store", "vendor", "name"],
            hint: ValueHint::Any,
        },
        ProfileField {
            key: "date",
            labels: &["date", "transaction date"],
            hint: ValueHint::Date,
        },
        ProfileField {
            key: "subtotal",
            labels: &["subtotal", "sub total"],
            hint: ValueHint::Amount,
        },
        ProfileField {
            key: "tax",
            labels: &["tax", "vat", "gst"],
            hint: ValueHint::Amount,
        },
        ProfileField {
            key: "total",
            labels: &["total", "amount", "amount due", "grand total"],
            hint: ValueHint::Amount,
        },
        ProfileField {
            key: "payment",
            labels: &["payment", "card", "paid", "tender"],
            hint: ValueHint::Any,
        },
    ],
};

/// A generic-form profile: no fixed field set (an empty profile just passes the
/// spatial pairs through), but it documents the type.
pub static GENERIC_FORM: FieldProfile = FieldProfile {
    doc_type: DocType::Form,
    fields: &[],
};

/// Look up the built-in profile for a document type.
pub fn profile_for(doc_type: DocType) -> &'static FieldProfile {
    match doc_type {
        DocType::Invoice => &INVOICE,
        DocType::Receipt => &RECEIPT,
        DocType::Form => &GENERIC_FORM,
        DocType::Generic => &GENERIC_FORM,
    }
}

// ── document-type detection ──────────────────────────────────────────────────

/// Detect the document type from keyword presence + structure. Pure heuristic:
/// strong invoice/receipt keywords win; an AcroForm-ish doc (many form fields)
/// is a Form; otherwise Generic.
pub fn detect_doc_type(doc: &Document, has_acroform_fields: bool) -> DocType {
    let text = all_text_lower(doc);

    let has = |needle: &str| text.contains(needle);

    let invoice_score = [
        has("invoice"),
        has("invoice number") || has("invoice no") || has("invoice #"),
        has("bill to"),
        has("amount due") || has("total due"),
        has("subtotal"),
    ]
    .iter()
    .filter(|b| **b)
    .count();

    let receipt_score = [
        has("receipt"),
        has("merchant") || has("store"),
        has("subtotal") && has("tax") && has("total"),
        has("change due") || has("tender") || has("card ****") || has("thank you"),
    ]
    .iter()
    .filter(|b| **b)
    .count();

    // Invoices and receipts share "total/subtotal/tax"; the discriminator is the
    // explicit "invoice" vs "receipt" cue.
    if has("invoice") && invoice_score >= receipt_score {
        return DocType::Invoice;
    }
    if has("receipt") || (receipt_score >= 2 && !has("invoice")) {
        return DocType::Receipt;
    }
    if invoice_score >= 2 {
        return DocType::Invoice;
    }
    if has_acroform_fields {
        return DocType::Form;
    }
    DocType::Generic
}

fn all_text_lower(doc: &Document) -> String {
    let mut s = String::new();
    for b in &doc.body {
        match &b.kind {
            BlockKind::Paragraph { text }
            | BlockKind::Text { text }
            | BlockKind::Heading { text, .. }
            | BlockKind::Title { text }
            | BlockKind::Header { text }
            | BlockKind::Footer { text }
            | BlockKind::Caption { text, .. } => {
                s.push_str(&text.to_plain());
                s.push('\n');
            }
            BlockKind::Table { table, .. } => {
                for row in &table.rows {
                    s.push_str(&row.join(" "));
                    s.push('\n');
                }
            }
            _ => {}
        }
    }
    s.to_ascii_lowercase()
}

// ── profile application: re-key spatial fields to canonical names ────────────

/// Apply a profile to the raw spatial fields: for each canonical field, find the
/// best-matching spatial pair (by label synonym), re-key it to the canonical
/// name, re-normalize under the profile's value hint, and mark it `Template`.
/// Returns `(canonical_fields, leftover_spatial_fields)`.
pub fn apply_profile(profile: &FieldProfile, spatial: Vec<Field>) -> (Vec<Field>, Vec<Field>) {
    let mut used = vec![false; spatial.len()];
    let mut canonical = Vec::new();

    for pf in profile.fields {
        // Find the spatial field whose (lowercased) key matches a synonym best.
        let mut best: Option<(usize, usize)> = None; // (idx, synonym-rank: lower=better)
        for (i, f) in spatial.iter().enumerate() {
            if used[i] {
                continue;
            }
            let fk = f.key.to_ascii_lowercase();
            for (rank, syn) in pf.labels.iter().enumerate() {
                if label_matches(&fk, syn) {
                    match best {
                        Some((_, r)) if r <= rank => {}
                        _ => best = Some((i, rank)),
                    }
                    break;
                }
            }
        }
        if let Some((i, _)) = best {
            used[i] = true;
            let src = &spatial[i];
            // Re-normalize the raw value under the profile's hint.
            let value = normalize(&src.raw, pf.hint);
            canonical.push(Field {
                key: pf.key.to_string(),
                value,
                raw: src.raw.clone(),
                page: src.page,
                bbox: src.bbox,
                // Template source; keep the spatial confidence (the profile only
                // re-keys/re-types, it doesn't add certainty).
                confidence: src.confidence,
                source: FieldSource::Template,
            });
        }
    }

    let leftovers: Vec<Field> = spatial
        .into_iter()
        .zip(used)
        .filter_map(|(f, u)| (!u).then_some(f))
        .collect();
    (canonical, leftovers)
}

/// Does a found label key match a profile synonym? Exact (case-insensitive) or
/// the key contains the synonym as a whole phrase (so "Total Due" matches
/// "total due", and "Invoice No." matches "invoice no").
fn label_matches(found_key: &str, synonym: &str) -> bool {
    let fk = found_key.trim_end_matches('.').trim();
    fk == synonym || fk.contains(synonym)
}

// ── line items: table → structured rows ─────────────────────────────────────

/// Find the line-item table (the largest multi-row table on the doc) and map its
/// columns to structured [`LineItem`]s by matching header names. Returns an
/// empty vec when no suitable table exists.
pub fn extract_line_items(doc: &Document) -> Vec<LineItem> {
    let Some(table) = pick_line_item_table(doc) else {
        return Vec::new();
    };
    map_table_rows(table)
}

/// Pick the table most likely to contain line items: the largest table that
/// has a recognizable line-item header row *somewhere* (a row containing a
/// "description"/"item" column alongside a "qty"/"price"/"amount" column).
fn pick_line_item_table(doc: &Document) -> Option<&Table> {
    let mut best: Option<&Table> = None;
    for b in &doc.body {
        if let BlockKind::Table { table, .. } = &b.kind {
            if table.rows.len() < 2 || find_header_row(table).is_none() {
                continue;
            }
            match best {
                Some(prev) if prev.rows.len() >= table.rows.len() => {}
                _ => best = Some(table),
            }
        }
    }
    best
}

/// Find the index of the line-item *header* row within a table — the row that
/// names the item columns. Returns `None` if no row looks like an item header.
fn find_header_row(table: &Table) -> Option<usize> {
    table.rows.iter().position(|row| {
        let has_desc = find_col(row, &["description", "item", "details", "product", "service"]).is_some();
        let has_metric = find_col(row, &["qty", "quantity", "price", "amount", "rate", "total"]).is_some();
        has_desc && has_metric
    })
}

/// Map a table's item rows to [`LineItem`]s. The header row is located anywhere
/// in the table; only the rows *after* it that still look like items (a
/// description and/or a numeric amount, and NOT a totals/label row) are mapped —
/// so header fields above and totals below are excluded.
fn map_table_rows(table: &Table) -> Vec<LineItem> {
    let Some(hdr) = find_header_row(table) else {
        return Vec::new();
    };
    let header = &table.rows[hdr];
    let desc_c = find_col(header, &["description", "item", "details", "product", "service"]);
    let qty_c = find_col(header, &["qty", "quantity", "units", "hours"]);
    let unit_c = find_col(header, &["unit price", "price", "rate", "unit cost"]);
    let amt_c = find_col(header, &["amount", "total", "line total", "ext"]);

    let mut items = Vec::new();
    for row in table.rows.iter().skip(hdr + 1) {
        let get = |c: Option<usize>| c.and_then(|i| row.get(i)).map(|s| s.trim().to_string());

        let description = get(desc_c).filter(|s| !s.is_empty());
        let quantity = get(qty_c).and_then(|s| crate::extract::value::parse_number(&s));
        let unit_price = get(unit_c)
            .filter(|s| !s.is_empty())
            .map(|s| normalize(&s, ValueHint::Amount));
        let amount = get(amt_c)
            .filter(|s| !s.is_empty())
            .map(|s| normalize(&s, ValueHint::Amount));

        // Stop at totals/label rows: a row whose description cell is a label
        // (e.g. "Subtotal:", "Total:") is not a line item.
        if let Some(d) = &description {
            if d.trim_end().ends_with(':') || is_totals_word(d) {
                continue;
            }
        }
        // Skip rows with neither a description nor an amount.
        if description.is_none() && amount.is_none() {
            continue;
        }
        items.push(LineItem {
            description,
            quantity,
            unit_price,
            amount,
        });
    }
    items
}

fn is_totals_word(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    matches!(
        l.as_str(),
        "subtotal" | "sub total" | "total" | "tax" | "vat" | "gst" | "amount due" | "balance"
            | "grand total" | "discount" | "shipping"
    )
}

fn find_col(header: &[String], names: &[&str]) -> Option<usize> {
    header.iter().position(|h| {
        let hl = h.to_ascii_lowercase();
        names.iter().any(|n| hl.contains(n))
    })
}

/// Heuristic vendor/merchant from the top-of-page block when no labeled value
/// was found (the seller name is usually the largest text at the top).
pub fn top_block_text(doc: &Document) -> Option<(String, u32, [f64; 4], f32)> {
    doc.body
        .iter()
        .filter(|b| b.page == 1)
        .filter_map(|b| match &b.kind {
            BlockKind::Title { text } | BlockKind::Heading { text, .. } => {
                let t = text.to_plain().trim().to_string();
                (!t.is_empty()).then_some((t, b.page, b.bbox, b.confidence))
            }
            _ => None,
        })
        // Highest on the page (largest y-top).
        .max_by(|a, b| a.2[3].total_cmp(&b.2[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_match_logic() {
        assert!(label_matches("invoice no.", "invoice no"));
        assert!(label_matches("total", "total"));
        assert!(label_matches("grand total", "total"));
        assert!(!label_matches("subtotal note", "total due"));
    }

    #[test]
    fn find_col_by_header_name() {
        let header: Vec<String> = ["Description", "Qty", "Unit Price", "Amount"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(find_col(&header, &["description"]), Some(0));
        assert_eq!(find_col(&header, &["qty", "quantity"]), Some(1));
        assert_eq!(find_col(&header, &["amount", "total"]), Some(3));
        assert_eq!(find_col(&header, &["nonexistent"]), None);
    }
}
