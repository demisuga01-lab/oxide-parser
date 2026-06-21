//! The **OCR seam** test (no Tesseract required).
//!
//! Proves the central architectural claim of the OCR stage: a mock
//! [`OcrEngine`] returning known positioned words for a `Scanned` page feeds the
//! *same* document-model pipeline as digital-born text, producing typed blocks
//! (heading / paragraph) in reading order, with the document labeled
//! [`SourceInfo::Ocr`] and per-block OCR confidence carried through. No PDF text
//! layer and no external OCR binary are involved — the words come straight from
//! the mock — so this isolates the seam itself.

use std::sync::Arc;

use oxide_engine::{
    ContentEngine, OcrEngine, OcrImage, OcrOptions, OcrPage, OcrWord, ParseOptions,
    SerializeOptions, SourceInfo,
};

// ── a single-page pure-scan PDF (no text layer) ──────────────────────────────

struct PdfBuilder {
    objects: Vec<Vec<u8>>,
}
impl PdfBuilder {
    fn new() -> Self {
        Self { objects: Vec::new() }
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

/// A 612×792 page whose only content is a full-page image — the classifier marks
/// it `Scanned`, routing it to the OCR path.
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

// ── the mock OCR engine ──────────────────────────────────────────────────────

/// Returns a fixed set of positioned words in image-pixel space (y-down). The
/// test renders the page at 72 DPI, so the image is 612×792 px and pixel coords
/// map 1:1 to page points (with a y-flip handled by the seam). A tall word near
/// the top is the heading; a run of smaller words below is the body paragraph.
struct MockOcrEngine {
    confidence: f32,
}

impl OcrEngine for MockOcrEngine {
    fn recognize(&self, image: &OcrImage, _opts: &OcrOptions) -> oxide_engine::Result<OcrPage> {
        // The seam must hand us a valid preprocessed image.
        assert!(image.is_valid(), "seam handed an invalid image to the engine");
        assert_eq!(image.width, 612, "rendered at 72 DPI → 612px wide");
        assert_eq!(image.height, 792, "rendered at 72 DPI → 792px tall");

        let mut words = Vec::new();

        // Heading line near the top: tall words (28px) sitting *adjacent* so they
        // read as one heading line (not table cells).
        let mut hx = 72.0;
        for t in ["Quarterly", "Report"] {
            let w = t.len() as f64 * 16.0;
            words.push(OcrWord {
                text: t.to_string(),
                bbox: [hx, 60.0, hx + w, 60.0 + 28.0],
                confidence: self.confidence,
                line_id: Some(0),
            });
            hx += w + 12.0;
        }

        // Body paragraph: several left-justified prose lines of small (11px)
        // words. Words flow naturally (varying widths) and lines have different
        // word counts/offsets so no regular grid forms — this must classify as a
        // paragraph, exercising the SAME geometric classifier the digital path
        // uses. Lines are 16px apart (tight leading → one block).
        let body_lines: &[&[&str]] = &[
            &["This", "is", "ordinary", "recognized", "body", "prose", "text"],
            &["that", "the", "shared", "layout", "pipeline", "classifies"],
            &["exactly", "as", "though", "it", "had", "come", "from", "a"],
            &["born-digital", "content", "stream", "instead", "of", "OCR."],
        ];
        let mut top = 160.0;
        for (i, toks) in body_lines.iter().enumerate() {
            let mut x = 72.0;
            for t in *toks {
                let w = t.len() as f64 * 6.0;
                words.push(OcrWord {
                    text: t.to_string(),
                    bbox: [x, top, x + w, top + 11.0],
                    confidence: self.confidence,
                    line_id: Some(1 + i as u32),
                });
                x += w + 5.0;
            }
            top += 16.0;
        }

        Ok(OcrPage::new(words))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn version(&self) -> Option<String> {
        Some("test-1".to_string())
    }
}

fn options_with(engine: Arc<dyn OcrEngine>, low_conf_warn: f32) -> ParseOptions {
    ParseOptions {
        ocr: Some(engine),
        ocr_dpi: 72, // 1:1 px↔pt mapping keeps the mock's boxes simple
        ocr_low_confidence_warn: low_conf_warn,
        ..ParseOptions::default()
    }
}

// ── the seam tests ───────────────────────────────────────────────────────────

#[test]
fn ocr_words_flow_through_shared_pipeline_to_typed_blocks() {
    let engine = ContentEngine::open_bytes(scanned_page()).unwrap();
    let opts = options_with(Arc::new(MockOcrEngine { confidence: 0.95 }), 0.0);
    let doc = engine.parse_document(&opts).unwrap();

    // The document is labeled as OCR-recovered.
    assert_eq!(doc.source, SourceInfo::Ocr, "all-scanned + OCR text → Ocr");
    assert_eq!(doc.pages.len(), 1);
    assert_eq!(
        doc.pages[0].source,
        oxide_engine::PageSource::Scanned,
        "the page was still routed as Scanned"
    );

    let md = doc.to_markdown(&SerializeOptions::default());
    // The recognized text survives into the output, in reading order.
    assert!(md.contains("Quarterly Report"), "heading text present:\n{md}");
    assert!(
        md.contains("ordinary recognized body prose"),
        "body text present:\n{md}"
    );
    let h = md.find("Quarterly Report").expect("heading");
    let body = md.find("ordinary recognized").expect("body");
    assert!(h < body, "heading must precede body:\n{md}");

    // The shared classifier typed the tall top line as a heading (the whole
    // point: OCR'd text is classified by the SAME geometric pipeline).
    assert!(
        md.contains("# Quarterly Report") || md.contains("## Quarterly Report"),
        "tall top line should classify as a heading:\n{md}"
    );

    // Provenance: no placeholder "OCR required" note remains, and the serialized
    // document source is `ocr` (the canonical, source-agnostic provenance the
    // JSON exposes — the internal per-block basis is a debug detail, not part of
    // the public schema).
    assert!(!md.contains("OCR required"), "placeholder replaced:\n{md}");
    let json = doc.to_json();
    assert!(
        json.contains("\"kind\":\"ocr\"") || json.contains("\"kind\": \"ocr\""),
        "json source is ocr:\n{json}"
    );
}

#[test]
fn ocr_confidence_is_carried_into_blocks() {
    let engine = ContentEngine::open_bytes(scanned_page()).unwrap();
    // Low mock confidence must cap each block's confidence and raise the warning.
    let opts = options_with(Arc::new(MockOcrEngine { confidence: 0.30 }), 0.5);
    let doc = engine.parse_document(&opts).unwrap();

    // Every text block's confidence is ≤ the OCR mean (0.30), never higher.
    let text_blocks: Vec<_> = doc
        .body
        .iter()
        .filter(|b| !matches!(b.kind, oxide_engine::BlockKind::Figure { .. }))
        .collect();
    assert!(!text_blocks.is_empty(), "should have recovered text blocks");
    for b in &text_blocks {
        assert!(
            b.confidence <= 0.30 + 1e-4,
            "block confidence {} exceeds OCR mean 0.30",
            b.confidence
        );
    }

    // The low-confidence page warning is present (it is a plain Text block, not
    // furniture, so it renders with the default serialize options).
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(
        md.contains("low-confidence OCR on page 1"),
        "low-confidence warning present:\n{md}"
    );
}

#[test]
fn no_ocr_engine_degrades_to_placeholder() {
    // Sanity: with no engine injected, the same scanned page still degrades to
    // the placeholder (pre-OCR behavior preserved).
    let engine = ContentEngine::open_bytes(scanned_page()).unwrap();
    let doc = engine.parse_document(&ParseOptions::default()).unwrap();
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(md.contains("scanned page 1"), "placeholder note present:\n{md}");
    assert_ne!(doc.source, SourceInfo::Ocr);
}

/// An engine that recovers nothing must fall back to the placeholder, not emit
/// an empty page.
#[test]
fn empty_ocr_falls_back_to_placeholder() {
    struct EmptyEngine;
    impl OcrEngine for EmptyEngine {
        fn recognize(&self, _i: &OcrImage, _o: &OcrOptions) -> oxide_engine::Result<OcrPage> {
            Ok(OcrPage::new(Vec::new()))
        }
        fn name(&self) -> &str {
            "empty"
        }
    }
    let engine = ContentEngine::open_bytes(scanned_page()).unwrap();
    let opts = options_with(Arc::new(EmptyEngine), 0.0);
    let doc = engine.parse_document(&opts).unwrap();
    let md = doc.to_markdown(&SerializeOptions::default());
    assert!(
        md.contains("scanned page 1"),
        "empty OCR should fall back to the placeholder:\n{md}"
    );
}
