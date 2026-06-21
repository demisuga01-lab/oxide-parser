use std::error::Error;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Whether this binary was compiled with the optional `ocr` feature (the
/// Tesseract OCR backend). Reported by `--version` so a user can tell, without
/// running an `--ocr` command, whether OCR is available.
#[cfg(feature = "ocr")]
const OCR_COMPILED_IN: bool = true;
#[cfg(not(feature = "ocr"))]
const OCR_COMPILED_IN: bool = false;

/// The long `--version` string: CLI version, the underlying engine version, and
/// the compiled feature flags (currently just OCR). Built once at first use.
fn long_version() -> &'static str {
    use std::sync::OnceLock;
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        format!(
            "{cli}\nengine: {engine}\nocr: {ocr}\nfeatures: [{features}]",
            cli = env!("CARGO_PKG_VERSION"),
            engine = oxide_engine::ENGINE_VERSION,
            ocr = if OCR_COMPILED_IN {
                "compiled-in (Tesseract backend available)"
            } else {
                "not compiled-in (rebuild with --features ocr to enable)"
            },
            features = if OCR_COMPILED_IN { "ocr" } else { "" },
        )
    })
    .as_str()
}

#[derive(Parser)]
#[command(
    name = "oxide",
    about = "Oxide — pure-Rust PDF processing tool",
    version,
    long_version = long_version()
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Extract plain text from a PDF
    ExtractText(ExtractTextArgs),
    /// Detect and extract tables from a PDF as CSV or JSON (no Poppler equivalent)
    ExtractTables(ExtractTablesArgs),
    /// Parse a PDF into the canonical document model and serialize it to clean,
    /// structured Markdown / JSON / HTML for AI/RAG pipelines and data
    /// automation — headings/paragraphs/lists/tables/figures/captions in
    /// recovered reading order, with metadata, per-page geometry, and inline
    /// styling. The primary document-parser surface.
    Parse(ParseArgs),
    /// Build a typed, ordered document model — headings/paragraphs/lists/figures/
    /// captions/tables in recovered reading order — as JSON or readable markdown.
    /// Superseded by `parse` (kept as a thin alias for back-compat).
    DocumentModel(DocumentModelArgs),
    /// Extract structured key-value fields (invoice number/date/total, receipt
    /// merchant/amount, form label→value pairs, line items) to JSON. Combines
    /// exact AcroForm fields, a spatial label→value engine, and document-type
    /// profiles — works on digital-born and OCR'd documents alike.
    ExtractFields(ExtractFieldsArgs),
    /// Split a PDF into RAG-ready semantic chunks (structure-aware, token-sized,
    /// with overlap + heading context) as a JSON chunks array for embedding
    /// pipelines. Tables/figures stay intact; headings drive boundaries.
    Chunk(ChunkArgs),
    /// Score an extraction result against ground truth using standard metrics
    /// (CER/WER/reading-order/table cell-F1/TEDS/field-F1/block-type accuracy).
    /// Reads a ScoreInput JSON (file or stdin), writes a ScoreOutput JSON. The
    /// pure-Rust scoring core the extraction benchmark harness drives.
    EvalScore(EvalScoreArgs),
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
    /// Layout-aware extraction: recover reading order across columns/blocks via
    /// geometric XY-cut segmentation (correct multi-column order, unlike a plain
    /// top-to-bottom dump). Additive; the default extraction is unchanged.
    #[arg(long)]
    structured: bool,
    /// Semantic extraction: use tagged-PDF structure when present, falling back
    /// to geometric layout analysis when absent.
    #[arg(long)]
    semantic: bool,
    /// Output format for --structured/--semantic: text or json. Ignored without
    /// either flag.
    #[arg(long, default_value = "text")]
    format: String,
    /// OCR scanned (image-only) pages with Tesseract and extract the recovered
    /// text, instead of returning nothing for pages with no text layer. Routes
    /// through the OCR-aware document parser. Requires the `tesseract` binary on
    /// PATH and a CLI built with `--features ocr`. Mutually exclusive with
    /// --structured/--semantic.
    #[arg(long)]
    ocr: bool,
    /// OCR languages (Tesseract codes), comma- or plus-separated, e.g. `eng` or
    /// `eng,deu`. The matching tessdata packs must be installed.
    #[arg(long, default_value = "eng")]
    ocr_lang: String,
    /// DPI at which scanned pages are rasterized for OCR (~300 is the sweet spot).
    #[arg(long, default_value = "300")]
    ocr_dpi: u32,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ExtractTablesArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output file, defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Output format: csv (flattened), json (structured), or html (span/header table)
    #[arg(short, long, default_value = "csv")]
    format: String,
    /// Emit the span/header/nested structure model. JSON always includes the
    /// structured fields; this flag is accepted for explicit CLI workflows.
    #[arg(long)]
    structure: bool,
    /// Minimum detection confidence to include a table (0.0-1.0). Borderless
    /// tables carry lower confidence; raise this to keep only high-confidence
    /// (typically ruled) tables.
    #[arg(long, default_value = "0.0")]
    min_confidence: f64,
    /// Accepted for surface consistency with the other extract commands, but
    /// table-grid reconstruction from OCR'd word boxes is not yet supported (a
    /// known gap — see docs/parser_benchmark.md). Passing --ocr errors with a
    /// pointer to `extract-fields --ocr` / `extract-text --ocr`, which DO OCR.
    #[arg(long)]
    ocr: bool,
    /// OCR languages (unused; --ocr is not supported for table extraction).
    #[arg(long, default_value = "eng")]
    ocr_lang: String,
    /// OCR DPI (unused; --ocr is not supported for table extraction).
    #[arg(long, default_value = "300")]
    ocr_dpi: u32,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ParseArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output file, defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Output format: markdown (RAG/AI-facing), json (full faithful model), or
    /// html (semantic, for viewing)
    #[arg(short, long, default_value = "markdown")]
    format: String,
    /// Keep page furniture (running headers/footers, page numbers) in the body
    /// and output. By default furniture is omitted (it is usually noise for RAG);
    /// it is always retained in the JSON per-page view regardless.
    #[arg(long)]
    keep_furniture: bool,
    /// Emit page-boundary markers in Markdown/HTML output (a comment + rule, or
    /// an <hr>) so downstream can attribute content to pages.
    #[arg(long)]
    mark_page_breaks: bool,
    /// Annotate each block with its source page + bounding box (HTML data
    /// attributes / Markdown trailing comments) for RAG citation/traceability.
    #[arg(long)]
    provenance: bool,
    /// Write extracted figure images into this directory and reference them by
    /// path (reserved; image bytes are surfaced in a later stage).
    #[arg(long)]
    images_dir: Option<PathBuf>,
    /// De-hyphenate words split across line ends (compi-\nlation → compilation).
    /// RAG-friendly; off by default to preserve extracted characters verbatim.
    #[arg(long)]
    dehyphenate: bool,
    /// Normalize ligature codepoints to plain letters (ﬁ→fi). Off by default.
    #[arg(long)]
    normalize_ligatures: bool,
    /// Drop blocks below this classification confidence (0.0-1.0)
    #[arg(long, default_value = "0.0")]
    min_confidence: f64,
    /// OCR scanned (image-only) pages with Tesseract instead of emitting a
    /// placeholder. Requires the `tesseract` binary on PATH and a CLI built with
    /// the `ocr` feature. Recovered text flows through the same layout/heading/
    /// table pipeline as digital-born text.
    #[arg(long)]
    ocr: bool,
    /// OCR languages (Tesseract codes), comma- or plus-separated, e.g.
    /// `eng` or `eng,deu`. The matching tessdata packs must be installed.
    #[arg(long, default_value = "eng")]
    ocr_lang: String,
    /// DPI at which scanned pages are rasterized for OCR (~300 is the sweet spot).
    #[arg(long, default_value = "300")]
    ocr_dpi: u32,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct DocumentModelArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output file, defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Output format: json (structured model) or md/markdown/text (readable)
    #[arg(short, long, default_value = "json")]
    format: String,
    /// Drop blocks below this classification confidence (0.0-1.0)
    #[arg(long, default_value = "0.0")]
    min_confidence: f64,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ExtractFieldsArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output file, defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Document type guiding the field profile: auto (detect), invoice, receipt,
    /// form, or generic.
    #[arg(long, default_value = "auto")]
    r#type: String,
    /// Output format (currently json only).
    #[arg(short, long, default_value = "json")]
    format: String,
    /// Drop fields below this confidence (0.0-1.0).
    #[arg(long, default_value = "0.0")]
    min_confidence: f32,
    /// OCR scanned pages first (Tesseract). Requires the `ocr` feature + the
    /// `tesseract` binary; lets field extraction work on scanned documents.
    #[arg(long)]
    ocr: bool,
    /// OCR languages (Tesseract codes), comma/plus-separated.
    #[arg(long, default_value = "eng")]
    ocr_lang: String,
    /// DPI for OCR rasterization.
    #[arg(long, default_value = "300")]
    ocr_dpi: u32,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct ChunkArgs {
    /// Path to the PDF file
    pdf: PathBuf,
    /// Output file, defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Page range: all, 1, 2-5, or 1,3,7
    #[arg(short, long, default_value = "all")]
    pages: String,
    /// Target chunk size in (estimated) tokens.
    #[arg(long, default_value = "512")]
    target_tokens: usize,
    /// Token overlap carried between consecutive chunks (0 disables).
    #[arg(long, default_value = "64")]
    overlap: usize,
    /// Do NOT prepend the heading hierarchy to each chunk (on by default).
    #[arg(long)]
    no_heading_context: bool,
    /// Do NOT start a new chunk at each heading (on by default).
    #[arg(long)]
    no_split_on_headings: bool,
    /// Keep page furniture (headers/footers/page numbers) in chunk text.
    #[arg(long)]
    keep_furniture: bool,
    /// Output format (currently json only).
    #[arg(short, long, default_value = "json")]
    format: String,
    /// OCR scanned pages first (Tesseract). Requires the `ocr` feature + the
    /// `tesseract` binary; lets chunking work on scanned documents.
    #[arg(long)]
    ocr: bool,
    /// OCR languages (Tesseract codes), comma/plus-separated.
    #[arg(long, default_value = "eng")]
    ocr_lang: String,
    /// DPI for OCR rasterization.
    #[arg(long, default_value = "300")]
    ocr_dpi: u32,
    /// Password for encrypted PDFs (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Parser)]
struct EvalScoreArgs {
    /// ScoreInput JSON file. If omitted, reads JSON from stdin.
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// Output file for the ScoreOutput JSON; defaults to stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
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
    /// Resolution in DPI (raster formats; also sets the device scale for svg/ps/eps)
    #[arg(short, long, default_value = "150")]
    dpi: u32,
    /// Output format: png, jpg, webp, svg (vector), ps (PostScript), or eps
    #[arg(short, long, default_value = "png")]
    format: String,
    /// JPEG quality 1-100
    #[arg(short, long, default_value = "85")]
    quality: u8,
    /// Raster compositing mode: compat matches Poppler/Splash; high uses linear-light RGB compositing
    #[arg(long, default_value = "compat", value_parser = ["compat", "high", "high-quality", "hq"])]
    render_quality: String,
    /// Password for an encrypted PDF (the empty user password is tried automatically)
    #[arg(long)]
    password: Option<String>,
    /// Maximum pixels (width*height) per rendered page. A page whose final pixel
    /// count would exceed this is skipped with a clean error instead of attempting
    /// an abusive allocation. Defaults to the engine cap (100 MP); overrides the
    /// OXIDE_MAX_RENDER_PIXELS environment variable when set.
    #[arg(long)]
    max_render_pixels: Option<u64>,
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
        Commands::ExtractTables(args) => run_extract_tables(args),
        Commands::Parse(args) => run_parse(args),
        Commands::DocumentModel(args) => run_document_model(args),
        Commands::ExtractFields(args) => run_extract_fields(args),
        Commands::Chunk(args) => run_chunk(args),
        Commands::EvalScore(args) => run_eval_score(args),
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

    if args.structured && args.semantic {
        return Err("--structured and --semantic are mutually exclusive".into());
    }
    if args.ocr && (args.structured || args.semantic) {
        return Err("--ocr cannot be combined with --structured or --semantic".into());
    }

    // OCR path: route through the OCR-aware document parser so scanned
    // (image-only) pages contribute recovered text. Digital-born pages parse
    // exactly as before; only pages with no text layer change. Additive — the
    // default (no --ocr) path below is untouched.
    if args.ocr {
        return run_extract_text_ocr(&engine, page_nums, &args);
    }

    // Layout-aware and semantic extraction take separate additive paths and do
    // NOT change the default extraction below.
    if args.semantic {
        return run_extract_text_semantic(&engine, &page_nums, &args);
    }
    if args.structured {
        return run_extract_text_structured(&engine, &page_nums, &args);
    }

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

/// Layout-aware extraction: XY-cut segmentation recovers reading order across
/// columns/blocks. `--format text` emits reading-order text; `--format json`
/// emits the structured block tree (bounding boxes + reading order).
fn run_extract_text_structured(
    engine: &oxide_engine::ContentEngine,
    page_nums: &[usize],
    args: &ExtractTextArgs,
) -> Result<(), Box<dyn Error>> {
    let as_json = match args.format.to_lowercase().as_str() {
        "json" => true,
        "text" | "txt" => false,
        other => {
            return Err(format!("unknown --format '{other}'; use text or json").into());
        }
    };

    if as_json {
        // One JSON object per document: pages -> blocks -> lines, in reading order.
        let mut pages = Vec::new();
        for &page_num in page_nums {
            let layout = engine.analyze_page_layout(page_num)?;
            pages.push(serde_json::json!({
                "page": page_num,
                "blocks": layout.blocks,
            }));
        }
        let doc = serde_json::json!({ "pages": pages });
        let s = serde_json::to_string_pretty(&doc)?;
        match &args.output {
            Some(path) => std::fs::write(path, s)?,
            None => println!("{s}"),
        }
        return Ok(());
    }

    let mut out = String::new();
    for &page_num in page_nums {
        if args.page_numbers {
            out.push_str(&format!("--- Page {page_num} ---\n"));
        }
        out.push_str(&engine.get_page_text_structured(page_num)?);
        out.push('\n');
    }
    match &args.output {
        Some(path) => std::fs::write(path, out)?,
        None => print!("{out}"),
    }
    Ok(())
}

/// Semantic extraction: tagged PDFs use `/StructTreeRoot` and MCID links;
/// untagged PDFs fall back to the geometric layout analyzer from `--structured`.
fn run_extract_text_semantic(
    engine: &oxide_engine::ContentEngine,
    page_nums: &[usize],
    args: &ExtractTextArgs,
) -> Result<(), Box<dyn Error>> {
    let as_json = match args.format.to_lowercase().as_str() {
        "json" => true,
        "text" | "txt" => false,
        other => {
            return Err(format!("unknown --format '{other}'; use text or json").into());
        }
    };

    let document = engine.extract_semantic_document(page_nums)?;
    let output = if as_json {
        serde_json::to_string_pretty(&document)?
    } else {
        document.to_text()
    };

    match &args.output {
        Some(path) => std::fs::write(path, output)?,
        None => {
            if as_json {
                println!("{output}");
            } else {
                print!("{output}");
                if !output.ends_with('\n') {
                    println!();
                }
            }
        }
    }
    Ok(())
}

/// OCR-aware text extraction: parse the document through the OCR seam so that
/// scanned (image-only) pages contribute recovered text, then emit the body
/// blocks as plain text in recovered reading order. Digital-born pages parse as
/// usual; only pages with no text layer differ from the non-OCR path.
fn run_extract_text_ocr(
    engine: &oxide_engine::ContentEngine,
    page_nums: Vec<usize>,
    args: &ExtractTextArgs,
) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{BlockKind, ParseOptions};

    let options = ParseOptions {
        pages: page_nums,
        // Keep furniture out of the text dump, matching the parser default.
        omit_furniture: true,
        ocr: Some(build_ocr_engine()?),
        ocr_options: ocr_options(&args.ocr_lang, args.ocr_dpi),
        ocr_dpi: args.ocr_dpi.max(1),
        ..ParseOptions::default()
    };
    let document = engine.parse_document(&options)?;

    // Walk the body blocks and emit their plain text, one logical block per
    // paragraph. Optional per-page markers mirror the non-OCR path.
    let mut out = String::new();
    let mut last_page: Option<u32> = None;
    for block in &document.body {
        if args.page_numbers && last_page != Some(block.page) {
            out.push_str(&format!("--- Page {} ---\n", block.page));
            last_page = Some(block.page);
        }
        let line = match &block.kind {
            BlockKind::Title { text }
            | BlockKind::Heading { text, .. }
            | BlockKind::Paragraph { text }
            | BlockKind::Caption { text, .. }
            | BlockKind::Header { text }
            | BlockKind::Footer { text }
            | BlockKind::PageNumber { text }
            | BlockKind::Text { text } => text.to_plain(),
            BlockKind::List { items, .. } => items
                .iter()
                .map(|it| it.text.to_plain())
                .collect::<Vec<_>>()
                .join("\n"),
            // Tables and figures carry no flowing prose; skip in a text dump.
            BlockKind::Table { .. } | BlockKind::Figure { .. } => continue,
        };
        if !line.trim().is_empty() {
            out.push_str(&line);
            out.push_str("\n\n");
        }
    }

    match &args.output {
        Some(path) => std::fs::write(path, out)?,
        None => print!("{out}"),
    }
    Ok(())
}

/// Detect and extract tables from a PDF — a capability Poppler's CLIs lack.
/// Ruled tables (drawn grid lines) and borderless tables (alignment-only) are
/// emitted as CSV (default), structured JSON, or span/header-preserving HTML.
fn run_extract_tables(args: ExtractTablesArgs) -> Result<(), Box<dyn Error>> {
    if args.ocr {
        return Err("table extraction does not support --ocr: reconstructing a \
                    table grid from OCR'd word boxes is a known gap (see \
                    docs/parser_benchmark.md). For scanned documents, use \
                    `extract-fields --ocr` to recover key-value fields and line \
                    items, or `extract-text --ocr` for the recovered text."
            .into());
    }
    let _ = (&args.ocr_lang, args.ocr_dpi); // accepted for flag consistency only
    let engine = match &args.password {
        Some(password) => {
            oxide_engine::ContentEngine::open_path_with_password(&args.pdf, password.as_bytes())?
        }
        None => oxide_engine::ContentEngine::open_path(&args.pdf)?,
    };
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let format = match args.format.to_lowercase().as_str() {
        "json" => "json",
        "html" | "htm" => "html",
        "csv" => "csv",
        other => return Err(format!("unknown --format '{other}'; use csv, json, or html").into()),
    };

    // (page, table) pairs above the confidence threshold, in page/reading order.
    let mut found = 0usize;
    let mut json_pages = Vec::new();
    let mut csv_out = String::new();
    let mut html_pages = Vec::new();

    for &page_num in &page_nums {
        let tables: Vec<_> = engine
            .extract_tables(page_num)?
            .into_iter()
            .filter(|t| t.confidence >= args.min_confidence)
            .collect();
        found += tables.len();

        match format {
            "json" => {
                json_pages.push(serde_json::json!({
                    "page": page_num,
                    "tables": tables,
                }));
            }
            "html" => {
                html_pages.push((page_num, tables));
            }
            _ => {
                for (i, t) in tables.iter().enumerate() {
                    if !csv_out.is_empty() {
                        csv_out.push('\n');
                    }
                    // A comment header makes multi-table CSV output navigable.
                    csv_out.push_str(&format!(
                        "# page {page_num} table {} ({:?}, confidence {:.2}, {}x{})\n",
                        i + 1,
                        t.source,
                        t.confidence,
                        t.num_rows(),
                        t.num_cols()
                    ));
                    csv_out.push_str(&t.to_csv());
                }
            }
        }
    }

    let output_text = match format {
        "json" => serde_json::to_string_pretty(&serde_json::json!({
            "structure": args.structure,
            "pages": json_pages
        }))?,
        "html" => table_pages_to_html(&html_pages),
        _ => csv_out,
    };

    match &args.output {
        Some(path) => std::fs::write(path, &output_text)?,
        None => print!("{output_text}"),
    }
    eprintln!(
        "Detected {found} table(s) across {} page(s)",
        page_nums.len()
    );
    Ok(())
}

fn table_pages_to_html(pages: &[(usize, Vec<oxide_engine::analysis::tables::Table>)]) -> String {
    let mut out = String::from(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Extracted Tables</title></head><body>\n",
    );
    for (page, tables) in pages {
        for (idx, table) in tables.iter().enumerate() {
            out.push_str(&format!(
                "<section data-page=\"{}\" data-table=\"{}\">\n",
                page,
                idx + 1
            ));
            out.push_str(&table.to_html());
            out.push_str("</section>\n");
        }
    }
    out.push_str("</body></html>\n");
    out
}

/// Construct the OCR backend for `--ocr`. Behind the `ocr` cargo feature: with
/// the feature on, this discovers and probes the external `tesseract` binary;
/// with the feature off, it returns an actionable error so a default
/// (pure-Rust) CLI build still parses, just without OCR.
#[cfg(feature = "ocr")]
fn build_ocr_engine(
) -> Result<std::sync::Arc<dyn oxide_engine::OcrEngine>, Box<dyn Error>> {
    let engine = oxide_ocr_tesseract::TesseractEngine::new()?;
    Ok(std::sync::Arc::new(engine))
}

#[cfg(not(feature = "ocr"))]
fn build_ocr_engine(
) -> Result<std::sync::Arc<dyn oxide_engine::OcrEngine>, Box<dyn Error>> {
    Err("this build of oxide has no OCR backend; rebuild the CLI with \
         `--features ocr` (and install the `tesseract` binary + language data) \
         to use --ocr"
        .into())
}

/// Build [`oxide_engine::OcrOptions`] from the shared CLI `--ocr-lang`/`--ocr-dpi`
/// flags (languages split on `+`/`,`, falling back to `eng`). Used by every
/// command that supports `--ocr` so the option parsing stays identical.
fn ocr_options(ocr_lang: &str, ocr_dpi: u32) -> oxide_engine::OcrOptions {
    let langs: Vec<String> = ocr_lang
        .split(['+', ','])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    oxide_engine::OcrOptions {
        languages: if langs.is_empty() {
            vec!["eng".to_string()]
        } else {
            langs
        },
        dpi: ocr_dpi,
        psm: None,
    }
}

/// Build a typed, ordered document model — a real document outline (headings,
/// paragraphs, lists, figures, captions, tables in reading order), not a text
/// dump. Tagged PDFs use their authored structure; untagged PDFs use the
/// geometric precedence-graph ordering + semantic classifier. JSON emits the
/// full model; markdown emits a readable rendering for human inspection.
fn run_parse(args: ParseArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{ImageHandling, ParseOptions, SerializeOptions};

    #[derive(Clone, Copy)]
    enum Fmt {
        Markdown,
        Json,
        Html,
    }
    let fmt = match args.format.to_lowercase().as_str() {
        "markdown" | "md" | "text" | "txt" => Fmt::Markdown,
        "json" => Fmt::Json,
        "html" => Fmt::Html,
        other => {
            return Err(format!("unknown --format '{other}'; use markdown, json, or html").into());
        }
    };

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let mut options = ParseOptions {
        pages: page_nums,
        min_confidence: args.min_confidence,
        omit_furniture: !args.keep_furniture,
        images: match &args.images_dir {
            Some(dir) => ImageHandling::SidecarDir(dir.clone()),
            None => ImageHandling::Omit,
        },
        dehyphenate: args.dehyphenate,
        normalize_ligatures: args.normalize_ligatures,
        ..ParseOptions::default()
    };
    if args.ocr {
        let langs: Vec<String> = args
            .ocr_lang
            .split(['+', ','])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        options.ocr = Some(build_ocr_engine()?);
        options.ocr_options = oxide_engine::OcrOptions {
            languages: if langs.is_empty() {
                vec!["eng".to_string()]
            } else {
                langs
            },
            dpi: args.ocr_dpi,
            psm: None,
        };
        options.ocr_dpi = args.ocr_dpi.max(1);
    }
    let document = engine.parse_document(&options)?;

    let ser_opts = SerializeOptions {
        include_furniture: args.keep_furniture,
        mark_page_breaks: args.mark_page_breaks,
        include_provenance: args.provenance,
    };
    let output_text = match fmt {
        Fmt::Json => document.to_json(),
        Fmt::Markdown => document.to_markdown(&ser_opts),
        Fmt::Html => document.to_html(&ser_opts),
    };

    match &args.output {
        Some(path) => std::fs::write(path, &output_text)?,
        None => {
            print!("{output_text}");
            if !output_text.ends_with('\n') {
                println!();
            }
        }
    }
    let scanned = document
        .pages
        .iter()
        .filter(|p| p.source == oxide_engine::PageSource::Scanned)
        .count();
    eprintln!(
        "Parsed: {} body block(s) across {} page(s) ({:?} source, schema {}){}",
        document.body.len(),
        document.pages.len(),
        document.source,
        document.schema_version,
        if scanned > 0 && options.ocr.is_none() {
            format!("; {scanned} scanned page(s) routed to OCR (no engine; placeholder)")
        } else if scanned > 0 {
            format!("; {scanned} scanned page(s) OCR'd")
        } else {
            String::new()
        },
    );
    Ok(())
}

/// Extract structured key-value fields to JSON (the data-automation surface).
fn run_extract_fields(args: ExtractFieldsArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{DocType, ExtractOptions};

    if !matches!(args.format.to_lowercase().as_str(), "json") {
        return Err(format!("unknown --format '{}'; only json is supported", args.format).into());
    }

    let doc_type = match args.r#type.to_lowercase().as_str() {
        "auto" => None,
        other => Some(
            DocType::parse(other)
                .ok_or_else(|| format!("unknown --type '{other}'; use auto, invoice, receipt, form, or generic"))?,
        ),
    };

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let mut options = ExtractOptions {
        doc_type,
        pages: page_nums,
        min_confidence: args.min_confidence,
        ..Default::default()
    };
    if args.ocr {
        let langs: Vec<String> = args
            .ocr_lang
            .split(['+', ','])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        options.ocr = Some(build_ocr_engine()?);
        options.ocr_options = oxide_engine::OcrOptions {
            languages: if langs.is_empty() {
                vec!["eng".to_string()]
            } else {
                langs
            },
            dpi: args.ocr_dpi,
            psm: None,
        };
        options.ocr_dpi = args.ocr_dpi.max(1);
    }

    let result = engine.extract_fields(&options)?;
    let output_text = result.to_json();

    match &args.output {
        Some(path) => std::fs::write(path, &output_text)?,
        None => {
            print!("{output_text}");
            if !output_text.ends_with('\n') {
                println!();
            }
        }
    }
    let low = result.fields.iter().filter(|f| f.confidence < 0.5).count();
    eprintln!(
        "Extracted {} field(s){} from a {:?}{} document ({} line item(s)).",
        result.fields.len(),
        if low > 0 {
            format!(" ({low} low-confidence)")
        } else {
            String::new()
        },
        result.doc_type,
        if result.doc_type_forced { " (forced)" } else { " (auto-detected)" },
        result.line_items.len(),
    );
    Ok(())
}

/// Split a PDF into RAG-ready semantic chunks (the embedding-pipeline surface).
fn run_chunk(args: ChunkArgs) -> Result<(), Box<dyn Error>> {
    use oxide_engine::{ChunkOptions, ParseOptions};

    if !matches!(args.format.to_lowercase().as_str(), "json") {
        return Err(format!("unknown --format '{}'; only json is supported", args.format).into());
    }

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    // Parse once into the canonical model (OCR scanned pages when requested);
    // keep furniture in the model so chunking can decide per its own option.
    let mut parse_opts = ParseOptions {
        pages: page_nums,
        omit_furniture: false,
        ..ParseOptions::default()
    };
    if args.ocr {
        let langs: Vec<String> = args
            .ocr_lang
            .split(['+', ','])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        parse_opts.ocr = Some(build_ocr_engine()?);
        parse_opts.ocr_options = oxide_engine::OcrOptions {
            languages: if langs.is_empty() { vec!["eng".to_string()] } else { langs },
            dpi: args.ocr_dpi,
            psm: None,
        };
        parse_opts.ocr_dpi = args.ocr_dpi.max(1);
    }
    let document = engine.parse_document(&parse_opts)?;

    let chunk_opts = ChunkOptions {
        target_tokens: args.target_tokens.max(1),
        overlap_tokens: args.overlap,
        heading_context: !args.no_heading_context,
        split_on_headings: !args.no_split_on_headings,
        include_furniture: args.keep_furniture,
        isolate_tables: true,
    };
    let set = document.chunk(&chunk_opts);
    let output_text = set.to_json();

    match &args.output {
        Some(path) => std::fs::write(path, &output_text)?,
        None => {
            print!("{output_text}");
            if !output_text.ends_with('\n') {
                println!();
            }
        }
    }
    let total_tokens: usize = set.chunks.iter().map(|c| c.tokens).sum();
    let avg = if set.chunks.is_empty() {
        0
    } else {
        total_tokens / set.chunks.len()
    };
    let oversized = set.chunks.iter().filter(|c| c.oversized).count();
    eprintln!(
        "Chunked into {} chunk(s), ~{} tokens avg (target {}){}.",
        set.chunks.len(),
        avg,
        chunk_opts.target_tokens,
        if oversized > 0 {
            format!(", {oversized} oversized")
        } else {
            String::new()
        },
    );
    Ok(())
}

/// Score an extraction result vs ground truth (the benchmark scoring core).
fn run_eval_score(args: EvalScoreArgs) -> Result<(), Box<dyn Error>> {
    use std::io::Read;
    let input_json = match &args.input {
        Some(path) => std::fs::read_to_string(path)?,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
    };
    let output_json = oxide_engine::score_json(&input_json).map_err(|e| -> Box<dyn Error> { e.into() })?;
    match &args.output {
        Some(path) => std::fs::write(path, &output_json)?,
        None => println!("{output_json}"),
    }
    Ok(())
}

fn run_document_model(args: DocumentModelArgs) -> Result<(), Box<dyn Error>> {
    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let as_json = match args.format.to_lowercase().as_str() {
        "json" => true,
        "md" | "markdown" | "text" | "txt" => false,
        other => {
            return Err(format!("unknown --format '{other}'; use json, md, or text").into());
        }
    };

    let mut model = engine.build_document_model(&page_nums)?;
    if args.min_confidence > 0.0 {
        model.blocks.retain(|b| b.confidence >= args.min_confidence);
        // Re-densify the reading-order indices after filtering; ids are kept so
        // any caption/figure cross-links remain resolvable.
        for (i, b) in model.blocks.iter_mut().enumerate() {
            b.reading_order_index = i;
        }
    }

    let output_text = if as_json {
        serde_json::to_string_pretty(&model)?
    } else {
        oxide_engine::render_document_markdown(&model)
    };

    match &args.output {
        Some(path) => std::fs::write(path, &output_text)?,
        None => {
            print!("{output_text}");
            if !output_text.ends_with('\n') {
                println!();
            }
        }
    }
    eprintln!(
        "Document model: {} block(s) across {} page(s) ({:?} source)",
        model.blocks.len(),
        model.page_count,
        model.source
    );
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
            return Err(format!(
                "unknown format '{}'; use png, jpg, webp, or original",
                other
            )
            .into())
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
    use oxide_engine::{ImageEncoder, ImageOutputFormat, RenderMode};
    use std::io::Write;
    use zip::{write::FileOptions, CompressionMethod, ZipWriter};

    let dpi = args.dpi.clamp(24, 600);
    if dpi != args.dpi {
        eprintln!("Warning: DPI clamped to {} (valid range: 24-600)", dpi);
    }

    // Honor an explicit per-page pixel cap by exporting it for the engine's
    // `max_render_pixels()` resolver (also read by the svg/ps/eps sub-paths,
    // which all size their pages through `page_viewport`).
    if let Some(cap) = args.max_render_pixels {
        std::env::set_var("OXIDE_MAX_RENDER_PIXELS", cap.to_string());
    }

    // Vector output formats take separate paths.
    match args.format.to_lowercase().as_str() {
        "svg" => return run_render_svg(args, dpi),
        "ps" => return run_render_ps(args, dpi),
        "eps" => return run_render_eps(args, dpi),
        _ => {}
    }

    let format = match args.format.to_lowercase().as_str() {
        "png" => ImageOutputFormat::Png,
        "jpg" | "jpeg" => ImageOutputFormat::Jpeg,
        "webp" => ImageOutputFormat::Webp,
        other => {
            return Err(format!(
                "unknown format '{}'; use png, jpg, webp, svg, ps, or eps",
                other
            )
            .into())
        }
    };

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;
    let render_mode = RenderMode::from_name(&args.render_quality)
        .ok_or_else(|| format!("unknown render quality '{}'", args.render_quality))?;

    let out_file = std::fs::File::create(&args.output)?;
    let mut zip = ZipWriter::new(out_file);
    let zip_opts = FileOptions::<()>::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));

    let mut rendered_count = 0usize;
    for page_num in &page_nums {
        let buf = match engine.render_page_with_mode(*page_num, dpi, render_mode) {
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

/// PostScript output (`pdftops` / `pdftocairo -ps` equivalent): a single
/// DSC-conformant multi-page `.ps` document written directly to `--output`.
fn run_render_ps(args: RenderArgs, dpi: u32) -> Result<(), Box<dyn Error>> {
    use std::io::Write;

    let engine = open_engine(&args.pdf, &args.password)?;
    let total = engine.page_count()?;
    let page_nums = parse_page_range_cli(&args.pages, total)?;

    let (ps, rasterized) = engine.render_document_ps(&page_nums, dpi)?;

    // A single .ps document is the natural PostScript artifact (unlike the
    // per-page raster/SVG ZIP). If the output path still ends in .zip (the
    // default), retarget it to .ps so users get a usable file.
    let out_path = if args
        .output
        .extension()
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
    {
        args.output.with_extension("ps")
    } else {
        args.output.clone()
    };

    let mut file = std::fs::File::create(&out_path)?;
    file.write_all(ps.as_bytes())?;

    eprintln!(
        "Rendered {} page(s) to PostScript -> {} ({} page(s) used the raster-embed fallback)",
        page_nums.len(),
        out_path.display(),
        rasterized
    );
    Ok(())
}

/// EPS output (`pdftops -eps` / `pdftocairo -eps` equivalent): one
/// single-page, EPSF-conformant `.eps` per page inside the output ZIP (EPS is
/// single-page by definition).
fn run_render_eps(args: RenderArgs, dpi: u32) -> Result<(), Box<dyn Error>> {
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
        let (eps, rasterized) = match engine.render_page_eps(*page_num, dpi) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Warning: skipped page {}: {}", page_num, err);
                continue;
            }
        };
        if rasterized {
            rasterized_fallback += 1;
        }
        let filename = format!("page-{:03}.eps", page_num);
        zip.start_file(&filename, zip_opts)?;
        zip.write_all(eps.as_bytes())?;
        rendered += 1;
    }
    zip.finish()?;

    eprintln!(
        "Rendered {} page(s) to EPS -> {} ({} page(s) used the raster-embed fallback)",
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
                println!(
                    "                 {} ({} page(s))",
                    page_size_label(s),
                    s.page_count
                );
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
        assert_eq!(
            parse_page_selection_ordered("1,3,99", 5).unwrap(),
            vec![1, 3]
        );
        // Descending ranges go in reverse.
        assert_eq!(
            parse_page_selection_ordered("9-7", 9).unwrap(),
            vec![9, 8, 7]
        );
        // "all" still expands forward.
        assert_eq!(
            parse_page_selection_ordered("all", 3).unwrap(),
            vec![1, 2, 3]
        );
        // Non-contiguous subset.
        assert_eq!(
            parse_page_selection_ordered("1,3,5", 5).unwrap(),
            vec![1, 3, 5]
        );
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
        assert_eq!(expand_split_pattern("p%04d", 42), PathBuf::from("p0042"));
        // No directive: page number inserted before extension.
        assert_eq!(
            expand_split_pattern("doc.pdf", 3),
            PathBuf::from("doc-3.pdf")
        );
    }
}
