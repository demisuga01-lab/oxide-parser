# Oxide — Positioning: the Rust-native document parser

> **Scope.** This document positions Oxide as a **document parser / structured
> extractor** (PDF → Markdown / JSON / RAG chunks / key-value fields), competing
> with Docling, PyMuPDF, and qpdf on the *extraction* axis. It is **not** a
> positioning of Oxide as a pixel-faithful *renderer* — for visual fidelity see
> `docs/oxide_vs_poppler.md`, which is candid that Oxide trails Poppler/PDFium
> for visual proofing. Keep the two axes separate: **Oxide leads on
> extraction/embedding/safety/deployment; it trails on render fidelity and on
> ML-grade understanding of messy scans.**
>
> All extraction numbers below come from the reproducible benchmark in
> `docs/parser_benchmark.md` (`extraction-benchmark/`). Re-run it to verify.

## What it is

**Oxide is a Rust-native document parser**: a single, pure-Rust core that turns a
PDF into a structured document model and serializes it to clean **Markdown**,
lossless **JSON**, semantic **HTML**, **RAG-ready chunks**, and **key-value
fields** — with **qpdf-class structural operations** (merge / split /
extract-pages) alongside. It has:

- **One canonical model, every surface.** `parse` produces a schema-versioned
  `Document` (headings / paragraphs / lists / tables / figures / captions in
  recovered reading order, with per-page geometry and provenance). The **CLI,
  the Rust library, the C ABI, and the WASM build all emit the same schema** —
  parse once, consume anywhere.
- **Optional, injected OCR.** Scanned pages are recognized via a pluggable
  `OcrEngine` trait (the bundled backend drives the external Tesseract process —
  no linked C). OCR'd text flows through the *same* model as digital-born text,
  so downstream chunking / fields / serialization are source-agnostic.
- **Pure-Rust, single static binary.** No Python, no torch/ML stack, no
  Poppler/Ghostscript. One ~12 MB binary, ~6 ms cold start.
- **Embeddable four ways.** Rust library (the engine API), C ABI
  (`oxide-capi`), WebAssembly (`oxide-wasm`, in-browser), and a self-hostable
  HTTP server (`oxide-server`). See `docs/bindings.md` and `docs/self_hosting.md`.
- **MIT OR Apache-2.0** — permissive, non-copyleft (contrast Poppler's
  GPL-family licensing). See `docs/licenses.md`.

## Where it wins (benchmark evidence)

| Win | Evidence (from `docs/parser_benchmark.md`) |
| --- | --- |
| **Startup & deployment** | 6.4 ms process startup vs 143.1 ms for Python+PyMuPDF (interpreter + import). Single 12.3 MB static binary, no runtime; Docling adds a multi-GB torch stack. |
| **Reading order** | Normalized Kendall-tau **1.0** on a multi-column report, where a naive top-to-bottom dump interleaves columns. |
| **Clean digital tables** | cell-F1 **1.0** / TEDS **1.0** (ties PyMuPDF); higher text accuracy than `pdftotext` on the table page (1.0 vs 0.298). |
| **Key-value extraction** | field-F1 **1.0** on the digital invoice — a capability PyMuPDF/Poppler do not have at all. Receipt 0.75 (honest partial). |
| **OCR is source-agnostic** | Recovers scanned text at **0.94** char-accuracy and fields where PyMuPDF/Poppler score **0** (no OCR). |
| **Structural integrity** | qpdf cross-validates Oxide's split output (parts pass `qpdf --check`; page counts agree) — qpdf-class round-trip integrity. |
| **Reach / embeddability** | Same canonical extraction in Rust, C, the browser (WASM), and over HTTP — Docling cannot run in a browser; PyMuPDF/Poppler are not WASM-first. |
| **Privacy / self-hosting** | Runs entirely on your own hardware; documents never leave the machine; no per-page cloud fees. See `docs/self_hosting.md`. |
| **Memory safety** | Pure-Rust core; the renderer benchmark's hostile-input sweep was crash/timeout/OOM-clean (`docs/oxide_vs_poppler.md`). |

Clean digital **text** accuracy is near-parity with PyMuPDF (both ~0.99 on the
paper) — a tie, stated as such.

## Where it trails (honest)

- **OCR'd table → grid reconstruction.** An OCR'd scanned table recovers its
  *text* but **not** a clean cell grid (cell-F1 **0**): the OCR path emits prose
  blocks, not a detected `Table` (no ruling-line graphics to key off). For
  scanned tabular data, `extract-fields --ocr` recovers the values/line-items;
  `extract-tables` does not support OCR (it errors with a pointer to the field
  extractor rather than emitting an empty grid).
- **Scanned key-value recall.** Invoice field-F1 drops from 1.0 (digital) to
  **0.4** on the OCR'd scan (OCR noise + single-line label/value merge). An
  ML-layout model (Docling-class) would likely do better on messy scans.
- **OCR quality is bounded by Tesseract.** Oxide's OCR is as good as the
  Tesseract engine and the input scan quality; it is not a learned document
  model. Pure-Rust preprocessing (deskew / binarize / denoise) helps but does
  not close the gap to ML OCR on hard scans.
- **Block-type labeling on some layouts.** Structure/block-type accuracy is
  uneven across document kinds (e.g. strong on tables/figures, weak on a dense
  paper) — see the per-document table in the benchmark.
- **Per-call CLI latency.** Oxide's per-document CLI time (17.8 ms) includes
  **process spawn**; PyMuPDF (3.6 ms) runs in-process. For many-small-doc
  throughput in a long-lived Python process, PyMuPDF's in-process call is
  faster. Embed the Oxide **library** (no spawn) to erase this for in-process
  pipelines; the CLI wins on startup, footprint, and zero-runtime deployment.
- **Docling head-to-head is not measured locally.** Docling's heavy
  ML/torch dependencies were not installable in the benchmark environment; its
  rows are marked *not run* and never fabricated. The breadth of model-based
  understanding on exotic/messy layouts is Docling's expected strength and
  remains unverified here.
- **Renderer fidelity (different axis).** Oxide is *not* positioned as a
  visual-proof renderer; for that, `docs/oxide_vs_poppler.md` recommends
  Poppler/PDFium. Oxide rendering exists primarily to feed OCR and previews.
  The last recorded visual-fidelity figure is **33.40%** visual-page pass
  (`docs/oxide_vs_poppler.md`, 2026-06-18). A fresh full renderer-benchmark run
  measuring the cumulative effect of the later render R&D was **deferred** in
  the foundation pass (it renders thousands of pages over a ~332 MB corpus and
  is not gating the parser/extraction story) — that number is therefore *not*
  re-measured here and is **not** fabricated; re-run
  `renderer-benchmark/scripts/renderer_benchmark.py` to refresh it.

### Not yet built (deferred structural-write operations)

Oxide's structural operations are currently **merge / split / extract-pages**
(read-and-rewrite). The following PDF-mutating operations are **not implemented**
anywhere (neither the library nor the CLI nor the server) and are honest future
work, not shipped features:

- **encrypt / decrypt** — the crypto layer is decrypt-only today; producing an
  encrypted PDF needs write-direction key derivation (and a security review
  before any server exposure).
- **rotate-and-write, optimize / linearize, repair** — page `/Rotate` is read
  only; there is no linearizer or xref-repair builder that emits new bytes.

These are deliberately deferred rather than half-built. The CLI does not expose
`encrypt`/`decrypt`/`rotate`/`optimize`/`linearize`/`repair` subcommands; the
server is intentionally **non-mutating** (a real safety property for a service
ingesting untrusted PDFs).

## Who it's for

Oxide fits teams that want **self-hosted, private, fast, embeddable structured
PDF extraction in Rust** — especially:

- **RAG / LLM ingestion pipelines** that need clean Markdown + token-sized,
  structure-aware chunks, run locally with no per-page cloud fees.
- **Digital-born-heavy document automation** (invoices, receipts, forms →
  JSON), where reading order, tables, and key-value extraction matter.
- **Embedders** building PDF understanding *into* a Rust, C, or browser app —
  one engine, four surfaces, one schema, no Python/torch runtime.
- Teams **fine with Tesseract-grade (not ML-grade) OCR** for the scanned subset
  of their corpus, and who value privacy/self-hosting and a permissive license.

It is **not** the right tool when you need pixel-faithful rendering (use
Poppler/PDFium) or best-in-class understanding of messy, unusual scanned layouts
(where an ML model like Docling is expected to lead).

## Reproducing the numbers

Everything cited here is regenerated by the extraction benchmark:

```sh
python extraction-benchmark/scripts/generate_corpus.py
python extraction-benchmark/scripts/extraction_benchmark.py   # needs target/debug/oxide built --features ocr for OCR rows
python extraction-benchmark/scripts/write_report.py           # writes docs/parser_benchmark.md
```

See `docs/parser_benchmark.md` for the full per-document tables, the tools
compared, and the determinism check.
