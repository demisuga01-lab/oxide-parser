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
use crate::render::{PageRenderer, PixelBuffer, Viewport, WHITE};
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

impl PageResources {
    pub fn from_dict(resources: &PdfDictionary, reader: &PdfReader) -> Self {
        let mut page_resources = PageResources::default();

        if let Some(font_dict) = resources.get_dict("Font") {
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

        if let Some(xobject_dict) = resources.get_dict("XObject") {
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

        if let Some(color_space_dict) = resources.get_dict("ColorSpace") {
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

        if let Some(ext_g_state_dict) = resources.get_dict("ExtGState") {
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

        if let Some(pattern_dict) = resources.get_dict("Pattern") {
            for (name, value) in pattern_dict.entries() {
                page_resources.patterns.insert(name.clone(), value.clone());
            }
        }

        if let Some(shading_dict) = resources.get_dict("Shading") {
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
    pub fn decode_image(&self, image: &ImageReference) -> Result<RawImage> {
        // TODO(perf): parallel decode for multi-image pages (added in HTTP endpoint step).
        ImageDecoder::decode(image, self.document().reader())
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
        let viewport = self.page_viewport(page_number, dpi)?;
        Ok(PixelBuffer::new_filled(
            viewport.width_px,
            viewport.height_px,
            WHITE,
        ))
    }

    /// Create a Viewport for the given page at the given DPI.
    pub fn page_viewport(&self, page_number: usize, dpi: u32) -> Result<Viewport> {
        self.validate_page(page_number)?;
        let page = self.get_page(page_number)?;
        Ok(Viewport::new_rotated(
            effective_page_box(&page),
            dpi,
            page_rotation_u32(page.rotate),
        ))
    }

    /// Render a page to a PixelBuffer at the given DPI.
    pub fn render_page(&self, page_number: usize, dpi: u32) -> Result<PixelBuffer> {
        PageRenderer::render_page(self, page_number, dpi)
    }

    /// Render a page and encode it as PNG using fast compression.
    pub fn render_page_png_fast(&self, page_number: usize, dpi: u32) -> Result<Vec<u8>> {
        // NOTE: line width 0 renders as 1px (PDF hairline spec). Verified in tests.
        let buf = self.render_page(page_number, dpi)?;
        ImageEncoder::encode_png_fast(&buf.to_raw_image())
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

fn page_rotation_u32(rotation: i32) -> u32 {
    rotation.rem_euclid(360) as u32
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
