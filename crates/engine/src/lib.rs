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

pub mod analyzer;
pub mod content;
pub mod crypto;
pub mod document;
pub mod engine;
pub mod error;
pub mod filters;
pub mod fonts;
pub mod images;
pub mod object;
pub mod parser;
pub mod reader;
pub mod render;
pub mod text;

/// Semantic version of the oxide-engine crate.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use analyzer::{PdfAnalyzer, TextLayerAnalysis, TextLayerRecommendation};
pub use content::{
    concat_matrix, BlendMode, Color, ColorSpace, ContentOperation, ContentParser, GraphicsState,
    Matrix, Operand, TextState, IDENTITY_MATRIX,
};
pub use crypto::{
    aes128_cbc_decrypt, compute_encryption_key, decrypt_stream, decrypt_string, md5, object_key,
    verify_user_password, CryptMethod, EncryptionInfo, Rc4, PADDING,
};
pub use document::{PdfDocument, PdfPage};
pub use engine::{ContentEngine, PageResources};
pub use error::{OxideError, Result};
pub use filters::{decode_stream, decode_stream_lossless, DecodedStream, StreamDecodeStatus};
pub use fonts::{FontResolver, FontType};
pub use images::decoder::{ColorSpaceConverter, ImageDecoder, RawImage};
pub use images::encoder::{ImageEncoder, ImageOutputFormat};
pub use images::locator::{ImageLocateOptions, ImageLocator, ImageReference};
pub use images::smask::SmaskLoader;
pub use object::{PdfDictionary, PdfObject};
pub use reader::{EncryptionContext, PdfReader, XrefEntry};
pub use render::{
    flatten_cubic, flatten_path, get_fallback_font, rgb, rgba, AlphaMask, CachedGlyph, ClipMask,
    ColorSpaceHandler, DashState, FillRule, FlatPath, FontRasterizer, GlyphCache, GlyphCacheKey,
    ImagePainter, LinePainter, PageRenderer, Path, PathPainter, PathSegment, PixelBuffer,
    PixelColor, RenderColor, RenderQuality, Transform2D, Viewport, WuLineRenderer, BLACK, BLUE,
    GREEN, RED, TRANSPARENT, WHITE,
};
pub use text::{
    LineEnding, ReadingOrderReconstructor, TextChunk, TextCollector, TextExtractOptions,
    TextExtractor, TextFormatOptions, TextFormatter, TextLine,
};
