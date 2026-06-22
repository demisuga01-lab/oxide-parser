# Renderer Benchmark 0A Report

Generated: 2026-06-22T16:09:31.372049+00:00

## Scope

- Files run: 265
- Normal files: 205
- Hostile files: 60
- Real-world files: 75
- Visual pages compared: 245
- Full target: 1000 real PDFs / 10000 rendered pages
- DPI: 144
- Page cap per file: 3

## Backends

- Oxide: `target\release\oxide.exe`
- Poppler: `{'pdfinfo': 'pdfinfo version 26.02.0\nCopyright 2005-2026 The Poppler Developers - http://poppler.freedesktop.org\nCopyright 1996-2011, 2022 Glyph & Cog, LLC', 'pdftoppm': 'pdftoppm version 26.02.0\nCopyright 2005-2026 The Poppler Developers - http://poppler.freedesktop.org\nCopyright 1996-2011, 2022 Glyph & Cog, LLC'}`
- PDFium: not available; skipped cleanly

## Results

- Weighted score: **87.19**
- Tier: **Tier 0**
- Visual pass: **78.37%**
- Hostile crash-free: **100.0%**
- Hostile timeout-safe: **100.0%**
- Hostile memory-bounded: **100.0%**
- Median speed ratio Poppler/Oxide: **2.7069**
- Determinism: {'sampled_files': 24, 'stable_files': 24, 'stable_percent': 100.0, 'unstable': []}

## Sub-Scores

- safety: 100.0%
- page_count_dimensions: 100.0%
- visual_match: 78.37%
- text_font_correctness_proxy: 68.67%
- image_color_correctness_proxy: 78.95%
- performance_memory: 100.0%

## Failure Breakdown

- page:pixel_difference: 18
- page:perceptual_hash_distance: 14
- perceptual_hash_distance: 14
- pixel_difference: 12
- blank_page_mismatch: 9
- page:blank_page_mismatch: 9
- page:large_region_difference: 6
- low_ssim: 5
- page:low_ssim: 5
- large_region_difference: 4
- poppler_render_failed: 4
- oxide_render_failed: 2
- page:rendered_page_missing: 2
- rendered_page_missing: 2
- edge_or_text_shift: 1
- page:edge_or_text_shift: 1

## Category Breakdown

| category | files | file pass % | visual pages | visual pass % |
| --- | ---: | ---: | ---: | ---: |
| hostile-bad-filter | 6 | 100.0 | 0 | None |
| hostile-huge-length | 6 | 100.0 | 0 | None |
| hostile-huge-page | 6 | 100.0 | 0 | None |
| hostile-launch-action | 6 | 100.0 | 0 | None |
| hostile-missing-eof | 6 | 100.0 | 0 | None |
| hostile-openaction-js | 6 | 100.0 | 0 | None |
| hostile-random | 6 | 100.0 | 0 | None |
| hostile-truncated | 6 | 100.0 | 0 | None |
| hostile-uri-action | 6 | 100.0 | 0 | None |
| hostile-wrong-startxref | 6 | 100.0 | 0 | None |
| large-files | 10 | 100.0 | 30 | 100.0 |
| real-cjk-text | 10 | 50.0 | 13 | 61.54 |
| real-complex-vector | 12 | 16.67 | 15 | 13.33 |
| real-encrypted | 6 | 50.0 | 4 | 75.0 |
| real-forms | 12 | 58.33 | 14 | 57.14 |
| real-jpeg2000 | 2 | 0.0 | 1 | 0.0 |
| real-large-multipage | 3 | 100.0 | 9 | 100.0 |
| real-multi-column | 10 | 40.0 | 17 | 29.41 |
| real-rtl-text | 5 | 40.0 | 5 | 40.0 |
| real-scanned | 9 | 33.33 | 9 | 33.33 |
| real-text-basic | 6 | 100.0 | 8 | 100.0 |
| synthetic-forms | 8 | 100.0 | 8 | 100.0 |
| synthetic-geometry | 24 | 75.0 | 24 | 75.0 |
| synthetic-graphics | 42 | 100.0 | 42 | 100.0 |
| synthetic-images | 16 | 100.0 | 16 | 100.0 |
| synthetic-text | 18 | 100.0 | 18 | 100.0 |
| synthetic-transparency | 12 | 100.0 | 12 | 100.0 |

## Blocking Findings

- `tests/corpus/pdfs/pdfjs/IdentityToUnicodeMap_charCodeOf.pdf` page -: large_region_difference (real-cjk-text)
- `tests/corpus/pdfs/pdfjs/IdentityToUnicodeMap_charCodeOf.pdf` page 1: large_region_difference (real-cjk-text)
- `tests/corpus/pdfs/pdfjs/function_based_shading.pdf` page -: rendered_page_missing (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/function_based_shading.pdf` page 1: rendered_page_missing (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/issue18032.pdf` page -: blank_page_mismatch (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/issue18032.pdf` page 1: blank_page_mismatch (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/smask_alpha_oob.pdf` page -: blank_page_mismatch (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/smask_alpha_oob.pdf` page 1: blank_page_mismatch (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/tiling_patterns_variations.pdf` page -: blank_page_mismatch (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/tiling_patterns_variations.pdf` page 1: blank_page_mismatch (real-complex-vector)
- `tests/corpus/pdfs/pdfjs/bug1802506.pdf` page -: blank_page_mismatch (real-forms)
- `tests/corpus/pdfs/pdfjs/bug1802506.pdf` page 1: blank_page_mismatch (real-forms)
- `tests/corpus/pdfs/pdfjs/checkbox_no_appearance.pdf` page -: blank_page_mismatch (real-forms)
- `tests/corpus/pdfs/pdfjs/checkbox_no_appearance.pdf` page 1: blank_page_mismatch (real-forms)
- `tests/corpus/pdfs/pdfjs/prefilled_f1040.pdf` page -: large_region_difference (real-forms)
- `tests/corpus/pdfs/pdfjs/prefilled_f1040.pdf` page 1: large_region_difference (real-forms)
- `tests/corpus/pdfs/pdfjs/prefilled_f1040.pdf` page 2: large_region_difference (real-forms)
- `tests/corpus/pdfs/pdfjs/bug_jpx.pdf` page -: rendered_page_missing (real-jpeg2000)
- `tests/corpus/pdfs/pdfjs/bug_jpx.pdf` page 1: rendered_page_missing (real-jpeg2000)
- `tests/corpus/pdfs/pdfjs/freeculture.pdf` page -: large_region_difference (real-multi-column)
- `tests/corpus/pdfs/pdfjs/freeculture.pdf` page 1: large_region_difference (real-multi-column)
- `tests/corpus/pdfs/pdfjs/freeculture.pdf` page 2: large_region_difference (real-multi-column)
- `tests/corpus/pdfs/pdfjs/ThuluthFeatures.pdf` page 1: major_color_or_inversion (real-rtl-text)
- `tests/corpus/pdfs/pdfjs/issue5801.pdf` page -: blank_page_mismatch (real-rtl-text)
- `tests/corpus/pdfs/pdfjs/issue5801.pdf` page 1: blank_page_mismatch (real-rtl-text)
- `tests/corpus/pdfs/pdfjs/image-rotated-black-white-ratio.pdf` page -: large_region_difference (real-scanned)
- `tests/corpus/pdfs/pdfjs/image-rotated-black-white-ratio.pdf` page 1: large_region_difference (real-scanned)
- `tests/corpus/pdfs/pdfjs/images.pdf` page -: blank_page_mismatch (real-scanned)
- `tests/corpus/pdfs/pdfjs/images.pdf` page 1: blank_page_mismatch (real-scanned)
- `tests/corpus/pdfs/pdfjs/images_1bit_grayscale.pdf` page -: blank_page_mismatch (real-scanned)
- `tests/corpus/pdfs/pdfjs/images_1bit_grayscale.pdf` page 1: blank_page_mismatch (real-scanned)
- `tests/corpus/pdfs/pdfjs/xobject-image.pdf` page -: blank_page_mismatch (real-scanned)
- `tests/corpus/pdfs/pdfjs/xobject-image.pdf` page 1: blank_page_mismatch (real-scanned)

## Scale Caveat

Tier is rated only at this run's corpus scale; Tier 3 requires 1,000+ real PDFs and 10,000+ rendered pages.
This report must not be used as a full Tier-3 claim until the corpus is expanded to the full target.
