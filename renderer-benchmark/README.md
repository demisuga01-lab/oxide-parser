# Renderer Benchmark 0A

This directory contains the renderer-compatibility benchmark for Oxide vs
reference renderers. Benchmark 0A is separate from Benchmark 0B:

- **0A renderer compatibility:** Oxide-rendered pages vs Poppler-rendered pages,
  with optional PDFium when a compatible CLI is present.
- **0B compression safety:** Oxide(original) vs Oxide(compressed), using stricter
  same-renderer thresholds. This is scaffolded only; it does not prove renderer
  compatibility.

## Layout

- `corpus/synthetic/`: generated feature-focused PDFs.
- `corpus/real-world/`: user-supplied real PDFs. The generated manifest also
  references the existing `tests/corpus/` PDFs without copying them.
- `corpus/hostile/`: malformed and adversarial PDFs for safety checks.
- `corpus/large-files/`: generated many-page PDFs for performance and memory.
- `corpus/wellpdf-before-after/`: 0B original/compressed pairs.
- `results/`: per-file JSON and aggregate reports.
- `scripts/`: corpus generator and benchmark runners.

## Generate The Seed Corpus

```powershell
py -3 renderer-benchmark\scripts\generate_benchmark_corpus.py
```

This creates a manifest at `renderer-benchmark/corpus/manifest.json`. It seeds:

- 120 synthetic PDFs,
- 60 hostile PDFs,
- 10 large/many-page PDFs,
- the existing `tests/corpus/manifest.json` real-world/parity corpus.

The synthetic set is broad but not exhaustive. Harder cases such as true
linearized files, hybrid xref files, embedded OpenType/CFF construction, and
fully realistic ICC/Lab/DeviceN examples should be added as follow-up corpus
expansion.

## Run Benchmark 0A

```powershell
cargo build --release -p oxide-cli
py -3 renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --poppler-bin-dir target\tools\poppler\poppler-26.02.0\Library\bin `
  --oxide-bin target\release\oxide.exe `
  --dpi 144 `
  --timeout-sec 20 `
  --max-memory-mb 1024 `
  --max-pages-per-file 3 `
  --output-dir renderer-benchmark\results\run-0a
```

PDFium is optional. If `pdfium_test` or `pdfium-render` is not found, the runner
records that PDFium was skipped and still emits a complete Poppler-only report.

## Expanding Toward The Full Bar

The full Tier-3 evidence target is 1,000+ real-world PDFs and 10,000+ rendered
pages. Drop real PDFs into `corpus/real-world/` and rerun the generator; the
benchmark will include them automatically. Do not present a small seed run as
full Tier-3 evidence.

Prompt F added a repeatable expansion helper:

```powershell
py -3 renderer-benchmark\scripts\expand_real_corpus.py
py -3 renderer-benchmark\scripts\generate_benchmark_corpus.py --no-clean
```

The helper downloads the full Mozilla pdf.js `test/pdfs` corpus (Apache-2.0)
and a small U.S. IRS public-domain forms/publications set, writing source and
license metadata to `corpus/real-world-sources.json`. User-supplied PDFs can
still be added under `corpus/real-world/`; files not present in the metadata
sidecar are tagged `real-user-supplied`.

For expanded re-baselines, prefer breadth over rendering every page of very
large files. A practical run should raise the seed cap from 3 pages/file to a
documented cap such as 20 or 50 pages/file:

```powershell
cargo build --release -p oxide-cli
py -3 renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --poppler-bin-dir target\tools\poppler\poppler-26.02.0\Library\bin `
  --oxide-bin target\release\oxide.exe `
  --dpi 144 `
  --timeout-sec 20 `
  --max-memory-mb 1024 `
  --max-pages-per-file 20 `
  --output-dir renderer-benchmark\results\prompt-f-expanded
```

## Benchmark 0B Scaffold

Place pairs under `corpus/wellpdf-before-after/` using either:

- `case-name/original.pdf` and `case-name/compressed.pdf`, or
- `case-name.before.pdf` and `case-name.after.pdf`.

Then run:

```powershell
py -3 renderer-benchmark\scripts\run_0b_compression_safety.py `
  --oxide-bin target\release\oxide.exe `
  --pairs-dir renderer-benchmark\corpus\wellpdf-before-after `
  --output-dir renderer-benchmark\results\run-0b
```

0B uses stricter same-renderer thresholds and must remain separate from 0A.
