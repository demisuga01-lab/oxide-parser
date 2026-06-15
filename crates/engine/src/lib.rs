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

pub mod analyzer;
pub mod attachments;
pub mod cancel;
pub mod content;
pub mod crypto;
pub mod document;
pub mod engine;
pub mod error;
pub mod filters;
pub mod fonts;
#[cfg(feature = "fuzzing")]
pub mod fuzz;
pub mod fonts_report;
pub mod html;
pub mod info;
pub mod images;
pub mod object;
pub mod parser;
pub mod reader;
pub mod render;
pub mod signature;
pub mod text;
pub mod writer;

/// Semantic version of the oxide-engine crate.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use analyzer::{PdfAnalyzer, TextLayerAnalysis, TextLayerRecommendation};
pub use attachments::{
    extract_attachment, list_attachments, sanitize_filename, Attachment, AttachmentSource,
};
pub use cancel::CancelToken;
pub use content::{
    concat_matrix, BlendMode, Color, ColorSpace, ContentOperation, ContentParser, GraphicsState,
    Matrix, Operand, TextState, IDENTITY_MATRIX,
};
pub use crypto::{
    aes128_cbc_decrypt, aes256_cbc_decrypt, compute_encryption_key, decrypt_stream,
    decrypt_string, derive_v5_file_key_from_owner, derive_v5_file_key_from_user, md5, object_key,
    r6_hash, verify_user_password, verify_v5_owner_password, verify_v5_perms,
    verify_v5_user_password, CryptMethod, EncryptionInfo, Rc4, V5Fields, PADDING,
};
pub use document::{PdfDocument, PdfPage};
pub use engine::{ContentEngine, PageResources};
pub use error::{OxideError, Result};
pub use filters::{
    decode_stream, decode_stream_lossless, DecodedStream, StreamDecodeStatus,
    MAX_FLATE_DECOMPRESSED_BYTES,
};
pub use fonts::{FontResolver, FontType};
pub use fonts_report::{list_fonts, FontInfo};
pub use html::{HtmlExporter, HtmlMode, HtmlOptions};
pub use info::{
    decode_pdf_text_string, format_pdf_date, DocumentInfo, EncryptionReport, PageSize, Permissions,
};
pub use images::decoder::{ColorSpaceConverter, ImageDecoder, RawImage};
pub use images::encoder::{ImageEncoder, ImageOutputFormat};
pub use images::locator::{ImageLocateOptions, ImageLocator, ImageReference, InlineImageData};
pub use images::smask::SmaskLoader;
pub use object::{PdfDictionary, PdfObject};
pub use reader::{EncryptionContext, PdfReader, XrefEntry};
pub use render::{
    flatten_cubic, flatten_path, get_fallback_font, rgb, rgba, AlphaMask, CachedGlyph, ClipMask,
    ColorSpaceHandler, DashState, FillRule, FlatPath, FontRasterizer, GlyphCache, GlyphCacheKey,
    ImagePainter, LinePainter, PageRenderer, Path, PathPainter, PathSegment, PixelBuffer,
    PixelColor, RenderColor, RenderQuality, SvgPage, Transform2D, Viewport, WuLineRenderer, BLACK,
    BLUE, GREEN, RED, TRANSPARENT, WHITE,
};
pub use render::{render_page_svg, svg, text_decode};
pub use signature::{
    verify_signatures, CertInfo, Coverage, SignatureReport, SignatureValidity,
};
pub use text::{
    LineEnding, ReadingOrderReconstructor, TextChunk, TextCollector, TextExtractOptions,
    TextExtractor, TextFormatOptions, TextFormatter, TextLine,
};
pub use writer::{
    build_merged, build_subset, rewrite_references, serialize_object, write_document_roundtrip,
    OutputObject, PdfWriter,
};

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
