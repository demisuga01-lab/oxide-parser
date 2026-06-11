use super::collector::TextCollector;
use super::formatter::{TextFormatOptions, TextFormatter};
use super::reading_order::{ReadingOrderReconstructor, TextLine};
use crate::engine::ContentEngine;
use crate::error::Result;

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
    pub fn extract(&self, engine: &ContentEngine, options: &TextExtractOptions) -> Result<String> {
        let total_pages = engine.page_count()?;
        let page_list: Vec<usize> = match &options.pages {
            Some(list) => list.clone(),
            None => (1..=total_pages).collect(),
        };

        let formatter = TextFormatter::new();
        let mut all_text = String::new();

        for page_num in page_list {
            if page_num == 0 || page_num > total_pages {
                log::warn!("TextExtractor: page {} out of range, skipping", page_num);
                continue;
            }

            let page_text = self.extract_page(engine, page_num, options);
            match page_text {
                Ok((page_n, lines)) => {
                    let page_str = formatter.format_page(&lines, page_n, &options.format);
                    all_text.push_str(&page_str);
                }
                Err(e) => {
                    log::warn!("TextExtractor: page {} failed: {}", page_num, e);
                }
            }
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
