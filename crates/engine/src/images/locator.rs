use std::collections::{HashMap, HashSet};

use crate::content::Operand;
use crate::engine::ContentEngine;
use crate::error::{OxideError, Result};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

#[derive(Debug, Clone)]
pub struct ImageLocateOptions {
    /// Pages to search. None = all pages.
    pub pages: Option<Vec<usize>>,

    /// Minimum image width in pixels.
    pub min_width: u32,

    /// Minimum image height in pixels.
    pub min_height: u32,

    /// Include ImageMask images.
    pub include_masks: bool,

    /// Include soft-mask images.
    pub include_soft_masks: bool,

    /// Include inline images.
    pub include_inline: bool,
}

impl Default for ImageLocateOptions {
    fn default() -> Self {
        Self {
            pages: None,
            min_width: 1,
            min_height: 1,
            include_masks: false,
            include_soft_masks: false,
            include_inline: true,
        }
    }
}

/// Raw data captured for an inline image (BI/ID/EI) so it can be decoded and
/// exported without re-walking the page content stream.
#[derive(Debug, Clone)]
pub struct InlineImageData {
    /// The raw bytes between `ID` and `EI`, with any preceding (non-image)
    /// filters still applied — i.e. exactly the inline image stream payload.
    pub bytes: Vec<u8>,

    /// Bits per component, resolved from `/BPC` or `/BitsPerComponent` (or 8).
    pub bits_per_component: u8,

    /// The filter chain (`/F` or `/Filter`), in application order. Both
    /// abbreviated (`Fl`, `AHx`, `CCF`, ...) and full names are preserved
    /// as-is; the decode path understands both.
    pub filters: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImageReference {
    /// Page number (1-indexed) where this image appears.
    pub page_number: usize,

    /// XObject resource name, or an auto-generated inline image name.
    pub xobject_name: String,

    /// PDF object number. 0 for inline images.
    pub object_number: u32,

    /// PDF generation number. 0 for inline images.
    pub generation_number: u16,

    /// Image width in pixels.
    pub width: u32,

    /// Image height in pixels.
    pub height: u32,

    /// Bits per component.
    pub bits_per_component: u8,

    /// Color space name.
    pub color_space: String,

    /// Filter(s) applied to the image stream.
    pub filter: Vec<String>,

    /// True if this is an inline image.
    pub is_inline: bool,

    /// True if /ImageMask is true.
    pub is_mask: bool,

    /// True if this image is referenced as /SMask by another image.
    pub is_smask: bool,

    /// For inline images, the captured raw data needed to decode/export them.
    /// `None` for XObject images (which are decoded via their object number).
    pub inline_data: Option<InlineImageData>,
}

impl ImageReference {
    /// Approximate uncompressed size in bytes.
    pub fn uncompressed_bytes(&self) -> usize {
        let channels = match self.color_space.as_str() {
            "DeviceGray" | "G" => 1usize,
            "DeviceCMYK" | "CMYK" => 4usize,
            _ => 3usize,
        };
        let bpp = (self.bits_per_component as usize * channels).div_ceil(8);
        self.width as usize * self.height as usize * bpp
    }
}

/// Scalar/array parameters parsed from an inline image's BI...ID dictionary.
#[derive(Debug, Default)]
struct InlineParams {
    nums: HashMap<String, i64>,
    strs: HashMap<String, String>,
    bools: HashMap<String, bool>,
    name_arrays: HashMap<String, Vec<String>>,
}

pub struct ImageLocator;

impl ImageLocator {
    /// Find all images in the document matching the given options.
    pub fn find_all_images(
        engine: &ContentEngine,
        options: &ImageLocateOptions,
    ) -> Result<Vec<ImageReference>> {
        let total_pages = engine.page_count()?;
        let pages: Vec<usize> = match &options.pages {
            Some(list) => list.clone(),
            None => (1..=total_pages).collect(),
        };
        let reader = engine.document().reader();

        let mut all_refs = Vec::new();
        for page_num in pages {
            let page_refs = Self::find_page_images(engine, page_num, options)?;
            all_refs.extend(page_refs);
        }

        Self::mark_soft_masks_with_reader(&mut all_refs, reader);

        all_refs.retain(|r| {
            if r.is_mask && !options.include_masks {
                return false;
            }
            if r.is_smask && !options.include_soft_masks {
                return false;
            }
            if r.width < options.min_width {
                return false;
            }
            if r.height < options.min_height {
                return false;
            }
            true
        });

        Ok(all_refs)
    }

    /// Find images on a single page.
    pub fn find_page_images(
        engine: &ContentEngine,
        page_number: usize,
        options: &ImageLocateOptions,
    ) -> Result<Vec<ImageReference>> {
        let resources = engine.get_page_resources(page_number)?;
        let reader = engine.document().reader();
        let mut refs = Vec::new();
        let mut visited = HashSet::new();

        Self::walk_xobject_dict(
            &resources.xobjects,
            page_number,
            reader,
            &mut visited,
            options,
            &mut refs,
        );

        if options.include_inline {
            let inline_refs = Self::find_inline_images(engine, page_number)?;
            refs.extend(inline_refs);
        }

        Ok(refs)
    }

    /// Return unique images by object_number. Inline images are always unique.
    pub fn deduplicate(refs: Vec<ImageReference>) -> Vec<ImageReference> {
        let mut seen: HashSet<u32> = HashSet::new();
        let mut result = Vec::new();

        for r in refs {
            if r.object_number == 0 || seen.insert(r.object_number) {
                result.push(r);
            }
        }

        result
    }

    /// Get the raw encoded stream bytes for an image reference.
    pub fn get_stream_bytes(image: &ImageReference, reader: &PdfReader) -> Result<Option<Vec<u8>>> {
        if image.is_inline || image.object_number == 0 {
            return Ok(None);
        }

        match reader.get_object(image.object_number, image.generation_number)? {
            PdfObject::Stream { raw, .. } => Ok(Some(raw)),
            _ => Err(OxideError::MalformedPdf(format!(
                "image object {} is not a stream",
                image.object_number
            ))),
        }
    }

    fn image_ref_from_dict(
        page_number: usize,
        xobject_name: String,
        object_number: u32,
        generation_number: u16,
        dict: &PdfDictionary,
    ) -> ImageReference {
        let width = dict
            .get_integer("Width")
            .or_else(|| dict.get_integer("W"))
            .unwrap_or(0)
            .max(0) as u32;
        let height = dict
            .get_integer("Height")
            .or_else(|| dict.get_integer("H"))
            .unwrap_or(0)
            .max(0) as u32;
        let bpc = dict
            .get_integer("BitsPerComponent")
            .or_else(|| dict.get_integer("BPC"))
            .unwrap_or(8)
            .clamp(0, 16) as u8;
        let is_mask = Self::get_image_mask(dict);

        ImageReference {
            page_number,
            xobject_name,
            object_number,
            generation_number,
            width,
            height,
            bits_per_component: if is_mask { 1 } else { bpc },
            color_space: Self::extract_color_space(dict),
            filter: Self::extract_filters(dict),
            is_inline: false,
            is_mask,
            is_smask: false,
            inline_data: None,
        }
    }

    fn extract_color_space(dict: &PdfDictionary) -> String {
        let value = dict.get("ColorSpace").or_else(|| dict.get("CS"));
        match value {
            Some(PdfObject::Name(name)) => Self::expand_color_space_name(name),
            Some(PdfObject::Array(arr)) => arr
                .first()
                .and_then(PdfObject::as_name)
                .unwrap_or("Unknown")
                .to_string(),
            _ => "Unknown".to_string(),
        }
    }

    fn extract_filters(dict: &PdfDictionary) -> Vec<String> {
        let value = dict.get("Filter").or_else(|| dict.get("F"));
        match value {
            Some(PdfObject::Name(name)) => vec![name.clone()],
            Some(PdfObject::Array(arr)) => arr
                .iter()
                .filter_map(PdfObject::as_name)
                .map(str::to_string)
                .collect(),
            _ => vec![],
        }
    }

    fn get_image_mask(dict: &PdfDictionary) -> bool {
        match dict.get("ImageMask").or_else(|| dict.get("IM")) {
            Some(PdfObject::Boolean(value)) => *value,
            Some(PdfObject::Name(name)) => name.eq_ignore_ascii_case("true"),
            _ => false,
        }
    }

    fn expand_color_space_name(name: &str) -> String {
        match name {
            "G" => "DeviceGray".to_string(),
            "RGB" => "DeviceRGB".to_string(),
            "CMYK" => "DeviceCMYK".to_string(),
            other => other.to_string(),
        }
    }

    fn find_inline_images(
        engine: &ContentEngine,
        page_number: usize,
    ) -> Result<Vec<ImageReference>> {
        let ops = engine.get_page_content(page_number)?;
        let mut inline_refs = Vec::new();
        let mut inline_index = 0usize;
        let mut i = 0usize;

        while i < ops.len() {
            let op = &ops[i];
            if op.operator == "ID" {
                let params = Self::parse_inline_image_params(&op.operands);
                let pixel_bytes =
                    if i + 1 < ops.len() && ops[i + 1].operator == "inline_image_data" {
                        ops[i + 1].string_bytes(0).map(|bytes| bytes.to_vec())
                    } else {
                        None
                    };

                let xobject_name = format!("inline_{}_{}", page_number, inline_index);
                let width = params
                    .nums
                    .get("W")
                    .or_else(|| params.nums.get("Width"))
                    .copied()
                    .unwrap_or(0)
                    .max(0) as u32;
                let height = params
                    .nums
                    .get("H")
                    .or_else(|| params.nums.get("Height"))
                    .copied()
                    .unwrap_or(0)
                    .max(0) as u32;
                let bpc = params
                    .nums
                    .get("BPC")
                    .or_else(|| params.nums.get("BitsPerComponent"))
                    .copied()
                    .unwrap_or(8)
                    .clamp(0, 16) as u8;
                let cs_key = params
                    .strs
                    .get("CS")
                    .or_else(|| params.strs.get("ColorSpace"))
                    .map(String::as_str)
                    .unwrap_or("DeviceRGB");
                let filter = Self::inline_filters(&params);
                let is_mask = params
                    .bools
                    .get("IM")
                    .or_else(|| params.bools.get("ImageMask"))
                    .copied()
                    .unwrap_or(false);
                let effective_bpc = if is_mask { 1 } else { bpc };

                let inline_data = pixel_bytes.map(|bytes| InlineImageData {
                    bytes,
                    bits_per_component: effective_bpc,
                    filters: filter.clone(),
                });

                inline_refs.push(ImageReference {
                    page_number,
                    xobject_name,
                    object_number: 0,
                    generation_number: 0,
                    width,
                    height,
                    bits_per_component: effective_bpc,
                    color_space: Self::expand_color_space_name(cs_key),
                    filter,
                    is_inline: true,
                    is_mask,
                    is_smask: false,
                    inline_data,
                });
                inline_index += 1;
            }
            i += 1;
        }

        Ok(inline_refs)
    }

    fn parse_inline_image_params(operands: &[Operand]) -> InlineParams {
        let mut params = InlineParams::default();

        let mut iter = operands.iter().peekable();
        while let Some(op) = iter.next() {
            if let Some(key) = op.as_name() {
                if let Some(next) = iter.peek() {
                    match *next {
                        Operand::Integer(n) => {
                            params.nums.insert(key.to_string(), *n);
                            iter.next();
                        }
                        Operand::Real(r) => {
                            params.nums.insert(key.to_string(), *r as i64);
                            iter.next();
                        }
                        Operand::Name(s) => {
                            params.strs.insert(key.to_string(), s.clone());
                            iter.next();
                        }
                        Operand::Boolean(b) => {
                            params.bools.insert(key.to_string(), *b);
                            iter.next();
                        }
                        Operand::Array(items) => {
                            // Filter chains may be expressed as a name array,
                            // e.g. /F [/AHx /Fl]. Capture every name in order.
                            let names: Vec<String> = items
                                .iter()
                                .filter_map(Operand::as_name)
                                .map(str::to_string)
                                .collect();
                            params.name_arrays.insert(key.to_string(), names);
                            iter.next();
                        }
                        _ => {}
                    }
                }
            }
        }

        params
    }

    /// Resolve the inline image filter chain from `/F` or `/Filter`, accepting
    /// either a single name or a name array. Returns names verbatim (the decode
    /// path understands both abbreviated and full forms).
    fn inline_filters(params: &InlineParams) -> Vec<String> {
        if let Some(arr) = params
            .name_arrays
            .get("F")
            .or_else(|| params.name_arrays.get("Filter"))
        {
            return arr.clone();
        }
        match params
            .strs
            .get("F")
            .or_else(|| params.strs.get("Filter"))
            .map(String::as_str)
        {
            Some(name) if !name.is_empty() => vec![name.to_string()],
            _ => vec![],
        }
    }

    fn walk_xobject_dict(
        xobjects: &HashMap<String, (u32, u16)>,
        page_number: usize,
        reader: &PdfReader,
        visited: &mut HashSet<u32>,
        options: &ImageLocateOptions,
        results: &mut Vec<ImageReference>,
    ) {
        let _ = options;
        for (name, &(obj_num, gen_num)) in xobjects {
            if !visited.insert(obj_num) {
                continue;
            }

            let obj = match reader.get_object(obj_num, gen_num) {
                Ok(obj) => obj,
                Err(err) => {
                    log::warn!(
                        "XObject '{}' (obj {}) failed to resolve: {}",
                        name,
                        obj_num,
                        err
                    );
                    continue;
                }
            };

            let dict = match &obj {
                PdfObject::Stream { dict, .. } => dict.clone(),
                _ => {
                    log::debug!("XObject '{}' is not a stream, skipping", name);
                    continue;
                }
            };

            match dict.get_name("Subtype") {
                Some("Image") => {
                    results.push(Self::image_ref_from_dict(
                        page_number,
                        name.clone(),
                        obj_num,
                        gen_num,
                        &dict,
                    ));
                }
                Some("Form") => {
                    log::debug!("XObject '{}' is a Form; walking nested images", name);
                    if let Some(res_dict) = Self::resolve_resource_dict(&dict, reader) {
                        if let Some(xobj_dict) = res_dict.get_dict("XObject") {
                            let nested: HashMap<String, (u32, u16)> = xobj_dict
                                .entries()
                                .filter_map(|(key, value)| {
                                    value
                                        .as_reference()
                                        .map(|reference| (key.clone(), reference))
                                })
                                .collect();
                            Self::walk_xobject_dict(
                                &nested,
                                page_number,
                                reader,
                                visited,
                                options,
                                results,
                            );
                        }
                    }
                }
                Some(other) => {
                    log::debug!(
                        "XObject '{}' has unsupported subtype '{}'; skipping",
                        name,
                        other
                    );
                }
                None => {
                    log::debug!("XObject '{}' has no /Subtype; skipping", name);
                }
            }
        }
    }

    fn resolve_resource_dict(dict: &PdfDictionary, reader: &PdfReader) -> Option<PdfDictionary> {
        match dict.get("Resources") {
            Some(PdfObject::Dictionary(resources)) => Some(resources.clone()),
            Some(PdfObject::Reference { number, generation }) => reader
                .get_object(*number, *generation)
                .ok()
                .and_then(|obj| obj.as_dict().cloned()),
            _ => None,
        }
    }

    fn mark_soft_masks_with_reader(refs: &mut [ImageReference], reader: &PdfReader) {
        // TODO: collect /SMask refs during the primary walk to avoid this second pass.
        let mut smask_objs: HashSet<u32> = HashSet::new();

        for image_ref in refs.iter() {
            if image_ref.is_inline || image_ref.object_number == 0 {
                continue;
            }
            if let Ok(PdfObject::Stream { dict, .. }) =
                reader.get_object(image_ref.object_number, image_ref.generation_number)
            {
                if let Some(PdfObject::Reference { number, .. }) = dict.get("SMask") {
                    smask_objs.insert(*number);
                }
            }
        }

        for image_ref in refs.iter_mut() {
            if smask_objs.contains(&image_ref.object_number) {
                image_ref.is_smask = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_ref(dict: &PdfDictionary) -> ImageReference {
        ImageLocator::image_ref_from_dict(1, "Im1".to_string(), 5, 0, dict)
    }

    #[test]
    fn extract_color_space_handles_names() {
        let mut d = PdfDictionary::empty();
        d.insert("ColorSpace", PdfObject::Name("DeviceRGB".to_string()));
        let img = image_ref(&d);
        assert_eq!(img.color_space, "DeviceRGB");
    }

    #[test]
    fn extract_filters_handles_single_name() {
        let mut d = PdfDictionary::empty();
        d.insert("Filter", PdfObject::Name("DCTDecode".to_string()));
        let img = image_ref(&d);
        assert_eq!(img.filter, vec!["DCTDecode"]);
    }

    #[test]
    fn extract_filters_handles_array() {
        let mut d = PdfDictionary::empty();
        d.insert(
            "Filter",
            PdfObject::Array(vec![
                PdfObject::Name("FlateDecode".to_string()),
                PdfObject::Name("DCTDecode".to_string()),
            ]),
        );
        let img = image_ref(&d);
        assert_eq!(img.filter, vec!["FlateDecode", "DCTDecode"]);
    }

    #[test]
    fn is_mask_detected_correctly() {
        let mut d = PdfDictionary::empty();
        d.insert("ImageMask", PdfObject::Boolean(true));
        d.insert("Width", PdfObject::Integer(10));
        d.insert("Height", PdfObject::Integer(10));
        let img = image_ref(&d);
        assert!(img.is_mask);
        assert_eq!(img.bits_per_component, 1);
    }

    #[test]
    fn image_mask_name_true_is_detected() {
        let mut d = PdfDictionary::empty();
        d.insert("IM", PdfObject::Name("true".to_string()));
        let img = image_ref(&d);
        assert!(img.is_mask);
        assert_eq!(img.bits_per_component, 1);
    }

    #[test]
    fn abbreviated_color_space_names_expanded() {
        let mut d = PdfDictionary::empty();
        d.insert("ColorSpace", PdfObject::Name("G".to_string()));
        let img = image_ref(&d);
        assert_eq!(img.color_space, "DeviceGray");

        let mut d2 = PdfDictionary::empty();
        d2.insert("ColorSpace", PdfObject::Name("RGB".to_string()));
        let img2 = ImageLocator::image_ref_from_dict(1, "Im2".to_string(), 6, 0, &d2);
        assert_eq!(img2.color_space, "DeviceRGB");
    }

    #[test]
    fn uncompressed_bytes_calculation() {
        let mut r = ImageReference {
            page_number: 1,
            xobject_name: "Im1".to_string(),
            object_number: 5,
            generation_number: 0,
            width: 100,
            height: 100,
            bits_per_component: 8,
            color_space: "DeviceRGB".to_string(),
            filter: vec![],
            is_inline: false,
            is_mask: false,
            is_smask: false,
            inline_data: None,
        };
        assert_eq!(r.uncompressed_bytes(), 30000);

        r.color_space = "DeviceGray".to_string();
        assert_eq!(r.uncompressed_bytes(), 10000);
    }

    #[test]
    fn uncompressed_bytes_for_cmyk() {
        let r = ImageReference {
            page_number: 1,
            xobject_name: "Im1".to_string(),
            object_number: 5,
            generation_number: 0,
            width: 50,
            height: 50,
            bits_per_component: 8,
            color_space: "DeviceCMYK".to_string(),
            filter: vec!["DCTDecode".to_string()],
            is_inline: false,
            is_mask: false,
            is_smask: false,
            inline_data: None,
        };
        assert_eq!(r.uncompressed_bytes(), 10000);
    }

    #[test]
    fn parse_inline_image_params_handles_mixed_types() {
        let operands = vec![
            Operand::Name("W".to_string()),
            Operand::Integer(200),
            Operand::Name("H".to_string()),
            Operand::Integer(150),
            Operand::Name("CS".to_string()),
            Operand::Name("RGB".to_string()),
            Operand::Name("BPC".to_string()),
            Operand::Integer(8),
            Operand::Name("IM".to_string()),
            Operand::Boolean(false),
        ];
        let params = ImageLocator::parse_inline_image_params(&operands);
        assert_eq!(params.nums.get("W"), Some(&200i64));
        assert_eq!(params.nums.get("H"), Some(&150i64));
        assert_eq!(params.strs.get("CS"), Some(&"RGB".to_string()));
        assert_eq!(params.bools.get("IM"), Some(&false));
    }

    #[test]
    fn inline_filters_handles_single_abbreviated_name() {
        let operands = vec![
            Operand::Name("F".to_string()),
            Operand::Name("Fl".to_string()),
        ];
        let params = ImageLocator::parse_inline_image_params(&operands);
        assert_eq!(ImageLocator::inline_filters(&params), vec!["Fl".to_string()]);
    }

    #[test]
    fn inline_filters_handles_full_filter_key() {
        let operands = vec![
            Operand::Name("Filter".to_string()),
            Operand::Name("FlateDecode".to_string()),
        ];
        let params = ImageLocator::parse_inline_image_params(&operands);
        assert_eq!(
            ImageLocator::inline_filters(&params),
            vec!["FlateDecode".to_string()]
        );
    }

    #[test]
    fn inline_filters_handles_name_array_in_order() {
        // /F [/AHx /Fl]
        let operands = vec![
            Operand::Name("F".to_string()),
            Operand::Array(vec![
                Operand::Name("AHx".to_string()),
                Operand::Name("Fl".to_string()),
            ]),
        ];
        let params = ImageLocator::parse_inline_image_params(&operands);
        assert_eq!(
            ImageLocator::inline_filters(&params),
            vec!["AHx".to_string(), "Fl".to_string()]
        );
    }

    #[test]
    fn inline_filters_empty_when_absent() {
        let operands = vec![
            Operand::Name("W".to_string()),
            Operand::Integer(2),
        ];
        let params = ImageLocator::parse_inline_image_params(&operands);
        assert!(ImageLocator::inline_filters(&params).is_empty());
    }

    #[test]
    fn extract_filters_handles_missing_filter_gracefully() {
        let d = PdfDictionary::empty();
        let img = image_ref(&d);
        assert!(img.filter.is_empty());
    }

    #[test]
    fn default_options_do_not_include_masks() {
        let opts = ImageLocateOptions::default();
        assert!(!opts.include_masks);
        assert!(!opts.include_soft_masks);
        assert!(opts.include_inline);
        assert_eq!(opts.min_width, 1);
        assert_eq!(opts.min_height, 1);
    }

    #[test]
    fn image_ref_from_dict_clamps_negative_dimensions() {
        let mut d = PdfDictionary::empty();
        d.insert("Width", PdfObject::Integer(-10));
        d.insert("Height", PdfObject::Integer(-1));
        d.insert("BitsPerComponent", PdfObject::Integer(20));
        let img = image_ref(&d);
        assert_eq!(img.width, 0);
        assert_eq!(img.height, 0);
        assert_eq!(img.bits_per_component, 16);
    }

    #[test]
    fn deduplicate_keeps_first_occurrence() {
        let refs = vec![
            ImageReference {
                page_number: 1,
                object_number: 5,
                xobject_name: "Im1".to_string(),
                generation_number: 0,
                width: 100,
                height: 100,
                bits_per_component: 8,
                color_space: "DeviceRGB".to_string(),
                filter: vec![],
                is_inline: false,
                is_mask: false,
                is_smask: false,
                inline_data: None,
            },
            ImageReference {
                page_number: 2,
                object_number: 5,
                xobject_name: "Im1".to_string(),
                generation_number: 0,
                width: 100,
                height: 100,
                bits_per_component: 8,
                color_space: "DeviceRGB".to_string(),
                filter: vec![],
                is_inline: false,
                is_mask: false,
                is_smask: false,
                inline_data: None,
            },
            ImageReference {
                page_number: 1,
                object_number: 7,
                xobject_name: "Im2".to_string(),
                generation_number: 0,
                width: 50,
                height: 50,
                bits_per_component: 8,
                color_space: "DeviceGray".to_string(),
                filter: vec![],
                is_inline: false,
                is_mask: false,
                is_smask: false,
                inline_data: None,
            },
        ];
        let deduped = ImageLocator::deduplicate(refs);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].page_number, 1);
        assert_eq!(deduped[1].object_number, 7);
    }
}
