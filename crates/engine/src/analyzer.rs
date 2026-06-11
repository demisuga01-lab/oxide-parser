use crate::engine::ContentEngine;
use crate::error::Result;
use crate::text::collector::TextCollector;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum TextLayerRecommendation {
    /// PDF has a real text layer; use extract-text directly.
    UseExtractText,
    /// PDF appears to be scanned images; OCR would be needed for text.
    UseOcr,
    /// Some pages have text, others do not.
    Mixed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TextLayerAnalysis {
    /// True if a usable text layer was detected in the sampled pages.
    pub has_text_layer: bool,

    /// Confidence in the has_text_layer assessment, 0.0-1.0.
    pub confidence: f32,

    /// Page numbers where text was found. Only covers sampled pages.
    pub pages_with_text: Vec<usize>,

    /// Page numbers where no text was found. Only covers sampled pages.
    pub pages_without_text: Vec<usize>,

    /// Total non-whitespace characters decoded across all sampled pages.
    pub total_char_count: usize,

    /// True if the document is very likely fully scanned.
    pub is_likely_scanned: bool,

    /// Recommended action for the caller.
    pub recommendation: TextLayerRecommendation,

    /// Total number of pages in the document.
    pub total_pages: usize,

    /// Number of pages that were actually sampled for this analysis.
    pub sampled_pages: usize,
}

pub struct PdfAnalyzer;

impl PdfAnalyzer {
    /// Analyse up to `max_pages` pages (None = all pages).
    pub fn analyze(engine: &ContentEngine, max_pages: Option<usize>) -> Result<TextLayerAnalysis> {
        let total_pages = engine.page_count()?;
        if total_pages == 0 {
            return Ok(TextLayerAnalysis {
                has_text_layer: false,
                confidence: 0.0,
                pages_with_text: vec![],
                pages_without_text: vec![],
                total_char_count: 0,
                is_likely_scanned: false,
                recommendation: TextLayerRecommendation::UseOcr,
                total_pages: 0,
                sampled_pages: 0,
            });
        }

        let sample_count = max_pages.unwrap_or(total_pages).min(total_pages);

        let mut pages_with_text: Vec<usize> = Vec::new();
        let mut pages_without_text: Vec<usize> = Vec::new();
        let mut total_char_count = 0usize;

        for page_num in 1..=sample_count {
            let page_result = Self::analyze_single_page(engine, page_num);
            match page_result {
                Ok(char_count) => {
                    total_char_count += char_count;
                    if char_count > 0 {
                        pages_with_text.push(page_num);
                    } else {
                        pages_without_text.push(page_num);
                    }
                }
                Err(e) => {
                    log::warn!("PdfAnalyzer: page {} analysis failed: {}", page_num, e);
                    pages_without_text.push(page_num);
                }
            }
        }

        let with_count = pages_with_text.len();
        let without_count = pages_without_text.len();
        let sampled = with_count + without_count;

        let (has_text_layer, confidence, is_likely_scanned) = if sampled == 0 {
            (false, 0.0_f32, false)
        } else if with_count == 0 {
            (false, 0.95, true)
        } else if without_count == 0 {
            (true, 0.95, false)
        } else if with_count > without_count {
            (true, 0.7, false)
        } else {
            (false, 0.5, false)
        };

        let recommendation = if is_likely_scanned {
            TextLayerRecommendation::UseOcr
        } else if has_text_layer && with_count == sampled {
            TextLayerRecommendation::UseExtractText
        } else if has_text_layer {
            TextLayerRecommendation::Mixed
        } else {
            TextLayerRecommendation::UseOcr
        };

        Ok(TextLayerAnalysis {
            has_text_layer,
            confidence,
            pages_with_text,
            pages_without_text,
            total_char_count,
            is_likely_scanned,
            recommendation,
            total_pages,
            sampled_pages: sampled,
        })
    }

    /// Convenience: quick analysis sampling at most 3 pages.
    pub fn quick_analysis(engine: &ContentEngine) -> Result<TextLayerAnalysis> {
        Self::analyze(engine, Some(3))
    }

    /// Convenience: full analysis of all pages.
    pub fn full_analysis(engine: &ContentEngine) -> Result<TextLayerAnalysis> {
        Self::analyze(engine, None)
    }

    /// Open PDF bytes and run a quick analysis.
    pub fn analyze_bytes(pdf_bytes: Vec<u8>) -> Result<TextLayerAnalysis> {
        let engine = ContentEngine::open_bytes(pdf_bytes)?;
        Self::quick_analysis(&engine)
    }

    fn analyze_single_page(engine: &ContentEngine, page_num: usize) -> Result<usize> {
        let ops = engine.get_page_content(page_num)?;
        let resources = engine.get_page_resources(page_num)?;

        let text_op_count = ops
            .iter()
            .filter(|op| matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
            .count();

        if text_op_count == 0 {
            return Ok(0);
        }

        let mut collector = TextCollector::new(resources, engine.document().reader());
        let chunks = collector.collect(&ops);

        let non_ws: usize = chunks
            .iter()
            .map(|c| c.text.chars().filter(|ch| !ch.is_whitespace()).count())
            .sum();

        Ok(non_ws)
    }
}
