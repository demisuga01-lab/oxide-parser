//! The **canonical document model** and the unified `parse` entry point.
//!
//! This module is the *spine* of Oxide's document-parser pivot. Every
//! extraction path — digital-born ([`crate::docmodel`] tags-first / geometric),
//! and (in a later stage) OCR — converges on the single [`Document`] type
//! defined here, and the output stage (Markdown / JSON / HTML / CSV) is written
//! *once* against it. If the model is right, the rest is wiring.
//!
//! # Relationship to [`crate::docmodel`]
//!
//! [`crate::docmodel`] already builds a flat, reading-ordered
//! [`DocumentModel`](crate::docmodel::DocumentModel) of typed
//! [`DocBlock`](crate::docmodel::DocBlock)s via a proven tags-first /
//! geometric-fallback pipeline (segmentation → classification → tables →
//! figures → caption linkage → running-element detection). Rather than
//! re-implement that, this module **wraps** it: [`parse`] runs
//! `build_document_model`, then assembles a [`Document`] that adds the pieces
//! the canonical model needs but the flat block list lacked:
//!
//! - [`DocumentMetadata`] — title/author/page-count/version/producer/tagged/
//!   dates/encrypted, lifted from [`crate::info::DocumentInfo`] (the `info`
//!   tool's work).
//! - [`Page`] — a per-page view that preserves page boundaries and geometry for
//!   consumers that paginate, derived from the same blocks.
//! - [`SourceInfo`] — per-document provenance (`DigitalBorn` / `Tagged`, with
//!   `Ocr` / `Mixed` reserved for the OCR stage).
//! - [`InlineText`] — a run-list of [`InlineSpan`]s that preserves inline
//!   emphasis (bold / italic) and link hrefs through to Markdown/HTML, instead
//!   of flattening to a bare `String`.
//! - A [`schema_version`](Document::schema_version) — the JSON shape is a public
//!   contract once consumers build on it.
//!
//! # Design guarantees
//!
//! - **Source-agnostic.** Nothing in [`Document`]/[`Block`] assumes digital-born
//!   vs OCR vs tagged. An OCR'd heading and a digital-born heading are the same
//!   [`BlockKind::Heading`]. The OCR stage produces positioned text → the *same*
//!   [`crate::docmodel`] builder → the *same* [`Document`].
//! - **Lossless-enough.** Every block keeps `page`, `bbox`, `reading_order`, and
//!   `confidence`, so consumers can re-derive geometry/order and filter by
//!   confidence. Classifying never discards position.
//! - **Deterministic.** Same PDF → byte-identical serialization. The wrapper
//!   adds no nondeterminism: ids and order come from the deterministic
//!   [`crate::docmodel`] builder; the serializers iterate in reading order; no
//!   `HashMap` is iterated to produce output.

use serde::{Deserialize, Serialize};

use crate::classify::{classify_document, ClassifyConfig, PageClassification, PageSource};
use crate::docmodel::{ClassifiedType, DocBlock, DocumentModel, ListItem, ModelSource};
use crate::engine::ContentEngine;
use crate::error::Result;
use crate::info::DocumentInfo;
use crate::ocr::{OcrEngine, OcrImage};

/// The schema version of the serialized [`Document`]. Bump the **major** when a
/// field is removed or its meaning changes (breaking); the **minor** when fields
/// are added (backward-compatible). Consumers should accept any document whose
/// major matches and minor is `<=` theirs.
///
/// `1.1` added per-page provenance ([`Page::source`] / [`Page::classification`]).
pub const SCHEMA_VERSION: &str = "1.1";

// ════════════════════════════════════════════════════════════════════════════
// Options
// ════════════════════════════════════════════════════════════════════════════

/// How figure image bytes are surfaced when serializing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ImageHandling {
    /// Do not extract image bytes; figures carry geometry/alt only (default).
    #[default]
    Omit,
    /// Reserved for a future pass: write extracted images to a sidecar directory
    /// and reference them by path. Currently behaves like [`ImageHandling::Omit`]
    /// at the wrapper level (image extraction wiring lands with the digital-born
    /// consolidation stage); the path is carried so the surface is stable.
    SidecarDir(std::path::PathBuf),
    /// Reserved: embed image bytes as base64 in JSON. Same note as `SidecarDir`.
    EmbedBase64,
}

/// Options for [`parse`] and the serializers.
#[derive(Clone)]
pub struct ParseOptions {
    /// Restrict to these 1-based pages; empty means all pages.
    pub pages: Vec<usize>,
    /// Drop blocks whose classification confidence is below this floor. `0.0`
    /// keeps everything (including honest low-confidence [`BlockKind::Text`]).
    pub min_confidence: f64,
    /// Omit page furniture (running headers/footers, page numbers) from the
    /// *body* and from Markdown/HTML output. Furniture is usually noise for RAG.
    /// It is always retained in the per-[`Page`] view and the JSON `pages` so no
    /// information is lost. Default `true`.
    pub omit_furniture: bool,
    /// How figure images are surfaced (see [`ImageHandling`]).
    pub images: ImageHandling,
    /// De-hyphenate words split across line ends: join `compi-\nlation` →
    /// `compilation`. Improves RAG text but mutates the extracted characters, so
    /// it defaults **off** for JSON fidelity. Documented as RAG-friendly.
    pub dehyphenate: bool,
    /// Normalize ligature codepoints to their constituent letters (ﬁ→fi, ﬂ→fl,
    /// …) for clean searchable text. Mutates characters, so defaults **off**.
    pub normalize_ligatures: bool,
    /// The OCR engine to apply to [`PageSource::Scanned`] pages. `None` (the
    /// default) keeps the pre-OCR behavior: scanned pages degrade to a
    /// placeholder note + the full-page scan figure. When `Some`, each scanned
    /// page is rasterized, preprocessed, recognized, and fed through the *same*
    /// document-model pipeline as digital-born text. The trait lives in the
    /// pure-Rust core; a concrete engine (e.g. `oxide-ocr-tesseract`) is injected.
    pub ocr: Option<std::sync::Arc<dyn crate::ocr::OcrEngine>>,
    /// Options passed to the OCR engine (languages, DPI, segmentation hint).
    pub ocr_options: crate::ocr::OcrOptions,
    /// Preprocessing applied to each rasterized scanned page before OCR — the
    /// quality lever (deskew/binarize/denoise). Ignored when [`Self::ocr`] is
    /// `None`.
    pub ocr_preprocess: crate::ocr::preprocess::PreprocessConfig,
    /// DPI at which scanned pages are rasterized for OCR. ~300 is the sweet spot.
    /// Ignored when [`Self::ocr`] is `None`.
    pub ocr_dpi: u32,
    /// Below this mean per-page OCR confidence, the page gets a low-confidence
    /// warning block so consumers know its text is unreliable. `0.0` disables the
    /// warning. Range 0..1.
    pub ocr_low_confidence_warn: f32,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            pages: Vec::new(),
            min_confidence: 0.0,
            omit_furniture: true,
            images: ImageHandling::default(),
            dehyphenate: false,
            normalize_ligatures: false,
            ocr: None,
            ocr_options: crate::ocr::OcrOptions::default(),
            ocr_preprocess: crate::ocr::preprocess::PreprocessConfig::default(),
            ocr_dpi: 300,
            ocr_low_confidence_warn: 0.5,
        }
    }
}

impl std::fmt::Debug for ParseOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseOptions")
            .field("pages", &self.pages)
            .field("min_confidence", &self.min_confidence)
            .field("omit_furniture", &self.omit_furniture)
            .field("images", &self.images)
            .field("dehyphenate", &self.dehyphenate)
            .field("normalize_ligatures", &self.normalize_ligatures)
            // The trait object has no Debug; record only whether one is present.
            .field(
                "ocr",
                &self.ocr.as_ref().map(|e| e.name()).unwrap_or("none"),
            )
            .field("ocr_options", &self.ocr_options)
            .field("ocr_preprocess", &self.ocr_preprocess)
            .field("ocr_dpi", &self.ocr_dpi)
            .field("ocr_low_confidence_warn", &self.ocr_low_confidence_warn)
            .finish()
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Inline text (run-list preserving emphasis + links)
// ════════════════════════════════════════════════════════════════════════════

/// Inline styling for one run of text. Derived from font flags (bold/italic) and
/// (in a later stage) `/Link` annotations + URI actions for `link`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InlineSpan {
    /// The literal text of the run.
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    /// Hyperlink target (absolute URI), when this run is a link. Populated by the
    /// digital-born consolidation stage; `None` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// A run-list of styled spans. Serializes as an array of [`InlineSpan`]. Keeping
/// it a span list (not a bare `String`) is what lets `**bold**`, `*italic*`, and
/// `[text](href)` survive into Markdown/HTML.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InlineText {
    pub spans: Vec<InlineSpan>,
}

impl InlineText {
    /// A single plain (unstyled) run. The common case for OCR / unattributed text.
    pub fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        if text.is_empty() {
            return InlineText { spans: Vec::new() };
        }
        InlineText {
            spans: vec![InlineSpan {
                text,
                ..Default::default()
            }],
        }
    }

    /// A single run with uniform bold/italic styling — used to lift a block's
    /// document-level emphasis flags onto its text. (Sub-run attribution at chunk
    /// granularity is a digital-born-stage refinement; block-level emphasis is
    /// what the current geometric builder knows.)
    pub fn styled(text: impl Into<String>, bold: bool, italic: bool) -> Self {
        let text = text.into();
        if text.is_empty() {
            return InlineText { spans: Vec::new() };
        }
        InlineText {
            spans: vec![InlineSpan {
                text,
                bold,
                italic,
                link: None,
            }],
        }
    }

    /// The concatenated plain text of all spans (styling dropped).
    pub fn to_plain(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Blocks
// ════════════════════════════════════════════════════════════════════════════

/// A nested list item carrying inline-styled text. Mirrors
/// [`crate::docmodel::ListItem`] but with [`InlineText`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListEntry {
    pub text: InlineText,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    pub ordered: bool,
}

/// The typed payload of a [`Block`]. Serialized with an internal `"kind"` tag,
/// e.g. `{"kind":"heading","level":1,"text":[...]}`.
///
/// This is the canonical, source-agnostic block vocabulary. It is a 1:1 mapping
/// of [`crate::docmodel::ClassifiedType`] enriched with [`InlineText`]; the
/// `Code`/`Quote` variants are reserved (the current builder does not yet emit
/// them) so the vocabulary is stable for downstream prompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockKind {
    Title {
        text: InlineText,
    },
    Heading {
        level: u8,
        text: InlineText,
    },
    Paragraph {
        text: InlineText,
    },
    List {
        ordered: bool,
        items: Vec<ListEntry>,
    },
    Figure {
        #[serde(skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        image: Option<ImageRef>,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<usize>,
    },
    Caption {
        text: InlineText,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<usize>,
    },
    Table {
        table: crate::analysis::tables::Table,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<usize>,
    },
    Header {
        text: InlineText,
    },
    Footer {
        text: InlineText,
    },
    PageNumber {
        text: InlineText,
    },
    /// Honest low-confidence fallback (better than a wrong label).
    Text {
        text: InlineText,
    },
}

impl BlockKind {
    /// `true` for running headers/footers and page numbers — the page furniture
    /// the `omit_furniture` option strips from the body.
    pub fn is_furniture(&self) -> bool {
        matches!(
            self,
            BlockKind::Header { .. } | BlockKind::Footer { .. } | BlockKind::PageNumber { .. }
        )
    }
}

/// A reference to an extracted figure image. Stable id plus, optionally, the
/// surfaced bytes (sidecar path or base64) per [`ImageHandling`]. Bytes are not
/// surfaced by the wrapper yet; the id + geometry are always present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Stable per-document image id (currently the owning block's id).
    pub id: usize,
    /// Sidecar file path, when `images = SidecarDir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Base64-encoded bytes, when `images = EmbedBase64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
}

/// One block in the document, located on a page and placed in global reading
/// order. The geometry/order/confidence are always present (lossless-enough).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// Stable id (survives `min_confidence` filtering); referenced by
    /// caption↔figure/table cross-links.
    pub id: usize,
    /// 1-based PDF page.
    pub page: u32,
    /// Bounding box in user space (y-up) `[x0,y0,x1,y1]`; `[0;4]` when unknown
    /// (a tagged element with no resolvable marked-content geometry).
    pub bbox: [f64; 4],
    /// Global reading-order index (ascending across pages).
    pub reading_order: u32,
    /// 0..1 classification confidence; low → treated as generic
    /// [`BlockKind::Text`].
    pub confidence: f32,
    #[serde(flatten)]
    pub kind: BlockKind,
}

// ════════════════════════════════════════════════════════════════════════════
// Pages, metadata, provenance
// ════════════════════════════════════════════════════════════════════════════

/// A per-page view: page geometry plus the ids of the blocks that live on it
/// (in reading order). The block bodies live once in [`Document::body`]; the
/// page view indexes into them so geometry/pagination consumers keep page
/// boundaries without duplicating content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// 1-based page number.
    pub number: u32,
    /// Page width in user-space units (from `/CropBox`, falling back to
    /// `/MediaBox`). `0.0` when unknown.
    pub width: f64,
    /// Page height in user-space units. `0.0` when unknown.
    pub height: f64,
    /// How this page's content was recovered (the routing decision). The
    /// per-page record that makes mixed documents (some born-digital, some
    /// scanned) representable. `DigitalBorn` by default when classification is
    /// not run. Added in schema `1.1`.
    pub source: PageSource,
    /// The full classifier signals behind [`Page::source`] (text/image coverage,
    /// char count, invisible-text). `None` when classification was not run (e.g.
    /// a model assembled directly without routing). Added in schema `1.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<PageClassification>,
    /// Ids of the blocks on this page, in reading order. Includes furniture even
    /// when `omit_furniture` strips it from the body, so no information is lost.
    pub block_ids: Vec<usize>,
}

/// Document-level metadata, lifted from [`crate::info::DocumentInfo`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creation_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modification_date: Option<String>,
    /// Document language (`/Lang`), when present. Reserved for the digital-born
    /// stage; `None` here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub page_count: usize,
    pub pdf_version: String,
    /// `true` if `/MarkInfo /Marked true` or a `/StructTreeRoot` is present.
    pub is_tagged: bool,
    pub is_encrypted: bool,
}

impl From<&DocumentInfo> for DocumentMetadata {
    fn from(info: &DocumentInfo) -> Self {
        DocumentMetadata {
            title: info.title.clone(),
            author: info.author.clone(),
            subject: info.subject.clone(),
            keywords: info.keywords.clone(),
            creator: info.creator.clone(),
            producer: info.producer.clone(),
            creation_date: info.creation_date.clone(),
            modification_date: info.mod_date.clone(),
            language: None,
            page_count: info.page_count,
            pdf_version: info.pdf_version.clone(),
            is_tagged: info.tagged,
            is_encrypted: info.encrypted,
        }
    }
}

/// How the document's content was recovered. Source-agnostic consumers ignore
/// this; provenance-aware ones (and the OCR stage) read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceInfo {
    /// Recovered from a tagged PDF's `/StructTreeRoot` (authoritative structure).
    Tagged,
    /// Recovered geometrically from a digital-born (selectable-text) PDF.
    DigitalBorn,
    /// Recovered by OCR of rasterized pages. Reserved for the OCR stage.
    Ocr,
    /// A mix of the above across pages. Reserved for the OCR stage.
    Mixed,
}

impl From<ModelSource> for SourceInfo {
    fn from(s: ModelSource) -> Self {
        match s {
            ModelSource::Tagged => SourceInfo::Tagged,
            ModelSource::Geometric => SourceInfo::DigitalBorn,
        }
    }
}

/// Roll up per-page classifications into the one document-level [`SourceInfo`].
/// `tagged` (the model came from `/StructTreeRoot`) takes precedence as the
/// document descriptor; otherwise the document is `DigitalBorn` if every page is
/// digital-born, `Ocr` if every page is scanned **and OCR recovered text**,
/// or `Mixed` when sources differ. An all-scanned doc with no OCR (or OCR that
/// recovered nothing) surfaces as `Mixed` — honestly "not digital-born".
///
/// `ocr_recovered` is `true` when at least one scanned page yielded non-empty
/// OCR text, so the label reflects what actually happened rather than merely
/// that an engine was configured.
fn rollup_source(tagged: bool, classes: &[PageClassification], ocr_recovered: bool) -> SourceInfo {
    if tagged {
        return SourceInfo::Tagged;
    }
    if classes.is_empty() {
        return SourceInfo::DigitalBorn;
    }
    let all_digital = classes.iter().all(|c| c.source.is_digital_born());
    let all_scanned = classes.iter().all(|c| c.source == PageSource::Scanned);
    if all_digital {
        SourceInfo::DigitalBorn
    } else if all_scanned {
        // Every page was scanned. If OCR recovered text, the document's content
        // came from OCR; otherwise it is not digital-born and not yet OCR'd.
        if ocr_recovered {
            SourceInfo::Ocr
        } else {
            SourceInfo::Mixed
        }
    } else {
        // A blend of digital-born and scanned pages → Mixed regardless (OCR'd or
        // not), since the document's content has more than one provenance.
        SourceInfo::Mixed
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Document
// ════════════════════════════════════════════════════════════════════════════

/// The canonical, source-agnostic parsed document: ordered, typed, located
/// content blocks plus metadata, a per-page view, and provenance. This is the
/// one model every extraction path produces and every serializer consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Schema version of this serialization (see [`SCHEMA_VERSION`]).
    pub schema_version: String,
    pub metadata: DocumentMetadata,
    pub source: SourceInfo,
    /// The flattened, cross-page reading-ordered block stream — the primary
    /// consumable (what Markdown/RAG ingest). With `omit_furniture`, furniture
    /// is excluded here (but kept in [`Page::block_ids`]).
    pub body: Vec<Block>,
    /// Per-page view preserving page boundaries + geometry. Derived from the
    /// same blocks; indexes into them by id.
    pub pages: Vec<Page>,
}

impl Document {
    /// Look up a block by id (linear; body is small relative to a page render).
    pub fn block(&self, id: usize) -> Option<&Block> {
        self.body.iter().find(|b| b.id == id)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// parse — the single entry point
// ════════════════════════════════════════════════════════════════════════════

/// Parse a PDF into the canonical [`Document`] model.
///
/// Routes **per page**: a [`crate::classify`] pass labels every page
/// `DigitalBorn` / `DigitalBornOverImage` / `Scanned`. Digital-born pages (incl.
/// searchable scans, which use their existing text layer) go through the proven
/// [`crate::docmodel`] builder (tags-first, geometric fallback); scanned pages
/// get a placeholder (a note + the full-page scan as a [`BlockKind::Figure`]) —
/// the **seam** the OCR stage fills in. Metadata, the per-page view (with
/// provenance), and inline styling are assembled on top. Deterministic.
pub fn parse(engine: &ContentEngine, options: &ParseOptions) -> Result<Document> {
    let info = engine.document_info()?;
    let metadata = DocumentMetadata::from(&info);

    // 0. The selected page list (mirrors the builder's expansion of "all").
    let page_list: Vec<usize> = if options.pages.is_empty() {
        (1..=info.page_count).collect()
    } else {
        options.pages.clone()
    };

    // 1. Per-page routing decision.
    let classes = classify_document(engine, &page_list, &ClassifyConfig::default())?;
    let digital_pages: Vec<usize> = page_list
        .iter()
        .zip(&classes)
        .filter(|(_, c)| c.source.is_digital_born())
        .map(|(&p, _)| p)
        .collect();

    // 2. Digital-born pages → the existing builder (restricted to that subset, so
    //    cross-page furniture detection and font stats run over real text only).
    let mut blocks: Vec<DocBlock> = if digital_pages.is_empty() {
        Vec::new()
    } else {
        let mut model: DocumentModel = engine.build_document_model(&digital_pages)?;
        if options.min_confidence > 0.0 {
            let floor = options.min_confidence;
            model.blocks.retain(|b| b.confidence >= floor);
        }
        model.blocks
    };
    let tagged = info.tagged && !digital_pages.is_empty();

    // 3. Scanned pages → the OCR seam. With an OCR engine injected, each scanned
    //    page is rasterized, preprocessed, recognized, and fed through the SAME
    //    document-model pipeline as digital text. Without one, it degrades to a
    //    placeholder (note + full-page scan figure). Ids continue after the
    //    builder's so cross-links stay unique; reading_order is patched in
    //    `assemble` after furniture filtering.
    let mut next_id = blocks.iter().map(|b| b.id + 1).max().unwrap_or(0);
    let mut ocr_recovered_any = false;
    for (&page, class) in page_list.iter().zip(&classes) {
        if class.source != PageSource::Scanned {
            continue;
        }
        let page_blocks = match options.ocr.as_deref() {
            Some(engine_ocr) => {
                match ocr_page_blocks(engine, engine_ocr, page, options, &mut next_id) {
                    Ok(bs) if bs.iter().any(|b| !b.text.trim().is_empty()) => {
                        ocr_recovered_any = true;
                        bs
                    }
                    // OCR ran but recovered nothing, or errored: fall back to the
                    // placeholder so the page is never silently dropped.
                    Ok(_) | Err(_) => scanned_placeholder_blocks(engine, page, &mut next_id),
                }
            }
            None => scanned_placeholder_blocks(engine, page, &mut next_id),
        };
        blocks.extend(page_blocks);
    }

    // 4. Page geometry + the rolled-up document source.
    let mut page_dims: Vec<(usize, f64, f64)> = Vec::with_capacity(page_list.len());
    for &p in &page_list {
        let (w, h) = engine.page_dimensions(p).unwrap_or((0.0, 0.0));
        page_dims.push((p, w, h));
    }
    let source = rollup_source(tagged, &classes, ocr_recovered_any);

    // 5. Hyperlinks: collect /Link annotations (URI actions) per digital-born
    //    page, rotated into the same upright space as the blocks, so the
    //    serializers can emit [text](href). Errors per page are non-fatal.
    let mut links: Vec<PageLink> = Vec::new();
    for (&page, class) in page_list.iter().zip(&classes) {
        if !class.source.is_digital_born() {
            continue;
        }
        if let Ok(page_links) = engine.page_links(page) {
            let rotate = engine.page_rotation(page).unwrap_or(0);
            let crop = engine.page_crop_box(page).unwrap_or([0.0, 0.0, 0.0, 0.0]);
            let (pw, ph) = engine.page_dimensions(page).unwrap_or((0.0, 0.0));
            for (rect, uri) in page_links {
                let rect = rotate_rect_for_links(rect, rotate, crop, pw, ph);
                links.push(PageLink {
                    page: page as u32,
                    rect,
                    uri,
                });
            }
        }
    }

    Ok(assemble(
        &blocks, metadata, source, &page_dims, &classes, &links, options,
    ))
}

/// A hyperlink rectangle on a page, in the same (upright) space as the blocks.
struct PageLink {
    page: u32,
    rect: [f64; 4],
    uri: String,
}

/// Rotate a link `/Rect` into the upright reading space the blocks live in,
/// matching the docmodel rotation normalization (so overlap tests are valid on
/// rotated pages). `pw`/`ph` are the pre-rotation page dims.
fn rotate_rect_for_links(
    rect: [f64; 4],
    rotate: i32,
    crop: [f64; 4],
    pw: f64,
    ph: f64,
) -> [f64; 4] {
    if rotate == 0 {
        return rect;
    }
    let cx0 = crop[0].min(crop[2]);
    let cy0 = crop[1].min(crop[3]);
    let map = |x: f64, y: f64| -> (f64, f64) {
        let u = x - cx0;
        let v = y - cy0;
        match rotate {
            90 => (v, pw - u),
            180 => (pw - u, ph - v),
            270 => (ph - v, u),
            _ => (u, v),
        }
    };
    let (ax, ay) = map(rect[0], rect[1]);
    let (bx, by) = map(rect[2], rect[3]);
    [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)]
}

/// Build the placeholder blocks for a `Scanned` page: a diagnostic note and the
/// full-page scan as a [`ClassifiedType::Figure`] so the page is *visible* even
/// before OCR exists. The OCR stage replaces the note with recovered text blocks
/// for this page (same shape: positioned blocks fed through the shared builder),
/// leaving the figure as the page background.
fn scanned_placeholder_blocks(
    engine: &ContentEngine,
    page: usize,
    next_id: &mut usize,
) -> Vec<DocBlock> {
    let (w, h) = engine.page_dimensions(page).unwrap_or((0.0, 0.0));
    let bbox = [0.0, 0.0, w, h];
    let note_id = *next_id;
    *next_id += 1;
    let fig_id = *next_id;
    *next_id += 1;

    let note = DocBlock {
        id: note_id,
        classified: ClassifiedType::Text,
        page,
        bbox,
        reading_order_index: 0,
        text: format!("[scanned page {page}: no text layer; OCR required]"),
        confidence: 0.0,
        basis: vec!["scanned:no-text-layer".to_string()],
        items: Vec::new(),
        caption_id: None,
        figure_id: None,
        header_footer: false,
        page_number: false,
        is_bold: false,
        is_italic: false,
        table: None,
    };
    let figure = DocBlock {
        id: fig_id,
        classified: ClassifiedType::Figure,
        page,
        bbox,
        reading_order_index: 0,
        text: format!("Scanned page {page}"),
        confidence: 0.9,
        basis: vec!["scanned:full-page-image".to_string()],
        items: Vec::new(),
        caption_id: None,
        figure_id: None,
        header_footer: false,
        page_number: false,
        is_bold: false,
        is_italic: false,
        table: None,
    };
    vec![note, figure]
}

/// OCR a single `Scanned` page and return its recovered [`DocBlock`]s — the
/// realized OCR seam.
///
/// Pipeline: rasterize the page → preprocess (deskew/binarize/denoise) →
/// recognize → map each word's pixel box into upright PDF user space → build
/// synthetic positioned [`TextChunk`]s → feed the SAME document-model machinery
/// (`page_data_from_chunks` + `assemble_pages_data`) the digital-born path uses.
/// The recovered blocks are re-keyed onto `next_id` and the page's full-page
/// scan is kept as a background [`ClassifiedType::Figure`]. A low mean confidence
/// adds a page-level warning block.
///
/// Errors (render/recognition failure) bubble up so the caller can fall back to
/// the placeholder; this never panics.
fn ocr_page_blocks(
    engine: &ContentEngine,
    ocr: &dyn OcrEngine,
    page: usize,
    options: &ParseOptions,
    next_id: &mut usize,
) -> Result<Vec<DocBlock>> {
    use crate::ocr::preprocess::preprocess;

    let dpi = options.ocr_dpi.max(1);
    // The viewport gives us the exact page↔pixel transform (DPI + display
    // rotation aware) we invert to place OCR words back in user space.
    let viewport = engine.page_viewport(page, dpi)?;
    let buffer = engine.render_page(page, dpi)?;

    // Rasterized page → grayscale → preprocess (the quality lever).
    let raw = buffer.to_raw_image();
    let gray = OcrImage::from(&raw);
    let (clean, skew_deg) = preprocess(&gray, &options.ocr_preprocess);

    let ocr_page = ocr.recognize(&clean, &options.ocr_options)?;

    // Map each recognized word from the (possibly deskewed) image frame back into
    // PDF user space and emit a synthetic positioned chunk.
    let img_w = clean.width as f64;
    let img_h = clean.height as f64;
    let layout_word_chunks: Vec<crate::text::TextChunk> = ocr_page
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| ocr_word_to_chunk(w, skew_deg, img_w, img_h, &viewport, true))
        .collect();
    let table_word_chunks: Vec<crate::text::TextChunk> = ocr_page
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| ocr_word_to_chunk(w, skew_deg, img_w, img_h, &viewport, false))
        .collect();

    // No words recovered -> let the caller fall back to a placeholder.
    if layout_word_chunks.is_empty() {
        return Ok(Vec::new());
    }

    // Merge per-word chunks into per-LINE chunks for prose/layout, while keeping
    // a separate cell-run view split at large column gaps for table detection.
    // That preserves the original anti-false-positive behavior for prose and
    // still gives OCR'd tables word-box geometry instead of whole-row lines.
    let line_ids: Vec<Option<u32>> = ocr_page
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| w.line_id)
        .collect();
    let chunks = merge_ocr_words_into_lines(&layout_word_chunks, &line_ids);
    let table_chunks = merge_ocr_words_into_cell_runs(&table_word_chunks, &line_ids);
    // Page dims in the upright space the chunks now live in (the viewport already
    // accounts for display rotation, so width/height here are post-rotation).
    let page_width = viewport.width_px as f64 / viewport.scale;
    let page_height = viewport.height_px as f64 / viewport.scale;

    // Feed the synthetic chunks through the SAME page assembly the digital-born
    // path uses — no OCR-specific layout/table/semantic code.
    let graphics = detect_ocr_ruling_graphics(&clean, &viewport);
    let page_data = crate::docmodel::page_data_from_layout_and_table_chunks(
        page,
        &chunks,
        &table_chunks,
        &graphics,
        page_width,
        page_height,
    );
    let model = crate::docmodel::assemble_pages_data(vec![page_data], 1)?;

    // Re-key the model's block ids onto the document-wide id space and carry the
    // OCR confidence into each block (so consumers can filter unreliable OCR).
    let mut out: Vec<DocBlock> = Vec::with_capacity(model.blocks.len() + 2);
    for mut b in model.blocks {
        b.id = *next_id;
        *next_id += 1;
        // Blend the page's mean OCR confidence into the geometric classifier's
        // confidence so a low-quality scan is reflected even on confidently-typed
        // blocks. (Min: a block is no more trustworthy than its OCR'd glyphs.)
        b.confidence = b.confidence.min(ocr_page.mean_confidence as f64);
        b.basis.push(format!(
            "ocr:{}{}",
            ocr.name(),
            ocr.version().map(|v| format!("@{v}")).unwrap_or_default()
        ));
        if skew_deg.abs() > 0.05 {
            b.basis.push(format!("ocr:deskew={skew_deg:.1}deg"));
        }
        out.push(b);
    }

    // Keep the full-page scan as a background figure (parity with the placeholder).
    let fig_id = *next_id;
    *next_id += 1;
    out.push(DocBlock {
        id: fig_id,
        classified: ClassifiedType::Figure,
        page,
        bbox: [0.0, 0.0, page_width, page_height],
        reading_order_index: 0,
        text: format!("Scanned page {page}"),
        confidence: 0.9,
        basis: vec!["scanned:full-page-image".to_string()],
        items: Vec::new(),
        caption_id: None,
        figure_id: None,
        header_footer: false,
        page_number: false,
        is_bold: false,
        is_italic: false,
        table: None,
    });

    // Low-confidence page warning (honesty about OCR quality).
    let warn = options.ocr_low_confidence_warn;
    if warn > 0.0 && ocr_page.mean_confidence < warn {
        let id = *next_id;
        *next_id += 1;
        out.push(DocBlock {
            id,
            classified: ClassifiedType::Text,
            page,
            bbox: [0.0, 0.0, page_width, page_height],
            reading_order_index: 0,
            text: format!(
                "[low-confidence OCR on page {page}: mean confidence {:.0}%]",
                ocr_page.mean_confidence * 100.0
            ),
            confidence: 0.0,
            basis: vec!["ocr:low-confidence".to_string()],
            items: Vec::new(),
            caption_id: None,
            figure_id: None,
            header_footer: false,
            page_number: false,
            is_bold: false,
            is_italic: false,
            table: None,
        });
    }

    Ok(out)
}

/// Convert one OCR word (pixel box in the preprocessed-image frame, y-down) into
/// a synthetic positioned [`TextChunk`] in upright PDF user space (y-up).
///
/// Two transforms compose: (1) undo the deskew rotation applied during
/// preprocessing, mapping the word's centre back into the *rendered* image frame;
/// (2) [`Viewport::pixel_to_page`] inverts the page→pixel transform (DPI + display
/// rotation + the y-flip) to land in the same upright user space the digital-born
/// chunks use. The chunk's `font_size` is the box height in points and its
/// `width` the box width in points, which is what the downstream line/segment
/// logic expects.
fn ocr_word_to_chunk(
    w: &crate::ocr::OcrWord,
    skew_deg: f64,
    img_w: f64,
    img_h: f64,
    viewport: &crate::render::Viewport,
    undo_deskew: bool,
) -> crate::text::TextChunk {
    // Word box corners in preprocessed-image pixels (y-down).
    let [x0, y0, x1, y1] = w.bbox;

    // Undo the deskew rotation about the image centre. `preprocess` rotated the
    // *content* by `skew_deg`; to recover original-render pixel positions we
    // rotate the points by `-skew_deg`.
    let unrotate = |px: f64, py: f64| -> (f64, f64) {
        if !undo_deskew || skew_deg.abs() <= 0.05 {
            return (px, py);
        }
        let a = (-skew_deg).to_radians();
        let (s, c) = a.sin_cos();
        let cx = img_w / 2.0;
        let cy = img_h / 2.0;
        let dx = px - cx;
        let dy = py - cy;
        (dx * c - dy * s + cx, dx * s + dy * c + cy)
    };

    // Map the four corners to page space and take the bounding box (rotation can
    // tilt the box, so bound it). `pixel_to_page` yields y-up user coordinates.
    let corners = [
        unrotate(x0, y0),
        unrotate(x1, y0),
        unrotate(x1, y1),
        unrotate(x0, y1),
    ];
    let mut pminx = f64::INFINITY;
    let mut pminy = f64::INFINITY;
    let mut pmaxx = f64::NEG_INFINITY;
    let mut pmaxy = f64::NEG_INFINITY;
    for (px, py) in corners {
        let (ux, uy) = viewport.pixel_to_page(px.round() as i32, py.round() as i32);
        pminx = pminx.min(ux);
        pminy = pminy.min(uy);
        pmaxx = pmaxx.max(ux);
        pmaxy = pmaxy.max(uy);
    }

    let width = (pmaxx - pminx).abs();
    let height = (pmaxy - pminy).abs();
    let is_rtl = crate::text::collector::is_rtl_dominant(&w.text);

    // TextChunk uses (x, y) as a lower-left baseline-ish anchor with width along
    // x and font_size as the glyph height. Downstream code groups by
    // y + font_size / 2, so y must be the lower box edge in user space.
    crate::text::TextChunk {
        text: w.text.clone(),
        x: pminx,
        y: pminy,
        font_size: height.max(1.0),
        font_name: "OCR".to_string(),
        width: width.max(0.0),
        is_rtl,
        is_vertical: false,
        is_invisible: false,
    }
}

/// Merge per-word OCR chunks into per-**line** chunks (one text-run per line),
/// matching the coarse granularity the digital-born path produces.
///
/// Words are grouped by their engine line id when available; words with no line
/// id (or a backend that does not report one) fall back to grouping by vertical
/// proximity (within ~60% of the median word height). Each group is sorted
/// left-to-right (right-to-left for RTL lines) and joined into a single chunk
/// spanning the line, with `font_size` = the line's median word height. This is
/// what keeps OCR'd prose from being mistaken for an aligned table and lets the
/// shared segmenter/classifier treat it exactly like digital text.
fn merge_ocr_words_into_lines(
    words: &[crate::text::TextChunk],
    line_ids: &[Option<u32>],
) -> Vec<crate::text::TextChunk> {
    if words.is_empty() {
        return Vec::new();
    }

    // Build groups of indices, one per line.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let have_ids = line_ids.iter().all(|l| l.is_some()) && line_ids.len() == words.len();

    if have_ids {
        use std::collections::BTreeMap;
        let mut by_id: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, lid) in line_ids.iter().enumerate() {
            by_id.entry(lid.unwrap()).or_default().push(i);
        }
        groups = by_id.into_values().collect();
    } else {
        // Fallback: cluster by y (chunk.y is the box top in y-up space, so words
        // on a line share ~the same y). Sort by descending y (top of page first).
        let med_h = {
            let mut hs: Vec<f64> = words.iter().map(|w| w.font_size).collect();
            hs.sort_by(|a, b| a.total_cmp(b));
            hs[hs.len() / 2].max(1.0)
        };
        let tol = med_h * 0.6;
        let mut idx: Vec<usize> = (0..words.len()).collect();
        idx.sort_by(|&a, &b| words[b].y.total_cmp(&words[a].y));
        let mut cur: Vec<usize> = Vec::new();
        let mut cur_y = f64::NAN;
        for i in idx {
            if cur.is_empty() || (words[i].y - cur_y).abs() <= tol {
                if cur.is_empty() {
                    cur_y = words[i].y;
                }
                cur.push(i);
            } else {
                groups.push(std::mem::take(&mut cur));
                cur_y = words[i].y;
                cur.push(i);
            }
        }
        if !cur.is_empty() {
            groups.push(cur);
        }
    }

    let mut lines: Vec<crate::text::TextChunk> = Vec::with_capacity(groups.len());
    for mut g in groups {
        if g.is_empty() {
            continue;
        }
        // RTL line if the majority of its words are RTL.
        let rtl = g.iter().filter(|&&i| words[i].is_rtl).count() * 2 > g.len();
        // Order words along the line. Visual left-to-right by x; for RTL the
        // reading order is right-to-left, so reverse.
        g.sort_by(|&a, &b| words[a].x.total_cmp(&words[b].x));
        if rtl {
            g.reverse();
        }

        let x0 = g.iter().map(|&i| words[i].x).fold(f64::INFINITY, f64::min);
        let x1 = g
            .iter()
            .map(|&i| words[i].x + words[i].width)
            .fold(f64::NEG_INFINITY, f64::max);
        let y = g
            .iter()
            .map(|&i| words[i].y)
            .fold(f64::NEG_INFINITY, f64::max);
        let mut hs: Vec<f64> = g.iter().map(|&i| words[i].font_size).collect();
        hs.sort_by(|a, b| a.total_cmp(b));
        let font_size = hs[hs.len() / 2].max(1.0);
        let text = g
            .iter()
            .map(|&i| words[i].text.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        lines.push(crate::text::TextChunk {
            text,
            x: x0,
            y,
            font_size,
            font_name: "OCR".to_string(),
            width: (x1 - x0).max(0.0),
            is_rtl: rtl,
            is_vertical: false,
            is_invisible: false,
        });
    }
    lines
}

/// Merge OCR words into table-oriented cell runs. Words on the same OCR line
/// stay together until a large horizontal gap appears; ordinary prose remains a
/// single run, while table rows split into column-like cells.
fn merge_ocr_words_into_cell_runs(
    words: &[crate::text::TextChunk],
    line_ids: &[Option<u32>],
) -> Vec<crate::text::TextChunk> {
    if words.is_empty() {
        return Vec::new();
    }

    let groups = group_ocr_words_by_line(words, line_ids);
    let mut runs = Vec::new();

    for mut group in groups {
        if group.is_empty() {
            continue;
        }
        group.sort_by(|&a, &b| words[a].x.total_cmp(&words[b].x));
        let mut heights: Vec<f64> = group.iter().map(|&i| words[i].font_size.max(1.0)).collect();
        heights.sort_by(|a, b| a.total_cmp(b));
        let line_h = heights[heights.len() / 2].max(1.0);
        let gap_cut = (line_h * 2.2).max(8.0);

        let mut current: Vec<usize> = Vec::new();
        let mut prev_right: Option<f64> = None;
        for idx in group {
            let word = &words[idx];
            let gap = prev_right.map(|r| word.x - r).unwrap_or(0.0);
            if !current.is_empty() && gap > gap_cut {
                runs.push(build_ocr_run(words, &current));
                current.clear();
            }
            prev_right = Some(word.x + word.width.max(0.0));
            current.push(idx);
        }
        if !current.is_empty() {
            runs.push(build_ocr_run(words, &current));
        }
    }

    runs
}

fn group_ocr_words_by_line(
    words: &[crate::text::TextChunk],
    line_ids: &[Option<u32>],
) -> Vec<Vec<usize>> {
    let have_ids = line_ids.iter().all(|l| l.is_some()) && line_ids.len() == words.len();
    if have_ids {
        use std::collections::BTreeMap;
        let mut by_id: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, lid) in line_ids.iter().enumerate() {
            by_id.entry(lid.unwrap()).or_default().push(i);
        }
        return by_id.into_values().collect();
    }

    let mut heights: Vec<f64> = words.iter().map(|w| w.font_size.max(1.0)).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let tol = heights[heights.len() / 2].max(1.0) * 0.6;
    let mut idx: Vec<usize> = (0..words.len()).collect();
    idx.sort_by(|&a, &b| words[b].y.total_cmp(&words[a].y));

    let mut groups = Vec::new();
    let mut cur = Vec::new();
    let mut cur_y = f64::NAN;
    for i in idx {
        if cur.is_empty() || (words[i].y - cur_y).abs() <= tol {
            if cur.is_empty() {
                cur_y = words[i].y;
            }
            cur.push(i);
        } else {
            groups.push(std::mem::take(&mut cur));
            cur_y = words[i].y;
            cur.push(i);
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
}

fn build_ocr_run(words: &[crate::text::TextChunk], idxs: &[usize]) -> crate::text::TextChunk {
    let rtl = idxs.iter().filter(|&&i| words[i].is_rtl).count() * 2 > idxs.len();
    let x0 = idxs
        .iter()
        .map(|&i| words[i].x)
        .fold(f64::INFINITY, f64::min);
    let x1 = idxs
        .iter()
        .map(|&i| words[i].x + words[i].width.max(0.0))
        .fold(f64::NEG_INFINITY, f64::max);
    let y = idxs
        .iter()
        .map(|&i| words[i].y)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut heights: Vec<f64> = idxs.iter().map(|&i| words[i].font_size.max(1.0)).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let font_size = heights[heights.len() / 2].max(1.0);
    let mut ordered = idxs.to_vec();
    ordered.sort_by(|&a, &b| words[a].x.total_cmp(&words[b].x));
    if rtl {
        ordered.reverse();
    }
    let text = ordered
        .iter()
        .map(|&i| words[i].text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    crate::text::TextChunk {
        text,
        x: x0,
        y,
        font_size,
        font_name: "OCR".to_string(),
        width: (x1 - x0).max(0.0),
        is_rtl: rtl,
        is_vertical: false,
        is_invisible: false,
    }
}

/// Recover long horizontal/vertical ruling lines directly from the preprocessed
/// scan image. Text glyph strokes are short broken runs; table borders produce
/// long continuous dark runs, which are mapped back to the same user-space
/// geometry consumed by the existing ruled-table detector.
fn detect_ocr_ruling_graphics(
    img: &OcrImage,
    viewport: &crate::render::Viewport,
) -> crate::analysis::graphics::DrawnGraphics {
    use crate::analysis::graphics::{DrawnGraphics, Segment};

    let mut graphics = DrawnGraphics::default();
    if !img.is_valid() {
        return graphics;
    }

    let w = img.width as i32;
    let h = img.height as i32;
    let min_h_run = ((w as usize) / 20).max(40);
    let min_v_run = ((h as usize) / 35).max(40);

    for y in 0..h {
        let mut x = 0;
        while x < w {
            while x < w && img.get(x as i64, y as i64) >= 128 {
                x += 1;
            }
            let start = x;
            while x < w && img.get(x as i64, y as i64) < 128 {
                x += 1;
            }
            let end = x;
            if (end - start) as usize >= min_h_run {
                let (ux0, uy0) = viewport.pixel_to_page(start, y);
                let (ux1, uy1) = viewport.pixel_to_page(end - 1, y);
                let y_avg = (uy0 + uy1) * 0.5;
                graphics.segments.push(Segment {
                    x0: ux0.min(ux1),
                    y0: y_avg,
                    x1: ux0.max(ux1),
                    y1: y_avg,
                });
            }
        }
    }

    for x in 0..w {
        let mut y = 0;
        while y < h {
            while y < h && img.get(x as i64, y as i64) >= 128 {
                y += 1;
            }
            let start = y;
            while y < h && img.get(x as i64, y as i64) < 128 {
                y += 1;
            }
            let end = y;
            if (end - start) as usize >= min_v_run {
                let (ux0, uy0) = viewport.pixel_to_page(x, start);
                let (ux1, uy1) = viewport.pixel_to_page(x, end - 1);
                let x_avg = (ux0 + ux1) * 0.5;
                graphics.segments.push(Segment {
                    x0: x_avg,
                    y0: uy0.min(uy1),
                    x1: x_avg,
                    y1: uy0.max(uy1),
                });
            }
        }
    }

    graphics
}
/// Assemble a [`Document`] from the flat block list + metadata + page geometry +
/// per-page classifications. Split out so it is unit-testable without a PDF.
/// `classes` may be empty (no routing run) — pages then default to
/// [`PageSource::DigitalBorn`] with no classification record.
fn assemble(
    blocks: &[DocBlock],
    metadata: DocumentMetadata,
    source: SourceInfo,
    page_dims: &[(usize, f64, f64)],
    classes: &[PageClassification],
    links: &[PageLink],
    options: &ParseOptions,
) -> Document {
    // Convert every block to the canonical form (furniture included), preserving
    // ids so cross-links and the page view stay resolvable.
    let mut converted: Vec<Block> = blocks.iter().map(convert_block).collect();

    // Attach hyperlinks: a link whose rect overlaps a block's bbox marks that
    // block's text spans with the href (so [text](href) survives). Whole-block
    // attribution (not per-glyph) — the common case is a short run that is the
    // whole link; finer span splitting is a future refinement.
    if !links.is_empty() {
        for b in &mut converted {
            attach_links(b, links);
        }
    }

    // Optional text-cleanup passes (mutate characters; off by default).
    if options.dehyphenate || options.normalize_ligatures {
        for b in &mut converted {
            clean_block_text(b, options);
        }
    }

    // Look up a page's classification by number (small slice; linear is fine).
    let class_for = |num: u32| -> (PageSource, Option<PageClassification>) {
        match classes.iter().find(|c| c.page == num) {
            Some(c) => (c.source, Some(*c)),
            None => (PageSource::DigitalBorn, None),
        }
    };

    // Per-page view: page geometry + the ids on each page in reading order. Built
    // from the full converted set so furniture is retained here regardless of
    // `omit_furniture`. A block's page may not be in `page_dims` (defensive); we
    // still index it under its page number with unknown geometry.
    let mut pages: Vec<Page> = page_dims
        .iter()
        .map(|&(num, w, h)| {
            let (psource, pclass) = class_for(num as u32);
            Page {
                number: num as u32,
                width: w,
                height: h,
                source: psource,
                classification: pclass,
                block_ids: Vec::new(),
            }
        })
        .collect();
    for b in &converted {
        match pages.iter_mut().find(|p| p.number == b.page) {
            Some(p) => p.block_ids.push(b.id),
            None => {
                let (psource, pclass) = class_for(b.page);
                pages.push(Page {
                    number: b.page,
                    width: 0.0,
                    height: 0.0,
                    source: psource,
                    classification: pclass,
                    block_ids: vec![b.id],
                });
            }
        }
    }
    // The block stream is already in reading order, so each page's ids are too.
    pages.sort_by_key(|p| p.number);

    // Body: optionally drop furniture, then re-densify reading_order so the body
    // is a clean 0..n sequence. Ids are preserved (cross-links stay valid).
    let mut body: Vec<Block> = if options.omit_furniture {
        converted
            .into_iter()
            .filter(|b| !b.kind.is_furniture())
            .collect()
    } else {
        converted
    };
    for (i, b) in body.iter_mut().enumerate() {
        b.reading_order = i as u32;
    }

    Document {
        schema_version: SCHEMA_VERSION.to_string(),
        metadata,
        source,
        body,
        pages,
    }
}

/// Attach a hyperlink href to a block's text spans when a link rectangle
/// overlaps the block's bbox. A block can carry only one link (the first
/// overlapping one in reading order); text/heading/caption/list blocks are
/// eligible (figures/tables are not). Marks every span so the whole run renders
/// as the link.
fn attach_links(b: &mut Block, links: &[PageLink]) {
    // Find the first link on this page whose rect overlaps the block bbox.
    let Some(link) = links
        .iter()
        .find(|l| l.page == b.page && rects_overlap(l.rect, b.bbox))
    else {
        return;
    };
    let href = link.uri.clone();
    let apply = |t: &mut InlineText| {
        for span in &mut t.spans {
            if span.link.is_none() {
                span.link = Some(href.clone());
            }
        }
    };
    match &mut b.kind {
        BlockKind::Title { text }
        | BlockKind::Heading { text, .. }
        | BlockKind::Paragraph { text }
        | BlockKind::Caption { text, .. }
        | BlockKind::Text { text } => apply(text),
        BlockKind::List { items, .. } => {
            for it in items {
                apply(&mut it.text);
            }
        }
        _ => {}
    }
}

/// Axis-aligned rectangle overlap (any positive-area intersection).
fn rects_overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    let ix = a[2].min(b[2]) - a[0].max(b[0]);
    let iy = a[3].min(b[3]) - a[1].max(b[1]);
    ix > 0.0 && iy > 0.0
}

/// Apply the optional text-cleanup passes to every [`InlineText`] a block holds
/// (paragraph/heading/caption/list text). Tables keep their cell text verbatim
/// (de-hyphenation across cells would be wrong).
fn clean_block_text(b: &mut Block, options: &ParseOptions) {
    let clean = |t: &mut InlineText| {
        for span in &mut t.spans {
            if options.normalize_ligatures {
                span.text = normalize_ligatures(&span.text);
            }
            if options.dehyphenate {
                span.text = dehyphenate(&span.text);
            }
        }
    };
    match &mut b.kind {
        BlockKind::Title { text }
        | BlockKind::Heading { text, .. }
        | BlockKind::Paragraph { text }
        | BlockKind::Caption { text, .. }
        | BlockKind::Header { text }
        | BlockKind::Footer { text }
        | BlockKind::PageNumber { text }
        | BlockKind::Text { text } => clean(text),
        BlockKind::List { items, .. } => {
            for it in items {
                clean(&mut it.text);
            }
        }
        BlockKind::Figure { .. } | BlockKind::Table { .. } => {}
    }
}

/// Join words split by a hyphen at a line break: `"compi- lation"` /
/// `"compi-\nlation"` → `"compilation"`. Only joins when the hyphen is preceded
/// by a letter and followed (after the break) by a lowercase letter — so real
/// hyphenated compounds (`"well-known"`) and dashes are left alone.
fn dehyphenate(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && i > 0 && chars[i - 1].is_alphabetic() {
            // Look past whitespace following the hyphen.
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            // Join only when a line break (or run-together) precedes a lowercase
            // continuation — the hallmark of an end-of-line hyphenation.
            if j > i + 1 && j < chars.len() && chars[j].is_lowercase() {
                // drop the hyphen + the whitespace; continue from j.
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Map common ligature codepoints to their constituent ASCII letters for clean
/// searchable text. Conservative: only the Latin presentation-form ligatures.
fn normalize_ligatures(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            _ => out.push(c),
        }
    }
    out
}

/// Convert one [`DocBlock`] into a canonical [`Block`], lifting its text into
/// [`InlineText`] with the block's emphasis flags.
fn convert_block(b: &DocBlock) -> Block {
    let text = || InlineText::styled(b.text.trim(), b.is_bold, b.is_italic);
    let kind = match b.classified {
        ClassifiedType::Title => BlockKind::Title { text: text() },
        ClassifiedType::Heading { level } => BlockKind::Heading {
            level: level.clamp(1, 6),
            text: text(),
        },
        ClassifiedType::Paragraph => BlockKind::Paragraph { text: text() },
        ClassifiedType::Text => BlockKind::Text { text: text() },
        ClassifiedType::List { ordered } => BlockKind::List {
            ordered,
            items: b.items.iter().map(convert_list_item).collect(),
        },
        // A bare `ListItem` (rare; tagged `LI` outside an `L`) becomes a
        // single-item list so it renders as one bullet.
        ClassifiedType::ListItem => BlockKind::List {
            ordered: false,
            items: vec![ListEntry {
                text: InlineText::plain(b.text.trim()),
                marker: None,
                ordered: false,
            }],
        },
        ClassifiedType::Figure => BlockKind::Figure {
            alt: {
                let t = b.text.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            },
            image: Some(ImageRef {
                id: b.id,
                path: None,
                data_base64: None,
            }),
            caption: b.caption_id,
        },
        ClassifiedType::Caption => BlockKind::Caption {
            text: text(),
            target: b.figure_id,
        },
        ClassifiedType::Table => BlockKind::Table {
            table: b
                .table
                .clone()
                .unwrap_or_else(|| empty_table_from_csv(&b.text)),
            caption: b.caption_id,
        },
        ClassifiedType::Header => BlockKind::Header { text: text() },
        ClassifiedType::Footer => BlockKind::Footer { text: text() },
        ClassifiedType::PageNumber => BlockKind::PageNumber { text: text() },
    };
    Block {
        id: b.id,
        page: b.page as u32,
        bbox: b.bbox,
        reading_order: b.reading_order_index as u32,
        confidence: b.confidence as f32,
        kind,
    }
}

fn convert_list_item(it: &ListItem) -> ListEntry {
    ListEntry {
        text: InlineText::plain(strip_marker(&it.text)),
        marker: it.marker.clone(),
        ordered: it.ordered,
    }
}

/// A `Table` block with no recovered structure (defensive): wrap the CSV text as
/// a single-column table so the block still serializes coherently.
fn empty_table_from_csv(csv: &str) -> crate::analysis::tables::Table {
    use crate::analysis::tables::{Table, TableSource};
    let rows: Vec<Vec<String>> = csv
        .lines()
        .map(|line| vec![line.to_string()])
        .collect::<Vec<_>>();
    Table {
        rows,
        cells: Vec::new(),
        header_hierarchy: Vec::new(),
        source: TableSource::Borderless,
        confidence: 0.0,
        bbox: [0.0; 4],
        notes: vec!["no recovered structure; csv fallback".to_string()],
    }
}

/// Drop a leading bullet/enumerator token from a list-item's text for clean
/// rendering. Mirrors the docmodel Markdown path so output stays consistent.
fn strip_marker(text: &str) -> String {
    let t = text.trim_start();
    let mut chars = t.chars();
    match chars.next() {
        Some(
            '\u{2022}' | '\u{25E6}' | '\u{2023}' | '\u{00B7}' | '\u{2043}' | '\u{2219}'
            | '\u{2013}' | '\u{2014}' | '-' | '*',
        ) if t.chars().nth(1).map(char::is_whitespace).unwrap_or(false) => {
            return t
                .chars()
                .skip(1)
                .collect::<String>()
                .trim_start()
                .to_string();
        }
        _ => {}
    }
    // Enumerator: token then '.'/')'/']' then space.
    if let Some(pos) = t.find(['.', ')', ']']) {
        let head = &t[..pos];
        if pos <= 7
            && !head.is_empty()
            && head.chars().all(|c| c.is_ascii_alphanumeric())
            && t[pos + 1..].starts_with(char::is_whitespace)
        {
            return t[pos + 1..].trim_start().to_string();
        }
    }
    t.to_string()
}

#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

// ════════════════════════════════════════════════════════════════════════════
// Serializers (written once, against the model)
// ════════════════════════════════════════════════════════════════════════════

mod serialize;

pub(crate) use serialize::serialize_block_markdown;
pub use serialize::SerializeOptions;

impl Document {
    /// The full faithful model as pretty JSON (the lossless format).
    pub fn to_json(&self) -> String {
        // serde_json on a fully-derived model is infallible here; fall back to a
        // minimal envelope on the impossible error rather than panicking.
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|_| format!("{{\"schema_version\":\"{}\"}}", self.schema_version))
    }

    /// The RAG/AI-facing Markdown rendering.
    pub fn to_markdown(&self, opts: &SerializeOptions) -> String {
        serialize::to_markdown(self, opts)
    }

    /// Markdown with default [`SerializeOptions`] — the common embedder path.
    ///
    /// Equivalent to `self.to_markdown(&SerializeOptions::default())`; provided
    /// so callers that don't need to tune furniture/page-break/provenance flags
    /// can render in one call.
    pub fn to_markdown_default(&self) -> String {
        self.to_markdown(&SerializeOptions::default())
    }

    /// Semantic HTML for human viewing / web.
    pub fn to_html(&self, opts: &SerializeOptions) -> String {
        serialize::to_html(self, opts)
    }

    /// HTML with default [`SerializeOptions`] — the common embedder path.
    ///
    /// Equivalent to `self.to_html(&SerializeOptions::default())`.
    pub fn to_html_default(&self) -> String {
        self.to_html(&SerializeOptions::default())
    }
}

#[cfg(test)]
mod tests;
