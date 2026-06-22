//! High-level PDF authoring API.
//!
//! Coordinates use native PDF user space: the origin is at the bottom-left of
//! the page, x grows to the right, and y grows upward. Use
//! [`PdfPageBuilder::pdf_y_from_top`] when a top-left UI coordinate is more
//! convenient.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::content::{Color, ColorSpace, LineCap, LineDash, LineJoin};
use crate::error::{OxideError, Result};
use crate::fonts::encoding::{zapf_dingbats_name_to_unicode, Encoding};
use crate::fonts::glyph_list::glyph_name_to_unicode;
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

    /// Serialize the authored document to PDF bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.pages.is_empty() {
            return Err(OxideError::MalformedPdf(
                "authoring: cannot save a PDF with no pages".to_string(),
            ));
        }

        let font_plan = FontBuildPlan::from_pages(&self.pages)?;
        let objects = AuthoredObjects::build(self, &font_plan)?;
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

    fn fonts_used(&self) -> Vec<FontFace> {
        let mut out = Vec::new();
        for command in &self.commands {
            if let PageCommand::Text { style, .. } = command {
                push_unique_font(&mut out, style.font);
            }
        }
        out
    }

    fn unicode_chars_used(&self) -> Vec<char> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for command in &self.commands {
            if let PageCommand::Text { text, style, .. } = command {
                if style.font == FontFace::BuiltinUnicode {
                    for ch in text.chars() {
                        if seen.insert(ch) {
                            out.push(ch);
                        }
                    }
                }
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
}

impl Default for FontFace {
    fn default() -> Self {
        Self::Standard(StandardFont::Helvetica)
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
}

struct AuthoredObjects {
    objects: Vec<OutputObject>,
    catalog_number: u32,
    info_number: Option<u32>,
}

impl AuthoredObjects {
    fn build(builder: &PdfBuilder, font_plan: &FontBuildPlan) -> Result<Self> {
        let catalog_number = 1u32;
        let pages_number = 2u32;
        let page_count = builder.pages.len();
        let page_start = 3u32;
        let content_start = page_start + page_count as u32;
        let mut next = content_start + page_count as u32;

        let mut font_objects = Vec::new();
        let mut font_refs = HashMap::new();
        for font in &font_plan.fonts {
            let built = build_font_objects(*font, &mut next, font_plan)?;
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

        for (idx, page) in builder.pages.iter().enumerate() {
            let page_number = page_start + idx as u32;
            let content_number = content_start + idx as u32;
            let content = build_content_stream(page, font_plan)?;
            objects.push(OutputObject {
                number: page_number,
                object: PdfObject::Dictionary(page_dict(
                    pages_number,
                    content_number,
                    page.size,
                    page.fonts_used(),
                    font_plan,
                    &font_refs,
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

#[derive(Debug)]
struct FontBuildPlan {
    fonts: Vec<FontFace>,
    resource_names: HashMap<FontFace, String>,
    unicode_cids: HashMap<char, u16>,
    unicode_chars: Vec<char>,
}

impl FontBuildPlan {
    fn from_pages(pages: &[PdfPageBuilder]) -> Result<Self> {
        let mut fonts = Vec::new();
        let mut unicode_chars = Vec::new();
        let mut unicode_seen = BTreeSet::new();

        for page in pages {
            for font in page.fonts_used() {
                push_unique_font(&mut fonts, font);
            }
            for ch in page.unicode_chars_used() {
                if unicode_seen.insert(ch) {
                    unicode_chars.push(ch);
                }
            }
        }

        let mut resource_names = HashMap::new();
        for (idx, font) in fonts.iter().enumerate() {
            resource_names.insert(*font, format!("F{}", idx + 1));
        }

        let mut unicode_cids = HashMap::new();
        for (idx, ch) in unicode_chars.iter().enumerate() {
            let cid = u16::try_from(idx + 1).map_err(|_| {
                OxideError::ResourceLimit(
                    "authoring: too many unique Unicode scalar values for one built-in font"
                        .to_string(),
                )
            })?;
            unicode_cids.insert(*ch, cid);
        }

        Ok(Self {
            fonts,
            resource_names,
            unicode_cids,
            unicode_chars,
        })
    }

    fn resource_name(&self, font: FontFace) -> Result<&str> {
        self.resource_names
            .get(&font)
            .map(String::as_str)
            .ok_or_else(|| {
                OxideError::MalformedPdf("authoring: font was not registered".to_string())
            })
    }

    fn cid_for(&self, ch: char) -> Result<u16> {
        self.unicode_cids.get(&ch).copied().ok_or_else(|| {
            OxideError::MalformedPdf(format!("authoring: Unicode character {ch:?} has no CID"))
        })
    }
}

fn push_unique_font(fonts: &mut Vec<FontFace>, font: FontFace) {
    if !fonts.contains(&font) {
        fonts.push(font);
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
    size: PageSize,
    page_fonts: Vec<FontFace>,
    plan: &FontBuildPlan,
    font_refs: &HashMap<FontFace, u32>,
) -> Result<PdfDictionary> {
    let mut fonts = PdfDictionary::empty();
    for font in page_fonts {
        let resource = plan.resource_name(font)?;
        let Some(number) = font_refs.get(&font).copied() else {
            return Err(OxideError::MalformedPdf(
                "authoring: font object missing".to_string(),
            ));
        };
        fonts.insert(resource, reference(number));
    }

    let mut resources = PdfDictionary::empty();
    resources.insert("Font", PdfObject::Dictionary(fonts));
    resources.insert(
        "ProcSet",
        PdfObject::Array(vec![
            PdfObject::Name("PDF".to_string()),
            PdfObject::Name("Text".to_string()),
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
                pdf_number(size.width),
                pdf_number(size.height),
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

fn build_font_objects(
    font: FontFace,
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
        FontFace::BuiltinUnicode => build_builtin_unicode_font(next, plan),
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

fn build_builtin_unicode_font(next: &mut u32, plan: &FontBuildPlan) -> Result<BuiltFontObjects> {
    let type0_number = alloc(next);
    let descendant_number = alloc(next);
    let descriptor_number = alloc(next);
    let font_file_number = alloc(next);
    let to_unicode_number = alloc(next);
    let cid_to_gid_number = alloc(next);

    let font_bytes = builtin_unicode_font_bytes()?;
    let metrics = TrueTypeMetrics::parse(font_bytes)?;

    let mut objects = Vec::new();
    objects.push(OutputObject {
        number: type0_number,
        object: PdfObject::Dictionary(dict(&[
            ("Type", PdfObject::Name("Font".to_string())),
            ("Subtype", PdfObject::Name("Type0".to_string())),
            (
                "BaseFont",
                PdfObject::Name(BUILTIN_UNICODE_RESOURCE_NAME.to_string()),
            ),
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
            descriptor_number,
            cid_to_gid_number,
            plan,
            &metrics,
        )?),
    });
    objects.push(OutputObject {
        number: descriptor_number,
        object: PdfObject::Dictionary(font_descriptor_dict(font_file_number, &metrics)),
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
            raw: build_to_unicode_cmap(plan),
        },
    });
    objects.push(OutputObject {
        number: cid_to_gid_number,
        object: PdfObject::Stream {
            dict: PdfDictionary::empty(),
            raw: build_cid_to_gid_map(plan, &metrics),
        },
    });

    Ok(BuiltFontObjects {
        top_object: type0_number,
        objects,
    })
}

fn cid_font_dict(
    descriptor_number: u32,
    cid_to_gid_number: u32,
    plan: &FontBuildPlan,
    metrics: &TrueTypeMetrics,
) -> Result<PdfDictionary> {
    let widths = unicode_width_array(plan, metrics)?;
    Ok(dict(&[
        ("Type", PdfObject::Name("Font".to_string())),
        ("Subtype", PdfObject::Name("CIDFontType2".to_string())),
        (
            "BaseFont",
            PdfObject::Name(BUILTIN_UNICODE_RESOURCE_NAME.to_string()),
        ),
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

fn font_descriptor_dict(font_file_number: u32, metrics: &TrueTypeMetrics) -> PdfDictionary {
    dict(&[
        ("Type", PdfObject::Name("FontDescriptor".to_string())),
        (
            "FontName",
            PdfObject::Name(BUILTIN_UNICODE_RESOURCE_NAME.to_string()),
        ),
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

fn unicode_width_array(plan: &FontBuildPlan, metrics: &TrueTypeMetrics) -> Result<Vec<PdfObject>> {
    if plan.unicode_chars.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::with_capacity(plan.unicode_chars.len() + 1);
    items.push(PdfObject::Integer(1));
    let widths = plan
        .unicode_chars
        .iter()
        .map(|ch| {
            let cid = plan.cid_for(*ch)?;
            let gid = metrics.glyph_id(*ch);
            let width = metrics.glyph_width_by_gid(gid);
            debug_assert!(cid > 0);
            Ok(pdf_number(width))
        })
        .collect::<Result<Vec<_>>>()?;
    items.push(PdfObject::Array(widths));
    Ok(items)
}

fn build_to_unicode_cmap(plan: &FontBuildPlan) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("/CIDInit /ProcSet findresource begin\n");
    out.push_str("12 dict begin\nbegincmap\n");
    out.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    out.push_str("/CMapName /OxideUnicode def\n/CMapType 2 def\n");
    out.push_str("1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");

    for chunk in plan.unicode_chars.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for ch in chunk {
            let cid = plan.unicode_cids[ch];
            out.push_str(&format!("<{cid:04X}> <{}>\n", utf16be_hex_for_char(*ch)));
        }
        out.push_str("endbfchar\n");
    }

    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

fn build_cid_to_gid_map(plan: &FontBuildPlan, metrics: &TrueTypeMetrics) -> Vec<u8> {
    let max_cid = plan.unicode_cids.values().copied().max().unwrap_or(0);
    let mut bytes = vec![0u8; (usize::from(max_cid) + 1) * 2];
    for ch in &plan.unicode_chars {
        let cid = plan.unicode_cids[ch];
        let gid = metrics.glyph_id(*ch);
        let offset = usize::from(cid) * 2;
        bytes[offset..offset + 2].copy_from_slice(&gid.to_be_bytes());
    }
    bytes
}

fn build_content_stream(page: &PdfPageBuilder, plan: &FontBuildPlan) -> Result<Vec<u8>> {
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
        FontFace::BuiltinUnicode => {
            let mut bytes = Vec::with_capacity(text.len() * 2);
            for ch in text.chars() {
                let cid = plan.cid_for(ch)?;
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
        let content = build_content_stream(&page, &plan).unwrap();
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
        let content = String::from_utf8(build_content_stream(&page, &plan).unwrap()).unwrap();
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
