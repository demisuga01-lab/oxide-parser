//! Oxide Engine — Pure Rust PDF processing.
//!
//! This crate has no external PDF processing system dependencies:
//! - No Poppler
//! - No Ghostscript
//! - No ImageMagick
//! - No Java or Python
//! - No system-installed font rendering libraries
//!
//! PDF parsing, text extraction, image decoding, and page rendering are
//! implemented in Rust. Compression and image support come from crate
//! dependencies and do not shell out to external PDF tools.
//!
//! # Getting started
//!
//! [`ContentEngine`] is the main entry point. Open a PDF, then call the
//! operation you need:
//!
//! ```no_run
//! use oxide_engine::ContentEngine;
//!
//! # fn main() -> oxide_engine::Result<()> {
//! let engine = ContentEngine::open_path("input.pdf")?;
//!
//! // Document facts.
//! let pages = engine.page_count()?;
//! let info = engine.document_info()?;          // pdfinfo-equivalent
//! let fonts = engine.list_fonts()?;            // pdffonts-equivalent
//!
//! // Text & rendering.
//! let text = engine.get_page_text(1)?;         // pdftotext-equivalent
//! let png = engine.render_page_png_fast(1, 150)?; // pdftoppm-equivalent
//! let svg = engine.render_page_svg(1, 96)?;    // pdftocairo -svg
//!
//! // Conversion & reporting.
//! let attachments = engine.list_attachments()?;       // pdfdetach-equivalent
//! let sigs = engine.verify_signatures()?;             // pdfsig-equivalent
//! # let _ = (pages, info, fonts, text, png, svg, attachments, sigs);
//! # Ok(())
//! # }
//! ```
//!
//! Document manipulation produces new PDF bytes via the pure-Rust writer:
//!
//! ```no_run
//! use oxide_engine::{build_merged, PdfDocument};
//!
//! # fn main() -> oxide_engine::Result<()> {
//! let a = PdfDocument::open_path("a.pdf")?;
//! let b = PdfDocument::open_path("b.pdf")?;
//! // Merge all pages of both documents (pdfunite-equivalent).
//! let merged: Vec<u8> = build_merged(&[
//!     (&a, vec![1]),
//!     (&b, vec![1]),
//! ])?;
//! std::fs::write("merged.pdf", merged)?;
//! # Ok(())
//! # }
//! ```
//!
//! Runnable examples live in the crate's `examples/` directory
//! (`cargo run --example getting_started -- input.pdf`).

pub mod analysis;
pub mod analyzer;
pub mod attachments;
pub mod cancel;
pub mod chunk;
pub mod classify;
pub mod content;
pub mod crypto;
pub mod docmodel;
pub mod document;
pub mod engine;
pub mod error;
pub mod eval;
pub mod extract;
pub mod filters;
pub mod fonts;
pub mod fonts_report;
#[cfg(feature = "fuzzing")]
pub mod fuzz;
pub mod html;
pub mod images;
pub mod info;
pub mod object;
pub mod ocr;
pub mod parse;
pub mod parser;
pub mod reader;
pub mod render;
pub mod semantic;
pub mod signature;
pub mod structural;
pub mod text;
pub mod writer;

/// Semantic version of the oxide-engine crate.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use analyzer::{PdfAnalyzer, TextLayerAnalysis, TextLayerRecommendation};
pub use attachments::{
    extract_attachment, list_attachments, sanitize_filename, Attachment, AttachmentSource,
};
pub use cancel::CancelToken;
pub use classify::{
    classify_document, classify_page, ClassifyConfig, PageClassification, PageSource,
};
pub use content::{
    concat_matrix, BlendMode, Color, ColorSpace, ContentOperation, ContentParser, GraphicsState,
    Matrix, Operand, TextState, IDENTITY_MATRIX,
};
pub use crypto::{
    aes128_cbc_decrypt, aes256_cbc_decrypt, build_encryption, compute_encryption_key,
    decrypt_stream, decrypt_string, derive_v5_file_key_from_owner, derive_v5_file_key_from_user,
    encrypt_bytes, md5, object_key, r6_hash, verify_user_password, verify_v5_owner_password,
    verify_v5_perms, verify_v5_user_password, CryptMethod, EncryptAlgorithm, EncryptParams,
    EncryptState, EncryptionInfo, Rc4, V5Fields, PADDING,
};
pub use analysis::graphics::{
    collect_graphics, collect_graphics_with_images, DrawnGraphics, ImagePlacement, Rect, Segment,
};
pub use docmodel::{
    render_markdown as render_document_markdown, ClassifiedType, DocBlock, DocumentModel, ListItem,
    ModelSource, RegionKind,
};
pub use parse::{
    parse, Block, BlockKind, Document, DocumentMetadata, ImageHandling, ImageRef, InlineSpan,
    InlineText, ListEntry, Page, ParseOptions, SerializeOptions, SourceInfo, SCHEMA_VERSION,
};
pub use ocr::preprocess::{
    binarize_otsu, binarize_sauvola, detect_skew, preprocess, Binarization, PreprocessConfig,
};
pub use ocr::{OcrEngine, OcrImage, OcrOptions, OcrPage, OcrWord};
pub use extract::{
    extract_fields, DocType, ExtractOptions, ExtractedFields, Field, FieldSource, FieldValue,
    LineItem, ValueHint, FIELDS_SCHEMA_VERSION,
};
pub use chunk::{
    chunk, estimate_tokens, Chunk, ChunkOptions, ChunkSet, CHUNK_SCHEMA_VERSION,
};
pub use eval::{score, score_json, ScoreInput, ScoreOutput};
pub use document::{PdfDocument, PdfPage};
pub use engine::{max_render_pixels, ContentEngine, PageResources, DEFAULT_MAX_RENDER_PIXELS};
pub use error::{OxideError, Result};
pub use filters::{
    decode_stream, decode_stream_lossless, flate_encode, DecodedStream, StreamDecodeStatus,
    MAX_FLATE_DECOMPRESSED_BYTES,
};
pub use fonts::variations::{AxisValue, VariationRequest};
pub use fonts::{FontResolver, FontType};
pub use fonts_report::{list_fonts, FontInfo};
pub use html::{HtmlExporter, HtmlMode, HtmlOptions};
pub use images::decoder::{ColorSpaceConverter, ImageDecoder, RawImage};
pub use images::encoder::{ImageEncoder, ImageOutputFormat};
pub use images::locator::{ImageLocateOptions, ImageLocator, ImageReference, InlineImageData};
pub use images::smask::SmaskLoader;
pub use info::{
    decode_pdf_text_string, format_pdf_date, DocumentInfo, EncryptionReport, PageSize, Permissions,
};
pub use object::{PdfDictionary, PdfObject};
pub use reader::{EncryptionContext, PdfReader, XrefEntry};
pub use render::{
    flatten_cubic, flatten_path, get_fallback_font, rgb, rgba, AlphaMask, CachedGlyph, ClipMask,
    ColorSpaceHandler, DashState, FillRule, FlatPath, FontRasterizer, GlyphCache, GlyphCacheKey,
    ImagePainter, LinePainter, PageRenderer, Path, PathPainter, PathSegment, PixelBuffer,
    PixelColor, RenderColor, RenderMode, RenderQuality, SvgPage, Transform2D, Viewport,
    WuLineRenderer, BLACK, BLUE, GREEN, RED, TRANSPARENT, WHITE,
};
pub use render::{render_page_svg, svg, text_decode};
pub use semantic::{SemanticDocument, SemanticElement, SemanticMcid, SemanticSource};
pub use signature::{verify_signatures, CertInfo, Coverage, SignatureReport, SignatureValidity};
pub use structural::{encrypt, optimize, repair, rotate_pages, OptimizeReport, Rotation};
pub use text::{
    LineEnding, MarkedTextChunk, ReadingOrderReconstructor, TextChunk, TextCollector,
    TextExtractOptions, TextExtractor, TextFormatOptions, TextFormatter, TextLine,
};
pub use writer::{
    build_merged, build_subset, rewrite_document, rewrite_document_objects,
    rewrite_document_with_mode, rewrite_references, serialize_object, write_document_roundtrip,
    OutputObject, PdfWriter, WriterMode,
};

/// The curated high-level embedding surface.
///
/// The crate root re-exports a large, flat surface that mixes the high-level
/// embedder API with low-level building blocks (renderer internals, font and
/// crypto primitives, the raw object/reader types). That breadth is useful for
/// advanced consumers but obscures the path most embedders want. This module
/// gathers exactly the types and functions needed to **open a document, parse
/// it to the canonical [`Document`] model, serialize it (Markdown / JSON /
/// HTML), chunk it for RAG, extract key-value fields, run the structural ops,
/// and inject an OCR backend** — nothing more.
///
/// ```no_run
/// use oxide_engine::prelude::*;
///
/// # fn main() -> oxide_engine::Result<()> {
/// let engine = ContentEngine::open_path("input.pdf")?;
///
/// // Parse → canonical model → Markdown / JSON for RAG and automation.
/// let doc = engine.parse_document(&ParseOptions::default())?;
/// let markdown = doc.to_markdown_default();
/// let json = doc.to_json();
///
/// // RAG-ready semantic chunks.
/// let chunks = doc.chunk(&ChunkOptions::default());
///
/// // Structured key-value fields (invoice/receipt/form).
/// let fields = engine.extract_fields(&ExtractOptions::default())?;
/// # let _ = (markdown, json, chunks, fields);
/// # Ok(())
/// # }
/// ```
///
/// To inject OCR for scanned pages, supply a concrete [`OcrEngine`] (e.g. the
/// `oxide-ocr-tesseract` crate) via [`ParseOptions::ocr`] /
/// [`ExtractOptions::ocr`]. Everything here works **without** the CLI, the
/// server, or any non-Rust binding.
pub mod prelude {
    pub use crate::chunk::{chunk, Chunk, ChunkOptions, ChunkSet, CHUNK_SCHEMA_VERSION};
    pub use crate::engine::ContentEngine;
    pub use crate::error::{OxideError, Result};
    pub use crate::eval::{score, score_json, ScoreInput, ScoreOutput};
    pub use crate::extract::{
        extract_fields, DocType, ExtractOptions, ExtractedFields, Field, FieldValue, LineItem,
    };
    pub use crate::ocr::{OcrEngine, OcrOptions};
    pub use crate::parse::{
        parse, Block, BlockKind, Document, DocumentMetadata, Page, ParseOptions, SerializeOptions,
        SourceInfo, SCHEMA_VERSION,
    };
    pub use crate::writer::{build_merged, build_subset};
    pub use crate::ENGINE_VERSION;
}

/// Compile-time guarantee that the parsed engine is thread-safe.
///
/// `ContentEngine` (and the `PdfDocument`/`PdfReader` it owns) must stay
/// `Send + Sync` so a single parsed document can be wrapped in an `Arc` and
/// shared across rayon worker threads for parallel text extraction and page
/// rendering — instead of cloning and re-parsing the whole PDF per thread.
/// The only interior mutability in the reader (the object-stream cache) is an
/// `RwLock` precisely to preserve this. If a future change reintroduces a
/// `RefCell`/`Rc`/raw pointer into the parse tree, this assertion fails to
/// compile, flagging the regression immediately.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ContentEngine>();
    assert_send_sync::<PdfDocument>();
    assert_send_sync::<PdfReader>();
};
