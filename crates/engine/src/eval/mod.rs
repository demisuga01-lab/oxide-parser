//! **Extraction-quality evaluation metrics** — the pure-Rust scoring core for the
//! extraction benchmark (Parser-Pivot prompt 6).
//!
//! These are standard, deterministic metrics so the numbers are comparable to
//! how Docling / PyMuPDF / the extraction literature report:
//!
//! - text: [`text::cer`] / [`text::wer`] (+ accuracies) and
//!   [`text::reading_order_similarity`] (normalized Kendall-tau);
//! - tables: [`table::cell_f1`] and [`table::teds_approx`] (a TEDS approximation);
//! - fields: [`fields::field_f1`] (SROIE/FUNSD-style, normalized values);
//! - structure: [`fields::block_type_accuracy`].
//!
//! Keeping the metrics in pure Rust (not the Python harness) makes them
//! unit-testable in `cargo test` and reusable; the head-to-head harness shells
//! out to `oxide eval-score` (see [`score_json`]) so every tool — Oxide and each
//! competitor — is scored by the *same* implementation.

pub mod fields;
pub mod table;
pub mod text;

use serde::{Deserialize, Serialize};

pub use fields::{block_type_accuracy, field_f1, KvField};
pub use table::{cell_f1, teds_approx, GridTable, Prf};
pub use text::{cer, char_accuracy, reading_order_similarity, wer, word_accuracy};

// ════════════════════════════════════════════════════════════════════════════
// JSON scoring entry point (driven by the `oxide eval-score` CLI)
// ════════════════════════════════════════════════════════════════════════════

/// One scoring request: a reference (ground-truth) side and a hypothesis (a
/// tool's output), each optional per capability. Fields absent on both sides are
/// simply not scored. This is the JSON the benchmark harness feeds to
/// `oxide eval-score`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreInput {
    /// Reading-ordered plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyp_text: Option<String>,
    /// Block identities in reading order (for the order metric). Identities must
    /// be comparable across ref/hyp (e.g. a normalized text key per block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyp_order: Option<Vec<String>>,
    /// Tables as flat grids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_tables: Option<Vec<Vec<Vec<String>>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyp_tables: Option<Vec<Vec<Vec<String>>>>,
    /// KV fields (values should be passed normalized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_fields: Option<Vec<KvPair>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyp_fields: Option<Vec<KvPair>>,
    /// Block-type label sequences (aligned by index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_block_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyp_block_types: Option<Vec<String>>,
}

/// A serializable key/value pair (mirrors [`KvField`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvPair {
    pub key: String,
    pub value: String,
}

impl From<&KvPair> for KvField {
    fn from(p: &KvPair) -> Self {
        KvField {
            key: p.key.clone(),
            value: p.value.clone(),
        }
    }
}

/// The scores produced for a [`ScoreInput`]. A capability not present in the
/// input yields `None` for its scores.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cer: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub char_accuracy: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wer: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_accuracy: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_order: Option<f64>,
    /// Mean cell-F1 across the aligned tables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_cell_f1: Option<f64>,
    /// Mean TEDS-approx across the aligned tables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_teds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_precision: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_recall: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_f1: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_type_accuracy: Option<f64>,
}

/// Compute all applicable scores for an input.
pub fn score(input: &ScoreInput) -> ScoreOutput {
    let mut out = ScoreOutput::default();

    if let (Some(r), Some(h)) = (&input.ref_text, &input.hyp_text) {
        out.cer = Some(cer(r, h));
        out.char_accuracy = Some(char_accuracy(r, h));
        out.wer = Some(wer(r, h));
        out.word_accuracy = Some(word_accuracy(r, h));
    }
    if let (Some(r), Some(h)) = (&input.ref_order, &input.hyp_order) {
        out.reading_order = Some(reading_order_similarity(r, h));
    }
    if let (Some(r), Some(h)) = (&input.ref_tables, &input.hyp_tables) {
        // Align tables pairwise by index; score the common prefix, count missing
        // tables as zero so a tool that drops a table is penalized.
        let n = r.len().max(h.len());
        if n > 0 {
            let mut f1_sum = 0.0;
            let mut teds_sum = 0.0;
            for i in 0..n {
                let rt = r.get(i).cloned().map(GridTable::new).unwrap_or_else(|| GridTable::new(vec![]));
                let ht = h.get(i).cloned().map(GridTable::new).unwrap_or_else(|| GridTable::new(vec![]));
                f1_sum += cell_f1(&rt, &ht).f1;
                teds_sum += teds_approx(&rt, &ht);
            }
            out.table_cell_f1 = Some(f1_sum / n as f64);
            out.table_teds = Some(teds_sum / n as f64);
        }
    }
    if let (Some(r), Some(h)) = (&input.ref_fields, &input.hyp_fields) {
        let rf: Vec<KvField> = r.iter().map(KvField::from).collect();
        let hf: Vec<KvField> = h.iter().map(KvField::from).collect();
        let prf = field_f1(&rf, &hf);
        out.field_precision = Some(prf.precision);
        out.field_recall = Some(prf.recall);
        out.field_f1 = Some(prf.f1);
    }
    if let (Some(r), Some(h)) = (&input.ref_block_types, &input.hyp_block_types) {
        out.block_type_accuracy = Some(block_type_accuracy(r, h));
    }
    out
}

/// Parse a [`ScoreInput`] from JSON, score it, and return [`ScoreOutput`] JSON.
/// The `oxide eval-score` CLI is a thin wrapper over this; the harness pipes one
/// JSON object per tool/doc through it so every tool is scored identically.
pub fn score_json(input_json: &str) -> Result<String, String> {
    let input: ScoreInput =
        serde_json::from_str(input_json).map_err(|e| format!("invalid ScoreInput JSON: {e}"))?;
    let output = score(&input);
    serde_json::to_string(&output).map_err(|e| format!("serialize ScoreOutput: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_text_only() {
        let input = ScoreInput {
            ref_text: Some("the quick brown fox".into()),
            hyp_text: Some("the quick brown fox".into()),
            ..Default::default()
        };
        let out = score(&input);
        assert_eq!(out.cer, Some(0.0));
        assert_eq!(out.char_accuracy, Some(1.0));
        assert!(out.reading_order.is_none(), "no order input → no order score");
    }

    #[test]
    fn score_all_capabilities() {
        let input = ScoreInput {
            ref_text: Some("Total 486".into()),
            hyp_text: Some("Total 486".into()),
            ref_order: Some(vec!["a".into(), "b".into()]),
            hyp_order: Some(vec!["a".into(), "b".into()]),
            ref_tables: Some(vec![vec![vec!["A".into(), "B".into()]]]),
            hyp_tables: Some(vec![vec![vec!["A".into(), "B".into()]]]),
            ref_fields: Some(vec![KvPair { key: "total".into(), value: "486".into() }]),
            hyp_fields: Some(vec![KvPair { key: "total".into(), value: "486".into() }]),
            ref_block_types: Some(vec!["heading".into()]),
            hyp_block_types: Some(vec!["heading".into()]),
        };
        let out = score(&input);
        assert_eq!(out.char_accuracy, Some(1.0));
        assert_eq!(out.reading_order, Some(1.0));
        assert_eq!(out.table_cell_f1, Some(1.0));
        assert_eq!(out.field_f1, Some(1.0));
        assert_eq!(out.block_type_accuracy, Some(1.0));
    }

    #[test]
    fn score_json_roundtrip() {
        let json = r#"{"ref_text":"abc","hyp_text":"abX"}"#;
        let out = score_json(json).unwrap();
        assert!(out.contains("\"cer\""));
        // 1 substitution / 3 chars.
        let parsed: ScoreOutput = serde_json::from_str(&out).unwrap();
        assert!((parsed.cer.unwrap() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn score_json_rejects_garbage() {
        assert!(score_json("not json").is_err());
    }

    #[test]
    fn missing_table_penalized() {
        // Reference has one table; hypothesis has none → cell-F1 should be 0.
        let input = ScoreInput {
            ref_tables: Some(vec![vec![vec!["A".into(), "B".into()]]]),
            hyp_tables: Some(vec![]),
            ..Default::default()
        };
        let out = score(&input);
        assert_eq!(out.table_cell_f1, Some(0.0));
    }
}
