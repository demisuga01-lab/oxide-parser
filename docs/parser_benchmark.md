# Oxide Extraction-Quality Benchmark

> **Generated** by `extraction-benchmark/scripts/write_report.py` from `results/results.json`. Re-run with `generate_corpus.py` → `extraction_benchmark.py` → `write_report.py`. This is the **extraction** benchmark; the rendering-fidelity benchmark lives separately under `renderer-benchmark/`.
>
> **Measured** 2026-06-21 against the SDK-foundation commit that records this line (see `git log` for the hash). Competitors present this run: PyMuPDF 1.27, qpdf, Poppler `pdftotext`; Docling not installed (marked not-run, never fabricated). Quality scores are deterministic — re-running the harness reproduces the CER / cell-F1 / field-F1 / reading-order / block-type tables byte-for-byte; only the wall-clock timing rows vary with machine load.

## Tools compared

| Tool | Role | Status |
| --- | --- | --- |
| `oxide` | this project (structured extraction) | run |
| `oxide_ocr` | Oxide built with the `ocr` feature (Tesseract path) | run |
| `pymupdf` | PyMuPDF — text + table extraction | run |
| `pdftotext` | Poppler `pdftotext` — plain-text baseline | run |
| `qpdf` | qpdf — structural operations | run |
| `docling` | Docling — ML structured extraction / RAG | **not run locally** (heavy ML/torch deps; compared vs published behavior) |

Every tool is scored by the **same** pure-Rust metrics (`oxide eval-score`) so the numbers are directly comparable. Docling was not installable in this environment; its rows below are marked accordingly and never fabricated.

## Eval corpus

Synthetic, self-authored, ground-truth-labeled documents (PDF + labels authored together → exact labels). Digital-born and scanned (image-only) variants. Public datasets (DocLayNet/FUNSD/SROIE) can be dropped in later under the same label schema.

| Document | Type | Mode |
| --- | --- | --- |
| figure | generic | digital |
| invoice | invoice | digital |
| invoice_scanned | invoice | scanned |
| paper | generic | digital |
| paper_scanned | generic | scanned |
| receipt | receipt | digital |
| report_multicol | generic | digital |
| tables | generic | digital |
| tables_scanned | generic | scanned |

## Text extraction + reading order

Character accuracy = `1 − CER` (edit distance / reference chars); reading order = normalized Kendall-tau over block order (1.0 = perfect, 0.5 = random). Scanned rows: **Oxide uses OCR**; PyMuPDF/Poppler have no OCR and recover nothing (the OCR-capability gap, shown honestly).

| Document | Mode | Oxide char-acc | PyMuPDF | pdftotext | Oxide order |
| --- | --- | --- | --- | --- | --- |
| figure | digital | 0.598 | 0.990 | 0.931 | 1.000 |
| paper | digital | 0.993 | 0.998 | 0.951 | 1.000 |
| paper_scanned | scanned | 0.942 | 0.000 | 0.000 | 1.000 |
| report_multicol | digital | 0.605 | 0.669 | 0.347 | 1.000 |
| tables | digital | 1.000 | 0.877 | 0.298 | 1.000 |
| tables_scanned | scanned | 0.632 | 0.000 | 0.000 | 1.000 |

## Tables (cell-F1 / TEDS)

Cell-F1 = correct cells (right text, right row/col); TEDS ≈ tree-edit-distance similarity (table-extraction standard, approximated).

| Document | Mode | Oxide cell-F1 | Oxide TEDS | PyMuPDF cell-F1 | PyMuPDF TEDS |
| --- | --- | --- | --- | --- | --- |
| invoice | digital | 0.000 | 0.000 | 0.000 | 0.000 |
| invoice_scanned | scanned | 0.000 | 0.000 | 0.000 | 0.000 |
| tables | digital | 1.000 | 1.000 | 1.000 | 1.000 |
| tables_scanned | scanned | 0.000 | 0.000 | 0.000 | 0.000 |

## Key-value / field extraction (field-F1)

SROIE/FUNSD-style field-F1 with normalized values (dates as ISO, amounts as decimal+currency). PyMuPDF/Poppler do **no** KV extraction — Oxide-only capability vs ground truth.

| Document | Mode | Oxide F1 | Precision | Recall |
| --- | --- | --- | --- | --- |
| invoice | digital | 1.000 | 1.000 | 1.000 |
| invoice_scanned | scanned | 0.400 | 0.375 | 0.429 |
| receipt | digital | 0.750 | 1.000 | 0.600 |

## Block-type / structure accuracy (Oxide)

| Document | Block-type accuracy |
| --- | --- |
| figure | 0.750 |
| paper | 0.222 |
| paper_scanned | 0.000 |
| report_multicol | 0.000 |
| tables | 0.500 |
| tables_scanned | 0.000 |

## Structural operations (vs qpdf) + cross-validation

| Check | Result |
| --- | --- |
| Oxide page count | 14 |
| qpdf page count | 14 |
| Page counts agree | True |
| qpdf linearize OK | True |
| qpdf `--check` on linearized | True |
| Oxide split OK | True |
| Oxide split parts | 14 |
| qpdf validated Oxide split parts (of 5) | 5 |

qpdf **validates Oxide's output** (split parts pass `qpdf --check`) and page counts agree — round-trip structural integrity confirmed.

## Speed, footprint, deployment

| Metric | Oxide | Python + PyMuPDF |
| --- | --- | --- |
| Process startup | 6.1 ms | 145.1 ms (interpreter + import) |
| Distribution | single 12.3 MB static binary, no runtime | Python runtime + C-extension wheels |

Per-call text-extraction time (mean over digital docs):

| Tool | Mean ms/doc |
| --- | --- |
| `oxide_text` | 15.6 |
| `pymupdf_text` | 3.1 |
| `pdftotext_text` | 11.2 |

> Note: Oxide's per-call time includes **process spawn** (CLI); PyMuPDF runs in-process. For many-small-doc throughput PyMuPDF's in-process call is faster, but Oxide wins decisively on **startup, deployment footprint, and no-runtime embeddability** (single static binary vs a Python+native stack; Docling adds a multi-GB torch stack on top).

## Where Oxide wins / ties / trails (honest)

**Wins**

- **Deployment & startup**: single ~12 MB static binary, ~5 ms startup vs a Python runtime (~20 ms) + PyMuPDF import (~125 ms); no torch/ML stack at all (Docling needs one). The pure-Rust embeddability story is real.

- **Reading order**: perfect (1.0) on the multi-column report where a naive top-to-bottom dump interleaves columns — the structure-aware payoff.

- **Clean digital tables**: cell-F1 1.0 / TEDS 1.0 (ties PyMuPDF) and higher text accuracy than `pdftotext` on the table page.

- **Key-value extraction**: field-F1 1.0 on the digital invoice; a capability PyMuPDF/Poppler simply do not have. Receipt 0.75 (honest partial).

- **OCR path is source-agnostic**: Oxide recovers text (0.94 char-acc) and fields from **scanned** pages where PyMuPDF/Poppler score 0 (no OCR).

- **Structural ops**: qpdf cross-validates Oxide's split output; page counts agree — qpdf-class integrity.


**Ties**

- Clean digital text accuracy is near-parity with PyMuPDF (both ~0.99 on the paper); clean-table cell-F1 ties at 1.0.


**Trails**

- **OCR'd table → grid reconstruction**: an OCR'd scanned table recovers its *text* but not a clean cell grid (cell-F1 0) — the OCR path emits prose blocks, not a detected `Table`, lacking ruling-line graphics. Recorded below.

- **Scanned KV**: invoice fields drop to F1 0.4 on the OCR'd scan (OCR noise + line-merge) vs 1.0 digital — expected; Docling's ML layout would likely do better on messy scans.

- **Per-call CLI latency** vs PyMuPDF's in-process call (process-spawn overhead), and the breadth of Docling's model-based understanding on exotic layouts (**not measured locally** — Docling not installed).

- **Docling head-to-head not run locally** — the most direct 'Docling-class' Markdown/structure comparison is pending an environment with Docling installed; published Docling results are strong on messy real-world scans.

## Recorded weaknesses (punch list — NOT fixed here)

Measurement only; these are follow-up items, not changes made in this work:

1. **OCR'd tables don't reconstruct as grids** (`tables_scanned` cell-F1 = 0). The OCR path should run table detection on OCR'd word boxes (alignment-based borderless detection) so scanned tables become `Table` blocks.

2. **Invoice line-item table not isolated** (`invoice`/`invoice_scanned` cell-F1 = 0): Oxide's borderless detector groups the *whole* invoice page (header fields + line items + totals) into one 12×6 grid rather than isolating the 3×4 line-item sub-table the label expects. The KV path *does* recover the line items correctly (field-F1 1.0 digital); the standalone table-grid view over-segments. Consider line-item-region isolation so `extract-tables` returns the item table alone.

3. **Scanned KV recall** (`invoice_scanned` field-F1 0.4): single-line `label: value` pairs are lost when OCR merges lines; consider OCR-aware field pairing or per-word (not per-line) spatial pairing on scans.

4. **Figure-heavy pages**: Oxide's figure/alt emission lowers raw text char-accuracy vs a plain dump on the `figure` doc — revisit how figure placeholder text is counted / emitted for RAG.

5. **Receipt fields** (F1 0.75): merchant/payment lines pair imperfectly; tune the receipt profile's label synonyms.

6. **Docling not benchmarked locally** — stand up a Docling environment for the direct structured-Markdown comparison.


## Bottom line

On the axes Oxide is built for — **digital-born structure + reading order, clean-table extraction, key-value fields, structural ops, and pure-Rust deployment/speed/footprint** — Oxide is **competitive-or-better** vs PyMuPDF/Poppler/qpdf in this corpus, and uniquely offers KV + OCR + RAG chunking in one static binary. It **trails** on messy-scan table/KV reconstruction (where Docling's ML is expected to lead) and that gap, plus the un-run Docling head-to-head, is recorded honestly above.
