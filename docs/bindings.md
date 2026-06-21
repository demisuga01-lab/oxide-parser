# Bindings

Oxide now exposes the engine beyond Rust through a native C ABI and a
`wasm-bindgen` browser wrapper. Python is intentionally deferred to a focused
PyO3 follow-up.

## C ABI

Crate: `crates/oxide-capi`

Build:

```sh
cargo build -p oxide-capi
```

Header: `crates/oxide-capi/include/oxide.h`

The C API uses an opaque `OxideDocument *` handle and caller-owned return
buffers:

- `oxide_document_open_from_bytes`
- `oxide_document_page_count`
- `oxide_document_extract_text`
- `oxide_document_parse_markdown` — parse → canonical model → Markdown (RAG-facing)
- `oxide_document_parse_json` — parse → canonical `Document` JSON (schema 1.1)
- `oxide_document_extract_fields_json` — key-value fields → JSON (`doc_type`:
  null/`auto`/`invoice`/`receipt`/`form`/`generic`)
- `oxide_document_extract_semantic_json` — **legacy** (older semantic model;
  prefer `oxide_document_parse_json` for new code)
- `oxide_document_info_json`
- `oxide_document_render_page_png`
- `oxide_document_free`
- `oxide_string_free` / `oxide_error_free`
- `oxide_buffer_free`

`oxide_document_parse_markdown`, `oxide_document_parse_json`, and the WASM
`parseMarkdown` / `parseJson` bindings all emit the **same canonical `Document`
schema** the CLI `oxide parse` and the server `POST /api/v1/parse` produce, so
output is consistent across every surface. The parser ops over C are
digital-born only (OCR is not wired through the C ABI). Returned strings are
freed with `oxide_string_free`.

Every exported function catches Rust panics before the FFI boundary and returns
one of:

- `OXIDE_STATUS_OK`
- `OXIDE_STATUS_NULL`
- `OXIDE_STATUS_ERROR`
- `OXIDE_STATUS_PANIC`

The sample `crates/oxide-capi/examples/extract_text.c` opens a PDF from bytes,
extracts page 1 text, and frees all returned resources. The sample
`crates/oxide-capi/examples/parse_document.c` opens a PDF, prints the parsed
Markdown, and prints extracted key-value fields as JSON.

Verified on this host:

```bat
cargo test -p oxide-capi
cargo build -p oxide-capi
call "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64
cl /I crates\oxide-capi\include crates\oxide-capi\examples\extract_text.c /Fe:target\debug\oxide_capi_extract_text_example.exe /link target\debug\oxide_capi.dll.lib
target\debug\oxide_capi_extract_text_example.exe crates\engine\tests\fixtures\minimal.pdf

cl /I crates\oxide-capi\include crates\oxide-capi\examples\parse_document.c /Fe:target\debug\oxide_capi_parse_example.exe /link target\debug\oxide_capi.dll.lib
target\debug\oxide_capi_parse_example.exe crates\engine\tests\fixtures\form_160f.pdf
```

The `parse_document` example was run on `form_160f.pdf`: `parse_markdown`
emitted structured Markdown (headings, paragraphs, a recovered borderless-table
grid) and `extract_fields_json` returned 67 AcroForm fields with
`"doc_type":"form"` — exercising the full parser surface over the C ABI.

`cbindgen` was not installed on this machine, so the header is committed along
with `crates/oxide-capi/cbindgen.toml` for regeneration in environments that
have `cbindgen`.

## WASM

Crate: `crates/oxide-wasm`

Build verified:

```sh
rustup target add wasm32-unknown-unknown
cargo build -p oxide-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir crates/oxide-wasm/examples/browser/pkg target/wasm32-unknown-unknown/release/oxide_wasm.wasm
```

The wrapper exposes a JS class:

- `new OxidePdf(Uint8Array)`
- `pageCount()`
- `extractText(page)`
- `extractStructuredText(page)`
- `extractSemanticJson()`
- `infoJson()`
- `renderPagePng(page, dpi)` returning PNG bytes

The wrapper uses only in-memory engine operations. Server routes, job queues,
filesystem opening, and package-side temp-file workflows are excluded. The
engine dependency graph builds for wasm32 after enabling `getrandom`'s `js`
feature in the WASM crate; native engine defaults are unchanged. Rayon remains
in the engine dependency graph, but the WASM wrapper does not expose the
parallel all-pages extractor.

Browser example: `crates/oxide-wasm/examples/browser/index.html`

Prompt H browser verification:

```sh
cd crates/oxide-wasm/examples/browser
py -m http.server 8765
```

Headless Chrome, driven through `puppeteer-core`, loaded the local demo, selected
`tests/corpus/pdfs/generated/generated_basic_text.pdf`, extracted 116
characters, and rendered a nonblank 1020x1320 page-1 PNG. The only browser
console error was the static server's missing favicon. The in-app Browser tool
was unavailable in this session, so the live verification used standalone Chrome
instead.

## Python

Deferred. The intended follow-up is a separate `oxide-py` crate using PyO3 and
maturin with:

- `Document(bytes)` / `Document.from_path(path)`
- `page_count()`
- `extract_text(page)`
- `extract_semantic() -> dict`
- `info() -> dict`
- `render_page_png(page, dpi) -> bytes`

Errors should map to Python exceptions, and wheels should be built with maturin.
This was deliberately not half-implemented in this round so C-ABI and WASM could
ship cleanly.
