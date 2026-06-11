use std::collections::HashSet;
use std::path::Path;

use crate::error::{OxideError, Result};
use crate::filters::{decode_stream_lossless, StreamDecodeStatus};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

const DEFAULT_MEDIA_BOX: [f64; 4] = [0.0, 0.0, 612.0, 792.0];

pub struct PdfDocument {
    reader: PdfReader,
}

#[derive(Debug, Clone)]
pub struct PdfPage {
    pub page_number: usize,
    pub object_number: u32,
    pub generation_number: u16,
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    pub rotate: i32,
    pub resources: PdfDictionary,
    pub contents: Vec<(u32, u16)>,
}

#[derive(Clone, Debug, Default)]
struct InheritedAttrs {
    media_box: Option<[f64; 4]>,
    crop_box: Option<[f64; 4]>,
    rotate: Option<i32>,
    resources: Option<PdfDictionary>,
}

impl PdfDocument {
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            reader: PdfReader::from_path(path)?,
        })
    }

    pub fn open_bytes(data: Vec<u8>) -> Result<Self> {
        Ok(Self {
            reader: PdfReader::from_bytes(data)?,
        })
    }

    /// Open a PDF from a file path, supplying a password for encrypted PDFs.
    pub fn open_path_with_password(path: impl AsRef<Path>, password: &[u8]) -> Result<Self> {
        Ok(Self {
            reader: PdfReader::from_path_with_password(path, password)?,
        })
    }

    /// Open a PDF from bytes, supplying a password for encrypted PDFs.
    pub fn open_bytes_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self> {
        Ok(Self {
            reader: PdfReader::from_bytes_with_password(data, password)?,
        })
    }

    pub fn reader(&self) -> &PdfReader {
        &self.reader
    }

    pub fn get_catalog(&self) -> Result<PdfDictionary> {
        let (number, generation) = self
            .reader
            .root_reference()
            .ok_or_else(|| OxideError::MalformedPdf("trailer is missing /Root".to_string()))?;
        let root = self.reader.get_and_resolve(number, generation)?;
        let catalog = root.as_dict().cloned().ok_or_else(|| {
            OxideError::MalformedPdf("/Root did not resolve to a dictionary".to_string())
        })?;
        match catalog.get_name("Type") {
            Some("Catalog") => {}
            Some(other) => log::warn!("catalog /Type is /{other}, expected /Catalog"),
            None => log::warn!("catalog dictionary is missing /Type"),
        }
        Ok(catalog)
    }

    pub fn get_pages(&self) -> Result<Vec<PdfPage>> {
        let catalog = self.get_catalog()?;
        let pages_ref = catalog.get_reference("Pages").ok_or_else(|| {
            OxideError::MalformedPdf("catalog is missing /Pages reference".to_string())
        })?;
        let root_pages_obj = self.reader.get_and_resolve(pages_ref.0, pages_ref.1)?;
        let root_pages = root_pages_obj.as_dict().cloned().ok_or_else(|| {
            OxideError::MalformedPdf("/Pages did not resolve to a dictionary".to_string())
        })?;
        let expected_count = root_pages.get_integer("Count");

        let mut visited = HashSet::new();
        visited.insert(pages_ref.0);
        let mut pages = Vec::new();
        self.walk_page_tree(
            pages_ref,
            &root_pages,
            InheritedAttrs::default(),
            &mut visited,
            &mut pages,
        )?;

        if let Some(expected_count) = expected_count {
            if expected_count >= 0 && expected_count as usize != pages.len() {
                log::warn!(
                    "root /Pages /Count is {}, but traversal collected {} pages",
                    expected_count,
                    pages.len()
                );
            }
        }

        Ok(pages)
    }

    pub fn get_page_content_bytes(&self, page_number: usize) -> Result<Vec<u8>> {
        if page_number == 0 {
            return Err(OxideError::MalformedPdf(
                "page numbers are 1-indexed".to_string(),
            ));
        }
        let pages = self.get_pages()?;
        let page = pages.get(page_number - 1).ok_or_else(|| {
            OxideError::MalformedPdf(format!("page {page_number} is out of range"))
        })?;
        let mut out = Vec::new();

        let mut wrote_stream = false;
        for (number, generation) in page.contents.iter().copied() {
            let object = match self.reader.get_object(number, generation) {
                Ok(object) => object,
                Err(OxideError::MissingObject { .. }) => {
                    log::warn!(
                        "page {}: content stream {} {} missing, skipping",
                        page_number,
                        number,
                        generation
                    );
                    continue;
                }
                Err(err) => {
                    log::warn!(
                        "page {}: content stream {} {} could not be read: {}",
                        page_number,
                        number,
                        generation,
                        err
                    );
                    continue;
                }
            };
            if object.as_stream().is_none() {
                log::warn!(
                    "page {}: content object {} {} is not a stream, skipping",
                    page_number,
                    number,
                    generation
                );
                continue;
            }
            if wrote_stream {
                out.push(b'\n');
            }
            let decoded = match decode_stream_lossless(&object, &self.reader) {
                Ok(decoded) => decoded,
                Err(err) => {
                    log::warn!(
                        "page {}: content stream {} {} could not be decoded: {}",
                        page_number,
                        number,
                        generation,
                        err
                    );
                    continue;
                }
            };
            if let StreamDecodeStatus::StoppedAtImageFilter(filter) = &decoded.status {
                log::warn!(
                    "page content stream {number} {generation} stopped at image filter {filter}"
                );
            }
            out.extend_from_slice(&decoded.data);
            wrote_stream = true;
        }

        Ok(out)
    }

    fn walk_page_tree(
        &self,
        object_ref: (u32, u16),
        dict: &PdfDictionary,
        inherited: InheritedAttrs,
        visited: &mut HashSet<u32>,
        pages: &mut Vec<PdfPage>,
    ) -> Result<()> {
        let inherited = apply_inherited_attrs(dict, inherited, Some(&self.reader))?;
        let has_kids = dict.get("Kids").is_some();
        let node_type = dict.get_name("Type");

        if has_kids {
            if let Some("Page") = node_type {
                log::warn!(
                    "page tree object {} {} has /Type /Page but also /Kids; treating as node",
                    object_ref.0,
                    object_ref.1
                );
            }
            let kids = dict.get_array("Kids").ok_or_else(|| {
                OxideError::MalformedPdf(format!(
                    "page tree node {} {} has non-array /Kids",
                    object_ref.0, object_ref.1
                ))
            })?;
            for kid in kids {
                let Some(kid_ref) = kid.as_reference() else {
                    log::warn!(
                        "page tree node {} {} contains a non-reference /Kids entry",
                        object_ref.0,
                        object_ref.1
                    );
                    continue;
                };
                if !visited.insert(kid_ref.0) {
                    log::warn!(
                        "skipping cyclic page-tree reference {} {}",
                        kid_ref.0,
                        kid_ref.1
                    );
                    continue;
                }
                let kid_object = self.reader.get_and_resolve(kid_ref.0, kid_ref.1)?;
                let kid_dict = kid_object.as_dict().ok_or_else(|| {
                    OxideError::MalformedPdf(format!(
                        "page-tree object {} {} did not resolve to a dictionary",
                        kid_ref.0, kid_ref.1
                    ))
                })?;
                self.walk_page_tree(kid_ref, kid_dict, inherited.clone(), visited, pages)?;
            }
        } else {
            if let Some("Pages") = node_type {
                log::warn!(
                    "page tree object {} {} has /Type /Pages but no /Kids; treating as leaf",
                    object_ref.0,
                    object_ref.1
                );
            }
            let page = build_page_from_dict(
                object_ref.0,
                object_ref.1,
                pages.len() + 1,
                dict,
                inherited,
                Some(&self.reader),
            )?;
            pages.push(page);
        }

        Ok(())
    }
}

fn apply_inherited_attrs(
    dict: &PdfDictionary,
    mut inherited: InheritedAttrs,
    reader: Option<&PdfReader>,
) -> Result<InheritedAttrs> {
    if let Some(media_box) = dict.get("MediaBox") {
        inherited.media_box = Some(parse_box(media_box, reader, "MediaBox")?);
    }
    if let Some(crop_box) = dict.get("CropBox") {
        inherited.crop_box = Some(parse_box(crop_box, reader, "CropBox")?);
    }
    if let Some(rotate) = dict.get("Rotate") {
        inherited.rotate = Some(parse_rotate_value(rotate, reader)?);
    }
    if let Some(resources) = dict.get("Resources") {
        inherited.resources = Some(parse_resources(resources, reader)?);
    }
    Ok(inherited)
}

fn build_page_from_dict(
    object_number: u32,
    generation_number: u16,
    page_number: usize,
    dict: &PdfDictionary,
    inherited: InheritedAttrs,
    reader: Option<&PdfReader>,
) -> Result<PdfPage> {
    let inherited = apply_inherited_attrs(dict, inherited, reader)?;
    let media_box = match inherited.media_box {
        Some(media_box) => media_box,
        None => {
            log::warn!(
                "page object {} {} is missing inherited /MediaBox; using US Letter",
                object_number,
                generation_number
            );
            DEFAULT_MEDIA_BOX
        }
    };
    let crop_box = inherited.crop_box.unwrap_or(media_box);
    let rotate = normalize_rotate(inherited.rotate.unwrap_or(0));
    let resources = match inherited.resources {
        Some(resources) => resources,
        None => {
            log::warn!(
                "page object {} {} is missing inherited /Resources; using empty dictionary",
                object_number,
                generation_number
            );
            PdfDictionary::empty()
        }
    };
    let contents = parse_contents(dict);

    Ok(PdfPage {
        page_number,
        object_number,
        generation_number,
        media_box,
        crop_box,
        rotate,
        resources,
        contents,
    })
}

fn parse_box(object: &PdfObject, reader: Option<&PdfReader>, key: &str) -> Result<[f64; 4]> {
    let object = resolve_if_possible(object, reader)?;
    let array = object
        .as_array()
        .ok_or_else(|| OxideError::MalformedPdf(format!("/{key} must resolve to an array")))?;
    if array.len() != 4 {
        return Err(OxideError::MalformedPdf(format!(
            "/{key} must contain four numbers"
        )));
    }

    let mut values = [0.0; 4];
    for (idx, item) in array.iter().enumerate() {
        let item = resolve_if_possible(item, reader)?;
        values[idx] = item.as_number().ok_or_else(|| {
            OxideError::MalformedPdf(format!("/{key} entry {} is not a number", idx + 1))
        })?;
    }
    Ok(values)
}

fn parse_rotate_value(object: &PdfObject, reader: Option<&PdfReader>) -> Result<i32> {
    let object = resolve_if_possible(object, reader)?;
    let value = object
        .as_integer()
        .ok_or_else(|| OxideError::MalformedPdf("/Rotate must be an integer".to_string()))?;
    i32::try_from(value)
        .map_err(|_| OxideError::MalformedPdf("/Rotate does not fit in i32".to_string()))
}

fn parse_resources(object: &PdfObject, reader: Option<&PdfReader>) -> Result<PdfDictionary> {
    let object = resolve_if_possible(object, reader)?;
    object.as_dict().cloned().ok_or_else(|| {
        OxideError::MalformedPdf("/Resources must resolve to a dictionary".to_string())
    })
}

fn parse_contents(dict: &PdfDictionary) -> Vec<(u32, u16)> {
    let Some(contents) = dict.get("Contents") else {
        return Vec::new();
    };
    match contents {
        PdfObject::Reference { number, generation } => vec![(*number, *generation)],
        PdfObject::Array(items) => {
            let mut refs = Vec::new();
            for item in items {
                if let Some(reference) = item.as_reference() {
                    refs.push(reference);
                } else {
                    log::warn!("/Contents array contains a non-reference entry");
                }
            }
            refs
        }
        PdfObject::Null => Vec::new(),
        other => {
            log::warn!(
                "/Contents must be a reference or array of references, got {}",
                other.variant_name()
            );
            Vec::new()
        }
    }
}

fn resolve_if_possible(object: &PdfObject, reader: Option<&PdfReader>) -> Result<PdfObject> {
    match reader {
        Some(reader) => reader.resolve(object.clone()),
        None => Ok(object.clone()),
    }
}

fn normalize_rotate(value: i32) -> i32 {
    let normalized = value.rem_euclid(360);
    match normalized {
        0 | 90 | 180 | 270 => normalized,
        other => {
            log::warn!("invalid /Rotate value {value} normalised to {other}; using 0");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn box_obj(values: [i64; 4]) -> PdfObject {
        PdfObject::Array(values.into_iter().map(PdfObject::Integer).collect())
    }

    #[test]
    fn page_inherits_and_overrides_attributes() {
        let parent_resources = dict(&[("Font", PdfObject::Dictionary(PdfDictionary::empty()))]);
        let parent = dict(&[
            ("MediaBox", box_obj([0, 0, 300, 400])),
            ("Resources", PdfObject::Dictionary(parent_resources.clone())),
        ]);
        let parent_attrs = apply_inherited_attrs(&parent, InheritedAttrs::default(), None).unwrap();

        let inherited_page = build_page_from_dict(
            10,
            0,
            1,
            &PdfDictionary::empty(),
            parent_attrs.clone(),
            None,
        )
        .unwrap();
        assert_eq!(inherited_page.media_box, [0.0, 0.0, 300.0, 400.0]);
        assert_eq!(inherited_page.rotate, 0);
        assert_eq!(inherited_page.resources, parent_resources);

        let page_resources = dict(&[("XObject", PdfObject::Dictionary(PdfDictionary::empty()))]);
        let page = dict(&[
            ("MediaBox", box_obj([10, 20, 500, 600])),
            ("Resources", PdfObject::Dictionary(page_resources.clone())),
        ]);
        let overridden_page = build_page_from_dict(11, 0, 2, &page, parent_attrs, None).unwrap();
        assert_eq!(overridden_page.media_box, [10.0, 20.0, 500.0, 600.0]);
        assert_eq!(overridden_page.rotate, 0);
        assert_eq!(overridden_page.resources, page_resources);
    }
}
