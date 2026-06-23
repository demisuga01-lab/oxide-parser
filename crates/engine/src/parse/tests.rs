//! Unit + serializer + determinism tests for the canonical document model.
//! Synthetic [`DocBlock`] inputs only — no PDF files needed (engine-level
//! integration lives in `crates/engine/tests/`).

use super::*;
use crate::analysis::tables::{Table, TableCell, TableSource};
use crate::docmodel::{ClassifiedType, DocBlock, ListItem};

// ── builders ────────────────────────────────────────────────────────────────

fn blk(id: usize, page: usize, ro: usize, classified: ClassifiedType, text: &str) -> DocBlock {
    DocBlock {
        id,
        classified,
        page,
        bbox: [50.0, 700.0, 300.0, 720.0],
        reading_order_index: ro,
        text: text.to_string(),
        confidence: 0.9,
        basis: vec![],
        items: vec![],
        caption_id: None,
        figure_id: None,
        header_footer: false,
        page_number: false,
        is_bold: false,
        is_italic: false,
        table: None,
    }
}

fn assemble_default(blocks: &[DocBlock]) -> Document {
    let dims: Vec<(usize, f64, f64)> = vec![(1, 612.0, 792.0)];
    assemble(
        blocks,
        DocumentMetadata {
            page_count: 1,
            pdf_version: "1.7".into(),
            ..Default::default()
        },
        SourceInfo::DigitalBorn,
        &dims,
        &[],
        &[],
        &ParseOptions::default(),
    )
}

// ── InlineText ────────────────────────────────────────────────────────────────

#[test]
fn inline_plain_and_styled_roundtrip() {
    let p = InlineText::plain("hello");
    assert_eq!(p.to_plain(), "hello");
    assert_eq!(p.spans.len(), 1);
    assert!(!p.spans[0].bold && !p.spans[0].italic);

    let s = InlineText::styled("hi", true, true);
    assert!(s.spans[0].bold && s.spans[0].italic);

    assert!(InlineText::plain("").is_empty());
    assert!(InlineText::styled("", true, false).is_empty());
}

#[test]
fn block_emphasis_survives_into_model_and_markdown() {
    let mut b = blk(0, 1, 0, ClassifiedType::Paragraph, "important");
    b.is_bold = true;
    let doc = assemble_default(&[b]);
    // Model carries the bold flag.
    match &doc.body[0].kind {
        BlockKind::Paragraph { text } => assert!(text.spans[0].bold),
        other => panic!("expected paragraph, got {other:?}"),
    }
    // Markdown renders it bold.
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("**important**"), "md was: {md:?}");
    // HTML renders <strong>.
    let html = doc.to_html(&SerializeOptions::default());
    assert!(html.contains("<strong>important</strong>"), "html: {html}");
}

#[test]
fn inline_link_survives_to_markdown_and_html() {
    // Links aren't populated by the builder yet, but the model + serializers must
    // already honor them.
    let mut b = blk(0, 1, 0, ClassifiedType::Paragraph, "");
    let kind = BlockKind::Paragraph {
        text: InlineText {
            spans: vec![
                InlineSpan {
                    text: "see ".into(),
                    ..Default::default()
                },
                InlineSpan {
                    text: "the site".into(),
                    bold: true,
                    italic: false,
                    link: Some("https://example.com/a b".into()),
                },
            ],
        },
    };
    b.classified = ClassifiedType::Paragraph;
    // Replace via a direct Block to exercise the serializer with a link span.
    let block = Block {
        id: 0,
        page: 1,
        bbox: [0.0; 4],
        reading_order: 0,
        confidence: 0.9,
        kind,
    };
    let doc = Document {
        schema_version: SCHEMA_VERSION.into(),
        metadata: DocumentMetadata::default(),
        source: SourceInfo::DigitalBorn,
        body: vec![block],
        pages: vec![],
    };
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(
        md.contains("[**the site**](https://example.com/a%20b)"),
        "md: {md}"
    );
    let html = doc.to_html(&SerializeOptions::default());
    assert!(
        html.contains("<a href=\"https://example.com/a b\"><strong>the site</strong></a>"),
        "html: {html}"
    );
}

// ── headings / lists ──────────────────────────────────────────────────────────

#[test]
fn headings_render_by_level() {
    let blocks = vec![
        blk(0, 1, 0, ClassifiedType::Title, "The Title"),
        blk(1, 1, 1, ClassifiedType::Heading { level: 1 }, "Section"),
        blk(2, 1, 2, ClassifiedType::Heading { level: 2 }, "Subsection"),
    ];
    let md = assemble_default(&blocks).to_markdown(&SerializeOptions::default());
    assert!(md.contains("# The Title"));
    assert!(md.contains("## Section"));
    assert!(md.contains("### Subsection"));
}

#[test]
fn page_breaks_marked_when_enabled() {
    // Two blocks on different pages.
    let mut b0 = blk(0, 1, 0, ClassifiedType::Paragraph, "On page one.");
    b0.page = 1;
    let mut b1 = blk(1, 2, 1, ClassifiedType::Paragraph, "On page two.");
    b1.page = 2;
    let dims = vec![(1, 612.0, 792.0), (2, 612.0, 792.0)];
    let doc = assemble(
        &[b0, b1],
        DocumentMetadata {
            page_count: 2,
            pdf_version: "1.7".into(),
            ..Default::default()
        },
        SourceInfo::DigitalBorn,
        &dims,
        &[],
        &[],
        &ParseOptions::default(),
    );

    // Off by default: no marker.
    let plain = doc.to_markdown(&SerializeOptions::default());
    assert!(
        !plain.contains("<!-- page 2 -->"),
        "no marker by default:\n{plain}"
    );

    // On: a marker between the two pages' blocks.
    let marked = doc.to_markdown(&SerializeOptions {
        mark_page_breaks: true,
        ..Default::default()
    });
    assert!(
        marked.contains("<!-- page 2 -->"),
        "page marker present:\n{marked}"
    );
    let p1 = marked.find("On page one").unwrap();
    let mark = marked.find("<!-- page 2 -->").unwrap();
    let p2 = marked.find("On page two").unwrap();
    assert!(
        p1 < mark && mark < p2,
        "marker sits between pages:\n{marked}"
    );

    // HTML uses an <hr> with data-page.
    let html = doc.to_html(&SerializeOptions {
        mark_page_breaks: true,
        ..Default::default()
    });
    assert!(
        html.contains("<hr class=\"page-break\" data-page=\"2\">"),
        "html hr:\n{html}"
    );
    assert!(
        html.contains("<article>") && html.contains("</article>"),
        "semantic article wrapper"
    );
}

#[test]
fn provenance_annotations_when_enabled() {
    let b = blk(0, 1, 0, ClassifiedType::Paragraph, "Traceable text.");
    let doc = assemble_default(&[b]);

    let md = doc.to_markdown(&SerializeOptions {
        include_provenance: true,
        ..Default::default()
    });
    assert!(
        md.contains("<!-- @page=1 bbox="),
        "md provenance comment:\n{md}"
    );

    let html = doc.to_html(&SerializeOptions {
        include_provenance: true,
        ..Default::default()
    });
    assert!(html.contains("data-page=\"1\""), "html data-page:\n{html}");
    assert!(html.contains("data-bbox="), "html data-bbox:\n{html}");

    // Default output carries NO provenance (clean for ingestion).
    let clean = doc.to_markdown(&SerializeOptions::default());
    assert!(!clean.contains("@page"), "default md is clean:\n{clean}");
}

#[test]
fn paragraph_lines_flow_into_one_block() {
    // A paragraph whose source text has embedded line breaks must render as one
    // flowing line (newlines collapsed to spaces), not N separate lines.
    let b = blk(
        0,
        1,
        0,
        ClassifiedType::Paragraph,
        "first line\nsecond line\nthird line",
    );
    let md = assemble_default(&[b]).to_markdown(&SerializeOptions::default());
    assert!(
        md.contains("first line second line third line"),
        "paragraph should flow into one line:\n{md:?}"
    );
}

#[test]
fn list_renders_ordered_and_unordered() {
    let mut ul = blk(0, 1, 0, ClassifiedType::List { ordered: false }, "");
    ul.items = vec![
        ListItem {
            text: "• first".into(),
            bbox: [0.0; 4],
            marker: Some("•".into()),
            ordered: false,
        },
        ListItem {
            text: "• second".into(),
            bbox: [0.0; 4],
            marker: Some("•".into()),
            ordered: false,
        },
    ];
    let mut ol = blk(1, 1, 1, ClassifiedType::List { ordered: true }, "");
    ol.items = vec![
        ListItem {
            text: "1. alpha".into(),
            bbox: [0.0; 4],
            marker: Some("1.".into()),
            ordered: true,
        },
        ListItem {
            text: "2. beta".into(),
            bbox: [0.0; 4],
            marker: Some("2.".into()),
            ordered: true,
        },
    ];
    let md = assemble_default(&[ul, ol]).to_markdown(&SerializeOptions::default());
    assert!(md.contains("- first"), "md: {md}");
    assert!(md.contains("- second"));
    assert!(md.contains("1. alpha"), "md: {md}");
    assert!(md.contains("2. beta"));

    let html = assemble_default(&[]).to_html(&SerializeOptions::default());
    let _ = html; // smoke: empty doc still produces a valid shell
}

// ── tables ────────────────────────────────────────────────────────────────────

fn small_table() -> Table {
    let cells = vec![
        TableCell {
            row: 0,
            col: 0,
            rowspan: 1,
            colspan: 1,
            text: "H1".into(),
            bbox: [0.0; 4],
            is_header: true,
            header_scope: None,
            nested_tables: vec![],
        },
        TableCell {
            row: 0,
            col: 1,
            rowspan: 1,
            colspan: 1,
            text: "H2".into(),
            bbox: [0.0; 4],
            is_header: true,
            header_scope: None,
            nested_tables: vec![],
        },
        TableCell {
            row: 1,
            col: 0,
            rowspan: 1,
            colspan: 1,
            text: "a".into(),
            bbox: [0.0; 4],
            is_header: false,
            header_scope: None,
            nested_tables: vec![],
        },
        TableCell {
            row: 1,
            col: 1,
            rowspan: 1,
            colspan: 1,
            text: "b".into(),
            bbox: [0.0; 4],
            is_header: false,
            header_scope: None,
            nested_tables: vec![],
        },
    ];
    Table {
        rows: vec![vec!["H1".into(), "H2".into()], vec!["a".into(), "b".into()]],
        cells,
        header_hierarchy: vec![],
        source: TableSource::Ruled,
        confidence: 0.95,
        bbox: [0.0; 4],
        notes: vec![],
    }
}

#[test]
fn table_block_renders_md_and_html() {
    let mut t = blk(0, 1, 0, ClassifiedType::Table, "csv");
    t.table = Some(small_table());
    let doc = assemble_default(&[t]);
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("| H1 | H2 |"), "md: {md}");
    assert!(md.contains("| --- | --- |"));
    assert!(md.contains("| a | b |"));
    let html = doc.to_html(&SerializeOptions::default());
    assert!(html.contains("<table>"), "html: {html}");
    assert!(html.contains("<th"));
}

// ── figure + caption linkage ──────────────────────────────────────────────────

#[test]
fn figure_caption_link_renders_under_figure_not_twice() {
    let mut fig = blk(0, 1, 0, ClassifiedType::Figure, "");
    fig.caption_id = Some(1);
    let mut cap = blk(1, 1, 1, ClassifiedType::Caption, "Figure 1: a chart");
    cap.figure_id = Some(0);

    let doc = assemble_default(&[fig, cap]);
    let md = doc.to_markdown(&SerializeOptions::default());
    // The caption is rendered exactly once (under the figure).
    assert_eq!(md.matches("Figure 1: a chart").count(), 1, "md: {md}");
    assert!(md.contains("!["), "figure image syntax present: {md}");
    let html = doc.to_html(&SerializeOptions::default());
    assert!(html.contains("<figure>"));
    assert!(html.contains("<figcaption>Figure 1: a chart</figcaption>"));
}

// ── furniture handling ────────────────────────────────────────────────────────

#[test]
fn furniture_omitted_from_body_but_kept_in_pages() {
    let blocks = vec![
        blk(0, 1, 0, ClassifiedType::Header, "running head"),
        blk(1, 1, 1, ClassifiedType::Paragraph, "body text"),
        blk(2, 1, 2, ClassifiedType::PageNumber, "1"),
    ];
    // Default omit_furniture = true.
    let doc = assemble_default(&blocks);
    assert_eq!(doc.body.len(), 1, "only the paragraph remains in body");
    assert!(matches!(doc.body[0].kind, BlockKind::Paragraph { .. }));
    // Page view still references all three ids.
    assert_eq!(doc.pages.len(), 1);
    assert_eq!(doc.pages[0].block_ids, vec![0, 1, 2]);

    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(!md.contains("running head"), "furniture stripped: {md}");
    assert!(md.contains("body text"));
}

#[test]
fn furniture_kept_when_requested() {
    let blocks = vec![
        blk(0, 1, 0, ClassifiedType::Header, "running head"),
        blk(1, 1, 1, ClassifiedType::Paragraph, "body text"),
    ];
    let opts = ParseOptions {
        omit_furniture: false,
        ..Default::default()
    };
    let dims = vec![(1usize, 612.0, 792.0)];
    let doc = assemble(
        &blocks,
        DocumentMetadata::default(),
        SourceInfo::DigitalBorn,
        &dims,
        &[],
        &[],
        &opts,
    );
    assert_eq!(doc.body.len(), 2);
    let md = doc.to_markdown(&SerializeOptions {
        include_furniture: true,
        ..Default::default()
    });
    assert!(md.contains("<!-- header: running head -->"), "md: {md}");
}

// ── reading-order densification + ids ─────────────────────────────────────────

#[test]
fn body_reading_order_densified_ids_preserved() {
    let blocks = vec![
        blk(7, 1, 0, ClassifiedType::Header, "head"),
        blk(8, 1, 1, ClassifiedType::Paragraph, "p1"),
        blk(9, 1, 2, ClassifiedType::Paragraph, "p2"),
    ];
    let doc = assemble_default(&blocks);
    // Header dropped → body has 2 blocks with reading_order 0,1 but original ids.
    assert_eq!(doc.body.len(), 2);
    assert_eq!(doc.body[0].id, 8);
    assert_eq!(doc.body[0].reading_order, 0);
    assert_eq!(doc.body[1].id, 9);
    assert_eq!(doc.body[1].reading_order, 1);
}

// ── JSON shape + schema version ───────────────────────────────────────────────

#[test]
fn json_carries_schema_version_and_kind_tag() {
    let doc = assemble_default(&[blk(0, 1, 0, ClassifiedType::Paragraph, "hi")]);
    let json = doc.to_json();
    assert!(json.contains("\"schema_version\": \"1.1\""), "json: {json}");
    assert!(json.contains("\"kind\": \"paragraph\""));
    assert!(json.contains("\"source\""));
    // Roundtrips back to an equal model.
    let back: Document = serde_json::from_str(&json).expect("roundtrip");
    assert_eq!(back, doc);
}

// ── determinism ───────────────────────────────────────────────────────────────

#[test]
fn serialization_is_byte_identical() {
    let blocks = vec![
        blk(0, 1, 0, ClassifiedType::Title, "T"),
        blk(1, 1, 1, ClassifiedType::Heading { level: 1 }, "H"),
        blk(2, 1, 2, ClassifiedType::Paragraph, "body"),
    ];
    let a = assemble_default(&blocks);
    let b = assemble_default(&blocks);
    assert_eq!(a.to_json(), b.to_json());
    assert_eq!(
        a.to_markdown(&SerializeOptions::default()),
        b.to_markdown(&SerializeOptions::default())
    );
    assert_eq!(
        a.to_html(&SerializeOptions::default()),
        b.to_html(&SerializeOptions::default())
    );
}

// ── markdown escaping ─────────────────────────────────────────────────────────

#[test]
fn markdown_metacharacters_escaped() {
    let doc = assemble_default(&[blk(0, 1, 0, ClassifiedType::Paragraph, "a*b_c[d]")]);
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("a\\*b\\_c\\[d\\]"), "md: {md}");
}

// ── text-cleanup options ──────────────────────────────────────────────────────

fn assemble_with_opts(blocks: &[DocBlock], options: &ParseOptions) -> Document {
    let dims: Vec<(usize, f64, f64)> = vec![(1, 612.0, 792.0)];
    assemble(
        blocks,
        DocumentMetadata::default(),
        SourceInfo::DigitalBorn,
        &dims,
        &[],
        &[],
        options,
    )
}

#[test]
fn dehyphenate_joins_line_broken_words_only() {
    assert_eq!(dehyphenate("compi- lation"), "compilation");
    assert_eq!(dehyphenate("inter-\nnational"), "international");
    // Real compound hyphen (followed by lowercase but NOT across a break) is kept.
    assert_eq!(dehyphenate("well-known"), "well-known");
    // Capitalized continuation (likely a proper noun across a dash) is kept.
    assert_eq!(dehyphenate("North- America"), "North- America");
}

#[test]
fn dehyphenate_off_by_default_on_in_options() {
    let b = blk(0, 1, 0, ClassifiedType::Paragraph, "frag- mented");
    // default: unchanged
    let plain = assemble_default(std::slice::from_ref(&b));
    assert!(plain
        .to_markdown(&SerializeOptions::default())
        .contains("frag- mented"));
    // with option
    let opts = ParseOptions {
        dehyphenate: true,
        ..Default::default()
    };
    let cleaned = assemble_with_opts(&[b], &opts);
    let md = cleaned.to_markdown(&SerializeOptions::default());
    assert!(md.contains("fragmented"), "md: {md}");
}

#[test]
fn ligature_normalization_maps_presentation_forms() {
    assert_eq!(normalize_ligatures("\u{FB01}le"), "file");
    assert_eq!(normalize_ligatures("\u{FB02}ow"), "flow");
    assert_eq!(normalize_ligatures("e\u{FB03}cient"), "efficient");
    let b = blk(0, 1, 0, ClassifiedType::Paragraph, "\u{FB01}nal");
    let opts = ParseOptions {
        normalize_ligatures: true,
        ..Default::default()
    };
    let doc = assemble_with_opts(&[b], &opts);
    assert!(doc
        .to_markdown(&SerializeOptions::default())
        .contains("final"));
}

// ── link attachment ───────────────────────────────────────────────────────────

#[test]
fn link_attached_to_overlapping_block() {
    let mut b = blk(0, 1, 0, ClassifiedType::Paragraph, "click here");
    b.bbox = [100.0, 700.0, 200.0, 712.0];
    let dims = vec![(1usize, 612.0, 792.0)];
    let links = vec![PageLink {
        page: 1,
        rect: [105.0, 702.0, 150.0, 710.0], // overlaps the block
        uri: "https://example.com".into(),
    }];
    let doc = assemble(
        &[b],
        DocumentMetadata::default(),
        SourceInfo::DigitalBorn,
        &dims,
        &[],
        &links,
        &ParseOptions::default(),
    );
    match &doc.body[0].kind {
        BlockKind::Paragraph { text } => {
            assert_eq!(text.spans[0].link.as_deref(), Some("https://example.com"));
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("](https://example.com)"), "md: {md}");
}

#[test]
fn link_not_attached_to_nonoverlapping_block() {
    let mut b = blk(0, 1, 0, ClassifiedType::Paragraph, "no link");
    b.bbox = [100.0, 700.0, 200.0, 712.0];
    let dims = vec![(1usize, 612.0, 792.0)];
    let links = vec![PageLink {
        page: 1,
        rect: [400.0, 100.0, 450.0, 110.0], // far away
        uri: "https://example.com".into(),
    }];
    let doc = assemble(
        &[b],
        DocumentMetadata::default(),
        SourceInfo::DigitalBorn,
        &dims,
        &[],
        &links,
        &ParseOptions::default(),
    );
    match &doc.body[0].kind {
        BlockKind::Paragraph { text } => assert!(text.spans[0].link.is_none()),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

// ── per-page source / routing rollup ──────────────────────────────────────────

fn class(page: u32, src: PageSource) -> PageClassification {
    PageClassification {
        page,
        source: src,
        confidence: 0.9,
        char_count: 100,
        text_coverage: 0.1,
        image_coverage: 0.0,
        has_invisible_text: false,
    }
}

#[test]
fn page_source_stamped_from_classification() {
    let blocks = vec![blk(0, 1, 0, ClassifiedType::Paragraph, "p")];
    let dims = vec![(1usize, 612.0, 792.0), (2usize, 612.0, 792.0)];
    let classes = vec![
        class(1, PageSource::DigitalBorn),
        class(2, PageSource::Scanned),
    ];
    let doc = assemble(
        &blocks,
        DocumentMetadata::default(),
        SourceInfo::Mixed,
        &dims,
        &classes,
        &[],
        &ParseOptions::default(),
    );
    assert_eq!(doc.pages[0].source, PageSource::DigitalBorn);
    assert_eq!(doc.pages[1].source, PageSource::Scanned);
    assert!(doc.pages[0].classification.is_some());
}

#[test]
fn rollup_source_mixed_when_pages_differ() {
    let all_digital = vec![
        class(1, PageSource::DigitalBorn),
        class(2, PageSource::DigitalBornOverImage),
    ];
    assert_eq!(
        rollup_source(false, &all_digital, false),
        SourceInfo::DigitalBorn
    );

    let mixed = vec![
        class(1, PageSource::DigitalBorn),
        class(2, PageSource::Scanned),
    ];
    assert_eq!(rollup_source(false, &mixed, false), SourceInfo::Mixed);
    // A mixed doc stays Mixed even when OCR recovered text on its scanned page.
    assert_eq!(rollup_source(false, &mixed, true), SourceInfo::Mixed);

    let all_scanned = vec![class(1, PageSource::Scanned)];
    // No OCR (or none recovered) → Mixed; OCR recovered text → Ocr.
    assert_eq!(rollup_source(false, &all_scanned, false), SourceInfo::Mixed);
    assert_eq!(rollup_source(false, &all_scanned, true), SourceInfo::Ocr);

    // Tagged always wins as the document descriptor.
    assert_eq!(rollup_source(true, &mixed, true), SourceInfo::Tagged);
}
