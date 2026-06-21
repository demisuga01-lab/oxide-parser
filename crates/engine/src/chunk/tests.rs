//! Unit tests for the semantic chunker. Synthetic [`Document`]s only (no PDF).

use super::*;
use crate::parse::{Block, BlockKind, Document, DocumentMetadata, InlineText, SourceInfo};

// ── builders ────────────────────────────────────────────────────────────────

fn block(id: usize, page: u32, ro: u32, kind: BlockKind) -> Block {
    Block {
        id,
        page,
        bbox: [50.0, 700.0 - ro as f64 * 20.0, 300.0, 720.0 - ro as f64 * 20.0],
        reading_order: ro,
        confidence: 0.9,
        kind,
    }
}

fn heading(id: usize, ro: u32, level: u8, text: &str) -> Block {
    block(id, 1, ro, BlockKind::Heading { level, text: InlineText::plain(text) })
}

fn para(id: usize, ro: u32, text: &str) -> Block {
    block(id, 1, ro, BlockKind::Paragraph { text: InlineText::plain(text) })
}

fn doc(blocks: Vec<Block>) -> Document {
    Document {
        schema_version: "1.1".into(),
        metadata: DocumentMetadata {
            title: Some("Test Doc".into()),
            page_count: 1,
            pdf_version: "1.7".into(),
            ..Default::default()
        },
        source: SourceInfo::DigitalBorn,
        body: blocks,
        pages: vec![],
    }
}

/// ~`words` words of filler prose.
fn prose(words: usize) -> String {
    let bank = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel"];
    (0..words).map(|i| bank[i % bank.len()]).collect::<Vec<_>>().join(" ")
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
fn splits_at_heading_boundaries() {
    let d = doc(vec![
        heading(0, 0, 1, "Section One"),
        para(1, 1, &prose(20)),
        heading(2, 2, 1, "Section Two"),
        para(3, 3, &prose(20)),
    ]);
    let set = d.chunk(&ChunkOptions { overlap_tokens: 0, ..Default::default() });
    // Two sections → at least two chunks, each carrying its own heading.
    assert!(set.chunks.len() >= 2, "got {} chunks", set.chunks.len());
    assert!(set.chunks[0].text.contains("Section One"));
    assert!(set.chunks.iter().any(|c| c.text.contains("Section Two")));
    // No chunk mixes both section headings (heading starts a new chunk).
    for c in &set.chunks {
        let one = c.text.contains("Section One");
        let two = c.text.contains("Section Two");
        assert!(!(one && two), "a chunk mixed two sections:\n{}", c.text);
    }
}

#[test]
fn packs_to_token_target() {
    // Many small paragraphs under one section; with a small target they should
    // pack into multiple chunks, each ≤ target (+ heading-context slack).
    let mut blocks = vec![heading(0, 0, 1, "S")];
    for i in 0..20 {
        blocks.push(para(i + 1, (i + 1) as u32, &prose(20)));
    }
    let d = doc(blocks);
    let target = 80;
    let set = d.chunk(&ChunkOptions {
        target_tokens: target,
        overlap_tokens: 0,
        ..Default::default()
    });
    assert!(set.chunks.len() > 1, "should split into multiple chunks");
    for c in &set.chunks {
        // Allow modest slack for the prepended heading-context line.
        assert!(
            c.tokens <= target + 20,
            "chunk {} has {} tokens > target {}",
            c.index,
            c.tokens,
            target
        );
    }
}

#[test]
fn oversized_paragraph_splits_at_sentences_not_mid_sentence() {
    // One paragraph far bigger than the target, made of clear sentences.
    let big = (0..40)
        .map(|i| format!("This is sentence number {i} with several words in it."))
        .collect::<Vec<_>>()
        .join(" ");
    let d = doc(vec![para(0, 0, &big)]);
    let set = d.chunk(&ChunkOptions {
        target_tokens: 60,
        overlap_tokens: 0,
        heading_context: false,
        ..Default::default()
    });
    assert!(set.chunks.len() > 1, "oversized paragraph should split");
    // No chunk ends mid-sentence: each chunk's text ends with sentence
    // punctuation (modulo trailing whitespace).
    for c in &set.chunks {
        let t = c.text.trim_end();
        assert!(
            t.ends_with('.') || t.ends_with('!') || t.ends_with('?'),
            "chunk ended mid-sentence:\n{:?}",
            t
        );
    }
}

#[test]
fn table_is_isolated_and_never_split() {
    use crate::analysis::tables::{Table, TableSource};
    let table = Table {
        rows: vec![
            vec!["A".into(), "B".into()],
            vec!["1".into(), "2".into()],
            vec!["3".into(), "4".into()],
        ],
        cells: vec![],
        header_hierarchy: vec![],
        source: TableSource::Borderless,
        confidence: 0.8,
        bbox: [0.0, 0.0, 100.0, 100.0],
        notes: vec![],
    };
    let d = doc(vec![
        heading(0, 0, 1, "Data"),
        para(1, 1, &prose(10)),
        block(2, 1, 2, BlockKind::Table { table, caption: None }),
        para(3, 3, &prose(10)),
    ]);
    let set = d.chunk(&ChunkOptions { overlap_tokens: 0, ..Default::default() });
    // Exactly one chunk is the table, flagged, and it contains the whole grid.
    let table_chunks: Vec<&Chunk> = set.chunks.iter().filter(|c| c.is_table_or_figure).collect();
    assert_eq!(table_chunks.len(), 1, "one isolated table chunk");
    let tc = table_chunks[0];
    assert!(tc.text.contains("| A | B |"), "table grid intact:\n{}", tc.text);
    assert!(tc.text.contains("| 3 | 4 |"), "all rows present:\n{}", tc.text);
}

#[test]
fn overlap_carries_trailing_context() {
    let mut blocks = vec![heading(0, 0, 1, "S")];
    for i in 0..10 {
        blocks.push(para(i + 1, (i + 1) as u32, &format!("paragraph number {i} {}", prose(15))));
    }
    let d = doc(blocks);
    let no_overlap = d.chunk(&ChunkOptions {
        target_tokens: 60,
        overlap_tokens: 0,
        heading_context: false,
        split_on_headings: false,
        ..Default::default()
    });
    let with_overlap = d.chunk(&ChunkOptions {
        target_tokens: 60,
        overlap_tokens: 40,
        heading_context: false,
        split_on_headings: false,
        ..Default::default()
    });
    assert!(no_overlap.chunks.len() > 1 && with_overlap.chunks.len() > 1);
    // With overlap, the start of chunk 2 repeats the tail of chunk 1.
    let c1_tail: Vec<&str> = no_overlap.chunks[0].text.split_whitespace().rev().take(5).collect();
    let c2 = &with_overlap.chunks[1].text;
    let repeated = c1_tail.iter().filter(|w| c2.contains(**w)).count();
    assert!(repeated >= 1, "overlap should repeat trailing context:\n{c2}");
}

#[test]
fn heading_context_prepended_with_path() {
    let d = doc(vec![
        block(0, 1, 0, BlockKind::Title { text: InlineText::plain("Doc Title") }),
        heading(1, 1, 1, "Chapter"),
        heading(2, 2, 2, "Subsection"),
        para(3, 3, &prose(20)),
    ]);
    let set = d.chunk(&ChunkOptions { overlap_tokens: 0, ..Default::default() });
    // The paragraph's chunk carries the full heading path prefix.
    let body_chunk = set
        .chunks
        .iter()
        .find(|c| c.block_kinds.iter().any(|k| k == "paragraph"))
        .expect("a paragraph chunk");
    assert!(
        body_chunk.text.contains("Chapter") && body_chunk.text.contains("Subsection"),
        "heading-context path should be prepended:\n{}",
        body_chunk.text
    );
    assert_eq!(
        body_chunk.section_path,
        vec!["Doc Title".to_string(), "Chapter".to_string(), "Subsection".to_string()],
        "section path nests title > chapter > subsection"
    );
}

#[test]
fn metadata_is_populated() {
    let d = doc(vec![heading(0, 0, 1, "Sec"), para(1, 1, &prose(30))]);
    let set = d.chunk(&ChunkOptions { overlap_tokens: 0, ..Default::default() });
    assert_eq!(set.title.as_deref(), Some("Test Doc"));
    let c = &set.chunks[0];
    assert_eq!(c.index, 0);
    assert!(c.tokens > 0);
    assert_eq!(c.pages, vec![1]);
    assert!(!c.block_kinds.is_empty());
    assert!(!c.bboxes.is_empty(), "bboxes carried for citation");
}

#[test]
fn furniture_excluded_by_default() {
    let d = doc(vec![
        block(0, 1, 0, BlockKind::Header { text: InlineText::plain("RUNNING HEAD") }),
        para(1, 1, "Real body content here."),
        block(2, 1, 2, BlockKind::Footer { text: InlineText::plain("page footer") }),
    ]);
    let set = d.chunk(&ChunkOptions::default());
    let all: String = set.chunks.iter().map(|c| c.text.clone()).collect();
    assert!(all.contains("Real body content"));
    assert!(!all.contains("RUNNING HEAD"), "furniture excluded:\n{all}");
    assert!(!all.contains("page footer"));
}

#[test]
fn deterministic_same_doc_same_chunks() {
    let d = doc(vec![
        heading(0, 0, 1, "A"),
        para(1, 1, &prose(40)),
        heading(2, 2, 1, "B"),
        para(3, 3, &prose(40)),
    ]);
    let opts = ChunkOptions::default();
    let a = d.chunk(&opts);
    let b = d.chunk(&opts);
    assert_eq!(a.to_json(), b.to_json(), "same doc + opts → identical chunks");
}

#[test]
fn empty_document_yields_no_chunks() {
    let d = doc(vec![]);
    let set = d.chunk(&ChunkOptions::default());
    assert!(set.chunks.is_empty());
}

#[test]
fn pages_span_recorded_across_a_chunk() {
    // Two small paragraphs on different pages packed into one chunk.
    let d = doc(vec![
        block(0, 1, 0, BlockKind::Paragraph { text: InlineText::plain(prose(10)) }),
        block(1, 2, 1, BlockKind::Paragraph { text: InlineText::plain(prose(10)) }),
    ]);
    let set = d.chunk(&ChunkOptions {
        target_tokens: 1000, // pack both into one chunk
        overlap_tokens: 0,
        heading_context: false,
        ..Default::default()
    });
    assert_eq!(set.chunks.len(), 1);
    assert_eq!(set.chunks[0].pages, vec![1, 2], "chunk spans both pages");
}
