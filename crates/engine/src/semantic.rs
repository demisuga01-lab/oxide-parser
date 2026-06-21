//! Tagged-PDF semantic extraction.
//!
//! Tagged PDFs expose an authored logical structure tree (`/StructTreeRoot`)
//! whose `/StructElem` nodes point to page marked-content ranges by MCID. This
//! module walks that tree in authored order, resolves `(page, MCID)` text from
//! the content streams, and emits a semantic JSON-ready tree. When no structure
//! tree is present it falls back to the geometric layout analyzer.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::analysis::layout::{LayoutBlock, LayoutConfig};
use crate::analysis::tables::Table;
use crate::engine::ContentEngine;
use crate::error::{OxideError, Result};
use crate::info::decode_pdf_text_string;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::text::{MarkedTextChunk, ReadingOrderReconstructor, TextChunk, TextCollector};

const MAX_STRUCT_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSource {
    TaggedPdf,
    GeometricFallback,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticDocument {
    pub tagged: bool,
    pub source: SemanticSource,
    pub elements: Vec<SemanticElement>,
    pub tables: Vec<Table>,
}

impl SemanticDocument {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for element in &self.elements {
            write_element_text(element, 0, &mut out);
        }
        out.trim_end().to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticElement {
    #[serde(rename = "type")]
    pub element_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    /// Bounding box of the element's own marked content in PDF user space
    /// `[x0, y0, x1, y1]` (y-up), recovered as the union of the text-chunk boxes
    /// for this element's MCIDs. `None` when the element has no resolvable
    /// marked text (e.g. a pure container, or a `Figure` whose content is a
    /// non-text XObject). Used by the document-model layer to give tagged
    /// elements geometry for ordering/linkage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcids: Vec<SemanticMcid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SemanticElement>,
}

impl SemanticElement {
    pub fn combined_text(&self) -> String {
        let mut parts = Vec::new();
        if !self.text.trim().is_empty() {
            parts.push(self.text.trim().to_string());
        }
        for child in &self.children {
            let child_text = child.combined_text();
            if !child_text.trim().is_empty() {
                parts.push(child_text);
            }
        }
        parts
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticMcid {
    pub page: usize,
    pub mcid: i64,
}

type PageRef = (u32, u16);
type McidKey = (usize, i64);
type McidTextMap = HashMap<McidKey, Vec<TextChunk>>;

pub fn extract_semantic_document(
    engine: &ContentEngine,
    pages: &[usize],
) -> Result<SemanticDocument> {
    let total = engine.page_count()?;
    let page_list: Vec<usize> = if pages.is_empty() {
        (1..=total).collect()
    } else {
        pages.to_vec()
    };
    for &page in &page_list {
        if page == 0 || page > total {
            return Err(OxideError::MalformedPdf(format!(
                "page {page} out of range (document has {total})"
            )));
        }
    }

    let catalog = engine.document().get_catalog()?;
    let Some(root_obj) = catalog.get("StructTreeRoot").cloned() else {
        return geometric_fallback(engine, &page_list);
    };

    let reader = engine.document().reader();
    let page_by_ref = page_ref_map(engine)?;
    let selected: BTreeSet<usize> = page_list.iter().copied().collect();
    let marked_text = collect_marked_text(engine, &page_list)?;

    let root = reader.resolve(root_obj)?;
    let Some(root_dict) = root.as_dict() else {
        return geometric_fallback(engine, &page_list);
    };

    let mut parser = StructParser {
        reader,
        page_by_ref,
        marked_text,
        visited: HashSet::new(),
    };
    let mut elements = Vec::new();
    if let Some(kids) = root_dict.get("K") {
        parser.parse_kids(kids, None, &mut elements, 0)?;
    }

    elements = elements
        .into_iter()
        .filter_map(|el| prune_for_pages(el, &selected))
        .collect();

    let tables = collect_semantic_tables(&elements);
    Ok(SemanticDocument {
        tagged: true,
        source: SemanticSource::TaggedPdf,
        elements,
        tables,
    })
}

fn geometric_fallback(engine: &ContentEngine, pages: &[usize]) -> Result<SemanticDocument> {
    let mut children = Vec::new();
    for &page in pages {
        let layout = engine.analyze_page_layout_with(page, &LayoutConfig::default())?;
        children.extend(
            layout
                .blocks
                .into_iter()
                .map(|block| block_to_element(page, block)),
        );
    }

    Ok(SemanticDocument {
        tagged: false,
        source: SemanticSource::GeometricFallback,
        tables: Vec::new(),
        elements: vec![SemanticElement {
            element_type: "Document".to_string(),
            text: String::new(),
            alt_text: None,
            actual_text: None,
            lang: None,
            page: None,
            bbox: None,
            mcids: Vec::new(),
            children,
        }],
    })
}

fn block_to_element(page: usize, block: LayoutBlock) -> SemanticElement {
    SemanticElement {
        element_type: "P".to_string(),
        text: block.text(),
        alt_text: None,
        actual_text: None,
        lang: None,
        page: Some(page),
        bbox: Some([
            block.bbox.x0,
            block.bbox.y0,
            block.bbox.x1,
            block.bbox.y1,
        ]),
        mcids: Vec::new(),
        children: Vec::new(),
    }
}

struct StructParser<'a> {
    reader: &'a PdfReader,
    page_by_ref: HashMap<PageRef, usize>,
    marked_text: McidTextMap,
    visited: HashSet<PageRef>,
}

impl<'a> StructParser<'a> {
    fn parse_kids(
        &mut self,
        object: &PdfObject,
        inherited_page: Option<usize>,
        out: &mut Vec<SemanticElement>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_STRUCT_DEPTH {
            return Err(OxideError::MalformedPdf(
                "structure tree exceeded depth limit".to_string(),
            ));
        }
        match object {
            PdfObject::Array(items) => {
                for item in items {
                    self.parse_kids(item, inherited_page, out, depth + 1)?;
                }
            }
            PdfObject::Reference { number, generation } => {
                let id = (*number, *generation);
                if !self.visited.insert(id) {
                    log::warn!("skipping cyclic structure reference {number} {generation}");
                    return Ok(());
                }
                let resolved = self.reader.get_and_resolve(*number, *generation)?;
                self.parse_kids(&resolved, inherited_page, out, depth + 1)?;
                self.visited.remove(&id);
            }
            PdfObject::Dictionary(dict)
                if dict.get_name("S").is_some()
                    || matches!(dict.get_name("Type"), Some("StructElem")) =>
            {
                out.push(self.parse_element(dict, inherited_page, depth + 1)?);
            }
            _ => {}
        }
        Ok(())
    }

    fn parse_element(
        &mut self,
        dict: &PdfDictionary,
        inherited_page: Option<usize>,
        depth: usize,
    ) -> Result<SemanticElement> {
        if depth > MAX_STRUCT_DEPTH {
            return Err(OxideError::MalformedPdf(
                "structure tree exceeded depth limit".to_string(),
            ));
        }
        let element_type = dict.get_name("S").unwrap_or("Span").to_string();
        let page = dict
            .get("Pg")
            .and_then(|obj| page_from_object(obj, &self.page_by_ref))
            .or(inherited_page);
        let alt_text = dict.get("Alt").and_then(pdf_text_value);
        let actual_text = dict.get("ActualText").and_then(pdf_text_value);
        let lang = dict.get("Lang").and_then(pdf_text_value);

        let mut mcids = Vec::new();
        let mut children = Vec::new();
        if let Some(kids) = dict.get("K") {
            self.parse_element_kids(kids, page, &mut mcids, &mut children, depth + 1)?;
        }

        let text = match &actual_text {
            Some(actual) => actual.clone(),
            None => text_for_mcids(&mcids, &self.marked_text),
        };
        let bbox = bbox_for_mcids(&mcids, &self.marked_text);

        Ok(SemanticElement {
            element_type,
            text,
            alt_text,
            actual_text,
            lang,
            page,
            bbox,
            mcids,
            children,
        })
    }

    fn parse_element_kids(
        &mut self,
        object: &PdfObject,
        inherited_page: Option<usize>,
        mcids: &mut Vec<SemanticMcid>,
        children: &mut Vec<SemanticElement>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_STRUCT_DEPTH {
            return Err(OxideError::MalformedPdf(
                "structure tree exceeded depth limit".to_string(),
            ));
        }
        match object {
            PdfObject::Integer(mcid) => {
                if let Some(page) = inherited_page {
                    mcids.push(SemanticMcid { page, mcid: *mcid });
                }
            }
            PdfObject::Array(items) => {
                for item in items {
                    self.parse_element_kids(item, inherited_page, mcids, children, depth + 1)?;
                }
            }
            PdfObject::Reference { number, generation } => {
                let id = (*number, *generation);
                if !self.visited.insert(id) {
                    log::warn!("skipping cyclic structure kid {number} {generation}");
                    return Ok(());
                }
                let resolved = self.reader.get_and_resolve(*number, *generation)?;
                self.parse_element_kids(&resolved, inherited_page, mcids, children, depth + 1)?;
                self.visited.remove(&id);
            }
            PdfObject::Dictionary(dict) => {
                if dict.get_name("S").is_some()
                    || matches!(dict.get_name("Type"), Some("StructElem"))
                {
                    children.push(self.parse_element(dict, inherited_page, depth + 1)?);
                } else if let Some(mcid) = dict.get_integer("MCID") {
                    let page = dict
                        .get("Pg")
                        .and_then(|obj| page_from_object(obj, &self.page_by_ref))
                        .or(inherited_page);
                    if let Some(page) = page {
                        mcids.push(SemanticMcid { page, mcid });
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn page_ref_map(engine: &ContentEngine) -> Result<HashMap<PageRef, usize>> {
    let mut out = HashMap::new();
    for page in engine.document().get_pages()? {
        out.insert(
            (page.object_number, page.generation_number),
            page.page_number,
        );
    }
    Ok(out)
}

fn page_from_object(object: &PdfObject, page_by_ref: &HashMap<PageRef, usize>) -> Option<usize> {
    object
        .as_reference()
        .and_then(|id| page_by_ref.get(&id).copied())
}

fn pdf_text_value(object: &PdfObject) -> Option<String> {
    match object {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        PdfObject::Name(name) => Some(name.clone()),
        _ => None,
    }
}

fn collect_marked_text(engine: &ContentEngine, pages: &[usize]) -> Result<McidTextMap> {
    let mut out: McidTextMap = HashMap::new();
    for &page in pages {
        let ops = engine.get_page_content(page)?;
        let resources = engine.get_page_resources(page)?;
        let mut collector = TextCollector::new(resources, engine.document().reader());
        for marked in collector.collect_marked(&ops) {
            push_marked_chunk(page, marked, &mut out);
        }
    }
    Ok(out)
}

fn push_marked_chunk(page: usize, marked: MarkedTextChunk, out: &mut McidTextMap) {
    if let Some(mcid) = marked.mcid {
        out.entry((page, mcid)).or_default().push(marked.chunk);
    }
}

fn text_for_mcids(mcids: &[SemanticMcid], text_map: &McidTextMap) -> String {
    let mut chunks = Vec::new();
    for id in mcids {
        if let Some(found) = text_map.get(&(id.page, id.mcid)) {
            chunks.extend(found.iter().cloned());
        }
    }
    chunks_to_text(chunks)
}

/// Union of the text-chunk boxes for an element's MCIDs, in user space (y-up).
/// Each chunk's box is `[x, x+width] × [y, y+font_size]` (matching the layout
/// analyzer). Returns `None` when no MCID resolves to any text. The union is
/// min/max, so the `HashMap` lookup order does not affect the result.
fn bbox_for_mcids(mcids: &[SemanticMcid], text_map: &McidTextMap) -> Option<[f64; 4]> {
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    let mut any = false;
    for id in mcids {
        let Some(found) = text_map.get(&(id.page, id.mcid)) else {
            continue;
        };
        for c in found {
            if c.text.trim().is_empty() {
                continue;
            }
            let fs = if c.font_size > 0.0 { c.font_size } else { 1.0 };
            x0 = x0.min(c.x);
            y0 = y0.min(c.y);
            x1 = x1.max(c.x + c.width.max(0.0));
            y1 = y1.max(c.y + fs);
            any = true;
        }
    }
    if any && x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite() {
        Some([x0, y0, x1, y1])
    } else {
        None
    }
}

fn chunks_to_text(chunks: Vec<TextChunk>) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    let mut reconstructor = ReadingOrderReconstructor::new();
    reconstructor.detect_columns = false;
    reconstructor
        .reconstruct(chunks)
        .into_iter()
        .map(|line| line.text)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn prune_for_pages(
    element: SemanticElement,
    selected: &BTreeSet<usize>,
) -> Option<SemanticElement> {
    let mut element = element;
    element.children = element
        .children
        .into_iter()
        .filter_map(|child| prune_for_pages(child, selected))
        .collect();

    let direct_selected = element
        .page
        .map(|page| selected.contains(&page))
        .unwrap_or(false)
        || element.mcids.iter().any(|id| selected.contains(&id.page));
    if direct_selected || !element.children.is_empty() || selected.is_empty() {
        Some(element)
    } else {
        None
    }
}

fn collect_semantic_tables(elements: &[SemanticElement]) -> Vec<Table> {
    let mut out = Vec::new();
    for element in elements {
        collect_semantic_tables_from(element, &mut out);
    }
    out
}

fn collect_semantic_tables_from(element: &SemanticElement, out: &mut Vec<Table>) {
    if element.element_type == "Table" {
        if let Some(table) = table_from_element(element) {
            out.push(table);
        }
    }
    for child in &element.children {
        collect_semantic_tables_from(child, out);
    }
}

fn table_from_element(element: &SemanticElement) -> Option<Table> {
    let rows: Vec<Vec<(String, bool)>> = element
        .children
        .iter()
        .filter(|child| child.element_type == "TR")
        .map(|row| {
            row.children
                .iter()
                .filter(|cell| cell.element_type == "TH" || cell.element_type == "TD")
                .map(|cell| {
                    (
                        cell.combined_text(),
                        cell.element_type.eq_ignore_ascii_case("TH"),
                    )
                })
                .collect::<Vec<_>>()
        })
        .filter(|row| !row.is_empty())
        .collect();

    if rows.is_empty() {
        None
    } else {
        Some(Table::from_semantic_rows(rows))
    }
}

fn write_element_text(element: &SemanticElement, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let display = element
        .alt_text
        .as_ref()
        .filter(|_| element.element_type == "Figure")
        .or_else(|| {
            if element.text.trim().is_empty() {
                None
            } else {
                Some(&element.text)
            }
        });

    if let Some(text) = display {
        out.push_str(&indent);
        out.push_str(&element.element_type);
        out.push_str(": ");
        out.push_str(text.trim());
        out.push('\n');
    } else if !element.children.is_empty() {
        out.push_str(&indent);
        out.push_str(&element.element_type);
        out.push('\n');
    }

    for child in &element.children {
        write_element_text(child, depth + 1, out);
    }
}
