# Oxide

<p align="center">
  <img src="docs/assets/oxide-github-hero.svg" alt="Oxide Enterprise PDF SDK workflow banner" width="100%" />
</p>

**A self-hosted PDF SDK written in Rust.** Oxide parses PDFs into structured
Markdown, JSON, semantic HTML, RAG chunks, and key-value fields. It also covers
authoring, editing, redaction, forms, signatures, PDF/A, structural operations,
OCR-enabled extraction, and deployable surfaces for CLI, Rust, C ABI,
WebAssembly, and HTTP server use.

The project is built around one canonical document model and a memory-safe core.
The README is intentionally direct: what is implemented is linked below; what
still needs human review or production hardening is listed in the scope section.

## Quick start

```sh
# Build the single-binary CLI (add --features ocr for scanned-page OCR):
cargo build --release -p oxide-cli

# Parse a PDF into clean Markdown / JSON for RAG and automation:
oxide parse input.pdf --format markdown
oxide parse input.pdf --format json

# RAG-ready semantic chunks, and structured key-value fields:
oxide chunk input.pdf --target-tokens 512
oxide extract-fields input.pdf --type invoice

# What did I build? (reports engine version + whether OCR is compiled in)
oxide --version
```

## Step-by-step setup

<details open>
<summary><strong>1. Install prerequisites</strong></summary>

- Rust stable toolchain with Cargo.
- `qpdf` for structural validation checks.
- Optional: a reference renderer for visual QA and veraPDF for compliance checks.
- Optional OCR: Tesseract with language data when building with `--features ocr`.

```sh
rustup update stable
cargo --version
qpdf --version
```

</details>

<details>
<summary><strong>2. Build the CLI</strong></summary>

```sh
cargo build --release -p oxide-cli

# Windows
target\release\oxide.exe --version

# macOS/Linux
./target/release/oxide --version
```

For scanned-page OCR support:

```sh
cargo build --release -p oxide-cli --features ocr
```

</details>

<details>
<summary><strong>3. Parse, chunk, and extract fields</strong></summary>

```sh
oxide parse input.pdf --format markdown --output output.md
oxide parse input.pdf --format json --output output.json
oxide chunk input.pdf --target-tokens 512 --output chunks.json
oxide extract-fields input.pdf --type invoice --output fields.json
```

Use this path for RAG ingestion, document intelligence, and structured
automation over digital-born PDFs.

</details>

<details>
<summary><strong>4. Run structural and compliance workflows</strong></summary>

```sh
oxide optimize input.pdf -o optimized.pdf
oxide linearize input.pdf -o fast-web-view.pdf
oxide encrypt input.pdf -o encrypted.pdf --password change-me
```

For compliance and release validation, pair Oxide output with external checks:

```sh
qpdf --check fast-web-view.pdf
verapdf --format text compliant.pdf
```

</details>

<details>
<summary><strong>5. Embed Oxide in your product</strong></summary>

- Rust library: `oxide-engine`
- CLI automation: `oxide`
- C ABI: `oxide-capi`
- Browser/WASM: `oxide-wasm`
- Self-hosted API: `oxide-server`

Start with [`docs/api_overview.md`](docs/api_overview.md) for the Rust surface
and [`docs/self_hosting.md`](docs/self_hosting.md) for the HTTP server.

</details>

## Embedding

The same canonical extraction is available four ways — parse once, consume
anywhere:

- **Rust library** (`oxide-engine`): `use oxide_engine::prelude::*;` then
  `engine.parse_document(&ParseOptions::default())?.to_markdown_default()`. See
  `crates/engine/examples/parse_to_markdown.rs`.
- **C ABI** (`oxide-capi`): `oxide_document_parse_markdown` /
  `oxide_document_parse_json` / `oxide_document_extract_fields_json`. See
  [`docs/bindings.md`](docs/bindings.md).
- **WebAssembly** (`oxide-wasm`): in-browser `parseMarkdown()` / `parseJson()` /
  `chunk()` / `extractFieldsJson()` — digital-born only (no OCR in the browser).
- **HTTP server** (`oxide-server`): self-hostable `POST /api/v1/parse` /
  `/chunk` / `/extract-fields` / `/info`, with auth, rate limits, resource caps,
  and an async job queue. See [`docs/self_hosting.md`](docs/self_hosting.md).

## Self-hosting

Run the whole stack on your own machine or VPS — documents never leave your
hardware, no per-page cloud fees. See
[**`docs/self_hosting.md`**](docs/self_hosting.md) for the CLI, the server (with
and without OCR), Docker, browser-side WASM extraction, and resource/privacy
guidance.

## Documentation

| Doc | What it covers |
| --- | --- |
| [`docs/self_hosting.md`](docs/self_hosting.md) | Running Oxide yourself: CLI, server, OCR, Docker, WASM, config. |
| [`docs/oxide_sdk.md`](docs/oxide_sdk.md) | Capstone integration, fresh benchmarks, capability matrix, and release-readiness verdict. |
| [`docs/api_overview.md`](docs/api_overview.md) | Stable Rust/API entry points and capability map. |
| [`docs/stability.md`](docs/stability.md) | SemVer, MSRV, stable-vs-experimental policy, API drift checks. |
| [`docs/packaging.md`](docs/packaging.md) | Feature flags, publishing dry-runs, license audit, artifacts, release checklist. |
| [`docs/parser_positioning.md`](docs/parser_positioning.md) | Measured capability boundaries, current strengths, and release positioning. |
| [`docs/parser_benchmark.md`](docs/parser_benchmark.md) | The reproducible extraction-quality benchmark + numbers. |
| [`docs/linearization_qpdf_clean_ga1.md`](docs/linearization_qpdf_clean_ga1.md) | qpdf-clean linearization hint-table fix and fixture breadth. |
| [`docs/document_parsing.md`](docs/document_parsing.md) | The canonical `Document` model and the `parse` surface. |
| [`docs/compliance.md`](docs/compliance.md) | PDF/A-1b/2b/2a/3b/3a validation and bounded conversion, plus PDF/UA basic checks. |
| [`docs/bindings.md`](docs/bindings.md) | C ABI and WebAssembly embedding. |
| [`docs/security.md`](docs/security.md) | Server security posture + deploy checklist. |
| [`docs/security/posture.md`](docs/security/posture.md) | Consolidated hardening posture: fuzzing, differential checks, property tests, audit gates, and residual risk. |
| [`docs/jobs.md`](docs/jobs.md) | The async job API and its limitations. |
| [`CHANGELOG.md`](CHANGELOG.md) | Release notes and notable API changes. |
| [`.env.example`](.env.example) | The complete `OXIDE_*` server configuration reference. |

## Scope and limits

Implemented and documented:

- Structured extraction to Markdown, JSON, semantic HTML, chunks, and fields.
- Programmatic PDF authoring with pages, text, graphics, images, fonts, tables,
  and single-column flow layout.
- Additive editing, watermarks, headers/footers, redaction, annotations, form
  fill/flatten, and incremental update support.
- Structural operations for merge, split, extract-pages, rotate, repair,
  optimize, encrypt/decrypt, and linearize.
- PDF/A-1b/2b/2a/3b/3a validation and bounded conversion paths.
- Digital signatures with core validation plus documented LTV material support.
- CLI, Rust library, C ABI, WebAssembly, and self-hosted HTTP server surfaces.

Known limits:

- Rendering is suitable for previews, OCR support, and regression checks. It is
  not a visual-proof renderer.
- OCR quality depends on the installed OCR backend and source scan quality.
  Messy scanned tables and low-quality forms still need human review.
- PDF/UA tagging is assistive and best-effort. Accessibility certification still
  requires manual semantic review.
- Signature LTV live TSA/OCSP trust policy and PAdES-B-LTA archival refresh are
  follow-up areas.
- Custom font subsetting depth, CFF/OpenType embedding breadth, and advanced
  multi-column document layout remain active follow-ups.

Measured claims and release evidence live in `docs/oxide_sdk.md`,
`docs/security/posture.md`, and the benchmark artifacts under
`extraction-benchmark/`.

## License

MIT OR Apache-2.0 — permissive, non-copyleft. See
[`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE), and
[`docs/licenses.md`](docs/licenses.md) (includes bundled-font licensing).
