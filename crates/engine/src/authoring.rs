//! High-level PDF authoring API.
//!
//! Coordinates use native PDF user space: the origin is at the bottom-left of
//! the page, x grows to the right, and y grows upward. Use
//! [`PdfPageBuilder::pdf_y_from_top`] when a top-left UI coordinate is more
//! convenient.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Cursor;
use std::path::Path;

use crate::content::{Color, ColorSpace, LineCap, LineDash, LineJoin};
use crate::error::{OxideError, Result};
use crate::filters::flate_encode;
use crate::fonts::encoding::{zapf_dingbats_name_to_unicode, Encoding};
use crate::fonts::glyph_list::glyph_name_to_unicode;
use crate::images::decoder::{ImageDecoder, RawImage};
use crate::object::{PdfDictionary, PdfObject};
use crate::render::get_fallback_font;
use crate::writer::{OutputObject, PdfWriter, WriterMode};

const DEFAULT_FONT_SIZE: f64 = 12.0;
const DEFAULT_LINE_HEIGHT: f64 = 1.2;
const BUILTIN_UNICODE_RESOURCE_NAME: &str = "OxideUnicode";
const KAPPA: f64 = 0.552_284_749_830_793_6;

/// A high-level PDF document builder.
///
/// The builder creates a fresh object graph and serializes it through
/// [`PdfWriter`]. The default writer mode is
/// [`WriterMode::XrefStreamWithObjStm`] for compact modern output.
#[derive(Debug, Clone)]
pub struct PdfBuilder {
    pages: Vec<PdfPageBuilder>,
    metadata: PdfMetadata,
    writer_mode: WriterMode,
    version: String,
    custom_fonts: Vec<CustomFont>,
    images: Vec<AuthoredImage>,
}

impl Default for PdfBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfBuilder {
    /// Create an empty PDF document.
    pub fn new() -> Self {
        Self {
            pages: Vec::new(),
            metadata: PdfMetadata::default(),
            writer_mode: WriterMode::XrefStreamWithObjStm,
            version: "1.7".to_string(),
            custom_fonts: Vec::new(),
            images: Vec::new(),
        }
    }

    /// Replace the document metadata dictionary.
    pub fn set_metadata(&mut self, metadata: PdfMetadata) -> &mut Self {
        self.metadata = metadata;
        self
    }

    pub fn metadata_mut(&mut self) -> &mut PdfMetadata {
        &mut self.metadata
    }

    pub fn set_title(&mut self, title: impl Into<String>) -> &mut Self {
        self.metadata.title = Some(title.into());
        self
    }

    pub fn set_author(&mut self, author: impl Into<String>) -> &mut Self {
        self.metadata.author = Some(author.into());
        self
    }

    pub fn set_subject(&mut self, subject: impl Into<String>) -> &mut Self {
        self.metadata.subject = Some(subject.into());
        self
    }

    pub fn set_keywords(&mut self, keywords: impl Into<String>) -> &mut Self {
        self.metadata.keywords = Some(keywords.into());
        self
    }

    pub fn set_creator(&mut self, creator: impl Into<String>) -> &mut Self {
        self.metadata.creator = Some(creator.into());
        self
    }

    /// Select the low-level writer mode used by [`Self::to_bytes`].
    pub fn with_writer_mode(mut self, mode: WriterMode) -> Self {
        self.writer_mode = mode;
        self
    }

    /// Set the PDF header version. Defaults to `1.7`.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Add a page and return its drawing surface.
    pub fn add_page(&mut self, size: PageSize) -> &mut PdfPageBuilder {
        self.pages.push(PdfPageBuilder::new(size));
        self.pages.last_mut().expect("page was just pushed")
    }

    /// Add a page with margins intended for paragraph/layout helpers.
    pub fn add_page_with_margins(
        &mut self,
        size: PageSize,
        margins: Margins,
    ) -> &mut PdfPageBuilder {
        self.pages.push(PdfPageBuilder::with_margins(size, margins));
        self.pages.last_mut().expect("page was just pushed")
    }

    pub fn pages(&self) -> &[PdfPageBuilder] {
        &self.pages
    }

    pub fn pages_mut(&mut self) -> &mut [PdfPageBuilder] {
        &mut self.pages
    }

    /// Register a TrueType font program for authored text.
    ///
    /// The current authoring layer embeds the complete font program as a
    /// Type0/CIDFontType2 font with Identity-H encoding and a ToUnicode CMap.
    /// Font subsetting is deliberately left to the next size-optimization pass.
    pub fn register_font_bytes(
        &mut self,
        name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<FontFace> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(OxideError::MalformedPdf(
                "authoring: custom font bytes are empty".to_string(),
            ));
        }
        TrueTypeMetrics::parse(&bytes)?;
        let id = CustomFontId(self.custom_fonts.len() as u32);
        let base_name = sanitize_pdf_name(&name.into(), &format!("OxideCustomFont{}", id.0 + 1));
        self.custom_fonts.push(CustomFont {
            id,
            base_name,
            bytes,
        });
        Ok(FontFace::Custom(id))
    }

    /// Alias for [`Self::register_font_bytes`] that documents the supported
    /// whole-font embedding format.
    pub fn register_truetype_font_bytes(
        &mut self,
        name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<FontFace> {
        self.register_font_bytes(name, bytes)
    }

    /// Register a JPEG image XObject. The source JPEG bytes are embedded
    /// directly with `DCTDecode`; they are decoded only to read dimensions and
    /// color channel count.
    pub fn add_jpeg_image(&mut self, bytes: impl Into<Vec<u8>>) -> Result<ImageHandle> {
        let bytes = bytes.into();
        let (_, width, height, channels) = ImageDecoder::decode_jpeg_with_info(&bytes)?;
        let color_space = ImageColorSpace::from_channels(channels)?;
        Ok(self.push_image(AuthoredImage {
            width,
            height,
            color_space,
            bits_per_component: 8,
            data: bytes,
            filter: ImageFilter::DctDecode,
            smask: None,
        }))
    }

    /// Register PNG bytes as an Image XObject. RGB/gray samples are Flate
    /// compressed; alpha is split into a PDF soft mask.
    pub fn add_png_image(&mut self, bytes: &[u8]) -> Result<ImageHandle> {
        let decoded = decode_png_for_authoring(bytes)?;
        self.add_raw_image(decoded)
    }

    /// Register interleaved RGB samples as a Flate-compressed Image XObject.
    pub fn add_rgb_image(
        &mut self,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<ImageHandle> {
        self.add_raw_image(RawImage {
            width,
            height,
            channels: 3,
            bits_per_sample: 8,
            pixels,
        })
    }

    /// Register interleaved RGBA samples as a Flate-compressed Image XObject
    /// with an `SMask` for alpha.
    pub fn add_rgba_image(
        &mut self,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<ImageHandle> {
        self.add_raw_image(RawImage {
            width,
            height,
            channels: 4,
            bits_per_sample: 8,
            pixels,
        })
    }

    fn add_raw_image(&mut self, raw: RawImage) -> Result<ImageHandle> {
        if !raw.is_valid() || raw.bits_per_sample != 8 {
            return Err(OxideError::MalformedPdf(
                "authoring: image samples must be non-empty 8-bit data".to_string(),
            ));
        }
        let authored = authored_image_from_raw(raw)?;
        Ok(self.push_image(authored))
    }

    fn push_image(&mut self, image: AuthoredImage) -> ImageHandle {
        let handle = ImageHandle(self.images.len() as u32);
        self.images.push(image);
        handle
    }

    fn image(&self, handle: ImageHandle) -> Result<&AuthoredImage> {
        self.images.get(handle.0 as usize).ok_or_else(|| {
            OxideError::MalformedPdf(format!(
                "authoring: image handle {} was not registered on this document",
                handle.0
            ))
        })
    }

    fn custom_font(&self, id: CustomFontId) -> Result<&CustomFont> {
        self.custom_fonts.get(id.0 as usize).ok_or_else(|| {
            OxideError::MalformedPdf(format!(
                "authoring: custom font handle {} was not registered on this document",
                id.0
            ))
        })
    }

    /// Serialize the authored document to PDF bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.pages.is_empty() {
            return Err(OxideError::MalformedPdf(
                "authoring: cannot save a PDF with no pages".to_string(),
            ));
        }

        let font_plan = FontBuildPlan::from_builder(self)?;
        let image_plan = ImageBuildPlan::from_builder(self)?;
        let objects = AuthoredObjects::build(self, &font_plan, &image_plan)?;
        PdfWriter::new(objects.objects, objects.catalog_number)
            .with_info(objects.info_number)
            .with_version(self.version.clone())
            .with_mode(self.writer_mode)
            .write()
    }

    /// Write the authored document to a file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_bytes()?)?;
        Ok(())
    }
}

/// Document information dictionary fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
}

impl PdfMetadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = Some(keywords.into());
        self
    }

    pub fn creator(mut self, creator: impl Into<String>) -> Self {
        self.creator = Some(creator.into());
        self
    }

    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.author.is_none()
            && self.subject.is_none()
            && self.keywords.is_none()
            && self.creator.is_none()
    }
}

/// Handle returned by [`PdfBuilder::add_jpeg_image`],
/// [`PdfBuilder::add_png_image`], or raw image registration helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageHandle(u32);

impl ImageHandle {
    pub fn index(self) -> u32 {
        self.0
    }
}

/// Handle for a document-registered custom font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CustomFontId(u32);

impl CustomFontId {
    pub fn index(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone)]
struct CustomFont {
    id: CustomFontId,
    base_name: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
}

impl ImageColorSpace {
    fn from_channels(channels: u8) -> Result<Self> {
        match channels {
            1 => Ok(Self::DeviceGray),
            3 => Ok(Self::DeviceRGB),
            4 => Ok(Self::DeviceCMYK),
            _ => Err(OxideError::UnsupportedFeature(format!(
                "authoring: unsupported image channel count {channels}"
            ))),
        }
    }

    fn pdf_name(self) -> &'static str {
        match self {
            Self::DeviceGray => "DeviceGray",
            Self::DeviceRGB => "DeviceRGB",
            Self::DeviceCMYK => "DeviceCMYK",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFilter {
    DctDecode,
    FlateDecode,
}

impl ImageFilter {
    fn pdf_name(self) -> &'static str {
        match self {
            Self::DctDecode => "DCTDecode",
            Self::FlateDecode => "FlateDecode",
        }
    }
}

#[derive(Debug, Clone)]
struct AuthoredImage {
    width: u32,
    height: u32,
    color_space: ImageColorSpace,
    bits_per_component: u8,
    data: Vec<u8>,
    filter: ImageFilter,
    smask: Option<AuthoredSoftMask>,
}

#[derive(Debug, Clone)]
struct AuthoredSoftMask {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// Page dimensions in PDF points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
}

impl PageSize {
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };
    pub const LEGAL: Self = Self {
        width: 612.0,
        height: 1008.0,
    };
    pub const A3: Self = Self {
        width: 841.8898,
        height: 1190.5512,
    };
    pub const A4: Self = Self {
        width: 595.2756,
        height: 841.8898,
    };
    pub const A5: Self = Self {
        width: 419.5276,
        height: 595.2756,
    };

    pub fn custom(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    pub fn inches(width: f64, height: f64) -> Self {
        Self {
            width: width * 72.0,
            height: height * 72.0,
        }
    }

    pub fn mm(width: f64, height: f64) -> Self {
        const POINTS_PER_MM: f64 = 72.0 / 25.4;
        Self {
            width: width * POINTS_PER_MM,
            height: height * POINTS_PER_MM,
        }
    }

    pub fn landscape(self) -> Self {
        Self {
            width: self.width.max(self.height),
            height: self.width.min(self.height),
        }
    }

    pub fn portrait(self) -> Self {
        Self {
            width: self.width.min(self.height),
            height: self.width.max(self.height),
        }
    }
}

/// Page margins in points, used by layout helpers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

impl Default for Margins {
    fn default() -> Self {
        Self::all(0.0)
    }
}

impl Margins {
    pub fn all(value: f64) -> Self {
        Self {
            left: value,
            right: value,
            top: value,
            bottom: value,
        }
    }

    pub fn vertical_horizontal(vertical: f64, horizontal: f64) -> Self {
        Self {
            left: horizontal,
            right: horizontal,
            top: vertical,
            bottom: vertical,
        }
    }
}

/// One authored PDF page.
#[derive(Debug, Clone)]
pub struct PdfPageBuilder {
    size: PageSize,
    margins: Margins,
    commands: Vec<PageCommand>,
}

impl PdfPageBuilder {
    pub fn new(size: PageSize) -> Self {
        Self {
            size,
            margins: Margins::default(),
            commands: Vec::new(),
        }
    }

    pub fn with_margins(size: PageSize, margins: Margins) -> Self {
        Self {
            size,
            margins,
            commands: Vec::new(),
        }
    }

    pub fn size(&self) -> PageSize {
        self.size
    }

    pub fn margins(&self) -> Margins {
        self.margins
    }

    pub fn set_margins(&mut self, margins: Margins) -> &mut Self {
        self.margins = margins;
        self
    }

    /// Convert a distance from the top page edge into native PDF y space.
    pub fn pdf_y_from_top(&self, y_from_top: f64) -> f64 {
        self.size.height - y_from_top
    }

    /// Draw one text run at a baseline position.
    pub fn draw_text(
        &mut self,
        text: impl Into<String>,
        x: f64,
        y: f64,
        style: &TextStyle,
    ) -> Result<&mut Self> {
        let text = text.into();
        validate_text_for_font(&text, &style.font)?;
        self.commands.push(PageCommand::Text {
            text,
            x,
            y,
            style: style.clone(),
        });
        Ok(self)
    }

    /// Draw one text run where y is measured from the top page edge.
    pub fn draw_text_from_top(
        &mut self,
        text: impl Into<String>,
        x: f64,
        y_from_top: f64,
        style: &TextStyle,
    ) -> Result<&mut Self> {
        self.draw_text(text, x, self.pdf_y_from_top(y_from_top), style)
    }

    pub fn draw_text_line(
        &mut self,
        text: impl Into<String>,
        x: f64,
        y: f64,
        style: &TextStyle,
    ) -> Result<&mut Self> {
        self.draw_text(text, x, y, style)
    }

    /// Return the width of a text run in points for the selected style.
    pub fn text_width(&self, text: &str, style: &TextStyle) -> Result<f64> {
        text_width(text, style)
    }

    /// Break text into lines that fit `max_width` in points.
    pub fn wrap_text(&self, text: &str, max_width: f64, style: &TextStyle) -> Result<Vec<String>> {
        wrap_text(text, max_width, style)
    }

    /// Draw wrapped paragraph text. Returns the emitted lines.
    pub fn draw_paragraph(
        &mut self,
        text: &str,
        x: f64,
        y: f64,
        max_width: f64,
        style: &TextStyle,
        paragraph: &ParagraphStyle,
    ) -> Result<Vec<String>> {
        let lines = self.wrap_text(text, max_width, style)?;
        let line_height = paragraph.line_height_points(style.size);
        for (idx, line) in lines.iter().enumerate() {
            let width = self.text_width(line, style)?;
            let aligned_x = match paragraph.align {
                TextAlign::Left => x,
                TextAlign::Center => x + (max_width - width) / 2.0,
                TextAlign::Right => x + max_width - width,
            };
            self.draw_text(line.clone(), aligned_x, y - idx as f64 * line_height, style)?;
        }
        Ok(lines)
    }

    pub fn draw_line(
        &mut self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        style: &GraphicsStyle,
    ) -> &mut Self {
        self.commands.push(PageCommand::Path {
            path: PathBuilder::new().move_to(x1, y1).line_to(x2, y2),
            style: style.clone().stroke_only_if_unpainted(),
        });
        self
    }

    pub fn draw_rect(
        &mut self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        style: &GraphicsStyle,
    ) -> &mut Self {
        self.commands.push(PageCommand::Rect {
            x,
            y,
            width,
            height,
            style: style.clone(),
        });
        self
    }

    pub fn draw_rounded_rect(
        &mut self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        radius: f64,
        style: &GraphicsStyle,
    ) -> &mut Self {
        let radius = radius
            .max(0.0)
            .min(width.abs() / 2.0)
            .min(height.abs() / 2.0);
        let x2 = x + width;
        let y2 = y + height;
        let k = radius * KAPPA;
        let path = PathBuilder::new()
            .move_to(x + radius, y)
            .line_to(x2 - radius, y)
            .curve_to(x2 - radius + k, y, x2, y + radius - k, x2, y + radius)
            .line_to(x2, y2 - radius)
            .curve_to(x2, y2 - radius + k, x2 - radius + k, y2, x2 - radius, y2)
            .line_to(x + radius, y2)
            .curve_to(x + radius - k, y2, x, y2 - radius + k, x, y2 - radius)
            .line_to(x, y + radius)
            .curve_to(x, y + radius - k, x + radius - k, y, x + radius, y)
            .close();
        self.draw_path(path, style)
    }

    pub fn draw_circle(
        &mut self,
        cx: f64,
        cy: f64,
        radius: f64,
        style: &GraphicsStyle,
    ) -> &mut Self {
        self.draw_ellipse(cx, cy, radius, radius, style)
    }

    pub fn draw_ellipse(
        &mut self,
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        style: &GraphicsStyle,
    ) -> &mut Self {
        let kx = rx * KAPPA;
        let ky = ry * KAPPA;
        let path = PathBuilder::new()
            .move_to(cx + rx, cy)
            .curve_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry)
            .curve_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy)
            .curve_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry)
            .curve_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy)
            .close();
        self.draw_path(path, style)
    }

    pub fn draw_polygon(&mut self, points: &[(f64, f64)], style: &GraphicsStyle) -> &mut Self {
        if points.is_empty() {
            return self;
        }
        let mut path = PathBuilder::new().move_to(points[0].0, points[0].1);
        for &(x, y) in &points[1..] {
            path = path.line_to(x, y);
        }
        self.draw_path(path.close(), style)
    }

    pub fn draw_path(&mut self, path: PathBuilder, style: &GraphicsStyle) -> &mut Self {
        self.commands.push(PageCommand::Path {
            path,
            style: style.clone(),
        });
        self
    }

    /// Place a registered image in the rectangle `(x, y, width, height)`.
    pub fn draw_image(
        &mut self,
        image: ImageHandle,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> &mut Self {
        self.commands.push(PageCommand::Image {
            image,
            x,
            y,
            width,
            height,
        });
        self
    }

    fn fonts_used(&self) -> Vec<FontFace> {
        let mut out = Vec::new();
        for command in &self.commands {
            if let PageCommand::Text { style, .. } = command {
                push_unique_font(&mut out, style.font);
            }
        }
        out
    }

    fn unicode_chars_used_by_font(&self) -> HashMap<FontFace, Vec<char>> {
        let mut out: HashMap<FontFace, Vec<char>> = HashMap::new();
        let mut seen: HashMap<FontFace, BTreeSet<char>> = HashMap::new();
        for command in &self.commands {
            if let PageCommand::Text { text, style, .. } = command {
                if style.font.is_embedded_unicode() {
                    let chars = out.entry(style.font).or_default();
                    let seen_for_font = seen.entry(style.font).or_default();
                    for ch in text.chars() {
                        if seen_for_font.insert(ch) {
                            chars.push(ch);
                        }
                    }
                }
            }
        }
        out
    }

    fn images_used(&self) -> Vec<ImageHandle> {
        let mut out = Vec::new();
        for command in &self.commands {
            if let PageCommand::Image { image, .. } = command {
                push_unique_image(&mut out, *image);
            }
        }
        out
    }
}

/// Text font selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FontFace {
    Standard(StandardFont),
    /// Bundled Liberation Sans, embedded as a Type0 TrueType font with
    /// ToUnicode. This is the Part-1 Unicode authoring baseline.
    BuiltinUnicode,
    /// Document-registered custom TrueType font, embedded whole as Type0.
    Custom(CustomFontId),
}

impl Default for FontFace {
    fn default() -> Self {
        Self::Standard(StandardFont::Helvetica)
    }
}

impl FontFace {
    fn is_embedded_unicode(self) -> bool {
        matches!(self, Self::BuiltinUnicode | Self::Custom(_))
    }
}

/// The PDF Standard-14 font faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StandardFont {
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    Symbol,
    ZapfDingbats,
}

impl StandardFont {
    pub fn base_font_name(self) -> &'static str {
        match self {
            Self::Helvetica => "Helvetica",
            Self::HelveticaBold => "Helvetica-Bold",
            Self::HelveticaOblique => "Helvetica-Oblique",
            Self::HelveticaBoldOblique => "Helvetica-BoldOblique",
            Self::TimesRoman => "Times-Roman",
            Self::TimesBold => "Times-Bold",
            Self::TimesItalic => "Times-Italic",
            Self::TimesBoldItalic => "Times-BoldItalic",
            Self::Courier => "Courier",
            Self::CourierBold => "Courier-Bold",
            Self::CourierOblique => "Courier-Oblique",
            Self::CourierBoldOblique => "Courier-BoldOblique",
            Self::Symbol => "Symbol",
            Self::ZapfDingbats => "ZapfDingbats",
        }
    }

    fn fallback_font_name(self) -> &'static str {
        self.base_font_name()
    }

    fn built_in_encoding(self) -> Option<&'static str> {
        match self {
            Self::Symbol => Some("SymbolEncoding"),
            Self::ZapfDingbats => Some("ZapfDingbatsEncoding"),
            _ => None,
        }
    }
}

/// Text drawing style.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub font: FontFace,
    pub size: f64,
    pub fill: Color,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: FontFace::default(),
            size: DEFAULT_FONT_SIZE,
            fill: Color::black(),
        }
    }
}

impl TextStyle {
    pub fn new(font: FontFace, size: f64) -> Self {
        Self {
            font,
            size,
            fill: Color::black(),
        }
    }

    pub fn standard(font: StandardFont, size: f64) -> Self {
        Self::new(FontFace::Standard(font), size)
    }

    pub fn unicode(size: f64) -> Self {
        Self::new(FontFace::BuiltinUnicode, size)
    }

    pub fn custom(font: CustomFontId, size: f64) -> Self {
        Self::new(FontFace::Custom(font), size)
    }

    pub fn fill(mut self, color: Color) -> Self {
        self.fill = color;
        self
    }
}

/// Paragraph alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Paragraph helper options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParagraphStyle {
    pub align: TextAlign,
    /// Multiplier over the text size. `1.2` is the default.
    pub line_height: f64,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        Self {
            align: TextAlign::Left,
            line_height: DEFAULT_LINE_HEIGHT,
        }
    }
}

impl ParagraphStyle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    pub fn line_height(mut self, line_height: f64) -> Self {
        self.line_height = line_height;
        self
    }

    fn line_height_points(self, font_size: f64) -> f64 {
        font_size * self.line_height.max(0.1)
    }
}

/// One table column with fixed width in PDF points.
#[derive(Debug, Clone, PartialEq)]
pub struct TableColumn {
    pub width: f64,
    pub align: TextAlign,
}

impl TableColumn {
    pub fn new(width: f64) -> Self {
        Self {
            width,
            align: TextAlign::Left,
        }
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }
}

/// Styling shared by authored tables.
#[derive(Debug, Clone, PartialEq)]
pub struct TableStyle {
    pub border_color: Color,
    pub header_fill: Color,
    pub row_fill: Option<Color>,
    pub padding: f64,
    pub line_width: f64,
    pub paragraph: ParagraphStyle,
}

impl Default for TableStyle {
    fn default() -> Self {
        Self {
            border_color: Color::device_rgb(0.28, 0.32, 0.36),
            header_fill: Color::device_rgb(0.9, 0.93, 0.96),
            row_fill: None,
            padding: 4.0,
            line_width: 0.5,
            paragraph: ParagraphStyle::new(),
        }
    }
}

impl TableStyle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn padding(mut self, padding: f64) -> Self {
        self.padding = padding.max(0.0);
        self
    }

    pub fn border(mut self, color: Color, line_width: f64) -> Self {
        self.border_color = color;
        self.line_width = line_width.max(0.0);
        self
    }

    pub fn header_fill(mut self, color: Color) -> Self {
        self.header_fill = color;
        self
    }

    pub fn row_fill(mut self, color: Option<Color>) -> Self {
        self.row_fill = color;
        self
    }
}

/// A text cell in an authored table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub text: String,
    pub style: Option<TextStyle>,
    pub background: Option<Color>,
    pub align: Option<TextAlign>,
}

impl TableCell {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            background: None,
            align: None,
        }
    }

    pub fn style(mut self, style: TextStyle) -> Self {
        self.style = Some(style);
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = Some(align);
        self
    }
}

impl From<&str> for TableCell {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<String> for TableCell {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

/// A table row.
#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

impl TableRow {
    pub fn new(cells: Vec<TableCell>) -> Self {
        Self { cells }
    }
}

/// Fixed-column table renderer with wrapped text and repeatable headers.
#[derive(Debug, Clone, PartialEq)]
pub struct TableBuilder {
    columns: Vec<TableColumn>,
    header: Option<TableRow>,
    rows: Vec<TableRow>,
    body_style: TextStyle,
    header_style: TextStyle,
    style: TableStyle,
}

impl TableBuilder {
    pub fn new(columns: Vec<TableColumn>) -> Self {
        Self {
            columns,
            header: None,
            rows: Vec::new(),
            body_style: TextStyle::standard(StandardFont::Helvetica, 9.0),
            header_style: TextStyle::standard(StandardFont::HelveticaBold, 9.0),
            style: TableStyle::default(),
        }
    }

    pub fn body_style(mut self, style: TextStyle) -> Self {
        self.body_style = style;
        self
    }

    pub fn header_style(mut self, style: TextStyle) -> Self {
        self.header_style = style;
        self
    }

    pub fn style(mut self, style: TableStyle) -> Self {
        self.style = style;
        self
    }

    pub fn set_header<I, C>(&mut self, cells: I) -> &mut Self
    where
        I: IntoIterator<Item = C>,
        C: Into<TableCell>,
    {
        self.header = Some(TableRow::new(cells.into_iter().map(Into::into).collect()));
        self
    }

    pub fn add_row<I, C>(&mut self, cells: I) -> &mut Self
    where
        I: IntoIterator<Item = C>,
        C: Into<TableCell>,
    {
        self.rows
            .push(TableRow::new(cells.into_iter().map(Into::into).collect()));
        self
    }

    pub fn rows(&self) -> &[TableRow] {
        &self.rows
    }

    pub fn columns(&self) -> &[TableColumn] {
        &self.columns
    }

    /// Draw the whole table on one page at a top-left anchor and return the
    /// consumed height. Long tables should use [`FlowDocument::add_table`].
    pub fn draw_on_page(&self, page: &mut PdfPageBuilder, x: f64, top_y: f64) -> Result<f64> {
        let mut cursor = top_y;
        if let Some(header) = &self.header {
            let height = self.measure_row(page, header, true)?;
            self.render_row(page, header, x, cursor, height, true)?;
            cursor -= height;
        }
        for row in &self.rows {
            let height = self.measure_row(page, row, false)?;
            self.render_row(page, row, x, cursor, height, false)?;
            cursor -= height;
        }
        Ok(top_y - cursor)
    }

    fn total_width(&self) -> f64 {
        self.columns.iter().map(|col| col.width.max(0.0)).sum()
    }

    fn measure_row(&self, page: &PdfPageBuilder, row: &TableRow, header: bool) -> Result<f64> {
        let mut height: f64 = 0.0;
        for (idx, column) in self.columns.iter().enumerate() {
            let cell = row.cells.get(idx);
            let style = self.cell_style(cell, header);
            let inner_width = (column.width - self.style.padding * 2.0).max(1.0);
            let text = cell.map(|cell| cell.text.as_str()).unwrap_or("");
            let line_count = page.wrap_text(text, inner_width, &style)?.len().max(1);
            let line_height = self.style.paragraph.line_height_points(style.size);
            height = height.max(line_count as f64 * line_height + self.style.padding * 2.0);
        }
        Ok(height.max(self.style.padding * 2.0 + self.body_style.size))
    }

    fn render_row(
        &self,
        page: &mut PdfPageBuilder,
        row: &TableRow,
        x: f64,
        top_y: f64,
        height: f64,
        header: bool,
    ) -> Result<()> {
        let mut x_cursor = x;
        for (idx, column) in self.columns.iter().enumerate() {
            let cell = row.cells.get(idx);
            let fill = cell
                .and_then(|cell| cell.background.clone())
                .or_else(|| header.then_some(self.style.header_fill.clone()))
                .or_else(|| self.style.row_fill.clone());
            page.draw_rect(
                x_cursor,
                top_y - height,
                column.width,
                height,
                &GraphicsStyle::fill_stroke(
                    fill.unwrap_or_else(|| Color::device_gray(1.0)),
                    self.style.border_color.clone(),
                    self.style.line_width,
                ),
            );

            let style = self.cell_style(cell, header);
            let align = cell.and_then(|cell| cell.align).unwrap_or(column.align);
            let inner_width = (column.width - self.style.padding * 2.0).max(1.0);
            let text = cell.map(|cell| cell.text.as_str()).unwrap_or("");
            let lines = page.wrap_text(text, inner_width, &style)?;
            let line_height = self.style.paragraph.line_height_points(style.size);
            let mut baseline = top_y - self.style.padding - style.size;
            for line in lines {
                let text_width = page.text_width(&line, &style)?;
                let text_x = match align {
                    TextAlign::Left => x_cursor + self.style.padding,
                    TextAlign::Center => {
                        x_cursor + self.style.padding + (inner_width - text_width) / 2.0
                    }
                    TextAlign::Right => x_cursor + self.style.padding + inner_width - text_width,
                };
                page.draw_text(line, text_x, baseline, &style)?;
                baseline -= line_height;
            }
            x_cursor += column.width;
        }
        Ok(())
    }

    fn cell_style(&self, cell: Option<&TableCell>, header: bool) -> TextStyle {
        cell.and_then(|cell| cell.style.clone()).unwrap_or_else(|| {
            if header {
                self.header_style.clone()
            } else {
                self.body_style.clone()
            }
        })
    }
}

/// Single-column layout helper that creates pages as content overflows.
#[derive(Debug, Clone)]
pub struct FlowDocument {
    builder: PdfBuilder,
    page_size: PageSize,
    margins: Margins,
    current_page: usize,
    cursor_y: f64,
}

impl FlowDocument {
    pub fn new(page_size: PageSize, margins: Margins) -> Self {
        let mut builder = PdfBuilder::new();
        builder.add_page_with_margins(page_size, margins);
        Self {
            builder,
            page_size,
            margins,
            current_page: 0,
            cursor_y: page_size.height - margins.top,
        }
    }

    pub fn builder(&self) -> &PdfBuilder {
        &self.builder
    }

    pub fn builder_mut(&mut self) -> &mut PdfBuilder {
        &mut self.builder
    }

    pub fn into_builder(self) -> PdfBuilder {
        self.builder
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.builder.save(path)
    }

    pub fn add_heading(&mut self, text: &str, level: u8) -> Result<&mut Self> {
        let size = match level {
            0 | 1 => 22.0,
            2 => 16.0,
            _ => 13.0,
        };
        let style = TextStyle::standard(StandardFont::HelveticaBold, size)
            .fill(Color::device_rgb(0.08, 0.12, 0.16));
        self.add_paragraph(
            text,
            &style,
            &ParagraphStyle::new().line_height(if level <= 1 { 1.15 } else { 1.2 }),
        )?;
        self.add_spacer(if level <= 1 { 8.0 } else { 5.0 });
        Ok(self)
    }

    pub fn add_paragraph(
        &mut self,
        text: &str,
        style: &TextStyle,
        paragraph: &ParagraphStyle,
    ) -> Result<&mut Self> {
        let width = self.content_width();
        let lines = wrap_text(text, width, style)?;
        let line_height = paragraph.line_height_points(style.size);
        for line in lines {
            self.ensure_space(line_height)?;
            let text_width = text_width(&line, style)?;
            let x = match paragraph.align {
                TextAlign::Left => self.margins.left,
                TextAlign::Center => self.margins.left + (width - text_width) / 2.0,
                TextAlign::Right => self.margins.left + width - text_width,
            };
            let y = self.cursor_y;
            self.current_page_mut().draw_text(line, x, y, style)?;
            self.cursor_y -= line_height;
        }
        Ok(self)
    }

    pub fn add_list<I, S>(
        &mut self,
        items: I,
        ordered: bool,
        style: &TextStyle,
        paragraph: &ParagraphStyle,
    ) -> Result<&mut Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let base_left = self.margins.left;
        let width = (self.content_width() - 18.0).max(1.0);
        let line_height = paragraph.line_height_points(style.size);
        for (idx, item) in items.into_iter().enumerate() {
            let marker = if ordered {
                format!("{}.", idx + 1)
            } else {
                "*".to_string()
            };
            let lines = wrap_text(item.as_ref(), width, style)?;
            self.ensure_space(line_height)?;
            let marker_y = self.cursor_y;
            self.current_page_mut()
                .draw_text(marker, base_left, marker_y, style)?;
            for (line_idx, line) in lines.into_iter().enumerate() {
                if line_idx > 0 {
                    self.cursor_y -= line_height;
                    self.ensure_space(line_height)?;
                }
                let line_y = self.cursor_y;
                self.current_page_mut()
                    .draw_text(line, base_left + 18.0, line_y, style)?;
            }
            self.cursor_y -= line_height;
        }
        Ok(self)
    }

    pub fn add_image(&mut self, image: ImageHandle, width: f64, height: f64) -> Result<&mut Self> {
        self.builder.image(image)?;
        self.ensure_space(height)?;
        let x = self.margins.left;
        let y = self.cursor_y - height;
        self.current_page_mut()
            .draw_image(image, x, y, width, height);
        self.cursor_y = y;
        Ok(self)
    }

    pub fn add_table(&mut self, table: &TableBuilder) -> Result<&mut Self> {
        let x = self.margins.left;
        let available_width = self.content_width();
        if table.total_width() > available_width + 0.0001 {
            return Err(OxideError::ResourceLimit(format!(
                "authoring: table width {} exceeds flow content width {}",
                fmt_num(table.total_width()),
                fmt_num(available_width)
            )));
        }

        let mut header_height = 0.0;
        if let Some(header) = &table.header {
            header_height = table.measure_row(self.current_page_ref(), header, true)?;
            self.ensure_space(header_height)?;
            let top = self.cursor_y;
            table.render_row(self.current_page_mut(), header, x, top, header_height, true)?;
            self.cursor_y -= header_height;
        }

        for row in &table.rows {
            let row_height = table.measure_row(self.current_page_ref(), row, false)?;
            if self.cursor_y - row_height < self.margins.bottom {
                self.add_page_break();
                if let Some(header) = &table.header {
                    self.ensure_space(header_height)?;
                    let top = self.cursor_y;
                    table.render_row(
                        self.current_page_mut(),
                        header,
                        x,
                        top,
                        header_height,
                        true,
                    )?;
                    self.cursor_y -= header_height;
                }
            }
            let top = self.cursor_y;
            table.render_row(self.current_page_mut(), row, x, top, row_height, false)?;
            self.cursor_y -= row_height;
        }
        Ok(self)
    }

    pub fn add_spacer(&mut self, height: f64) -> &mut Self {
        let height = height.max(0.0);
        if self.cursor_y - height < self.margins.bottom {
            self.add_page_break();
        } else {
            self.cursor_y -= height;
        }
        self
    }

    pub fn add_page_break(&mut self) -> &mut Self {
        self.builder
            .add_page_with_margins(self.page_size, self.margins);
        self.current_page = self.builder.pages.len() - 1;
        self.cursor_y = self.page_size.height - self.margins.top;
        self
    }

    fn ensure_space(&mut self, height: f64) -> Result<()> {
        if height > self.page_size.height - self.margins.top - self.margins.bottom {
            return Err(OxideError::ResourceLimit(format!(
                "authoring: flow block height {} exceeds usable page height",
                fmt_num(height)
            )));
        }
        if self.cursor_y - height < self.margins.bottom {
            self.add_page_break();
        }
        Ok(())
    }

    fn content_width(&self) -> f64 {
        (self.page_size.width - self.margins.left - self.margins.right).max(1.0)
    }

    fn current_page_ref(&self) -> &PdfPageBuilder {
        &self.builder.pages[self.current_page]
    }

    fn current_page_mut(&mut self) -> &mut PdfPageBuilder {
        &mut self.builder.pages[self.current_page]
    }
}

/// Stroke/fill state for vector drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphicsStyle {
    pub stroke: Option<Color>,
    pub fill: Option<Color>,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub dash: LineDash,
}

impl Default for GraphicsStyle {
    fn default() -> Self {
        Self {
            stroke: Some(Color::black()),
            fill: None,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash: LineDash::default(),
        }
    }
}

impl GraphicsStyle {
    pub fn stroke(color: Color, line_width: f64) -> Self {
        Self {
            stroke: Some(color),
            line_width,
            ..Default::default()
        }
    }

    pub fn fill(color: Color) -> Self {
        Self {
            stroke: None,
            fill: Some(color),
            ..Default::default()
        }
    }

    pub fn fill_stroke(fill: Color, stroke: Color, line_width: f64) -> Self {
        Self {
            stroke: Some(stroke),
            fill: Some(fill),
            line_width,
            ..Default::default()
        }
    }

    pub fn line_cap(mut self, cap: LineCap) -> Self {
        self.line_cap = cap;
        self
    }

    pub fn line_join(mut self, join: LineJoin) -> Self {
        self.line_join = join;
        self
    }

    pub fn dash(mut self, pattern: Vec<f64>, phase: f64) -> Self {
        self.dash = LineDash { pattern, phase };
        self
    }

    fn stroke_only_if_unpainted(mut self) -> Self {
        if self.stroke.is_none() && self.fill.is_none() {
            self.stroke = Some(Color::black());
        }
        self.fill = None;
        self
    }
}

/// Arbitrary path builder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathBuilder {
    segments: Vec<PathSegment>,
}

impl PathBuilder {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    pub fn move_to(mut self, x: f64, y: f64) -> Self {
        self.segments.push(PathSegment::MoveTo(x, y));
        self
    }

    pub fn line_to(mut self, x: f64, y: f64) -> Self {
        self.segments.push(PathSegment::LineTo(x, y));
        self
    }

    pub fn curve_to(mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) -> Self {
        self.segments
            .push(PathSegment::CurveTo(x1, y1, x2, y2, x3, y3));
        self
    }

    pub fn close(mut self) -> Self {
        self.segments.push(PathSegment::Close);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
enum PathSegment {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CurveTo(f64, f64, f64, f64, f64, f64),
    Close,
}

#[derive(Debug, Clone)]
enum PageCommand {
    Text {
        text: String,
        x: f64,
        y: f64,
        style: TextStyle,
    },
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        style: GraphicsStyle,
    },
    Path {
        path: PathBuilder,
        style: GraphicsStyle,
    },
    Image {
        image: ImageHandle,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}

struct AuthoredObjects {
    objects: Vec<OutputObject>,
    catalog_number: u32,
    info_number: Option<u32>,
}

impl AuthoredObjects {
    fn build(
        builder: &PdfBuilder,
        font_plan: &FontBuildPlan,
        image_plan: &ImageBuildPlan,
    ) -> Result<Self> {
        let catalog_number = 1u32;
        let pages_number = 2u32;
        let page_count = builder.pages.len();
        let page_start = 3u32;
        let content_start = page_start + page_count as u32;
        let mut next = content_start + page_count as u32;

        let mut image_objects = Vec::new();
        let mut image_refs = HashMap::new();
        for handle in &image_plan.images {
            let image = builder.image(*handle)?;
            let smask_number = if image.smask.is_some() {
                Some(alloc(&mut next))
            } else {
                None
            };
            let image_number = alloc(&mut next);
            image_refs.insert(*handle, image_number);
            if let (Some(number), Some(mask)) = (smask_number, image.smask.as_ref()) {
                image_objects.push(OutputObject {
                    number,
                    object: PdfObject::Stream {
                        dict: smask_image_dict(mask),
                        raw: mask.data.clone(),
                    },
                });
            }
            image_objects.push(OutputObject {
                number: image_number,
                object: PdfObject::Stream {
                    dict: authored_image_dict(image, smask_number),
                    raw: image.data.clone(),
                },
            });
        }

        let mut font_objects = Vec::new();
        let mut font_refs = HashMap::new();
        for font in &font_plan.fonts {
            let built = build_font_objects(*font, builder, &mut next, font_plan)?;
            font_refs.insert(*font, built.top_object);
            font_objects.extend(built.objects);
        }

        let info_number = if builder.metadata.is_empty() {
            None
        } else {
            let number = next;
            next += 1;
            Some(number)
        };

        let mut objects = Vec::new();
        objects.push(OutputObject {
            number: catalog_number,
            object: PdfObject::Dictionary(catalog_dict(pages_number)),
        });
        objects.push(OutputObject {
            number: pages_number,
            object: PdfObject::Dictionary(pages_tree_dict(page_start, page_count)),
        });

        let resource_refs = PageResourceRefs {
            font_plan,
            font_refs: &font_refs,
            image_plan,
            image_refs: &image_refs,
        };

        for (idx, page) in builder.pages.iter().enumerate() {
            let page_number = page_start + idx as u32;
            let content_number = content_start + idx as u32;
            let content = build_content_stream(page, font_plan, image_plan)?;
            objects.push(OutputObject {
                number: page_number,
                object: PdfObject::Dictionary(page_dict(
                    pages_number,
                    content_number,
                    page,
                    &resource_refs,
                )?),
            });
            objects.push(OutputObject {
                number: content_number,
                object: PdfObject::Stream {
                    dict: PdfDictionary::empty(),
                    raw: content,
                },
            });
        }

        objects.extend(image_objects);
        objects.extend(font_objects);
        if let Some(number) = info_number {
            objects.push(OutputObject {
                number,
                object: PdfObject::Dictionary(info_dict(&builder.metadata)),
            });
        } else {
            let _ = next;
        }

        Ok(Self {
            objects,
            catalog_number,
            info_number,
        })
    }
}

struct BuiltFontObjects {
    top_object: u32,
    objects: Vec<OutputObject>,
}

struct PageResourceRefs<'a> {
    font_plan: &'a FontBuildPlan,
    font_refs: &'a HashMap<FontFace, u32>,
    image_plan: &'a ImageBuildPlan,
    image_refs: &'a HashMap<ImageHandle, u32>,
}

#[derive(Debug)]
struct FontBuildPlan {
    fonts: Vec<FontFace>,
    resource_names: HashMap<FontFace, String>,
    embedded: HashMap<FontFace, EmbeddedFontPlan>,
}

#[derive(Debug, Clone)]
struct EmbeddedFontPlan {
    cids: HashMap<char, u16>,
    chars: Vec<char>,
}

impl FontBuildPlan {
    fn from_builder(builder: &PdfBuilder) -> Result<Self> {
        let mut fonts = Vec::new();
        let mut chars_by_font: HashMap<FontFace, Vec<char>> = HashMap::new();
        let mut seen_by_font: HashMap<FontFace, BTreeSet<char>> = HashMap::new();

        for page in &builder.pages {
            for font in page.fonts_used() {
                if let FontFace::Custom(id) = font {
                    builder.custom_font(id)?;
                }
                push_unique_font(&mut fonts, font);
            }
            for (font, chars) in page.unicode_chars_used_by_font() {
                let out = chars_by_font.entry(font).or_default();
                let seen = seen_by_font.entry(font).or_default();
                for ch in chars {
                    if seen.insert(ch) {
                        out.push(ch);
                    }
                }
            }
        }

        let mut resource_names = HashMap::new();
        for (idx, font) in fonts.iter().enumerate() {
            resource_names.insert(*font, format!("F{}", idx + 1));
        }

        let mut embedded = HashMap::new();
        for font in &fonts {
            if !font.is_embedded_unicode() {
                continue;
            }
            let chars = chars_by_font.remove(font).unwrap_or_default();
            let mut cids = HashMap::new();
            for (idx, ch) in chars.iter().enumerate() {
                let cid = u16::try_from(idx + 1).map_err(|_| {
                    OxideError::ResourceLimit(format!(
                        "authoring: too many unique Unicode scalar values for font {font:?}"
                    ))
                })?;
                cids.insert(*ch, cid);
            }
            embedded.insert(*font, EmbeddedFontPlan { cids, chars });
        }

        Ok(Self {
            fonts,
            resource_names,
            embedded,
        })
    }

    #[cfg(test)]
    fn from_pages(pages: &[PdfPageBuilder]) -> Result<Self> {
        let mut builder = PdfBuilder::new();
        builder.pages = pages.to_vec();
        Self::from_builder(&builder)
    }

    fn resource_name(&self, font: FontFace) -> Result<&str> {
        self.resource_names
            .get(&font)
            .map(String::as_str)
            .ok_or_else(|| {
                OxideError::MalformedPdf("authoring: font was not registered".to_string())
            })
    }

    fn embedded_plan(&self, font: FontFace) -> Result<&EmbeddedFontPlan> {
        self.embedded.get(&font).ok_or_else(|| {
            OxideError::MalformedPdf(format!("authoring: embedded font {font:?} was not planned"))
        })
    }

    fn cid_for(&self, font: FontFace, ch: char) -> Result<u16> {
        self.embedded_plan(font)?
            .cids
            .get(&ch)
            .copied()
            .ok_or_else(|| {
                OxideError::MalformedPdf(format!(
                    "authoring: Unicode character {ch:?} has no CID for {font:?}"
                ))
            })
    }
}

fn push_unique_font(fonts: &mut Vec<FontFace>, font: FontFace) {
    if !fonts.contains(&font) {
        fonts.push(font);
    }
}

#[derive(Debug)]
struct ImageBuildPlan {
    images: Vec<ImageHandle>,
    resource_names: HashMap<ImageHandle, String>,
}

impl ImageBuildPlan {
    fn from_builder(builder: &PdfBuilder) -> Result<Self> {
        let mut images = Vec::new();
        for page in &builder.pages {
            for image in page.images_used() {
                builder.image(image)?;
                push_unique_image(&mut images, image);
            }
        }
        let mut resource_names = HashMap::new();
        for (idx, image) in images.iter().enumerate() {
            resource_names.insert(*image, format!("Im{}", idx + 1));
        }
        Ok(Self {
            images,
            resource_names,
        })
    }

    fn resource_name(&self, image: ImageHandle) -> Result<&str> {
        self.resource_names
            .get(&image)
            .map(String::as_str)
            .ok_or_else(|| {
                OxideError::MalformedPdf("authoring: image was not registered".to_string())
            })
    }
}

fn push_unique_image(images: &mut Vec<ImageHandle>, image: ImageHandle) {
    if !images.contains(&image) {
        images.push(image);
    }
}

fn catalog_dict(pages_number: u32) -> PdfDictionary {
    dict(&[
        ("Type", PdfObject::Name("Catalog".to_string())),
        ("Pages", reference(pages_number)),
    ])
}

fn pages_tree_dict(page_start: u32, page_count: usize) -> PdfDictionary {
    let kids = (0..page_count)
        .map(|idx| reference(page_start + idx as u32))
        .collect();
    dict(&[
        ("Type", PdfObject::Name("Pages".to_string())),
        ("Count", PdfObject::Integer(page_count as i64)),
        ("Kids", PdfObject::Array(kids)),
    ])
}

fn page_dict(
    parent: u32,
    contents: u32,
    page: &PdfPageBuilder,
    resource_refs: &PageResourceRefs<'_>,
) -> Result<PdfDictionary> {
    let mut fonts = PdfDictionary::empty();
    for font in page.fonts_used() {
        let resource = resource_refs.font_plan.resource_name(font)?;
        let Some(number) = resource_refs.font_refs.get(&font).copied() else {
            return Err(OxideError::MalformedPdf(
                "authoring: font object missing".to_string(),
            ));
        };
        fonts.insert(resource, reference(number));
    }

    let mut xobjects = PdfDictionary::empty();
    for image in page.images_used() {
        let resource = resource_refs.image_plan.resource_name(image)?;
        let Some(number) = resource_refs.image_refs.get(&image).copied() else {
            return Err(OxideError::MalformedPdf(
                "authoring: image object missing".to_string(),
            ));
        };
        xobjects.insert(resource, reference(number));
    }

    let mut resources = PdfDictionary::empty();
    if !fonts.is_empty() {
        resources.insert("Font", PdfObject::Dictionary(fonts));
    }
    if !xobjects.is_empty() {
        resources.insert("XObject", PdfObject::Dictionary(xobjects));
    }
    resources.insert(
        "ProcSet",
        PdfObject::Array(vec![
            PdfObject::Name("PDF".to_string()),
            PdfObject::Name("Text".to_string()),
            PdfObject::Name("ImageB".to_string()),
            PdfObject::Name("ImageC".to_string()),
            PdfObject::Name("ImageI".to_string()),
        ]),
    );

    Ok(dict(&[
        ("Type", PdfObject::Name("Page".to_string())),
        ("Parent", reference(parent)),
        (
            "MediaBox",
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                pdf_number(page.size.width),
                pdf_number(page.size.height),
            ]),
        ),
        ("Resources", PdfObject::Dictionary(resources)),
        ("Contents", reference(contents)),
    ]))
}

fn info_dict(metadata: &PdfMetadata) -> PdfDictionary {
    let mut info = PdfDictionary::empty();
    if let Some(value) = &metadata.title {
        info.insert("Title", PdfObject::String(pdf_text_string(value)));
    }
    if let Some(value) = &metadata.author {
        info.insert("Author", PdfObject::String(pdf_text_string(value)));
    }
    if let Some(value) = &metadata.subject {
        info.insert("Subject", PdfObject::String(pdf_text_string(value)));
    }
    if let Some(value) = &metadata.keywords {
        info.insert("Keywords", PdfObject::String(pdf_text_string(value)));
    }
    if let Some(value) = &metadata.creator {
        info.insert("Creator", PdfObject::String(pdf_text_string(value)));
    }
    info.insert(
        "Producer",
        PdfObject::String(pdf_text_string("Oxide PDF SDK")),
    );
    info
}

fn authored_image_dict(image: &AuthoredImage, smask_number: Option<u32>) -> PdfDictionary {
    let mut dict = dict(&[
        ("Type", PdfObject::Name("XObject".to_string())),
        ("Subtype", PdfObject::Name("Image".to_string())),
        ("Width", PdfObject::Integer(i64::from(image.width))),
        ("Height", PdfObject::Integer(i64::from(image.height))),
        (
            "ColorSpace",
            PdfObject::Name(image.color_space.pdf_name().to_string()),
        ),
        (
            "BitsPerComponent",
            PdfObject::Integer(i64::from(image.bits_per_component)),
        ),
        (
            "Filter",
            PdfObject::Name(image.filter.pdf_name().to_string()),
        ),
    ]);
    if let Some(number) = smask_number {
        dict.insert("SMask", reference(number));
    }
    dict
}

fn smask_image_dict(mask: &AuthoredSoftMask) -> PdfDictionary {
    dict(&[
        ("Type", PdfObject::Name("XObject".to_string())),
        ("Subtype", PdfObject::Name("Image".to_string())),
        ("Width", PdfObject::Integer(i64::from(mask.width))),
        ("Height", PdfObject::Integer(i64::from(mask.height))),
        ("ColorSpace", PdfObject::Name("DeviceGray".to_string())),
        ("BitsPerComponent", PdfObject::Integer(8)),
        ("Filter", PdfObject::Name("FlateDecode".to_string())),
    ])
}

fn authored_image_from_raw(raw: RawImage) -> Result<AuthoredImage> {
    let expected = raw.byte_count();
    if raw.pixels.len() != expected {
        return Err(OxideError::MalformedPdf(format!(
            "authoring: image data has {} bytes but expected {expected}",
            raw.pixels.len()
        )));
    }

    let (samples, color_space, smask) = match raw.channels {
        1 => (raw.pixels, ImageColorSpace::DeviceGray, None),
        2 => {
            let mut samples = Vec::with_capacity(raw.pixel_count());
            let mut alpha = Vec::with_capacity(raw.pixel_count());
            for px in raw.pixels.chunks_exact(2) {
                samples.push(px[0]);
                alpha.push(px[1]);
            }
            (
                samples,
                ImageColorSpace::DeviceGray,
                Some(AuthoredSoftMask {
                    width: raw.width,
                    height: raw.height,
                    data: flate_encode(&alpha, 9),
                }),
            )
        }
        3 => (raw.pixels, ImageColorSpace::DeviceRGB, None),
        4 => {
            let mut samples = Vec::with_capacity(raw.pixel_count() * 3);
            let mut alpha = Vec::with_capacity(raw.pixel_count());
            for px in raw.pixels.chunks_exact(4) {
                samples.extend_from_slice(&px[..3]);
                alpha.push(px[3]);
            }
            (
                samples,
                ImageColorSpace::DeviceRGB,
                Some(AuthoredSoftMask {
                    width: raw.width,
                    height: raw.height,
                    data: flate_encode(&alpha, 9),
                }),
            )
        }
        channels => {
            return Err(OxideError::UnsupportedFeature(format!(
                "authoring: unsupported raw image channel count {channels}"
            )))
        }
    };

    Ok(AuthoredImage {
        width: raw.width,
        height: raw.height,
        color_space,
        bits_per_component: 8,
        data: flate_encode(&samples, 9),
        filter: ImageFilter::FlateDecode,
        smask,
    })
}

fn decode_png_for_authoring(bytes: &[u8]) -> Result<RawImage> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|err| OxideError::MalformedPdf(format!("authoring: cannot read PNG: {err}")))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|err| OxideError::MalformedPdf(format!("authoring: cannot decode PNG: {err}")))?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err(OxideError::UnsupportedFeature(format!(
            "authoring: PNG bit depth {:?} is not supported after expansion",
            info.bit_depth
        )));
    }
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err(OxideError::UnsupportedFeature(
                "authoring: indexed PNG did not expand to samples".to_string(),
            ))
        }
    };
    Ok(RawImage {
        width: info.width,
        height: info.height,
        channels,
        bits_per_sample: 8,
        pixels: buf[..info.buffer_size()].to_vec(),
    })
}

fn build_font_objects(
    font: FontFace,
    builder: &PdfBuilder,
    next: &mut u32,
    plan: &FontBuildPlan,
) -> Result<BuiltFontObjects> {
    match font {
        FontFace::Standard(standard) => {
            let number = alloc(next);
            Ok(BuiltFontObjects {
                top_object: number,
                objects: vec![OutputObject {
                    number,
                    object: PdfObject::Dictionary(standard_font_dict(standard)?),
                }],
            })
        }
        FontFace::BuiltinUnicode => build_embedded_type0_font(
            next,
            font,
            BUILTIN_UNICODE_RESOURCE_NAME,
            builtin_unicode_font_bytes()?,
            plan,
        ),
        FontFace::Custom(id) => {
            let custom = builder.custom_font(id)?;
            debug_assert_eq!(custom.id, id);
            build_embedded_type0_font(next, font, &custom.base_name, &custom.bytes, plan)
        }
    }
}

fn standard_font_dict(font: StandardFont) -> Result<PdfDictionary> {
    let widths = standard_widths(font)?;
    let mut font_dict = dict(&[
        ("Type", PdfObject::Name("Font".to_string())),
        ("Subtype", PdfObject::Name("Type1".to_string())),
        (
            "BaseFont",
            PdfObject::Name(font.base_font_name().to_string()),
        ),
        ("FirstChar", PdfObject::Integer(32)),
        ("LastChar", PdfObject::Integer(255)),
        (
            "Widths",
            PdfObject::Array(widths.into_iter().map(pdf_number).collect()),
        ),
    ]);
    if font.built_in_encoding().is_none() {
        font_dict.insert("Encoding", PdfObject::Name("WinAnsiEncoding".to_string()));
    }
    Ok(font_dict)
}

fn build_embedded_type0_font(
    next: &mut u32,
    font: FontFace,
    base_name: &str,
    font_bytes: &[u8],
    plan: &FontBuildPlan,
) -> Result<BuiltFontObjects> {
    let type0_number = alloc(next);
    let descendant_number = alloc(next);
    let descriptor_number = alloc(next);
    let font_file_number = alloc(next);
    let to_unicode_number = alloc(next);
    let cid_to_gid_number = alloc(next);

    let metrics = TrueTypeMetrics::parse(font_bytes)?;
    let cmap_name = format!("{base_name}ToUnicode");

    let mut objects = Vec::new();
    objects.push(OutputObject {
        number: type0_number,
        object: PdfObject::Dictionary(dict(&[
            ("Type", PdfObject::Name("Font".to_string())),
            ("Subtype", PdfObject::Name("Type0".to_string())),
            ("BaseFont", PdfObject::Name(base_name.to_string())),
            ("Encoding", PdfObject::Name("Identity-H".to_string())),
            (
                "DescendantFonts",
                PdfObject::Array(vec![reference(descendant_number)]),
            ),
            ("ToUnicode", reference(to_unicode_number)),
        ])),
    });

    objects.push(OutputObject {
        number: descendant_number,
        object: PdfObject::Dictionary(cid_font_dict(
            base_name,
            descriptor_number,
            cid_to_gid_number,
            font,
            plan,
            &metrics,
        )?),
    });
    objects.push(OutputObject {
        number: descriptor_number,
        object: PdfObject::Dictionary(font_descriptor_dict(base_name, font_file_number, &metrics)),
    });

    let mut font_file_dict = PdfDictionary::empty();
    font_file_dict.insert("Length1", PdfObject::Integer(font_bytes.len() as i64));
    objects.push(OutputObject {
        number: font_file_number,
        object: PdfObject::Stream {
            dict: font_file_dict,
            raw: font_bytes.to_vec(),
        },
    });

    objects.push(OutputObject {
        number: to_unicode_number,
        object: PdfObject::Stream {
            dict: PdfDictionary::empty(),
            raw: build_to_unicode_cmap(font, plan, &cmap_name)?,
        },
    });
    objects.push(OutputObject {
        number: cid_to_gid_number,
        object: PdfObject::Stream {
            dict: PdfDictionary::empty(),
            raw: build_cid_to_gid_map(font, plan, &metrics)?,
        },
    });

    Ok(BuiltFontObjects {
        top_object: type0_number,
        objects,
    })
}

fn cid_font_dict(
    base_name: &str,
    descriptor_number: u32,
    cid_to_gid_number: u32,
    font: FontFace,
    plan: &FontBuildPlan,
    metrics: &TrueTypeMetrics,
) -> Result<PdfDictionary> {
    let widths = unicode_width_array(font, plan, metrics)?;
    Ok(dict(&[
        ("Type", PdfObject::Name("Font".to_string())),
        ("Subtype", PdfObject::Name("CIDFontType2".to_string())),
        ("BaseFont", PdfObject::Name(base_name.to_string())),
        (
            "CIDSystemInfo",
            PdfObject::Dictionary(dict(&[
                ("Registry", PdfObject::String(b"Adobe".to_vec())),
                ("Ordering", PdfObject::String(b"Identity".to_vec())),
                ("Supplement", PdfObject::Integer(0)),
            ])),
        ),
        ("FontDescriptor", reference(descriptor_number)),
        ("DW", PdfObject::Integer(500)),
        ("W", PdfObject::Array(widths)),
        ("CIDToGIDMap", reference(cid_to_gid_number)),
    ]))
}

fn font_descriptor_dict(
    base_name: &str,
    font_file_number: u32,
    metrics: &TrueTypeMetrics,
) -> PdfDictionary {
    dict(&[
        ("Type", PdfObject::Name("FontDescriptor".to_string())),
        ("FontName", PdfObject::Name(base_name.to_string())),
        ("Flags", PdfObject::Integer(32)),
        (
            "FontBBox",
            PdfObject::Array(metrics.bbox.iter().copied().map(pdf_number).collect()),
        ),
        ("ItalicAngle", PdfObject::Integer(0)),
        ("Ascent", pdf_number(metrics.ascender)),
        ("Descent", pdf_number(metrics.descender)),
        ("CapHeight", pdf_number(metrics.cap_height)),
        ("StemV", PdfObject::Integer(80)),
        ("FontFile2", reference(font_file_number)),
    ])
}

fn unicode_width_array(
    font: FontFace,
    plan: &FontBuildPlan,
    metrics: &TrueTypeMetrics,
) -> Result<Vec<PdfObject>> {
    let embedded = plan.embedded_plan(font)?;
    if embedded.chars.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::with_capacity(embedded.chars.len() + 1);
    items.push(PdfObject::Integer(1));
    let widths = plan
        .embedded_plan(font)?
        .chars
        .iter()
        .map(|ch| {
            let cid = plan.cid_for(font, *ch)?;
            let gid = metrics.glyph_id(*ch);
            let width = metrics.glyph_width_by_gid(gid);
            debug_assert!(cid > 0);
            Ok(pdf_number(width))
        })
        .collect::<Result<Vec<_>>>()?;
    items.push(PdfObject::Array(widths));
    Ok(items)
}

fn build_to_unicode_cmap(font: FontFace, plan: &FontBuildPlan, cmap_name: &str) -> Result<Vec<u8>> {
    let embedded = plan.embedded_plan(font)?;
    let mut out = String::new();
    out.push_str("/CIDInit /ProcSet findresource begin\n");
    out.push_str("12 dict begin\nbegincmap\n");
    out.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    out.push_str(&format!("/CMapName /{cmap_name} def\n/CMapType 2 def\n"));
    out.push_str("1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");

    for chunk in embedded.chars.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for ch in chunk {
            let cid = embedded.cids[ch];
            out.push_str(&format!("<{cid:04X}> <{}>\n", utf16be_hex_for_char(*ch)));
        }
        out.push_str("endbfchar\n");
    }

    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    Ok(out.into_bytes())
}

fn build_cid_to_gid_map(
    font: FontFace,
    plan: &FontBuildPlan,
    metrics: &TrueTypeMetrics,
) -> Result<Vec<u8>> {
    let embedded = plan.embedded_plan(font)?;
    let max_cid = embedded.cids.values().copied().max().unwrap_or(0);
    let mut bytes = vec![0u8; (usize::from(max_cid) + 1) * 2];
    for ch in &embedded.chars {
        let cid = embedded.cids[ch];
        let gid = metrics.glyph_id(*ch);
        let offset = usize::from(cid) * 2;
        bytes[offset..offset + 2].copy_from_slice(&gid.to_be_bytes());
    }
    Ok(bytes)
}

fn build_content_stream(
    page: &PdfPageBuilder,
    plan: &FontBuildPlan,
    image_plan: &ImageBuildPlan,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for command in &page.commands {
        match command {
            PageCommand::Text { text, x, y, style } => {
                write_text_command(&mut out, text, *x, *y, style, plan)?;
            }
            PageCommand::Rect {
                x,
                y,
                width,
                height,
                style,
            } => {
                write_graphics_state(&mut out, style);
                out.extend_from_slice(
                    format!(
                        "{} {} {} {} re\n{}\nQ\n",
                        fmt_num(*x),
                        fmt_num(*y),
                        fmt_num(*width),
                        fmt_num(*height),
                        paint_operator(style)
                    )
                    .as_bytes(),
                );
            }
            PageCommand::Path { path, style } => {
                write_graphics_state(&mut out, style);
                write_path(&mut out, path);
                out.extend_from_slice(format!("{}\nQ\n", paint_operator(style)).as_bytes());
            }
            PageCommand::Image {
                image,
                x,
                y,
                width,
                height,
            } => {
                write_image_command(&mut out, *image, *x, *y, *width, *height, image_plan)?;
            }
        }
    }
    Ok(out)
}

fn write_text_command(
    out: &mut Vec<u8>,
    text: &str,
    x: f64,
    y: f64,
    style: &TextStyle,
    plan: &FontBuildPlan,
) -> Result<()> {
    let resource = plan.resource_name(style.font)?;
    out.extend_from_slice(b"q\n");
    write_fill_color(out, &style.fill);
    let encoded = encode_text_for_font(text, style.font, plan)?;
    out.extend_from_slice(
        format!(
            "BT /{} {} Tf {} {} Td <{}> Tj ET\nQ\n",
            resource,
            fmt_num(style.size),
            fmt_num(x),
            fmt_num(y),
            hex_string(&encoded)
        )
        .as_bytes(),
    );
    Ok(())
}

fn write_image_command(
    out: &mut Vec<u8>,
    image: ImageHandle,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    image_plan: &ImageBuildPlan,
) -> Result<()> {
    let resource = image_plan.resource_name(image)?;
    out.extend_from_slice(
        format!(
            "q\n{} 0 0 {} {} {} cm\n/{} Do\nQ\n",
            fmt_num(width),
            fmt_num(height),
            fmt_num(x),
            fmt_num(y),
            resource
        )
        .as_bytes(),
    );
    Ok(())
}

fn write_graphics_state(out: &mut Vec<u8>, style: &GraphicsStyle) {
    out.extend_from_slice(b"q\n");
    out.extend_from_slice(format!("{} w\n", fmt_num(style.line_width.max(0.0))).as_bytes());
    out.extend_from_slice(format!("{} J\n", style.line_cap.clone() as i32).as_bytes());
    out.extend_from_slice(format!("{} j\n", style.line_join.clone() as i32).as_bytes());
    if style.dash.pattern.is_empty() {
        out.extend_from_slice(b"[] 0 d\n");
    } else {
        out.push(b'[');
        for (idx, value) in style.dash.pattern.iter().enumerate() {
            if idx > 0 {
                out.push(b' ');
            }
            out.extend_from_slice(fmt_num(*value).as_bytes());
        }
        out.extend_from_slice(format!("] {} d\n", fmt_num(style.dash.phase)).as_bytes());
    }
    if let Some(color) = &style.stroke {
        write_stroke_color(out, color);
    }
    if let Some(color) = &style.fill {
        write_fill_color(out, color);
    }
}

fn write_path(out: &mut Vec<u8>, path: &PathBuilder) {
    for segment in &path.segments {
        match *segment {
            PathSegment::MoveTo(x, y) => {
                out.extend_from_slice(format!("{} {} m\n", fmt_num(x), fmt_num(y)).as_bytes());
            }
            PathSegment::LineTo(x, y) => {
                out.extend_from_slice(format!("{} {} l\n", fmt_num(x), fmt_num(y)).as_bytes());
            }
            PathSegment::CurveTo(x1, y1, x2, y2, x3, y3) => {
                out.extend_from_slice(
                    format!(
                        "{} {} {} {} {} {} c\n",
                        fmt_num(x1),
                        fmt_num(y1),
                        fmt_num(x2),
                        fmt_num(y2),
                        fmt_num(x3),
                        fmt_num(y3)
                    )
                    .as_bytes(),
                );
            }
            PathSegment::Close => out.extend_from_slice(b"h\n"),
        }
    }
}

fn write_stroke_color(out: &mut Vec<u8>, color: &Color) {
    write_color(out, color, false);
}

fn write_fill_color(out: &mut Vec<u8>, color: &Color) {
    write_color(out, color, true);
}

fn write_color(out: &mut Vec<u8>, color: &Color, fill: bool) {
    let op = match (&color.space, fill) {
        (ColorSpace::DeviceGray, false) => "G",
        (ColorSpace::DeviceGray, true) => "g",
        (ColorSpace::DeviceRGB, false) => "RG",
        (ColorSpace::DeviceRGB, true) => "rg",
        (ColorSpace::DeviceCMYK, false) => "K",
        (ColorSpace::DeviceCMYK, true) => "k",
        (ColorSpace::Named(_), false) => "RG",
        (ColorSpace::Named(_), true) => "rg",
    };
    let components = match color.space {
        ColorSpace::Named(_) => vec![0.0, 0.0, 0.0],
        _ => color.components.clone(),
    };
    for (idx, component) in components.iter().enumerate() {
        if idx > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(fmt_num(component.clamp(0.0, 1.0)).as_bytes());
    }
    out.extend_from_slice(format!(" {op}\n").as_bytes());
}

fn paint_operator(style: &GraphicsStyle) -> &'static str {
    match (style.fill.is_some(), style.stroke.is_some()) {
        (true, true) => "B",
        (true, false) => "f",
        (false, true) => "S",
        (false, false) => "n",
    }
}

fn validate_text_for_font(text: &str, font: &FontFace) -> Result<()> {
    if let FontFace::Standard(standard) = font {
        for ch in text.chars() {
            encode_standard_char(*standard, ch).ok_or_else(|| {
                OxideError::UnsupportedFeature(format!(
                    "authoring: character {ch:?} is not encodable in {}; use FontFace::BuiltinUnicode",
                    standard.base_font_name()
                ))
            })?;
        }
    }
    Ok(())
}

fn encode_text_for_font(text: &str, font: FontFace, plan: &FontBuildPlan) -> Result<Vec<u8>> {
    match font {
        FontFace::Standard(standard) => text
            .chars()
            .map(|ch| {
                encode_standard_char(standard, ch).ok_or_else(|| {
                    OxideError::UnsupportedFeature(format!(
                        "authoring: character {ch:?} is not encodable in {}",
                        standard.base_font_name()
                    ))
                })
            })
            .collect(),
        FontFace::BuiltinUnicode | FontFace::Custom(_) => {
            let mut bytes = Vec::with_capacity(text.len() * 2);
            for ch in text.chars() {
                let cid = plan.cid_for(font, ch)?;
                bytes.extend_from_slice(&cid.to_be_bytes());
            }
            Ok(bytes)
        }
    }
}

fn text_width(text: &str, style: &TextStyle) -> Result<f64> {
    validate_text_for_font(text, &style.font)?;
    let units = match style.font {
        FontFace::Standard(font) => text
            .chars()
            .map(|ch| standard_char_width(font, ch))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum::<f64>(),
        FontFace::BuiltinUnicode => {
            let metrics = TrueTypeMetrics::parse(builtin_unicode_font_bytes()?)?;
            text.chars().map(|ch| metrics.glyph_width(ch)).sum::<f64>()
        }
        FontFace::Custom(_) => {
            let metrics = TrueTypeMetrics::parse(builtin_unicode_font_bytes()?)?;
            text.chars().map(|ch| metrics.glyph_width(ch)).sum::<f64>()
        }
    };
    Ok(units / 1000.0 * style.size)
}

fn wrap_text(text: &str, max_width: f64, style: &TextStyle) -> Result<Vec<String>> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if text_width(&candidate, style)? <= max_width || current.is_empty() {
                current = candidate;
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    Ok(lines)
}

fn standard_widths(font: StandardFont) -> Result<Vec<f64>> {
    (32u8..=255)
        .map(|byte| {
            let ch = decode_standard_byte(font, byte);
            fallback_glyph_width(font.fallback_font_name(), ch)
        })
        .collect()
}

fn standard_char_width(font: StandardFont, ch: char) -> Result<f64> {
    let _ = encode_standard_char(font, ch).ok_or_else(|| {
        OxideError::UnsupportedFeature(format!(
            "authoring: character {ch:?} is not encodable in {}",
            font.base_font_name()
        ))
    })?;
    fallback_glyph_width(font.fallback_font_name(), ch)
}

fn encode_standard_char(font: StandardFont, ch: char) -> Option<u8> {
    match font.built_in_encoding() {
        None => encode_win_ansi_char(ch),
        Some(encoding) => {
            (0u8..=255).find(|byte| decode_symbolic_byte(encoding, *byte) == Some(ch))
        }
    }
}

fn decode_standard_byte(font: StandardFont, byte: u8) -> char {
    match font.built_in_encoding() {
        None => decode_win_ansi(byte),
        Some(encoding) => decode_symbolic_byte(encoding, byte).unwrap_or(' '),
    }
}

fn decode_symbolic_byte(encoding: &str, byte: u8) -> Option<char> {
    let glyph = Encoding::lookup(encoding, byte);
    if glyph == ".notdef" {
        return None;
    }
    glyph_name_to_unicode(glyph).or_else(|| zapf_dingbats_name_to_unicode(glyph))
}

fn fallback_glyph_width(font_name: &str, ch: char) -> Result<f64> {
    let bytes = get_fallback_font(font_name).ok_or_else(|| {
        OxideError::MalformedPdf(format!("authoring: missing fallback font {font_name}"))
    })?;
    let metrics = TrueTypeMetrics::parse(bytes)?;
    Ok(metrics.glyph_width(ch))
}

fn builtin_unicode_font_bytes() -> Result<&'static [u8]> {
    get_fallback_font("Helvetica").ok_or_else(|| {
        OxideError::MalformedPdf("authoring: bundled Liberation Sans font missing".to_string())
    })
}

struct TrueTypeMetrics<'a> {
    face: ttf_parser::Face<'a>,
    scale: f64,
    ascender: f64,
    descender: f64,
    cap_height: f64,
    bbox: [f64; 4],
}

impl<'a> TrueTypeMetrics<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        let face = ttf_parser::Face::parse(bytes, 0).map_err(|err| {
            OxideError::MalformedPdf(format!("authoring: cannot parse bundled font: {err:?}"))
        })?;
        let units = f64::from(face.units_per_em());
        let scale = 1000.0 / units;
        let bbox = face.global_bounding_box();
        let ascender = f64::from(face.ascender()) * scale;
        let descender = f64::from(face.descender()) * scale;
        let cap_height = face
            .capital_height()
            .map(|value| f64::from(value) * scale)
            .unwrap_or(ascender);
        Ok(Self {
            face,
            scale,
            ascender,
            descender,
            cap_height,
            bbox: [
                f64::from(bbox.x_min) * scale,
                f64::from(bbox.y_min) * scale,
                f64::from(bbox.x_max) * scale,
                f64::from(bbox.y_max) * scale,
            ],
        })
    }

    fn glyph_id(&self, ch: char) -> u16 {
        self.face.glyph_index(ch).map(|gid| gid.0).unwrap_or(0)
    }

    fn glyph_width(&self, ch: char) -> f64 {
        self.glyph_width_by_gid(self.glyph_id(ch))
    }

    fn glyph_width_by_gid(&self, gid: u16) -> f64 {
        self.face
            .glyph_hor_advance(ttf_parser::GlyphId(gid))
            .map(|advance| f64::from(advance) * self.scale)
            .unwrap_or(500.0)
    }
}

fn encode_win_ansi_char(ch: char) -> Option<u8> {
    if ('\u{20}'..='\u{7e}').contains(&ch) {
        return Some(ch as u8);
    }
    WIN_ANSI_EXTRA
        .iter()
        .find_map(|(byte, mapped)| (*mapped == ch).then_some(*byte))
}

fn decode_win_ansi(byte: u8) -> char {
    WIN_ANSI_EXTRA
        .iter()
        .find_map(|(candidate, ch)| (*candidate == byte).then_some(*ch))
        .unwrap_or(byte as char)
}

const WIN_ANSI_EXTRA: &[(u8, char)] = &[
    (0x80, '\u{20AC}'),
    (0x82, '\u{201A}'),
    (0x83, '\u{0192}'),
    (0x84, '\u{201E}'),
    (0x85, '\u{2026}'),
    (0x86, '\u{2020}'),
    (0x87, '\u{2021}'),
    (0x88, '\u{02C6}'),
    (0x89, '\u{2030}'),
    (0x8A, '\u{0160}'),
    (0x8B, '\u{2039}'),
    (0x8C, '\u{0152}'),
    (0x8E, '\u{017D}'),
    (0x91, '\u{2018}'),
    (0x92, '\u{2019}'),
    (0x93, '\u{201C}'),
    (0x94, '\u{201D}'),
    (0x95, '\u{2022}'),
    (0x96, '\u{2013}'),
    (0x97, '\u{2014}'),
    (0x98, '\u{02DC}'),
    (0x99, '\u{2122}'),
    (0x9A, '\u{0161}'),
    (0x9B, '\u{203A}'),
    (0x9C, '\u{0153}'),
    (0x9E, '\u{017E}'),
    (0x9F, '\u{0178}'),
];

fn reference(number: u32) -> PdfObject {
    PdfObject::Reference {
        number,
        generation: 0,
    }
}

fn alloc(next: &mut u32) -> u32 {
    let number = *next;
    *next += 1;
    number
}

fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
    let mut map = BTreeMap::new();
    for (key, value) in entries {
        map.insert((*key).to_string(), value.clone());
    }
    PdfDictionary::new(map)
}

fn pdf_number(value: f64) -> PdfObject {
    if is_integerish(value) {
        PdfObject::Integer(value.round() as i64)
    } else {
        PdfObject::Real(round_pdf_num(value))
    }
}

fn fmt_num(value: f64) -> String {
    let value = round_pdf_num(value);
    if is_integerish(value) {
        return format!("{}", value.round() as i64);
    }
    let mut s = format!("{value:.4}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

fn round_pdf_num(value: f64) -> f64 {
    if value.abs() < 0.000_000_1 {
        0.0
    } else {
        (value * 10_000.0).round() / 10_000.0
    }
}

fn is_integerish(value: f64) -> bool {
    (value - value.round()).abs() < 0.000_000_1
}

fn hex_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + (value - 10)) as char,
    }
}

fn utf16be_hex_for_char(ch: char) -> String {
    let mut units = [0u16; 2];
    let encoded = ch.encode_utf16(&mut units);
    let mut out = String::new();
    for unit in encoded {
        out.push_str(&format!("{unit:04X}"));
    }
    out
}

fn pdf_text_string(value: &str) -> Vec<u8> {
    let mut bytes = vec![0xfe, 0xff];
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

fn sanitize_pdf_name(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        }
    }
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentEngine, ContentParser};

    #[test]
    fn content_stream_has_expected_text_and_graphics_operators() {
        let mut page = PdfPageBuilder::new(PageSize::LETTER);
        let text = TextStyle::standard(StandardFont::Helvetica, 12.0)
            .fill(Color::device_rgb(0.1, 0.2, 0.3));
        page.draw_text("Hello", 72.0, 720.0, &text).unwrap();
        page.draw_rect(
            72.0,
            680.0,
            144.0,
            24.0,
            &GraphicsStyle::fill_stroke(
                Color::device_rgb(0.9, 0.9, 0.9),
                Color::device_rgb(0.0, 0.0, 0.0),
                2.0,
            ),
        );
        page.draw_line(
            72.0,
            660.0,
            216.0,
            660.0,
            &GraphicsStyle::stroke(Color::black(), 1.0),
        );

        let plan = FontBuildPlan::from_pages(&[page.clone()]).unwrap();
        let image_plan = ImageBuildPlan {
            images: Vec::new(),
            resource_names: HashMap::new(),
        };
        let content = build_content_stream(&page, &plan, &image_plan).unwrap();
        let ops = ContentParser::parse(&content).unwrap();
        let names: Vec<_> = ops.iter().map(|op| op.operator.as_str()).collect();
        assert!(names.windows(2).any(|pair| pair == ["BT", "Tf"]));
        assert!(names.contains(&"Tj"));
        assert!(names.contains(&"re"));
        assert!(names.contains(&"m"));
        assert!(names.contains(&"l"));
        assert!(names.contains(&"S"));
        assert!(names.contains(&"B"));
    }

    #[test]
    fn authored_object_graph_reopens_with_pages_and_resources() {
        let mut doc = PdfBuilder::new();
        doc.set_title("Authored");
        doc.add_page(PageSize::LETTER)
            .draw_text(
                "Hello object graph",
                72.0,
                720.0,
                &TextStyle::standard(StandardFont::Helvetica, 12.0),
            )
            .unwrap();
        let bytes = doc.to_bytes().unwrap();
        let engine = ContentEngine::open_bytes(bytes).unwrap();
        assert_eq!(engine.page_count().unwrap(), 1);
        let page = engine.document().get_pages().unwrap().remove(0);
        assert!(page.resources.get_dict("Font").is_some());
        assert!(!page.contents.is_empty());
    }

    #[test]
    fn standard_and_unicode_text_extract() {
        let mut doc = PdfBuilder::new();
        let page = doc.add_page(PageSize::LETTER);
        page.draw_text(
            "Standard text",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 12.0),
        )
        .unwrap();
        page.draw_text(
            "Unicode cafe \u{03c0}",
            72.0,
            690.0,
            &TextStyle::unicode(12.0),
        )
        .unwrap();
        page.draw_text(
            "\u{03b1}\u{03b2}",
            72.0,
            660.0,
            &TextStyle::standard(StandardFont::Symbol, 12.0),
        )
        .unwrap();

        let engine = ContentEngine::open_bytes(doc.to_bytes().unwrap()).unwrap();
        let text = engine.get_page_text(1).unwrap();
        assert!(text.contains("Standard text"), "{text}");
        assert!(text.contains("Unicode cafe \u{03c0}"), "{text}");
        assert!(text.contains("\u{03b1}\u{03b2}"), "{text}");
    }

    #[test]
    fn wrapping_and_alignment_use_measured_widths() {
        let mut page = PdfPageBuilder::new(PageSize::LETTER);
        let style = TextStyle::standard(StandardFont::Helvetica, 12.0);
        let lines = page
            .draw_paragraph(
                "alpha beta gamma delta",
                100.0,
                700.0,
                80.0,
                &style,
                &ParagraphStyle::new().align(TextAlign::Center),
            )
            .unwrap();
        assert!(lines.len() >= 2);

        let plan = FontBuildPlan::from_pages(&[page.clone()]).unwrap();
        let image_plan = ImageBuildPlan {
            images: Vec::new(),
            resource_names: HashMap::new(),
        };
        let content =
            String::from_utf8(build_content_stream(&page, &plan, &image_plan).unwrap()).unwrap();
        assert!(
            content.contains(" Td <"),
            "paragraph should emit positioned text: {content}"
        );
        assert!(
            page.text_width(&lines[0], &style).unwrap() <= 80.0,
            "wrapped line fits"
        );
    }

    #[test]
    fn authored_output_is_deterministic() {
        fn build() -> Vec<u8> {
            let mut doc = PdfBuilder::new();
            let page = doc.add_page(PageSize::A4);
            page.draw_text(
                "Deterministic",
                72.0,
                720.0,
                &TextStyle::standard(StandardFont::TimesRoman, 14.0),
            )
            .unwrap();
            page.draw_circle(
                120.0,
                620.0,
                20.0,
                &GraphicsStyle::fill(Color::device_rgb(0.2, 0.4, 0.7)),
            );
            doc.to_bytes().unwrap()
        }
        assert_eq!(build(), build());
    }
}
