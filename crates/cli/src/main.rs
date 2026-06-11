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
    /// Output format: png, jpg, or original
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
    /// Resolution in DPI
    #[arg(short, long, default_value = "150")]
    dpi: u32,
    /// Output format: png or jpg
    #[arg(short, long, default_value = "png")]
    format: String,
    /// JPEG quality 1-100
    #[arg(short, long, default_value = "85")]
    quality: u8,
}

#[derive(Parser)]
struct AnalyzeArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output as pretty-printed JSON
    #[arg(long)]
    pretty: bool,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::ExtractText(args) => run_extract_text(args),
        Commands::ExtractImages(args) => run_extract_images(args),
        Commands::Render(args) => run_render(args),
        Commands::Analyze(args) => run_analyze(args),
    };

    if let Err(err) = result {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}

fn run_extract_text(args: ExtractTextArgs) -> Result<(), Box<dyn Error>> {
    let engine = match &args.password {
        Some(password) => {
            oxide_engine::ContentEngine::open_path_with_password(&args.pdf, password.as_bytes())?
        }
        None => oxide_engine::ContentEngine::open_path(&args.pdf)?,
    };
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let mut output_text = String::new();
    for page_num in page_nums {
        let text = engine.get_page_text(page_num)?;
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
    use oxide_engine::{ContentEngine, ImageLocateOptions, ImageLocator, ImageOutputFormat};
    use std::io::Write;
    use zip::{write::FileOptions, CompressionMethod, ZipWriter};

    let engine = ContentEngine::open_path(&args.pdf)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let format = match args.format.to_lowercase().as_str() {
        "png" => ImageOutputFormat::Png,
        "jpg" | "jpeg" => ImageOutputFormat::Jpeg,
        "original" | "" => ImageOutputFormat::Original,
        other => {
            return Err(format!("unknown format '{}'; use png, jpg, or original", other).into())
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
        if img_ref.is_inline || img_ref.object_number == 0 {
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

        let filename = format!(
            "page-{:03}-image-{:03}.{}",
            img_ref.page_number,
            idx + 1,
            format.file_extension()
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
    use oxide_engine::{ContentEngine, ImageEncoder, ImageOutputFormat};
    use std::io::Write;
    use zip::{write::FileOptions, CompressionMethod, ZipWriter};

    let dpi = args.dpi.clamp(24, 600);
    if dpi != args.dpi {
        eprintln!("Warning: DPI clamped to {} (valid range: 24-600)", dpi);
    }

    let format = match args.format.to_lowercase().as_str() {
        "png" => ImageOutputFormat::Png,
        "jpg" | "jpeg" => ImageOutputFormat::Jpeg,
        other => return Err(format!("unknown format '{}'; use png or jpg", other).into()),
    };

    let engine = ContentEngine::open_path(&args.pdf)?;
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
    use super::parse_page_range_cli;

    #[test]
    fn cli_page_range_parser_handles_all_formats() {
        assert_eq!(parse_page_range_cli("all", 3).unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_page_range_cli("1", 5).unwrap(), vec![1]);
        assert_eq!(parse_page_range_cli("2-4", 5).unwrap(), vec![2, 3, 4]);
        assert_eq!(parse_page_range_cli("1,3,5", 5).unwrap(), vec![1, 3, 5]);
        assert_eq!(parse_page_range_cli("3-10", 5).unwrap(), vec![3, 4, 5]);
    }
}
