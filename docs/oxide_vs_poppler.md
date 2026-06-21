# Oxide vs Poppler - Fidelity Fix 4 Final Baseline

Generated: 2026-06-18.

This report replaces the Prompt J positioning report with the final
Fidelity-Fix-1-through-4 at-scale baseline. All headline numbers below were
freshly measured from the current working tree in this session. The default
headline mode is Compat proof mode.

Bottom line: Oxide improved materially across the fidelity arc, but it is still
not ready to replace Poppler as the WellPDF visual-proof renderer. It is already
the better engineering choice for memory-safe embedding, browser/WASM reach,
permissive-license deployment, C ABI integration, and semantic/layout-oriented
workflows. Poppler remains the better choice for production visual proof.

## 1. What Changed in Fidelity Fix 4

Fix 4 was a long-tail cleanup pass, not a "chase 100%" pass. The remaining
high-count, clean item was a benchmark threshold calibration issue:

| Item | Evidence | Fix | Verification |
|---|---|---|---|
| `synthetic-graphics` false failures | The 42 generated graphics fixtures include text plus simple vector marks. Failures had high SSIM and low structural error, matching text antialiasing drift rather than a visible graphics defect. | Treat `synthetic-graphics` as text-heavy for pixel/AA tolerance only. Structural checks such as blank page, large-region, SSIM, pHash, and edge/text shift remain strict. | Target run `renderer-benchmark/results/fidelity-fix4-synthetic-graphics-threshold`: 42/42 files and pages passed, 100.00%. Full final run kept this at 100.00%. |
| Hostile visual-only findings in fidelity blockers | Hostile files are safety fixtures; visual missing-page diagnostics from intentionally malformed PDFs polluted fidelity blocker lists. | Exclude hostile `visual_compare.failed_pages` from fidelity `failure_breakdown` and `blocking_findings`. Hostile safety accounting is unchanged. | Unit test added; final aggregate still reports hostile safety separately at 100.00%. |

Regression tests added in `renderer-benchmark/scripts/test_renderer_benchmark.py`:

- `test_synthetic_graphics_use_text_heavy_pixel_threshold_only`
- `test_hostile_visual_gaps_do_not_pollute_fidelity_breakdowns`

No new Rust dependencies were added.

## 2. Long-Tail Triage

Remaining failures after Fix 4 are dominated by real renderer gaps, not cheap
threshold issues.

| Root-cause group | Fresh final evidence | Decision |
|---|---:|---|
| Multi-column and long-form layout/text positioning | `real-multi-column`: 130 failed files, 587 failed pages; main page reasons: 298 pixel, 81 edge/text shift, 80 large-region, 53 blank. | Recorded. This needs deeper layout/font positioning work, not a safe late cleanup. |
| Forms and annotations | `real-forms`: 75 failed files, 181 failed pages; main page reasons: 81 pixel, 32 pHash, 24 edge/text shift, 18 large-region, 17 low SSIM. | Recorded. Appearance streams/widget semantics are still incomplete. |
| pdf.js general corner cases | `real-pdfjs-general`: 503 failed files, 560 failed pages; main page reasons: 155 blank, 141 large-region, 111 pixel, 66 pHash, 41 edge/text shift, 35 missing page. | Recorded. This is a grab bag of parser, graphics, masks, fonts, malformed, and edge PDF behavior. |
| Complex vector, shading, patterns, masks | `real-complex-vector`: 24 failed files, 30 failed pages; visual pass 18.18%. | Recorded. Fixing this safely needs broader vector/shading/pattern parity work. |
| Fonts, CJK, RTL, shaping | CJK 43.75%, RTL 40.00%, font-edge 30.00% visual pass. | Recorded. Remaining cases need font fallback, CID/CMap, shaping, and metrics work. |
| Scanned/image/JBIG2/JPEG2000/color decode | Scanned 37.50%; JPEG2000 0.00%; image/color proxy 68.33. | Recorded. Remaining cases include decode, mask, color-space, JBIG2/JPEG2000, and scaling variants. |
| Encrypted PDFs | `real-encrypted`: 75.00% visual pass, 50.00% file pass; password-protected variants still fail cleanly. | Recorded. Needs broader encryption variant coverage. |
| Malformed/unsupported/timeouts | Final full run still has 41 `oxide_render_failed`; CVE-class sweep has one 20s render timeout on `bug1721218_reduced.pdf`. | Recorded as a safety/performance limitation. It is one file, not a high-count fidelity cluster. |

Threshold sanity check: only the synthetic-graphics text-AA calibration was
changed. No structural/safety detector was loosened to inflate the score.

## 3. Final At-Scale 0A Baseline

Command:

```powershell
py -3 renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --poppler-bin-dir target\tools\poppler\poppler-26.02.0\Library\bin `
  --oxide-bin target\release\oxide.exe `
  --dpi 144 `
  --timeout-sec 20 `
  --max-memory-mb 1024 `
  --max-pages-per-file 5 `
  --determinism-sample 40 `
  --output-dir renderer-benchmark\results\fidelity-fix4-final-expanded-cap5
```

Artifact: `renderer-benchmark/results/fidelity-fix4-final-expanded-cap5/aggregate.json`.

Environment:

- OS: Microsoft Windows 11 Home Single Language 10.0.26200, build 26200.
- Hardware: Acer Predator PHN16-71.
- CPU: 13th Gen Intel(R) Core(TM) i5-13500HX, 14 cores, 20 logical processors.
- RAM: 16,886,128,640 bytes installed; performance harness reported 16,104 MB.
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`.
- Poppler: `pdfinfo` / `pdftoppm` 26.02.0, vendored under
  `target/tools/poppler/poppler-26.02.0/Library/bin`.
- PDFium: not available in this run.

| Metric | Final Fix 4 value |
|---|---:|
| Files | 1,335 |
| Normal files | 1,275 |
| Hostile files | 60 |
| Real-world files | 1,145 |
| Visual pages compared | 2,042 |
| Visual pages passed | 682 |
| Oxide rendered pages | 2,086 |
| Poppler rendered pages | 2,109 |
| Total backend page images | 4,195 |
| File pass rate | 42.40% |
| Visual-page pass rate | 33.40% |
| Weighted score | 64.87 |
| Tier | Tier 0 |

Scale caveat: this satisfies the 1,000+ real-PDF scale requirement, but the
run is capped at 5 pages per file and compares 2,042 pages, not the 10,000+
rendered-page Tier 3 scale.

### 3.1 Fidelity Arc History

| Run | Files | Visual pages | File pass | Visual-page pass | Weighted score | Notes |
|---|---:|---:|---:|---:|---:|---|
| Prompt J baseline | 1,335 | 1,923 | 28.76% | 23.50% | 57.94 | Original at-scale baseline before Fixes 1-4. |
| Fidelity Fix 1 | 1,335 | 2,002 | 35.66% | 26.84% | not restated here | Fixed the 157-render-failure class from the earlier round. |
| Fidelity Fix 2 | 1,335 | 2,038 | 37.38% | 29.93% | 61.12 | Large-files and synthetic-transparency reached 100%. |
| Fidelity Fix 3 | 238 | 849 | 13.87% | 9.54% | 25.25 | Targeted forms/multi-column rerun only, not a full at-scale rerun. |
| Fidelity Fix 4 final | 1,335 | 2,042 | 42.40% | 33.40% | 64.87 | Final coherent at-scale rerun after long-tail cleanup. |

### 3.2 Category Breakdown

| Category | Files | File pass | Visual pages | Visual pass |
|---|---:|---:|---:|---:|
| large-files | 10 | 100.00% | 50 | 100.00% |
| real-cjk-text | 13 | 46.15% | 16 | 43.75% |
| real-complex-vector | 30 | 20.00% | 33 | 18.18% |
| real-encrypted | 6 | 50.00% | 4 | 75.00% |
| real-font-edge | 6 | 50.00% | 10 | 30.00% |
| real-forms | 104 | 27.88% | 218 | 16.97% |
| real-jpeg2000 | 2 | 0.00% | 1 | 0.00% |
| real-large-multipage | 3 | 100.00% | 11 | 100.00% |
| real-multi-column | 134 | 2.99% | 631 | 6.97% |
| real-pdfjs-general | 820 | 38.66% | 919 | 42.87% |
| real-rtl-text | 5 | 40.00% | 5 | 40.00% |
| real-scanned | 16 | 37.50% | 16 | 37.50% |
| real-text-basic | 6 | 100.00% | 8 | 100.00% |
| synthetic-forms | 8 | 100.00% | 8 | 100.00% |
| synthetic-geometry | 24 | 75.00% | 24 | 75.00% |
| synthetic-graphics | 42 | 100.00% | 42 | 100.00% |
| synthetic-images | 16 | 100.00% | 16 | 100.00% |
| synthetic-text | 18 | 83.33% | 18 | 83.33% |
| synthetic-transparency | 12 | 100.00% | 12 | 100.00% |

All hostile categories passed file-level safety checks at 100.00%; they do not
have Poppler-vs-Oxide visual comparison pages in the category table.

### 3.3 Failure Breakdown

| Failure reason | Count |
|---|---:|
| `page:pixel_difference` | 509 |
| `page:large_region_difference` | 247 |
| `pixel_difference` | 247 |
| `page:blank_page_mismatch` | 232 |
| `large_region_difference` | 214 |
| `blank_page_mismatch` | 200 |
| `page:edge_or_text_shift` | 150 |
| `page:perceptual_hash_distance` | 148 |
| `perceptual_hash_distance` | 123 |
| `edge_or_text_shift` | 88 |
| `page:low_ssim` | 74 |
| `low_ssim` | 60 |
| `oxide_render_failed` | 41 |
| `page:rendered_page_missing` | 39 |
| `rendered_page_missing` | 32 |
| `poppler_render_failed` | 30 |
| `page_count_mismatch` | 1 |

## 4. Safety, Determinism, and Performance

### 4.1 Safety

Full 0A hostile subset:

| Slice | Crash-free | Timeout-safe | Memory-bounded |
|---|---:|---:|---:|
| 60 hostile files in final full run | 100.00% | 100.00% | 100.00% |

Fresh CVE-class Oxide-only safety sweep:

```text
Manifest: renderer-benchmark/corpus/prompt-g-cve-class-manifest.json
Entries: 751
Command shape: oxide render <pdf> --format png --dpi 144 --pages 1-5
Timeout: 20s per file
Memory cap: 1024 MB
Artifact: target/fidelity-fix4-cve-safety/summary.json
```

| Metric | Result |
|---|---:|
| Crash-free | 100.00% |
| Completion before 20s timeout | 99.87% |
| Memory-bounded | 100.00% |
| Max observed Oxide peak memory | 223.49 MB |
| Nonzero clean exits | 58 |
| Render timeouts | 1 |

The timeout was `pdfjs_full_bug1721218_reduced` at 20.007s with 76.59 MB peak
memory and no crash. This is the one fresh safety discrepancy against a strict
"no render timeout" gate. It is recorded as a known limitation rather than
hidden.

### 4.2 Determinism

The final full 0A run sampled 40 files and all 40 were bit-stable:

| Sampled files | Stable files | Stable percent | Unstable |
|---:|---:|---:|---|
| 40 | 40 | 100.00% | none |

### 4.3 Weighted Score

| Sub-score | Weight | Value |
|---|---:|---:|
| Safety | 25 | 100.00 |
| Page count / dimensions | 15 | 99.96 |
| Visual match | 35 | 33.40 |
| Text/font correctness proxy | 10 | 13.57 |
| Image/color correctness proxy | 10 | 68.33 |
| Performance/memory | 5 | 100.00 |
| Total | 100 | 64.87 |

Supported tier: Tier 0 at this corpus scale. The score improved, but visual
match remains far below the Tier 3 visual-proof requirement.

### 4.4 Performance

Command:

```powershell
py -3 scripts\perf_prompt_h.py --repeats 3 --dpi 150 --threads 20 --timeout-sec 240
```

Artifacts:

- `docs/perf_prompt_h_results.json`
- `docs/perf_prompt_h_summary.md`

Fresh summary:

- Across 47 non-info comparisons, Oxide won 25 and Poppler won 22.
- For the 23 non-info `oxide@N` vs Poppler comparisons, Oxide won 12 and
  Poppler won 11.
- Oxide strongly wins startup/info, small text, small renders, image
  extraction, and the incremental/XFA fixture.
- Poppler wins vector-heavy render, font-heavy render time, image-heavy render
  time/memory, 120-page render all, and large-linearized text extraction.

Representative medians:

| Case / op | Oxide@1 | Oxide@20 | Poppler | Result |
|---|---:|---:|---:|---|
| 1-page text, render all | 0.0262s / 20.4 MB | 0.0285s / 20.3 MB | 0.0537s / 21.9 MB | Oxide wins |
| 120-page text, text all | 0.3479s / 6.1 MB | 0.0717s / 12.2 MB | 0.0328s / 11.0 MB | Poppler wins |
| 120-page text, render all | 2.2050s / 21.9 MB | 2.2508s / 21.9 MB | 1.7957s / 21.9 MB | Poppler wins |
| Image-heavy, image extraction | 0.0527s / 7.7 MB | n/a | 0.5512s / 11.0 MB | Oxide wins strongly |
| Image-heavy, render all | 0.3721s / 67.1 MB | 0.3439s / 67.7 MB | 0.3278s / 32.3 MB | Poppler wins |
| Vector-heavy, render all | 0.1180s / 29.0 MB | 0.1193s / 29.0 MB | 0.0560s / 19.3 MB | Poppler wins |
| Font-heavy, render all | 0.2452s / 22.6 MB | 0.2364s / 22.6 MB | 0.1212s / 31.0 MB | Poppler wins time; Oxide wins memory |
| Large linearized, text all | 8.3380s / 12.9 MB | 1.3679s / 35.8 MB | 0.8013s / 13.6 MB | Poppler wins |
| Incremental/XFA, render all | 0.0231s / 23.4 MB | 0.0237s / 23.4 MB | 0.0577s / 22.0 MB | Oxide wins |

## 5. Verdict

### 5.1 Tier

The evidence supports Tier 0 for visual renderer parity at this achieved scale.
The result is better than the Prompt J baseline, but not close to a production
visual-proof bar.

### 5.2 WellPDF Visual-Proof Decision

Decision: NO, Oxide is not yet fit to be WellPDF's visual-proof backend.

The WellPDF Tier 3 bar is approximately 99% visual pass on normal PDFs plus
hostile safety. The final measured visual-page pass is 33.40%, which is 65.60
percentage points below 99%. The weakest high-volume categories are:

- `real-multi-column`: 6.97% visual pass.
- `real-forms`: 16.97% visual pass.
- `real-complex-vector`: 18.18% visual pass.
- `real-font-edge`: 30.00% visual pass.
- `real-scanned`: 37.50% visual pass.
- `real-rtl-text`: 40.00% visual pass.
- `real-cjk-text`: 43.75% visual pass.
- `real-pdfjs-general`: 42.87% visual pass.
- `real-jpeg2000`: 0.00% visual pass on one comparable page.

Recommendation: use Poppler or PDFium for the visual-proof step today, while
using Oxide where it is already a better fit: memory-safe embedding, WASM,
extraction, semantic/layout analysis, C ABI integration, and permissive-license
deployment. Continuing Oxide fidelity work is valid, but the remaining curve is
asymptotic and should be planned as sustained renderer engineering, not a short
cleanup sprint.

### 5.3 Per-Axis Oxide vs Poppler

| Axis | Verdict |
|---|---|
| Fidelity | Poppler wins. Oxide trails on multi-column layout, forms, vector/shading/patterns, images, fonts/CJK/RTL, JPEG2000, and pdf.js edge cases. |
| Safety | Oxide is strong. Final hostile subset was 100/100/100 for crash/timeout/memory safety. CVE-class sweep had no crashes and no memory breaches, but one 20s render timeout. |
| Performance | Mixed. Oxide wins many startup, small render/text, image extraction, and XFA cases. Poppler still wins several raster, vector, image-heavy render, large text extraction, and mature rendering paths. |
| Reach | Oxide wins for WASM/browser, C ABI, single-binary integration, and embeddability. |
| Capabilities | Oxide wins for semantic/layout/table-oriented APIs that Poppler CLI does not directly provide. |
| License/deployment | Oxide wins for permissive-license deployment. Poppler remains GPLv2-family and operationally heavier. |
| Maturity/breadth | Poppler wins. Its renderer is far broader and more mature across long-tail PDFs. |

## 6. Known Limitations and Roadmap

The remaining work is explicit:

1. Multi-column and document layout: improve text positioning, line breaking,
   font metrics, column ordering, and dense long-form rendering.
2. Forms and annotations: broaden widget appearance streams, default appearance
   behavior, button/check/radio states, XFA-adjacent fixtures, and filled form
   parity.
3. Vector/shading/patterns: continue tiling pattern, shading pattern, clipping,
   knockout group, blend mode, soft mask, and path raster parity.
4. Images and scanned PDFs: expand color spaces, masks, decode arrays, JBIG2,
   JPEG2000, CCITT, 1-bit grayscale, and scaling behavior.
5. Fonts and text systems: improve CID/CMap fallback, CJK, RTL, shaping,
   vertical text, missing-font substitution, and font-edge metrics.
6. Encrypted PDFs: add more encryption variants and clean password/user
   permission handling.
7. Parser/malformed corpus: continue clean-failure behavior for corrupt xref,
   missing objects, unsupported filters such as BrotliDecode, malformed streams,
   and cyclic object graphs.
8. Safety timeout: investigate `bug1721218_reduced.pdf`, which is the single
   fresh CVE-class 20s render timeout.
9. Benchmark calibration: keep structural detectors strict. Future threshold
   changes should require visible inspection evidence, not score pressure.

## 7. Final Bottom Line

Oxide is now a credible Rust PDF engine for safe integration, extraction,
semantic analysis, WASM, C ABI embedding, and workflows where deployment,
ownership, and memory-safety matter more than exact Poppler visual parity.

Poppler is still preferable for production visual proof, at least until Oxide
gets much closer to 99% visual pass on normal real-world PDFs and eliminates
the remaining strict timeout discrepancy.
