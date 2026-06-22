//! Additive page-content editing for existing PDFs.
//!
//! Edits are emitted as new content streams that are prepended as underlays or
//! appended as overlays. Existing page content streams are left untouched.

use std::collections::{BTreeMap, BTreeSet};

use crate::content::{
    concat_matrix, transform_point, Color, ColorSpace, ContentOperation, ContentParser, Matrix,
    Operand, IDENTITY_MATRIX,
};
use crate::document::{PdfDocument, PdfPage};
use crate::error::{OxideError, Result};
use crate::filters::{decode_stream_lossless, flate_encode};
use crate::images::decoder::{ImageDecoder, RawImage};
use crate::info::decode_pdf_text_string;
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

/// Redaction options. Redaction removes affected content and then paints a mark.
#[derive(Debug, Clone, PartialEq)]
pub struct RedactionOptions {
    pub fill: Color,
    pub scrub_metadata: bool,
}

impl Default for RedactionOptions {
    fn default() -> Self {
        Self {
            fill: Color::black(),
            scrub_metadata: true,
        }
    }
}

/// Common annotation styling and metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationOptions {
    pub color: Color,
    pub opacity: f64,
    pub author: Option<String>,
    pub contents: Option<String>,
}

impl Default for AnnotationOptions {
    fn default() -> Self {
        Self {
            color: Color::device_rgb(1.0, 0.9, 0.0),
            opacity: 0.35,
            author: None,
            contents: None,
        }
    }
}

impl AnnotationOptions {
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    pub fn contents(mut self, contents: impl Into<String>) -> Self {
        self.contents = Some(contents.into());
        self
    }
}

/// Additive editor for an existing PDF.
pub struct PdfEditor {
    document: PdfDocument,
    edits: BTreeMap<usize, Vec<PageEdit>>,
    redactions: BTreeMap<usize, Vec<RedactionEdit>>,
    annotations: BTreeMap<usize, Vec<AnnotationEdit>>,
    form_fills: BTreeMap<String, FormValue>,
    flatten_forms: bool,
}

impl PdfEditor {
    pub fn open_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self {
            document: PdfDocument::open_bytes(bytes)?,
            edits: BTreeMap::new(),
            redactions: BTreeMap::new(),
            annotations: BTreeMap::new(),
            form_fills: BTreeMap::new(),
            flatten_forms: false,
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

    /// Redact a page rectangle by removing intersecting text/image/path content
    /// and drawing a fill mark over the now-empty region.
    ///
    /// Redactions intentionally require full rewrite output. Incremental output
    /// preserves the original byte prefix, which would retain the old revision's
    /// sensitive content.
    pub fn redact(
        &mut self,
        page_number: usize,
        rect: ImageRect,
        options: RedactionOptions,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.redactions
            .entry(page_number)
            .or_default()
            .push(RedactionEdit { rect, options });
        Ok(self)
    }

    pub fn add_highlight_annotation(
        &mut self,
        page_number: usize,
        rect: ImageRect,
        options: AnnotationOptions,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.annotations
            .entry(page_number)
            .or_default()
            .push(AnnotationEdit::Add(AnnotationSpec {
                kind: AnnotationKind::Highlight,
                rect,
                label: options.contents.clone().unwrap_or_default(),
                options,
            }));
        Ok(self)
    }

    pub fn add_text_note_annotation(
        &mut self,
        page_number: usize,
        rect: ImageRect,
        contents: impl Into<String>,
        options: AnnotationOptions,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.annotations
            .entry(page_number)
            .or_default()
            .push(AnnotationEdit::Add(AnnotationSpec {
                kind: AnnotationKind::TextNote,
                rect,
                label: contents.into(),
                options,
            }));
        Ok(self)
    }

    pub fn add_stamp_annotation(
        &mut self,
        page_number: usize,
        rect: ImageRect,
        label: impl Into<String>,
        options: AnnotationOptions,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.annotations
            .entry(page_number)
            .or_default()
            .push(AnnotationEdit::Add(AnnotationSpec {
                kind: AnnotationKind::Stamp,
                rect,
                label: label.into(),
                options,
            }));
        Ok(self)
    }

    pub fn add_link_uri(
        &mut self,
        page_number: usize,
        rect: ImageRect,
        uri: impl Into<String>,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.annotations
            .entry(page_number)
            .or_default()
            .push(AnnotationEdit::Add(AnnotationSpec {
                kind: AnnotationKind::Link,
                rect,
                label: uri.into(),
                options: AnnotationOptions::default(),
            }));
        Ok(self)
    }

    pub fn edit_annotation_contents(
        &mut self,
        page_number: usize,
        annotation_index: usize,
        contents: impl Into<String>,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.annotations
            .entry(page_number)
            .or_default()
            .push(AnnotationEdit::EditContents {
                index: annotation_index,
                contents: contents.into(),
            });
        Ok(self)
    }

    pub fn delete_annotations_in_rect(
        &mut self,
        page_number: usize,
        rect: ImageRect,
    ) -> Result<&mut Self> {
        self.validate_page(page_number)?;
        self.annotations
            .entry(page_number)
            .or_default()
            .push(AnnotationEdit::DeleteInRect { rect });
        Ok(self)
    }

    pub fn set_form_text(
        &mut self,
        field_name: impl Into<String>,
        value: impl Into<String>,
    ) -> &mut Self {
        self.form_fills
            .insert(field_name.into(), FormValue::Text(value.into()));
        self
    }

    pub fn set_form_choice(
        &mut self,
        field_name: impl Into<String>,
        value: impl Into<String>,
    ) -> &mut Self {
        self.form_fills
            .insert(field_name.into(), FormValue::Choice(value.into()));
        self
    }

    pub fn set_form_checkbox(&mut self, field_name: impl Into<String>, checked: bool) -> &mut Self {
        self.form_fills
            .insert(field_name.into(), FormValue::Checkbox(checked));
        self
    }

    /// Bake current AcroForm widget values into page content and remove fields.
    pub fn flatten_forms(&mut self) -> &mut Self {
        self.flatten_forms = true;
        self
    }

    pub fn save_to_bytes(&self, mode: EditMode) -> Result<Vec<u8>> {
        if mode == EditMode::Incremental && !self.redactions.is_empty() {
            return Err(OxideError::UnsupportedFeature(
                "redaction requires full rewrite; incremental output preserves old revision bytes"
                    .to_string(),
            ));
        }
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
        let mut changes = ChangeSet::new(self.document.reader());
        let mut redact_report = RedactionReport::default();
        let flatten_visuals = self.apply_form_changes(&pages, &mut changes)?;

        let mut page_numbers: BTreeSet<usize> = BTreeSet::new();
        page_numbers.extend(self.edits.keys().copied());
        page_numbers.extend(self.redactions.keys().copied());
        page_numbers.extend(self.annotations.keys().copied());
        page_numbers.extend(flatten_visuals.keys().copied());

        for page_number in page_numbers {
            let edits = self
                .edits
                .get(&page_number)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let redactions = self
                .redactions
                .get(&page_number)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let annotation_edits = self
                .annotations
                .get(&page_number)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let form_visuals = flatten_visuals
                .get(&page_number)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let page = by_page.get(&page_number).ok_or_else(|| {
                OxideError::MalformedPdf(format!("page {page_number} is out of range"))
            })?;
            let page_object = changes.current_object(
                self.document.reader(),
                page.object_number,
                page.generation_number,
            )?;
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
                write_edit_command(out, &edit.command, &mut resources, &mut changes)?;
            }
            for redaction in redactions {
                write_redaction_mark(&mut overlay, redaction);
            }
            for visual in form_visuals {
                write_form_flatten_visual(&mut overlay, &mut resources, visual);
            }
            for edit in annotation_edits {
                if let AnnotationEdit::Add(spec) = edit {
                    write_annotation_visual_to_content(&mut overlay, &mut resources, spec);
                }
            }

            let mut content_refs = Vec::new();
            if !underlay.is_empty() {
                let number = changes.alloc();
                changes.insert_new_stream(number, underlay);
                content_refs.push(reference(number, 0));
            }
            if redactions.is_empty() {
                for (number, generation) in &page.contents {
                    content_refs.push(reference(*number, *generation));
                }
            } else {
                let rewritten = rewrite_page_content_for_redaction(
                    self.document.reader(),
                    page,
                    &resources,
                    redactions,
                    &mut redact_report,
                    &mut changes,
                )?;
                let number = changes.alloc();
                changes.insert_new_stream(number, rewritten);
                content_refs.push(reference(number, 0));
            }
            if !overlay.is_empty() {
                let number = changes.alloc();
                changes.insert_new_stream(number, overlay);
                content_refs.push(reference(number, 0));
            }

            apply_annotation_edits(
                self.document.reader(),
                &mut page_dict,
                redactions,
                annotation_edits,
                self.flatten_forms,
                &mut changes,
            )?;
            page_dict.insert("Resources", PdfObject::Dictionary(resources));
            page_dict.insert("Contents", PdfObject::Array(content_refs));
            changes.insert_existing(
                page.object_number,
                page.generation_number,
                PdfObject::Dictionary(page_dict),
            );
        }

        if redact_report.scrub_metadata && !redact_report.removed_text.is_empty() {
            self.apply_metadata_scrub(&redact_report.removed_text, &mut changes)?;
        }

        Ok(changes.into_vec())
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

    fn apply_metadata_scrub(
        &self,
        removed_text: &BTreeSet<String>,
        changes: &mut ChangeSet,
    ) -> Result<()> {
        for (number, generation) in self.document.reader().object_ids() {
            let object = changes.current_object(self.document.reader(), number, generation)?;
            let mut scrubbed = object.clone();
            if scrub_pdf_strings(&mut scrubbed, removed_text) {
                changes.insert_existing(number, generation, scrubbed);
            }
        }
        Ok(())
    }

    fn apply_form_changes(
        &self,
        pages: &[PdfPage],
        changes: &mut ChangeSet,
    ) -> Result<BTreeMap<usize, Vec<FormFlattenVisual>>> {
        if self.form_fills.is_empty() && !self.flatten_forms {
            return Ok(BTreeMap::new());
        }
        let fields = collect_acroform_fields(self.document.reader(), pages)?;
        let mut visuals: BTreeMap<usize, Vec<FormFlattenVisual>> = BTreeMap::new();
        let mut matched = BTreeSet::new();

        for field in &fields {
            let requested = self.form_fills.get(&field.name);
            if let Some(value) = requested {
                matched.insert(field.name.clone());
                update_field_value(self.document.reader(), changes, field, value)?;
            }
            let value = requested
                .cloned()
                .or_else(|| field.current_value.clone())
                .unwrap_or_else(|| FormValue::Text(String::new()));
            if self.flatten_forms {
                for widget in &field.widgets {
                    visuals
                        .entry(widget.page_number)
                        .or_default()
                        .push(FormFlattenVisual {
                            page_number: widget.page_number,
                            rect: widget.rect,
                            value: value.clone(),
                        });
                }
            }
        }

        for name in self.form_fills.keys() {
            if !matched.contains(name) {
                return Err(OxideError::MalformedPdf(format!(
                    "form field '{name}' was not found"
                )));
            }
        }

        if self.flatten_forms {
            remove_acroform_from_catalog(self.document.reader(), changes)?;
        }
        Ok(visuals)
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
struct RedactionEdit {
    rect: ImageRect,
    options: RedactionOptions,
}

#[derive(Debug, Clone)]
enum AnnotationEdit {
    Add(AnnotationSpec),
    EditContents { index: usize, contents: String },
    DeleteInRect { rect: ImageRect },
}

#[derive(Debug, Clone)]
struct AnnotationSpec {
    kind: AnnotationKind,
    rect: ImageRect,
    label: String,
    options: AnnotationOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotationKind {
    Highlight,
    TextNote,
    Stamp,
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FormValue {
    Text(String),
    Choice(String),
    Checkbox(bool),
}

#[derive(Debug, Clone)]
struct FormFlattenVisual {
    page_number: usize,
    rect: ImageRect,
    value: FormValue,
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

struct ChangeSet {
    next: u32,
    objects: BTreeMap<(u32, u16), PdfObject>,
}

impl ChangeSet {
    fn new(reader: &PdfReader) -> Self {
        Self {
            next: next_free_object_number(reader),
            objects: BTreeMap::new(),
        }
    }

    fn alloc(&mut self) -> u32 {
        let number = self.next;
        self.next += 1;
        number
    }

    fn insert_existing(&mut self, number: u32, generation: u16, object: PdfObject) {
        self.objects.insert((number, generation), object);
    }

    fn insert_new(&mut self, number: u32, object: PdfObject) {
        self.insert_existing(number, 0, object);
    }

    fn insert_new_stream(&mut self, number: u32, raw: Vec<u8>) {
        self.insert_new(
            number,
            PdfObject::Stream {
                dict: PdfDictionary::empty(),
                raw,
            },
        );
    }

    fn current_object(
        &self,
        reader: &PdfReader,
        number: u32,
        generation: u16,
    ) -> Result<PdfObject> {
        self.objects
            .get(&(number, generation))
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| reader.get_object(number, generation))
    }

    fn into_vec(self) -> Vec<IncrementalObject> {
        self.objects
            .into_iter()
            .map(|((number, generation), object)| IncrementalObject {
                number,
                generation,
                object,
            })
            .collect()
    }
}

#[derive(Default)]
struct RedactionReport {
    removed_text: BTreeSet<String>,
    scrub_metadata: bool,
}

#[derive(Clone)]
struct RedactionState {
    ctm: Matrix,
    stack: Vec<Matrix>,
    text_matrix: Matrix,
    text_line_matrix: Matrix,
    font_size: f64,
}

impl Default for RedactionState {
    fn default() -> Self {
        Self {
            ctm: IDENTITY_MATRIX,
            stack: Vec::new(),
            text_matrix: IDENTITY_MATRIX,
            text_line_matrix: IDENTITY_MATRIX,
            font_size: 12.0,
        }
    }
}

impl RedactionState {
    fn apply(&mut self, op: &ContentOperation) {
        match op.operator.as_str() {
            "q" => self.stack.push(self.ctm),
            "Q" => {
                if let Some(ctm) = self.stack.pop() {
                    self.ctm = ctm;
                }
            }
            "cm" => {
                let m = [
                    op.number(0).unwrap_or(1.0),
                    op.number(1).unwrap_or(0.0),
                    op.number(2).unwrap_or(0.0),
                    op.number(3).unwrap_or(1.0),
                    op.number(4).unwrap_or(0.0),
                    op.number(5).unwrap_or(0.0),
                ];
                self.ctm = concat_matrix(&m, &self.ctm);
            }
            "BT" => {
                self.text_matrix = IDENTITY_MATRIX;
                self.text_line_matrix = IDENTITY_MATRIX;
            }
            "Tf" => {
                if let Some(size) = op.number(1) {
                    self.font_size = size.abs().max(1.0);
                }
            }
            "Td" | "TD" => {
                let tx = op.number(0).unwrap_or(0.0);
                let ty = op.number(1).unwrap_or(0.0);
                self.text_line_matrix[4] += tx;
                self.text_line_matrix[5] += ty;
                self.text_matrix = self.text_line_matrix;
            }
            "Tm" => {
                self.text_matrix = [
                    op.number(0).unwrap_or(1.0),
                    op.number(1).unwrap_or(0.0),
                    op.number(2).unwrap_or(0.0),
                    op.number(3).unwrap_or(1.0),
                    op.number(4).unwrap_or(0.0),
                    op.number(5).unwrap_or(0.0),
                ];
                self.text_line_matrix = self.text_matrix;
            }
            "T*" => {
                self.text_line_matrix[5] -= self.font_size * 1.2;
                self.text_matrix = self.text_line_matrix;
            }
            "Tj" => {
                if let Some(bytes) = op.string_bytes(0) {
                    self.advance_text(bytes.len());
                }
            }
            "'" => {
                self.apply(&ContentOperation::new("T*", Vec::new()));
                if let Some(bytes) = op.string_bytes(0) {
                    self.advance_text(bytes.len());
                }
            }
            "\"" => {
                self.apply(&ContentOperation::new("T*", Vec::new()));
                if let Some(bytes) = op.string_bytes(2) {
                    self.advance_text(bytes.len());
                }
            }
            "TJ" => {
                if let Some(items) = op.operand(0).and_then(Operand::as_array) {
                    for item in items {
                        match item {
                            Operand::String(bytes) => self.advance_text(bytes.len()),
                            Operand::Integer(n) => self.advance_text_units(-(*n as f64)),
                            Operand::Real(n) => self.advance_text_units(-*n),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn advance_text(&mut self, bytes: usize) {
        self.advance_text_units(bytes as f64 * 500.0);
    }

    fn advance_text_units(&mut self, units: f64) {
        self.text_matrix[4] += units / 1000.0 * self.font_size;
    }

    fn glyph_rect(&self, index: usize) -> ImageRect {
        let char_width = self.font_size * 0.5;
        let x0 = self.text_matrix[4] + index as f64 * char_width;
        let y0 = self.text_matrix[5] - self.font_size * 0.25;
        let (x1, y1) = transform_point(&self.ctm, x0, y0);
        let (x2, y2) = transform_point(&self.ctm, x0 + char_width, y0 + self.font_size);
        rect_from_corners(x1, y1, x2, y2)
    }

    fn unit_rect(&self) -> ImageRect {
        let (x1, y1) = transform_point(&self.ctm, 0.0, 0.0);
        let (x2, y2) = transform_point(&self.ctm, 1.0, 0.0);
        let (x3, y3) = transform_point(&self.ctm, 0.0, 1.0);
        let (x4, y4) = transform_point(&self.ctm, 1.0, 1.0);
        rect_from_points(&[(x1, y1), (x2, y2), (x3, y3), (x4, y4)])
    }
}

#[derive(Default)]
struct PendingPath {
    operations: Vec<ContentOperation>,
    bbox: Option<ImageRect>,
}

impl PendingPath {
    fn push(&mut self, op: ContentOperation, state: &RedactionState) {
        self.expand_from_operation(&op, state);
        self.operations.push(op);
    }

    fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    fn intersects(&self, redactions: &[RedactionEdit]) -> bool {
        self.bbox
            .as_ref()
            .map(|bbox| {
                redactions
                    .iter()
                    .any(|redaction| rects_intersect(*bbox, redaction.rect))
            })
            .unwrap_or(false)
    }

    fn flush_to(&mut self, out: &mut Vec<u8>) {
        for op in self.operations.drain(..) {
            serialize_content_operation(&op, out);
        }
        self.bbox = None;
    }

    fn clear(&mut self) {
        self.operations.clear();
        self.bbox = None;
    }

    fn expand_from_operation(&mut self, op: &ContentOperation, state: &RedactionState) {
        let points: Vec<(f64, f64)> = match op.operator.as_str() {
            "re" => {
                let x = op.number(0).unwrap_or(0.0);
                let y = op.number(1).unwrap_or(0.0);
                let w = op.number(2).unwrap_or(0.0);
                let h = op.number(3).unwrap_or(0.0);
                vec![(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
            }
            "m" | "l" => vec![(op.number(0).unwrap_or(0.0), op.number(1).unwrap_or(0.0))],
            "c" => vec![
                (op.number(0).unwrap_or(0.0), op.number(1).unwrap_or(0.0)),
                (op.number(2).unwrap_or(0.0), op.number(3).unwrap_or(0.0)),
                (op.number(4).unwrap_or(0.0), op.number(5).unwrap_or(0.0)),
            ],
            "v" | "y" => vec![
                (op.number(0).unwrap_or(0.0), op.number(1).unwrap_or(0.0)),
                (op.number(2).unwrap_or(0.0), op.number(3).unwrap_or(0.0)),
            ],
            _ => Vec::new(),
        };
        for (x, y) in points {
            let (tx, ty) = transform_point(&state.ctm, x, y);
            self.include_point(tx, ty);
        }
    }

    fn include_point(&mut self, x: f64, y: f64) {
        self.bbox = Some(match self.bbox {
            Some(rect) => ImageRect {
                x: rect.x.min(x),
                y: rect.y.min(y),
                width: (rect.x + rect.width).max(x) - rect.x.min(x),
                height: (rect.y + rect.height).max(y) - rect.y.min(y),
            },
            None => ImageRect::new(x, y, 0.0, 0.0),
        });
    }
}

fn rewrite_page_content_for_redaction(
    reader: &PdfReader,
    page: &PdfPage,
    resources: &PdfDictionary,
    redactions: &[RedactionEdit],
    report: &mut RedactionReport,
    changes: &mut ChangeSet,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut state = RedactionState::default();
    let mut pending_path = PendingPath::default();
    report.scrub_metadata |= redactions
        .iter()
        .any(|redaction| redaction.options.scrub_metadata);

    for (number, generation) in &page.contents {
        let object = reader.get_object(*number, *generation)?;
        let decoded = decode_stream_lossless(&object, reader)?;
        let operations = ContentParser::parse(&decoded.data)?;
        for op in operations {
            if is_path_construction(&op) {
                pending_path.push(op, &state);
                continue;
            }
            if is_path_paint(&op) {
                if pending_path.intersects(redactions) {
                    pending_path.clear();
                } else {
                    pending_path.flush_to(&mut out);
                    serialize_content_operation(&op, &mut out);
                }
                state.apply(&op);
                continue;
            }
            if !pending_path.is_empty() {
                pending_path.flush_to(&mut out);
            }
            match op.operator.as_str() {
                "Tj" => {
                    if let Some(rewritten) = redact_text_show(&op, &state, redactions, report) {
                        serialize_content_operation(&rewritten, &mut out);
                    }
                    state.apply(&op);
                }
                "TJ" => {
                    if let Some(rewritten) = redact_text_array(&op, &state, redactions, report) {
                        serialize_content_operation(&rewritten, &mut out);
                    }
                    state.apply(&op);
                }
                "'" | "\"" => {
                    if text_operation_intersects(&op, &state, redactions) {
                        collect_text_from_operation(&op, report);
                    } else {
                        serialize_content_operation(&op, &mut out);
                    }
                    state.apply(&op);
                }
                "Do" => {
                    let image_rect = state.unit_rect();
                    if redactions
                        .iter()
                        .any(|redaction| rects_intersect(image_rect, redaction.rect))
                    {
                        if let Some(name) = op.name(0) {
                            if let Some((obj, gen)) = xobject_reference(resources, reader, name) {
                                changes.insert_existing(obj, gen, blank_image_xobject());
                            }
                        }
                    } else {
                        serialize_content_operation(&op, &mut out);
                    }
                    state.apply(&op);
                }
                _ => {
                    serialize_content_operation(&op, &mut out);
                    state.apply(&op);
                }
            }
        }
        if !pending_path.is_empty() {
            pending_path.flush_to(&mut out);
        }
        out.push(b'\n');
    }
    Ok(out)
}

fn redact_text_show(
    op: &ContentOperation,
    state: &RedactionState,
    redactions: &[RedactionEdit],
    report: &mut RedactionReport,
) -> Option<ContentOperation> {
    let bytes = op.string_bytes(0)?;
    let rewritten = redact_string_bytes(bytes, state, redactions, report);
    rewritten.map(|operands| ContentOperation::new("TJ", vec![Operand::Array(operands)]))
}

fn redact_text_array(
    op: &ContentOperation,
    state: &RedactionState,
    redactions: &[RedactionEdit],
    report: &mut RedactionReport,
) -> Option<ContentOperation> {
    let items = op.operand(0).and_then(Operand::as_array)?;
    let mut local = state.clone();
    let mut out = Vec::new();
    for item in items {
        match item {
            Operand::String(bytes) => {
                if let Some(mut replacement) =
                    redact_string_bytes(bytes, &local, redactions, report)
                {
                    out.append(&mut replacement);
                }
                local.advance_text(bytes.len());
            }
            Operand::Integer(n) => {
                out.push(Operand::Integer(*n));
                local.advance_text_units(-(*n as f64));
            }
            Operand::Real(n) => {
                out.push(Operand::Real(*n));
                local.advance_text_units(-*n);
            }
            other => out.push(other.clone()),
        }
    }
    (!out.is_empty()).then(|| ContentOperation::new("TJ", vec![Operand::Array(out)]))
}

fn redact_string_bytes(
    bytes: &[u8],
    state: &RedactionState,
    redactions: &[RedactionEdit],
    report: &mut RedactionReport,
) -> Option<Vec<Operand>> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut removed = Vec::new();
    let mut removed_run = 0usize;

    for (idx, byte) in bytes.iter().copied().enumerate() {
        let glyph_rect = state.glyph_rect(idx);
        if redactions
            .iter()
            .any(|redaction| rects_intersect(glyph_rect, redaction.rect))
        {
            if !current.is_empty() {
                out.push(Operand::String(std::mem::take(&mut current)));
            }
            removed.push(byte);
            removed_run += 1;
        } else {
            if removed_run > 0 {
                out.push(Operand::Integer(-(removed_run as i64 * 500)));
                removed_run = 0;
            }
            current.push(byte);
        }
    }
    if !current.is_empty() {
        out.push(Operand::String(current));
    }
    if removed_run > 0 {
        out.push(Operand::Integer(-(removed_run as i64 * 500)));
    }
    if !removed.is_empty() {
        let text = decode_pdf_text_string(&removed);
        if !text.trim().is_empty() {
            report.removed_text.insert(text);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn text_operation_intersects(
    op: &ContentOperation,
    state: &RedactionState,
    redactions: &[RedactionEdit],
) -> bool {
    let bytes = match op.operator.as_str() {
        "'" => op.string_bytes(0),
        "\"" => op.string_bytes(2),
        _ => None,
    };
    bytes
        .map(|bytes| {
            bytes.iter().enumerate().any(|(idx, _)| {
                let glyph_rect = state.glyph_rect(idx);
                redactions
                    .iter()
                    .any(|redaction| rects_intersect(glyph_rect, redaction.rect))
            })
        })
        .unwrap_or(false)
}

fn collect_text_from_operation(op: &ContentOperation, report: &mut RedactionReport) {
    let bytes = match op.operator.as_str() {
        "'" => op.string_bytes(0),
        "\"" => op.string_bytes(2),
        _ => None,
    };
    if let Some(bytes) = bytes {
        let text = decode_pdf_text_string(bytes);
        if !text.trim().is_empty() {
            report.removed_text.insert(text);
        }
    }
}

fn is_path_construction(op: &ContentOperation) -> bool {
    matches!(
        op.operator.as_str(),
        "m" | "l" | "c" | "v" | "y" | "h" | "re"
    )
}

fn is_path_paint(op: &ContentOperation) -> bool {
    matches!(
        op.operator.as_str(),
        "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n"
    )
}

fn write_redaction_mark(out: &mut Vec<u8>, redaction: &RedactionEdit) {
    let style = EditRectStyle {
        stroke: None,
        fill: Some(redaction.options.fill.clone()),
        line_width: 0.0,
        opacity: 1.0,
    };
    write_rect(out, None, redaction.rect, &style);
}

fn write_form_flatten_visual(
    out: &mut Vec<u8>,
    resources: &mut PdfDictionary,
    visual: &FormFlattenVisual,
) {
    let _ = visual.page_number;
    match &visual.value {
        FormValue::Text(text) | FormValue::Choice(text) => {
            let font = ensure_standard_font(resources);
            let style = EditTextStyle::new((visual.rect.height * 0.45).clamp(8.0, 14.0))
                .fill(Color::black());
            write_text(
                out,
                &font,
                None,
                text,
                visual.rect.x + 3.0,
                visual.rect.y + visual.rect.height * 0.35,
                &style,
            );
        }
        FormValue::Checkbox(checked) => {
            let style = EditRectStyle {
                stroke: Some(Color::black()),
                fill: Some(Color::device_gray(1.0)),
                line_width: 1.0,
                opacity: 1.0,
            };
            write_rect(out, None, visual.rect, &style);
            if *checked {
                out.extend_from_slice(
                    format!(
                        "q 0 0 0 RG 2 w {} {} m {} {} l {} {} l S Q\n",
                        fmt_num(visual.rect.x + 3.0),
                        fmt_num(visual.rect.y + visual.rect.height * 0.5),
                        fmt_num(visual.rect.x + visual.rect.width * 0.4),
                        fmt_num(visual.rect.y + 3.0),
                        fmt_num(visual.rect.x + visual.rect.width - 3.0),
                        fmt_num(visual.rect.y + visual.rect.height - 3.0)
                    )
                    .as_bytes(),
                );
            }
        }
    }
}

fn serialize_content_operation(op: &ContentOperation, out: &mut Vec<u8>) {
    for operand in &op.operands {
        serialize_content_operand(operand, out);
        out.push(b' ');
    }
    out.extend_from_slice(op.operator.as_bytes());
    out.push(b'\n');
}

fn serialize_content_operand(operand: &Operand, out: &mut Vec<u8>) {
    match operand {
        Operand::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Operand::Real(value) => out.extend_from_slice(fmt_num(*value).as_bytes()),
        Operand::Boolean(value) => {
            out.extend_from_slice(if *value { b"true" } else { b"false" });
        }
        Operand::Name(value) => {
            out.push(b'/');
            out.extend_from_slice(value.as_bytes());
        }
        Operand::String(bytes) => {
            out.push(b'<');
            out.extend_from_slice(hex_string(bytes).as_bytes());
            out.push(b'>');
        }
        Operand::Array(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b' ');
                }
                serialize_content_operand(item, out);
            }
            out.push(b']');
        }
    }
}

fn xobject_reference(
    resources: &PdfDictionary,
    reader: &PdfReader,
    name: &str,
) -> Option<(u32, u16)> {
    let xobjects = resources.get("XObject")?;
    let resolved = reader.resolve(xobjects.clone()).ok()?;
    let dict = resolved.as_dict()?;
    dict.get(name).and_then(PdfObject::as_reference)
}

fn blank_image_xobject() -> PdfObject {
    PdfObject::Stream {
        dict: dict(&[
            ("Type", PdfObject::Name("XObject".to_string())),
            ("Subtype", PdfObject::Name("Image".to_string())),
            ("Width", PdfObject::Integer(1)),
            ("Height", PdfObject::Integer(1)),
            ("ColorSpace", PdfObject::Name("DeviceGray".to_string())),
            ("BitsPerComponent", PdfObject::Integer(8)),
        ]),
        raw: vec![0],
    }
}

fn apply_annotation_edits(
    reader: &PdfReader,
    page_dict: &mut PdfDictionary,
    redactions: &[RedactionEdit],
    edits: &[AnnotationEdit],
    remove_widgets: bool,
    changes: &mut ChangeSet,
) -> Result<()> {
    let mut annots = resolve_annotation_refs(reader, page_dict.get("Annots"))?;
    if !redactions.is_empty() || remove_widgets {
        let mut kept = Vec::new();
        for annot_ref in annots {
            let annot = reader.get_and_resolve(annot_ref.0, annot_ref.1)?;
            let Some(dict) = annot.as_dict() else {
                kept.push(annot_ref);
                continue;
            };
            let remove_for_redaction = rect_from_dict(dict, reader)
                .map(|rect| {
                    redactions
                        .iter()
                        .any(|redaction| rects_intersect(rect, redaction.rect))
                })
                .unwrap_or(false);
            let remove_widget = remove_widgets && dict.get_name("Subtype") == Some("Widget");
            if !remove_for_redaction && !remove_widget {
                kept.push(annot_ref);
            }
        }
        annots = kept;
    }

    for edit in edits {
        match edit {
            AnnotationEdit::Add(spec) => {
                let appearance = (spec.kind != AnnotationKind::Link)
                    .then(|| annotation_appearance(spec, changes))
                    .transpose()?;
                let annot_number = changes.alloc();
                let annot = annotation_dictionary(spec, appearance);
                changes.insert_new(annot_number, PdfObject::Dictionary(annot));
                annots.push((annot_number, 0));
            }
            AnnotationEdit::EditContents { index, contents } => {
                if let Some((number, generation)) = annots.get(*index).copied() {
                    let object = changes.current_object(reader, number, generation)?;
                    let mut dict = object.as_dict().cloned().ok_or_else(|| {
                        OxideError::MalformedPdf(format!(
                            "annotation {number} {generation} is not a dictionary"
                        ))
                    })?;
                    dict.insert("Contents", pdf_text_string(contents));
                    changes.insert_existing(number, generation, PdfObject::Dictionary(dict));
                }
            }
            AnnotationEdit::DeleteInRect { rect } => {
                let mut kept = Vec::new();
                for annot_ref in annots {
                    let annot = changes.current_object(reader, annot_ref.0, annot_ref.1)?;
                    let delete = annot
                        .as_dict()
                        .and_then(|dict| rect_from_dict(dict, reader))
                        .map(|annot_rect| rects_intersect(annot_rect, *rect))
                        .unwrap_or(false);
                    if !delete {
                        kept.push(annot_ref);
                    }
                }
                annots = kept;
            }
        }
    }

    if annots.is_empty() {
        page_dict.remove("Annots");
    } else {
        page_dict.insert(
            "Annots",
            PdfObject::Array(
                annots
                    .into_iter()
                    .map(|(number, generation)| reference(number, generation))
                    .collect(),
            ),
        );
    }
    Ok(())
}

fn resolve_annotation_refs(
    reader: &PdfReader,
    annots: Option<&PdfObject>,
) -> Result<Vec<(u32, u16)>> {
    let Some(annots) = annots else {
        return Ok(Vec::new());
    };
    let resolved = reader.resolve(annots.clone())?;
    Ok(resolved
        .as_array()
        .map(|items| items.iter().filter_map(PdfObject::as_reference).collect())
        .unwrap_or_default())
}

fn annotation_appearance(spec: &AnnotationSpec, changes: &mut ChangeSet) -> Result<u32> {
    let number = changes.alloc();
    let mut raw = Vec::new();
    let width = spec.rect.width.max(1.0);
    let height = spec.rect.height.max(1.0);
    match spec.kind {
        AnnotationKind::Highlight => {
            write_fill_color(&mut raw, &spec.options.color);
            raw.extend_from_slice(
                format!("0 0 {} {} re f\n", fmt_num(width), fmt_num(height)).as_bytes(),
            );
        }
        AnnotationKind::TextNote => {
            write_fill_color(&mut raw, &spec.options.color);
            raw.extend_from_slice(
                format!("0 0 {} {} re f\n", fmt_num(width), fmt_num(height)).as_bytes(),
            );
            raw.extend_from_slice(b"0 0 0 RG 1 w 0 0 16 16 re S\n");
        }
        AnnotationKind::Stamp => {
            raw.extend_from_slice(b"q 0.9 0.95 1 rg 0 0 0 RG 1 w\n");
            raw.extend_from_slice(
                format!("0 0 {} {} re B\nQ\n", fmt_num(width), fmt_num(height)).as_bytes(),
            );
            let font = "OxAnnF1";
            raw.extend_from_slice(
                format!(
                    "BT /{} {} Tf 0 0 0 rg 4 {} Td <{}> Tj ET\n",
                    font,
                    fmt_num((height * 0.38).clamp(8.0, 16.0)),
                    fmt_num(height * 0.38),
                    hex_string(&encode_win_ansi_lossy(&spec.label))
                )
                .as_bytes(),
            );
        }
        AnnotationKind::Link => {}
    }
    let mut form_dict = form_xobject_dict(width, height);
    if spec.kind == AnnotationKind::Stamp {
        let mut resources = PdfDictionary::empty();
        let mut fonts = PdfDictionary::empty();
        fonts.insert(
            "OxAnnF1",
            PdfObject::Dictionary(dict(&[
                ("Type", PdfObject::Name("Font".to_string())),
                ("Subtype", PdfObject::Name("Type1".to_string())),
                ("BaseFont", PdfObject::Name("Helvetica".to_string())),
                ("Encoding", PdfObject::Name("WinAnsiEncoding".to_string())),
            ])),
        );
        resources.insert("Font", PdfObject::Dictionary(fonts));
        form_dict.insert("Resources", PdfObject::Dictionary(resources));
    }
    changes.insert_new(
        number,
        PdfObject::Stream {
            dict: form_dict,
            raw,
        },
    );
    Ok(number)
}

fn annotation_dictionary(spec: &AnnotationSpec, appearance_number: Option<u32>) -> PdfDictionary {
    let mut annot = PdfDictionary::empty();
    annot.insert("Type", PdfObject::Name("Annot".to_string()));
    annot.insert(
        "Subtype",
        PdfObject::Name(
            match spec.kind {
                AnnotationKind::Highlight => "Highlight",
                AnnotationKind::TextNote => "Text",
                AnnotationKind::Stamp => "Stamp",
                AnnotationKind::Link => "Link",
            }
            .to_string(),
        ),
    );
    annot.insert("Rect", rect_array(spec.rect));
    annot.insert("F", PdfObject::Integer(4));
    if let Some(author) = &spec.options.author {
        annot.insert("T", pdf_text_string(author));
    }
    let contents = if spec.options.contents.is_some() {
        spec.options.contents.as_deref().unwrap_or("")
    } else {
        &spec.label
    };
    if !contents.is_empty() {
        annot.insert("Contents", pdf_text_string(contents));
    }
    if spec.kind != AnnotationKind::Link {
        annot.insert("C", color_array(&spec.options.color));
        annot.insert("CA", pdf_number(spec.options.opacity.clamp(0.0, 1.0)));
        if spec.kind == AnnotationKind::Highlight {
            annot.insert("QuadPoints", highlight_quad_points(spec.rect));
        }
        if let Some(appearance_number) = appearance_number {
            let mut ap = PdfDictionary::empty();
            ap.insert("N", reference(appearance_number, 0));
            annot.insert("AP", PdfObject::Dictionary(ap));
        }
    } else {
        let mut action = PdfDictionary::empty();
        action.insert("S", PdfObject::Name("URI".to_string()));
        action.insert("URI", PdfObject::String(spec.label.as_bytes().to_vec()));
        annot.insert("A", PdfObject::Dictionary(action));
        annot.insert(
            "Border",
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(0),
            ]),
        );
    }
    annot
}

fn write_annotation_visual_to_content(
    out: &mut Vec<u8>,
    resources: &mut PdfDictionary,
    spec: &AnnotationSpec,
) {
    match spec.kind {
        AnnotationKind::Highlight => {
            let gs = ensure_extgstate(resources, spec.options.opacity);
            let style = EditRectStyle {
                stroke: None,
                fill: Some(spec.options.color.clone()),
                line_width: 0.0,
                opacity: spec.options.opacity,
            };
            write_rect(out, Some(&gs), spec.rect, &style);
        }
        AnnotationKind::Stamp => {
            let style = EditRectStyle {
                stroke: Some(Color::black()),
                fill: Some(spec.options.color.clone()),
                line_width: 1.0,
                opacity: spec.options.opacity,
            };
            write_rect(out, None, spec.rect, &style);
            let font = ensure_standard_font(resources);
            let text_style = EditTextStyle::new((spec.rect.height * 0.35).clamp(8.0, 18.0));
            write_text(
                out,
                &font,
                None,
                &spec.label,
                spec.rect.x + 4.0,
                spec.rect.y + spec.rect.height * 0.38,
                &text_style,
            );
        }
        AnnotationKind::TextNote => {
            let style = EditRectStyle {
                stroke: Some(Color::black()),
                fill: Some(spec.options.color.clone()),
                line_width: 1.0,
                opacity: spec.options.opacity,
            };
            write_rect(out, None, spec.rect, &style);
        }
        AnnotationKind::Link => {}
    }
}

#[derive(Debug, Clone)]
struct FieldInfo {
    object_ref: (u32, u16),
    name: String,
    dict: PdfDictionary,
    widgets: Vec<WidgetInfo>,
    current_value: Option<FormValue>,
}

#[derive(Debug, Clone)]
struct WidgetInfo {
    object_ref: (u32, u16),
    dict: PdfDictionary,
    rect: ImageRect,
    page_number: usize,
}

fn collect_acroform_fields(reader: &PdfReader, pages: &[PdfPage]) -> Result<Vec<FieldInfo>> {
    let catalog = reader
        .root_reference()
        .and_then(|(n, g)| reader.get_and_resolve(n, g).ok())
        .and_then(|obj| obj.as_dict().cloned())
        .ok_or_else(|| OxideError::MalformedPdf("catalog is missing".to_string()))?;
    let Some(acroform_obj) = catalog.get("AcroForm") else {
        return Ok(Vec::new());
    };
    let acroform = reader.resolve(acroform_obj.clone())?;
    let Some(acroform_dict) = acroform.as_dict() else {
        return Ok(Vec::new());
    };
    let Some(fields) = acroform_dict
        .get("Fields")
        .and_then(|obj| reader.resolve(obj.clone()).ok())
        .and_then(|obj| obj.as_array().map(|items| items.to_vec()))
    else {
        return Ok(Vec::new());
    };
    let mut page_annots = BTreeMap::new();
    for page in pages {
        let page_obj = reader.get_and_resolve(page.object_number, page.generation_number)?;
        let Some(page_dict) = page_obj.as_dict() else {
            continue;
        };
        for annot_ref in resolve_annotation_refs(reader, page_dict.get("Annots"))? {
            page_annots.insert(annot_ref, page.page_number);
        }
    }
    let mut out = Vec::new();
    for field in fields {
        walk_field_for_editing(reader, &field, "", &page_annots, 0, &mut out)?;
    }
    Ok(out)
}

fn walk_field_for_editing(
    reader: &PdfReader,
    object: &PdfObject,
    parent_name: &str,
    page_annots: &BTreeMap<(u32, u16), usize>,
    depth: usize,
    out: &mut Vec<FieldInfo>,
) -> Result<()> {
    if depth > 32 {
        return Ok(());
    }
    let Some(object_ref) = object.as_reference() else {
        return Ok(());
    };
    let resolved = reader.get_and_resolve(object_ref.0, object_ref.1)?;
    let Some(dict) = resolved.as_dict().cloned() else {
        return Ok(());
    };
    let name = qualified_field_name(parent_name, dict.get("T"));
    let kids = dict
        .get("Kids")
        .and_then(|obj| reader.resolve(obj.clone()).ok())
        .and_then(|obj| obj.as_array().map(|items| items.to_vec()))
        .unwrap_or_default();
    let child_fields: Vec<PdfObject> = kids
        .iter()
        .filter(|kid| kid_is_editable_field(reader, kid))
        .cloned()
        .collect();
    if !child_fields.is_empty() {
        for kid in child_fields {
            walk_field_for_editing(reader, &kid, &name, page_annots, depth + 1, out)?;
        }
        return Ok(());
    }
    let Some(field_type) = inherited_field_name(reader, &dict, "FT") else {
        return Ok(());
    };
    let mut widgets = Vec::new();
    if dict.get_name("Subtype") == Some("Widget") && dict.get("Rect").is_some() {
        if let Some(widget) = widget_info(reader, object_ref, &dict, page_annots) {
            widgets.push(widget);
        }
    }
    for kid in kids {
        if let Some(kid_ref) = kid.as_reference() {
            let kid_obj = reader.get_and_resolve(kid_ref.0, kid_ref.1)?;
            if let Some(kid_dict) = kid_obj.as_dict() {
                if kid_dict.get_name("Subtype") == Some("Widget") {
                    if let Some(widget) = widget_info(reader, kid_ref, kid_dict, page_annots) {
                        widgets.push(widget);
                    }
                }
            }
        }
    }
    out.push(FieldInfo {
        object_ref,
        name,
        current_value: inherited_field_object(reader, &dict, "V")
            .and_then(|value| form_value_from_object(&field_type, &value)),
        dict,
        widgets,
    });
    Ok(())
}

fn update_field_value(
    reader: &PdfReader,
    changes: &mut ChangeSet,
    field: &FieldInfo,
    value: &FormValue,
) -> Result<()> {
    let mut field_dict = changes
        .current_object(reader, field.object_ref.0, field.object_ref.1)?
        .as_dict()
        .cloned()
        .unwrap_or_else(|| field.dict.clone());
    let value_obj = form_value_pdf_object(value);
    field_dict.insert("V", value_obj.clone());
    if matches!(value, FormValue::Checkbox(_)) {
        let state = checkbox_state(value);
        field_dict.insert("AS", PdfObject::Name(state.to_string()));
    }
    changes.insert_existing(
        field.object_ref.0,
        field.object_ref.1,
        PdfObject::Dictionary(field_dict),
    );

    for widget in &field.widgets {
        let mut widget_dict = changes
            .current_object(reader, widget.object_ref.0, widget.object_ref.1)?
            .as_dict()
            .cloned()
            .unwrap_or_else(|| widget.dict.clone());
        widget_dict.insert("V", value_obj.clone());
        if matches!(value, FormValue::Checkbox(_)) {
            widget_dict.insert("AS", PdfObject::Name(checkbox_state(value).to_string()));
        }
        let ap_number = changes.alloc();
        changes.insert_new(
            ap_number,
            appearance_stream_for_form_value(widget.rect, value),
        );
        let mut ap = PdfDictionary::empty();
        ap.insert("N", reference(ap_number, 0));
        widget_dict.insert("AP", PdfObject::Dictionary(ap));
        changes.insert_existing(
            widget.object_ref.0,
            widget.object_ref.1,
            PdfObject::Dictionary(widget_dict),
        );
    }
    Ok(())
}

fn remove_acroform_from_catalog(reader: &PdfReader, changes: &mut ChangeSet) -> Result<()> {
    let (root, generation) = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("flatten forms: trailer is missing /Root".to_string())
    })?;
    let object = changes.current_object(reader, root, generation)?;
    let mut catalog = object.as_dict().cloned().ok_or_else(|| {
        OxideError::MalformedPdf("flatten forms: /Root is not a dictionary".to_string())
    })?;
    catalog.remove("AcroForm");
    changes.insert_existing(root, generation, PdfObject::Dictionary(catalog));
    Ok(())
}

fn widget_info(
    reader: &PdfReader,
    object_ref: (u32, u16),
    dict: &PdfDictionary,
    page_annots: &BTreeMap<(u32, u16), usize>,
) -> Option<WidgetInfo> {
    Some(WidgetInfo {
        object_ref,
        dict: dict.clone(),
        rect: rect_from_dict(dict, reader)?,
        page_number: *page_annots.get(&object_ref).unwrap_or(&1),
    })
}

fn kid_is_editable_field(reader: &PdfReader, object: &PdfObject) -> bool {
    let Ok(resolved) = reader.resolve(object.clone()) else {
        return false;
    };
    let Some(dict) = resolved.as_dict() else {
        return false;
    };
    dict.contains_key("T") || dict.contains_key("FT")
}

fn qualified_field_name(parent: &str, local: Option<&PdfObject>) -> String {
    let local = local.and_then(pdf_string_or_name).unwrap_or_default();
    match (parent.is_empty(), local.is_empty()) {
        (true, true) => String::new(),
        (true, false) => local,
        (false, true) => parent.to_string(),
        (false, false) => format!("{parent}.{local}"),
    }
}

fn inherited_field_name(reader: &PdfReader, dict: &PdfDictionary, key: &str) -> Option<String> {
    inherited_field_object(reader, dict, key).and_then(|obj| obj.as_name().map(str::to_string))
}

fn inherited_field_object(
    reader: &PdfReader,
    dict: &PdfDictionary,
    key: &str,
) -> Option<PdfObject> {
    let mut current = dict.clone();
    for _ in 0..32 {
        if let Some(value) = current.get(key) {
            return reader.resolve(value.clone()).ok();
        }
        let parent = current.get("Parent")?.clone();
        current = reader.resolve(parent).ok()?.as_dict()?.clone();
    }
    None
}

fn form_value_from_object(field_type: &str, value: &PdfObject) -> Option<FormValue> {
    match field_type {
        "Btn" => Some(FormValue::Checkbox(
            value.as_name().map(|name| name != "Off").unwrap_or(false),
        )),
        "Ch" => Some(FormValue::Choice(
            pdf_string_or_name(value).unwrap_or_default(),
        )),
        _ => Some(FormValue::Text(
            pdf_string_or_name(value).unwrap_or_default(),
        )),
    }
}

fn form_value_pdf_object(value: &FormValue) -> PdfObject {
    match value {
        FormValue::Text(text) | FormValue::Choice(text) => pdf_text_string(text),
        FormValue::Checkbox(checked) => {
            PdfObject::Name(if *checked { "Yes" } else { "Off" }.to_string())
        }
    }
}

fn checkbox_state(value: &FormValue) -> &'static str {
    match value {
        FormValue::Checkbox(true) => "Yes",
        _ => "Off",
    }
}

fn appearance_stream_for_form_value(rect: ImageRect, value: &FormValue) -> PdfObject {
    let width = rect.width.max(1.0);
    let height = rect.height.max(1.0);
    let mut raw = Vec::new();
    let mut form_dict = form_xobject_dict(width, height);
    match value {
        FormValue::Text(text) | FormValue::Choice(text) => {
            let mut resources = PdfDictionary::empty();
            let mut fonts = PdfDictionary::empty();
            fonts.insert(
                "OxFormF1",
                PdfObject::Dictionary(dict(&[
                    ("Type", PdfObject::Name("Font".to_string())),
                    ("Subtype", PdfObject::Name("Type1".to_string())),
                    ("BaseFont", PdfObject::Name("Helvetica".to_string())),
                    ("Encoding", PdfObject::Name("WinAnsiEncoding".to_string())),
                ])),
            );
            resources.insert("Font", PdfObject::Dictionary(fonts));
            form_dict.insert("Resources", PdfObject::Dictionary(resources));
            raw.extend_from_slice(
                format!(
                    "q 1 1 1 rg 0 0 {} {} re f 0 0 0 RG 1 w 0 0 {} {} re S Q\n",
                    fmt_num(width),
                    fmt_num(height),
                    fmt_num(width),
                    fmt_num(height)
                )
                .as_bytes(),
            );
            raw.extend_from_slice(
                format!(
                    "BT /OxFormF1 {} Tf 0 0 0 rg 3 {} Td <{}> Tj ET\n",
                    fmt_num((height * 0.45).clamp(8.0, 14.0)),
                    fmt_num(height * 0.35),
                    hex_string(&encode_win_ansi_lossy(text))
                )
                .as_bytes(),
            );
        }
        FormValue::Checkbox(checked) => {
            raw.extend_from_slice(
                format!(
                    "q 1 1 1 rg 0 0 {} {} re f 0 0 0 RG 1 w 0 0 {} {} re S\n",
                    fmt_num(width),
                    fmt_num(height),
                    fmt_num(width),
                    fmt_num(height)
                )
                .as_bytes(),
            );
            if *checked {
                raw.extend_from_slice(
                    format!(
                        "2 w 3 {} m {} 3 l {} {} l S\n",
                        fmt_num(height * 0.5),
                        fmt_num(width * 0.4),
                        fmt_num(width - 3.0),
                        fmt_num(height - 3.0)
                    )
                    .as_bytes(),
                );
            }
            raw.extend_from_slice(b"Q\n");
        }
    }
    PdfObject::Stream {
        dict: form_dict,
        raw,
    }
}

fn write_edit_command(
    out: &mut Vec<u8>,
    command: &EditCommand,
    resources: &mut PdfDictionary,
    changes: &mut ChangeSet,
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
                Some(changes.alloc())
            } else {
                None
            };
            let image_number = changes.alloc();
            if let (Some(number), Some(mask)) = (smask_number, image.smask.as_ref()) {
                changes.insert_new(
                    number,
                    PdfObject::Stream {
                        dict: smask_dict(mask),
                        raw: mask.data.clone(),
                    },
                );
            }
            changes.insert_new(
                image_number,
                PdfObject::Stream {
                    dict: image_dict(image, smask_number),
                    raw: image.data.clone(),
                },
            );
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

fn rect_from_dict(dict: &PdfDictionary, reader: &PdfReader) -> Option<ImageRect> {
    let rect_obj = dict.get("Rect")?;
    let resolved = reader.resolve(rect_obj.clone()).ok()?;
    let arr = resolved.as_array()?;
    if arr.len() != 4 {
        return None;
    }
    let mut vals = [0.0; 4];
    for (idx, item) in arr.iter().enumerate() {
        vals[idx] = reader.resolve(item.clone()).ok()?.as_number()?;
    }
    Some(rect_from_corners(vals[0], vals[1], vals[2], vals[3]))
}

fn rect_array(rect: ImageRect) -> PdfObject {
    PdfObject::Array(vec![
        pdf_number(rect.x),
        pdf_number(rect.y),
        pdf_number(rect.x + rect.width),
        pdf_number(rect.y + rect.height),
    ])
}

fn color_array(color: &Color) -> PdfObject {
    PdfObject::Array(
        color
            .components
            .iter()
            .take(3)
            .map(|component| pdf_number(component.clamp(0.0, 1.0)))
            .collect(),
    )
}

fn highlight_quad_points(rect: ImageRect) -> PdfObject {
    PdfObject::Array(vec![
        pdf_number(rect.x),
        pdf_number(rect.y + rect.height),
        pdf_number(rect.x + rect.width),
        pdf_number(rect.y + rect.height),
        pdf_number(rect.x),
        pdf_number(rect.y),
        pdf_number(rect.x + rect.width),
        pdf_number(rect.y),
    ])
}

fn form_xobject_dict(width: f64, height: f64) -> PdfDictionary {
    dict(&[
        ("Type", PdfObject::Name("XObject".to_string())),
        ("Subtype", PdfObject::Name("Form".to_string())),
        (
            "BBox",
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                pdf_number(width),
                pdf_number(height),
            ]),
        ),
    ])
}

fn pdf_text_string(text: &str) -> PdfObject {
    if text.is_ascii() {
        PdfObject::String(text.as_bytes().to_vec())
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for code in text.encode_utf16() {
            bytes.push((code >> 8) as u8);
            bytes.push((code & 0xff) as u8);
        }
        PdfObject::String(bytes)
    }
}

fn pdf_string_or_name(object: &PdfObject) -> Option<String> {
    match object {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        PdfObject::Name(name) => Some(name.clone()),
        _ => None,
    }
}

fn scrub_pdf_strings(object: &mut PdfObject, removed_text: &BTreeSet<String>) -> bool {
    match object {
        PdfObject::String(bytes) => {
            let text = decode_pdf_text_string(bytes);
            let scrubbed = removed_text
                .iter()
                .fold(text.clone(), |acc, secret| acc.replace(secret, ""));
            if scrubbed != text {
                *object = pdf_text_string(&scrubbed);
                true
            } else {
                false
            }
        }
        PdfObject::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= scrub_pdf_strings(item, removed_text);
            }
            changed
        }
        PdfObject::Dictionary(dict) => {
            let keys: Vec<String> = dict.entries().map(|(key, _)| key.clone()).collect();
            let mut changed = false;
            for key in keys {
                if let Some(value) = dict.get(&key).cloned() {
                    let mut value = value;
                    if scrub_pdf_strings(&mut value, removed_text) {
                        dict.insert(key, value);
                        changed = true;
                    }
                }
            }
            changed
        }
        PdfObject::Stream { dict, .. } => {
            let mut wrapper = PdfObject::Dictionary(dict.clone());
            let changed = scrub_pdf_strings(&mut wrapper, removed_text);
            if let PdfObject::Dictionary(scrubbed) = wrapper {
                *dict = scrubbed;
            }
            changed
        }
        _ => false,
    }
}

fn rects_intersect(a: ImageRect, b: ImageRect) -> bool {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    a.x < bx2 && ax2 > b.x && a.y < by2 && ay2 > b.y
}

fn rect_from_corners(x1: f64, y1: f64, x2: f64, y2: f64) -> ImageRect {
    ImageRect {
        x: x1.min(x2),
        y: y1.min(y2),
        width: (x1 - x2).abs(),
        height: (y1 - y2).abs(),
    }
}

fn rect_from_points(points: &[(f64, f64)]) -> ImageRect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in points {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    if !min_x.is_finite() {
        return ImageRect::new(0.0, 0.0, 0.0, 0.0);
    }
    ImageRect::new(min_x, min_y, max_x - min_x, max_y - min_y)
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
