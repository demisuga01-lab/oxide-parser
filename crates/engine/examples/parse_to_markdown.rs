//! Embedding the document parser via the `prelude` — the curated high-level API.
//!
//! Shows the canonical parser path an embedder uses to turn a PDF into
//! structured outputs for RAG / automation: open → parse to the canonical
//! [`Document`] model → Markdown / JSON → RAG chunks → key-value fields. Every
//! call here is a `prelude` import; none of it needs the CLI or the server.
//!
//! Run with a PDF path (falls back to a bundled fixture so it always runs and
//! is built by `cargo test`, so it can't rot):
//!     cargo run --example parse_to_markdown -- path/to/input.pdf

use std::path::PathBuf;

use oxide_engine::prelude::*;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("tracemonkey.pdf")
        });
    println!("Opening {}", path.display());

    // 1. Open the document (no password here; use ContentEngine::open_path_with_password for encrypted PDFs).
    let engine = ContentEngine::open_path(&path)?;

    // 2. Parse into the canonical Document model. ParseOptions::default() parses
    //    all pages and strips page furniture from the body. To OCR scanned
    //    pages, set `ocr: Some(Arc::new(my_engine))` (e.g. the
    //    `oxide-ocr-tesseract` crate's TesseractEngine) — the model stays the
    //    same whether text is digital-born or OCR'd.
    let doc: Document = engine.parse_document(&ParseOptions::default())?;
    println!(
        "Parsed {} blocks across {} pages (schema {})",
        doc.body.len(),
        doc.pages.len(),
        doc.schema_version
    );

    // 3. Serialize. Markdown is the RAG/AI-facing rendering; JSON is the
    //    lossless model. Both are the SAME schema the CLI, C ABI, and WASM emit.
    let markdown = doc.to_markdown_default();
    let _json = doc.to_json();
    let preview: String = markdown.lines().take(8).collect::<Vec<_>>().join("\n");
    println!("\n--- Markdown (first 8 lines) ---\n{preview}\n");

    // 4. RAG-ready semantic chunks (structure-aware, token-sized, with overlap).
    let chunks = doc.chunk(&ChunkOptions::default());
    println!("Chunked into {} RAG chunks", chunks.chunks.len());

    // 5. Structured key-value fields (invoice/receipt/form auto-detected).
    let fields: ExtractedFields = engine.extract_fields(&ExtractOptions::default())?;
    println!(
        "Detected doc type {:?} with {} field(s)",
        fields.doc_type,
        fields.fields.len()
    );

    Ok(())
}
