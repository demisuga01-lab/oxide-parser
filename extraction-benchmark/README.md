# Extraction-Quality Benchmark

The **extraction** benchmark for Oxide: ground-truth scoring of structured
extraction (text, reading order, tables, key-value fields, structure) head-to-head
vs PyMuPDF / qpdf / Poppler `pdftotext` / Docling-when-available.

This is **separate** from the rendering-fidelity benchmark (`../renderer-benchmark/`):
extraction scoring is fast and the metrics are crisp, so this can run regularly.
It is **measurement only** — it does not change the parser; weaknesses are recorded
as a punch list in the report.

## Layout

```
extraction-benchmark/
  corpus/        generated labeled PDFs (digital-born + scanned variants)
  expected/      ground-truth JSON labels, one per corpus doc
  results/       results.json (raw scores + availability + speed/size)
  scripts/
    generate_corpus.py        author the labeled corpus (reportlab + PyMuPDF rasterizer)
    extraction_benchmark.py   run every available tool, score via `oxide eval-score`
    write_report.py           render results.json -> docs/parser_benchmark.md
    test_harness.py           self-tests for the scorer + tool detection
```

## Metrics (all computed by the pure-Rust scorer, `oxide eval-score`)

- **Text**: character accuracy (`1 − CER`), word accuracy (`1 − WER`) — edit distance.
- **Reading order**: normalized Kendall-tau over block order (1.0 perfect, 0.5 random).
- **Tables**: cell-level precision/recall/F1 and a TEDS approximation.
- **Key-value**: SROIE/FUNSD-style field-F1 with normalized values.
- **Structure**: block-type classification accuracy.
- **Structural ops**: page-count agreement + qpdf cross-validation of Oxide output.
- **Speed / footprint**: startup, per-call time, single-binary size.

Keeping the metrics in pure Rust (`crates/engine/src/eval/`) makes them
unit-tested in `cargo test` and means **every** tool — Oxide and each competitor —
is scored by the *same* implementation.

## Label schema (`expected/<doc>.json`)

```json
{
  "doc_type": "invoice|receipt|form|generic",
  "mode": "digital|scanned",
  "text": "reading-order plain text (empty → text not scored for this doc)",
  "order": ["block-identity-key", "..."],
  "tables": [ [["cell","..."], "..."] ],
  "fields": [ {"key": "...", "value": "normalized"} ],
  "block_types": ["heading","paragraph","table","figure","..."]
}
```

## Corpus & sources

All corpus documents are **synthetic and self-authored** (PDF and labels authored
together → exact ground truth). No third-party corpus is redistributed, so the
corpus is freely usable. Public datasets (DocLayNet, FUNSD, SROIE, PubLayNet) can
be dropped into `corpus/` with matching `expected/*.json` using the schema above.

## Running

```sh
cargo build -p oxide-cli --features ocr          # OCR path for scanned docs
python3 -m pip install pymupdf reportlab          # dev tooling (competitors/generation)
python3 extraction-benchmark/scripts/generate_corpus.py
python3 extraction-benchmark/scripts/extraction_benchmark.py
python3 extraction-benchmark/scripts/write_report.py        # -> docs/parser_benchmark.md
python3 extraction-benchmark/scripts/test_harness.py        # self-tests
```

Competitors are detected at runtime; absent ones are skipped cleanly and never
fabricated. Docling (heavy ML/torch) is marked **not run locally** when absent.
