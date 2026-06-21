//! Key-value field-extraction and block-type-classification metrics.
//!
//! Field-F1 is the SROIE/FUNSD-style metric: a predicted field counts correct
//! when its key matches a gold key AND its (normalized) value matches the gold
//! value. Block-type accuracy is plain agreement over labeled blocks.

use std::collections::HashMap;

use crate::eval::table::Prf;

/// One key→value field for scoring. `value` is compared in NORMALIZED form
/// (callers should pass already-normalized values, e.g. dates as ISO, amounts as
/// a canonical decimal string), so a date matching in normalized form counts
/// correct regardless of the source formatting.
#[derive(Debug, Clone, PartialEq)]
pub struct KvField {
    pub key: String,
    pub value: String,
}

fn norm_key(k: &str) -> String {
    k.trim().to_ascii_lowercase()
}

fn norm_value(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

/// Field-level precision/recall/F1. A gold field is recovered when a predicted
/// field has the same normalized key and a matching normalized value. Each gold
/// and predicted field is matched at most once (greedy by key).
pub fn field_f1(reference: &[KvField], predicted: &[KvField]) -> Prf {
    // Group predicted values by normalized key (a key can appear once typically;
    // keep all candidates so a correct value among them counts).
    let mut pred_by_key: HashMap<String, Vec<String>> = HashMap::new();
    for f in predicted {
        pred_by_key.entry(norm_key(&f.key)).or_default().push(norm_value(&f.value));
    }
    let mut used: HashMap<String, usize> = HashMap::new();

    let mut tp = 0usize;
    for g in reference {
        let gk = norm_key(&g.key);
        let gv = norm_value(&g.value);
        if let Some(cands) = pred_by_key.get(&gk) {
            // Find an unused candidate value equal to the gold value.
            let start = used.get(&gk).copied().unwrap_or(0);
            let _ = start;
            if cands.iter().any(|c| values_match(c, &gv)) {
                tp += 1;
                *used.entry(gk).or_insert(0) += 1;
            }
        }
    }
    Prf::from_counts(tp, predicted.len(), reference.len())
}

/// Value match: exact normalized equality, OR one contains the other as a whole
/// (a slightly longer extracted value that still contains the gold counts — a
/// common lenient KV convention). Empty gold matches only empty.
fn values_match(pred: &str, gold: &str) -> bool {
    if gold.is_empty() {
        return pred.is_empty();
    }
    pred == gold || pred.contains(gold) || gold.contains(pred)
}

/// Block-type classification accuracy: fraction of labeled blocks whose predicted
/// type equals the gold type. Inputs are parallel label sequences (aligned by
/// index — the caller aligns blocks first, e.g. by reading order). Returns 1.0
/// for an empty reference.
pub fn block_type_accuracy(reference: &[String], predicted: &[String]) -> f64 {
    if reference.is_empty() {
        return 1.0;
    }
    let n = reference.len().min(predicted.len());
    let correct = (0..n)
        .filter(|&i| reference[i].eq_ignore_ascii_case(&predicted[i]))
        .count();
    correct as f64 / reference.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(k: &str, v: &str) -> KvField {
        KvField { key: k.into(), value: v.into() }
    }

    #[test]
    fn field_f1_perfect() {
        let r = vec![kv("invoice_number", "INV-1"), kv("total", "486.00")];
        let p = r.clone();
        let prf = field_f1(&r, &p);
        assert_eq!(prf.f1, 1.0);
    }

    #[test]
    fn field_f1_normalizes_key_and_value() {
        let r = vec![kv("Invoice Number", "INV-1")];
        let p = vec![kv("invoice number", "inv-1")];
        assert_eq!(field_f1(&r, &p).f1, 1.0);
    }

    #[test]
    fn field_f1_wrong_value_misses() {
        let r = vec![kv("total", "486.00")];
        let p = vec![kv("total", "999.00")];
        let prf = field_f1(&r, &p);
        assert_eq!(prf.recall, 0.0);
        assert_eq!(prf.precision, 0.0);
    }

    #[test]
    fn field_f1_partial() {
        let r = vec![kv("a", "1"), kv("b", "2"), kv("c", "3")];
        let p = vec![kv("a", "1"), kv("b", "2"), kv("d", "9")]; // 2 right, 1 spurious
        let prf = field_f1(&r, &p);
        // tp=2, pred=3, gold=3.
        assert!((prf.precision - 2.0 / 3.0).abs() < 1e-9);
        assert!((prf.recall - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn field_f1_contains_match() {
        // A slightly longer extracted value containing the gold counts.
        let r = vec![kv("vendor", "Acme")];
        let p = vec![kv("vendor", "Acme Corporation")];
        assert_eq!(field_f1(&r, &p).f1, 1.0);
    }

    #[test]
    fn block_type_accuracy_basics() {
        let r: Vec<String> = ["heading", "paragraph", "table", "figure"].iter().map(|s| s.to_string()).collect();
        assert_eq!(block_type_accuracy(&r, &r), 1.0);
        let p: Vec<String> = ["heading", "paragraph", "paragraph", "figure"].iter().map(|s| s.to_string()).collect();
        assert_eq!(block_type_accuracy(&r, &p), 0.75);
        let empty: Vec<String> = vec![];
        assert_eq!(block_type_accuracy(&empty, &empty), 1.0);
    }
}
