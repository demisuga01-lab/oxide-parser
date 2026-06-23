//! **Key-value / form-field extraction** — turning a document into structured
//! fields (invoice number, date, total, line items; receipt merchant/amount;
//! form label→value pairs).
//!
//! # Source-agnostic by construction
//!
//! The spatial engine ([`spatial`]) and templates ([`profile`]) operate on the
//! canonical [`crate::parse::Document`] — the *same* blocks whether they came
//! from a digital-born page or an OCR'd scan ([`crate::ocr`]). A scanned invoice
//! and a digital invoice become the same blocks, so one KV engine handles both.
//! The only path that touches the PDF directly is AcroForm extraction
//! ([`acroform`]), which reads real `/AcroForm` form fields when present.
//!
//! # Pure-Rust, no ML
//!
//! Extraction uses three complementary strategies, all geometric / pattern /
//! proximity heuristics (no ML model):
//!
//! 1. [`acroform`] — exact field→value pairs from a real `/AcroForm` (highest
//!    confidence; zero heuristics).
//! 2. [`spatial`] — label→value pairing by geometry (`Total: $42.00`, a value
//!    below or right of its label) with pattern-based value typing.
//! 3. [`profile`] — document-type profiles (invoice / receipt / form) that know
//!    which fields to seek and assemble a clean typed result, plus line-item
//!    table → structured rows.
//!
//! ML-based KV (LayoutLM-style) is a possible future backend behind this same
//! `ExtractedFields` interface; it is intentionally not built (it would break
//! the pure-Rust contract and is unnecessary for the structured documents that
//! dominate this use case).

pub mod acroform;
pub mod profile;
pub mod spatial;
pub mod value;

use serde::{Deserialize, Serialize};

pub use value::{FieldValue, ValueHint};

/// Where a field came from — its provenance and rough trust level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    /// A real `/AcroForm` form field — exact.
    AcroForm,
    /// Spatial label→value pairing on the document blocks.
    Spatial,
    /// Assembled/normalized under a document-type profile.
    Template,
}

/// One extracted field: a key, its typed/normalized value, the raw text, where
/// it was found, how confident we are, and where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// The field key (human label when known: `/TU`, a profile name, or the
    /// detected label text).
    pub key: String,
    /// The normalized, typed value.
    pub value: FieldValue,
    /// The raw value text before normalization (always preserved).
    pub raw: String,
    /// 1-based page the value sits on (`0` when unknown, e.g. some AcroForm
    /// fields with no widget).
    pub page: u32,
    /// Value bounding box in user space `[x0,y0,x1,y1]` (`[0;4]` when unknown).
    pub bbox: [f64; 4],
    /// 0..1 confidence (label clarity × geometric strength × pattern match, or
    /// `1.0` for exact AcroForm values, scaled by OCR confidence on scans).
    pub confidence: f32,
    pub source: FieldSource,
}

/// A structured line-item row (invoice/receipt), mapped from the line-item
/// table's header columns. All parts optional — real tables vary.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LineItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_price: Option<FieldValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<FieldValue>,
}

/// The detected document type, which selects the profile applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocType {
    Invoice,
    Receipt,
    Form,
    Generic,
}

impl DocType {
    /// Parse a CLI `--type` value.
    pub fn parse(s: &str) -> Option<DocType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "invoice" => Some(DocType::Invoice),
            "receipt" => Some(DocType::Receipt),
            "form" => Some(DocType::Form),
            "generic" => Some(DocType::Generic),
            _ => None,
        }
    }
}

/// The complete extraction result for a document — the automation-consumable
/// JSON payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedFields {
    /// Schema version of this payload.
    pub schema_version: String,
    /// The detected (or caller-forced) document type.
    pub doc_type: DocType,
    /// `true` if the type was caller-forced rather than auto-detected.
    pub doc_type_forced: bool,
    /// All extracted fields, in a stable order (AcroForm first, then the
    /// profile's canonical fields, then remaining spatial pairs).
    pub fields: Vec<Field>,
    /// Structured line items (invoice/receipt), when a line-item table was
    /// found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_items: Vec<LineItem>,
}

/// Schema version of the [`ExtractedFields`] JSON.
pub const FIELDS_SCHEMA_VERSION: &str = "1.0";

/// Options for [`crate::engine::ContentEngine::extract_fields`].
#[derive(Clone)]
pub struct ExtractOptions {
    /// Force a document type, or `None` to auto-detect.
    pub doc_type: Option<DocType>,
    /// Restrict to these 1-based pages; empty means all.
    pub pages: Vec<usize>,
    /// Drop fields below this confidence in the output (`0.0` keeps all).
    pub min_confidence: f32,
    /// The OCR engine to use if scanned pages must be recognized first (passed
    /// straight through to [`crate::parse`]). `None` → scanned pages degrade.
    pub ocr: Option<std::sync::Arc<dyn crate::ocr::OcrEngine>>,
    /// OCR languages / segmentation passed through to the parse step. Ignored
    /// when [`Self::ocr`] is `None`.
    pub ocr_options: crate::ocr::OcrOptions,
    /// DPI for OCR rasterization. Ignored when [`Self::ocr`] is `None`.
    pub ocr_dpi: u32,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            doc_type: None,
            pages: Vec::new(),
            min_confidence: 0.0,
            ocr: None,
            ocr_options: crate::ocr::OcrOptions::default(),
            ocr_dpi: 300,
        }
    }
}

impl std::fmt::Debug for ExtractOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractOptions")
            .field("doc_type", &self.doc_type)
            .field("pages", &self.pages)
            .field("min_confidence", &self.min_confidence)
            .field(
                "ocr",
                &self.ocr.as_ref().map(|e| e.name()).unwrap_or("none"),
            )
            .finish()
    }
}

impl ExtractedFields {
    /// Serialize to pretty JSON (the automation output).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Look up the first field with the given key (case-insensitive).
    pub fn get(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key.eq_ignore_ascii_case(key))
    }
}

/// Extract structured key-value fields from a document — the orchestrator that
/// combines all three strategies.
///
/// 1. AcroForm fields (exact, when the PDF has real form fields).
/// 2. The canonical [`Document`](crate::parse::Document) is parsed once (running
///    OCR on scanned pages when an engine is supplied), then the spatial engine
///    finds label→value pairs over its blocks.
/// 3. The detected (or forced) document-type profile re-keys spatial pairs to
///    canonical names and assembles line items.
///
/// Source-agnostic: steps 2–3 see the same blocks whether digital-born or OCR'd.
pub fn extract_fields(
    engine: &crate::engine::ContentEngine,
    options: &ExtractOptions,
) -> crate::error::Result<ExtractedFields> {
    // 1. AcroForm (direct). Built before parsing so detection can use it.
    let widget_pages = acroform::WidgetPageIndex::build(engine);
    let acroform_fields = match engine.document().get_catalog() {
        Ok(catalog) => {
            acroform::extract_acroform_fields(&catalog, engine.document().reader(), &widget_pages)
        }
        Err(_) => Vec::new(),
    };
    let has_acroform = !acroform_fields.is_empty();

    // 2. Parse the document once (OCR scanned pages if an engine was supplied),
    //    then run the spatial label→value engine over its blocks.
    let parse_opts = crate::parse::ParseOptions {
        pages: options.pages.clone(),
        // Keep furniture: a label/value can live in a header/footer band.
        omit_furniture: false,
        ocr: options.ocr.clone(),
        ocr_options: options.ocr_options.clone(),
        ocr_dpi: options.ocr_dpi,
        ..Default::default()
    };
    let doc = crate::parse::parse(engine, &parse_opts)?;
    let spatial_fields = spatial::extract_spatial_fields(&doc);

    // 3. Document type + profile.
    let (doc_type, forced) = match options.doc_type {
        Some(t) => (t, true),
        None => (profile::detect_doc_type(&doc, has_acroform), false),
    };
    let prof = profile::profile_for(doc_type);
    let (canonical, leftover_spatial) = profile::apply_profile(prof, spatial_fields);

    // Assemble fields: AcroForm first (exact), then the profile's canonical
    // fields, then remaining spatial pairs. Stable order.
    let mut fields = Vec::new();
    fields.extend(acroform_fields);
    fields.extend(canonical);
    fields.extend(leftover_spatial);

    // Apply the confidence floor.
    if options.min_confidence > 0.0 {
        let floor = options.min_confidence;
        fields.retain(|f| f.confidence >= floor);
    }

    // Line items for invoice/receipt.
    let line_items = match doc_type {
        DocType::Invoice | DocType::Receipt => profile::extract_line_items(&doc),
        _ => Vec::new(),
    };

    Ok(ExtractedFields {
        schema_version: FIELDS_SCHEMA_VERSION.to_string(),
        doc_type,
        doc_type_forced: forced,
        fields,
        line_items,
    })
}
