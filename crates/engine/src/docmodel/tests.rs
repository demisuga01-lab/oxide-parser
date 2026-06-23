//! Unit tests for the document-model layer: ordering, classification, list
//! grouping, figure/caption linkage, running header/footer/page-number, and
//! determinism. Synthetic inputs only — no PDF files needed (the engine-level
//! integration tests live in `crates/engine/tests/document_model.rs`).

use super::*;
use crate::analysis::layout::LayoutLine;

fn bx(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
    BBox { x0, y0, x1, y1 }
}

fn node(kind: RegionKind, x0: f64, y0: f64, x1: f64, y1: f64, idx: usize, rtl: bool) -> OrderNode {
    OrderNode::new(bx(x0, y0, x1, y1), kind, idx, rtl)
}

/// Order a fresh node set: assign columns then run reading_order.
fn order(mut nodes: Vec<OrderNode>, rtl: bool, line_h: f64) -> Vec<usize> {
    assign_columns(&mut nodes, line_h);
    reading_order(&nodes, rtl, line_h)
        .into_iter()
        .map(|pos| nodes[pos].original_index)
        .collect()
}

// ── ordering ────────────────────────────────────────────────────────────────

#[test]
fn o1_clean_two_columns() {
    // left col (x 50..290): top idx0, bot idx1; right col (x 310..550): top 2, bot 3.
    let nodes = vec![
        node(RegionKind::Text, 50.0, 740.0, 290.0, 780.0, 0, false),
        node(RegionKind::Text, 50.0, 690.0, 290.0, 730.0, 1, false),
        node(RegionKind::Text, 310.0, 740.0, 550.0, 780.0, 2, false),
        node(RegionKind::Text, 310.0, 690.0, 550.0, 730.0, 3, false),
    ];
    assert_eq!(order(nodes, false, 12.0), vec![0, 1, 2, 3]);
}

#[test]
fn o2_spanning_header_over_two_columns() {
    let nodes = vec![
        node(RegionKind::Text, 50.0, 760.0, 550.0, 790.0, 0, false), // spanning header
        node(RegionKind::Text, 50.0, 690.0, 290.0, 740.0, 1, false), // left
        node(RegionKind::Text, 310.0, 690.0, 550.0, 740.0, 2, false), // right
    ];
    assert_eq!(order(nodes, false, 12.0), vec![0, 1, 2]);
}

#[test]
fn o4_spanning_header_and_footer() {
    let nodes = vec![
        node(RegionKind::Text, 50.0, 760.0, 550.0, 790.0, 0, false), // header (top)
        node(RegionKind::Text, 50.0, 400.0, 290.0, 740.0, 1, false), // left col
        node(RegionKind::Text, 310.0, 400.0, 550.0, 740.0, 2, false), // right col
        node(RegionKind::Text, 50.0, 350.0, 550.0, 380.0, 3, false), // footer (bottom)
    ];
    assert_eq!(order(nodes, false, 12.0), vec![0, 1, 2, 3]);
}

#[test]
fn o5_rtl_two_columns_right_first() {
    // RTL page: right column (x 310..550) reads before left column.
    let nodes = vec![
        node(RegionKind::Text, 50.0, 740.0, 290.0, 780.0, 0, true), // left top
        node(RegionKind::Text, 50.0, 690.0, 290.0, 730.0, 1, true), // left bot
        node(RegionKind::Text, 310.0, 740.0, 550.0, 780.0, 2, true), // right top
        node(RegionKind::Text, 310.0, 690.0, 550.0, 730.0, 3, true), // right bot
    ];
    // right column first: 2,3 then 0,1
    assert_eq!(order(nodes, true, 12.0), vec![2, 3, 0, 1]);
}

#[test]
fn o7_unequal_height_columns_no_interleave() {
    // The ORD-C1 regression: left col tall (two stacked), right col one short
    // block high up. Column-major must yield [L-top, L-bot, R], NOT [L-top, R, L-bot].
    let nodes = vec![
        node(RegionKind::Text, 50.0, 740.0, 290.0, 780.0, 0, false), // L-top
        node(RegionKind::Text, 50.0, 300.0, 290.0, 340.0, 1, false), // L-bot
        node(RegionKind::Text, 310.0, 740.0, 550.0, 780.0, 2, false), // R (high)
    ];
    assert_eq!(order(nodes, false, 12.0), vec![0, 1, 2]);
}

#[test]
fn o8_cycle_fallback_returns_all_nodes() {
    // Hand-built 3-cycle adjacency through the internal seam: every node has an
    // in-edge, so Kahn starts empty and the fallback must still return all 3.
    let nodes = vec![
        node(RegionKind::Text, 0.0, 30.0, 10.0, 40.0, 0, false),
        node(RegionKind::Text, 0.0, 20.0, 10.0, 30.0, 1, false),
        node(RegionKind::Text, 0.0, 10.0, 10.0, 20.0, 2, false),
    ];
    let adj = vec![vec![1], vec![2], vec![0]];
    let indeg = vec![1, 1, 1];
    let out = reading_order_from_edges(adj, indeg, &nodes, false);
    let mut sorted = out.clone();
    sorted.sort();
    assert_eq!(sorted, vec![0, 1, 2], "all nodes returned despite cycle");
}

#[test]
fn o9_order_is_permutation_and_deterministic() {
    let nodes = vec![
        node(RegionKind::Text, 50.0, 740.0, 290.0, 780.0, 0, false),
        node(RegionKind::Text, 310.0, 690.0, 550.0, 730.0, 1, false),
        node(RegionKind::Figure, 50.0, 400.0, 550.0, 600.0, 2, false),
    ];
    let mut a = nodes.clone();
    let mut b = nodes;
    assign_columns(&mut a, 12.0);
    assign_columns(&mut b, 12.0);
    let oa = reading_order(&a, false, 12.0);
    let ob = reading_order(&b, false, 12.0);
    assert_eq!(oa, ob, "deterministic");
    let mut sorted = oa.clone();
    sorted.sort();
    assert_eq!(sorted, vec![0, 1, 2], "permutation of 0..n");
}

#[test]
fn o10_empty_and_single() {
    assert_eq!(reading_order(&[], false, 12.0), Vec::<usize>::new());
    let one = vec![node(RegionKind::Text, 0.0, 0.0, 10.0, 10.0, 0, false)];
    assert_eq!(order(one, false, 12.0), vec![0]);
}

// ── classification ────────────────────────────────────────────────────────────

fn lblock(text: &str, x0: f64, y0: f64, x1: f64, y1: f64, fs: f64) -> LayoutBlock {
    let lines: Vec<LayoutLine> = text
        .split('\n')
        .map(|t| LayoutLine {
            text: t.to_string(),
            bbox: bx(x0, y0, x1, y1),
            is_rtl: false,
        })
        .collect();
    LayoutBlock {
        bbox: bx(x0, y0, x1, y1),
        lines,
        font_size: fs,
    }
}

fn stats(body: f64, tiers: Vec<f64>) -> DocStats {
    DocStats {
        body_size: body,
        heading_tiers: tiers,
        median_line_height: body,
        column_width: 240.0,
    }
}

fn feat(
    block: &LayoutBlock,
    doc: &DocStats,
    bold: bool,
    italic: bool,
    gap_above: f64,
) -> BlockFeatures {
    let first = block
        .lines
        .first()
        .map(|l| l.text.clone())
        .unwrap_or_default();
    let last = block
        .lines
        .last()
        .map(|l| l.text.clone())
        .unwrap_or_default();
    let wc: usize = block
        .lines
        .iter()
        .map(|l| l.text.split_whitespace().count())
        .sum();
    BlockFeatures {
        font_size: block.font_size,
        size_ratio: block.font_size / doc.body_size,
        is_bold: bold,
        is_italic: italic,
        line_count: block.lines.len(),
        word_count: wc,
        first_line_text: first,
        fill_ratio: (block.bbox.width() / doc.column_width).min(1.0),
        ends_with_sentence_punct: ends_sentence(&last),
        gap_above,
    }
}

#[test]
fn c1_large_bold_is_heading_level1() {
    let doc = stats(10.0, vec![20.0, 14.0]);
    let b = lblock("Big Section Title", 50.0, 740.0, 300.0, 760.0, 20.0);
    let f = feat(&b, &doc, true, false, 30.0);
    let (ty, conf, basis) = classify_block(&f, &doc);
    assert_eq!(ty, ClassifiedType::Heading { level: 1 });
    assert!(conf >= CONF_FLOOR);
    assert!(basis.iter().any(|s| s.starts_with("size:")));
    assert!(basis.iter().any(|s| s == "bold"));
}

#[test]
fn c2_body_size_bold_numbered_is_heading() {
    // No size cue (body-size), but bold + numbered short line => heading at the
    // lowest level.
    let doc = stats(10.0, vec![20.0, 14.0]); // two tiers => lowest level = 3
    let b = lblock("3. Methods", 50.0, 700.0, 160.0, 712.0, 10.0);
    let f = feat(&b, &doc, true, false, 25.0);
    let (ty, _conf, basis) = classify_block(&f, &doc);
    match ty {
        ClassifiedType::Heading { level } => assert_eq!(level, 3, "lowest level"),
        other => panic!("expected heading, got {other:?}"),
    }
    assert!(basis.iter().any(|s| s == "numbered"));
    assert!(basis.iter().any(|s| s == "bold"));
}

#[test]
fn c3_justified_prose_is_paragraph() {
    let doc = stats(10.0, vec![20.0]);
    let b = lblock(
        "This is a long body paragraph that fills the column width and ends with a period.\nSecond line continues the sentence and also reads as ordinary prose here.",
        50.0,
        600.0,
        290.0,
        640.0,
        10.0,
    );
    let f = feat(&b, &doc, false, false, 12.0);
    let (ty, conf, _) = classify_block(&f, &doc);
    assert_eq!(ty, ClassifiedType::Paragraph);
    assert!(conf >= 0.7, "prose confidence {conf}");
}

#[test]
fn c9_odd_fragment_is_text_fallback() {
    // A tiny odd-size single word that isn't clearly heading or prose.
    let doc = stats(10.0, vec![20.0]);
    let mut b = lblock("xq", 50.0, 500.0, 60.0, 507.0, 7.0);
    b.font_size = 7.0;
    let mut f = feat(&b, &doc, false, false, 0.5);
    f.fill_ratio = 0.95; // not short-fill
    let (ty, _conf, basis) = classify_block(&f, &doc);
    assert_eq!(ty, ClassifiedType::Text);
    assert!(basis.iter().any(|s| s.contains("low-confidence")));
}

// ── prefix scanners ───────────────────────────────────────────────────────────

#[test]
fn scanner_bullets_and_enumerators() {
    assert!(is_bullet_marker("\u{2022} first item"));
    assert!(is_bullet_marker("- dash item"));
    assert!(is_bullet_marker("* star item"));
    assert!(!is_bullet_marker("-no-space-hyphenated"));
    assert_eq!(enum_marker("1. one"), Some(true));
    assert_eq!(enum_marker("a) alpha"), Some(true));
    assert_eq!(enum_marker("(iv) roman"), Some(true));
    assert_eq!(enum_marker("[12] bracketed"), Some(true));
    assert_eq!(enum_marker("hello world"), None);
}

#[test]
fn scanner_section_and_caption_and_pagenum() {
    assert!(is_section_numbered("1.2.3 Subsection"));
    assert!(is_section_numbered("Chapter 4"));
    assert!(!is_section_numbered("Hello world"));
    assert!(is_caption_prefixed("Figure 1: Revenue"));
    assert!(is_caption_prefixed("Table 3 — results"));
    assert!(!is_caption_prefixed("Figures show that"));
    assert_eq!(page_number_value("12"), Some(12));
    assert_eq!(page_number_value("Page 7"), Some(7));
    assert_eq!(page_number_value("iv"), Some(4));
    assert_eq!(page_number_value("3 / 10"), Some(3));
    assert_eq!(page_number_value("hello"), None);
}

// ── list grouping ──────────────────────────────────────────────────────────────

#[test]
fn c4_bulleted_block_becomes_list() {
    let mut block = LayoutBlock {
        bbox: bx(50.0, 600.0, 290.0, 660.0),
        lines: Vec::new(),
        font_size: 10.0,
    };
    for (i, t) in ["\u{2022} apples", "\u{2022} oranges", "\u{2022} pears"]
        .iter()
        .enumerate()
    {
        let y = 650.0 - i as f64 * 16.0;
        block.lines.push(LayoutLine {
            text: t.to_string(),
            bbox: bx(50.0, y, 200.0, y + 12.0),
            is_rtl: false,
        });
    }
    let list = try_group_list(&block, 1, 12.0).expect("should group as list");
    assert!(!list.ordered);
    assert_eq!(list.items.len(), 3);
}

#[test]
fn c5_enumerated_list_with_continuation() {
    let mut block = LayoutBlock {
        bbox: bx(50.0, 560.0, 290.0, 660.0),
        lines: Vec::new(),
        font_size: 10.0,
    };
    // item 1 spans 2 lines (continuation), items 2 and 3 single.
    let rows = [
        ("1) first item that wraps", 650.0, 50.0),
        ("onto a second line", 638.0, 60.0),
        ("2) second item", 620.0, 50.0),
        ("3) third item", 604.0, 50.0),
    ];
    for (t, y, x) in rows {
        block.lines.push(LayoutLine {
            text: t.to_string(),
            bbox: bx(x, y, 250.0, y + 12.0),
            is_rtl: false,
        });
    }
    let list = try_group_list(&block, 1, 12.0).expect("should group");
    assert!(list.ordered);
    assert_eq!(list.items.len(), 3);
    assert!(
        list.items[0].text.contains("second line"),
        "continuation merged"
    );
}

#[test]
fn c10_lone_numbered_line_is_not_a_list() {
    let block = lblock("1. Introduction", 50.0, 700.0, 200.0, 712.0, 10.0);
    assert!(
        try_group_list(&block, 1, 12.0).is_none(),
        "single line is not a list"
    );
}

// ── finer line→block segmentation: list-marker transition splits ───────────────

fn seg(text: &str, x0: f64, y: f64) -> SegLine {
    // A 12pt-tall line; bbox y-up so top = y+12.
    SegLine {
        text: text.to_string(),
        bbox: bx(x0, y, 290.0, y + 12.0),
        font_size: 10.0,
        is_rtl: false,
    }
}

#[test]
fn c11_intro_list_outro_split_into_three_blocks() {
    // Intro paragraph, a bullet list, and a trailing paragraph at NORMAL leading
    // (no big gaps) must become three blocks via the marker-transition break, so
    // the list is groupable and the paragraphs are not swallowed into it.
    let line_h = 12.0;
    let lines = vec![
        seg("Intro paragraph before the list.", 50.0, 700.0),
        seg("\u{2022} first item", 50.0, 684.0),
        seg("\u{2022} second item", 50.0, 668.0),
        seg("\u{2022} third item", 50.0, 652.0),
        seg("Outro paragraph after the list.", 50.0, 636.0),
    ];
    let blocks = lines_to_blocks(lines, line_h);
    assert_eq!(blocks.len(), 3, "intro | list | outro => 3 blocks");
    assert!(blocks[0].lines[0].text.starts_with("Intro"));
    // middle block is the bullet run and groups as a list
    let list = try_group_list(&blocks[1], 1, line_h).expect("middle block groups as a list");
    assert_eq!(list.items.len(), 3);
    assert!(blocks[2].lines[0].text.starts_with("Outro"));
}

#[test]
fn c12_wrapped_list_item_continuation_stays_in_list_block() {
    // A wrapped (hanging-indent) continuation line is indented further right than
    // the marker, so it must NOT split the list block.
    let line_h = 12.0;
    let lines = vec![
        seg("\u{2022} first item that wraps", 50.0, 700.0),
        seg("onto a second indented line", 70.0, 684.0), // indented continuation
        seg("\u{2022} second item", 50.0, 668.0),
    ];
    let blocks = lines_to_blocks(lines, line_h);
    assert_eq!(
        blocks.len(),
        1,
        "wrapped continuation stays in the one list block"
    );
    let list = try_group_list(&blocks[0], 1, line_h).expect("groups as a list");
    assert_eq!(list.items.len(), 2);
    assert!(
        list.items[0].text.contains("second indented line"),
        "continuation merged"
    );
}

// ── figures + caption linkage ────────────────────────────────────────────────

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    Rect { x0, y0, x1, y1 }
}

#[test]
fn f6_adjacent_image_tiles_merge_order_independent() {
    let a = rect(50.0, 600.0, 150.0, 700.0);
    let b = rect(150.0, 600.0, 250.0, 700.0); // touching right edge
    let m1 = merge_image_rects(&[a, b], 12.0);
    let m2 = merge_image_rects(&[b, a], 12.0);
    assert_eq!(m1.len(), 1, "two adjacent tiles merge");
    assert_eq!(m2.len(), 1);
    assert_eq!(
        bbox_to_array(&m1[0]),
        bbox_to_array(&m2[0]),
        "order independent"
    );
    assert!((m1[0].x0 - 50.0).abs() < 0.1 && (m1[0].x1 - 250.0).abs() < 0.1);
}

#[test]
fn f3_caption_links_to_prefixed_block_below_figure() {
    let mut blocks = vec![
        // figure
        DocBlock {
            id: 0,
            classified: ClassifiedType::Figure,
            page: 1,
            bbox: [50.0, 500.0, 300.0, 700.0],
            reading_order_index: 0,
            text: String::new(),
            confidence: 0.75,
            basis: vec![],
            items: vec![],
            caption_id: None,
            figure_id: None,
            header_footer: false,
            page_number: false,
            is_bold: false,
            is_italic: false,
            table: None,
        },
        // caption just below (within K_BELOW * line_h)
        DocBlock {
            id: 1,
            classified: ClassifiedType::Paragraph,
            page: 1,
            bbox: [50.0, 480.0, 300.0, 495.0],
            reading_order_index: 1,
            text: "Figure 1: revenue by quarter".into(),
            confidence: 0.6,
            basis: vec![],
            items: vec![],
            caption_id: None,
            figure_id: None,
            header_footer: false,
            page_number: false,
            is_bold: false,
            is_italic: false,
            table: None,
        },
        // far body block (should NOT be chosen)
        DocBlock {
            id: 2,
            classified: ClassifiedType::Paragraph,
            page: 1,
            bbox: [50.0, 100.0, 300.0, 160.0],
            reading_order_index: 2,
            text: "Some unrelated body text far below the figure.".into(),
            confidence: 0.8,
            basis: vec![],
            items: vec![],
            caption_id: None,
            figure_id: None,
            header_footer: false,
            page_number: false,
            is_bold: false,
            is_italic: false,
            table: None,
        },
    ];
    link_captions(&mut blocks, 12.0);
    assert_eq!(blocks[1].classified, ClassifiedType::Caption);
    assert_eq!(blocks[1].figure_id, Some(0));
    assert_eq!(blocks[0].caption_id, Some(1));
    assert_eq!(
        blocks[2].classified,
        ClassifiedType::Paragraph,
        "far block untouched"
    );
}

// ── running header/footer/page-number ─────────────────────────────────────────

fn running_block(id: usize, page: usize, text: &str, y0: f64, y1: f64) -> DocBlock {
    DocBlock {
        id,
        classified: ClassifiedType::Paragraph,
        page,
        bbox: [50.0, y0, 200.0, y1],
        reading_order_index: id,
        text: text.into(),
        confidence: 0.6,
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

#[test]
fn h1_repeated_top_text_becomes_header() {
    let mut dims = BTreeMap::new();
    let mut blocks = Vec::new();
    for p in 1..=4usize {
        dims.insert(p, (612.0, 792.0));
        // top band: y near 760 (>= 792*0.88 = 697)
        blocks.push(running_block(
            p - 1,
            p,
            "ACME Quarterly Report",
            760.0,
            775.0,
        ));
    }
    detect_running_elements(&mut blocks, &dims, 4);
    for b in &blocks {
        assert_eq!(b.classified, ClassifiedType::Header, "page {}", b.page);
        assert!(b.header_footer);
        assert!(b.basis.iter().any(|s| s.contains("running:4of4")));
    }
}

#[test]
fn h2_incrementing_footer_numbers_become_pagenumber() {
    let mut dims = BTreeMap::new();
    let mut blocks = Vec::new();
    for p in 1..=4usize {
        dims.insert(p, (612.0, 792.0));
        // bottom band: y near 30 (<= 792*0.12 = 95)
        blocks.push(running_block(p - 1, p, &format!("{p}"), 20.0, 32.0));
    }
    detect_running_elements(&mut blocks, &dims, 4);
    for b in &blocks {
        assert_eq!(b.classified, ClassifiedType::PageNumber, "page {}", b.page);
        assert!(b.page_number);
        assert!(b.basis.iter().any(|s| s.contains("pagenum:sequence")));
    }
}

#[test]
fn h3_unknown_geometry_blocks_skipped() {
    // tagged-style blocks with [0;4] bbox must not be classified as running.
    let mut dims = BTreeMap::new();
    dims.insert(1usize, (612.0, 792.0));
    dims.insert(2usize, (612.0, 792.0));
    let mut blocks = vec![
        DocBlock {
            id: 0,
            classified: ClassifiedType::Paragraph,
            page: 1,
            bbox: [0.0; 4],
            reading_order_index: 0,
            text: "Repeated".into(),
            confidence: 0.6,
            basis: vec![],
            items: vec![],
            caption_id: None,
            figure_id: None,
            header_footer: false,
            page_number: false,
            is_bold: false,
            is_italic: false,
            table: None,
        },
        DocBlock {
            id: 1,
            classified: ClassifiedType::Paragraph,
            page: 2,
            bbox: [0.0; 4],
            reading_order_index: 1,
            text: "Repeated".into(),
            confidence: 0.6,
            basis: vec![],
            items: vec![],
            caption_id: None,
            figure_id: None,
            header_footer: false,
            page_number: false,
            is_bold: false,
            is_italic: false,
            table: None,
        },
    ];
    detect_running_elements(&mut blocks, &dims, 2);
    assert!(blocks.iter().all(|b| !b.header_footer));
}

// ── page rotation normalization ───────────────────────────────────────────────

#[test]
fn rot_90_maps_corners_into_upright_space() {
    // Original page 600w × 800h (portrait content), displayed /Rotate 90 → upright
    // page is 800w × 600h. A point at the original top-left-ish (x=0, y=800) must
    // land at upright (800, 600) per (u,v)→(v, w0-u): (800, 600-0)=(800,600).
    let rot = PageRotation::new(90, [0.0, 0.0, 600.0, 800.0], 800.0, 600.0);
    assert_eq!(rot.point(0.0, 800.0), (800.0, 600.0));
    // Origin (0,0) → (0, 600).
    assert_eq!(rot.point(0.0, 0.0), (0.0, 600.0));
    // The far corner (600,800) → (800, 0).
    assert_eq!(rot.point(600.0, 800.0), (800.0, 0.0));
}

#[test]
fn rot_180_flips_both_axes() {
    let rot = PageRotation::new(180, [0.0, 0.0, 600.0, 800.0], 600.0, 800.0);
    assert_eq!(rot.point(0.0, 0.0), (600.0, 800.0));
    assert_eq!(rot.point(600.0, 800.0), (0.0, 0.0));
    assert_eq!(rot.point(150.0, 200.0), (450.0, 600.0));
}

#[test]
fn rot_270_maps_corners() {
    let rot = PageRotation::new(270, [0.0, 0.0, 600.0, 800.0], 800.0, 600.0);
    // (u,v)→(h0-v, u) with h0=800.
    assert_eq!(rot.point(0.0, 0.0), (800.0, 0.0));
    assert_eq!(rot.point(600.0, 800.0), (0.0, 600.0));
}

#[test]
fn rot_chunk_90_swaps_width_and_height_into_upright_run() {
    // A horizontal run on a /Rotate 90 page: width 100, font 10, anchored at
    // (50, 400). After normalization it reads horizontally in upright space; its
    // upright width should come from the original run width (100), and its upright
    // height (font_size) from the original glyph height (10).
    let rot = PageRotation::new(90, [0.0, 0.0, 600.0, 800.0], 800.0, 600.0);
    let mut c = TextChunk {
        text: "hello".into(),
        x: 50.0,
        y: 400.0,
        font_size: 10.0,
        font_name: "F1".into(),
        width: 100.0,
        is_rtl: false,
        is_vertical: false,
        is_invisible: false,
    };
    rot.rotate_chunk(&mut c);
    // Box corners (50,400)→(400,750) and (150,410)→(410,650) give a hull of
    // 10 wide × 100 tall. The run length (100 = the original width, the longer
    // side) becomes the upright advance/width; the glyph height (10 = original
    // font_size, the shorter side) stays the upright font_size — so a rotated
    // horizontal run looks like an ordinary upright run to the segmenter.
    assert!(
        (c.width - 100.0).abs() < 1e-6,
        "upright width = run length: {}",
        c.width
    );
    assert!(
        (c.font_size - 10.0).abs() < 1e-6,
        "upright font_size = glyph height: {}",
        c.font_size
    );
}

// The serde-shape (D-Serde: `#[serde(flatten)]` over the internally-tagged
// `ClassifiedType`) assertion lives in the CLI integration test
// (`crates/cli/tests/`), where `serde_json` is already a dependency — adding it
// to the engine's dev-deps introduces an unrelated `PartialEq` ambiguity.
