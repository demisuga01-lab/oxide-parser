# Oxide Public API Overview

This is the supported entry-point map for integrators. Prefer
`oxide_engine::prelude::*` for application code. The crate root still re-exports
low-level building blocks for advanced consumers, but those are a wider surface
and may move while the crate is `0.x`.

## Stable Rust Surface

| Capability | Entry points |
| --- | --- |
| Open/read PDFs | `ContentEngine::open_path`, `ContentEngine::open_bytes`, `PdfDocument` |
| Parse to canonical document model | `ContentEngine::parse_document`, `parse`, `ParseOptions`, `Document` |
| Text extraction | `ContentEngine::get_page_text`, `TextExtractOptions` |
| RAG chunking | `Document::chunk`, `chunk`, `ChunkOptions`, `ChunkSet` |
| Key-value fields | `ContentEngine::extract_fields`, `extract_fields`, `ExtractOptions` |
| Rendering | `ContentEngine::render_page_png_fast`, `render_page_svg` |
| Authoring | `PdfBuilder`, `PdfPageBuilder`, `FlowDocument`, `TextStyle`, `GraphicsStyle` |
| Editing | `PdfEditor`, `WatermarkOptions`, `HeaderFooterOptions`, `RedactionOptions` |
| Structural ops | `build_subset`, `build_merged`, `rotate_pages`, `optimize`, `repair`, `encrypt`, `linearize` |
| PDF/A and PDF/UA | `validate_pdfa`, `convert_to_pdfa`, `validate_pdfua`, `PdfAProfile` |
| Signatures | `ContentEngine::sign`, `ContentEngine::add_ltv_material`, `sign_document`, `add_ltv_material`, `PdfSigner`, `verify_signatures` |
| Errors | `Result<T>`, `OxideError`, `ErrorKind`, `OxideError::code()` |

## Bindings

| Surface | Status | Docs |
| --- | --- | --- |
| CLI | Stable command names for common operations | `oxide --help`, README |
| C ABI | Stable exported C symbols in committed header | `docs/bindings.md` |
| WASM | Stable browser parse/render wrapper for digital-born PDFs | `docs/bindings.md` |
| HTTP server | Stable `/api/v1/*` JSON endpoints; job API documented separately | `docs/self_hosting.md`, `docs/jobs.md` |

## Experimental / Low-Level Surface

These modules are public for advanced use and tests but are not the preferred
integration contract while the crate is `0.x`: `content`, `filters`, `fonts`,
`images`, `object`, `parser`, `reader`, `render`, and `writer`.

Use them when you need PDF internals. For application integrations, start with
`prelude` and `ContentEngine`.

## Error Handling

Every public operation returns `oxide_engine::Result<T>`. For programmatic
handling:

```rust
use oxide_engine::{ErrorKind, OxideError};

fn classify(err: &OxideError) -> &'static str {
    match err.kind() {
        ErrorKind::Encrypted => "ask for a password",
        ErrorKind::UnsupportedFeature => "show an unsupported-feature message",
        ErrorKind::ResourceLimit => "ask for a smaller request",
        _ => err.code(),
    }
}
```

Library code should return `OxideError` rather than panicking on malformed input.
Panic catching at C/server boundaries is documented in `docs/bindings.md` and
`docs/security.md`.
