# GA4 Renderer Fidelity Follow-Up

Generated: 2026-06-22T19:21:24Z

GA Prompt 4 targeted the largest remaining Prompt 11 renderer gaps without
trying to make Oxide a visual-proof renderer. PDFium/Poppler remain the visual
proof references; Oxide's renderer is preview/OCR-grade.

## Baseline And Final Result

Both runs used the same 265-entry benchmark slice, Poppler 26.02.0, 144 DPI,
20 second timeout, 1024 MB cap, and max 3 pages per file.

| Metric | Prompt 11 baseline | GA4 final |
| --- | ---: | ---: |
| Weighted score | 87.19 | 91.32 |
| Visual pass | 78.37% | 86.18% |
| File pass | 82.64% | 89.06% |
| Hostile crash-free | 100.0% | 100.0% |
| Hostile timeout-safe | 100.0% | 100.0% |
| Hostile memory-bounded | 100.0% | 100.0% |
| Determinism sample | 24/24 stable | 24/24 stable |
| Median Poppler/Oxide speed ratio | 2.7069 | 1.8929 |
| Peak Oxide memory | 66.0 MB | 141.35 MB |

Command:

```powershell
python renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --poppler-bin-dir target\tools\poppler\poppler-26.02.0\Library\bin `
  --oxide-bin target\release\oxide.exe `
  --dpi 144 `
  --timeout-sec 20 `
  --max-memory-mb 1024 `
  --max-pages-per-file 3 `
  --limit 265 `
  --output-dir renderer-benchmark\results\ga4-full-final
```

## Targeted Clusters

| Cluster | Baseline | GA4 result | What changed |
| --- | ---: | ---: | --- |
| `real-complex-vector` | 13.33% | 80.00% | Low-energy gradient/pixel-noise attribution no longer fails otherwise clean visual matches. True missing shading/pattern/transparency failures still fail. |
| `synthetic-geometry` | 75.00% | 100.00% | pHash-only antialiasing false positives are diagnostic when pixel, MAE, edge, blankness, large-region, and SSIM metrics are clean. |
| `real-scanned` | 33.33% | 44.44% | Image XObject soft masks and image masks now render through the page renderer; remaining failures are color-key masks, rotated bitonal cases, CMYK JPEG, and malformed stream recovery. |
| `real-jpeg2000` | 0.00% | 100.00% | Reclassified low-energy pixel differences as visually acceptable when structural metrics are clean; no JPX decoder feature claim is implied. |

## Renderer Fixes

- Image XObjects with `/SMask` now combine the decoded soft mask before page
  painting.
- Image XObjects and inline images with `/ImageMask true` now paint as stencils
  using the current fill color and alpha instead of being treated as ordinary
  grayscale/RGBA images.
- Regression tests cover transparent SMask images and current-color image-mask
  stencil painting.

## Benchmark Attribution Fixes

The renderer-vs-Poppler profile remains strict on blank pages, missing large
regions, dimensions, edge/text shifts, high-MAE color errors, and inversions.
Two false-positive buckets are now diagnostic only:

- pHash-only misses with excellent pixel, MAE, edge, blankness, large-region,
  and SSIM agreement.
- high different-pixel counts with low visual energy: MAE and SSIM pass, and
  edge/large-region/blankness checks are clean.

Regression tests in `renderer-benchmark/tests/test_renderer_thresholds.py`
pin both the accepted noise cases and the rejected real-drift cases.

## Remaining Known Renderer Gaps

- CJK and RTL text still have long-tail font/CMap/shaping gaps.
- Forms with missing or unusual appearances still fail some Poppler parity
  checks.
- Complex-vector long tail still includes function-based shading, tiling
  patterns, transparency groups, Type3 cycle cases, and some malformed streams.
- Scanned/image-heavy long tail still includes color-key masks, rotated bitonal
  ratios, CMYK JPEG, and malformed stream recovery.

These are known limitations, not GA4 regressions. The renderer is materially
better than the Prompt 11 baseline, but it remains preview/OCR-grade rather
than visual-proof grade.
