#[cfg(target_arch = "wasm32")]
mod wasm_api {
    use wasm_bindgen::prelude::*;

    use oxide_engine::{ChunkOptions, ContentEngine, DocType, ExtractOptions, ParseOptions};

    #[wasm_bindgen]
    pub struct OxidePdf {
        engine: ContentEngine,
    }

    #[wasm_bindgen]
    impl OxidePdf {
        #[wasm_bindgen(constructor)]
        pub fn new(bytes: &[u8]) -> Result<OxidePdf, JsValue> {
            install_panic_hook();
            let engine = ContentEngine::open_bytes(bytes.to_vec()).map_err(js_err)?;
            Ok(Self { engine })
        }

        #[wasm_bindgen(js_name = pageCount)]
        pub fn page_count(&self) -> Result<usize, JsValue> {
            self.engine.page_count().map_err(js_err)
        }

        #[wasm_bindgen(js_name = extractText)]
        pub fn extract_text(&self, page: usize) -> Result<String, JsValue> {
            self.engine.get_page_text(page).map_err(js_err)
        }

        #[wasm_bindgen(js_name = extractStructuredText)]
        pub fn extract_structured_text(&self, page: usize) -> Result<String, JsValue> {
            self.engine.get_page_text_structured(page).map_err(js_err)
        }

        #[wasm_bindgen(js_name = extractSemanticJson)]
        pub fn extract_semantic_json(&self) -> Result<String, JsValue> {
            let semantic = self.engine.extract_semantic_document(&[]).map_err(js_err)?;
            serde_json::to_string(&semantic).map_err(|err| JsValue::from_str(&err.to_string()))
        }

        /// Parse the whole document into the canonical model and render it as
        /// clean, RAG-ready Markdown (headings/paragraphs/lists/tables/figures
        /// in recovered reading order). This is the digital-born parser surface
        /// — scanned pages degrade to a placeholder (OCR is not available
        /// in-browser; it requires the external Tesseract process).
        #[wasm_bindgen(js_name = parseMarkdown)]
        pub fn parse_markdown(&self) -> Result<String, JsValue> {
            let doc = self
                .engine
                .parse_document(&ParseOptions::default())
                .map_err(js_err)?;
            Ok(doc.to_markdown_default())
        }

        /// Parse the whole document into the canonical [`Document`] model and
        /// return it as JSON. This is the SAME schema the CLI `parse --format
        /// json` and the server `/parse` endpoint emit (schema 1.1) — distinct
        /// from the legacy `extractSemanticJson`, which serializes the older
        /// semantic model and is kept only for back-compat.
        #[wasm_bindgen(js_name = parseJson)]
        pub fn parse_json(&self) -> Result<String, JsValue> {
            let doc = self
                .engine
                .parse_document(&ParseOptions::default())
                .map_err(js_err)?;
            Ok(doc.to_json())
        }

        /// Parse the document and split it into RAG-ready semantic chunks
        /// (structure-aware, token-sized, with overlap and heading context).
        /// Returns the `ChunkSet` as JSON. Pass `0` for `target_tokens` or
        /// `overlap` to use the defaults (512 / 64).
        #[wasm_bindgen(js_name = chunk)]
        pub fn chunk(&self, target_tokens: usize, overlap: usize) -> Result<String, JsValue> {
            let doc = self
                .engine
                .parse_document(&ParseOptions::default())
                .map_err(js_err)?;
            let mut opts = ChunkOptions::default();
            if target_tokens > 0 {
                opts.target_tokens = target_tokens;
            }
            if overlap > 0 {
                opts.overlap_tokens = overlap;
            }
            Ok(doc.chunk(&opts).to_json())
        }

        /// Extract structured key-value fields (invoice number/date/total,
        /// receipt merchant/amount, form label→value pairs, line items) as
        /// JSON. `doc_type` is one of `auto` (default), `invoice`, `receipt`,
        /// `form`, or `generic`; an unrecognized value falls back to `auto`.
        /// Digital-born only in-browser (no OCR).
        #[wasm_bindgen(js_name = extractFieldsJson)]
        pub fn extract_fields_json(&self, doc_type: &str) -> Result<String, JsValue> {
            let opts = ExtractOptions {
                doc_type: DocType::parse(doc_type),
                ..Default::default()
            };
            let fields = self.engine.extract_fields(&opts).map_err(js_err)?;
            Ok(fields.to_json())
        }

        #[wasm_bindgen(js_name = infoJson)]
        pub fn info_json(&self) -> Result<String, JsValue> {
            let info = self.engine.document_info().map_err(js_err)?;
            serde_json::to_string(&info).map_err(|err| JsValue::from_str(&err.to_string()))
        }

        #[wasm_bindgen(js_name = renderPagePng)]
        pub fn render_page_png(&self, page: usize, dpi: u32) -> Result<Vec<u8>, JsValue> {
            self.engine.render_page_png_fast(page, dpi).map_err(js_err)
        }
    }

    fn js_err(err: oxide_engine::OxideError) -> JsValue {
        JsValue::from_str(&err.to_string())
    }

    fn install_panic_hook() {
        #[cfg(feature = "panic-hook")]
        console_error_panic_hook::set_once();
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct OxideWasmBuildsOnlyForWasm32;
