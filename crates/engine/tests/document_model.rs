//! End-to-end validation of the typed, ordered **document model**
//! (`engine.build_document_model`) on real PDF bytes.
//!
//! The `docmodel` unit tests (`crates/engine/src/docmodel/tests.rs`) exercise
//! the internals with synthetic inputs; this suite drives whole PDFs through the
//! public entry point so the full pipeline is covered:
//!   chunks → fine segmentation → columns → classify → precedence-graph order
//!         → list grouping → figure/caption linkage → running header/footer.
//!
//! Each test encodes a Part-D.1 unit case, end-to-end through real PDF bytes:
//!   - a spanning title over stacked paragraphs → title first, then top-to-bottom;
//!   - a heading vs body distinguished by font size → labelled heading;
//!   - a bulleted block → a list with items;
//!   - an image with "Figure 1" below → figure + caption linked;
//!   - a repeating top-margin block across pages → header; incrementing footer
//!     numbers → page numbers;
//!   - the tagged-PDF tags-first path → authored order (left before right despite
//!     stream order) + roles;
//!   - an RTL column (via a `/ToUnicode` CMap) → ordered top-to-bottom, text kept;
//!   - determinism: same PDF → identical model.
//!
//! Note on multi-column *text*: a borderless two-column text body is
//! geometrically indistinguishable from a borderless table and is claimed by the
//! table detector before the ordering layer runs, so the column-precedence claim
//! is validated on geometry directly in the `docmodel` unit tests (`o1`, `o5`,
//! `o7`) and end-to-end through the *tagged* path (`dm7`), not on synthetic
//! gridded text here.

use oxide_engine::{ClassifiedType, ContentEngine, DocBlock, DocumentModel, ModelSource};

// ════════════════════════════════════════════════════════════════════════════
// Minimal PDF builder (mirrors crates/engine/tests/{layout_analysis,
// semantic_extraction}.rs so fixtures read the same across the suite).
// ════════════════════════════════════════════════════════════════════════════

struct PdfBuilder {
    objects: Vec<Vec<u8>>,
}

impl PdfBuilder {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }
    fn add(&mut self, body: &str) -> usize {
        self.objects.push(body.as_bytes().to_vec());
        self.objects.len()
    }
    fn add_stream(&mut self, dict_extra: &str, stream: &[u8]) -> usize {
        let mut body =
            format!("<< /Length {} {} >>\nstream\n", stream.len(), dict_extra).into_bytes();
        body.extend_from_slice(stream);
        body.extend_from_slice(b"\nendstream");
        self.objects.push(body);
        self.objects.len()
    }
    fn build(&self) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                offsets.len() + 1,
                xref_start
            )
            .as_bytes(),
        );
        pdf
    }
}

/// A text run placing `text` at absolute (x, y) at `size`pt using font resource
/// `font` (the resource *name* is what the collector reports as `font_name`, so
/// a name containing "Bold" makes the docmodel see the block as bold).
fn text(font: &str, size: f64, x: f64, y: f64, s: &str) -> String {
    format!("BT /{font} {size} Tf 1 0 0 1 {x:.1} {y:.1} Tm ({s}) Tj ET\n")
}

/// Standard single-page scaffold: catalog, pages, one page with the given font
/// resource dict and content stream (raw bytes, so callers can embed single-byte
/// glyph codes like the WinAnsi bullet `0x95`).
fn one_page(font_resources: &str, content: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(&format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << {font_resources} >> >> /Contents 4 0 R >>"
    ));
    b.add_stream("", content);
    // Fonts (objects 5+): Helvetica + Helvetica-Bold, both WinAnsi-encoded (so a
    // single 0x95 byte decodes to a bullet, and a name with "Bold" => is_bold).
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    b.build()
}

/// WinAnsi bullet glyph (`U+2022`) as its single content-stream byte.
const BULLET: u8 = 0x95;

/// Find the in-reading-order index of the first block whose text contains `needle`.
fn order_of(model: &DocumentModel, needle: &str) -> usize {
    model
        .blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .map(|b| b.reading_order_index)
        .unwrap_or_else(|| panic!("no block containing {needle:?} in model:\n{}", dump(model)))
}

fn block_with<'a>(model: &'a DocumentModel, needle: &str) -> &'a DocBlock {
    model
        .blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .unwrap_or_else(|| panic!("no block containing {needle:?} in model:\n{}", dump(model)))
}

fn dump(model: &DocumentModel) -> String {
    let mut s = String::new();
    for b in &model.blocks {
        s.push_str(&format!(
            "  [{}] {:?} conf={:.2} {:?}\n",
            b.reading_order_index,
            b.classified,
            b.confidence,
            b.text.chars().take(40).collect::<String>()
        ));
    }
    s
}

// ════════════════════════════════════════════════════════════════════════════
// Fixtures
// ════════════════════════════════════════════════════════════════════════════

/// A single column with a large bold spanning TITLE on top, then three stacked
/// body paragraphs. The precedence-graph ordering must recover title → p1 → p2 →
/// p3 (strict top-to-bottom). (A *text* two-column body is geometrically
/// indistinguishable from a borderless table and is claimed by the table
/// detector before ordering — multi-column reading order is therefore validated
/// in the `docmodel` unit tests `o1`/`o7` and, end-to-end, via the tagged path
/// `dm7`; see deferrals in the summary.)
fn title_then_paragraphs() -> Vec<u8> {
    let mut c = String::new();
    c.push_str(&text(
        "FB",
        22.0,
        72.0,
        750.0,
        "The Spanning Document Title",
    ));
    let paras = [
        [
            "First paragraph opening line of ordinary prose",
            "that continues onto a second wrapped line below",
            "and finishes with a terminal full stop here.",
        ],
        [
            "Second paragraph begins well beneath the first",
            "with a clear vertical gap separating the two",
            "distinct blocks of running body text on the page.",
        ],
        [
            "Third and final paragraph rounds out the single",
            "column page with more ordinary flowing prose and",
            "ends the document body cleanly at the very last.",
        ],
    ];
    let mut y = 712.0;
    for p in &paras {
        for ln in p {
            c.push_str(&text("F1", 10.0, 72.0, y, ln));
            y -= 13.0;
        }
        y -= 18.0; // paragraph gap
    }
    one_page("/F1 5 0 R /FB 6 0 R", c.as_bytes())
}

/// A single column: a large bold heading line, then a body paragraph.
fn heading_then_paragraph() -> Vec<u8> {
    let mut c = String::new();
    c.push_str(&text("FB", 18.0, 72.0, 740.0, "Introduction Heading"));
    // Body paragraph: several body-size lines that fill the column and end with a
    // period (prose cues).
    let lines = [
        "This opening paragraph is ordinary running prose that fills the column",
        "width and continues over several lines as a real body paragraph would,",
        "ending with a terminal period to read clearly as flowing sentence text.",
    ];
    for (i, ln) in lines.iter().enumerate() {
        let y = 712.0 - i as f64 * 14.0;
        c.push_str(&text("F1", 10.0, 72.0, y, ln));
    }
    one_page("/F1 5 0 R /FB 6 0 R", c.as_bytes())
}

/// A bulleted list: three "• item" lines at a consistent left edge. The bullet
/// is the single WinAnsi byte `0x95` (a multi-byte UTF-8 "•" would decode glyph-
/// by-glyph through the font encoding into garbage, not a bullet).
fn bulleted_list() -> Vec<u8> {
    let mut c: Vec<u8> = Vec::new();
    c.extend_from_slice(text("FB", 16.0, 72.0, 740.0, "Shopping List").as_bytes());
    let items = ["apples and pears", "oranges in season", "fresh whole milk"];
    for (i, it) in items.iter().enumerate() {
        let y = 700.0 - i as f64 * 16.0;
        c.extend_from_slice(format!("BT /F1 10 Tf 1 0 0 1 72 {y:.1} Tm (").as_bytes());
        c.push(BULLET);
        c.extend_from_slice(format!(" {it}) Tj ET\n").as_bytes());
    }
    one_page("/F1 5 0 R /FB 6 0 R", &c)
}

/// An image figure with a "Figure 1: ..." caption directly below it.
fn figure_with_caption() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R /FB 6 0 R >> /XObject << /Im0 7 0 R >> >> \
         /Contents 4 0 R >>",
    );
    // Image is a 300x200 box at (72,480); caption "Figure 1: ..." just below it.
    let mut c = String::new();
    c.push_str("q 300 0 0 200 72 480 cm /Im0 Do Q\n");
    c.push_str(&text(
        "F1",
        9.0,
        72.0,
        462.0,
        "Figure 1: revenue chart by quarter",
    ));
    // Some far-away body text that must NOT be linked as the caption.
    c.push_str(&text(
        "F1",
        10.0,
        72.0,
        120.0,
        "Unrelated body text far below the figure region.",
    ));
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
    // Minimal image XObject (1x1 gray); its bytes never get decoded for the
    // document model — only the /Subtype /Image gate and the Do placement matter.
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.build()
}

/// Three pages that all repeat the same top-margin banner and an incrementing
/// bottom-margin page number, plus distinct body text per page.
fn running_header_and_page_numbers() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    // 3 page objects: 3,4,5 ; shared fonts at 6,7 ; contents at 8,9,10.
    b.add("<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
    for (pi, content_obj) in [(0usize, 8), (1, 9), (2, 10)] {
        let _ = pi;
        b.add(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 6 0 R /FB 7 0 R >> >> /Contents {content_obj} 0 R >>"
        ));
    }
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
    for page in 1..=3usize {
        let mut c = String::new();
        // top-margin banner (y ~ 765, within top 12% band of 792 => >= 697)
        c.push_str(&text("F1", 9.0, 72.0, 765.0, "ACME Annual Report 2026"));
        // distinct body in the middle
        c.push_str(&text(
            "F1",
            10.0,
            72.0,
            420.0,
            &format!("Body content unique to page {page} describing the section."),
        ));
        // bottom-margin page number (y ~ 30, within bottom 12% band => <= 95)
        c.push_str(&text("F1", 9.0, 300.0, 30.0, &format!("{page}")));
        b.add_stream("", c.as_bytes());
    }
    b.build()
}

/// A tagged PDF: H1 title, two columns whose AUTHORED order (left before right)
/// contradicts the interleaved stream order, a list, and a figure.
fn tagged_pdf() -> Vec<u8> {
    let mcid_text = |mcid: i64, tag: &str, x: f64, y: f64, s: &str| -> String {
        format!(
            "/{tag} <</MCID {mcid}>> BDC\nBT /F1 10 Tf 1 0 0 1 {x:.1} {y:.1} Tm ({s}) Tj ET\nEMC\n"
        )
    };
    let mut content = String::new();
    content.push_str(&mcid_text(0, "H1", 72.0, 740.0, "Tagged Document Title"));
    // Stream order: right column emitted before left, but tags author L before R.
    content.push_str(&mcid_text(
        1,
        "P",
        320.0,
        700.0,
        "Right column paragraph text",
    ));
    content.push_str(&mcid_text(
        2,
        "P",
        72.0,
        700.0,
        "Left column paragraph text",
    ));
    content.push_str(&mcid_text(3, "Lbl", 90.0, 670.0, "Alpha"));
    content.push_str(&mcid_text(4, "Lbl", 90.0, 655.0, "Beta"));

    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", content.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    // StructTreeRoot (obj 6) children: H1, P(left), P(right), L, Figure.
    b.add("<< /Type /StructTreeRoot /K [7 0 R 8 0 R 9 0 R 10 0 R 13 0 R] >>");
    b.add("<< /Type /StructElem /S /H1 /P 6 0 R /Pg 3 0 R /K 0 >>");
    // Authored order: left (MCID 2) before right (MCID 1).
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 2 >>");
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 1 >>");
    b.add("<< /Type /StructElem /S /L /P 6 0 R /K [11 0 R 12 0 R] >>");
    b.add("<< /Type /StructElem /S /LI /P 10 0 R /Pg 3 0 R /K 3 >>");
    b.add("<< /Type /StructElem /S /LI /P 10 0 R /Pg 3 0 R /K 4 >>");
    b.add("<< /Type /StructElem /S /Figure /P 6 0 R /Pg 3 0 R /Alt (A bar chart) >>");
    b.build()
}

/// A single RTL column whose font carries a `/ToUnicode` CMap mapping single
/// byte codes to Hebrew letters, so the extracted text is genuinely RTL-dominant
/// (a multi-byte UTF-8 "שלום" in the content string would instead decode glyph-
/// by-glyph through the base encoding into Latin garbage). The model must order
/// the blocks top-to-bottom and preserve the RTL text. (Right-column-before-left
/// RTL *column* traversal is covered by the `docmodel` unit test `o5`.)
fn rtl_single_column() -> Vec<u8> {
    // Content bytes 'A'(0x41)..'F'(0x46) map to Hebrew letters via the ToUnicode
    // CMap below. Each block uses a distinct mapped "word" so the test can locate
    // it: heading = "AB", paragraph 1 = "CD", paragraph 2 = "EF".
    let mut c = String::new();
    c.push_str(&text("F1", 18.0, 380.0, 740.0, "AB AB AB")); // heading
    let mut y = 705.0;
    for line in ["CD CD CD", "CD CD CD"] {
        c.push_str(&text("F1", 10.0, 380.0, y, line));
        y -= 13.0;
    }
    y -= 22.0; // paragraph gap
    for line in ["EF EF EF", "EF EF EF"] {
        c.push_str(&text("F1", 10.0, 380.0, y, line));
        y -= 13.0;
    }

    // ToUnicode CMap: map single-byte codes 'A'(0x41)..'F'(0x46) to Hebrew.
    let cmap = "\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
6 beginbfchar
<41> <05D0>
<42> <05D1>
<43> <05D2>
<44> <05D3>
<45> <05D4>
<46> <05D5>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end";

    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>");
    b.add_stream("", cmap.as_bytes());
    b.build()
}

// ════════════════════════════════════════════════════════════════════════════
// Tests — geometric path
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn dm1_title_then_paragraphs_in_top_to_bottom_order() {
    let engine = ContentEngine::open_bytes(title_then_paragraphs()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();
    assert_eq!(model.source, ModelSource::Geometric);

    // Title reads first, then the three paragraphs strictly top-to-bottom.
    let title = order_of(&model, "Spanning Document Title");
    let p1 = order_of(&model, "First paragraph");
    let p2 = order_of(&model, "Second paragraph");
    let p3 = order_of(&model, "Third and final paragraph");
    assert!(
        title < p1 && p1 < p2 && p2 < p3,
        "reading order must be title, p1, p2, p3:\n{}",
        dump(&model)
    );

    // The title classifies as a heading/title; the bodies as paragraphs.
    let t = block_with(&model, "Spanning Document Title");
    assert!(matches!(
        t.classified,
        ClassifiedType::Heading { .. } | ClassifiedType::Title
    ));
    assert_eq!(
        block_with(&model, "First paragraph").classified,
        ClassifiedType::Paragraph
    );
}

#[test]
fn dm2_heading_by_font_size_is_labelled_heading() {
    let engine = ContentEngine::open_bytes(heading_then_paragraph()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();

    let h = block_with(&model, "Introduction Heading");
    assert!(
        matches!(
            h.classified,
            ClassifiedType::Heading { .. } | ClassifiedType::Title
        ),
        "large bold short line should classify as heading/title, got {:?}:\n{}",
        h.classified,
        dump(&model)
    );
    let p = block_with(&model, "opening paragraph");
    assert_eq!(
        p.classified,
        ClassifiedType::Paragraph,
        "body prose should be a paragraph:\n{}",
        dump(&model)
    );
    // Heading reads before the paragraph.
    assert!(h.reading_order_index < p.reading_order_index);
}

#[test]
fn dm3_bulleted_block_becomes_list() {
    let engine = ContentEngine::open_bytes(bulleted_list()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();

    let list = model
        .blocks
        .iter()
        .find(|b| matches!(b.classified, ClassifiedType::List { .. }))
        .unwrap_or_else(|| panic!("expected a list block:\n{}", dump(&model)));
    assert!(matches!(
        list.classified,
        ClassifiedType::List { ordered: false }
    ));
    assert_eq!(
        list.items.len(),
        3,
        "three bulleted items:\n{}",
        dump(&model)
    );
    assert!(list.items.iter().any(|i| i.text.contains("apples")));
    assert!(list.items.iter().any(|i| i.text.contains("milk")));
}

#[test]
fn dm4_image_with_caption_below_is_figure_and_caption_linked() {
    let engine = ContentEngine::open_bytes(figure_with_caption()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();

    let fig = model
        .blocks
        .iter()
        .find(|b| b.classified == ClassifiedType::Figure)
        .unwrap_or_else(|| panic!("expected a figure block:\n{}", dump(&model)));
    let cap = model
        .blocks
        .iter()
        .find(|b| b.classified == ClassifiedType::Caption)
        .unwrap_or_else(|| panic!("expected a caption block:\n{}", dump(&model)));

    assert!(
        cap.text.contains("Figure 1"),
        "caption text: {:?}",
        cap.text
    );
    assert_eq!(fig.caption_id, Some(cap.id), "figure links its caption");
    assert_eq!(cap.figure_id, Some(fig.id), "caption links its figure");

    // The far body text must remain an ordinary block, never the caption.
    let far = block_with(&model, "Unrelated body text");
    assert_ne!(far.classified, ClassifiedType::Caption);
}

#[test]
fn dm5_repeating_top_block_becomes_header_and_footer_numbers_pagenumber() {
    let engine = ContentEngine::open_bytes(running_header_and_page_numbers()).unwrap();
    let model = engine.build_document_model(&[1, 2, 3]).unwrap();
    assert_eq!(model.page_count, 3);

    // Every "ACME Annual Report" banner is a header.
    let headers: Vec<&DocBlock> = model
        .blocks
        .iter()
        .filter(|b| b.text.contains("ACME Annual Report"))
        .collect();
    assert_eq!(headers.len(), 3, "one banner per page:\n{}", dump(&model));
    for h in &headers {
        assert_eq!(h.classified, ClassifiedType::Header, "banner => header");
        assert!(h.header_footer);
    }

    // The incrementing bottom numbers become page numbers.
    let page_nums: Vec<&DocBlock> = model
        .blocks
        .iter()
        .filter(|b| b.classified == ClassifiedType::PageNumber)
        .collect();
    assert!(
        page_nums.len() >= 2,
        "incrementing footer numbers => page numbers:\n{}",
        dump(&model)
    );
    for pn in &page_nums {
        assert!(pn.page_number);
    }
}

#[test]
fn dm6_rtl_single_column_orders_top_to_bottom_and_preserves_text() {
    let engine = ContentEngine::open_bytes(rtl_single_column()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();

    // Expected ToUnicode decodings: AB→אב, CD→גד, EF→הו (all Hebrew, RTL).
    let heading = "\u{05D0}\u{05D1}"; // אב
    let para1 = "\u{05D2}\u{05D3}"; // גד
    let para2 = "\u{05D4}\u{05D5}"; // הו

    // RTL blocks still read top-to-bottom: heading, then p1, then p2.
    let h = order_of(&model, heading);
    let p1 = order_of(&model, para1);
    let p2 = order_of(&model, para2);
    assert!(
        h < p1 && p1 < p2,
        "RTL column must order top-to-bottom (heading, p1, p2):\n{}",
        dump(&model)
    );

    // The Hebrew (RTL) text survived extraction and lands in the model.
    assert!(
        model.blocks.iter().any(|b| b.text.contains(heading)),
        "RTL text must be preserved in the model:\n{}",
        dump(&model)
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Tests — tagged path (tags-first)
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn dm7_tagged_uses_authored_order_and_roles() {
    let engine = ContentEngine::open_bytes(tagged_pdf()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();
    assert_eq!(
        model.source,
        ModelSource::Tagged,
        "tagged PDF => tags-first"
    );

    // Title is an H1 heading (level 1) or Title.
    let title = block_with(&model, "Tagged Document Title");
    assert!(
        matches!(
            title.classified,
            ClassifiedType::Heading { level: 1 } | ClassifiedType::Title
        ),
        "H1 tag => heading level 1, got {:?}",
        title.classified
    );

    // Authored order wins over stream order: left paragraph before right.
    let left = order_of(&model, "Left column paragraph");
    let right = order_of(&model, "Right column paragraph");
    assert!(
        left < right,
        "tagged reading order (left before right) must win over stream order:\n{}",
        dump(&model)
    );

    // The list tag becomes a list with its two items.
    let list = model
        .blocks
        .iter()
        .find(|b| matches!(b.classified, ClassifiedType::List { .. }))
        .unwrap_or_else(|| panic!("expected a tagged list:\n{}", dump(&model)));
    assert_eq!(list.items.len(), 2, "two LI children:\n{}", dump(&model));

    // The figure tag becomes a figure carrying its alt text.
    let fig = model
        .blocks
        .iter()
        .find(|b| b.classified == ClassifiedType::Figure)
        .unwrap_or_else(|| panic!("expected a tagged figure:\n{}", dump(&model)));
    assert!(
        fig.text.contains("bar chart"),
        "figure alt text: {:?}",
        fig.text
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Cross-cutting: determinism, invariants, min-confidence shape
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn dm8_determinism_same_pdf_same_model() {
    // Determinism is checked on the full `Debug` rendering of the model (every
    // field, in order) — `serde_json` is not an engine dev-dep, and `Debug` is
    // an equally strict structural fingerprint for "same model".
    let render = |b: &[u8]| -> String {
        let engine = ContentEngine::open_bytes(b.to_vec()).unwrap();
        let model = engine.build_document_model(&[1]).unwrap();
        format!("{model:#?}")
    };
    let bytes = title_then_paragraphs();
    assert_eq!(
        render(&bytes),
        render(&bytes),
        "same PDF must yield an identical document model (geometric path)"
    );

    // And the tagged path too.
    let tbytes = tagged_pdf();
    assert_eq!(
        render(&tbytes),
        render(&tbytes),
        "tagged model must be deterministic"
    );
}

#[test]
fn dm9_reading_order_indices_are_a_dense_permutation() {
    let engine = ContentEngine::open_bytes(title_then_paragraphs()).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();
    let mut idxs: Vec<usize> = model.blocks.iter().map(|b| b.reading_order_index).collect();
    idxs.sort_unstable();
    let expected: Vec<usize> = (0..model.blocks.len()).collect();
    assert_eq!(
        idxs, expected,
        "reading_order_index is a dense 0..n permutation"
    );

    // Every block carries a confidence in [0,1].
    for b in &model.blocks {
        assert!(
            (0.0..=1.0).contains(&b.confidence),
            "confidence in range for {:?}",
            b.classified
        );
    }
}

#[test]
fn dm10_empty_page_yields_empty_model_not_panic() {
    // A page with no text/graphics must produce a valid (empty) model.
    let pdf = one_page("/F1 5 0 R /FB 6 0 R", b"");
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let model = engine.build_document_model(&[1]).unwrap();
    assert_eq!(model.source, ModelSource::Geometric);
    assert!(model.blocks.is_empty(), "no content => no blocks");
    assert_eq!(model.page_count, 1);
}

#[test]
fn dm11_out_of_range_page_errors() {
    let engine = ContentEngine::open_bytes(heading_then_paragraph()).unwrap();
    assert!(
        engine.build_document_model(&[2]).is_err(),
        "page 2 of a 1-page doc errors"
    );
    assert!(engine.build_document_model(&[0]).is_err(), "page 0 errors");
}
