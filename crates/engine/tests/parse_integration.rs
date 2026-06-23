//! End-to-end smoke for the canonical-model `parse` entry point.
//!
//! PARSER PIVOT prompt 1, Part E.3: run `parse` over a curated set spanning a
//! tagged PDF, an untagged single-column doc, a doc with a figure + caption, and
//! a doc with running furniture; serialize to Markdown + JSON + HTML; and assert
//! the structure is faithful and the output is coherent/LLM-ready end-to-end.
//!
//! The fixtures are deterministic synthetic PDFs (same builder shape as
//! `document_model.rs`) whose correct structure is known by construction, so the
//! assertions are exact rather than eyeballed. No full renderer benchmark.

use oxide_engine::{ContentEngine, PageSource, ParseOptions, SerializeOptions, SourceInfo};

// ════════════════════════════════════════════════════════════════════════════
// Minimal PDF builder (shared shape with the rest of the engine test suite)
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

fn text(font: &str, size: f64, x: f64, y: f64, s: &str) -> String {
    format!("BT /{font} {size} Tf 1 0 0 1 {x:.1} {y:.1} Tm ({s}) Tj ET\n")
}

fn one_page(font_resources: &str, content: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(&format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << {font_resources} >> >> /Contents 4 0 R >>"
    ));
    b.add_stream("", content);
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    b.build()
}

// ════════════════════════════════════════════════════════════════════════════
// Fixtures
// ════════════════════════════════════════════════════════════════════════════

/// Single column: a large bold heading then a body paragraph — the digital-born
/// (geometric) path.
fn heading_then_paragraph() -> Vec<u8> {
    let mut c = String::new();
    c.push_str(&text("FB", 18.0, 72.0, 740.0, "Introduction Heading"));
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

/// Three pages repeating a top banner + an incrementing bottom page number, with
/// distinct body text per page — exercises furniture detection + omission.
fn running_furniture() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
    for content_obj in [8, 9, 10] {
        b.add(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 6 0 R /FB 7 0 R >> >> /Contents {content_obj} 0 R >>"
        ));
    }
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
    for page in 1..=3usize {
        let mut c = String::new();
        c.push_str(&text("F1", 9.0, 72.0, 765.0, "ACME Annual Report 2026"));
        c.push_str(&text(
            "F1",
            10.0,
            72.0,
            420.0,
            &format!("Body content unique to page {page} describing the section."),
        ));
        c.push_str(&text("F1", 9.0, 300.0, 30.0, &format!("{page}")));
        b.add_stream("", c.as_bytes());
    }
    b.build()
}

/// An image figure with a "Figure 1: ..." caption directly below — figure +
/// caption linkage.
fn figure_with_caption() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R /FB 6 0 R >> /XObject << /Im0 7 0 R >> >> \
         /Contents 4 0 R >>",
    );
    let mut c = String::new();
    c.push_str("q 300 0 0 200 72 480 cm /Im0 Do Q\n");
    c.push_str(&text(
        "F1",
        9.0,
        72.0,
        462.0,
        "Figure 1: revenue chart by quarter",
    ));
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
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.build()
}

/// A tagged PDF (H1 + two paragraphs + a list) — the tags-first path.
fn tagged_pdf() -> Vec<u8> {
    let mcid_text = |mcid: i64, tag: &str, x: f64, y: f64, s: &str| -> String {
        format!(
            "/{tag} <</MCID {mcid}>> BDC\nBT /F1 10 Tf 1 0 0 1 {x:.1} {y:.1} Tm ({s}) Tj ET\nEMC\n"
        )
    };
    let mut content = String::new();
    content.push_str(&mcid_text(0, "H1", 72.0, 740.0, "Tagged Document Title"));
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
    b.add("<< /Type /StructTreeRoot /K [7 0 R 8 0 R 9 0 R 10 0 R 13 0 R] >>");
    b.add("<< /Type /StructElem /S /H1 /P 6 0 R /Pg 3 0 R /K 0 >>");
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 2 >>");
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 1 >>");
    b.add("<< /Type /StructElem /S /L /P 6 0 R /K [11 0 R 12 0 R] >>");
    b.add("<< /Type /StructElem /S /LI /P 10 0 R /Pg 3 0 R /K 3 >>");
    b.add("<< /Type /StructElem /S /LI /P 10 0 R /Pg 3 0 R /K 4 >>");
    b.add("<< /Type /StructElem /S /Figure /P 6 0 R /Pg 3 0 R /Alt (A bar chart) >>");
    b.build()
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn p1_digital_born_heading_and_paragraph_to_markdown() {
    let engine = ContentEngine::open_bytes(heading_then_paragraph()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();

    assert_eq!(doc.source, SourceInfo::DigitalBorn);
    assert_eq!(doc.schema_version, "1.1");
    assert_eq!(doc.metadata.page_count, 1);
    assert_eq!(doc.pages.len(), 1);
    assert!(doc.pages[0].width > 0.0 && doc.pages[0].height > 0.0);

    let md = doc.to_markdown(&SerializeOptions::default());
    // Heading rendered as a Markdown heading; body present as prose.
    assert!(md.contains("Introduction Heading"), "md:\n{md}");
    assert!(
        md.lines()
            .any(|l| l.starts_with('#') && l.contains("Introduction")),
        "md:\n{md}"
    );
    assert!(md.contains("ordinary running prose"), "md:\n{md}");
}

#[test]
fn p2_tagged_path_uses_authored_structure() {
    let engine = ContentEngine::open_bytes(tagged_pdf()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();

    assert_eq!(doc.source, SourceInfo::Tagged);
    assert!(doc.metadata.is_tagged);

    let md = doc.to_markdown(&SerializeOptions::default());
    // The H1 became a top-level Markdown heading.
    assert!(md.contains("# Tagged Document Title"), "md:\n{md}");
    // Authored order: left column paragraph precedes right column paragraph.
    let left = md.find("Left column").expect("left present");
    let right = md.find("Right column").expect("right present");
    assert!(left < right, "authored order (L before R) preserved:\n{md}");
    // The figure's alt text surfaced.
    assert!(md.contains("A bar chart"), "figure alt in md:\n{md}");
}

#[test]
fn p3_furniture_omitted_by_default_kept_in_pages() {
    let engine = ContentEngine::open_bytes(running_furniture()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    assert_eq!(doc.pages.len(), 3);

    let md = doc.to_markdown(&SerializeOptions::default());
    // The repeated banner is furniture → stripped from the body/markdown.
    assert!(
        !md.contains("ACME Annual Report"),
        "furniture stripped:\n{md}"
    );
    // Body content per page survives.
    assert!(md.contains("unique to page 1"), "md:\n{md}");

    // But the page view still references the furniture blocks (lossless).
    let total_in_pages: usize = doc.pages.iter().map(|p| p.block_ids.len()).sum();
    assert!(
        total_in_pages > doc.body.len(),
        "page view retains more blocks (incl. furniture) than the body"
    );
}

#[test]
fn p4_figure_and_caption_linked_and_rendered_once() {
    let engine = ContentEngine::open_bytes(figure_with_caption()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();

    let html = doc.to_html(&SerializeOptions::default());
    assert!(html.contains("<figure>"), "html:\n{html}");
    assert!(
        html.contains("<figcaption>"),
        "caption linked under figure:\n{html}"
    );
    assert!(html.contains("Figure 1: revenue chart"), "html:\n{html}");

    let md = doc.to_markdown(&SerializeOptions::default());
    // The caption text appears exactly once (under the figure, not duplicated).
    assert_eq!(
        md.matches("Figure 1: revenue chart").count(),
        1,
        "md:\n{md}"
    );
}

#[test]
fn p5_json_is_faithful_and_roundtrips() {
    let engine = ContentEngine::open_bytes(heading_then_paragraph()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    let json = doc.to_json();
    assert!(json.contains("\"schema_version\": \"1.1\""));
    assert!(json.contains("\"source\""));
    assert!(json.contains("\"metadata\""));
    assert!(json.contains("\"pages\""));
    assert!(json.contains("\"body\""));
    // Roundtrip back to an equal model (public-contract guarantee).
    let back: oxide_engine::Document = serde_json::from_str(&json).expect("roundtrip");
    assert_eq!(back, doc);
}

#[test]
fn p6_serialization_is_deterministic_across_runs() {
    let bytes = tagged_pdf();
    let run = || {
        let engine = ContentEngine::open_bytes(bytes.clone()).unwrap();
        let doc = engine.parse_document(&ParseOptions::default()).unwrap();
        (
            doc.to_json(),
            doc.to_markdown(&SerializeOptions::default()),
            doc.to_html(&SerializeOptions::default()),
        )
    };
    assert_eq!(run(), run(), "same PDF → byte-identical serialization");
}

// ════════════════════════════════════════════════════════════════════════════
// Prompt 2: classifier + robustness fixtures
// ════════════════════════════════════════════════════════════════════════════

/// A pure-scan page: a full-page image XObject and NO text. The classifier must
/// route this to `Scanned`; the parser emits a placeholder (note + figure).
fn scanned_page() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", b"q 612 0 0 792 0 0 cm /Im0 Do Q\n");
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.build()
}

/// A searchable scan: a full-page image PLUS an invisible (Tr 3) text layer the
/// producer already OCR'd. The classifier must route this to
/// `DigitalBornOverImage` and the parser must USE the text layer (not re-OCR).
fn searchable_scan() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 6 0 R >> /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    c.push_str("q 612 0 0 792 0 0 cm /Im0 Do Q\n");
    c.push_str("BT 3 Tr /F1 10 Tf 1 0 0 1 72 720 Tm (Invisible OCR layer text recovered from scan) Tj ET\n");
    c.push_str("BT 3 Tr /F1 10 Tf 1 0 0 1 72 700 Tm (A second line of the existing searchable layer here) Tj ET\n");
    b.add_stream("", c.as_bytes());
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.build()
}

/// A page with /Rotate 90. Content is authored in unrotated user space; the
/// parser must normalize coordinates and still recover and order the text.
fn rotated_page() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 \
         /Resources << /Font << /F1 5 0 R /FB 6 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    c.push_str(&text("FB", 18.0, 72.0, 740.0, "Rotated Heading"));
    c.push_str(&text(
        "F1",
        10.0,
        72.0,
        712.0,
        "Body text on a page that is rotated ninety degrees.",
    ));
    c.push_str(&text(
        "F1",
        10.0,
        72.0,
        698.0,
        "It must still extract and read in the right order.",
    ));
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
    b.build()
}

/// A page with a `/Link` annotation (URI action) over a line of text.
fn page_with_link() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R /Annots [6 0 R] >>",
    );
    let c = text("F1", 12.0, 72.0, 700.0, "Visit the Oxide project page");
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add(
        "<< /Type /Annot /Subtype /Link /Rect [72 698 300 714] \
         /A << /S /URI /URI (https://example.com/oxide) >> >>",
    );
    b.build()
}

#[test]
fn p7_pure_scan_routes_to_scanned_and_runs_end_to_end() {
    let engine = ContentEngine::open_bytes(scanned_page()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    assert_eq!(doc.pages.len(), 1);
    assert_eq!(doc.pages[0].source, PageSource::Scanned);
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("scanned page 1"), "scanned note present:\n{md}");
    let json = doc.to_json();
    assert!(json.contains("\"scanned\""), "json records scanned source");
}

#[test]
fn p8_searchable_scan_uses_existing_text_layer_not_ocr() {
    let engine = ContentEngine::open_bytes(searchable_scan()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    assert_eq!(doc.pages[0].source, PageSource::DigitalBornOverImage);
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(
        md.contains("Invisible OCR layer text"),
        "uses text layer:\n{md}"
    );
    assert!(
        !md.contains("scanned page"),
        "must not be treated as a pure scan:\n{md}"
    );
}

#[test]
fn p9_rotated_page_text_recovered_in_order() {
    let engine = ContentEngine::open_bytes(rotated_page()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    assert_eq!(doc.pages[0].source, PageSource::DigitalBorn);
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("Rotated Heading"), "heading recovered:\n{md}");
    assert!(
        md.contains("rotated ninety degrees"),
        "body recovered:\n{md}"
    );
    let h = md.find("Rotated Heading").unwrap();
    let body = md.find("rotated ninety degrees").unwrap();
    assert!(h < body, "heading before body:\n{md}");
}

#[test]
fn p10_link_annotation_attached_to_text() {
    let engine = ContentEngine::open_bytes(page_with_link()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(
        md.contains("](https://example.com/oxide)"),
        "link survives into markdown:\n{md}"
    );
}

#[test]
fn p11_mixed_document_per_page_routing() {
    // 2-page doc: page 1 digital-born text, page 2 a pure scan → Mixed rollup.
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>",
    );
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /XObject << /Im0 8 0 R >> >> /Contents 6 0 R >>",
    );
    b.add_stream(
        "",
        text(
            "F1",
            12.0,
            72.0,
            700.0,
            "This is a born-digital first page with real selectable text content.",
        )
        .as_bytes(),
    );
    b.add_stream("", b"q 612 0 0 792 0 0 cm /Im0 Do Q\n");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    let engine = ContentEngine::open_bytes(b.build()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    assert_eq!(doc.pages.len(), 2);
    assert_eq!(doc.pages[0].source, PageSource::DigitalBorn);
    assert_eq!(doc.pages[1].source, PageSource::Scanned);
    assert_eq!(doc.source, SourceInfo::Mixed, "document rolls up to Mixed");
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("born-digital first page"), "page 1 text:\n{md}");
    assert!(md.contains("scanned page 2"), "page 2 scanned note:\n{md}");
}
