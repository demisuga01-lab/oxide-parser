//! Per-**page** classifier: is this page digital-born (real extractable text),
//! scanned (a full-page image needing OCR), or a searchable scan (a full-page
//! image with an existing/invisible text layer the producer already OCR'd)?
//!
//! This is the routing gate in front of the extraction pipeline. A real-world
//! PDF is often *mixed* — a born-digital report with a few scanned inserts, or a
//! scanned doc with a digital cover — so the decision is made **per page**, not
//! per document. Digital-born pages (including searchable scans) go through the
//! consolidated digital-born extraction pass; pages classified [`PageSource::Scanned`]
//! are routed to the OCR path (a documented seam the OCR stage fills in).
//!
//! # Signals (combined; never decided on one alone)
//!
//! - **Text coverage** — non-whitespace character count and the fraction of page
//!   area covered by text boxes. Near-zero → likely scanned.
//! - **Image coverage** — the fraction of page area covered by image XObject
//!   placements. One image covering most of the page → likely a scan.
//! - **Invisible text** — render-mode-3 text (`Tr 3`), the hallmark of a
//!   scanner-produced searchable layer over the scan image. Invisible text is
//!   still *usable* text, so a page with a full-page image **and** a real or
//!   invisible text layer is [`PageSource::DigitalBornOverImage`], NOT scanned —
//!   it must use its existing layer, never be re-OCR'd.
//!
//! # Determinism
//!
//! Pure geometry + counts; no `HashMap` iterated to produce output. Same page →
//! same classification.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::analysis::graphics::{collect_graphics_with_images, ImagePlacement};
use crate::engine::ContentEngine;
use crate::error::Result;
use crate::object::PdfObject;

/// How a single page's content can be recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageSource {
    /// Real extractable text; use the digital-born extraction pass.
    DigitalBorn,
    /// A full-page image **with** an existing/invisible text layer (a scanner-
    /// produced searchable PDF). Uses the existing text layer — never re-OCR'd.
    DigitalBornOverImage,
    /// (Almost) no usable text and a dominant full-page image — needs OCR. The
    /// OCR stage replaces this page's placeholder with recovered blocks.
    Scanned,
}

impl PageSource {
    /// `true` when this page has usable text and goes through the digital-born
    /// extraction pass (`DigitalBorn` or `DigitalBornOverImage`).
    pub fn is_digital_born(self) -> bool {
        matches!(
            self,
            PageSource::DigitalBorn | PageSource::DigitalBornOverImage
        )
    }
}

/// The classification of one page: the decision, its confidence, and the raw
/// signals it was based on (so consumers can audit or re-threshold).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PageClassification {
    /// 1-based page number.
    pub page: u32,
    pub source: PageSource,
    /// 0..1 confidence in the decision.
    pub confidence: f32,
    /// Non-whitespace characters of extractable (incl. invisible) text.
    pub char_count: usize,
    /// Fraction of page area covered by text boxes (clamped to 1.0).
    pub text_coverage: f32,
    /// Fraction of page area covered by image XObject placements (clamped 1.0).
    pub image_coverage: f32,
    /// Whether any render-mode-3 (invisible) text was present — the searchable-
    /// scan signal.
    pub has_invisible_text: bool,
}

/// Tunable thresholds. Defaults are documented and document-relative where it
/// matters (coverage is page-area-relative, so resolution-independent).
#[derive(Debug, Clone, Copy)]
pub struct ClassifyConfig {
    /// Below this many non-whitespace chars, a page has effectively no text.
    pub min_text_chars: usize,
    /// An image covering at least this fraction of the page is "full-page".
    pub full_page_image_frac: f32,
    /// Text covering at least this fraction of the page is a confident text page
    /// regardless of an image background.
    pub strong_text_coverage: f32,
}

impl Default for ClassifyConfig {
    fn default() -> Self {
        ClassifyConfig {
            // ~a short caption's worth of text; below this a "text" page is noise.
            min_text_chars: 16,
            // A scan image typically fills ≥70% of the page.
            full_page_image_frac: 0.70,
            // A genuine text page usually paints ≥5% of its area with glyphs.
            strong_text_coverage: 0.05,
        }
    }
}

/// Classify a single page. Never errors on a malformed page — on failure it
/// returns a low-confidence [`PageSource::Scanned`] (the safe default: routes to
/// OCR, which can still try, rather than silently emitting nothing as text).
pub fn classify_page(
    engine: &ContentEngine,
    page: usize,
    cfg: &ClassifyConfig,
) -> PageClassification {
    classify_page_inner(engine, page, cfg).unwrap_or(PageClassification {
        page: page as u32,
        source: PageSource::Scanned,
        confidence: 0.3,
        char_count: 0,
        text_coverage: 0.0,
        image_coverage: 0.0,
        has_invisible_text: false,
    })
}

fn classify_page_inner(
    engine: &ContentEngine,
    page: usize,
    cfg: &ClassifyConfig,
) -> Result<PageClassification> {
    use crate::text::TextCollector;

    let (pw, ph) = engine.page_dimensions(page)?;
    let page_area = (pw * ph).max(1.0);

    let ops = engine.get_page_content(page)?;
    let resources = engine.get_page_resources(page)?;

    // Text signals: collect chunks, count non-whitespace chars, sum text box area,
    // and note any invisible (Tr 3) text.
    let mut collector = TextCollector::new(resources, engine.document().reader());
    let chunks = collector.collect(&ops);
    let mut char_count = 0usize;
    let mut text_area = 0.0f64;
    let mut has_invisible_text = false;
    for c in &chunks {
        let n = c.text.chars().filter(|ch| !ch.is_whitespace()).count();
        if n == 0 {
            continue;
        }
        char_count += n;
        if c.is_invisible {
            has_invisible_text = true;
        }
        let h = if c.font_size > 0.0 { c.font_size } else { 1.0 };
        text_area += c.width.max(0.0) * h;
    }
    let text_coverage = (text_area / page_area).clamp(0.0, 1.0) as f32;

    // Image signal: total area covered by image XObject `Do` placements.
    let image_names = page_image_names(engine, page)?;
    let graphics = collect_graphics_with_images(&ops, &image_names);
    let image_coverage = (image_area(&graphics.images) / page_area).clamp(0.0, 1.0) as f32;

    let (source, confidence) = decide(
        char_count,
        text_coverage,
        image_coverage,
        has_invisible_text,
        cfg,
    );

    Ok(PageClassification {
        page: page as u32,
        source,
        confidence,
        char_count,
        text_coverage,
        image_coverage,
        has_invisible_text,
    })
}

/// The decision logic, split out so it is unit-testable on raw signals without a
/// PDF. Combines the signals; never relies on one alone.
fn decide(
    char_count: usize,
    text_coverage: f32,
    image_coverage: f32,
    has_invisible_text: bool,
    cfg: &ClassifyConfig,
) -> (PageSource, f32) {
    let has_text = char_count >= cfg.min_text_chars;
    let full_page_image = image_coverage >= cfg.full_page_image_frac;

    match (has_text, full_page_image) {
        // Real text over a full-page image → searchable scan. Use the text layer.
        // Invisible text makes this near-certain; visible text over a full-page
        // image is the rarer "text on a photo background" but is still text-first.
        (true, true) => {
            let conf = if has_invisible_text { 0.95 } else { 0.85 };
            (PageSource::DigitalBornOverImage, conf)
        }
        // Real text, no dominant image → ordinary digital-born page.
        (true, false) => {
            // Stronger when text actually covers a meaningful share of the page.
            let conf = if text_coverage >= cfg.strong_text_coverage {
                0.95
            } else {
                0.8
            };
            (PageSource::DigitalBorn, conf)
        }
        // No usable text but a full-page image → a scan needing OCR.
        (false, true) => (PageSource::Scanned, 0.95),
        // No usable text and no dominant image → an (almost) empty page. Treat as
        // scanned (route to OCR, which is harmless on a blank) but low confidence.
        (false, false) => {
            // A page with a little text but below the floor leans digital-born;
            // a truly empty page leans scanned.
            if char_count > 0 {
                (PageSource::DigitalBorn, 0.55)
            } else {
                (PageSource::Scanned, 0.5)
            }
        }
    }
}

/// Classify every page in `pages` (empty → all pages).
pub fn classify_document(
    engine: &ContentEngine,
    pages: &[usize],
    cfg: &ClassifyConfig,
) -> Result<Vec<PageClassification>> {
    let total = engine.page_count()?;
    let list: Vec<usize> = if pages.is_empty() {
        (1..=total).collect()
    } else {
        pages.to_vec()
    };
    Ok(list
        .iter()
        .map(|&p| classify_page(engine, p, cfg))
        .collect())
}

/// Names of the page's top-level *image* XObjects (never Form XObjects) — the
/// set passed to [`collect_graphics_with_images`] so only image `Do`s are
/// measured. (Mirrors the docmodel layer's private helper; kept local so the
/// classifier has no dependency on the model builder.)
fn page_image_names(engine: &ContentEngine, page: usize) -> Result<BTreeSet<String>> {
    let resources = engine.get_page_resources(page)?;
    let reader = engine.document().reader();
    let mut names = BTreeSet::new();
    for (name, &(obj, gen)) in &resources.xobjects {
        if let Ok(PdfObject::Stream { dict, .. }) = reader.get_object(obj, gen) {
            if dict.get_name("Subtype") == Some("Image") {
                names.insert(name.clone());
            }
        }
    }
    Ok(names)
}

/// Total area of image placements, **merging overlaps** so two `Do`s of the same
/// image (or a tiled background) don't double-count past the page area. Uses a
/// simple union-of-area via inclusion bound: sum of areas minus pairwise
/// intersections is an approximation, so instead we take the area of the union
/// bounding the largest single placement and add the non-overlapping remainder
/// conservatively — but for the "is one image full-page?" question the single
/// largest placement is the robust signal, so we report the max single-image
/// area, which cannot exceed the page and is overlap-free by construction.
fn image_area(images: &[ImagePlacement]) -> f64 {
    images
        .iter()
        .map(|p| p.bbox.width() * p.bbox.height())
        .fold(0.0f64, f64::max)
}

#[cfg(test)]
mod tests;
