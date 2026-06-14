# Poppler Parity Baseline

Generated: 2026-06-14T01:25:15.314765+00:00

## Scope

- Corpus files tested: 75
- DPI: 150
- Render page cap: 1
- Poppler pdftotext: `E:\wellpdfsdk\target\tools\poppler\poppler-26.02.0\Library\bin\pdftotext.exe`
- Poppler pdftoppm: `E:\wellpdfsdk\target\tools\poppler\poppler-26.02.0\Library\bin\pdftoppm.exe`
- Oxide CLI: `E:\wellpdfsdk\target\release\oxide.exe`

## Headline Numbers

- Overall text similarity: 66.8%
- Overall render PSNR: 26.13 dB
- Analyze success rate: 93.3%
- Extract-images success rate: 93.3%

## Category Breakdown

| category | files tested | text similarity | render PSNR | extract-images success rate | notes |
| --- | ---: | ---: | ---: | ---: | --- |
| cjk-text | 10 | 30.7% | 25.88 dB | 100.0% |  |
| complex-vector | 12 | 91.4% | 19.29 dB | 100.0% |  |
| encrypted | 6 | 24.9% | 13.41 dB | 16.7% | text failed: pdfjs_empty_protected, pdfjs_encrypted-attachment, pdfjs_issue15893_reduced; render failed: pdfjs_empty_protected, pdfjs_encrypted-attachment, pdfjs_issue15893_reduced; analyze failed: pdfjs_empty_protected, pdfjs_encrypted-attachment, pdfjs_issue15893_reduced; extract_images failed: pdfjs_empty_protected, pdfjs_encrypted-attachment, pdfjs_issue15893_reduced |
| forms | 12 | 69.4% | 35.86 dB | 100.0% |  |
| jpeg2000 | 2 | 100.0% | 4.02 dB | 100.0% |  |
| large-multipage | 3 | 100.0% | 31.81 dB | 100.0% |  |
| multi-column | 10 | 59.0% | 18.51 dB | 100.0% |  |
| rtl-text | 5 | 26.6% | 17.90 dB | 100.0% |  |
| scanned | 9 | 88.9% | 26.08 dB | 100.0% |  |
| text-basic | 6 | 64.9% | 43.32 dB | 100.0% |  |

## Weakest Categories

- Text: encrypted (24.9%), rtl-text (26.6%), cjk-text (30.7%), multi-column (59.0%), text-basic (64.9%)
- Render: jpeg2000 (4.02 dB), encrypted (13.41 dB), rtl-text (17.90 dB), multi-column (18.51 dB), complex-vector (19.29 dB)

## Failure Details

- `pdfjs_empty_protected` (encrypted): text/oxide: Error: document is encrypted; render/oxide: Error: document is encrypted; analyze/oxide: Error: document is encrypted; extract_images/oxide: Error: document is encrypted
- `pdfjs_encrypted-attachment` (encrypted): text/oxide: Error: parse error: indirect object header is missing obj keyword; render/oxide: Error: parse error: indirect object header is missing obj keyword; analyze/oxide: Error: parse error: indirect object header is missing obj keyword; extract_images/oxide: Error: parse error: indirect object header is missing obj keyword
- `pdfjs_issue15893_reduced` (encrypted): text/oxide: Error: parse error: expected numeric token; render/oxide: Error: parse error: expected numeric token; analyze/oxide: Error: parse error: expected numeric token; extract_images/oxide: Error: parse error: expected numeric token
- `pdfjs_print_protection` (encrypted): text/poppler: Command Line Error: Incorrect password; text/oxide: Error: document is encrypted; render/poppler: Command Line Error: Incorrect password; render/oxide: Error: document is encrypted; analyze/oxide: Error: document is encrypted; extract_images/oxide: Error: document is encrypted
- `pdfjs_secHandler` (encrypted): text/oxide: Error: document is encrypted; render/oxide: Error: document is encrypted; analyze/oxide: Error: document is encrypted; extract_images/oxide: Error: document is encrypted
- `pdfjs_bug_jpx` (jpeg2000): render/poppler: Syntax Error: Malformed JP2 file format: first box must be JPEG 2000 signature box<0a> Syntax Warning: Unable to read header Syntax Warning: Did no succeed opening JPX Stream as JP
- Rust panic signatures recorded: 0
- Command timeouts recorded: 0

## Notes

- Text similarity is a normalized word-token SequenceMatcher ratio against Poppler pdftotext output; very large token streams use a linear token Dice score.
- Render quality is PSNR against Poppler pdftoppm PPM output. Infinite PSNR pages are capped at 100 dB for averages.
- If Poppler and Oxide render dimensions differ, PSNR is computed over the overlapping crop and the mismatch is recorded per page.
- A failed Oxide or Poppler command is recorded as data and does not stop the run.
- The harness output directory contains results.json and results.csv with per-file command status, stderr snippets, and page-level PSNR values.
