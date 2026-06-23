//! End-to-end Tesseract smoke tests. **Gated**: these require the external
//! `tesseract` binary (+ the `eng` language pack) to be installed, so they are
//! `#[ignore]`d by default and run explicitly with:
//!
//! ```text
//! cargo test -p oxide-ocr-tesseract -- --ignored
//! ```
//!
//! They render a known digital-born page to an image and OCR it, checking that
//! real recognition recovers the text — honest accuracy, not a mock.

use oxide_engine::ocr::preprocess::{preprocess, PreprocessConfig};
use oxide_engine::{ContentEngine, OcrEngine, OcrImage, OcrOptions};
use oxide_ocr_tesseract::TesseractEngine;

/// Minimal single-page PDF with a few lines of large Helvetica text.
fn text_pdf(lines: &[&str]) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let mut add = |s: String| objects.push(s.into_bytes());

    add("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string());
    add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
        .to_string());
    let mut c = String::new();
    let mut y = 720.0;
    for line in lines {
        c.push_str(&format!(
            "BT /F1 24 Tf 1 0 0 1 72 {y:.1} Tm ({line}) Tj ET\n"
        ));
        y -= 40.0;
    }
    let content = format!("<< /Length {} >>\nstream\n{}\nendstream", c.len(), c);
    add(content);
    add(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_string(),
    );

    // Serialize.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len() + 1,
            xref
        )
        .as_bytes(),
    );
    pdf
}

fn tesseract_available() -> bool {
    TesseractEngine::new().is_ok()
}

/// Build a single-page **image-only** PDF (no text layer) embedding `gray`
/// (an 8-bit DeviceGray raster of `w`×`h`) as a raw, unfiltered image stream.
/// The classifier marks such a page `Scanned`, routing it to the OCR path — a
/// faithful stand-in for a scanned document built from real rendered pixels.
fn image_only_pdf(gray: &[u8], w: u32, h: u32) -> Vec<u8> {
    use std::io::Write as _;
    // Raw stream, no /Filter — valid PDF, avoids pulling in a compressor here.
    let raw = gray.to_vec();
    let page_w = 612.0;
    let page_h = 792.0;

    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_w} {page_h}] \
             /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
        )
        .into_bytes(),
    );
    let content = format!("q {page_w} 0 0 {page_h} 0 0 cm /Im0 Do Q\n");
    objects.push(
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
        .into_bytes(),
    );
    let mut img = format!(
        "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>\nstream\n",
        raw.len()
    )
    .into_bytes();
    img.extend_from_slice(&raw);
    img.extend_from_slice(b"\nendstream");
    objects.push(img);

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    let _ =
        pdf.write_all(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len() + 1,
            xref
        )
        .as_bytes(),
    );
    pdf
}

#[test]
#[ignore = "requires the external `tesseract` binary + eng language data"]
fn ocr_recovers_rendered_text() {
    if !tesseract_available() {
        eprintln!("SKIP: tesseract not installed; install it + the eng pack to run this test");
        return;
    }
    let pdf = text_pdf(&["The quick brown fox", "jumps over the lazy dog"]);
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buffer = engine.render_page(1, 300).unwrap();
    let raw = buffer.to_raw_image();
    let gray = OcrImage::from(&raw);
    let (clean, _angle) = preprocess(&gray, &PreprocessConfig::default());

    let tess = TesseractEngine::new().unwrap();
    let page = tess.recognize(&clean, &OcrOptions::default()).unwrap();

    let text: String = page
        .words
        .iter()
        .map(|w| w.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "tesseract {} recognized {} words (mean conf {:.2}): {text}",
        tess.version().unwrap_or_default(),
        page.words.len(),
        page.mean_confidence
    );
    // Honest, fuzzy assertion: most of the distinctive words should be present.
    let hits = ["quick", "brown", "fox", "jumps", "lazy", "dog"]
        .iter()
        .filter(|w| text.contains(*w))
        .count();
    assert!(
        hits >= 4,
        "expected to recover most words, found {hits}/6 in: {text}"
    );
    assert!(page.mean_confidence > 0.5, "mean confidence too low");
}

#[test]
#[ignore = "requires the external `tesseract` binary + eng language data"]
fn full_parse_path_ocrs_a_scanned_pdf() {
    use std::sync::Arc;

    use oxide_engine::{ParseOptions, SerializeOptions, SourceInfo};

    if !tesseract_available() {
        eprintln!("SKIP: tesseract not installed");
        return;
    }

    // 1. Make a digital text page and render it to raw grayscale pixels.
    let digital = text_pdf(&["Scanned Heading Here", "and some body text below it"]);
    let dengine = ContentEngine::open_bytes(digital).unwrap();
    // 150 DPI keeps the embedded raster modest while staying legible.
    let buf = dengine.render_page(1, 150).unwrap();
    let raw = buf.to_raw_image();
    let gray = OcrImage::from(&raw);

    // 2. Wrap those pixels as an image-only ("scanned") PDF.
    let scanned = image_only_pdf(&gray.gray, gray.width, gray.height);
    let engine = ContentEngine::open_bytes(scanned).unwrap();

    // 3. Parse with the real Tesseract engine injected — the whole wired path:
    //    classify → rasterize → preprocess → OCR → shared pipeline → blocks.
    let opts = ParseOptions {
        ocr: Some(Arc::new(TesseractEngine::new().unwrap()) as Arc<dyn OcrEngine>),
        ocr_dpi: 300,
        ..ParseOptions::default()
    };
    let doc = engine.parse_document(&opts).unwrap();

    assert_eq!(doc.source, SourceInfo::Ocr, "scanned + OCR text → Ocr");
    let md = doc.to_markdown(&SerializeOptions::default());
    eprintln!("--- OCR'd scanned page → markdown ---\n{md}\n---");
    let lower = md.to_lowercase();
    let hits = ["scanned", "heading", "body", "text", "below"]
        .iter()
        .filter(|w| lower.contains(*w))
        .count();
    assert!(
        hits >= 3,
        "expected the OCR'd scanned page to recover its text, found {hits}/5:\n{md}"
    );
}

/// **Source-agnostic KV proof** (Parser-Pivot prompt 4): the *same* field
/// extractor that handles digital invoices recovers fields from a SCANNED,
/// OCR'd invoice. Renders a labeled invoice to pixels, wraps it as an image-only
/// PDF, then runs `extract_fields` with OCR enabled.
#[test]
#[ignore = "requires the external `tesseract` binary + eng language data"]
fn extract_fields_on_an_ocrd_scanned_invoice() {
    use std::sync::Arc;

    use oxide_engine::{DocType, ExtractOptions, FieldValue};

    if !tesseract_available() {
        eprintln!("SKIP: tesseract not installed");
        return;
    }

    // A simple invoice rendered large enough to OCR cleanly.
    let digital = text_pdf(&[
        "INVOICE",
        "Invoice Number: INV-2024-0042",
        "Date: 2024-01-15",
        "Total: $486.00",
    ]);
    let dengine = ContentEngine::open_bytes(digital).unwrap();
    let buf = dengine.render_page(1, 200).unwrap();
    let raw = buf.to_raw_image();
    let gray = OcrImage::from(&raw);
    let scanned = image_only_pdf(&gray.gray, gray.width, gray.height);
    let engine = ContentEngine::open_bytes(scanned).unwrap();

    let opts = ExtractOptions {
        doc_type: Some(DocType::Invoice),
        ocr: Some(Arc::new(TesseractEngine::new().unwrap()) as Arc<dyn OcrEngine>),
        ocr_dpi: 300,
        ..Default::default()
    };
    let result = engine.extract_fields(&opts).unwrap();
    eprintln!(
        "--- fields from OCR'd scanned invoice ---\n{}",
        result.to_json()
    );

    assert_eq!(result.doc_type, DocType::Invoice);
    // The same profile + spatial engine recovered fields from OCR'd text.
    // OCR is imperfect, so assert on the high-value fields with tolerance.
    let total = result.get("total");
    assert!(
        total.is_some(),
        "expected a total field from the OCR'd invoice; got fields: {:?}",
        result.fields.iter().map(|f| &f.key).collect::<Vec<_>>()
    );
    if let Some(t) = total {
        // The amount should normalize even from OCR'd text.
        assert!(
            matches!(&t.value, FieldValue::Amount { value, .. } if (*value - 486.0).abs() < 1.0),
            "total value from OCR: {:?}",
            t.value
        );
    }
}

/// **Source-agnostic chunking proof** (Parser-Pivot prompt 5): the same chunker
/// produces RAG chunks from an OCR'd scanned document. Renders a multi-section
/// page to pixels, wraps it image-only, OCRs it, and chunks the result.
#[test]
#[ignore = "requires the external `tesseract` binary + eng language data"]
fn chunk_an_ocrd_scanned_document() {
    use std::sync::Arc;

    use oxide_engine::{ChunkOptions, ParseOptions};

    if !tesseract_available() {
        eprintln!("SKIP: tesseract not installed");
        return;
    }

    let digital = text_pdf(&[
        "Introduction",
        "This document was scanned and recovered by OCR.",
        "Methods",
        "We measured several things and report them below.",
    ]);
    let dengine = ContentEngine::open_bytes(digital).unwrap();
    let buf = dengine.render_page(1, 200).unwrap();
    let gray = OcrImage::from(&buf.to_raw_image());
    let scanned = image_only_pdf(&gray.gray, gray.width, gray.height);
    let engine = ContentEngine::open_bytes(scanned).unwrap();

    let doc = engine
        .parse_document(&ParseOptions {
            ocr: Some(Arc::new(TesseractEngine::new().unwrap()) as Arc<dyn OcrEngine>),
            ocr_dpi: 300,
            omit_furniture: false,
            ..ParseOptions::default()
        })
        .unwrap();

    let set = doc.chunk(&ChunkOptions {
        target_tokens: 100,
        ..Default::default()
    });
    eprintln!("--- chunks from OCR'd scan ---\n{}", set.to_json());

    assert!(!set.chunks.is_empty(), "OCR'd document should chunk");
    let all: String = set.chunks.iter().map(|c| c.text.to_lowercase()).collect();
    // The recovered text flows into chunks just like digital text.
    let hits = ["introduction", "scanned", "ocr", "methods", "measured"]
        .iter()
        .filter(|w| all.contains(*w))
        .count();
    assert!(
        hits >= 3,
        "expected OCR'd text in chunks, found {hits}/5:\n{all}"
    );
}
