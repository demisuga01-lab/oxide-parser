# Attack Surface Map

This map enumerates untrusted entry points and the controls that defend them.

## Rust Library

| Entry Point | Input | Controls |
| --- | --- | --- |
| `ContentEngine::open_bytes/open_path` | Untrusted PDF bytes/files | Parser errors are classified, no JS execution, fuzz/property/corpus coverage. |
| Text extraction, document model, chunks, fields | Parsed PDF content streams, fonts, geometry | Bounded page operations, semantic/layout tests, differential/property checks. |
| Rendering | Page graphics, fonts, images, forms, transparency | Pixel caps, DPI caps, hostile render tests, image/font fuzz targets, grammar-aware `structured_pdf` target. |
| Structural writer/rewrite/optimize/repair/linearize | Parsed object graph | Writer fuzz targets, qpdf checks, property writer-mode invariants, linearization checks. |
| Editing/redaction/form flattening | Existing PDFs plus edit operations | Full rewrite/incremental tests, editing fuzz target, redaction extraction tests. |
| PDF/A/UA validation/conversion | Parsed PDFs and metadata | Compliance tests, PDF/A fuzz target, veraPDF-oriented docs/tests. |
| Signature validation | Signed PDFs, CMS/X.509/DSS-like data | Signature fuzz target, tamper tests, ByteRange checks, LTV fixture tests. |
| Signing/encryption | Caller-supplied keys/passwords | RustCrypto crates, OS CSPRNG via `getrandom`, constant-time password verifier comparisons, no key logging. |

## CLI

The `oxide` CLI accepts file paths and writes output files. It inherits library
resource controls and returns process errors instead of panics for malformed
inputs. Shell command injection is not used for PDF processing.

## C ABI

The C ABI is the only place with Rust `unsafe` pointer boundaries:

- `oxide_document_open_from_bytes`
- `oxide_document_free`
- `oxide_string_free`
- `oxide_error_free`
- `oxide_buffer_free`
- page count, extraction, render, parse, and field accessors

Defenses:

- Null pointers are checked.
- Input bytes are copied before parsing, so caller-owned buffers are not kept.
- Returned buffers/strings have explicit free functions.
- C ABI tests cover null handling, open/count/extract/free, parse JSON, and
  field extraction.

Residual risk: callers must pair returned pointers with the matching free
function and must not double-free or pass invalid pointers.

## WASM

The WASM crate exposes browser/JS-callable PDF operations. Inputs are untrusted
bytes from JS. It inherits the Rust engine's memory-safe parser and resource
caps, but browser memory limits are host-controlled.

## Server

Network-facing endpoints accept untrusted uploads:

- `/api/v1/extract-text`
- `/api/v1/extract-images`
- `/api/v1/analyze`
- `/api/v1/pdf2img`
- parse/chunk/field operations where enabled by route modules

Defenses:

- API key auth fails closed by default.
- Constant-time API-key comparison.
- Restrictive CORS default.
- File-size, page, DPI, render-pixel, output-size, and timeout limits.
- Rate limiting with bounded state.
- Sanitized client errors and correlation IDs for internal failures.

## OCR Subprocess

OCR uses an explicit backend such as Tesseract. The SDK writes bounded temporary
images and invokes the configured binary. The binary path and language data are
deployment-controlled.

## Unsafe Inventory

The core engine contains no `unsafe` blocks. Unsafe usage is isolated to
`crates/oxide-capi/src/lib.rs` for FFI pointer conversion and ownership transfer.
Tests exercise the C ABI null/error/free paths.
