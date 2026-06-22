use std::path::PathBuf;

use oxide_engine::authoring::{PageSize, PdfBuilder};
use oxide_engine::{
    Color, FontFace, GraphicsStyle, ParagraphStyle, StandardFont, TextAlign, TextStyle,
};

fn main() -> oxide_engine::Result<()> {
    let out = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/authored-example.pdf"));
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc = PdfBuilder::new();
    doc.set_title("Oxide authored example")
        .set_author("Oxide PDF SDK")
        .set_subject("PDF authoring API smoke")
        .set_creator("oxide-engine examples/authoring.rs");

    let title = TextStyle::standard(StandardFont::HelveticaBold, 24.0)
        .fill(Color::device_rgb(0.08, 0.12, 0.18));
    let body = TextStyle::standard(StandardFont::TimesRoman, 11.5);
    let unicode =
        TextStyle::new(FontFace::BuiltinUnicode, 12.0).fill(Color::device_rgb(0.0, 0.25, 0.5));

    let page = doc.add_page(PageSize::LETTER);
    page.draw_text("Oxide PDF Authoring", 72.0, 720.0, &title)?;
    page.draw_paragraph(
        "This page was created from scratch with PdfBuilder. Coordinates use native PDF user space: bottom-left origin, y-up.",
        72.0,
        680.0,
        360.0,
        &body,
        &ParagraphStyle::new().align(TextAlign::Left),
    )?;
    page.draw_text(
        "Unicode text via embedded Liberation Sans: cafe \u{03c0}",
        72.0,
        625.0,
        &unicode,
    )?;
    page.draw_rounded_rect(
        70.0,
        575.0,
        260.0,
        32.0,
        6.0,
        &GraphicsStyle::fill_stroke(
            Color::device_rgb(0.9, 0.94, 0.98),
            Color::device_rgb(0.15, 0.25, 0.38),
            1.2,
        ),
    );
    page.draw_line(
        72.0,
        548.0,
        420.0,
        548.0,
        &GraphicsStyle::stroke(Color::device_rgb(0.65, 0.08, 0.08), 2.0).dash(vec![8.0, 4.0], 0.0),
    );

    let page2 = doc.add_page(PageSize::A4.landscape());
    page2.draw_text_from_top(
        "Second page using top-left helper",
        72.0,
        72.0,
        &TextStyle::standard(StandardFont::Courier, 12.0),
    )?;
    page2.draw_circle(
        180.0,
        300.0,
        40.0,
        &GraphicsStyle::fill(Color::device_rgb(0.2, 0.45, 0.75)),
    );

    doc.save(&out)?;
    println!("wrote {}", out.display());
    Ok(())
}
