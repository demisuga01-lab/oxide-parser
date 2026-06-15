use rayon::prelude::*;

use super::collector::TextCollector;
use super::formatter::{TextFormatOptions, TextFormatter};
use super::reading_order::{ReadingOrderReconstructor, TextLine};
use crate::engine::ContentEngine;
use crate::error::Result;

/// Documents with at least this many pages are extracted in parallel. Below
/// this threshold the rayon fan-out/join overhead outweighs the benefit, so we
/// stay on the simple serial path to avoid regressing small-document latency.
const PARALLEL_PAGE_THRESHOLD: usize = 4;

#[derive(Debug, Clone, Default)]
pub struct TextExtractOptions {
    /// Which pages to extract. None = all pages.
    pub pages: Option<Vec<usize>>,

    /// Page marker and formatting options.
    pub format: TextFormatOptions,

    /// Reading-order reconstruction config.
    pub reading_order: ReadingOrderReconstructor,
}

pub struct TextExtractor;

impl Default for TextExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl TextExtractor {
    pub fn new() -> Self {
        TextExtractor
    }

    /// Extract text from all or selected pages of a document.
    ///
    /// Multi-page documents are extracted across rayon worker threads (the
    /// parsed [`ContentEngine`] is shared by immutable reference, so every
    /// thread reads the same parsed document — no per-page reparse). Page
    /// output is reassembled in the original page order regardless of the order
    /// threads finish, so the result is byte-identical to serial extraction. A
    /// page that fails to extract logs a warning and contributes no text,
    /// exactly as in the serial path.
    pub fn extract(&self, engine: &ContentEngine, options: &TextExtractOptions) -> Result<String> {
        let total_pages = engine.page_count()?;
        let page_list: Vec<usize> = match &options.pages {
            Some(list) => list.clone(),
            None => (1..=total_pages).collect(),
        };

        let formatter = TextFormatter::new();

        // Format a single page's text, or None for an out-of-range/failed page
        // (warning already logged). Shared by both the serial and parallel
        // paths so their output is identical by construction.
        let format_one = |page_num: usize| -> Option<String> {
            if page_num == 0 || page_num > total_pages {
                log::warn!("TextExtractor: page {} out of range, skipping", page_num);
                return None;
            }
            match self.extract_page(engine, page_num, options) {
                Ok((page_n, lines)) => Some(formatter.format_page(&lines, page_n, &options.format)),
                Err(e) => {
                    log::warn!("TextExtractor: page {} failed: {}", page_num, e);
                    None
                }
            }
        };

        let page_strings: Vec<Option<String>> = if page_list.len() >= PARALLEL_PAGE_THRESHOLD {
            // `par_iter().map(...).collect()` preserves input order, so pages
            // land in `page_strings` by their position in `page_list`.
            page_list.par_iter().map(|&p| format_one(p)).collect()
        } else {
            page_list.iter().map(|&p| format_one(p)).collect()
        };

        let mut all_text = String::new();
        for page_str in page_strings.into_iter().flatten() {
            all_text.push_str(&page_str);
        }

        Ok(all_text)
    }

    /// Extract and reconstruct text for a single page.
    pub fn extract_page(
        &self,
        engine: &ContentEngine,
        page_number: usize,
        options: &TextExtractOptions,
    ) -> Result<(usize, Vec<TextLine>)> {
        let ops = engine.get_page_content(page_number)?;
        let resources = engine.get_page_resources(page_number)?;

        let mut collector = TextCollector::new(resources, engine.document().reader());
        let chunks = collector.collect(&ops);

        let lines = options.reading_order.reconstruct(chunks);

        Ok((page_number, lines))
    }

    /// Convenience: extract text from all pages with default options.
    pub fn extract_default(engine: &ContentEngine) -> Result<String> {
        TextExtractor::new().extract(engine, &TextExtractOptions::default())
    }
}
