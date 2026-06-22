use std::path::PathBuf;

use oxide_engine::{
    AnnotationOptions, AuthorPageSize as PageSize, Color, EditMode, EditRectStyle, EditTextStyle,
    HeaderFooterOptions, ImageRect, ImageStampOptions, OverlayLayer, PdfBuilder, PdfEditor,
    RedactionOptions, StandardFont, TextStyle, WatermarkOptions,
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
    let redacted = redact_pdf(base.clone())?;
    let annotated = annotate_pdf(base.clone(), EditMode::Incremental)?;
    let flattened_form = fill_and_flatten_form()?;

    let base_path = out_dir.join("editing-base.pdf");
    let full_path = out_dir.join("editing-full-rewrite.pdf");
    let incremental_path = out_dir.join("editing-incremental.pdf");
    let redacted_path = out_dir.join("editing-redacted.pdf");
    let annotated_path = out_dir.join("editing-annotated.pdf");
    let flattened_form_path = out_dir.join("editing-form-flattened.pdf");
    std::fs::write(&base_path, base)?;
    std::fs::write(&full_path, full)?;
    std::fs::write(&incremental_path, incremental)?;
    std::fs::write(&redacted_path, redacted)?;
    std::fs::write(&annotated_path, annotated)?;
    std::fs::write(&flattened_form_path, flattened_form)?;

    println!("wrote {}", base_path.display());
    println!("wrote {}", full_path.display());
    println!("wrote {}", incremental_path.display());
    println!("wrote {}", redacted_path.display());
    println!("wrote {}", annotated_path.display());
    println!("wrote {}", flattened_form_path.display());
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
        if page_number == 2 {
            page.draw_text(
                "Account secret: 123-45-6789",
                72.0,
                660.0,
                &TextStyle::standard(StandardFont::Helvetica, 11.0),
            )?;
        }
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

fn redact_pdf(base: Vec<u8>) -> oxide_engine::Result<Vec<u8>> {
    let mut editor = PdfEditor::open_bytes(base)?;
    editor.redact(
        2,
        ImageRect::new(70.0, 650.0, 180.0, 24.0),
        RedactionOptions::default(),
    )?;
    editor.save_to_bytes(EditMode::FullRewrite)
}

fn annotate_pdf(base: Vec<u8>, mode: EditMode) -> oxide_engine::Result<Vec<u8>> {
    let mut editor = PdfEditor::open_bytes(base)?;
    editor
        .add_highlight_annotation(
            1,
            ImageRect::new(70.0, 686.0, 360.0, 18.0),
            AnnotationOptions::default().contents("review this sentence"),
        )?
        .add_stamp_annotation(
            1,
            ImageRect::new(380.0, 640.0, 100.0, 32.0),
            "APPROVED",
            AnnotationOptions::default().color(Color::device_rgb(0.86, 0.94, 1.0)),
        )?
        .add_link_uri(
            1,
            ImageRect::new(72.0, 620.0, 110.0, 18.0),
            "https://example.com",
        )?;
    editor.save_to_bytes(mode)
}

fn fill_and_flatten_form() -> oxide_engine::Result<Vec<u8>> {
    let mut editor = PdfEditor::open_bytes(form_fixture_pdf())?;
    editor
        .set_form_text("name", "Alice Example")
        .set_form_checkbox("agree", true)
        .set_form_choice("plan", "Gold")
        .flatten_forms();
    editor.save_to_bytes(EditMode::FullRewrite)
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

fn form_fixture_pdf() -> Vec<u8> {
    let mut b = TinyPdf::new();
    b.add("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R] /DA (/Helv 10 Tf 0 g) >> >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >> >> /Contents 4 0 R /Annots [5 0 R 6 0 R 7 0 R] >>");
    let stream = b"BT /Helv 10 Tf 20 170 Td (Form fixture) Tj ET";
    b.add_raw(
        [
            format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
            stream.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    );
    b.add("<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [80 130 220 150] /V () /DA (/Helv 10 Tf 0 g) >>");
    b.add("<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [80 95 100 115] /V /Off /AS /Off >>");
    b.add("<< /Type /Annot /Subtype /Widget /FT /Ch /T (plan) /Rect [80 60 160 80] /Opt [(Silver) (Gold)] /V (Silver) /DA (/Helv 10 Tf 0 g) >>");
    b.build()
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

    fn add(&mut self, body: &str) {
        self.objects.push(body.as_bytes().to_vec());
    }

    fn add_raw(&mut self, body: Vec<u8>) {
        self.objects.push(body);
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
