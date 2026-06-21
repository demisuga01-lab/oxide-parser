//! Part A — direct extraction from a PDF's `/AcroForm` form fields.
//!
//! When a PDF has real form fields the field name→value pairs are available
//! exactly, with zero heuristics — the highest-confidence KV source. Many
//! government and business forms are AcroForms.

use crate::extract::{Field, FieldSource, FieldValue, ValueHint};
use crate::extract::value::normalize;
use crate::info::decode_pdf_text_string;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

/// Maximum field-tree depth (defends against cyclic `/Kids`).
const MAX_DEPTH: usize = 32;

/// Extract every terminal AcroForm field as a [`Field`]. Returns an empty vec
/// when the document has no `/AcroForm`. Never errors on a malformed tree — a
/// bad node is skipped.
pub fn extract_acroform_fields(
    catalog: &PdfDictionary,
    reader: &PdfReader,
    widget_pages: &WidgetPageIndex,
) -> Vec<Field> {
    let Some(acroform) = catalog
        .get("AcroForm")
        .and_then(|o| reader.resolve(o.clone()).ok())
        .and_then(|o| o.as_dict().cloned())
    else {
        return Vec::new();
    };
    let Some(fields_obj) = acroform.get("Fields") else {
        return Vec::new();
    };
    let Ok(resolved) = reader.resolve(fields_obj.clone()) else {
        return Vec::new();
    };
    let Some(items) = resolved.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in items {
        walk_field(item, reader, widget_pages, &[], 0, &mut out);
    }
    out
}

/// Recursively walk the field tree. A node is *terminal* (emits a field) when it
/// has a `/FT` (field type) and no `/Kids` that are themselves fields; nodes
/// with `/Kids` are intermediate (we descend, accumulating the `/T` name path).
fn walk_field(
    node_obj: &PdfObject,
    reader: &PdfReader,
    widget_pages: &WidgetPageIndex,
    name_path: &[String],
    depth: usize,
    out: &mut Vec<Field>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(resolved) = reader.resolve(node_obj.clone()) else {
        return;
    };
    let Some(dict) = resolved.as_dict() else {
        return;
    };

    // Build this node's name path (partial /T joined with '.').
    let mut path = name_path.to_vec();
    if let Some(t) = dict.get("T").and_then(string_of) {
        path.push(t);
    }

    // Does it have child *fields*? `/Kids` whose entries are field dicts (they
    // have /T or /FT). A widget-only kid (one field with multiple widget
    // appearances) is NOT a child field — it is the same terminal field.
    let kids_owned: Vec<PdfObject> = dict
        .get("Kids")
        .and_then(|k| reader.resolve(k.clone()).ok())
        .and_then(|k| k.as_array().map(|a| a.to_vec()))
        .unwrap_or_default();
    let child_fields: Vec<PdfObject> = kids_owned
        .iter()
        .filter(|kid| kid_is_field(kid, reader))
        .cloned()
        .collect();

    if !child_fields.is_empty() {
        for kid in &child_fields {
            walk_field(kid, reader, widget_pages, &path, depth + 1, out);
        }
        return;
    }

    // Terminal field. Must have a field type to be meaningful.
    let Some(ft) = inherited_name(dict, reader, "FT") else {
        return;
    };

    if let Some(field) = build_field(dict, reader, widget_pages, &path, &ft) {
        out.push(field);
    }
}

/// A `/Kids` entry is a child *field* (vs. a pure widget) if it has its own
/// `/T` or `/FT` (a partial name / a field type). Pure widgets (multiple
/// appearances of one field) carry only `/Subtype /Widget` and `/Rect`.
fn kid_is_field(kid: &PdfObject, reader: &PdfReader) -> bool {
    let Ok(resolved) = reader.resolve(kid.clone()) else {
        return false;
    };
    let Some(d) = resolved.as_dict() else {
        return false;
    };
    d.contains_key("T") || d.contains_key("FT")
}

fn build_field(
    dict: &PdfDictionary,
    reader: &PdfReader,
    widget_pages: &WidgetPageIndex,
    name_path: &[String],
    ft: &str,
) -> Option<Field> {
    // Key: prefer the human label /TU (tooltip), fall back to the qualified /T.
    let qualified = name_path.join(".");
    let key = dict
        .get("TU")
        .and_then(string_of)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if qualified.is_empty() {
                "(unnamed field)".to_string()
            } else {
                qualified.clone()
            }
        });

    // Value /V (inheritable). Type by field kind.
    let v = inherited_object(dict, reader, "V");
    let (raw, value) = match ft {
        "Tx" | "Ch" => {
            let raw = v.as_ref().and_then(value_text).unwrap_or_default();
            let hint = ValueHint::Any;
            (raw.clone(), normalize(&raw, hint))
        }
        "Btn" => {
            // Checkbox/radio: /V is a name (the "on" state) or /Off.
            let on = v
                .as_ref()
                .and_then(|o| o.as_name().map(|n| n.to_string()))
                .or_else(|| v.as_ref().and_then(value_text));
            let checked = matches!(on.as_deref(), Some(s) if !s.is_empty() && s != "Off");
            let raw = on.clone().unwrap_or_else(|| "Off".to_string());
            (raw, FieldValue::Bool { value: checked })
        }
        _ => {
            let raw = v.as_ref().and_then(value_text).unwrap_or_default();
            (raw.clone(), FieldValue::Text { text: raw })
        }
    };

    // Geometry: the widget /Rect (this dict if it is also the widget, else its
    // first widget kid) and the page it lives on.
    let (page, bbox) = widget_geometry(dict, reader, widget_pages);

    Some(Field {
        key,
        value,
        raw,
        page,
        bbox,
        // Exact source. (On scanned forms an AcroForm is still exact — the field
        // value is real PDF data, not OCR.)
        confidence: 1.0,
        source: FieldSource::AcroForm,
    })
}

/// Find the widget rect + page for a terminal field: either the field dict is
/// itself the widget (`/Rect` present), or its first widget kid is.
fn widget_geometry(
    dict: &PdfDictionary,
    reader: &PdfReader,
    widget_pages: &WidgetPageIndex,
) -> (u32, [f64; 4]) {
    if let Some(rect) = rect_of(dict, reader) {
        let page = widget_pages.page_for_rect(&rect).unwrap_or(0);
        return (page, rect);
    }
    if let Some(kids) = dict
        .get("Kids")
        .and_then(|k| reader.resolve(k.clone()).ok())
        .and_then(|k| k.as_array().map(|a| a.to_vec()))
    {
        for kid in kids {
            if let Ok(resolved) = reader.resolve(kid.clone()) {
                if let Some(kd) = resolved.as_dict() {
                    if let Some(rect) = rect_of(kd, reader) {
                        let page = widget_pages.page_for_rect(&rect).unwrap_or(0);
                        return (page, rect);
                    }
                }
            }
        }
    }
    (0, [0.0; 4])
}

fn rect_of(dict: &PdfDictionary, reader: &PdfReader) -> Option<[f64; 4]> {
    let arr = dict
        .get("Rect")
        .and_then(|o| reader.resolve(o.clone()).ok())?;
    let arr = arr.as_array()?;
    if arr.len() != 4 {
        return None;
    }
    let mut v = [0.0f64; 4];
    for (i, item) in arr.iter().enumerate() {
        v[i] = reader.resolve(item.clone()).ok()?.as_number()?;
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn inherited_object(dict: &PdfDictionary, reader: &PdfReader, key: &str) -> Option<PdfObject> {
    let mut cur = dict.clone();
    for _ in 0..MAX_DEPTH {
        if let Some(o) = cur.get(key) {
            return reader.resolve(o.clone()).ok();
        }
        let parent = cur.get("Parent")?.clone();
        cur = reader.resolve(parent).ok()?.as_dict()?.clone();
    }
    None
}

fn inherited_name(dict: &PdfDictionary, reader: &PdfReader, key: &str) -> Option<String> {
    inherited_object(dict, reader, key).and_then(|o| o.as_name().map(|s| s.to_string()))
}

/// Decode a PDF text/name object to a Rust string (UTF-16BE/PDFDocEncoding).
fn string_of(obj: &PdfObject) -> Option<String> {
    match obj {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        PdfObject::Name(n) => Some(n.clone()),
        _ => None,
    }
}

/// The displayable text of a field value object.
fn value_text(obj: &PdfObject) -> Option<String> {
    match obj {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        PdfObject::Name(n) if n != "Off" => Some(n.clone()),
        PdfObject::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(value_text).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

/// Maps a widget `/Rect` to the page it appears on, built once by scanning each
/// page's `/Annots`. Pure geometry: a rect belongs to the page whose annot list
/// contains it (matched by equal rect).
pub struct WidgetPageIndex {
    /// (page, rect) for every widget annotation, in page order.
    widgets: Vec<(u32, [f64; 4])>,
}

impl WidgetPageIndex {
    /// Build the index from the document's pages.
    pub fn build(engine: &crate::engine::ContentEngine) -> Self {
        let mut widgets = Vec::new();
        let reader = engine.document().reader();
        if let Ok(pages) = engine.document().get_pages() {
            for (i, page) in pages.iter().enumerate() {
                let page_num = (i + 1) as u32;
                let Ok(page_obj) =
                    reader.get_and_resolve(page.object_number, page.generation_number)
                else {
                    continue;
                };
                let Some(pd) = page_obj.as_dict() else {
                    continue;
                };
                let Some(annots) = pd.get("Annots").and_then(|a| reader.resolve(a.clone()).ok())
                else {
                    continue;
                };
                let Some(items) = annots.as_array() else {
                    continue;
                };
                for it in items {
                    if let Ok(r) = reader.resolve(it.clone()) {
                        if let Some(ad) = r.as_dict() {
                            if ad.get_name("Subtype") == Some("Widget") {
                                if let Some(rect) = rect_of(ad, reader) {
                                    widgets.push((page_num, rect));
                                }
                            }
                        }
                    }
                }
            }
        }
        WidgetPageIndex { widgets }
    }

    fn page_for_rect(&self, rect: &[f64; 4]) -> Option<u32> {
        self.widgets
            .iter()
            .find(|(_, r)| rects_close(r, rect))
            .map(|(p, _)| *p)
    }
}

fn rects_close(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 0.5)
}
