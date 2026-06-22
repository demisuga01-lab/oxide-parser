use std::collections::HashMap;
use std::path::Path;

use crate::content::{ContentOperation, ContentParser};
use crate::document::{PdfDocument, PdfPage};
use crate::error::{OxideError, Result};
use crate::images::decoder::{ImageDecoder, RawImage};
use crate::images::encoder::{ImageEncoder, ImageOutputFormat};
use crate::images::locator::{ImageLocateOptions, ImageLocator, ImageReference};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::render::{PageRenderer, PixelBuffer, RenderMode, Viewport, WHITE};
use crate::text::{TextExtractOptions, TextExtractor, TextFormatOptions};

#[derive(Debug, Clone, Default)]
pub struct PageResources {
    pub fonts: HashMap<String, PdfDictionary>,
    pub xobjects: HashMap<String, (u32, u16)>,
    pub color_spaces: HashMap<String, PdfObject>,
    pub ext_g_states: HashMap<String, PdfDictionary>,
    pub patterns: HashMap<String, PdfObject>,
    pub shadings: HashMap<String, PdfObject>,
}

/// Fetch a resource sub-dictionary (e.g. `/Font`, `/ColorSpace`, `/Pattern`),
/// resolving an indirect reference when the entry is one. Real-world PDFs
/// (notably pdf.js-generated files) often store these sub-dictionaries as
/// indirect objects, e.g. `/ColorSpace 12 0 R`, so a direct `get_dict` lookup
/// would miss them and leave the corresponding resources empty.
fn resolve_subdict(
    resources: &PdfDictionary,
    key: &str,
    reader: &PdfReader,
) -> Option<PdfDictionary> {
    match resources.get(key) {
        Some(PdfObject::Dictionary(d)) => Some(d.clone()),
        Some(obj @ PdfObject::Reference { .. }) => match reader.resolve(obj.clone()) {
            Ok(PdfObject::Dictionary(d)) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

impl PageResources {
    pub fn from_dict(resources: &PdfDictionary, reader: &PdfReader) -> Self {
        let mut page_resources = PageResources::default();

        if let Some(font_dict) = resolve_subdict(resources, "Font", reader) {
            for (name, value) in font_dict.entries() {
                match reader.resolve(value.clone()) {
                    Ok(PdfObject::Dictionary(dict)) => {
                        page_resources.fonts.insert(name.clone(), dict);
                    }
                    Ok(other) => {
                        log::warn!(
                            "PageResources: font '{}' resolved to non-dict {}",
                            name,
                            other.variant_name()
                        );
                    }
                    Err(err) => {
                        log::warn!("PageResources: could not resolve font '{}': {}", name, err);
                    }
                }
            }
        }

        if let Some(xobject_dict) = resolve_subdict(resources, "XObject", reader) {
            for (name, value) in xobject_dict.entries() {
                if let Some(reference) = value.as_reference() {
                    page_resources.xobjects.insert(name.clone(), reference);
                } else {
                    log::warn!(
                        "PageResources: XObject '{}' is not an indirect reference",
                        name
                    );
                }
            }
        }

        if let Some(color_space_dict) = resolve_subdict(resources, "ColorSpace", reader) {
            for (name, value) in color_space_dict.entries() {
                let resolved = match reader.resolve(value.clone()) {
                    Ok(object) => object,
                    Err(err) => {
                        log::warn!(
                            "PageResources: could not resolve ColorSpace '{}': {}",
                            name,
                            err
                        );
                        value.clone()
                    }
                };
                page_resources.color_spaces.insert(name.clone(), resolved);
            }
        }

        if let Some(ext_g_state_dict) = resolve_subdict(resources, "ExtGState", reader) {
            for (name, value) in ext_g_state_dict.entries() {
                match reader.resolve(value.clone()) {
                    Ok(PdfObject::Dictionary(dict)) => {
                        page_resources.ext_g_states.insert(name.clone(), dict);
                    }
                    Ok(other) => {
                        log::warn!(
                            "PageResources: ExtGState '{}' resolved to non-dict {}",
                            name,
                            other.variant_name()
                        );
                    }
                    Err(err) => {
                        log::warn!("PageResources: ExtGState '{}' error: {}", name, err);
                    }
                }
            }
        }

        if let Some(pattern_dict) = resolve_subdict(resources, "Pattern", reader) {
            for (name, value) in pattern_dict.entries() {
                page_resources.patterns.insert(name.clone(), value.clone());
            }
        }

        if let Some(shading_dict) = resolve_subdict(resources, "Shading", reader) {
            for (name, value) in shading_dict.entries() {
                page_resources.shadings.insert(name.clone(), value.clone());
            }
        }

        page_resources
    }
}

/// Parse a `/Resources` object (a direct dictionary or an indirect reference)
/// into a [`PageResources`]. Used when rendering Form XObjects that carry their
/// own resource dictionary.
///
/// Returns an empty [`PageResources`] when the object does not resolve to a
/// dictionary. Never panics on malformed input.
pub(crate) fn parse_resources_from_obj(res_obj: &PdfObject, reader: &PdfReader) -> PageResources {
    let dict = match res_obj {
        PdfObject::Dictionary(d) => d.clone(),
        PdfObject::Reference { number, generation } => {
            match reader.get_and_resolve(*number, *generation) {
                Ok(PdfObject::Dictionary(d)) => d,
                _ => return PageResources::default(),
            }
        }
        _ => return PageResources::default(),
    };
    PageResources::from_dict(&dict, reader)
}

pub struct ContentEngine {
    doc: PdfDocument,
}

impl ContentEngine {
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        let doc = PdfDocument::open_path(path)?;
        Ok(Self { doc })
    }

    pub fn open_bytes(data: Vec<u8>) -> Result<Self> {
        let doc = PdfDocument::open_bytes(data)?;
        Ok(Self { doc })
    }

    /// Open a PDF from bytes, supplying a password for encrypted PDFs.
    ///
    /// For non-encrypted PDFs the password is ignored. For encrypted PDFs with
    /// an empty user password, pass `b""` (or just call [`open_bytes`]).
    ///
    /// [`open_bytes`]: ContentEngine::open_bytes
    pub fn open_bytes_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self> {
        let doc = PdfDocument::open_bytes_with_password(data, password)?;
        Ok(Self { doc })
    }

    /// Open a PDF from a file path, supplying a password for encrypted PDFs.
    pub fn open_path_with_password(path: impl AsRef<Path>, password: &[u8]) -> Result<Self> {
        let doc = PdfDocument::open_path_with_password(path, password)?;
        Ok(Self { doc })
    }

    /// True if the underlying reader has an active encryption (decryption)
    /// context — i.e. the document was encrypted and successfully unlocked.
    pub fn is_encrypted(&self) -> bool {
        self.doc.reader().is_encrypted()
    }

    pub fn document(&self) -> &PdfDocument {
        &self.doc
    }

    pub fn page_count(&self) -> Result<usize> {
        Ok(self.doc.get_pages()?.len())
    }

    pub fn get_page_content(&self, page_number: usize) -> Result<Vec<ContentOperation>> {
        self.validate_page(page_number)?;
        let bytes = self.doc.get_page_content_bytes(page_number)?;
        ContentParser::parse(&bytes)
    }

    pub fn get_page_resources(&self, page_number: usize) -> Result<PageResources> {
        self.validate_page(page_number)?;
        let pages = self.doc.get_pages()?;
        let page = pages
            .get(page_number - 1)
            .ok_or_else(|| OxideError::MalformedPdf(format!("page {page_number} out of range")))?;
        Ok(PageResources::from_dict(&page.resources, self.doc.reader()))
    }

    pub fn get_page(&self, page_number: usize) -> Result<PdfPage> {
        self.validate_page(page_number)?;
        let pages = self.doc.get_pages()?;
        pages
            .get(page_number - 1)
            .cloned()
            .ok_or_else(|| OxideError::MalformedPdf(format!("page {page_number} out of range")))
    }

    pub fn get_page_text(&self, page_number: usize) -> Result<String> {
        let extractor = TextExtractor::new();
        let options = TextExtractOptions {
            pages: Some(vec![page_number]),
            format: TextFormatOptions {
                include_page_markers: false,
                ..Default::default()
            },
            ..Default::default()
        };
        extractor.extract(self, &options)
    }

    /// Run geometric layout analysis (XY-cut segmentation + reading order) on a
    /// page, returning the structured [`PageLayout`](crate::analysis::layout::PageLayout)
    /// (blocks → lines, in reading order). This is **additive** — it does not
    /// affect [`get_page_text`](Self::get_page_text) or the default extraction
    /// path. See [`crate::analysis::layout`].
    pub fn analyze_page_layout(
        &self,
        page_number: usize,
    ) -> Result<crate::analysis::layout::PageLayout> {
        self.analyze_page_layout_with(
            page_number,
            &crate::analysis::layout::LayoutConfig::default(),
        )
    }

    /// Layout analysis with an explicit [`LayoutConfig`](crate::analysis::layout::LayoutConfig).
    pub fn analyze_page_layout_with(
        &self,
        page_number: usize,
        config: &crate::analysis::layout::LayoutConfig,
    ) -> Result<crate::analysis::layout::PageLayout> {
        let ops = self.get_page_content(page_number)?;
        let resources = self.get_page_resources(page_number)?;
        let mut collector = crate::text::TextCollector::new(resources, self.doc.reader());
        let chunks = collector.collect(&ops);
        Ok(crate::analysis::layout::analyze_page(&chunks, config))
    }

    /// Structured (layout-aware) text for a page: the page's text in
    /// reading order recovered by XY-cut segmentation, with blocks separated by
    /// a blank line. Correct for multi-column pages where the default
    /// top-to-bottom dump (and plain `pdftotext`) interleaves columns. Additive;
    /// the default [`get_page_text`](Self::get_page_text) is unchanged.
    pub fn get_page_text_structured(&self, page_number: usize) -> Result<String> {
        Ok(self.analyze_page_layout(page_number)?.text())
    }

    /// Extract semantic structure for selected pages. Tagged PDFs use the
    /// authored `/StructTreeRoot` and MCID links; untagged PDFs fall back to the
    /// geometric layout analyzer. Additive; the default text path is unchanged.
    pub fn extract_semantic_document(
        &self,
        pages: &[usize],
    ) -> Result<crate::semantic::SemanticDocument> {
        crate::semantic::extract_semantic_document(self, pages)
    }

    /// Readable text view of [`extract_semantic_document`](Self::extract_semantic_document).
    pub fn extract_semantic_text(&self, pages: &[usize]) -> Result<String> {
        Ok(self.extract_semantic_document(pages)?.to_text())
    }

    /// Detect and extract tables on a page (the `extract-tables` tool — a
    /// capability Poppler's CLIs lack). Tries ruled-grid detection from drawn
    /// lines first, then falls back to borderless inference from text alignment.
    /// See [`crate::analysis::tables`].
    pub fn extract_tables(
        &self,
        page_number: usize,
    ) -> Result<Vec<crate::analysis::tables::Table>> {
        let semantic = self.extract_semantic_document(&[page_number])?;
        if semantic.tagged && !semantic.tables.is_empty() {
            return Ok(semantic.tables);
        }

        let ops = self.get_page_content(page_number)?;
        let resources = self.get_page_resources(page_number)?;
        let mut collector = crate::text::TextCollector::new(resources, self.doc.reader());
        let chunks = collector.collect(&ops);
        let graphics = crate::analysis::graphics::collect_graphics(&ops);
        Ok(crate::analysis::tables::detect_tables(&chunks, &graphics))
    }

    /// Build a typed, ordered **document model** for the selected pages: each
    /// recovered block is classified (heading/paragraph/list/figure/caption/
    /// table/header/footer/page-number) and placed in a robust reading order
    /// (tagged-PDF authored order when present, else a geometric precedence
    /// graph). A capability beyond Poppler's CLIs. See [`crate::docmodel`].
    pub fn build_document_model(
        &self,
        pages: &[usize],
    ) -> Result<crate::docmodel::DocumentModel> {
        crate::docmodel::build_document_model(self, pages)
    }

    /// Parse this PDF into the canonical [`crate::parse::Document`] model — the
    /// single structured representation every output format (Markdown / JSON /
    /// HTML) is serialized from. Wraps [`Self::build_document_model`] with
    /// metadata, a per-page view, provenance, and inline-styled text.
    pub fn parse_document(
        &self,
        options: &crate::parse::ParseOptions,
    ) -> Result<crate::parse::Document> {
        crate::parse::parse(self, options)
    }

    /// Extract structured **key-value / form fields** (invoice number, date,
    /// total, line items; receipt merchant/amount; form label→value pairs).
    ///
    /// Combines exact AcroForm fields with a pure-Rust spatial label→value
    /// engine and document-type profiles. Operates on the canonical model, so it
    /// works identically on digital-born and OCR'd pages. See [`crate::extract`].
    pub fn extract_fields(
        &self,
        options: &crate::extract::ExtractOptions,
    ) -> Result<crate::extract::ExtractedFields> {
        crate::extract::extract_fields(self, options)
    }

    /// Visible page size `(width, height)` in user-space units, from the page's
    /// `/CropBox` (falling back to `/MediaBox`). Used by the document-model
    /// layer for margin-band (header/footer) detection and page-area thresholds.
    pub(crate) fn page_dimensions(&self, page_number: usize) -> Result<(f64, f64)> {
        self.validate_page(page_number)?;
        let pages = self.doc.get_pages()?;
        let page = pages.get(page_number - 1).ok_or_else(|| {
            OxideError::MalformedPdf(format!("page {page_number} out of range"))
        })?;
        let b = page.crop_box;
        Ok(((b[2] - b[0]).abs(), (b[3] - b[1]).abs()))
    }

    /// The page's `/Rotate` value, normalized to one of `0`, `90`, `180`, `270`
    /// (clockwise). Used by the document-model layer to normalize text/graphics
    /// coordinates into upright reading orientation before layout analysis.
    pub(crate) fn page_rotation(&self, page_number: usize) -> Result<i32> {
        self.validate_page(page_number)?;
        let pages = self.doc.get_pages()?;
        let page = pages.get(page_number - 1).ok_or_else(|| {
            OxideError::MalformedPdf(format!("page {page_number} out of range"))
        })?;
        Ok(page.rotate.rem_euclid(360))
    }

    /// The page's crop box `[x0, y0, x1, y1]` in user space (falls back to the
    /// media box). The origin needed to rotate coordinates about the page.
    pub(crate) fn page_crop_box(&self, page_number: usize) -> Result<[f64; 4]> {
        self.validate_page(page_number)?;
        let pages = self.doc.get_pages()?;
        let page = pages.get(page_number - 1).ok_or_else(|| {
            OxideError::MalformedPdf(format!("page {page_number} out of range"))
        })?;
        Ok(page.crop_box)
    }

    /// External hyperlinks on a page: each `/Link` annotation with a URI action
    /// (`/A << /S /URI /URI (…) >>`, or a direct `/URI`), as `(rect, uri)` where
    /// `rect` is the annotation's `/Rect` `[x0,y0,x1,y1]` in user space (y-up).
    /// Used by the digital-born pass to attach `[text](href)` links to the blocks
    /// the link rectangles overlap. Best-effort: a malformed annotation is
    /// skipped, never an error. Never resolves remote targets.
    pub(crate) fn page_links(&self, page_number: usize) -> Result<Vec<([f64; 4], String)>> {
        self.validate_page(page_number)?;
        let pages = self.doc.get_pages()?;
        let page = pages.get(page_number - 1).ok_or_else(|| {
            OxideError::MalformedPdf(format!("page {page_number} out of range"))
        })?;
        let reader = self.doc.reader();
        let page_obj = reader.get_and_resolve(page.object_number, page.generation_number)?;
        let Some(page_dict) = page_obj.as_dict() else {
            return Ok(Vec::new());
        };
        let Some(annots_obj) = page_dict.get("Annots") else {
            return Ok(Vec::new());
        };
        let annots = reader.resolve(annots_obj.clone())?;
        let Some(items) = annots.as_array() else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for item in items {
            let Ok(resolved) = reader.resolve(item.clone()) else {
                continue;
            };
            let Some(adict) = resolved.as_dict() else {
                continue;
            };
            if adict.get_name("Subtype") != Some("Link") {
                continue;
            }
            let Some(rect) = rect_from_obj(adict.get("Rect"), reader) else {
                continue;
            };
            if let Some(uri) = link_uri(adict, reader) {
                out.push((rect, uri));
            }
        }
        Ok(out)
    }

    pub fn get_text_range(
        &self,
        start_page: usize,
        end_page: usize,
    ) -> Result<Vec<(usize, String)>> {
        self.validate_page(start_page)?;
        self.validate_page(end_page)?;
        let mut results = Vec::new();
        for page in start_page..=end_page {
            match self.get_page_text(page) {
                Ok(text) => results.push((page, text)),
                Err(err) => log::warn!("get_text_range: page {} failed: {}", page, err),
            }
        }
        Ok(results)
    }

    pub fn get_all_text(&self) -> Result<Vec<(usize, String)>> {
        let count = self.page_count()?;
        if count == 0 {
            return Ok(Vec::new());
        }
        self.get_text_range(1, count)
    }

    pub fn page_has_text_layer(&self, page_number: usize) -> Result<bool> {
        let operations = self.get_page_content(page_number)?;
        Ok(operations
            .iter()
            .any(|operation| operation.operator == "Tj" || operation.operator == "TJ"))
    }

    /// Find all image XObjects on a single page.
    pub fn find_page_images(&self, page_number: usize) -> Result<Vec<ImageReference>> {
        self.validate_page(page_number)?;
        let opts = ImageLocateOptions::default();
        ImageLocator::find_page_images(self, page_number, &opts)
    }

    /// Find all image XObjects in the entire document.
    pub fn find_all_images(&self, options: &ImageLocateOptions) -> Result<Vec<ImageReference>> {
        ImageLocator::find_all_images(self, options)
    }

    /// Decode a single image from its ImageReference.
    ///
    /// Inline images (BI/ID/EI) are decoded from the pixel bytes captured on the
    /// reference; XObject images are decoded from their PDF object.
    pub fn decode_image(&self, image: &ImageReference) -> Result<RawImage> {
        // TODO: parallel-decode multi-image pages (decode is currently serial per call).
        if image.is_inline {
            return self.decode_inline_image(image);
        }
        ImageDecoder::decode(image, self.document().reader())
    }

    /// Decode an inline image from the raw data captured during location.
    pub fn decode_inline_image(&self, image: &ImageReference) -> Result<RawImage> {
        let data = image.inline_data.as_ref().ok_or_else(|| {
            OxideError::UnsupportedFeature(format!(
                "inline image '{}' has no captured pixel data",
                image.xobject_name
            ))
        })?;
        let filters: Vec<&str> = data.filters.iter().map(String::as_str).collect();
        ImageDecoder::decode_inline(
            &data.bytes,
            image.width,
            image.height,
            data.bits_per_component,
            &image.color_space,
            &filters,
            None,
        )
    }

    /// Encode a decoded RawImage to the specified format.
    pub fn encode_image(
        image: &RawImage,
        format: ImageOutputFormat,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        ImageEncoder::encode(image, &format, quality)
    }

    /// Convenience: decode + encode in one call.
    pub fn extract_image_bytes(
        &self,
        image: &ImageReference,
        format: ImageOutputFormat,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        if let Ok(Some((bytes, _ext))) =
            ImageEncoder::keep_original(image, self.document().reader(), &format)
        {
            return Ok(bytes);
        }

        let raw = self.decode_image(image)?;
        ImageEncoder::encode(&raw, &format, quality)
    }

    /// Create a PixelBuffer sized to render the given page at the given DPI.
    pub fn create_page_buffer(&self, page_number: usize, dpi: u32) -> Result<PixelBuffer> {
        self.create_page_buffer_with_mode(page_number, dpi, RenderMode::Compat)
    }

    /// Create a PixelBuffer sized to render the given page with an explicit render mode.
    pub fn create_page_buffer_with_mode(
        &self,
        page_number: usize,
        dpi: u32,
        render_mode: RenderMode,
    ) -> Result<PixelBuffer> {
        let viewport = self.page_viewport(page_number, dpi)?;
        Ok(PixelBuffer::new_filled_with_mode(
            viewport.width_px,
            viewport.height_px,
            WHITE,
            render_mode,
        ))
    }

    /// Create a Viewport for the given page at the given DPI.
    ///
    /// Rejects a page whose final pixel count (post-DPI, post-rotation) would
    /// exceed [`max_render_pixels`] BEFORE any buffer is allocated, so a hostile
    /// PDF declaring a giant `/MediaBox` (e.g. `[0 0 200000 200000]`) returns a
    /// clean [`OxideError::ResourceLimit`] instead of attempting a multi-hundred-
    /// gigabyte allocation that aborts the process.
    pub fn page_viewport(&self, page_number: usize, dpi: u32) -> Result<Viewport> {
        self.validate_page(page_number)?;
        let page = self.get_page(page_number)?;
        let viewport = Viewport::new_rotated(
            effective_page_box(&page),
            dpi,
            page_rotation_u32(page.rotate),
        );
        let pixels = viewport.width_px as u64 * viewport.height_px as u64;
        let cap = max_render_pixels();
        if pixels > cap {
            return Err(OxideError::ResourceLimit(format!(
                "page {} would render {} pixels ({}x{}) at {} DPI, exceeding the limit of {} \
                 pixels; lower the DPI or the page is abusively large",
                page_number, pixels, viewport.width_px, viewport.height_px, dpi, cap
            )));
        }
        Ok(viewport)
    }

    /// Render a page to a PixelBuffer at the given DPI.
    pub fn render_page(&self, page_number: usize, dpi: u32) -> Result<PixelBuffer> {
        PageRenderer::render_page(self, page_number, dpi)
    }

    /// Render a page with an explicit render mode.
    ///
    /// [`RenderMode::Compat`] is byte-for-byte the default Poppler-compatible
    /// path used by [`render_page`](Self::render_page). [`RenderMode::HighQuality`]
    /// keeps the same geometry/AA coverage but composites RGB in linear light.
    pub fn render_page_with_mode(
        &self,
        page_number: usize,
        dpi: u32,
        render_mode: RenderMode,
    ) -> Result<PixelBuffer> {
        PageRenderer::render_page_with_mode(self, page_number, dpi, render_mode)
    }

    /// Render a page with a cancellation token threaded into the hot loops.
    ///
    /// The token is polled periodically while executing the page content
    /// stream (and any nested Form XObjects / tiling patterns). When the token
    /// is cancelled — e.g. by a server request-timeout timer — rendering bails
    /// out early with [`OxideError::Cancelled`] instead of running to
    /// completion, freeing the worker thread promptly.
    pub fn render_page_cancellable(
        &self,
        page_number: usize,
        dpi: u32,
        cancel: &crate::cancel::CancelToken,
    ) -> Result<PixelBuffer> {
        PageRenderer::render_page_cancellable(self, page_number, dpi, cancel)
    }

    /// Render a page with cancellation and an explicit render mode.
    pub fn render_page_cancellable_with_mode(
        &self,
        page_number: usize,
        dpi: u32,
        cancel: &crate::cancel::CancelToken,
        render_mode: RenderMode,
    ) -> Result<PixelBuffer> {
        PageRenderer::render_page_cancellable_with_mode(self, page_number, dpi, cancel, render_mode)
    }

    /// Verify every digital signature field in the document (the `verify-sig`
    /// tool — `pdfsig`-equivalent). See [`crate::signature`] for the precise
    /// scope (cryptographic validity + coverage + cert details; no trust-chain
    /// or revocation checking).
    pub fn verify_signatures(&self) -> Result<Vec<crate::signature::SignatureReport>> {
        crate::signature::verify_signatures(&self.doc)
    }

    /// Apply an RSA/SHA-256 detached CMS digital signature as an incremental
    /// update, preserving the original file bytes as an exact prefix.
    pub fn sign(
        &self,
        signer: &crate::signature::PdfSigner,
        options: &crate::signature::SignatureOptions,
    ) -> Result<Vec<u8>> {
        crate::signature::sign_document(&self.doc, signer, options)
    }

    /// Export the given 1-based pages to a single self-contained HTML or XML
    /// document (the `to-html` tool — `pdftohtml`-equivalent). See
    /// [`crate::html`] for the modes (complex / simple / xml).
    pub fn export_html(
        &self,
        pages: &[usize],
        options: &crate::html::HtmlOptions,
    ) -> Result<String> {
        for &p in pages {
            self.validate_page(p)?;
        }
        crate::html::HtmlExporter::export(self, pages, options)
    }

    /// Render a page to an SVG document (`pdftocairo -svg`-equivalent).
    ///
    /// Pages using only path/text/solid-fill/clip operations become true
    /// scalable vector SVG (text emitted as glyph outlines); pages using
    /// images, shadings, patterns, Form XObjects, or soft masks fall back to a
    /// single embedded rasterized page image (see [`crate::render::svg`]). The
    /// returned [`crate::render::SvgPage`] reports which path was taken.
    pub fn render_page_svg(&self, page_number: usize, dpi: u32) -> Result<crate::render::SvgPage> {
        crate::render::render_page_svg(self, page_number, dpi)
    }

    /// Render a single page to a PostScript page body (the building block of the
    /// `render --format ps` / `pdftops` equivalent). See
    /// [`crate::render::postscript`]. Pages using only path/text/solid-fill/clip
    /// operations become true vector PostScript (text as glyph outlines); pages
    /// using images, shadings, patterns, Form XObjects, or soft masks fall back
    /// to a single embedded rasterised page image.
    pub fn render_page_ps(&self, page_number: usize, dpi: u32) -> Result<crate::render::PsPage> {
        crate::render::render_page_ps(self, page_number, dpi)
    }

    /// Render the given 1-based pages to a complete, DSC-conformant multi-page
    /// PostScript document (`%!PS-Adobe-3.0`). The `is_rasterized` count is the
    /// number of pages that took the rasterize-embed fallback.
    pub fn render_document_ps(&self, pages: &[usize], dpi: u32) -> Result<(String, usize)> {
        let mut ps_pages = Vec::with_capacity(pages.len());
        let mut rasterized = 0usize;
        for &p in pages {
            let page = self.render_page_ps(p, dpi)?;
            if page.is_rasterized {
                rasterized += 1;
            }
            ps_pages.push(page);
        }
        Ok((crate::render::assemble_ps_document(&ps_pages), rasterized))
    }

    /// Render a single page to a conforming EPS document (`%!PS-Adobe-3.0
    /// EPSF-3.0`) with a precise `%%BoundingBox` and no `showpage`/
    /// `setpagedevice` (the `render --format eps` / `pdftops -eps` /
    /// `pdftocairo -eps` equivalent). Returns `(eps, is_rasterized)`.
    pub fn render_page_eps(&self, page_number: usize, dpi: u32) -> Result<(String, bool)> {
        let page = self.render_page_ps(page_number, dpi)?;
        let rasterized = page.is_rasterized;
        Ok((crate::render::assemble_eps_document(&page), rasterized))
    }

    /// Render a page and encode it as PNG using fast compression.
    pub fn render_page_png_fast(&self, page_number: usize, dpi: u32) -> Result<Vec<u8>> {
        // NOTE: line width 0 renders as 1px (PDF hairline spec). Verified in tests.
        let buf = self.render_page(page_number, dpi)?;
        ImageEncoder::encode_png_fast(&buf.to_raw_image())
    }

    /// Render a page with an explicit render mode and encode it as PNG.
    pub fn render_page_png_fast_with_mode(
        &self,
        page_number: usize,
        dpi: u32,
        render_mode: RenderMode,
    ) -> Result<Vec<u8>> {
        let buf = self.render_page_with_mode(page_number, dpi, render_mode)?;
        ImageEncoder::encode_png_fast(&buf.to_raw_image())
    }

    /// Build a new PDF containing exactly the given 1-based pages, in the
    /// order given (duplicates and arbitrary ordering are honoured). Underlies
    /// the `extract-pages` tool. Output is unencrypted (see [`crate::writer`]).
    pub fn extract_pages(&self, page_indices: &[usize]) -> Result<Vec<u8>> {
        for &p in page_indices {
            self.validate_page(p)?;
        }
        crate::writer::build_subset(&self.doc, page_indices)
    }

    /// Build a single-page PDF for the given 1-based page. Underlies the
    /// `split` tool, which calls this once per page.
    pub fn extract_single_page(&self, page_number: usize) -> Result<Vec<u8>> {
        self.validate_page(page_number)?;
        crate::writer::build_subset(&self.doc, &[page_number])
    }

    /// Gather document metadata and structural facts (the `info` tool —
    /// `pdfinfo`-equivalent). Works on encrypted documents (they are decrypted
    /// on open).
    pub fn document_info(&self) -> Result<crate::info::DocumentInfo> {
        crate::info::DocumentInfo::gather(&self.doc)
    }

    /// Enumerate every distinct font used in the document (the `fonts` tool —
    /// `pdffonts`-equivalent), walking all resource scopes and deduping by
    /// object id.
    pub fn list_fonts(&self) -> Result<Vec<crate::fonts_report::FontInfo>> {
        crate::fonts_report::list_fonts(&self.doc)
    }

    /// Enumerate every embedded file attachment (the `detach` tool —
    /// `pdfdetach`-equivalent), from both the name tree and file-attachment
    /// annotations, deduped by embedded-file stream object id.
    pub fn list_attachments(&self) -> Result<Vec<crate::attachments::Attachment>> {
        crate::attachments::list_attachments(&self.doc)
    }

    /// Extract the (filter-decoded) bytes of an embedded file attachment.
    pub fn extract_attachment(
        &self,
        attachment: &crate::attachments::Attachment,
    ) -> Result<Vec<u8>> {
        crate::attachments::extract_attachment(&self.doc, attachment)
    }

    fn validate_page(&self, page_number: usize) -> Result<()> {
        if page_number == 0 {
            return Err(OxideError::MalformedPdf(
                "page_number is 1-indexed; 0 is invalid".to_string(),
            ));
        }
        let count = self.doc.get_pages()?.len();
        if page_number > count {
            return Err(OxideError::MalformedPdf(format!(
                "page {} out of range (document has {} pages)",
                page_number, count
            )));
        }
        Ok(())
    }
}

/// Default ceiling on the pixel count of a single rendered page (width * height
/// after DPI and rotation). 100 megapixels admits normal high-DPI pages while
/// keeping the 4-byte-per-pixel buffer around 400 MB before renderer overhead.
/// The cap exists to turn a hostile giant `/MediaBox` into a clean error rather
/// than a process abort from a failed multi-hundred-gigabyte allocation.
pub const DEFAULT_MAX_RENDER_PIXELS: u64 = 100_000_000;

/// The active per-page render pixel cap. Overridable at runtime via the
/// `OXIDE_MAX_RENDER_PIXELS` environment variable (a positive integer); falls
/// back to [`DEFAULT_MAX_RENDER_PIXELS`] when unset, empty, zero, or unparsable.
/// Keeping this an env-var keeps the engine API free of a config object while
/// still letting the CLI/server/benchmark tune the bound.
pub fn max_render_pixels() -> u64 {
    std::env::var("OXIDE_MAX_RENDER_PIXELS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_RENDER_PIXELS)
}

fn page_rotation_u32(rotation: i32) -> u32 {
    rotation.rem_euclid(360) as u32
}

/// Parse an annotation `/Rect` `[x0 y0 x1 y1]` (resolving indirect refs and
/// normalizing so `x0<=x1, y0<=y1`). `None` if it is not a 4-number array.
fn rect_from_obj(
    obj: Option<&crate::object::PdfObject>,
    reader: &PdfReader,
) -> Option<[f64; 4]> {
    let resolved = reader.resolve(obj?.clone()).ok()?;
    let arr = resolved.as_array()?;
    if arr.len() != 4 {
        return None;
    }
    let mut v = [0.0f64; 4];
    for (i, item) in arr.iter().enumerate() {
        let n = reader.resolve(item.clone()).ok()?;
        v[i] = n.as_number()?;
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

/// Extract the URI from a `/Link` annotation: either its `/A << /S /URI /URI … >>`
/// action or a direct `/URI`. Returns `None` for GoTo/internal links.
fn link_uri(adict: &crate::object::PdfDictionary, reader: &PdfReader) -> Option<String> {
    use crate::info::decode_pdf_text_string;
    // Direct /URI on the annotation (older style).
    if let Some(crate::object::PdfObject::String(bytes)) = adict.get("URI") {
        return Some(decode_pdf_text_string(bytes));
    }
    // /A action dictionary (may be indirect).
    let action = reader.resolve(adict.get("A")?.clone()).ok()?;
    let act_dict = action.as_dict()?;
    if act_dict.get_name("S") != Some("URI") {
        return None;
    }
    match reader.resolve(act_dict.get("URI")?.clone()).ok()? {
        crate::object::PdfObject::String(bytes) => Some(decode_pdf_text_string(&bytes)),
        _ => None,
    }
}

fn effective_page_box(page: &PdfPage) -> [f64; 4] {
    intersect_boxes(page.media_box, page.crop_box).unwrap_or(page.media_box)
}

fn intersect_boxes(media: [f64; 4], crop: [f64; 4]) -> Option<[f64; 4]> {
    let result = [
        media[0].max(crop[0]),
        media[1].max(crop[1]),
        media[2].min(crop[2]),
        media[3].min(crop[3]),
    ];

    if result[0] >= result[2] || result[1] >= result[3] {
        None
    } else {
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(media_box: [f64; 4], crop_box: [f64; 4]) -> PdfPage {
        PdfPage {
            page_number: 1,
            object_number: 1,
            generation_number: 0,
            media_box,
            crop_box,
            rotate: 0,
            resources: PdfDictionary::empty(),
            contents: Vec::new(),
        }
    }

    #[test]
    fn intersect_boxes_clips_cropbox_to_mediabox() {
        let media = [0.0, 0.0, 612.0, 792.0];
        let crop = [-10.0, -10.0, 100.0, 100.0];

        assert_eq!(intersect_boxes(media, crop), Some([0.0, 0.0, 100.0, 100.0]));
    }

    #[test]
    fn intersect_boxes_identical_cropbox_returns_mediabox() {
        let media = [0.0, 0.0, 612.0, 792.0];

        assert_eq!(intersect_boxes(media, media), Some(media));
    }

    #[test]
    fn effective_page_box_ignores_invalid_cropbox() {
        let page = page([0.0, 0.0, 200.0, 200.0], [250.0, 250.0, 300.0, 300.0]);

        assert_eq!(effective_page_box(&page), [0.0, 0.0, 200.0, 200.0]);
    }
}
