# Oxide

<p align="center">
  <img src="docs/assets/oxide-github-hero.svg" alt="Oxide — pure-Rust PDF engine" width="100%" />
</p>

**A pure-Rust PDF engine.** Parse PDFs into structured Markdown, JSON, semantic HTML, RAG chunks, and key-value fields — and author, edit, sign, structurally transform, and validate them in the same engine. One static binary. Four embed surfaces. No Python runtime, no C++ render stack, no per-page API.

```text
  PDF ─► parse  ─► Document { blocks, tables, fields, figures, geometry }
                 ─► Markdown · JSON · HTML · Chunks · Fields
        author  ─► new PDF (FlowDocument, fonts, tables, images)
        edit    ─► watermarks · redaction · forms · annotations
        sign    ─► signed PDF (RSA/SHA-256, ByteRange, LTV substrate)
        validate─► PDF/A-1b/2b/2a/3b/3a · PDF/UA (best-effort)
```

Oxide is built for teams that need a **self-hostable, memory-safe, embeddable** PDF stack — extraction, transformation, and governance in one Rust core, with no per-page cloud fees and no Python interpreter to ship alongside.

## By the numbers

Provenance: [`docs/oxide_sdk.md`](docs/oxide_sdk.md) (capstone, 2026-06-22, Windows 11, 20-core), [`docs/parser_benchmark.md`](docs/parser_benchmark.md), [`docs/security/posture.md`](docs/security/posture.md). Every figure is reproducible from `extraction-benchmark/`, `renderer-benchmark/`, and `scripts/`.

| Metric | Oxide | Reference | Source |
| --- | ---: | --- | --- |
| Process cold start | **~7.5 ms** | 158.7 ms (Python + PyMuPDF import) | capstone |
| Static binary size | **~12.8 MB** | Python runtime + C-extensions | capstone |
| Reading order (multi-column report) | **1.000** Kendall-tau | n/a — Poppler/PyMuPDF aren't order-aware | benchmark |
| Clean digital table cell-F1 / TEDS | **1.000 / 1.000** | 1.000 / 1.000 (PyMuPDF) | benchmark |
| Invoice key-value field-F1 (digital) | **1.000** | not a feature in PyMuPDF/Poppler | benchmark |
| OCR'd scan char-accuracy (paper) | **0.942** | 0.000 (no OCR in PyMuPDF/Poppler) | benchmark |
| Renderer weighted score (Prompt 3, 265-file slice) | **91.82** | Poppler reference | renderer-benchmark |
| Hostile cross-pillar sweep | **0 crashes, 0 timeouts** (1,590 ops) | n/a | GA5 sweep |
| PDF/A-1b/2b/2a/3b/3a | **PASS** (qpdf + veraPDF 1.30.2) | — | capstone |

The renderer is preview/OCR-grade — fast and crash-safe, but it does not match Poppler/PDFium pixel fidelity. See [What it's not (yet)](#what-its-not-yet).

## Why Oxide

- **Pure-Rust core, single static binary.** Memory-safe against the buffer-overflow / use-after-free / type-confusion classes; no Python, no torch stack, no native C++ rendering dependency. One ~12.8 MB binary, ~7.5 ms cold start, zero runtime.
- **One canonical model, every surface.** `parse` produces a schema-versioned `Document` (headings, paragraphs, lists, tables, figures, captions in recovered reading order, with per-page geometry and provenance). The **CLI, Rust library, C ABI, WASM, and HTTP server emit the same schema** — byte-identical extraction across all four surfaces on the same page (see [Surface consistency](#surface-consistency)). Parse once, consume anywhere.
- **Structured extraction, not text dumps.** Markdown for RAG, JSON for lossless round-trip, semantic HTML, structure-aware RAG chunks (target token size, overlap, heading context), and key-value fields (invoice / receipt / form, with line items). Optional Tesseract OCR via an injected `OcrEngine` — OCR'd text flows through the same model.
- **qpdf-class structural operations.** Merge, split, extract-pages, rotate, encrypt (AES-256 default), optimize, repair, and qpdf-clean linearization for the supported subset. `qpdf --check` cross-validates Oxide's split output and page counts agree.
- **Compliance and signatures.** PDF/A-1b / 2b / 2a / 3b / 3a validation and conversion (veraPDF 1.30.2 PASS); RSA / SHA-256 signing with ByteRange coverage and incremental update; offline PAdES B-T / B-LT substrate (DSS, VRI, CRL, OCSP, timestamp tokens).
- **Embeddable four ways.** Rust library (`oxide-engine`), CLI (`oxide`), C ABI (`oxide-capi`), WebAssembly (`oxide-wasm`, digital-born in the browser), self-hostable HTTP server (`oxide-server`).
- **Self-hostable and private.** Documents never leave your hardware — no per-page cloud fees and no Python runtime to ship alongside. See [`docs/self_hosting.md`](docs/self_hosting.md).
- **MIT OR Apache-2.0.** Permissive, non-copyleft — drop-in for commercial products, no GPL-family contagion.

## Features

| Group | Capability |
| --- | --- |
| **Extract** | Plain text · structured Markdown · lossless JSON · semantic HTML · RAG chunks (token-sized, heading-aware, with overlap) · key-value fields (invoice / receipt / form, line items) · tables (CSV / JSON / HTML) · images (ZIP) · attachments |
| **Parse model** | Reading order (XY-cut) · semantic tagged-PDF path · per-page geometry · block-type classification · figures with captions · dehyphenation · ligature normalization · provenance annotations |
| **Create** | Programmatic authoring (`FlowDocument` / `PdfBuilder`) · pages, text, vector graphics, images, tables, single-column flow layout · TrueType font embedding · standard 14 fonts |
| **Edit** | Watermarks · overlays / underlays · headers / footers · annotations · redaction (with extract-back verification) · AcroForm fill and flatten · incremental updates |
| **Secure** | AES-256 / AES-128 / RC4 encryption · permission flags · RSA / SHA-256 signing · ByteRange coverage · LTV substrate (DSS, VRI, CRL, OCSP, timestamp tokens) · PAdES B-B / B-T / B-LT |
| **Convert / validate** | PDF/A-1b / 2b / 2a / 3b / 3a (veraPDF PASS) · PDF/UA assistive validation · linearization (qpdf-clean) · optimize (recompress) · repair (qpdf-clean normalize) |
| **Render** | PNG / JPG / WebP / SVG / PostScript / EPS · `compat` (Poppler-equivalent) or `high` (linear-light) compositing · DPI and pixel caps · hostile-input safe |
| **OCR** | Pluggable `OcrEngine` trait · bundled Tesseract backend (off by default; drives the external `tesseract` process, no linked C) · routed through the same parse pipeline |
| **Surfaces** | Rust library · CLI · C ABI · WebAssembly · self-hostable HTTP server (auth, rate limits, resource caps, async job queue) |

## Quick start

### CLI

```sh
# Build the single-binary CLI (add --features full for OCR):
cargo build --release -p oxide-cli

# Digital-born document → clean Markdown / JSON for RAG or automation:
oxide parse input.pdf --format markdown > input.md
oxide parse input.pdf --format json --output input.json

# RAG-ready structure-aware chunks; structured key-value fields:
oxide chunk input.pdf --target-tokens 512 --output chunks.json
oxide extract-fields input.pdf --type invoice --output fields.json

# Tables, text, images, metadata, layout:
oxide extract-tables input.pdf --format csv
oxide extract-text input.pdf --structured --format json
oxide extract-images input.pdf --output images.zip
oxide info input.pdf
oxide fonts input.pdf
oxide analyze input.pdf                    # has text layer? scanned?

# Structural and compliance operations (qpdf-validated):
oxide optimize input.pdf -o optimized.pdf
oxide linearize input.pdf -o fast-web-view.pdf
oxide encrypt input.pdf -o encrypted.pdf --user-pw change-me   # AES-256 default
oxide verify-sig signed.pdf
oxide merge a.pdf b.pdf -o merged.pdf
oxide split input.pdf -o page-%d.pdf
```

Pair with external validation when you want a second pair of eyes:

```sh
qpdf --check fast-web-view.pdf
verapdf --format text compliant.pdf
```

### Rust library

```toml
# Cargo.toml
[dependencies]
oxide-engine = "0.1"
```

```rust
use oxide_engine::prelude::*;

fn main() -> Result<()> {
    // Open + parse to the canonical Document model.
    let engine = ContentEngine::open_path("input.pdf")?;
    let doc = engine.parse_document(&ParseOptions::default())?;

    // Serialize: Markdown (RAG/AI), lossless JSON, semantic HTML.
    println!("{}", doc.to_markdown_default());
    let _json = doc.to_json();

    // RAG chunks and key-value fields on the same model.
    let chunks = doc.chunk(&ChunkOptions::default());
    let fields = engine.extract_fields(&ExtractOptions::default())?;
    println!("{} chunks, {} fields on a {:?} doc",
        chunks.chunks.len(), fields.fields.len(), fields.doc_type);
    Ok(())
}
```

Authoring and signing live in the same engine via the example binaries:

```sh
# Build a PDF from scratch (FlowDocument, fonts, tables, images).
cargo run --example authoring -- target/authored.pdf

# Sign an existing PDF (RSA / SHA-256, ByteRange coverage, LTV substrate).
cargo run --example sign_document -- \
    input.pdf private-key.pem signer-cert.pem signed.pdf

# Validate + convert to PDF/A (1b/2b/2a/3b/3a) and PDF/UA best-effort.
cargo run --example compliance -- target
```

> The authoring and signing surfaces are library-first — there is intentionally no `oxide create` / `oxide sign` CLI command. Embedding keeps the API expressive and the binary small; the CLI is for the ops a human or a cron job actually runs.

### C ABI, WebAssembly, HTTP server

- **C ABI** — `oxide-capi` (cdylib + staticlib). Stable exported symbols in the committed header. See [`docs/bindings.md`](docs/bindings.md).
- **WebAssembly** — `oxide-wasm` (wasm-bindgen). In-browser `parseMarkdown()` / `parseJson()` / `chunk()` / `extractFieldsJson()` for digital-born PDFs. OCR is not in the browser build.
- **HTTP server** — `oxide-server` (axum). Self-hostable `POST /api/v1/parse` / `/chunk` / `/extract-fields` / `/info`, with fail-closed API-key auth, rate limits, resource caps, and an async job queue. See [`docs/self_hosting.md`](docs/self_hosting.md) and [`docs/jobs.md`](docs/jobs.md).

## Use cases

- **RAG and LLM ingestion.** Clean Markdown + token-sized, heading-aware chunks, run locally with no per-page cloud fees. The same `Document` feeds chunkers, fields, and downstream agents.
- **Document automation.** Invoices, receipts, forms → JSON (field-F1 1.0 on the digital invoice, honest 0.4 on the scanned variant). Line items extracted as a first-class structure, not text matching.
- **Embedding into Rust, C, or browser apps.** One engine, four surfaces, one schema, no Python/torch runtime to ship alongside.
- **Compliance pipelines.** Generate, validate, and convert PDF/A-1b/2b/2a/3b/3a; pair Oxide's `validate_pdfa` with veraPDF for an independent check.
- **Self-hosted privacy.** Invoices, contracts, medical records, legal discovery — documents never leave your hardware, no per-document API fees.
- **Long-term signed archives.** RSA / SHA-256 signing with offline PAdES B-T / B-LT substrate (DSS, VRI, CRL, OCSP, timestamp tokens); signer and verifier both pure-Rust.

## Benchmarks (measured, not aspirational)

### Extraction quality — vs PyMuPDF, pdftotext

Char-accuracy = `1 − CER`; reading order = normalized Kendall-tau (1.0 = perfect). Scanned rows use Oxide's OCR path; PyMuPDF / Poppler have no OCR and recover nothing.

| Document | Mode | Oxide char-acc | PyMuPDF | pdftotext | Oxide order |
| --- | --- | ---: | ---: | ---: | ---: |
| figure | digital | 0.598 | 0.990 | 0.833 | 1.000 |
| paper | digital | 0.993 | 0.998 | 0.956 | 1.000 |
| paper_scanned | scanned | **0.942** | 0.000 | 0.000 | 1.000 |
| report_multicol | digital | 0.605 | 0.669 | 0.596 | **1.000** |
| tables | digital | **1.000** | 0.877 | 0.088 | 1.000 |
| tables_scanned | scanned | 0.632 | 0.000 | 0.000 | 1.000 |

Key-value field-F1 (PyMuPDF / Poppler do not do KV extraction — Oxide-only capability):

| Document | Mode | Oxide F1 | Precision | Recall |
| --- | --- | ---: | ---: | ---: |
| invoice | digital | **1.000** | 1.000 | 1.000 |
| invoice_scanned | scanned | 0.400 | 0.375 | 0.429 |
| receipt | digital | 0.750 | 1.000 | 0.600 |

`qpdf` cross-validates Oxide's structural output: split parts pass `qpdf --check`, page counts agree, linearized output passes `qpdf --check` and `qpdf --show-linearization`.

### Renderer fidelity (Prompt 3, 265-file slice)

| Metric | Result |
| --- | ---: |
| Weighted score | **91.82** |
| Visual-page pass | 86.94% |
| File pass | 88.68% |
| Hostile crash / timeout / memory safety | **100% / 100% / 100%** |
| Determinism sample (24/24 stable) | 100% |
| Peak Oxide memory | 98.44 MB |
| Median Poppler / Oxide speed ratio | 1.91x |

Weakest real-world categories (honest disclosure, not polished away): RTL 40.00%, scanned 44.44%, multi-column 47.06%, forms 57.14%, CJK 61.54%, complex vector 80.00%.

### Cross-pillar hostile sweep (GA5)

| Metric | Result |
| --- | ---: |
| Files | 265 |
| Operations | 1,590 |
| Crashes | **0** |
| Timeouts | **0** |
| Crash-free | 100.0% |
| Timeout-free | 100.0% |
| Invalid transformed outputs from qpdf-clean inputs | **0** |

The sweep covers `info`, `parse`, `verify-sig`, first-page `render`, `optimize`, and `linearize` with a 60-second per-operation cap.

### Operation smoke (single-doc, release build, best of 3)

| Operation | Best ms | Peak MB |
| --- | ---: | ---: |
| Parse to JSON (CLI) | 89.3 | 19.0 |
| Extract text (CLI) | 37.2 | 20.0 |
| Render PNG ZIP (CLI) | 56.2 | 18.6 |
| Authoring example | 22.4 | 7.7 |
| PDF/A conversion example | 21.5 | 9.1 |
| RSA signing example | 11.9 | 5.1 |
| Optimize (CLI) | 9.2 | 6.1 |
| Linearize (CLI) | 12.6 | 6.9 |
| Encrypt AES-256 (CLI) | 15.6 | 6.6 |

These are smoke operation benchmarks, not statistically rigorous throughput claims.

### External validation

| Output | External check | Result |
| --- | --- | --- |
| PDF/A-1b / 2b / 2a / 3b / 3a | veraPDF 1.30.2 | **PASS** |
| AES-256 encrypted | `qpdf --check` | Clean (AESv3) |
| Linearized | `qpdf --check`, `--show-linearization` | Clean |
| Optimized | `qpdf --check` | Clean |
| Signed (RSA / SHA-256) | `qpdf --check`, Oxide `verify-sig` | Cryptographically valid, whole-file coverage |
| Authored | `qpdf --check`, Poppler render/extract | Clean |

Reproduce with `python renderer-benchmark/scripts/renderer_benchmark.py` and `python extraction-benchmark/scripts/extraction_benchmark.py`.

## Surface consistency

`parse` is the same operation whether you call it from the Rust library, the CLI, the C ABI, or the HTTP server. The capstone integration test hashes the extracted text of `basicapi.pdf` page 1 across all four surfaces; the SHA-256 matches in every case. See the [Surface consistency table](docs/oxide_sdk.md#integration-results) in the capstone.

## What it's not (yet)

Honesty is the product. Oxide is a credible v1 candidate; it is not a panacea, and pretending otherwise would burn the trust of every serious evaluator.

- **Renderer is preview / OCR-grade, not pixel-proof.** Prompt 3 reaches a 91.82 weighted score and 86.94% visual pass on the 265-file hostile slice, with 100% hostile crash/timeout/memory safety. It still trails Poppler / MuPDF / PDFium for visual fidelity. The renderer exists to feed OCR, previews, and regression checks; if you need a pixel-perfect "this PDF will print exactly as displayed" guarantee, render with Poppler / PDFium and use Oxide for everything else.
- **OCR is Tesseract, not an ML layout model.** Bounded by the Tesseract engine and scan quality. Scanned tables don't reconstruct as clean cell grids (cell-F1 0 on `tables_scanned`); the OCR path emits prose blocks, and the grid detector needs ruling-line graphics it can't see on a scan. For scanned tabular data, use `extract-fields --ocr` to recover values and line items. Docling's ML layout would likely do better on messy scans; Docling is not benchmarked locally and is not part of this binary.
- **No external security audit yet.** Continuous fuzzing, differential checks, property tests, grammar-aware deep fuzzing, dependency auditing, and a cross-pillar hostile sweep are all live and green — but a paid third-party audit is the next trust-builder, especially for the signature and encryption surfaces. See [`docs/security/posture.md`](docs/security/posture.md).
- **Signature LTV is offline-first.** Core signing + offline timestamp / DSS / CRL substrate works. Live TSA / OCSP fetching, system trust-store policy, PAdES-B-LTA archive-timestamp refresh, and ECDSA breadth remain deployment-sensitive follow-ups.
- **PDF/UA is best-effort.** Assistive tagging is emitted, but full accessibility certification still requires manual semantic review.
- **Per-call CLI latency** includes process spawn; for many-small-doc throughput in a long-lived process, embed the `oxide-engine` library to erase the spawn cost.

## Architecture

| Crate | Role |
| --- | --- |
| `oxide-engine` | The Rust core: parse, extract, author, edit, render, sign, validate. |
| `oxide-cli` | The `oxide` binary — every command is a thin adapter over the engine. |
| `oxide-server` | Self-hostable axum HTTP server with auth, rate limits, resource caps, async jobs. |
| `oxide-capi` | C ABI wrapper (`cdylib` + `staticlib`). |
| `oxide-wasm` | `wasm-bindgen` wrapper for in-browser digital-born extraction. |
| `oxide-ocr-tesseract` | Tesseract OCR backend (drives the external `tesseract` process, no linked C). |

One `Document` model flows through every surface. The server is intentionally **non-mutating** unless a route explicitly documents otherwise — a real safety property for a service ingesting untrusted PDFs.

## Install / build

Stable Rust toolchain (edition 2021). No `rust-version` pin yet — current stable.

```sh
rustup update stable

# Engine (the library):
cargo build --release -p oxide-engine

# CLI (digital-born, no OCR):
cargo build --release -p oxide-cli

# CLI with OCR (Tesseract must be installed and on PATH):
cargo build --release -p oxide-cli --features full   # CLI's `full` = ["ocr"]

# Server, C ABI, WASM:
cargo build --release -p oxide-server
cargo build --release -p oxide-capi
cargo build --release -p oxide-wasm
```

### Engine feature flags (`oxide-engine`)

`default = ["parse", "render", "structural"]`; `full` enables everything.

| Flag | Pulls in |
| --- | --- |
| `parse` (default) | Parser, `Document` model, text / field / table extraction |
| `render` (default) | PNG / SVG / PostScript / EPS rendering |
| `structural` (default) | Merge, split, encrypt, optimize, linearize, repair |
| `extract` | Structured field / table / image extraction on top of `parse` |
| `create` | `PdfBuilder` / `FlowDocument` authoring |
| `edit` | Watermarks, redaction, form fill, annotations (pulls in `create` + `structural`) |
| `sign` | RSA signing + verification + LTV substrate |
| `pdfa` | PDF/A validation + conversion and PDF/UA checks |
| `ocr` | Plugs the `OcrEngine` trait into the parse pipeline |
| `fuzzing` | Exposes internal parse/decode entry points for the fuzz workspace |
| `full` | All of the above |

Pull only what you need. The default build gives you parse + render + structural ops; add `extract` for fields and chunks, `create` to author, `sign` for signing, `pdfa` for compliance.

## Verification and security

- Unit, integration, and doc tests across the workspace (`cargo test --workspace`).
- Clippy clean under `-D warnings` (`cargo clippy --workspace --all-targets -- -D warnings`).
- 16 private fuzz corpora, replayed on every push, plus a `structured_pdf` grammar-aware target reaching content interpretation, renderer, editing, linearization, PDF/A, and signature paths.
- Differential fuzzing vs qpdf and Poppler for page count, structural validity, text similarity, and writer round-trip.
- Property tests for round-trip identities, writer-mode equivalence, AES-256 preserve-content, and no-panic arbitrary bytes.
- Cross-pillar hostile sweep: 265 files, 1,590 operations, 0 crashes, 0 timeouts.
- PDF encryption secrets use zeroizing wrappers; the engine crate enforces `#![forbid(unsafe_code)]`.
- Linux sanitizer CI covers ASan/TSan/Rust UB checks for C-ABI/crypto tests plus ASan fuzz corpus replay.
- `cargo audit` and `cargo deny` clean against the documented `RUSTSEC-2023-0071` (RustCrypto `rsa 0.9.10`) advisory exception.

Full evidence and residual-risk list: [`docs/security/posture.md`](docs/security/posture.md). Threat model: [`docs/security/threat_model.md`](docs/security/threat_model.md). Disclosure: [`SECURITY.md`](SECURITY.md).

## Documentation

| Doc | What it covers |
| --- | --- |
| [`docs/oxide_sdk.md`](docs/oxide_sdk.md) | Capstone: integration, benchmarks, capability matrix, release-readiness verdict. |
| [`docs/api_overview.md`](docs/api_overview.md) | Stable Rust API entry points and capability map. |
| [`docs/parser_benchmark.md`](docs/parser_benchmark.md) | The reproducible extraction-quality benchmark + numbers. |
| [`docs/parser_positioning.md`](docs/parser_positioning.md) | Measured capability boundaries, current strengths, positioning. |
| [`docs/document_parsing.md`](docs/document_parsing.md) | The canonical `Document` model and the `parse` surface. |
| [`docs/compliance.md`](docs/compliance.md) | PDF/A-1b/2b/2a/3b/3a and PDF/UA. |
| [`docs/signatures.md`](docs/signatures.md) | RSA signing, verification, LTV substrate. |
| [`docs/manipulation.md`](docs/manipulation.md) | Structural ops: merge, split, encrypt, optimize, linearize, repair. |
| [`docs/bindings.md`](docs/bindings.md) | C ABI and WebAssembly embedding. |
| [`docs/self_hosting.md`](docs/self_hosting.md) | CLI, server (with and without OCR), Docker, WASM, config. |
| [`docs/jobs.md`](docs/jobs.md) | The async job API. |
| [`docs/packaging.md`](docs/packaging.md) | Feature flags, publishing dry-runs, license audit, release checklist. |
| [`docs/stability.md`](docs/stability.md) | SemVer, MSRV, stable-vs-experimental policy. |
| [`docs/security/posture.md`](docs/security/posture.md) | Consolidated hardening posture + residual risk. |
| [`.env.example`](.env.example) | The complete `OXIDE_*` server configuration reference. |
| [`CHANGELOG.md`](CHANGELOG.md) | Release notes and notable API changes. |

## License

**MIT OR Apache-2.0** — permissive, non-copyleft. See [`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE), and [`docs/licenses.md`](docs/licenses.md) (includes bundled-font licensing).
