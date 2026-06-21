# Renderer Benchmark 0A — Recalibration + Confirmed Fixes (this round)

Generated: 2026-06-16. This round (Prompt 0A "Fix") did three things: **recalibrated
the harness measurement** so renderer-vs-renderer comparison is honest, **fixed two
confirmed real defects** (huge-page allocation abort; the dimension "bug"), and
**re-ran 0A** to establish the true baseline. It deliberately did NOT broadly change
the renderer to chase the visual number — that is Prompt B, fed by the categorized
list below.

## Part A — Harness recalibration (measurement, not renderer)

**What the thresholds WERE (single strict profile applied to 0A):** SSIM ≥ 0.995,
differing-pixel ≤ 0.5 %, MAE ≤ 1.5, edge_mae ≤ 0.035, large_region ≤ 0.18,
phash ≤ 10, dimension_rounding ≤ 2 px. That is near-exact-match tolerance — two
*different but correct* renderers antialias/hint text differently and routinely score
SSIM 0.95–0.99 with 1–6 % differing pixels on correct text pages, so this profile
counted correct pages as failures. (The 0A spec asked for a looser renderer-vs-renderer
profile; only the strict one had been implemented.)

**What they ARE now (distinct loose renderer-vs-renderer profile; 0B's strict
`compression` profile untouched):**

| metric | text-heavy | flat/vector | role |
| --- | --- | --- | --- |
| SSIM floor | 0.93 | 0.95 | pixel/AA (loose) |
| differing-pixel % | ≤ 8 % | ≤ 3 % | pixel/AA (loose) |
| MAE | ≤ 6 | ≤ 3 | pixel/AA (loose) |
| dimension rounding | 4 px | 4 px | DPI-rounding tolerance |
| large_region | 0.45 | 0.45 | **structural (strict)** |
| edge_mae | 0.10 | 0.06 | **structural (strict)** |
| phash | 14 | 14 | **structural (strict)** |
| blank_delta | 0.06 | 0.06 | **structural (strict)** |

The philosophy: **loose on global AA noise, STRICT on structural/semantic
differences.** The structural detectors (blank-page, large-region, dimension,
edge-map, colour inversion) stay strict — they catch a missing image, blanked page,
shifted/garbled text, or wrong page size through the loosened pixel tolerance.

**Two-profile separation:** `thresholds("renderer", text_heavy=…)` (0A) vs
`thresholds("compression")` (0B). 0B was already strict and is unchanged.

**Dimension-normalization ordering (A.2):** when two pages are a genuine size
mismatch (beyond DPI rounding), the larger is now nearest-neighbour-resampled onto the
smaller's grid BEFORE pixel/SSIM metrics, so a dimension mismatch no longer shears the
content and cascade-inflates the pixel-diff bucket. Each failing page is attributed to
its **primary cause** (a real dimension mismatch is reported alone; otherwise structural
reasons rank ahead of pixel/AA), with the rest recorded as `diagnostic_reasons`.

**SSIM corroboration refinement (A.3 follow-up):** global SSIM is unreliable on
sparse/mostly-white pages (its variance-normalised form craters when one text line sits
on a large white field), so two visually-identical sparse pages scored SSIM ~0.94 purely
from AA. `low_ssim` is now a failure only when **corroborated** by a real pixel or edge
difference. This removed false-fails on visually-identical sparse pages
(`synthetic_geometry_rotate_000`, etc.) without letting any real bug through (real bugs
spike pixel% and edge_mae together, so they remain caught).

**Sanity check (A.3):** a solid-colour page (`synthetic_graphics_000_rgb-red`), a
correct text page (`doc_1_3_pages`, `two_paragraphs`), and a visually-identical sparse
page (`rotate_000`) now PASS; the real bugs (`tracemonkey` blank, `synthetic_image`
interpolation, `synthetic_transparency` blend) still FAIL. The ruler measures a known
length correctly.

**Performance fix:** `pixel_metrics` was a triple-nested per-channel Python loop
(~1.2 s/page); rewritten to slice comparable rows as `bytes` with a squared-delta LUT
(~0.42 s/page, 3× faster, byte-identical output).

## Part C — The "dimension bug" was a MediaBox-vs-CropBox default difference

The 16 "dimension_mismatch" failures were **not a renderer bug.** Oxide renders the
**CropBox** (MediaBox ∩ CropBox) — the PDF spec / common-viewer default that pdfinfo's
reported page size, PDFium, Chrome, and `pdftocairo` all agree on. Poppler's `pdftoppm`
**defaults to the MediaBox** (`-cropbox` is opt-in; confirmed via `pdftoppm -h`). Proof:

| file | Oxide (CropBox) | pdftoppm default (MediaBox) | pdftoppm `-cropbox` |
| --- | --- | --- | --- |
| bug1802506 | 536×291 | 1224×1584 | 536×**292** |
| rotate_003 | 1504×1144 | 1584×1224 | 1504×1144 |

Oxide matches `pdftoppm -cropbox` exactly (±1 px rounding). **Fix (per user decision):
align the harness — pass `-cropbox` to `pdftoppm` so both render the same region. Oxide's
spec-aligned CropBox default is unchanged.** All 16 dimension mismatches resolved. The
rotate files then exposed a *real* content bug (text mirrored under /Rotate 270) that the
dimension confusion had masked — handed to Prompt B.

Regression: `crates/engine/tests/render_resource_limits.rs` asserts CropBox-derived
dimensions (`cropbox_drives_page_dimensions_matching_poppler_cropbox`,
`rotated_cropbox_swaps_dimensions`).

## Part B — Huge-page allocation abort (real safety blocker, FIXED)

A hostile page with `/MediaBox [0 0 200000 200000]` at 144 DPI = 400000×400000 px;
the 4-byte buffer is ~640 GB, and `PixelBuffer::new`'s `vec![0u8; len]` aborted the
process. The Mega-12 `max_render_pixels` cap existed **only in the server**; the
engine/CLI render path was unguarded.

**Fix:** `ContentEngine::page_viewport` (the single chokepoint for raster/SVG/PS) now
validates the final post-DPI/rotation pixel count against `max_render_pixels()`
(default 300 MP, override via `OXIDE_MAX_RENDER_PIXELS` or the new CLI
`render --max-render-pixels`) and returns a clean `OxideError::ResourceLimit` BEFORE any
allocation. The CLI's per-page loop already skips-with-warning on error, so the process
survives and stays usable. Server `From<OxideError>` maps it to `ResourceLimit`.

Regression: engine `huge_mediabox_is_rejected_cleanly_not_aborted`,
`*_at_viewport_before_allocation`, `extreme_dpi_on_normal_page_is_capped`; CLI
`render_rejects_huge_page_without_abort`. Hostile crash-free: **90 % → 100 %**.

## Part D — The honest re-baseline (run-0a, recalibrated)

| metric | pre-calibration | recalibrated (loose only) | + SSIM-corroboration (final) |
| --- | --- | --- | --- |
| **Visual pass (Oxide vs Poppler)** | **2.05 %** | 20.41 % | **34.02 %** |
| Weighted score | 44.74 | 56.70 | **64.03** |
| Tier | 0 | 0 | **0** |
| Hostile crash-free | 90 % | 100 % | **100 %** |
| Hostile timeout-safe | 100 % | 100 % | 100 % |
| Hostile memory-bounded | 100 % | 100 % | 100 % |
| Median speed ratio (Poppler/Oxide) | 1.91 | 1.93 | 1.93 (Oxide ~1.9× faster) |
| Peak Oxide memory | 64 MB | 64 MB | 64 MB |
| Determinism | 100 % bit-stable | 100 % bit-stable | 100 % bit-stable* |

> *Determinism sampling was disabled in the final run (it triples render cost on a
> contended machine); it was 100 % bit-stable (24/24, 30/30) in the prior recalibrated
> runs and is unaffected by these measurement/cap/CropBox changes, so it is carried
> forward. Corpus: 265 files (205 normal / 60 hostile), 244 comparable pages, 144 DPI,
> 3-page cap. File-level pass: 73/205 normal (35.6 %).
>
> The 2.05 % → 34.02 % jump is real (AA-noise false-fails removed), but the number stays
> below "near-100 %" because the recalibration EXPOSED genuine, severe renderer bugs the
> old strict thresholds had buried alongside AA noise. Clean categories now pass well
> (real-text-basic 100 %, synthetic-text 83 %, real-large-multipage 100 %, synthetic-forms
> 100 %); the failing categories are real defects (synthetic-images 0 %, synthetic-
> transparency 0 %, scanned 11 %).

**The jump from 2.05 % is real (AA-noise false-fails removed), but the number stays
modest because the recalibration EXPOSED genuine, severe renderer bugs** that the old
strict thresholds had buried in the same bucket as AA noise. That is the honest finding:
Oxide has real rendering defects, listed below for Prompt B.

### Remaining failures by PRIMARY cause (the input to Prompt B)

(final run, primary cause per failing page; 89 of the failures are genuine renderer
bugs, the rest borderline pixel/AA)

| primary reason | pages | files | kind | example fixtures |
| --- | ---: | ---: | --- | --- |
| pixel_difference | 53 | 31 | mostly AA | `large_NNN_pages` (ssim ~0.99 — borderline), CMYK solids |
| blank_page_mismatch | 32 | 28 | **REAL** | `tracemonkey`, `form_160f`, `TAMReview`, `synthetic_transparency_*` |
| large_region_difference | 26 | 23 | **REAL** | `synthetic_image_*` (interpolation), `freeculture`, `images_1bit_grayscale` |
| perceptual_hash_distance | 24 | 22 | **REAL** | `synthetic_geometry_rotate_*` (mirrored text), `ArabicCIDTrueType`, `mixedfonts` |
| low_ssim (corroborated) | 22 | 22 | mixed | real text-fidelity; `noembed-eucjp/sjis` |
| rendered_page_missing | 3 | 3 | **REAL** | `bug_jpx`, `function_based_shading`, `encrypted-attachment` |
| edge_or_text_shift | 3 | 3 | **REAL** | `ThuluthFeatures`, `issue14297` |
| major_color_or_inversion | 1 | 1 | **REAL** | `ccitt_EndOfBlock_false` |

## Prompt-B input: genuine renderer bugs (visually confirmed)

1. **Blank / near-blank real pages** — `tracemonkey.pdf` renders entirely white;
   `form_160f.pdf` renders the grid but garbles/overlaps glyphs; `TAMReview`,
   `openoffice`, `bug1802506` (form). Likely font/encoding/text-positioning. **Highest
   priority** (whole real-world PDFs unreadable).
2. **Image upscaling uses smooth/bilinear interpolation when `/Interpolate` is absent**
   — PDF default is nearest-neighbour. `synthetic_image_*` (3×3 image scaled up) renders
   as a blurred gradient instead of crisp colour blocks. 16 files, identical cause.
3. **Transparency: constant alpha (`ca`/`CA`) + Screen/Multiply blend modes wrong** —
   `synthetic_transparency_*` washes the second rectangle out to near-white instead of
   blending; no visible overlap. ~12 files.
4. **Page rotation mirrors content** — under `/Rotate 270`, text renders mirror-imaged
   (`synthetic_geometry_rotate_001/003/005…`, the odd indices = 90°/270°). Dimensions are
   now correct (CropBox); the content transform has a sign error.
5. **Missing content** — `function_based_shading` (type-1 shading), `bug_jpx` (JPX),
   `encrypted-attachment` — page renders empty/missing.
6. **Font/glyph fidelity** — `noembed-eucjp/sjis`, `ArabicCIDTrueType`, `ThuluthFeatures`,
   `mixedfonts`, CJK — non-embedded/CID/complex-script text renders wrong or garbled.

### Now-passing AA noise (correctly PASS after recalibration)
Solid-colour pages, correct text pages, and visually-identical sparse pages
(`synthetic_geometry_rotate_000` 0°, `doc_1_3_pages`, `two_paragraphs`,
`synthetic_graphics_000`). Borderline-but-minor: thin **dash/stroke weight** differs
(`synthetic_clip_curve_*` — Oxide draws a lighter/thinner dashed border); CMYK solids
differ ~3 % from a slightly different CMYK→RGB conversion. These are minor real
differences, reported honestly rather than tuned away.

All offending files are already in-repo (`renderer-benchmark/corpus/synthetic/` and
`tests/corpus/pdfs/`); no separate fixture copy needed.

---

# Benchmark Fix Prompt B — Fixing the genuine residual failures

This round fixed three root-cause renderer-bug groups from the Prompt-A list,
verified each against its fixture and Poppler, and re-ran 0A. Font groups were
deliberately deferred (large/risky). `cargo test --workspace` + clippy stay green.

## Triage (confirmed-real, grouped by root cause)

| group | files | confirmed cause | decision |
| --- | --- | --- | --- |
| Image magnify interpolation | 16 (synthetic_image_*) | `paint_image` always bilinear; PDF default = nearest when magnifying | **FIXED** |
| /Rotate 90/270 mirroring | 12 (synthetic_geometry_rotate_*) | 90/270 device matrices had det>0 (rotation without the y-flip) → mirrored | **FIXED** |
| Transparency gamma + Screen-white-out | 12 (synthetic_transparency_*) | linear-light compositing diverged from Poppler; Screen-over-opaque-white quirk | **PARTIAL** (gamma fixed; Screen quirk + overlap deferred) |
| Type1 `/FontFile` blank text | tracemonkey, TAMReview, … | no Type1 charstring interpreter (only TrueType + bare-CFF supported) | **DEFERRED** (whole feature) |
| Embedded TrueType garbling | form_160f, openoffice | code→glyph mapping for subset TrueType | **DEFERRED** (regression-risky) |
| CID/CJK encoding | mixedfonts, noembed-eucjp/sjis, ArabicCIDTrueType | CID/CMap + non-embedded CJK | **DEFERRED** |

## Fixes (root cause → verification)

1. **Image nearest-neighbour on magnify** (`image_painter.rs`). Added `magnifying()`
   (dst extent ≥ source on both axes) + `nearest_sample()`; bilinear kept for
   minification so scanned downscaling stays smooth. Verified: synthetic_image_000
   renders crisp colour blocks identical to Poppler (was a blurred gradient).
   Regression test `magnified_image_uses_nearest_neighbour_blocks`.
   **Recovered: synthetic-images 0% → 100% (16 files).**

2. **/Rotate 90/270 transform** (`transform.rs`). Corrected the 90/270 matrices to
   keep the page→device y-flip reflection (det < 0): 90 → (0,s,s,0,-y1·s,-x1·s),
   270 → (0,-s,-s,0,y2·s,x2·s). Verified: rotate_001 (90°) and rotate_003 (270°)
   text reads correctly, pixel-aligned with Poppler (was mirror-imaged). Regression
   tests `rotation_transforms_are_proper_not_mirrored`,
   `rotation_270_corner_orientation_is_clockwise`.
   **Recovered: synthetic-geometry 50% → 75% (the 90°/270° pages).**

3. **sRGB compositing** (`buffer.rs::blend_pixel`, `color.rs::alpha_composite`).
   Switched alpha/blend compositing from linear light to sRGB to match Poppler
   (user decision; reverts the Mega-24 gamma-correct property). Verified pixel-
   exact: semi-transparent red 255,140,140 (was 255,196,196); Multiply red/gray
   204,0,0. Updated all gamma-expectation tests (buffer/color/page_renderer/
   transparency) to sRGB midpoints; regenerated 2 goldens (basicapi, form_160f),
   confirmed neutral vs Poppler (~25.6 dB each). **Recovered: real-scanned 11% →
   33%; transparency pages went ssim 0.50 → 0.99 (though still failing — see
   deferred).**

## Re-baseline (Prompt-B, same recalibrated thresholds / corpus / 144 DPI)

| metric | Prompt A | **Prompt B** |
| --- | --- | --- |
| Visual pass (Oxide vs Poppler) | 34.02 % | **43.85 %** (107/244 pages) |
| Weighted score | 64.03 | **69.36** |
| File-level pass (normal) | 73/205 | **89/205 (43.4 %)** |
| Hostile crash-free / timeout / memory | 100 % | 100 % |
| Median speed (Poppler/Oxide) | 1.93× | 1.95× |
| Determinism | bit-stable | bit-stable (no nondeterminism introduced) |
| Tier | 0 | 0 |

Per-category gains (rest unchanged): synthetic-images 0→100 %, synthetic-geometry
50→75 %, real-scanned 11→33 %. No category regressed.

## Remaining failures (honest, with fixtures)

| primary reason | pages | kind |
| --- | ---: | --- |
| pixel_difference | 30 | mostly borderline AA (large_NNN_pages ssim ~0.99; CMYK solids ~3 %) |
| low_ssim | 22 | mixed real text-fidelity + borderline |
| large_region_difference | 18 | synthetic_transparency overlap; image-mask/colorspace cases |
| perceptual_hash_distance | 17 | DEFERRED fonts (CID/CJK/garbled TrueType) |
| blank_page_mismatch | 15 | DEFERRED Type1 `/FontFile` (tracemonkey, TAMReview) |
| rendered_page_missing | 3 | bug_jpx (JPX), function_based_shading, encrypted-attachment |
| edge_or_text_shift | 3 | ThuluthFeatures, issue14297 (complex-script fonts) |

## Deferred (large/risky — for a future round, fixtures in-repo)

- **Type1 `/FontFile` glyph rendering** — requires a Type1 charstring interpreter
  (eexec decrypt + Type1 charstring ops). Blanks tracemonkey/TAMReview. Biggest
  single remaining win but a whole feature, not a bug fix.
- **Embedded TrueType subset garbling** (form_160f, openoffice) — code→glyph
  mapping; touching it risks the many text PDFs that already render correctly.
- **CID/CJK + complex-script** (mixedfonts, noembed-eucjp/sjis, ArabicCIDTrueType,
  ThuluthFeatures) — CMap/CID + shaping.
- **Transparency Screen-over-opaque-white + two-object overlap** — Poppler/Splash
  applies Multiply directly on the page (204,0,0, matched) but renders
  Screen-over-white as Normal (115,115,255), which the spec formula does not
  produce. A blanket page-level-Normal gate breaks Multiply, so it was reverted;
  replicating Splash's exact convention is a contained-but-fiddly follow-up. The
  transparency pages are now visually near-correct (2/3 regions pixel-exact).
- **JPX / function-based shading / encrypted-attachment** — missing/partial decode.

## Tier statement

Oxide does **not** yet meet Tier 2 (≥99 % visual on normal PDFs) on this corpus —
43.85 % visual pass. Safety, performance, and determinism ARE Tier-2-grade (100 %
crash-free / timeout-safe / memory-bounded on the hostile corpus, ~1.9× faster
than Poppler, bit-stable). The gap to Tier 2 is renderer fidelity, dominated by
the deferred **font groups** (Type1/TrueType/CID) — fixing them is the highest-
leverage next step. Tier 3 (WellPDF visual-proof backend) additionally requires
the corpus to expand to 1,000+ real PDFs / 10,000+ rendered pages (currently
75 real / 244 pages) — a corpus-scale requirement separate from renderer
correctness.
