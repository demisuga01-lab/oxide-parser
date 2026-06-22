use std::path::PathBuf;

use oxide_engine::{
    AuthorPageSize, Color, FlowDocument, Margins, ParagraphStyle, StandardFont, TableBuilder,
    TableColumn, TextAlign, TextStyle,
};

fn main() -> oxide_engine::Result<()> {
    let out = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/authored-example.pdf"));
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut flow = FlowDocument::new(AuthorPageSize::LETTER, Margins::all(72.0));
    flow.builder_mut()
        .set_title("Oxide authored capstone")
        .set_author("Oxide PDF SDK")
        .set_subject("PDF authoring API smoke")
        .set_creator("oxide-engine examples/authoring.rs");

    let custom_font = flow.builder_mut().register_font_bytes(
        "LiberationSerifCapstone",
        include_bytes!("../fonts/LiberationSerif-Regular.ttf").as_slice(),
    )?;
    let image = flow
        .builder_mut()
        .add_rgba_image(16, 12, sample_rgba_gradient(16, 12))?;

    let body = TextStyle::new(custom_font, 11.0).fill(Color::device_rgb(0.1, 0.12, 0.14));
    let paragraph = ParagraphStyle::new()
        .align(TextAlign::Left)
        .line_height(1.28);

    flow.add_heading("Oxide PDF Authoring", 1)?;
    flow.add_paragraph(
        "This capstone document was created from scratch with the authoring API. It combines flowing text, a custom embedded TrueType font, an RGBA image soft mask, vector drawing, and a table that can continue across pages.",
        &body,
        &paragraph,
    )?;
    flow.add_image(image, 180.0, 96.0)?;
    flow.add_spacer(28.0);

    let mut table = TableBuilder::new(vec![
        TableColumn::new(96.0),
        TableColumn::new(132.0),
        TableColumn::new(168.0),
    ])
    .body_style(TextStyle::standard(StandardFont::Helvetica, 8.5))
    .header_style(TextStyle::standard(StandardFont::HelveticaBold, 8.5));
    table.set_header(["Section", "Owner", "Notes"]);
    for idx in 0..24 {
        table.add_row([
            format!("Item {}", idx + 1),
            if idx % 2 == 0 {
                "SDK".to_string()
            } else {
                "QA".to_string()
            },
            "Wrapped table text validates row height, borders, fills, and repeated headers."
                .to_string(),
        ]);
    }
    flow.add_heading("Implementation Notes", 2)?;
    flow.add_table(&table)?;

    flow.save(&out)?;
    println!("wrote {}", out.display());
    Ok(())
}

fn sample_rgba_gradient(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let r = (40 + x * 10).min(255) as u8;
            let g = (80 + y * 12).min(255) as u8;
            let b = 190u8;
            let a = (90 + x * 8 + y * 6).min(255) as u8;
            pixels.extend_from_slice(&[r, g, b, a]);
        }
    }
    pixels
}
