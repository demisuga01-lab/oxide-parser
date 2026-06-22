use std::path::PathBuf;

use oxide_engine::{
    AuthorPageSize as PageSize, Color, EditMode, EditRectStyle, EditTextStyle, HeaderFooterOptions,
    ImageRect, ImageStampOptions, OverlayLayer, PdfBuilder, PdfEditor, StandardFont, TextStyle,
    WatermarkOptions,
};

fn main() -> oxide_engine::Result<()> {
    let out_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    std::fs::create_dir_all(&out_dir)?;

    let base = base_pdf()?;
    let full = edit_pdf(base.clone(), EditMode::FullRewrite)?;
    let incremental = edit_pdf(base.clone(), EditMode::Incremental)?;

    let base_path = out_dir.join("editing-base.pdf");
    let full_path = out_dir.join("editing-full-rewrite.pdf");
    let incremental_path = out_dir.join("editing-incremental.pdf");
    std::fs::write(&base_path, base)?;
    std::fs::write(&full_path, full)?;
    std::fs::write(&incremental_path, incremental)?;

    println!("wrote {}", base_path.display());
    println!("wrote {}", full_path.display());
    println!("wrote {}", incremental_path.display());
    Ok(())
}

fn base_pdf() -> oxide_engine::Result<Vec<u8>> {
    let mut doc = PdfBuilder::new();
    doc.set_title("Editing API base")
        .set_author("Oxide PDF SDK")
        .set_creator("oxide-engine examples/editing.rs");
    for page_number in 1..=2 {
        let page = doc.add_page(PageSize::LETTER);
        page.draw_text(
            format!("Original report page {page_number}"),
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::HelveticaBold, 18.0),
        )?;
        page.draw_text(
            "This text came from the original revision and remains extractable after edits.",
            72.0,
            690.0,
            &TextStyle::standard(StandardFont::Helvetica, 11.0),
        )?;
    }
    doc.to_bytes()
}

fn edit_pdf(base: Vec<u8>, mode: EditMode) -> oxide_engine::Result<Vec<u8>> {
    let mut editor = PdfEditor::open_bytes(base)?;
    editor
        .add_watermark_text("CONFIDENTIAL", WatermarkOptions::default())?
        .add_header(
            "Edited with Oxide",
            HeaderFooterOptions {
                style: EditTextStyle::new(9.0).fill(Color::device_gray(0.25)),
                ..HeaderFooterOptions::default()
            },
        )?
        .add_footer("Page {page} of {total}", HeaderFooterOptions::default())?
        .draw_rect(
            1,
            ImageRect::new(66.0, 646.0, 250.0, 26.0),
            EditRectStyle {
                fill: Some(Color::device_rgb(0.9, 0.94, 0.98)),
                stroke: Some(Color::device_rgb(0.2, 0.28, 0.36)),
                line_width: 0.8,
                opacity: 0.9,
            },
            OverlayLayer::Underlay,
        )?
        .draw_text(
            1,
            "Overlay note",
            72.0,
            654.0,
            EditTextStyle::new(11.0).fill(Color::device_rgb(0.1, 0.18, 0.28)),
            OverlayLayer::Overlay,
        )?
        .stamp_rgba_image(
            2,
            12,
            12,
            rgba_badge(12, 12),
            ImageRect::new(420.0, 620.0, 72.0, 72.0),
            ImageStampOptions {
                opacity: 0.8,
                layer: OverlayLayer::Overlay,
            },
        )?;
    editor.save_to_bytes(mode)
}

fn rgba_badge(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[
                (40 + x * 8).min(255) as u8,
                (120 + y * 6).min(255) as u8,
                210,
                (110 + x * 6 + y * 4).min(255) as u8,
            ]);
        }
    }
    pixels
}
