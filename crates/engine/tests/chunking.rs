//! Integration smoke tests for RAG chunking on real PDFs (Parser-Pivot prompt 5).
//!
//! Unit-level chunker behavior (boundaries/overlap/heading-context/metadata/
//! determinism) is covered in `src/chunk/tests.rs`; these exercise the full
//! parse → chunk pipeline on a real multi-section document and confirm sensible,
//! deterministic, structure-respecting output.

use oxide_engine::{ChunkOptions, ContentEngine, ParseOptions};

/// A real, long, multi-section academic paper (17 pages).
fn tracemonkey() -> ContentEngine {
    let bytes = std::fs::read("tests/fixtures/tracemonkey.pdf").expect("fixture");
    ContentEngine::open_bytes(bytes).unwrap()
}

#[test]
fn chunks_a_real_paper_into_sensible_passages() {
    let engine = tracemonkey();
    let doc = engine
        .parse_document(&ParseOptions { omit_furniture: false, ..Default::default() })
        .unwrap();
    let set = doc.chunk(&ChunkOptions {
        target_tokens: 300,
        overlap_tokens: 50,
        ..Default::default()
    });

    assert!(set.chunks.len() > 10, "a 17-page paper should yield many chunks");
    assert_eq!(set.target_tokens, 300);

    // Indices are contiguous from 0.
    for (i, c) in set.chunks.iter().enumerate() {
        assert_eq!(c.index, i, "chunk indices must be contiguous");
    }

    // Size sanity: the median chunk is in a reasonable band near the target, and
    // the vast majority sit at or below target + slop. (Oversized single blocks
    // — e.g. a code listing with no sentence breaks — are allowed but flagged.)
    let mut toks: Vec<usize> = set.chunks.iter().map(|c| c.tokens).collect();
    toks.sort_unstable();
    let median = toks[toks.len() / 2];
    assert!((120..=360).contains(&median), "median chunk tokens = {median}");
    let within = set.chunks.iter().filter(|c| c.tokens <= 360).count();
    assert!(
        within * 100 / set.chunks.len() >= 80,
        "≥80% of chunks should be within target+slop; got {within}/{}",
        set.chunks.len()
    );
    for c in &set.chunks {
        if c.tokens > 360 {
            assert!(c.oversized, "an over-target chunk must be flagged oversized");
        }
    }

    // Every chunk carries citation metadata.
    for c in &set.chunks {
        assert!(c.tokens > 0);
        assert!(!c.pages.is_empty(), "chunk {} has no page", c.index);
        assert!(!c.block_kinds.is_empty());
    }

    // Heading context: many chunks carry a section path, and a chunk that opens
    // with its own heading does not print that heading twice.
    let with_section = set.chunks.iter().filter(|c| !c.section_path.is_empty()).count();
    assert!(with_section > 0, "some chunks should carry a section path");
    for c in &set.chunks {
        if let Some(last) = c.section_path.last() {
            let dup = format!("# {last}\n\n## {last}");
            assert!(!c.text.contains(&dup), "heading printed twice:\n{}", c.text);
        }
    }
}

#[test]
fn chunking_is_deterministic_on_a_real_pdf() {
    let engine = tracemonkey();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    let opts = ChunkOptions::default();
    let a = doc.chunk(&opts);
    let b = doc.chunk(&opts);
    assert_eq!(a.to_json(), b.to_json(), "same doc + opts → identical chunks");
}

#[test]
fn tables_stay_intact_as_their_own_chunks() {
    // tracemonkey has figures/tables; any table/figure chunk must be isolated
    // (flagged) and not merged with prose.
    let engine = tracemonkey();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    let set = doc.chunk(&ChunkOptions::default());
    for c in &set.chunks {
        if c.is_table_or_figure {
            // An isolated table/figure chunk holds exactly one source block.
            assert_eq!(
                c.block_kinds.len(),
                1,
                "table/figure chunk should be a single block: {:?}",
                c.block_kinds
            );
        }
    }
}
