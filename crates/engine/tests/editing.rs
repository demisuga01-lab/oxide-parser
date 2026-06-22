use std::io::Cursor;

use oxide_engine::{
    AuthorPageSize as PageSize, Color, ContentEngine, EditMode, EditRectStyle, EditTextStyle,
    HeaderFooterOptions, ImageRect, ImageStampOptions, OverlayLayer, PdfBuilder, PdfDocument,
    PdfEditor, StandardFont, TextStyle, WatermarkOptions,
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
