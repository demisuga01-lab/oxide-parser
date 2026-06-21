# Oxide WASM Browser Demo

Build the raw wasm and browser glue:

```sh
cargo build -p oxide-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web \
  --out-dir crates/oxide-wasm/examples/browser/pkg \
  target/wasm32-unknown-unknown/release/oxide_wasm.wasm
```

Alternatively, `wasm-pack` can produce the same web package:

```sh
wasm-pack build crates/oxide-wasm --target web --out-dir examples/browser/pkg
```

Then serve `crates/oxide-wasm/examples/browser` with any static server and open
`index.html`. The demo runs entirely in the browser: it reads a selected PDF as
a `Uint8Array`, opens it with `OxidePdf`, then parses it to Markdown, splits it
into RAG chunks, extracts key-value fields, extracts page text, and renders
page 1 to PNG bytes for display. Nothing is uploaded — the document never leaves
the browser tab.

Verified in Prompt H on this host:

- `cargo build -p oxide-wasm --target wasm32-unknown-unknown --release` passed.
- `wasm-bindgen --target web` generated `examples/browser/pkg`.
- A local static server plus headless Chrome loaded `index.html`, selected
  `tests/corpus/pdfs/generated/generated_basic_text.pdf`, extracted 116
  characters, and rendered a nonblank 1020x1320 PNG image. The only console
  error was the browser's automatic missing-favicon request.

Scope: the WASM wrapper exposes open-from-bytes, page count, the **document
parser** (`parseMarkdown`, `parseJson` — the canonical schema-1.1 `Document`
model, identical to what the CLI `parse` and the server `/parse` emit),
RAG `chunk`, key-value `extractFieldsJson`, plain/structured text extraction,
info JSON, and render-page-to-PNG. The legacy `extractSemanticJson` (the older
semantic model) is retained for back-compat — prefer `parseJson` for new code.

OCR is intentionally **excluded** in the browser: the Tesseract backend is an
external process, so the WASM surface is **digital-born only** — scanned pages
degrade to a placeholder. It also does not expose server endpoints, filesystem
batch tools, async jobs, C/Python bindings, or multi-threaded rayon execution.

Verified additionally for the parser surface: `cargo build -p oxide-wasm
--target wasm32-unknown-unknown` compiles the new `parseMarkdown` / `parseJson`
/ `chunk` / `extractFieldsJson` bindings. (`wasm-bindgen`/`wasm-pack` are not
installed on the current host, so the regenerated `pkg/` glue and a fresh
headless-browser run were not re-executed this round; the prebuilt `pkg/`
predates these bindings and must be regenerated with the commands above before
the new methods appear in `oxide_wasm.js`.)
