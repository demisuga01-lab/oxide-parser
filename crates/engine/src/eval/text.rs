//! Text-extraction quality metrics: character/word error rate and reading-order
//! similarity. Pure-Rust, deterministic, standard definitions so the numbers are
//! comparable to how competitors report (CER/WER are the OCR/extraction standard).

/// Levenshtein edit distance between two char sequences (insertions, deletions,
/// substitutions each cost 1). O(n·m) time, O(min) space.
pub fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // Two-row DP, iterating over the shorter on the inner axis for less memory.
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ai) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, bj) in b.iter().enumerate() {
            let cost = if ai == bj { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Character Error Rate: `edit_distance(chars) / len(reference_chars)`, clamped
/// so an empty reference with non-empty hypothesis reports 1.0 (total error)
/// rather than dividing by zero. Lower is better; 0.0 is perfect.
pub fn cer(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<char> = reference.chars().collect();
    let h: Vec<char> = hypothesis.chars().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

/// Character *accuracy* = `1 - CER`, floored at 0 (CER can exceed 1 when the
/// hypothesis is much longer than the reference).
pub fn char_accuracy(reference: &str, hypothesis: &str) -> f64 {
    (1.0 - cer(reference, hypothesis)).max(0.0)
}

/// Word Error Rate: token-level edit distance / reference word count. Tokenizes
/// on Unicode whitespace. Lower is better.
pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<&str> = reference.split_whitespace().collect();
    let h: Vec<&str> = hypothesis.split_whitespace().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

/// Word *accuracy* = `1 - WER`, floored at 0.
pub fn word_accuracy(reference: &str, hypothesis: &str) -> f64 {
    (1.0 - wer(reference, hypothesis)).max(0.0)
}

// ── reading order ────────────────────────────────────────────────────────────

/// Reading-order similarity in `[0,1]` via normalized Kendall-tau distance.
///
/// Given the *reference* order of a set of block identities and the *predicted*
/// order of (a subset of) the same identities, this measures how often pairs are
/// in the same relative order. `1.0` = identical order, `0.5` = random, `0.0` =
/// fully reversed. Items present in the prediction but not the reference (and
/// vice-versa) are ignored — only the common set's relative order is scored, so
/// extra/missing blocks don't dominate the order metric (text CER captures those
/// separately).
///
/// This is the metric that rewards structure-aware parsers: a naive top-to-bottom
/// dump scrambles multi-column reading order; a precedence-graph parser preserves
/// it, and that shows up here.
pub fn reading_order_similarity(reference: &[String], predicted: &[String]) -> f64 {
    // Map each common item to its rank in the reference.
    use std::collections::HashMap;
    let ref_rank: HashMap<&str, usize> = reference
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    // Predicted sequence projected to items that exist in the reference,
    // de-duplicated keeping first occurrence (a block should appear once).
    let mut seen = std::collections::HashSet::new();
    let proj: Vec<usize> = predicted
        .iter()
        .filter_map(|s| ref_rank.get(s.as_str()).copied())
        .filter(|r| seen.insert(*r))
        .collect();
    let n = proj.len();
    if n < 2 {
        // 0 or 1 comparable items: perfectly ordered by definition.
        return 1.0;
    }
    let total_pairs = n * (n - 1) / 2;
    let mut concordant = 0usize;
    for i in 0..n {
        for j in (i + 1)..n {
            // Concordant if the predicted pair (i before j) agrees with the
            // reference ranks.
            if proj[i] < proj[j] {
                concordant += 1;
            }
        }
    }
    concordant as f64 / total_pairs as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance(b"kitten", b"sitting"), 3);
        assert_eq!(edit_distance(b"", b"abc"), 3);
        assert_eq!(edit_distance(b"abc", b"abc"), 0);
    }

    #[test]
    fn cer_perfect_and_errors() {
        assert_eq!(cer("hello world", "hello world"), 0.0);
        // one substitution over 11 chars.
        let c = cer("hello world", "hallo world");
        assert!((c - 1.0 / 11.0).abs() < 1e-9, "got {c}");
        // empty reference.
        assert_eq!(cer("", ""), 0.0);
        assert_eq!(cer("", "x"), 1.0);
    }

    #[test]
    fn char_accuracy_is_one_minus_cer() {
        assert_eq!(char_accuracy("abcd", "abcd"), 1.0);
        assert!((char_accuracy("abcd", "abXd") - 0.75).abs() < 1e-9);
        // Much-longer hypothesis floors at 0, never negative.
        assert_eq!(char_accuracy("a", "aaaaaaaaaa"), 0.0);
    }

    #[test]
    fn wer_counts_words() {
        assert_eq!(wer("the quick brown fox", "the quick brown fox"), 0.0);
        // one word substituted of four.
        assert!((wer("the quick brown fox", "the quick red fox") - 0.25).abs() < 1e-9);
        // collapse of whitespace doesn't matter.
        assert_eq!(wer("a  b\tc", "a b c"), 0.0);
    }

    #[test]
    fn reading_order_identical_and_reversed() {
        let r: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(reading_order_similarity(&r, &r), 1.0);
        let rev: Vec<String> = r.iter().rev().cloned().collect();
        assert_eq!(reading_order_similarity(&r, &rev), 0.0);
    }

    #[test]
    fn reading_order_partial_and_swapped() {
        let r: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        // one adjacent swap among 4 → 5/6 concordant pairs.
        let p: Vec<String> = ["a", "c", "b", "d"].iter().map(|s| s.to_string()).collect();
        let s = reading_order_similarity(&r, &p);
        assert!((s - 5.0 / 6.0).abs() < 1e-9, "got {s}");
        // unknown items in prediction are ignored.
        let p2: Vec<String> = ["a", "z", "b", "c", "d"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(reading_order_similarity(&r, &p2), 1.0);
    }

    #[test]
    fn reading_order_degenerate() {
        let r = vec!["only".to_string()];
        assert_eq!(reading_order_similarity(&r, &r), 1.0);
        let empty: Vec<String> = vec![];
        assert_eq!(reading_order_similarity(&empty, &empty), 1.0);
    }
}
