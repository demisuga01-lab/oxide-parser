use std::error::Error;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "oxide",
    about = "Oxide — pure-Rust PDF processing tool",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Extract plain text from a PDF
    ExtractText(ExtractTextArgs),
    /// Extract embedded images from a PDF as a ZIP
    ExtractImages(ExtractImagesArgs),
    /// Render PDF pages to images as a ZIP
    Render(RenderArgs),
    /// Analyze whether a PDF has a real text layer
    Analyze(AnalyzeArgs),
    /// Merge several PDFs into one (pdfunite-equivalent)
    Merge(MergeArgs),
    /// Split a PDF into separate single-page PDFs (pdfseparate-equivalent)
    Split(SplitArgs),
    /// Extract a subset of pages into a new PDF
    ExtractPages(ExtractPagesArgs),
    /// Report document metadata and structural facts (pdfinfo-equivalent)
    Info(InfoArgs),
    /// List the fonts used in a PDF (pdffonts-equivalent)
    Fonts(FontsArgs),
    /// List or extract embedded file attachments (pdfdetach-equivalent)
    Detach(DetachArgs),
    /// Convert a PDF to HTML or XML (pdftohtml-equivalent)
    ToHtml(ToHtmlArgs),
    /// Verify digital signatures in a PDF (pdfsig-equivalent)
    VerifySig(VerifySigArgs),
}

#[derive(Parser)]
struct ExtractTextArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output file, defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Include page numbers in output
    #[arg(long)]
    page_numbers: bool,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ExtractImagesArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output ZIP file
    #[arg(short, long, default_value = "images.zip")]
    output: PathBuf,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Output format: png, jpg, webp, or original
    #[arg(short, long, default_value = "original")]
    format: String,
    /// JPEG quality 1-100, only for --format jpg
    #[arg(short, long, default_value = "85")]
    quality: u8,
    /// Minimum image width in pixels
    #[arg(long, default_value = "1")]
    min_width: u32,
    /// Minimum image height in pixels
    #[arg(long, default_value = "1")]
    min_height: u32,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct RenderArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output ZIP file
    #[arg(short, long, default_value = "pages.zip")]
    output: PathBuf,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Resolution in DPI (raster formats; also sets the device scale for svg)
    #[arg(short, long, default_value = "150")]
    dpi: u32,
    /// Output format: png, jpg, webp, or svg (vector)
    #[arg(short, long, default_value = "png")]
    format: String,
    /// JPEG quality 1-100
    #[arg(short, long, default_value = "85")]
    quality: u8,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct AnalyzeArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output as pretty-printed JSON
    #[arg(long)]
    pretty: bool,
}

#[derive(Parser)]
struct MergeArgs {
    /// Input PDF files, in the order their pages should appear
    #[arg(required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,
    /// Output PDF file
    #[arg(short, long, default_value = "merged.pdf")]
    output: PathBuf,
    /// Passwords for encrypted inputs, comma-separated, positionally matched to
    /// inputs (the empty user password is tried automatically). Fewer passwords
    /// than inputs is fine; missing ones default to empty.
    #[arg(long)]
    passwords: Option<String>,
}

#[derive(Parser)]
struct SplitArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output filename pattern; %d is replaced with the page number
    /// (e.g. "page-%d.pdf"). %0Nd zero-pads to width N (e.g. "page-%03d.pdf").
    #[arg(short, long, default_value = "page-%d.pdf")]
    output: String,
    /// First page to emit (1-based). Defaults to the first page.
    #[arg(short = 'f', long)]
    first: Option<usize>,
    /// Last page to emit (1-based). Defaults to the last page.
    #[arg(short = 'l', long)]
    last: Option<usize>,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ExtractPagesArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Page selection, e.g. "1,3,5-9". Order is preserved and duplicates kept.
    pages: String,
    /// Output PDF file
    #[arg(short, long, default_value = "extracted.pdf")]
    output: PathBuf,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct InfoArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Emit machine-readable JSON instead of the human-readable report
    #[arg(long)]
    json: bool,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct FontsArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Emit machine-readable JSON instead of the human-readable table
    #[arg(long)]
    json: bool,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct DetachArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// List embedded files (the default action when no save flag is given)
    #[arg(long)]
    list: bool,
    /// Save the attachment with this 1-based index (from --list)
    #[arg(long, value_name = "N")]
    save: Option<usize>,
    /// Save the attachment with this (original) file name
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Save every attachment
    #[arg(long)]
    save_all: bool,
    /// Directory to write extracted files into (filenames are sanitized)
    #[arg(long, default_value = ".")]
    output_dir: PathBuf,
    /// Emit machine-readable JSON (for --list)
    #[arg(long)]
    json: bool,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ToHtmlArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output HTML/XML file (defaults to stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Complex mode: absolutely-positioned text (default)
    #[arg(long)]
    complex: bool,
    /// Simple mode: flowing paragraphs (lower fidelity, readable)
    #[arg(long)]
    simple: bool,
    /// XML mode: positioned text fragments (pdftohtml -xml)
    #[arg(long)]
    xml: bool,
    /// Complex mode: render the page to a PNG behind the text for full fidelity
    #[arg(long)]
    background: bool,
    /// With --background, make the overlaid text invisible (selectable only)
    #[arg(long)]
    invisible_text: bool,
    /// DPI for the raster background (with --background)
    #[arg(long, default_value = "150")]
    background_dpi: u32,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct VerifySigArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Emit machine-readable JSON
    #[arg(long)]
    json: bool,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::ExtractText(args) => run_extract_text(args),
        Commands::ExtractImages(args) => run_extract_images(args),
        Commands::Render(args) => run_render(args),
        Commands::Analyze(args) => run_analyze(args),
        Commands::Merge(args) => run_merge(args),
        Commands::Split(args) => run_split(args),
        Commands::ExtractPages(args) => run_extract_pages(args),
        Commands::Info(args) => run_info(args),
        Commands::Fonts(args) => run_fonts(args),
        Commands::Detach(args) => run_detach(args),
        Commands::ToHtml(args) => run_to_html(args),
        Commands::VerifySig(args) => run_verify_sig(args),
    };

    if let Err(err) = result {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}

fn run_extract_text(args: ExtractTextArgs) -> Result<(), Box<dyn Error>> {
    use rayon::prelude::*;

    let engine = match &args.password {
        Some(password) => {
            oxide_engine::ContentEngine::open_path_with_password(&args.pdf, password.as_bytes())?
        }
        None => oxide_engine::ContentEngine::open_path(&args.pdf)?,
    };
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    // Per-page text rendering is independent and read-only, so extract pages
    // across rayon worker threads sharing the single parsed engine. Results are
    // collected in page order (par_iter preserves input order on collect), so
    // the output is byte-identical to serial extraction. The first page that
    // errors (lowest page index) is propagated, matching the serial `?`.
    let page_texts: Vec<oxide_engine::Result<String>> = page_nums
        .par_iter()
        .map(|&page_num| engine.get_page_text(page_num))
        .collect();

    let mut output_text = String::new();
    for (page_num, text) in page_nums.iter().zip(page_texts) {
        let text = text?;
        if args.page_numbers {
            output_text.push_str(&format!("--- Page {} ---\n", page_num));
        }
        output_text.push_str(&text);
        output_text.push('\n');
    }

    match args.output {
        Some(path) => std::fs::write(path, output_text)?,
        None => print!("{}", output_text),
    }
    Ok(())
}

fn run_extract_images(args: ExtractImagesArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{ImageLocateOptions, ImageLocator, ImageOutputFormat};
    use std::io::Write;
    use zip::{write::FileOptions, CompressionMethod, ZipWriter};

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let format = match args.format.to_lowercase().as_str() {
        "png" => ImageOutputFormat::Png,
        "jpg" | "jpeg" => ImageOutputFormat::Jpeg,
        "webp" => ImageOutputFormat::Webp,
        "original" | "" => ImageOutputFormat::Original,
        other => {
            return Err(
                format!("unknown format '{}'; use png, jpg, webp, or original", other).into(),
            )
        }
    };

    let opts = ImageLocateOptions {
        pages: Some(page_nums),
        min_width: args.min_width,
        min_height: args.min_height,
        ..Default::default()
    };
    let images = ImageLocator::find_all_images(&engine, &opts)?;

    let out_file = std::fs::File::create(&args.output)?;
    let mut zip = ZipWriter::new(out_file);
    let zip_opts = FileOptions::<()>::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));

    let mut encoded_count = 0usize;
    for (idx, img_ref) in images.iter().enumerate() {
        // Inline images (object_number == 0 with captured data) are exported too;
        // only skip references that carry no usable data.
        if img_ref.object_number == 0 && img_ref.inline_data.is_none() {
            continue;
        }

        let bytes = match engine.extract_image_bytes(img_ref, format.clone(), Some(args.quality)) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!(
                    "Warning: skipped image {} on page {}: {}",
                    img_ref.xobject_name, img_ref.page_number, err
                );
                continue;
            }
        };

        // For inline images, the chosen output extension is used as-is; XObject
        // images follow the same naming. The "-inline" marker keeps inline
        // exports recognizable without disturbing XObject numbering.
        let ext = if matches!(format, ImageOutputFormat::Original) && img_ref.is_inline {
            "png"
        } else {
            format.file_extension()
        };
        let suffix = if img_ref.is_inline { "-inline" } else { "" };
        let filename = format!(
            "page-{:03}-image-{:03}{}.{}",
            img_ref.page_number,
            idx + 1,
            suffix,
            ext
        );
        zip.start_file(&filename, zip_opts)?;
        zip.write_all(&bytes)?;
        encoded_count += 1;
    }
    zip.finish()?;

    eprintln!(
        "Extracted {} image(s) -> {}",
        encoded_count,
        args.output.display()
    );
    Ok(())
}

fn run_render(args: RenderArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{ImageEncoder, ImageOutputFormat};
    use std::io::Write;
    use zip::{write::FileOptions, CompressionMethod, ZipWriter};

    let dpi = args.dpi.clamp(24, 600);
    if dpi != args.dpi {
        eprintln!("Warning: DPI clamped to {} (valid range: 24-600)", dpi);
    }

    // Vector SVG output takes a separate path (one .svg per page in the ZIP).
    if matches!(args.format.to_lowercase().as_str(), "svg") {
        return run_render_svg(args, dpi);
    }

    let format = match args.format.to_lowercase().as_str() {
        "png" => ImageOutputFormat::Png,
        "jpg" | "jpeg" => ImageOutputFormat::Jpeg,
        "webp" => ImageOutputFormat::Webp,
        other => {
            return Err(format!("unknown format '{}'; use png, jpg, webp, or svg", other).into())
        }
    };

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let out_file = std::fs::File::create(&args.output)?;
    let mut zip = ZipWriter::new(out_file);
    let zip_opts = FileOptions::<()>::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));

    let mut rendered_count = 0usize;
    for page_num in &page_nums {
        let buf = match engine.render_page(*page_num, dpi) {
            Ok(buf) => buf,
            Err(err) => {
                eprintln!("Warning: skipped page {}: {}", page_num, err);
                continue;
            }
        };
        let raw = buf.to_raw_image();
        let bytes = match format {
            ImageOutputFormat::Jpeg => ImageEncoder::encode_jpeg(&raw, args.quality)?,
            ImageOutputFormat::Webp => ImageEncoder::encode_webp(&raw, args.quality)?,
            ImageOutputFormat::Png | ImageOutputFormat::Original => {
                ImageEncoder::encode_png_fast(&raw)?
            }
        };
        let filename = format!("page-{:03}.{}", page_num, format.file_extension());
        zip.start_file(&filename, zip_opts)?;
        zip.write_all(&bytes)?;
        rendered_count += 1;
    }
    zip.finish()?;

    eprintln!(
        "Rendered {} page(s) at {} DPI -> {}",
        rendered_count,
        dpi,
        args.output.display()
    );
    Ok(())
}

fn run_render_svg(args: RenderArgs, dpi: u32) -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    use zip::{write::FileOptions, CompressionMethod, ZipWriter};

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let out_file = std::fs::File::create(&args.output)?;
    let mut zip = ZipWriter::new(out_file);
    let zip_opts = FileOptions::<()>::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));

    let mut rendered = 0usize;
    let mut rasterized_fallback = 0usize;
    for page_num in &page_nums {
        let page = match engine.render_page_svg(*page_num, dpi) {
            Ok(page) => page,
            Err(err) => {
                eprintln!("Warning: skipped page {}: {}", page_num, err);
                continue;
            }
        };
        if page.is_rasterized {
            rasterized_fallback += 1;
        }
        let filename = format!("page-{:03}.svg", page_num);
        zip.start_file(&filename, zip_opts)?;
        zip.write_all(page.svg.as_bytes())?;
        rendered += 1;
    }
    zip.finish()?;

    eprintln!(
        "Rendered {} page(s) to SVG -> {} ({} page(s) used the raster-embed fallback)",
        rendered,
        args.output.display(),
        rasterized_fallback
    );
    Ok(())
}

fn run_analyze(args: AnalyzeArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{ContentEngine, PdfAnalyzer};

    let engine = ContentEngine::open_path(&args.pdf)?;
    let analysis = PdfAnalyzer::quick_analysis(&engine)?;

    let json = if args.pretty {
        serde_json::to_string_pretty(&serde_json::json!({
            "has_text_layer": analysis.has_text_layer,
            "confidence": analysis.confidence,
            "pages_with_text": analysis.pages_with_text,
            "is_likely_scanned": analysis.is_likely_scanned,
            "recommendation": analysis.recommendation,
        }))?
    } else {
        serde_json::to_string(&serde_json::json!({
            "has_text_layer": analysis.has_text_layer,
            "confidence": analysis.confidence,
            "is_likely_scanned": analysis.is_likely_scanned,
        }))?
    };

    println!("{}", json);
    Ok(())
}

fn open_engine(
    pdf: &std::path::Path,
    password: &Option<String>,
) -> Result<oxide_engine::ContentEngine, Box<dyn Error>> {
    use oxide_engine::ContentEngine;
    let engine = match password {
        Some(pw) => ContentEngine::open_path_with_password(pdf, pw.as_bytes())?,
        None => ContentEngine::open_path(pdf)?,
    };
    Ok(engine)
}

fn run_info(args: InfoArgs) -> Result<(), Box<dyn Error>> {
    let engine = open_engine(&args.pdf, &args.password)?;
    let info = engine.document_info()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }

    // Human-readable, pdfinfo-style "Label: value" lines. Optional fields are
    // only printed when present.
    let print_field = |label: &str, value: &str| {
        if !value.is_empty() {
            println!("{label:<16} {value}");
        }
    };
    print_field("Title:", info.title.as_deref().unwrap_or(""));
    print_field("Subject:", info.subject.as_deref().unwrap_or(""));
    print_field("Keywords:", info.keywords.as_deref().unwrap_or(""));
    print_field("Author:", info.author.as_deref().unwrap_or(""));
    print_field("Creator:", info.creator.as_deref().unwrap_or(""));
    print_field("Producer:", info.producer.as_deref().unwrap_or(""));
    print_field("CreationDate:", info.creation_date.as_deref().unwrap_or(""));
    print_field("ModDate:", info.mod_date.as_deref().unwrap_or(""));

    println!("{:<16} {}", "Tagged:", yes_no(info.tagged));
    println!("{:<16} {}", "Pages:", info.page_count);

    // Page size: first size, or "varies" with the distinct list.
    if let Some(first) = info.page_sizes.first() {
        let label = page_size_label(first);
        if info.page_size_varies {
            println!("{:<16} varies", "Page size:");
            for s in &info.page_sizes {
                println!("                 {} ({} page(s))", page_size_label(s), s.page_count);
            }
        } else {
            println!("{:<16} {}", "Page size:", label);
        }
    }

    println!("{:<16} {}", "Encrypted:", yes_no(info.encrypted));
    if let Some(enc) = &info.encryption {
        println!(
            "{:<16} {} (V{} R{}, {}-bit)",
            "  Algorithm:", enc.algorithm, enc.version, enc.revision, enc.key_length_bits
        );
        let p = &enc.permissions;
        println!(
            "{:<16} print:{} copy:{} modify:{} annotate:{} fill:{} accessible:{} assemble:{} hq-print:{}",
            "  Permissions:",
            yes_no(p.print),
            yes_no(p.copy),
            yes_no(p.modify),
            yes_no(p.annotate),
            yes_no(p.fill_forms),
            yes_no(p.extract_accessibility),
            yes_no(p.assemble),
            yes_no(p.high_quality_print),
        );
    }

    println!("{:<16} {}", "Optimized:", yes_no(info.linearized));
    println!("{:<16} {}", "PDF version:", info.pdf_version);
    println!("{:<16} {} bytes", "File size:", info.file_size_bytes);
    if let Some(id) = &info.file_id {
        println!("{:<16} {}", "File ID:", id);
    }
    println!("{:<16} {}", "XMP Metadata:", yes_no(info.has_xmp_metadata));

    Ok(())
}

fn page_size_label(s: &oxide_engine::PageSize) -> String {
    let base = format!(
        "{:.2} x {:.2} pts ({:.0} x {:.0} mm)",
        s.width_pts,
        s.height_pts,
        s.width_pts * 25.4 / 72.0,
        s.height_pts * 25.4 / 72.0,
    );
    if s.rotation != 0 {
        format!("{base} rotated {}\u{00B0}", s.rotation)
    } else {
        base
    }
}

fn run_fonts(args: FontsArgs) -> Result<(), Box<dyn Error>> {
    let engine = open_engine(&args.pdf, &args.password)?;
    let fonts = engine.list_fonts()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&fonts)?);
        return Ok(());
    }

    if fonts.is_empty() {
        println!("(no fonts found)");
        return Ok(());
    }

    // pdffonts-style table.
    println!(
        "{:<32} {:<16} {:<16} {:>3} {:>3} {:>3} {:>8}",
        "name", "type", "encoding", "emb", "sub", "uni", "object ID"
    );
    println!("{}", "-".repeat(88));
    for f in &fonts {
        println!(
            "{:<32} {:<16} {:<16} {:>3} {:>3} {:>3} {:>5} {:>1}",
            truncate(&f.name, 32),
            truncate(&f.font_type, 16),
            truncate(&f.encoding, 16),
            yes_no(f.embedded),
            yes_no(f.subset),
            yes_no(f.to_unicode),
            f.object_number,
            f.generation,
        );
    }

    Ok(())
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn run_detach(args: DetachArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::sanitize_filename;

    let engine = open_engine(&args.pdf, &args.password)?;
    let attachments = engine.list_attachments()?;

    let want_save = args.save.is_some() || args.name.is_some() || args.save_all;

    // Default action (and explicit --list) is to list.
    if !want_save || args.list {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&attachments)?);
        } else if attachments.is_empty() {
            println!("0 embedded files");
        } else {
            println!("{} embedded file(s)", attachments.len());
            for a in &attachments {
                let size = a
                    .size
                    .map(|s| format!("{s} bytes"))
                    .unwrap_or_else(|| "size unknown".to_string());
                let desc = a
                    .description
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                println!("{}: {} ({}){}", a.index, a.name, size, desc);
            }
        }
        // If only listing was requested, stop here.
        if !want_save {
            return Ok(());
        }
    }

    // Determine which attachments to save.
    let to_save: Vec<&oxide_engine::Attachment> = if args.save_all {
        attachments.iter().collect()
    } else if let Some(n) = args.save {
        let a = attachments
            .iter()
            .find(|a| a.index == n)
            .ok_or_else(|| format!("no attachment with index {n} (have {})", attachments.len()))?;
        vec![a]
    } else if let Some(name) = &args.name {
        let a = attachments
            .iter()
            .find(|a| &a.name == name)
            .ok_or_else(|| format!("no attachment named '{name}'"))?;
        vec![a]
    } else {
        Vec::new()
    };

    if to_save.is_empty() {
        return Ok(());
    }

    std::fs::create_dir_all(&args.output_dir)?;
    for a in to_save {
        let bytes = engine.extract_attachment(a)?;
        // Sanitize the attacker-controlled name down to a single safe
        // component, then join onto the chosen output directory.
        let safe = sanitize_filename(&a.name);
        let target = args.output_dir.join(&safe);
        std::fs::write(&target, &bytes)?;
        eprintln!(
            "Saved attachment {} '{}' -> {} ({} bytes)",
            a.index,
            a.name,
            target.display(),
            bytes.len()
        );
    }

    Ok(())
}

fn run_to_html(args: ToHtmlArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{HtmlMode, HtmlOptions};

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;
    if page_nums.is_empty() {
        return Err("no pages selected".into());
    }

    // Mode precedence: --xml, then --simple, else complex (the default).
    let mode = if args.xml {
        HtmlMode::Xml
    } else if args.simple {
        HtmlMode::Simple
    } else {
        HtmlMode::Complex
    };
    if args.complex && (args.simple || args.xml) {
        eprintln!("Warning: --complex ignored because --simple/--xml was given");
    }

    let options = HtmlOptions {
        mode,
        background: args.background,
        background_dpi: args.background_dpi.clamp(24, 600),
        invisible_text_over_background: args.invisible_text,
        ..Default::default()
    };

    let doc = engine.export_html(&page_nums, &options)?;

    match args.output {
        Some(path) => {
            std::fs::write(&path, doc.as_bytes())?;
            eprintln!(
                "Wrote {} page(s) as {:?} -> {}",
                page_nums.len(),
                mode,
                path.display()
            );
        }
        None => print!("{doc}"),
    }
    Ok(())
}

fn run_verify_sig(args: VerifySigArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{Coverage, SignatureValidity};

    let engine = open_engine(&args.pdf, &args.password)?;
    let reports = engine.verify_signatures()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }

    if reports.is_empty() {
        println!("No digital signatures found.");
        return Ok(());
    }

    println!("{} signature(s) found.\n", reports.len());
    for r in &reports {
        let verdict = match r.validity {
            SignatureValidity::Valid => "Signature is cryptographically VALID",
            SignatureValidity::Invalid => "Signature is INVALID (digest/signature mismatch)",
            SignatureValidity::UnsupportedAlgorithm => "Signature algorithm UNSUPPORTED",
            SignatureValidity::Error => "Signature could NOT be verified",
        };
        let coverage = match r.coverage {
            Coverage::WholeFile => "covers the whole file",
            Coverage::ModifiedAfterSigning => "document MODIFIED after signing (bytes appended)",
        };
        println!("Signature #{}:", r.index);
        if let Some(f) = &r.field_name {
            println!("  - Field:        {f}");
        }
        if let Some(n) = &r.signer_name {
            println!("  - Signer:       {n}");
        }
        if let Some(t) = &r.signing_time {
            println!("  - Signing time: {t}");
        }
        if let Some(s) = &r.sub_filter {
            println!("  - SubFilter:    {s}");
        }
        if let Some(d) = &r.digest_algorithm {
            println!("  - Digest:       {d}");
        }
        if let Some(reason) = &r.reason {
            println!("  - Reason:       {reason}");
        }
        if let Some(loc) = &r.location {
            println!("  - Location:     {loc}");
        }
        println!("  - Verdict:      {verdict}");
        println!("  - Coverage:     {coverage}");
        if let Some(c) = &r.certificate {
            println!("  - Certificate:");
            println!("      Subject:  {}", c.subject);
            println!("      Issuer:   {}", c.issuer);
            println!("      Serial:   {}", c.serial_hex);
            println!("      Validity: {} .. {}", c.not_before, c.not_after);
        }
        println!("  - Note: {}", r.note);
        println!();
    }
    Ok(())
}

fn run_merge(args: MergeArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{build_merged, ContentEngine};

    // Split positional passwords (comma-separated) and match by input index.
    let passwords: Vec<String> = args
        .passwords
        .as_deref()
        .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
        .unwrap_or_default();

    // Open every input first; keep the engines alive for the whole merge so the
    // builder can borrow each document.
    let mut engines = Vec::with_capacity(args.inputs.len());
    for (idx, path) in args.inputs.iter().enumerate() {
        let engine = match passwords.get(idx) {
            Some(pw) if !pw.is_empty() => {
                ContentEngine::open_path_with_password(path, pw.as_bytes())?
            }
            _ => ContentEngine::open_path(path)?,
        };
        engines.push(engine);
    }

    // Take all pages of each input, in order.
    let mut inputs = Vec::with_capacity(engines.len());
    let mut total_pages = 0usize;
    for engine in &engines {
        let count = engine.page_count()?;
        total_pages += count;
        let all: Vec<usize> = (1..=count).collect();
        inputs.push((engine.document(), all));
    }

    let bytes = build_merged(&inputs)?;
    std::fs::write(&args.output, &bytes)?;
    eprintln!(
        "Merged {} file(s), {} page(s) -> {}",
        engines.len(),
        total_pages,
        args.output.display()
    );
    Ok(())
}

fn run_split(args: SplitArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::ContentEngine;

    let engine = match &args.password {
        Some(pw) => ContentEngine::open_path_with_password(&args.pdf, pw.as_bytes())?,
        None => ContentEngine::open_path(&args.pdf)?,
    };
    let total = engine.page_count()?;
    if total == 0 {
        return Err("document has no pages".into());
    }

    let first = args.first.unwrap_or(1);
    let last = args.last.unwrap_or(total);
    if first == 0 || first > total {
        return Err(format!("--first {first} is out of range (1..={total})").into());
    }
    if last < first || last > total {
        return Err(format!("--last {last} is out of range ({first}..={total})").into());
    }

    let mut written = 0usize;
    for page in first..=last {
        let bytes = engine.extract_single_page(page)?;
        let path = expand_split_pattern(&args.output, page);
        std::fs::write(&path, &bytes)?;
        written += 1;
    }
    eprintln!(
        "Split {} page(s) [{}..={}] using pattern '{}'",
        written, first, last, args.output
    );
    Ok(())
}

fn run_extract_pages(args: ExtractPagesArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::ContentEngine;

    let engine = match &args.password {
        Some(pw) => ContentEngine::open_path_with_password(&args.pdf, pw.as_bytes())?,
        None => ContentEngine::open_path(&args.pdf)?,
    };
    let total = engine.page_count()?;
    let pages = parse_page_selection_ordered(&args.pages, total)?;
    if pages.is_empty() {
        return Err(format!("selection '{}' matched no pages in 1..={total}", args.pages).into());
    }

    let bytes = engine.extract_pages(&pages)?;
    std::fs::write(&args.output, &bytes)?;
    eprintln!(
        "Extracted {} page(s) -> {}",
        pages.len(),
        args.output.display()
    );
    Ok(())
}

/// Expand a split output pattern. Supports `%d` and `%0Nd` (zero-padded width
/// N) for the page number. If the pattern contains no `%`, the page number is
/// appended before the extension to avoid overwriting a single file.
fn expand_split_pattern(pattern: &str, page: usize) -> std::path::PathBuf {
    if let Some(pct) = pattern.find('%') {
        // Parse a printf-ish "%[0][width]d" directive.
        let after = &pattern[pct + 1..];
        let mut chars = after.char_indices().peekable();
        let mut zero_pad = false;
        if let Some(&(_, '0')) = chars.peek() {
            zero_pad = true;
            chars.next();
        }
        let mut width = 0usize;
        let mut consumed = 0usize;
        while let Some(&(i, c)) = chars.peek() {
            if c.is_ascii_digit() {
                width = width * 10 + (c as usize - '0' as usize);
                consumed = i + 1;
                chars.next();
            } else {
                break;
            }
        }
        // Expect a trailing 'd'.
        if let Some(&(i, 'd')) = chars.peek() {
            let directive_end = pct + 1 + i + 1;
            let num = if zero_pad {
                format!("{page:0width$}")
            } else {
                page.to_string()
            };
            let _ = consumed;
            let mut result = String::with_capacity(pattern.len() + num.len());
            result.push_str(&pattern[..pct]);
            result.push_str(&num);
            result.push_str(&pattern[directive_end..]);
            return std::path::PathBuf::from(result);
        }
    }
    // No usable directive: insert -<page> before the extension.
    let p = std::path::Path::new(pattern);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("page");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("pdf");
    let parent = p.parent();
    let name = format!("{stem}-{page}.{ext}");
    match parent {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => std::path::PathBuf::from(name),
    }
}

/// Parse a page selection preserving order and duplicates (e.g. "5,1,3-4,1").
/// Unlike [`parse_page_range_cli`], it does NOT sort or dedupe — extraction
/// honours the exact order the user requests. Out-of-range pages are dropped
/// with a warning so a typo doesn't silently reorder the rest.
fn parse_page_selection_ordered(spec: &str, total: usize) -> Result<Vec<usize>, Box<dyn Error>> {
    if spec.trim() == "all" || spec.trim().is_empty() {
        return Ok((1..=total).collect());
    }
    let mut pages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start: usize = start.trim().parse()?;
            let end: usize = end.trim().parse()?;
            if start <= end {
                for p in start..=end {
                    push_in_range(&mut pages, p, total);
                }
            } else {
                // Descending range, e.g. "9-5": honour the reverse order.
                for p in (end..=start).rev() {
                    push_in_range(&mut pages, p, total);
                }
            }
        } else {
            let p: usize = part.parse()?;
            push_in_range(&mut pages, p, total);
        }
    }
    Ok(pages)
}

fn push_in_range(pages: &mut Vec<usize>, page: usize, total: usize) {
    if (1..=total).contains(&page) {
        pages.push(page);
    } else {
        eprintln!("Warning: page {page} out of range (1..={total}); skipping");
    }
}

fn parse_page_range_cli(spec: &str, total: usize) -> Result<Vec<usize>, Box<dyn Error>> {
    if spec == "all" || spec.trim().is_empty() {
        return Ok((1..=total).collect());
    }

    let mut pages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if let Some((start, end)) = part.split_once('-') {
            let start = start.trim().parse::<usize>()?;
            let end = end.trim().parse::<usize>()?;
            if start <= end {
                for page in start..=end {
                    if (1..=total).contains(&page) {
                        pages.push(page);
                    }
                }
            }
        } else {
            let page = part.parse::<usize>()?;
            if (1..=total).contains(&page) {
                pages.push(page);
            }
        }
    }

    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::{expand_split_pattern, parse_page_range_cli, parse_page_selection_ordered};
    use std::path::PathBuf;

    #[test]
    fn cli_page_range_parser_handles_all_formats() {
        assert_eq!(parse_page_range_cli("all", 3).unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_page_range_cli("1", 5).unwrap(), vec![1]);
        assert_eq!(parse_page_range_cli("2-4", 5).unwrap(), vec![2, 3, 4]);
        assert_eq!(parse_page_range_cli("1,3,5", 5).unwrap(), vec![1, 3, 5]);
        assert_eq!(parse_page_range_cli("3-10", 5).unwrap(), vec![3, 4, 5]);
    }

    #[test]
    fn ordered_selection_preserves_order_and_duplicates() {
        // Order is kept, duplicates retained, ranges expanded in place.
        assert_eq!(
            parse_page_selection_ordered("5,1,3-4,1", 9).unwrap(),
            vec![5, 1, 3, 4, 1]
        );
        // Out-of-range pages are dropped (with a warning), not errors.
        assert_eq!(parse_page_selection_ordered("1,3,99", 5).unwrap(), vec![1, 3]);
        // Descending ranges go in reverse.
        assert_eq!(parse_page_selection_ordered("9-7", 9).unwrap(), vec![9, 8, 7]);
        // "all" still expands forward.
        assert_eq!(parse_page_selection_ordered("all", 3).unwrap(), vec![1, 2, 3]);
        // Non-contiguous subset.
        assert_eq!(parse_page_selection_ordered("1,3,5", 5).unwrap(), vec![1, 3, 5]);
    }

    #[test]
    fn split_pattern_expands_directives() {
        assert_eq!(
            expand_split_pattern("page-%d.pdf", 7),
            PathBuf::from("page-7.pdf")
        );
        assert_eq!(
            expand_split_pattern("out-%03d.pdf", 7),
            PathBuf::from("out-007.pdf")
        );
        assert_eq!(
            expand_split_pattern("p%04d", 42),
            PathBuf::from("p0042")
        );
        // No directive: page number inserted before extension.
        assert_eq!(
            expand_split_pattern("doc.pdf", 3),
            PathBuf::from("doc-3.pdf")
        );
    }
}
