//! Getting-started example for embedding `oxide-engine` as a library.
//!
//! Demonstrates the common operations through the single `ContentEngine` entry
//! point — the library equivalent of what the `oxide` CLI does.
//!
//! Run with a PDF path:
//!     cargo run --example getting_started -- path/to/input.pdf
//!
//! If no path is given it falls back to a bundled test fixture so the example
//! always runs (and is built by `cargo test`, so it can't rot).

use std::path::PathBuf;

use oxide_engine::{build_subset, ContentEngine, HtmlOptions, PdfDocument};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pick the input: a CLI arg, else a bundled fixture.
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("tracemonkey.pdf")
        });
    println!("Opening {}", path.display());

    // 1. Open the document.
    let engine = ContentEngine::open_path(&path)?;

    // 2. Document facts (pdfinfo / pdffonts equivalents).
    let info = engine.document_info()?;
    println!(
        "Pages: {}  PDF version: {}  Encrypted: {}",
        info.page_count, info.pdf_version, info.encrypted
    );
    if let Some(first) = info.page_sizes.first() {
        println!(
            "First page: {:.0} x {:.0} pts",
            first.width_pts, first.height_pts
        );
    }
    let fonts = engine.list_fonts()?;
    println!("Fonts used: {}", fonts.len());

    // 3. Text extraction (pdftotext equivalent).
    if info.page_count >= 1 {
        let text = engine.get_page_text(1)?;
        let preview: String = text
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" ");
        println!("Page 1 text preview: {preview}");
    }

    // 4. Rendering (pdftoppm / pdftocairo equivalents) — to bytes, not files.
    if info.page_count >= 1 {
        let png = engine.render_page_png_fast(1, 150)?;
        println!("Rendered page 1 to PNG: {} bytes", png.len());
        let svg = engine.render_page_svg(1, 96)?;
        println!(
            "Rendered page 1 to SVG: {} bytes (rasterized fallback: {})",
            svg.svg.len(),
            svg.is_rasterized
        );
    }

    // 5. Attachments and signatures (pdfdetach / pdfsig equivalents).
    let attachments = engine.list_attachments()?;
    println!("Embedded files: {}", attachments.len());
    let sigs = engine.verify_signatures()?;
    println!("Digital signatures: {}", sigs.len());

    // 6. Conversion: HTML (pdftohtml equivalent).
    if info.page_count >= 1 {
        let html = engine.export_html(&[1], &HtmlOptions::default())?;
        println!("HTML export of page 1: {} bytes", html.len());
    }

    // 7. Manipulation: extract page 1 into a fresh PDF (page-extract / writer).
    if info.page_count >= 1 {
        let doc = PdfDocument::open_path(&path)?;
        let subset = build_subset(&doc, &[1])?;
        println!("Extracted page 1 to a new PDF: {} bytes", subset.len());
    }

    Ok(())
}
