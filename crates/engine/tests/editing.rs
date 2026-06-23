use std::io::Cursor;

use oxide_engine::{
    AnnotationOptions, AuthorPageSize as PageSize, Color, ContentEngine, EditMode, EditRectStyle,
    EditTextStyle, ExtractOptions, HeaderFooterOptions, ImageRect, ImageStampOptions, OverlayLayer,
    PdfBuilder, PdfDocument, PdfEditor, RedactionOptions, StandardFont, TextStyle,
    WatermarkOptions,
};

fn base_pdf() -> Vec<u8> {
    let mut doc = PdfBuilder::new();
    doc.set_title("Editing base");
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Original page one",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 14.0),
        )
        .unwrap();
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Original page two",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 14.0),
        )
        .unwrap();
    doc.to_bytes().unwrap()
}

#[test]
fn full_rewrite_watermark_header_footer_preserves_original_text() {
    let mut editor = PdfEditor::open_bytes(base_pdf()).unwrap();
    editor
        .add_watermark_text("CONFIDENTIAL", WatermarkOptions::default())
        .unwrap()
        .add_footer("Page {page} of {total}", HeaderFooterOptions::default())
        .unwrap();

    let edited = editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let engine = ContentEngine::open_bytes(edited).unwrap();
    assert_eq!(engine.page_count().unwrap(), 2);
    let page1 = engine.get_page_text(1).unwrap();
    let page2 = engine.get_page_text(2).unwrap();
    assert!(page1.contains("Original page one"), "{page1}");
    assert!(page1.contains("CONFIDENTIAL"), "{page1}");
    assert!(page1.contains("Page 1 of 2"), "{page1}");
    assert!(page2.contains("Original page two"), "{page2}");
    assert!(page2.contains("Page 2 of 2"), "{page2}");

    let png = engine.render_page_png_fast(1, 72).unwrap();
    assert_png_has_non_white_pixels(&png, 100);
}

#[test]
fn incremental_overlay_preserves_original_prefix_and_prev_chain() {
    let base = base_pdf();
    let mut editor = PdfEditor::open_bytes(base.clone()).unwrap();
    editor
        .draw_text(
            1,
            "Incremental note",
            72.0,
            650.0,
            EditTextStyle::new(12.0).fill(Color::device_rgb(0.7, 0.0, 0.0)),
            OverlayLayer::Overlay,
        )
        .unwrap();

    let edited = editor.save_to_bytes(EditMode::Incremental).unwrap();
    assert!(
        edited.starts_with(&base),
        "original bytes must be untouched"
    );
    assert!(String::from_utf8_lossy(&edited[base.len()..]).contains("/Prev"));

    let engine = ContentEngine::open_bytes(edited.clone()).unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Original page one"), "{text}");
    assert!(text.contains("Incremental note"), "{text}");

    let reparsed = PdfDocument::open_bytes(edited).unwrap();
    assert_eq!(reparsed.get_pages().unwrap()[0].contents.len(), 2);
}

#[test]
fn underlay_and_overlay_are_ordered_around_existing_content() {
    let mut editor = PdfEditor::open_bytes(base_pdf()).unwrap();
    editor
        .draw_rect(
            1,
            ImageRect::new(50.0, 600.0, 200.0, 80.0),
            EditRectStyle {
                fill: Some(Color::device_rgb(0.9, 0.95, 1.0)),
                stroke: None,
                line_width: 0.0,
                opacity: 1.0,
            },
            OverlayLayer::Underlay,
        )
        .unwrap()
        .draw_text(
            1,
            "Overlay label",
            72.0,
            610.0,
            EditTextStyle::new(11.0),
            OverlayLayer::Overlay,
        )
        .unwrap();
    let edited = editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let document = PdfDocument::open_bytes(edited.clone()).unwrap();
    assert_eq!(document.get_pages().unwrap()[0].contents.len(), 3);
    let text = ContentEngine::open_bytes(edited)
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert!(text.contains("Original page one"), "{text}");
    assert!(text.contains("Overlay label"), "{text}");
}

#[test]
fn rgba_image_stamp_renders_and_keeps_text() {
    let mut editor = PdfEditor::open_bytes(base_pdf()).unwrap();
    editor
        .stamp_rgba_image(
            1,
            4,
            4,
            rgba_pixels(4, 4),
            ImageRect::new(300.0, 600.0, 80.0, 80.0),
            ImageStampOptions {
                opacity: 0.8,
                layer: OverlayLayer::Overlay,
            },
        )
        .unwrap();

    let edited = editor.save_to_bytes(EditMode::Incremental).unwrap();
    let text = ContentEngine::open_bytes(edited.clone())
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert!(text.contains("Original page one"), "{text}");
    let png = ContentEngine::open_bytes(edited)
        .unwrap()
        .render_page_png_fast(1, 72)
        .unwrap();
    assert_png_has_non_white_pixels(&png, 100);
}

#[test]
fn redaction_removes_text_and_rejects_incremental_mode() {
    let mut editor = PdfEditor::open_bytes(base_pdf()).unwrap();
    editor
        .redact(
            1,
            ImageRect::new(70.0, 710.0, 125.0, 25.0),
            RedactionOptions::default(),
        )
        .unwrap();
    assert!(editor.save_to_bytes(EditMode::Incremental).is_err());

    let redacted = editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let text = ContentEngine::open_bytes(redacted.clone())
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert!(!text.contains("Original page one"), "{text}");
    assert!(ContentEngine::open_bytes(redacted)
        .unwrap()
        .render_page_png_fast(1, 72)
        .is_ok());
}

#[test]
fn redaction_truly_removes_text_under_box_with_proportional_font() {
    // H-1 guarantee: a glyph that is *visually* under the redaction box must be
    // removed from the content stream, not merely covered. This exercises the
    // path the old fixed-width (0.5em/byte) geometry got wrong: wide glyphs (W)
    // push later text far to the right, so the fake metric mis-places "SECRET"
    // to the LEFT of the box and the old code kept its bytes verbatim.
    //
    // Layout (Helvetica 20pt, baseline y=700, x0=72):
    //   "WWWWWWWW " advances ~151.6pt of real width -> "SECRET" really sits at
    //   x in [~228, ~310]. The redaction box [230..330] covers it. The eight W's
    //   end at ~223pt and must survive (no over-removal of unrelated text).
    let mut doc = PdfBuilder::new();
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "WWWWWWWW SECRET",
            72.0,
            700.0,
            &TextStyle::standard(StandardFont::Helvetica, 20.0),
        )
        .unwrap();
    let mut editor = PdfEditor::open_bytes(doc.to_bytes().unwrap()).unwrap();
    editor
        .redact(
            1,
            ImageRect::new(230.0, 693.0, 100.0, 24.0),
            RedactionOptions::default(),
        )
        .unwrap();
    let redacted = editor.save_to_bytes(EditMode::FullRewrite).unwrap();

    // The secret must be gone from EVERY text channel we can extract.
    let engine = ContentEngine::open_bytes(redacted.clone()).unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(
        !text.contains("SECRET"),
        "redacted text leaked under the mark: {text:?}"
    );
    // And the raw content-stream bytes must not still carry the glyphs.
    assert!(
        !String::from_utf8_lossy(&redacted).contains("SECRET"),
        "redacted glyphs survived verbatim in the content stream"
    );
    // Unrelated text outside the box must be preserved (no fail-open over-removal
    // of the whole line just because part of it intersected).
    assert!(
        text.contains("WWWWWWWW"),
        "redaction over-removed text outside the box: {text:?}"
    );
    assert!(engine.render_page_png_fast(1, 72).is_ok());
}

#[test]
fn redaction_removes_intersecting_image_invocation() {
    let mut doc = PdfBuilder::new();
    let image = doc
        .add_rgb_image(2, 2, vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0])
        .unwrap();
    let page = doc.add_page(PageSize::LETTER);
    page.draw_text(
        "Visible caption",
        72.0,
        720.0,
        &TextStyle::standard(StandardFont::Helvetica, 14.0),
    )
    .unwrap();
    page.draw_image(image, 250.0, 600.0, 80.0, 80.0);
    let mut editor = PdfEditor::open_bytes(doc.to_bytes().unwrap()).unwrap();
    editor
        .redact(
            1,
            ImageRect::new(245.0, 595.0, 90.0, 90.0),
            RedactionOptions::default(),
        )
        .unwrap();
    let redacted = editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let text = ContentEngine::open_bytes(redacted.clone())
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert!(text.contains("Visible caption"), "{text}");
    assert!(
        !String::from_utf8_lossy(&redacted).contains("/OxIm"),
        "redacted content stream should not invoke the old image resource"
    );
}

#[test]
fn annotations_add_edit_and_delete_roundtrip() {
    let mut editor = PdfEditor::open_bytes(base_pdf()).unwrap();
    editor
        .add_highlight_annotation(
            1,
            ImageRect::new(70.0, 710.0, 130.0, 24.0),
            AnnotationOptions::default().contents("important"),
        )
        .unwrap()
        .add_text_note_annotation(
            1,
            ImageRect::new(250.0, 700.0, 20.0, 20.0),
            "review",
            AnnotationOptions::default().author("Oxide"),
        )
        .unwrap()
        .add_stamp_annotation(
            1,
            ImageRect::new(300.0, 650.0, 80.0, 30.0),
            "APPROVED",
            AnnotationOptions::default().color(Color::device_rgb(0.85, 0.95, 1.0)),
        )
        .unwrap()
        .add_link_uri(
            1,
            ImageRect::new(72.0, 690.0, 80.0, 18.0),
            "https://example.com",
        )
        .unwrap()
        .edit_annotation_contents(1, 0, "edited highlight")
        .unwrap()
        .delete_annotations_in_rect(1, ImageRect::new(248.0, 698.0, 25.0, 25.0))
        .unwrap();

    let edited = editor.save_to_bytes(EditMode::Incremental).unwrap();
    let document = PdfDocument::open_bytes(edited.clone()).unwrap();
    let page = document.get_pages().unwrap().remove(0);
    let page_obj = document
        .reader()
        .get_and_resolve(page.object_number, page.generation_number)
        .unwrap();
    let annots = page_obj
        .as_dict()
        .unwrap()
        .get("Annots")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(annots.len(), 3);
    let text = ContentEngine::open_bytes(edited)
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert!(text.contains("APPROVED"), "{text}");
}

#[test]
fn form_fill_and_flatten_bakes_values_and_removes_fields() {
    let form = acroform_pdf();
    let mut filled_editor = PdfEditor::open_bytes(form.clone()).unwrap();
    filled_editor
        .set_form_text("name", "Alice")
        .set_form_checkbox("agree", true)
        .set_form_choice("plan", "Gold");
    let filled = filled_editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let filled_doc = PdfDocument::open_bytes(filled.clone()).unwrap();
    let fields = ContentEngine::open_bytes(filled.clone())
        .unwrap()
        .extract_fields(&ExtractOptions::default())
        .unwrap()
        .fields;
    assert!(fields
        .iter()
        .any(|field| field.key == "name" && field.raw == "Alice"));
    assert!(fields
        .iter()
        .any(|field| field.key == "plan" && field.raw == "Gold"));
    assert!(filled_doc.get_catalog().unwrap().get("AcroForm").is_some());

    let mut flatten_editor = PdfEditor::open_bytes(filled).unwrap();
    flatten_editor.flatten_forms();
    let flattened = flatten_editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let engine = ContentEngine::open_bytes(flattened.clone()).unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Alice"), "{text}");
    assert!(text.contains("Gold"), "{text}");
    assert!(PdfDocument::open_bytes(flattened)
        .unwrap()
        .get_catalog()
        .unwrap()
        .get("AcroForm")
        .is_none());
}

fn rgba_pixels(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[
                (40 + x * 30) as u8,
                (80 + y * 25) as u8,
                220,
                (120 + x * 20 + y * 10).min(255) as u8,
            ]);
        }
    }
    pixels
}

fn assert_png_has_non_white_pixels(bytes: &[u8], min: usize) {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    let pixels = &buf[..info.buffer_size()];
    let non_white = pixels
        .chunks(4)
        .filter(|px| px.len() >= 3 && (px[0] < 250 || px[1] < 250 || px[2] < 250))
        .count();
    assert!(
        non_white > min,
        "expected more than {min} non-white pixels, got {non_white}"
    );
}

struct TinyPdf {
    objects: Vec<Vec<u8>>,
}

impl TinyPdf {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    fn add(&mut self, body: &str) -> usize {
        self.objects.push(body.as_bytes().to_vec());
        self.objects.len()
    }

    fn add_raw(&mut self, body: Vec<u8>) -> usize {
        self.objects.push(body);
        self.objects.len()
    }

    fn build(&self) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (idx, body) in self.objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                self.objects.len() + 1,
                xref
            )
            .as_bytes(),
        );
        pdf
    }
}

fn acroform_pdf() -> Vec<u8> {
    let mut b = TinyPdf::new();
    b.add("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R] /DA (/Helv 10 Tf 0 g) >> >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >> >> /Contents 4 0 R /Annots [5 0 R 6 0 R 7 0 R] >>");
    b.add_raw(
        b"<< /Length 46 >>\nstream\nBT /Helv 10 Tf 20 170 Td (Form fixture) Tj ET\nendstream"
            .to_vec(),
    );
    b.add("<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [80 130 220 150] /V () /DA (/Helv 10 Tf 0 g) >>");
    b.add("<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [80 95 100 115] /V /Off /AS /Off >>");
    b.add("<< /Type /Annot /Subtype /Widget /FT /Ch /T (plan) /Rect [80 60 160 80] /Opt [(Silver) (Gold)] /V (Silver) /DA (/Helv 10 Tf 0 g) >>");
    b.build()
}
