use std::path::PathBuf;

use oxide_engine::{
    convert_to_pdfa_checked, get_fallback_font, improve_pdfua_best_effort, validate_pdfa,
    validate_pdfua, AuthorPageSize as PageSize, ContentEngine, PdfAProfile, PdfBuilder,
    PdfDocument, TextStyle,
};

fn main() -> oxide_engine::Result<()> {
    let out_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    std::fs::create_dir_all(&out_dir)?;

    let source = embedded_source_pdf()?;
    let source_path = out_dir.join("compliance-source.pdf");
    std::fs::write(&source_path, &source)?;

    let source_doc = PdfDocument::open_bytes(source)?;
    let before = validate_pdfa(&source_doc, PdfAProfile::PdfA2B)?;
    println!(
        "{} before conversion compliant: {} ({} findings)",
        PdfAProfile::PdfA2B.label(),
        before.compliant,
        before.violations.len()
    );

    let (pdfa, conversion) = convert_to_pdfa_checked(&source_doc, PdfAProfile::PdfA2B)?;
    let pdfa_path = out_dir.join("compliance-pdfa-2b.pdf");
    std::fs::write(&pdfa_path, &pdfa)?;
    println!(
        "{} after conversion compliant: {}",
        conversion.profile.label(),
        conversion.validation.compliant
    );

    let (pdfa1, conversion1) = convert_to_pdfa_checked(&source_doc, PdfAProfile::PdfA1B)?;
    let pdfa1_path = out_dir.join("compliance-pdfa-1b.pdf");
    std::fs::write(&pdfa1_path, &pdfa1)?;
    println!(
        "{} after conversion compliant: {}",
        conversion1.profile.label(),
        conversion1.validation.compliant
    );

    for profile in [
        PdfAProfile::PdfA2A,
        PdfAProfile::PdfA3B,
        PdfAProfile::PdfA3A,
    ] {
        let (bytes, conversion) = convert_to_pdfa_checked(&source_doc, profile)?;
        let path = out_dir.join(format!(
            "compliance-pdfa-{}{}.pdf",
            profile.part(),
            profile.conformance().to_ascii_lowercase()
        ));
        std::fs::write(&path, &bytes)?;
        println!(
            "{} after conversion compliant: {}",
            conversion.profile.label(),
            conversion.validation.compliant
        );
        println!("wrote {}", path.display());
    }

    let ua = improve_pdfua_best_effort(&PdfDocument::open_bytes(pdfa)?, "en-US")?;
    let ua_path = out_dir.join("compliance-ua-best-effort.pdf");
    std::fs::write(&ua_path, &ua)?;
    let ua_report = validate_pdfua(&PdfDocument::open_bytes(ua.clone())?)?;
    println!(
        "PDF/UA basic validation after best-effort tagging: {} ({} findings)",
        ua_report.compliant,
        ua_report.violations.len()
    );

    let text = ContentEngine::open_bytes(ua)?.get_page_text(1)?;
    println!("round-trip text: {}", text.trim());
    println!("wrote {}", source_path.display());
    println!("wrote {}", pdfa_path.display());
    println!("wrote {}", pdfa1_path.display());
    println!("wrote {}", ua_path.display());
    Ok(())
}

fn embedded_source_pdf() -> oxide_engine::Result<Vec<u8>> {
    let mut doc = PdfBuilder::new();
    doc.set_title("Compliance example")
        .set_author("Oxide PDF SDK")
        .set_creator("oxide-engine examples/compliance.rs");
    let font = doc.register_truetype_font_bytes(
        "LiberationSans",
        get_fallback_font("Helvetica")
            .expect("bundled fallback font")
            .to_vec(),
    )?;
    doc.add_page(PageSize::LETTER).draw_text(
        "Embedded-font document converted to PDF/A-2b.",
        72.0,
        720.0,
        &TextStyle::new(font, 12.0),
    )?;
    doc.to_bytes()
}
