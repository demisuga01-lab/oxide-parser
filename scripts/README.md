# Scripts

## Poppler Comparison Harness

`poppler_compare.py` runs Poppler and the Oxide CLI against the same PDFs and
records text similarity, render PSNR, `analyze` status, and `extract-images`
status.

Poppler is required as a development dependency:

- Windows: download a release from
  `https://github.com/oschwartz10612/poppler-windows/releases`, then pass
  `--poppler-bin-dir <extract-dir>\Library\bin`.
- macOS: `brew install poppler`.
- Debian/Ubuntu: `sudo apt-get install poppler-utils`.

Example:

```powershell
py scripts\poppler_compare.py `
  --manifest tests\corpus\manifest.json `
  --poppler-bin-dir target\tools\poppler\poppler-26.02.0\Library\bin `
  --output-dir target\poppler_compare\baseline `
  --report-path docs\poppler_parity_baseline.md
```

Focused runs:

```powershell
py scripts\poppler_compare.py --category forms
py scripts\poppler_compare.py --file vertical.pdf
py scripts\poppler_compare.py --max-render-pages 0
```

`--max-render-pages 0` renders all pages. The default is one page per PDF so the
full corpus remains practical for frequent regression checks.

## Corpus Generator

`generate_parity_corpus.py` populates `tests/corpus/pdfs/` and writes
`tests/corpus/manifest.json`.

```powershell
py scripts\generate_parity_corpus.py
```
