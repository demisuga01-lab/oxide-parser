//! Font analysis reporting (`pdffonts`-equivalent).
//!
//! Walks every resource scope a font can hide in — page resources, Form
//! XObject resources, tiling-pattern resources, and Type3 font resources —
//! collects each font's indirect reference, dedupes by object id, and reports
//! the columns `pdffonts` prints: name, type, encoding, embedded, subset,
//! ToUnicode, and object id.
//!
//! This is aggregation + attribute reporting, not new font parsing: it reads
//! fields straight off the already-parsed font dictionaries and reuses the
//! resolver's subtype mapping.

use std::collections::HashSet;

use serde::Serialize;

use crate::document::PdfDocument;
use crate::error::Result;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

/// Maximum resource-scope recursion depth (Form XObjects nesting Form
/// XObjects, etc.). Real documents nest only a few levels; this bounds a
/// pathological/cyclic resource graph.
const MAX_SCOPE_DEPTH: usize = 32;

/// One distinct font used in the document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FontInfo {
    /// `/BaseFont` (e.g. "ABCDEF+Helvetica"), or the Type3 font's name, or
    /// `[none]` when absent.
    pub name: String,
    /// pdffonts-style type label: "Type 1", "TrueType", "Type 0", "Type 3",
    /// "CID TrueType", "CID Type 0", etc.
    pub font_type: String,
    /// Encoding label: "WinAnsi", "MacRoman", "Identity-H", "Custom",
    /// "Builtin", etc.
    pub encoding: String,
    /// Whether a font program is embedded (FontFile/FontFile2/FontFile3).
    pub embedded: bool,
    /// Whether the font is a subset (6-uppercase-letter "+"-prefixed BaseFont).
    pub subset: bool,
    /// Whether the font carries a `/ToUnicode` CMap.
    pub to_unicode: bool,
    /// Object number of the font dictionary.
    pub object_number: u32,
    /// Generation number of the font dictionary.
    pub generation: u16,
}

/// Enumerate every distinct font used across the document.
pub fn list_fonts(doc: &PdfDocument) -> Result<Vec<FontInfo>> {
    let reader = doc.reader();
    let pages = doc.get_pages()?;

    // Collected font references, in first-seen order, deduped by object id.
    let mut seen: HashSet<(u32, u16)> = HashSet::new();
    let mut font_refs: Vec<(u32, u16)> = Vec::new();
    // Visited resource scopes (by the object number of the resource-carrying
    // object) to stop cycles in the XObject/pattern graph.
    let mut visited_scopes: HashSet<u32> = HashSet::new();

    for page in &pages {
        // page.resources is the resolved /Resources dictionary (inheritance
        // already applied). Walk it and everything it reaches.
        walk_resources(
            &page.resources,
            reader,
            &mut seen,
            &mut font_refs,
            &mut visited_scopes,
            0,
        );

        // Annotation appearance streams are Form XObjects with their own
        // /Resources, reachable only via the page's /Annots array (NOT via the
        // page /Resources). Form fields in particular keep their fonts here, so
        // skipping /Annots silently drops every font used only in widget
        // appearances — a common pdffonts-disagreement bug.
        walk_page_annotations(
            page.object_number,
            page.generation_number,
            reader,
            &mut seen,
            &mut font_refs,
            &mut visited_scopes,
        );
    }

    // Resolve and describe each distinct font.
    let mut fonts = Vec::with_capacity(font_refs.len());
    for (num, gen) in font_refs {
        if let Ok(PdfObject::Dictionary(font_dict)) = reader.get_and_resolve(num, gen) {
            fonts.push(describe_font(&font_dict, reader, num, gen));
        }
    }
    Ok(fonts)
}

/// Walk a `/Resources` dictionary: collect every `/Font` reference, then
/// recurse into Form XObjects' and tiling patterns' own resources.
fn walk_resources(
    resources: &PdfDictionary,
    reader: &PdfReader,
    seen: &mut HashSet<(u32, u16)>,
    font_refs: &mut Vec<(u32, u16)>,
    visited_scopes: &mut HashSet<u32>,
    depth: usize,
) {
    if depth > MAX_SCOPE_DEPTH {
        return;
    }

    // /Font: each entry references a font dictionary. Collect its object id,
    // and (for Type3) recurse into the font's own /Resources used by CharProcs.
    if let Some(font_dict) = resolve_dict(resources.get("Font"), reader) {
        for (_name, value) in font_dict.entries() {
            // Resolve the font dictionary itself (whether the entry is an
            // indirect ref — the common case — or an inline dict).
            let (resolved_font, object_id) = match value {
                PdfObject::Reference { number, generation } => {
                    match reader.get_and_resolve(*number, *generation) {
                        Ok(PdfObject::Dictionary(fd)) => (Some(fd), Some((*number, *generation))),
                        _ => (None, Some((*number, *generation))),
                    }
                }
                PdfObject::Dictionary(fd) => (Some(fd.clone()), None),
                _ => (None, None),
            };

            // Record the font reference (id-keyed listing; inline fonts have no
            // object id to report and are skipped from the listing).
            if let Some((num, gen)) = object_id {
                if seen.insert((num, gen)) {
                    font_refs.push((num, gen));
                }
            }

            // Type3 fonts carry their own /Resources; recurse into them so
            // fonts used only inside Type3 glyph procedures aren't missed.
            if let Some(fd) = resolved_font {
                if fd.get_name("Subtype") == Some("Type3") {
                    let scope_ok = match object_id {
                        Some((num, _)) => visited_scopes.insert(num),
                        None => true,
                    };
                    if scope_ok {
                        if let Some(t3_res) = resolve_dict(fd.get("Resources"), reader) {
                            walk_resources(
                                &t3_res, reader, seen, font_refs, visited_scopes, depth + 1,
                            );
                        }
                    }
                }
            }
        }
    }

    // /XObject: recurse into Form XObjects' /Resources.
    if let Some(xobj_dict) = resolve_dict(resources.get("XObject"), reader) {
        for (_name, value) in xobj_dict.entries() {
            let Some((num, gen)) = value.as_reference() else {
                continue;
            };
            if !visited_scopes.insert(num) {
                continue;
            }
            if let Ok(PdfObject::Stream { dict, .. }) = reader.get_and_resolve(num, gen) {
                if dict.get_name("Subtype") == Some("Form") {
                    if let Some(form_res) = resolve_dict(dict.get("Resources"), reader) {
                        walk_resources(
                            &form_res, reader, seen, font_refs, visited_scopes, depth + 1,
                        );
                    }
                }
            }
        }
    }

    // /Pattern: tiling patterns (PatternType 1) are streams with /Resources.
    if let Some(pat_dict) = resolve_dict(resources.get("Pattern"), reader) {
        for (_name, value) in pat_dict.entries() {
            let Some((num, gen)) = value.as_reference() else {
                continue;
            };
            if !visited_scopes.insert(num) {
                continue;
            }
            if let Ok(obj) = reader.get_and_resolve(num, gen) {
                let pat_resources = match &obj {
                    PdfObject::Stream { dict, .. } => resolve_dict(dict.get("Resources"), reader),
                    PdfObject::Dictionary(dict) => resolve_dict(dict.get("Resources"), reader),
                    _ => None,
                };
                if let Some(pr) = pat_resources {
                    walk_resources(&pr, reader, seen, font_refs, visited_scopes, depth + 1);
                }
            }
        }
    }
}

/// Walk a page's `/Annots`: each annotation's appearance streams (`/AP /N`,
/// `/D`, `/R`) are Form XObjects whose `/Resources` may carry fonts used
/// nowhere else. The `/AP` entry for a state may be a single stream or a
/// sub-dictionary mapping appearance-state names to streams.
fn walk_page_annotations(
    page_num: u32,
    page_gen: u16,
    reader: &PdfReader,
    seen: &mut HashSet<(u32, u16)>,
    font_refs: &mut Vec<(u32, u16)>,
    visited_scopes: &mut HashSet<u32>,
) {
    let Ok(PdfObject::Dictionary(page_dict)) = reader.get_and_resolve(page_num, page_gen) else {
        return;
    };
    let annots = match page_dict.get("Annots") {
        Some(obj) => match reader.resolve(obj.clone()) {
            Ok(PdfObject::Array(items)) => items,
            _ => return,
        },
        None => return,
    };

    for annot in &annots {
        let Ok(PdfObject::Dictionary(annot_dict)) = reader.resolve(annot.clone()) else {
            continue;
        };
        let Some(ap) = annot_dict.get("AP") else {
            continue;
        };
        let Ok(PdfObject::Dictionary(ap_dict)) = reader.resolve(ap.clone()) else {
            continue;
        };
        // Each appearance type (/N normal, /D down, /R rollover).
        for (_state, value) in ap_dict.entries() {
            collect_appearance_resources(value, reader, seen, font_refs, visited_scopes);
        }
    }
}

/// Resolve an appearance entry — either a Form XObject stream directly, or a
/// sub-dictionary of appearance-state → stream — and walk its resources.
fn collect_appearance_resources(
    value: &PdfObject,
    reader: &PdfReader,
    seen: &mut HashSet<(u32, u16)>,
    font_refs: &mut Vec<(u32, u16)>,
    visited_scopes: &mut HashSet<u32>,
) {
    // Track the scope object id for cycle protection when it's an indirect ref.
    let scope_id = value.as_reference().map(|(n, _)| n);
    if let Some(num) = scope_id {
        if !visited_scopes.insert(num) {
            return;
        }
    }
    let Ok(resolved) = reader.resolve(value.clone()) else {
        return;
    };
    match resolved {
        PdfObject::Stream { dict, .. } => {
            if let Some(res) = resolve_dict(dict.get("Resources"), reader) {
                walk_resources(&res, reader, seen, font_refs, visited_scopes, 1);
            }
        }
        PdfObject::Dictionary(state_dict) => {
            // Sub-dictionary: appearance-state name → stream.
            for (_name, stream_ref) in state_dict.entries() {
                if let Ok(PdfObject::Stream { dict, .. }) = reader.resolve(stream_ref.clone()) {
                    if let Some(res) = resolve_dict(dict.get("Resources"), reader) {
                        walk_resources(&res, reader, seen, font_refs, visited_scopes, 1);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Build a [`FontInfo`] from a resolved font dictionary.
fn describe_font(
    font_dict: &PdfDictionary,
    reader: &PdfReader,
    num: u32,
    gen: u16,
) -> FontInfo {
    let base_font = font_dict.get_name("BaseFont").unwrap_or("");
    let name = if base_font.is_empty() {
        font_dict
            .get_name("Name")
            .map(str::to_string)
            .unwrap_or_else(|| "[none]".to_string())
    } else {
        base_font.to_string()
    };

    let subtype = font_dict.get_name("Subtype").unwrap_or("");
    let descendant = descendant_font(font_dict, reader);
    let font_type = font_type_label(subtype, descendant.as_ref());

    let encoding = encoding_label(font_dict, reader, subtype);

    // Embedded: a font program present in the relevant FontDescriptor.
    let embedded = is_embedded(font_dict, descendant.as_ref(), reader);

    // Subset: BaseFont starts with "XXXXXX+" (6 uppercase letters + '+').
    let subset = is_subset_name(base_font);

    let to_unicode = font_dict.contains_key("ToUnicode");

    FontInfo {
        name,
        font_type,
        encoding,
        embedded,
        subset,
        to_unicode,
        object_number: num,
        generation: gen,
    }
}

/// Map a PDF font subtype (+ descendant CIDFont subtype for Type0) to a
/// pdffonts-style type label.
fn font_type_label(subtype: &str, descendant: Option<&PdfDictionary>) -> String {
    match subtype {
        "Type1" => "Type 1".to_string(),
        "MMType1" => "Type 1 (Multiple Master)".to_string(),
        "TrueType" => "TrueType".to_string(),
        "Type3" => "Type 3".to_string(),
        "Type0" => match descendant.and_then(|d| d.get_name("Subtype")) {
            Some("CIDFontType0") => "CID Type 0".to_string(),
            Some("CIDFontType2") => "CID TrueType".to_string(),
            _ => "Type 0".to_string(),
        },
        "CIDFontType0" => "CID Type 0".to_string(),
        "CIDFontType2" => "CID TrueType".to_string(),
        "" => "[unknown]".to_string(),
        other => other.to_string(),
    }
}

/// Normalize an `/Encoding` name to the short label `pdffonts` prints
/// ("WinAnsi", "MacRoman", "Standard", "PDFDoc", or the name verbatim for CMap
/// names like "Identity-H").
fn normalize_encoding_name(name: &str) -> String {
    match name {
        "WinAnsiEncoding" => "WinAnsi".to_string(),
        "MacRomanEncoding" => "MacRoman".to_string(),
        "StandardEncoding" => "Standard".to_string(),
        "PDFDocEncoding" => "PDFDoc".to_string(),
        "MacExpertEncoding" => "MacExpert".to_string(),
        other => other.to_string(),
    }
}

/// Encoding label, matched to `pdffonts`' output.
///
/// - A name `/Encoding` is normalized (WinAnsiEncoding → "WinAnsi", etc.);
///   CMap names (Identity-H, …) pass through verbatim.
/// - An encoding dictionary with `/Differences` is "Custom"; otherwise its
///   normalized `/BaseEncoding`.
/// - An embedded CMap stream (Type0) is "Custom".
/// - When `/Encoding` is **absent**, Poppler reports the *implicit* encoding a
///   simple non-symbolic font would use rather than "Builtin": WinAnsi/Standard.
///   We mirror that for simple fonts (using the FontDescriptor symbolic flag),
///   and report "Identity" / "Builtin" for composite fonts that genuinely have
///   no encoding name.
fn encoding_label(font_dict: &PdfDictionary, reader: &PdfReader, subtype: &str) -> String {
    let Some(encoding) = font_dict.get("Encoding") else {
        return implicit_encoding_label(font_dict, reader, subtype);
    };
    let resolved = reader.resolve(encoding.clone()).unwrap_or_else(|_| encoding.clone());
    match resolved {
        PdfObject::Name(name) => normalize_encoding_name(&name),
        PdfObject::Dictionary(dict) => {
            // Simple-font encoding dict: /Differences ⇒ Custom; else the base.
            if dict.contains_key("Differences") {
                "Custom".to_string()
            } else {
                match dict.get_name("BaseEncoding") {
                    Some(base) => normalize_encoding_name(base),
                    None => implicit_encoding_label(font_dict, reader, subtype),
                }
            }
        }
        // Embedded CMap (Type0) ⇒ a custom mapping.
        PdfObject::Stream { .. } => "Custom".to_string(),
        _ => implicit_encoding_label(font_dict, reader, subtype),
    }
}

/// The implicit encoding label for a font with no explicit `/Encoding`,
/// following pdffonts' behaviour: composite fonts report "Identity"; simple
/// fonts report their standard encoding unless flagged symbolic (then
/// "Builtin"). Matches Poppler closely so the cross-check agrees.
fn implicit_encoding_label(
    font_dict: &PdfDictionary,
    reader: &PdfReader,
    subtype: &str,
) -> String {
    if subtype == "Type0" {
        return "Identity".to_string();
    }
    // Simple font. pdffonts treats embedded TrueType subset fonts as using a
    // standard (WinAnsi) encoding even when the symbolic flag is set, so we key
    // on subtype: TrueType ⇒ WinAnsi, Type1/others ⇒ Standard. A genuinely
    // symbolic Type1 with a built-in encoding and no FontFile is "Builtin".
    let symbolic = font_descriptor_symbolic(font_dict, reader);
    match subtype {
        "TrueType" => "WinAnsi".to_string(),
        "Type1" | "MMType1" => {
            if symbolic {
                "Builtin".to_string()
            } else {
                "Standard".to_string()
            }
        }
        _ => "Builtin".to_string(),
    }
}

/// True if the font's FontDescriptor `/Flags` has the Symbolic bit (bit 3, i.e.
/// value 4) set. Returns false when there is no descriptor.
fn font_descriptor_symbolic(font_dict: &PdfDictionary, reader: &PdfReader) -> bool {
    let Some(descriptor) = resolve_dict(font_dict.get("FontDescriptor"), reader) else {
        return false;
    };
    let flags = descriptor.get_integer("Flags").unwrap_or(0);
    (flags & 0b100) != 0
}

/// Whether the font has an embedded program. For simple fonts this is a
/// FontFile/FontFile2/FontFile3 in the font's own /FontDescriptor; for Type0
/// it is in the descendant CIDFont's /FontDescriptor.
fn is_embedded(
    font_dict: &PdfDictionary,
    descendant: Option<&PdfDictionary>,
    reader: &PdfReader,
) -> bool {
    if descriptor_has_fontfile(font_dict, reader) {
        return true;
    }
    if let Some(desc) = descendant {
        if descriptor_has_fontfile(desc, reader) {
            return true;
        }
    }
    false
}

fn descriptor_has_fontfile(font_dict: &PdfDictionary, reader: &PdfReader) -> bool {
    let Some(descriptor) = resolve_dict(font_dict.get("FontDescriptor"), reader) else {
        return false;
    };
    descriptor.contains_key("FontFile")
        || descriptor.contains_key("FontFile2")
        || descriptor.contains_key("FontFile3")
}

/// Resolve a Type0 font's first descendant CIDFont dictionary.
fn descendant_font(font_dict: &PdfDictionary, reader: &PdfReader) -> Option<PdfDictionary> {
    if font_dict.get_name("Subtype") != Some("Type0") {
        return None;
    }
    let descendants = match font_dict.get("DescendantFonts")? {
        PdfObject::Array(items) => items.clone(),
        obj @ PdfObject::Reference { .. } => match reader.resolve(obj.clone()).ok()? {
            PdfObject::Array(items) => items,
            _ => return None,
        },
        _ => return None,
    };
    match descendants.first()? {
        PdfObject::Dictionary(dict) => Some(dict.clone()),
        obj @ PdfObject::Reference { .. } => match reader.resolve(obj.clone()).ok()? {
            PdfObject::Dictionary(dict) => Some(dict),
            _ => None,
        },
        _ => None,
    }
}

/// True if `name` carries the 6-uppercase-letter subset prefix `XXXXXX+`.
pub fn is_subset_name(name: &str) -> bool {
    let Some(plus) = name.find('+') else {
        return false;
    };
    let prefix = &name[..plus];
    prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase())
}

/// Resolve an optional object to a dictionary (following one indirect ref).
fn resolve_dict(obj: Option<&PdfObject>, reader: &PdfReader) -> Option<PdfDictionary> {
    match obj? {
        PdfObject::Dictionary(dict) => Some(dict.clone()),
        r @ PdfObject::Reference { .. } => match reader.resolve(r.clone()).ok()? {
            PdfObject::Dictionary(dict) => Some(dict),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_prefix_detection() {
        assert!(is_subset_name("ABCDEF+Helvetica"));
        assert!(is_subset_name("WXYZAB+Arial-Bold"));
        assert!(!is_subset_name("Helvetica"));
        assert!(!is_subset_name("ABC+Helvetica")); // too short
        assert!(!is_subset_name("abcdef+Helvetica")); // lowercase
        assert!(!is_subset_name("ABCDE1+Helvetica")); // digit
    }

    #[test]
    fn type_label_mapping() {
        assert_eq!(font_type_label("Type1", None), "Type 1");
        assert_eq!(font_type_label("TrueType", None), "TrueType");
        assert_eq!(font_type_label("Type3", None), "Type 3");
        assert_eq!(font_type_label("Type0", None), "Type 0");

        let mut cid0 = PdfDictionary::empty();
        cid0.insert("Subtype", PdfObject::Name("CIDFontType0".to_string()));
        assert_eq!(font_type_label("Type0", Some(&cid0)), "CID Type 0");

        let mut cid2 = PdfDictionary::empty();
        cid2.insert("Subtype", PdfObject::Name("CIDFontType2".to_string()));
        assert_eq!(font_type_label("Type0", Some(&cid2)), "CID TrueType");
    }
}
