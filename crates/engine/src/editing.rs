//! Additive page-content editing for existing PDFs.
//!
//! Edits are emitted as new content streams that are prepended as underlays or
//! appended as overlays. Existing page content streams are left untouched.

use std::collections::{BTreeMap, BTreeSet};

use crate::content::{Color, ColorSpace};
use crate::document::{PdfDocument, PdfPage};
use crate::error::{OxideError, Result};
use crate::filters::flate_encode;
use crate::images::decoder::{ImageDecoder, RawImage};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::writer::{
    write_incremental_update, IncrementalObject, OutputObject, PdfWriter, WriterMode,
};
use crate::TextAlign;

/// How an editing operation is serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditMode {
    /// Rewrite the whole file with the modern writer.
    #[default]
    FullRewrite,
    /// Append changed/new objects after the original bytes, preserving the
    /// original byte prefix exactly.
    Incremental,
}

/// Whether new content is placed before or after existing page content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayLayer {
    /// Draw before existing page content.
    Underlay,
    /// Draw after existing page content.
    #[default]
    Overlay,
}

/// Style for text added to an existing page.
#[derive(Debug, Clone, PartialEq)]
pub struct EditTextStyle {
    pub font_size: f64,
    pub fill: Color,
    pub opacity: f64,
    pub rotation_degrees: f64,
}

impl Default for EditTextStyle {
    fn default() -> Self {
        Self {
            font_size: 12.0,
            fill: Color::black(),
            opacity: 1.0,
            rotation_degrees: 0.0,
        }
    }
}

impl EditTextStyle {
    pub fn new(font_size: f64) -> Self {
        Self {
            font_size,
            ..Default::default()
        }
    }

    pub fn fill(mut self, color: Color) -> Self {
        self.fill = color;
        self
    }

    pub fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn rotation_degrees(mut self, rotation: f64) -> Self {
        self.rotation_degrees = rotation;
        self
    }
}

/// Text watermark options.
#[derive(Debug, Clone, PartialEq)]
pub struct WatermarkOptions {
    pub pages: Option<Vec<usize>>,
    pub style: EditTextStyle,
    pub layer: OverlayLayer,
}

impl Default for WatermarkOptions {
    fn default() -> Self {
        Self {
            pages: None,
            style: EditTextStyle::new(64.0)
                .fill(Color::device_gray(0.55))
                .opacity(0.28)
                .rotation_degrees(45.0),
            layer: OverlayLayer::Overlay,
        }
    }
}

/// Header/footer options. Text may include `{page}` and `{total}` tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderFooterOptions {
    pub pages: Option<Vec<usize>>,
    pub style: EditTextStyle,
    pub align: TextAlign,
    pub y: Option<f64>,
    pub layer: OverlayLayer,
}

impl Default for HeaderFooterOptions {
    fn default() -> Self {
        Self {
            pages: None,
            style: EditTextStyle::new(10.0).fill(Color::device_gray(0.2)),
            align: TextAlign::Center,
            y: None,
            layer: OverlayLayer::Overlay,
        }
    }
}

/// Rectangle drawing style for existing-page edits.
#[derive(Debug, Clone, PartialEq)]
pub struct EditRectStyle {
    pub stroke: Option<Color>,
    pub fill: Option<Color>,
    pub line_width: f64,
    pub opacity: f64,
}

impl Default for EditRectStyle {
    fn default() -> Self {
        Self {
            stroke: Some(Color::black()),
            fill: None,
            line_width: 1.0,
            opacity: 1.0,
        }
    }
}

/// Image placement options.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageStampOptions {
    pub opacity: f64,
    pub layer: OverlayLayer,
}

impl Default for ImageStampOptions {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            layer: OverlayLayer::Overlay,
        }
    }
}

/// Additive editor for an existing PDF.
pub struct PdfEditor {
    document: PdfDocument,
    edits: BTreeMap<usize, Vec<PageEdit>>,
}

impl PdfEditor {
    pub fn open_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self {
            document: PdfDocument::open_bytes(bytes)?,
            edits: BTreeMap::new(),
        })
    }

    pub fn document(&self) -> &PdfDocument {
        &self.document
    }

    pub fn add_watermark_text(
        &mut self,
        text: impl Into<String>,
        options: WatermarkOptions,
    ) -> Result<&mut Self> {
        let text = text.into();
        let pages = self.target_pages(options.pages.as_deref())?;
        let all_pages = self.document.get_pages()?;
        for page_number in pages {
            let page = &all_pages[page_number - 1];
            let (cx, cy) = page_center(page);
            let width = page.media_box[2] - page.media_box[0];
            let text_width = approximate_text_width(&text, options.style.font_size);
            let x = cx - text_width.min(width) / 2.0;
            self.push_edit(
                page_number,
                PageEdit {
                    layer: options.layer,
                    command: EditCommand::Text {
                        text: text.clone(),
                        x,
                        y: cy,
                        style: options.style.clone(),
                    },
                },
            );
        }
        Ok(self)
    }

    pub fn add_header(
        &mut self,
        template: impl Into<String>,
        options: HeaderFooterOptions,
    ) -> Result<&mut Self> {
        self.add_header_footer(template.into(), options, true)
    }

    pub fn add_footer(
        &mut self,
        template: impl Into<String>,
        options: HeaderFooterOptions,
    ) -> Result<&mut Self> {
        self.add_header_footer(template.into(), options, false)
    }

    pub fn draw_text(
        &mut self,
        page_number: usize,
        text: impl Into<String>,
        x: f64,
        y: f64,
        style: EditTextStyle,
        layer: OverlayLayer,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.push_edit(
            page_number,
            PageEdit {
                layer,
                command: EditCommand::Text {
                    text: text.into(),
                    x,
                    y,
                    style,
                },
            },
        );
        Ok(self)
    }

    pub fn draw_rect(
        &mut self,
        page_number: usize,
        rect: ImageRect,
        style: EditRectStyle,
        layer: OverlayLayer,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.push_edit(
            page_number,
            PageEdit {
                layer,
                command: EditCommand::Rect { rect, style },
            },
        );
        Ok(self)
    }

    pub fn stamp_jpeg_image(
        &mut self,
        page_number: usize,
        bytes: impl Into<Vec<u8>>,
        rect: ImageRect,
        options: ImageStampOptions,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        let bytes = bytes.into();
        let (_, width, height, channels) = ImageDecoder::decode_jpeg_with_info(&bytes)?;
        let image = EditImage {
            width,
            height,
            color_space: image_color_space(channels)?,
            bits_per_component: 8,
            data: bytes,
            filter: ImageFilter::DctDecode,
            smask: None,
        };
        self.push_edit(
            page_number,
            PageEdit {
                layer: options.layer,
                command: EditCommand::Image {
                    image,
                    rect,
                    opacity: options.opacity,
                },
            },
        );
        Ok(self)
    }

    pub fn stamp_rgba_image(
        &mut self,
        page_number: usize,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        rect: ImageRect,
        options: ImageStampOptions,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        let image = edit_image_from_raw(RawImage {
            width,
            height,
            channels: 4,
            bits_per_sample: 8,
            pixels,
        })?;
        self.push_edit(
            page_number,
            PageEdit {
                layer: options.layer,
                command: EditCommand::Image {
                    image,
                    rect,
                    opacity: options.opacity,
                },
            },
        );
        Ok(self)
    }

    pub fn save_to_bytes(&self, mode: EditMode) -> Result<Vec<u8>> {
        let changes = self.build_changes()?;
        match mode {
            EditMode::Incremental => write_incremental_update(self.document.reader(), changes),
            EditMode::FullRewrite => write_full_rewrite(self.document.reader(), changes),
        }
    }

    fn add_header_footer(
        &mut self,
        template: String,
        options: HeaderFooterOptions,
        header: bool,
    ) -> Result<&mut Self> {
        let pages = self.target_pages(options.pages.as_deref())?;
        let all_pages = self.document.get_pages()?;
        let total = all_pages.len();
        for page_number in pages {
            let page = &all_pages[page_number - 1];
            let text = template
                .replace("{page}", &page_number.to_string())
                .replace("{total}", &total.to_string());
            let y = options.y.unwrap_or_else(|| {
                if header {
                    page.media_box[3] - 36.0
                } else {
                    page.media_box[1] + 30.0
                }
            });
            let width = page.media_box[2] - page.media_box[0];
            let text_width = approximate_text_width(&text, options.style.font_size);
            let x = match options.align {
                TextAlign::Left => page.media_box[0] + 36.0,
                TextAlign::Center => page.media_box[0] + (width - text_width) / 2.0,
                TextAlign::Right => page.media_box[2] - 36.0 - text_width,
            };
            self.push_edit(
                page_number,
                PageEdit {
                    layer: options.layer,
                    command: EditCommand::Text {
                        text,
                        x,
                        y,
                        style: options.style.clone(),
                    },
                },
            );
        }
        Ok(self)
    }

    fn build_changes(&self) -> Result<Vec<IncrementalObject>> {
        let pages = self.document.get_pages()?;
        let by_page: BTreeMap<usize, &PdfPage> =
            pages.iter().map(|page| (page.page_number, page)).collect();
        let mut next = next_free_object_number(self.document.reader());
        let mut changes = Vec::new();

        for (page_number, edits) in &self.edits {
            let page = by_page.get(page_number).ok_or_else(|| {
                OxideError::MalformedPdf(format!("page {page_number} is out of range"))
            })?;
            let page_object = self
                .document
                .reader()
                .get_object(page.object_number, page.generation_number)?;
            let mut page_dict = page_object.as_dict().cloned().ok_or_else(|| {
                OxideError::MalformedPdf(format!(
                    "page object {} {} is not a dictionary",
                    page.object_number, page.generation_number
                ))
            })?;
            let mut resources = page.resources.clone();
            let mut underlay = Vec::new();
            let mut overlay = Vec::new();

            for edit in edits {
                let out = match edit.layer {
                    OverlayLayer::Underlay => &mut underlay,
                    OverlayLayer::Overlay => &mut overlay,
                };
                write_edit_command(out, &edit.command, &mut resources, &mut next, &mut changes)?;
            }

            let mut content_refs = Vec::new();
            if !underlay.is_empty() {
                let number = alloc_object(&mut next);
                changes.push(stream_object(number, underlay));
                content_refs.push(reference(number, 0));
            }
            for (number, generation) in &page.contents {
                content_refs.push(reference(*number, *generation));
            }
            if !overlay.is_empty() {
                let number = alloc_object(&mut next);
                changes.push(stream_object(number, overlay));
                content_refs.push(reference(number, 0));
            }

            page_dict.insert("Resources", PdfObject::Dictionary(resources));
            page_dict.insert("Contents", PdfObject::Array(content_refs));
            changes.push(IncrementalObject {
                number: page.object_number,
                generation: page.generation_number,
                object: PdfObject::Dictionary(page_dict),
            });
        }

        changes.sort_by_key(|obj| (obj.number, obj.generation));
        Ok(changes)
    }

    fn push_edit(&mut self, page_number: usize, edit: PageEdit) {
        self.edits.entry(page_number).or_default().push(edit);
    }

    fn validate_page(&self, page_number: usize) -> Result<()> {
        if page_number == 0 || page_number > self.document.get_pages()?.len() {
            return Err(OxideError::MalformedPdf(format!(
                "page {page_number} is out of range"
            )));
        }
        Ok(())
    }

    fn target_pages(&self, pages: Option<&[usize]>) -> Result<Vec<usize>> {
        let total = self.document.get_pages()?.len();
        match pages {
            Some(pages) => {
                let mut out = Vec::new();
                let mut seen = BTreeSet::new();
                for &page in pages {
                    if page == 0 || page > total {
                        return Err(OxideError::MalformedPdf(format!(
                            "page {page} is out of range"
                        )));
                    }
                    if seen.insert(page) {
                        out.push(page);
                    }
                }
                Ok(out)
            }
            None => Ok((1..=total).collect()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ImageRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone)]
struct PageEdit {
    layer: OverlayLayer,
    command: EditCommand,
}

#[derive(Debug, Clone)]
enum EditCommand {
    Text {
        text: String,
        x: f64,
        y: f64,
        style: EditTextStyle,
    },
    Rect {
        rect: ImageRect,
        style: EditRectStyle,
    },
    Image {
        image: EditImage,
        rect: ImageRect,
        opacity: f64,
    },
}

#[derive(Debug, Clone)]
struct EditImage {
    width: u32,
    height: u32,
    color_space: &'static str,
    bits_per_component: u8,
    data: Vec<u8>,
    filter: ImageFilter,
    smask: Option<EditSoftMask>,
}

#[derive(Debug, Clone)]
struct EditSoftMask {
    width: u32,
    height: u32,
    data: Vec<u8>,
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

fn write_edit_command(
    out: &mut Vec<u8>,
    command: &EditCommand,
    resources: &mut PdfDictionary,
    next: &mut u32,
    changes: &mut Vec<IncrementalObject>,
) -> Result<()> {
    match command {
        EditCommand::Text { text, x, y, style } => {
            let font = ensure_standard_font(resources);
            let gs = ensure_extgstate(resources, style.opacity);
            write_text(out, &font, Some(&gs), text, *x, *y, style);
        }
        EditCommand::Rect { rect, style } => {
            let gs = ensure_extgstate(resources, style.opacity);
            write_rect(out, Some(&gs), *rect, style);
        }
        EditCommand::Image {
            image,
            rect,
            opacity,
        } => {
            let smask_number = if image.smask.is_some() {
                Some(alloc_object(next))
            } else {
                None
            };
            let image_number = alloc_object(next);
            if let (Some(number), Some(mask)) = (smask_number, image.smask.as_ref()) {
                changes.push(IncrementalObject {
                    number,
                    generation: 0,
                    object: PdfObject::Stream {
                        dict: smask_dict(mask),
                        raw: mask.data.clone(),
                    },
                });
            }
            changes.push(IncrementalObject {
                number: image_number,
                generation: 0,
                object: PdfObject::Stream {
                    dict: image_dict(image, smask_number),
                    raw: image.data.clone(),
                },
            });
            let image_name = add_xobject(resources, image_number);
            let gs = ensure_extgstate(resources, *opacity);
            write_image(out, &image_name, Some(&gs), *rect);
        }
    }
    Ok(())
}

fn write_text(
    out: &mut Vec<u8>,
    font: &str,
    gs: Option<&str>,
    text: &str,
    x: f64,
    y: f64,
    style: &EditTextStyle,
) {
    let rotation = style.rotation_degrees.to_radians();
    let cos = rotation.cos();
    let sin = rotation.sin();
    out.extend_from_slice(b"q\n");
    if let Some(gs) = gs {
        out.extend_from_slice(format!("/{gs} gs\n").as_bytes());
    }
    write_fill_color(out, &style.fill);
    out.extend_from_slice(
        format!(
            "BT /{} {} Tf {} {} {} {} {} {} Tm <{}> Tj ET\nQ\n",
            font,
            fmt_num(style.font_size),
            fmt_num(cos),
            fmt_num(sin),
            fmt_num(-sin),
            fmt_num(cos),
            fmt_num(x),
            fmt_num(y),
            hex_string(&encode_win_ansi_lossy(text))
        )
        .as_bytes(),
    );
}

fn write_rect(out: &mut Vec<u8>, gs: Option<&str>, rect: ImageRect, style: &EditRectStyle) {
    out.extend_from_slice(b"q\n");
    if let Some(gs) = gs {
        out.extend_from_slice(format!("/{gs} gs\n").as_bytes());
    }
    out.extend_from_slice(format!("{} w\n", fmt_num(style.line_width.max(0.0))).as_bytes());
    if let Some(color) = &style.stroke {
        write_stroke_color(out, color);
    }
    if let Some(color) = &style.fill {
        write_fill_color(out, color);
    }
    out.extend_from_slice(
        format!(
            "{} {} {} {} re\n{}\nQ\n",
            fmt_num(rect.x),
            fmt_num(rect.y),
            fmt_num(rect.width),
            fmt_num(rect.height),
            match (style.fill.is_some(), style.stroke.is_some()) {
                (true, true) => "B",
                (true, false) => "f",
                (false, true) => "S",
                (false, false) => "n",
            }
        )
        .as_bytes(),
    );
}

fn write_image(out: &mut Vec<u8>, image_name: &str, gs: Option<&str>, rect: ImageRect) {
    out.extend_from_slice(b"q\n");
    if let Some(gs) = gs {
        out.extend_from_slice(format!("/{gs} gs\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "{} 0 0 {} {} {} cm\n/{} Do\nQ\n",
            fmt_num(rect.width),
            fmt_num(rect.height),
            fmt_num(rect.x),
            fmt_num(rect.y),
            image_name
        )
        .as_bytes(),
    );
}

fn ensure_standard_font(resources: &mut PdfDictionary) -> String {
    let mut fonts = dict_resource(resources, "Font");
    let name = next_resource_name(&fonts, "OxEdF");
    fonts.insert(
        &name,
        PdfObject::Dictionary(dict(&[
            ("Type", PdfObject::Name("Font".to_string())),
            ("Subtype", PdfObject::Name("Type1".to_string())),
            ("BaseFont", PdfObject::Name("Helvetica".to_string())),
            ("Encoding", PdfObject::Name("WinAnsiEncoding".to_string())),
        ])),
    );
    resources.insert("Font", PdfObject::Dictionary(fonts));
    name
}

fn ensure_extgstate(resources: &mut PdfDictionary, opacity: f64) -> String {
    let mut states = dict_resource(resources, "ExtGState");
    let name = next_resource_name(&states, "OxEdGs");
    let alpha = opacity.clamp(0.0, 1.0);
    states.insert(
        &name,
        PdfObject::Dictionary(dict(&[
            ("Type", PdfObject::Name("ExtGState".to_string())),
            ("ca", pdf_number(alpha)),
            ("CA", pdf_number(alpha)),
        ])),
    );
    resources.insert("ExtGState", PdfObject::Dictionary(states));
    name
}

fn add_xobject(resources: &mut PdfDictionary, number: u32) -> String {
    let mut xobjects = dict_resource(resources, "XObject");
    let name = next_resource_name(&xobjects, "OxEdIm");
    xobjects.insert(&name, reference(number, 0));
    resources.insert("XObject", PdfObject::Dictionary(xobjects));
    name
}

fn dict_resource(resources: &PdfDictionary, key: &str) -> PdfDictionary {
    resources
        .get(key)
        .and_then(PdfObject::as_dict)
        .cloned()
        .unwrap_or_else(PdfDictionary::empty)
}

fn next_resource_name(dict: &PdfDictionary, prefix: &str) -> String {
    let mut idx = 1usize;
    loop {
        let candidate = format!("{prefix}{idx}");
        if !dict.contains_key(&candidate) {
            return candidate;
        }
        idx += 1;
    }
}

fn stream_object(number: u32, raw: Vec<u8>) -> IncrementalObject {
    IncrementalObject {
        number,
        generation: 0,
        object: PdfObject::Stream {
            dict: PdfDictionary::empty(),
            raw,
        },
    }
}

fn write_full_rewrite(reader: &PdfReader, changes: Vec<IncrementalObject>) -> Result<Vec<u8>> {
    if reader.is_encrypted() {
        return Err(OxideError::UnsupportedFeature(
            "editing full rewrite does not re-encrypt encrypted inputs".to_string(),
        ));
    }
    let mut changed = BTreeMap::new();
    for object in changes {
        if object.generation != 0 {
            return Err(OxideError::UnsupportedFeature(
                "editing full rewrite currently supports generation-0 updates only".to_string(),
            ));
        }
        changed.insert(object.number, object.object);
    }

    let mut objects = BTreeMap::new();
    for (number, generation) in reader.object_ids() {
        if generation != 0 {
            return Err(OxideError::UnsupportedFeature(
                "editing full rewrite currently supports generation-0 source objects only"
                    .to_string(),
            ));
        }
        let object = reader.get_object(number, generation)?;
        if is_xref_stream(&object) {
            continue;
        }
        objects.insert(number, changed.remove(&number).unwrap_or(object));
    }
    for (number, object) in changed {
        objects.insert(number, object);
    }

    let outputs = objects
        .into_iter()
        .map(|(number, object)| OutputObject { number, object })
        .collect();
    let (root, root_generation) = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("editing full rewrite: trailer is missing /Root".to_string())
    })?;
    if root_generation != 0 {
        return Err(OxideError::UnsupportedFeature(
            "editing full rewrite currently supports generation-0 /Root only".to_string(),
        ));
    }
    let info = match reader.info_reference() {
        Some((number, 0)) => Some(number),
        Some(_) => {
            return Err(OxideError::UnsupportedFeature(
                "editing full rewrite currently supports generation-0 /Info only".to_string(),
            ))
        }
        None => None,
    };
    PdfWriter::new(outputs, root)
        .with_info(info)
        .with_id(reader.first_file_id())
        .with_mode(WriterMode::XrefStreamWithObjStm)
        .write()
}

fn is_xref_stream(object: &PdfObject) -> bool {
    matches!(object, PdfObject::Stream { dict, .. } if dict.get_name("Type") == Some("XRef"))
}

fn next_free_object_number(reader: &PdfReader) -> u32 {
    let max_seen = reader
        .object_ids()
        .into_iter()
        .map(|(number, _)| number)
        .max()
        .unwrap_or(0);
    let trailer_size = reader.size().unwrap_or(0).max(0) as u32;
    max_seen.max(trailer_size.saturating_sub(1)) + 1
}

fn alloc_object(next: &mut u32) -> u32 {
    let number = *next;
    *next += 1;
    number
}

fn image_color_space(channels: u8) -> Result<&'static str> {
    match channels {
        1 => Ok("DeviceGray"),
        3 => Ok("DeviceRGB"),
        4 => Ok("DeviceCMYK"),
        _ => Err(OxideError::UnsupportedFeature(format!(
            "editing: unsupported image channel count {channels}"
        ))),
    }
}

fn edit_image_from_raw(raw: RawImage) -> Result<EditImage> {
    if !raw.is_valid() || raw.bits_per_sample != 8 {
        return Err(OxideError::MalformedPdf(
            "editing: image samples must be non-empty 8-bit data".to_string(),
        ));
    }
    let mut samples = Vec::with_capacity(raw.pixel_count() * 3);
    let mut alpha = Vec::with_capacity(raw.pixel_count());
    match raw.channels {
        3 => samples = raw.pixels,
        4 => {
            for px in raw.pixels.chunks_exact(4) {
                samples.extend_from_slice(&px[..3]);
                alpha.push(px[3]);
            }
        }
        other => {
            return Err(OxideError::UnsupportedFeature(format!(
                "editing: unsupported raw image channel count {other}"
            )))
        }
    }
    let smask = (!alpha.is_empty()).then(|| EditSoftMask {
        width: raw.width,
        height: raw.height,
        data: flate_encode(&alpha, 9),
    });
    Ok(EditImage {
        width: raw.width,
        height: raw.height,
        color_space: "DeviceRGB",
        bits_per_component: 8,
        data: flate_encode(&samples, 9),
        filter: ImageFilter::FlateDecode,
        smask,
    })
}

fn image_dict(image: &EditImage, smask_number: Option<u32>) -> PdfDictionary {
    let mut out = dict(&[
        ("Type", PdfObject::Name("XObject".to_string())),
        ("Subtype", PdfObject::Name("Image".to_string())),
        ("Width", PdfObject::Integer(i64::from(image.width))),
        ("Height", PdfObject::Integer(i64::from(image.height))),
        ("ColorSpace", PdfObject::Name(image.color_space.to_string())),
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
        out.insert("SMask", reference(number, 0));
    }
    out
}

fn smask_dict(mask: &EditSoftMask) -> PdfDictionary {
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

fn page_center(page: &PdfPage) -> (f64, f64) {
    (
        (page.media_box[0] + page.media_box[2]) / 2.0,
        (page.media_box[1] + page.media_box[3]) / 2.0,
    )
}

fn approximate_text_width(text: &str, font_size: f64) -> f64 {
    text.chars().count() as f64 * font_size * 0.5
}

fn encode_win_ansi_lossy(text: &str) -> Vec<u8> {
    text.chars()
        .map(|ch| {
            if ('\u{20}'..='\u{7e}').contains(&ch) {
                ch as u8
            } else {
                b'?'
            }
        })
        .collect()
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

fn reference(number: u32, generation: u16) -> PdfObject {
    PdfObject::Reference { number, generation }
}

fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
    let mut out = PdfDictionary::empty();
    for (key, value) in entries {
        out.insert(*key, value.clone());
    }
    out
}

fn pdf_number(value: f64) -> PdfObject {
    if (value - value.round()).abs() < 0.000_000_1 {
        PdfObject::Integer(value.round() as i64)
    } else {
        PdfObject::Real((value * 10_000.0).round() / 10_000.0)
    }
}

fn fmt_num(value: f64) -> String {
    let value = if value.abs() < 0.000_000_1 {
        0.0
    } else {
        (value * 10_000.0).round() / 10_000.0
    };
    if (value - value.round()).abs() < 0.000_000_1 {
        return format!("{}", value.round() as i64);
    }
    let mut s = format!("{value:.4}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
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
