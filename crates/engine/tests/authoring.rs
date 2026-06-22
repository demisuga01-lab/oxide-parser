use std::io::Cursor;

use oxide_engine::authoring::{FlowDocument, PageSize, PdfBuilder, TableBuilder, TableColumn};
use oxide_engine::{
    Color, ContentEngine, FontFace, GraphicsStyle, ParagraphStyle, StandardFont, TextAlign,
    TextStyle, WriterMode,
};

fn authored_sample() -> Vec<u8> {
    let mut doc = PdfBuilder::new();
    doc.set_title("Authoring smoke")
        .set_author("Oxide")
        .set_creator("oxide-engine test");

    let title = TextStyle::standard(StandardFont::HelveticaBold, 22.0)
        .fill(Color::device_rgb(0.05, 0.1, 0.2));
    let body = TextStyle::standard(StandardFont::TimesRoman, 11.0);
    let unicode =
        TextStyle::new(FontFace::BuiltinUnicode, 12.0).fill(Color::device_rgb(0.0, 0.2, 0.45));

    let page = doc.add_page(PageSize::LETTER);
    page.draw_text("Authored PDF", 72.0, 720.0, &title).unwrap();
    page.draw_paragraph(
        "This paragraph is wrapped and aligned from measured glyph widths.",
        72.0,
        680.0,
        260.0,
        &body,
        &ParagraphStyle::new().align(TextAlign::Left),
    )
    .unwrap();
    page.draw_text("Unicode: cafe \u{03c0}", 72.0, 620.0, &unicode)
        .unwrap();
    page.draw_rect(
        70.0,
        590.0,
        180.0,
        22.0,
        &GraphicsStyle::fill_stroke(
            Color::device_rgb(0.88, 0.92, 0.96),
            Color::device_rgb(0.1, 0.2, 0.35),
            1.5,
        ),
    );
    page.draw_line(
        72.0,
        570.0,
        320.0,
        570.0,
        &GraphicsStyle::stroke(Color::device_rgb(0.7, 0.1, 0.1), 2.0),
    );

    let page2 = doc.add_page(PageSize::A4.landscape());
    page2
        .draw_text_from_top(
            "Second page from top coordinates",
            72.0,
            72.0,
            &TextStyle::standard(StandardFont::Courier, 12.0),
        )
        .unwrap();
    page2.draw_circle(
        180.0,
        360.0,
        36.0,
        &GraphicsStyle::fill(Color::device_rgb(0.2, 0.45, 0.75)),
    );

    doc.to_bytes().unwrap()
}

#[test]
fn authored_document_reopens_extracts_and_renders() {
    let bytes = authored_sample();
    let engine = ContentEngine::open_bytes(bytes).expect("open authored pdf");
    assert_eq!(engine.page_count().unwrap(), 2);

    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Authored PDF"), "{text}");
    assert!(text.contains("wrapped and aligned"), "{text}");
    assert!(text.contains("Unicode: cafe \u{03c0}"), "{text}");

    let png = engine.render_page_png_fast(1, 72).unwrap();
    assert_png_has_non_white_pixels(&png, 100);
}

#[test]
fn authored_document_is_deterministic_across_writer_modes() {
    let a = authored_sample();
    let b = authored_sample();
    assert_eq!(a, b);

    let mut classic = PdfBuilder::new().with_writer_mode(WriterMode::ClassicXref);
    classic
        .add_page(PageSize::LETTER)
        .draw_text(
            "Classic authoring",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 12.0),
        )
        .unwrap();
    let bytes = classic.to_bytes().unwrap();
    let engine = ContentEngine::open_bytes(bytes).unwrap();
    assert_eq!(engine.get_page_text(1).unwrap().trim(), "Classic authoring");
}

#[test]
fn authored_images_embed_jpeg_passthrough_and_png_alpha() {
    let mut doc = PdfBuilder::new();
    let jpeg = doc.add_jpeg_image(test_jpeg()).unwrap();
    let png = doc.add_png_image(&test_rgba_png()).unwrap();
    let page = doc.add_page(PageSize::LETTER);
    page.draw_image(jpeg, 72.0, 620.0, 72.0, 72.0);
    page.draw_image(png, 160.0, 620.0, 72.0, 72.0);

    let bytes = doc.to_bytes().unwrap();
    let pdf = String::from_utf8_lossy(&bytes);
    assert!(pdf.contains("/DCTDecode"), "JPEG must stay DCT encoded");
    assert!(pdf.contains("/SMask"), "RGBA PNG must emit a soft mask");

    let engine = ContentEngine::open_bytes(bytes).expect("open image pdf");
    assert_eq!(engine.page_count().unwrap(), 1);
    let png = engine.render_page_png_fast(1, 72).unwrap();
    assert_png_has_non_white_pixels(&png, 100);
}

#[test]
fn custom_truetype_font_embeds_and_extracts_unicode() {
    let mut doc = PdfBuilder::new();
    let custom = doc
        .register_font_bytes(
            "LiberationSerifPrompt4",
            include_bytes!("../fonts/LiberationSerif-Regular.ttf").as_slice(),
        )
        .unwrap();
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Custom font: cafe \u{03c0}",
            72.0,
            720.0,
            &TextStyle::new(custom, 14.0),
        )
        .unwrap();

    let bytes = doc.to_bytes().unwrap();
    let engine = ContentEngine::open_bytes(bytes).expect("open custom font pdf");
    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Custom font: cafe \u{03c0}"), "{text}");
    let png = engine.render_page_png_fast(1, 72).unwrap();
    assert_png_has_non_white_pixels(&png, 40);
}

#[test]
fn flow_layout_page_breaks_table_and_repeats_header() {
    let mut flow = FlowDocument::new(
        PageSize::custom(300.0, 220.0),
        oxide_engine::Margins::all(24.0),
    );
    flow.builder_mut().set_title("Flow table smoke");
    flow.add_heading("Flow report", 1).unwrap();
    flow.add_paragraph(
        "A compact flow document should create pages as table rows overflow.",
        &TextStyle::standard(StandardFont::Helvetica, 9.0),
        &ParagraphStyle::new(),
    )
    .unwrap();

    let mut table = TableBuilder::new(vec![
        TableColumn::new(76.0),
        TableColumn::new(172.0).align(TextAlign::Left),
    ]);
    table.set_header(["Metric", "Notes"]);
    for idx in 0..14 {
        table.add_row([
            format!("Row {}", idx + 1),
            "This row wraps into more than one line to exercise row height measurement."
                .to_string(),
        ]);
    }
    flow.add_table(&table).unwrap();

    let bytes = flow.into_builder().to_bytes().unwrap();
    let engine = ContentEngine::open_bytes(bytes).expect("open flow pdf");
    assert!(engine.page_count().unwrap() > 1);

    let mut header_count = 0;
    for page in 1..=engine.page_count().unwrap() {
        let text = engine.get_page_text(page).unwrap();
        if text.contains("Metric") {
            header_count += 1;
        }
    }
    assert!(header_count > 1, "header should repeat after page breaks");
}

#[test]
fn standard_font_rejects_unencodable_unicode() {
    let mut doc = PdfBuilder::new();
    let err = doc
        .add_page(PageSize::LETTER)
        .draw_text(
            "pi \u{03c0}",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 12.0),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("BuiltinUnicode"),
        "error should point users to Unicode font: {err}"
    );
}

fn test_jpeg() -> Vec<u8> {
    let pixels = [220, 30, 40, 40, 180, 70, 40, 70, 220, 250, 220, 40];
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(&pixels, 2, 2, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

fn test_rgba_png() -> Vec<u8> {
    let pixels = [
        255, 0, 0, 255, 0, 120, 255, 160, 0, 160, 60, 160, 255, 230, 0, 64,
    ];
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
    }
    out
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
