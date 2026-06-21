# Prompt H Performance Results

Generated: 2026-06-18T17:38:03Z

## Context

- Platform: `Windows-11-10.0.26200-SP0`
- CPU: `13th Gen Intel(R) Core(TM) i5-13500HX`
- Logical CPUs: 20
- Memory: 16104 MB
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`
- Oxide: `E:\wellpdfsdk\target\release\oxide.exe`
- Poppler: `pdftoppm version 26.02.0`
- DPI: 150
- Repeats: 3 (median reported; first run is cold-start proxy)

## Measurements

| class | case | op | engine | median s | cold s | warm median s | peak MB | pages | rc |
|---|---|---|---|---:|---:|---:|---:|---:|---|
| 1-page small text | small_1p_text | info | oxide@1 | 0.0092 | 0.0116 | 0.0092 | 5.2 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | info | poppler | 0.0202 | 0.0715 | 0.0201 | 10.5 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | text_page1 | oxide@1 | 0.0124 | 0.0277 | 0.0122 | 5.7 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | text_page1 | oxide@N | 0.0131 | 0.0131 | 0.0133 | 6.3 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | text_page1 | poppler | 0.0233 | 0.0597 | 0.0228 | 10.9 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | text_all | oxide@1 | 0.0128 | 0.0201 | 0.0125 | 5.7 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | text_all | oxide@N | 0.0111 | 0.0115 | 0.0109 | 6.3 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | text_all | poppler | 0.0230 | 0.0362 | 0.0225 | 10.8 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | render_page1 | oxide@1 | 0.0258 | 0.0258 | 0.0276 | 20.4 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | render_page1 | oxide@N | 0.0254 | 0.0254 | 0.0252 | 20.3 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | render_page1 | poppler | 0.0543 | 0.1092 | 0.0540 | 21.9 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | render_all | oxide@1 | 0.0262 | 0.0244 | 0.0262 | 20.4 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | render_all | oxide@N | 0.0285 | 0.0239 | 0.0334 | 20.3 | 1 | 0,0,0 |
| 1-page small text | small_1p_text | render_all | poppler | 0.0537 | 0.0566 | 0.0529 | 21.9 | 1 | 0,0,0 |
| 100+ page text | multi_120p_text | info | oxide@1 | 0.0127 | 0.0197 | 0.0124 | 5.3 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | info | poppler | 0.0210 | 0.0210 | 0.0256 | 10.5 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | text_page1 | oxide@1 | 0.0172 | 0.0172 | 0.0167 | 5.9 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | text_page1 | oxide@N | 0.0166 | 0.0258 | 0.0165 | 6.4 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | text_page1 | poppler | 0.0245 | 0.0228 | 0.0283 | 10.9 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | text_all | oxide@1 | 0.3479 | 0.3427 | 0.3546 | 6.1 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | text_all | oxide@N | 0.0717 | 0.0689 | 0.0772 | 12.2 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | text_all | poppler | 0.0328 | 0.0328 | 0.0316 | 11.0 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | render_page1 | oxide@1 | 0.0332 | 0.0293 | 0.0335 | 20.8 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | render_page1 | oxide@N | 0.0324 | 0.0324 | 0.0322 | 20.8 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | render_page1 | poppler | 0.0558 | 0.1561 | 0.0549 | 21.9 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | render_all | oxide@1 | 2.2050 | 2.1467 | 2.3146 | 21.9 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | render_all | oxide@N | 2.2508 | 2.3832 | 2.2149 | 21.9 | 120 | 0,0,0 |
| 100+ page text | multi_120p_text | render_all | poppler | 1.7957 | 1.8190 | 1.7917 | 21.9 | 120 | 0,0,0 |
| image-heavy scanned | image_heavy | info | oxide@1 | 0.0121 | 0.0105 | 0.0150 | 6.3 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | info | poppler | 0.0206 | 0.0236 | 0.0202 | 10.6 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | render_page1 | oxide@1 | 0.3615 | 0.3615 | 0.3529 | 67.6 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | render_page1 | oxide@N | 0.3590 | 0.3590 | 0.3532 | 67.6 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | render_page1 | poppler | 0.3299 | 0.3289 | 0.3387 | 32.3 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | render_all | oxide@1 | 0.3721 | 0.3721 | 0.3776 | 67.1 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | render_all | oxide@N | 0.3439 | 0.3456 | 0.3439 | 67.7 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | render_all | poppler | 0.3278 | 0.3278 | 0.3296 | 32.3 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | images | oxide@1 | 0.0527 | 0.0527 | 0.0606 | 7.7 | 1 | 0,0,0 |
| image-heavy scanned | image_heavy | images | poppler | 0.5512 | 0.5867 | 0.5437 | 11.0 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | info | oxide@1 | 0.0110 | 0.0113 | 0.0104 | 5.2 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | info | poppler | 0.0183 | 0.0205 | 0.0177 | 10.5 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | render_page1 | oxide@1 | 0.1210 | 0.1210 | 0.1391 | 29.0 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | render_page1 | oxide@N | 0.1188 | 0.1188 | 0.1190 | 29.0 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | render_page1 | poppler | 0.0540 | 0.0560 | 0.0538 | 19.4 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | render_all | oxide@1 | 0.1180 | 0.1180 | 0.1199 | 29.0 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | render_all | oxide@N | 0.1193 | 0.1201 | 0.1185 | 29.0 | 1 | 0,0,0 |
| vector-heavy | vector_heavy | render_all | poppler | 0.0560 | 0.0560 | 0.0564 | 19.3 | 1 | 0,0,0 |
| font-heavy | font_heavy | info | oxide@1 | 0.0107 | 0.0099 | 0.0109 | 5.6 | 1 | 0,0,0 |
| font-heavy | font_heavy | info | poppler | 0.0212 | 0.0225 | 0.0203 | 10.7 | 1 | 0,0,0 |
| font-heavy | font_heavy | text_page1 | oxide@1 | 0.0304 | 0.0321 | 0.0301 | 12.9 | 1 | 0,0,0 |
| font-heavy | font_heavy | text_page1 | oxide@N | 0.0297 | 0.0291 | 0.0298 | 13.5 | 1 | 0,0,0 |
| font-heavy | font_heavy | text_page1 | poppler | 0.0355 | 0.0495 | 0.0335 | 13.4 | 1 | 0,0,0 |
| font-heavy | font_heavy | text_all | oxide@1 | 0.0286 | 0.0286 | 0.0281 | 12.9 | 1 | 0,0,0 |
| font-heavy | font_heavy | text_all | oxide@N | 0.0283 | 0.0326 | 0.0282 | 13.5 | 1 | 0,0,0 |
| font-heavy | font_heavy | text_all | poppler | 0.0338 | 0.0338 | 0.0344 | 13.3 | 1 | 0,0,0 |
| font-heavy | font_heavy | render_page1 | oxide@1 | 0.2230 | 0.2094 | 0.2288 | 23.3 | 1 | 0,0,0 |
| font-heavy | font_heavy | render_page1 | oxide@N | 0.2445 | 0.2392 | 0.2486 | 22.6 | 1 | 0,0,0 |
| font-heavy | font_heavy | render_page1 | poppler | 0.1283 | 0.1408 | 0.1275 | 31.1 | 1 | 0,0,0 |
| font-heavy | font_heavy | render_all | oxide@1 | 0.2452 | 0.2509 | 0.2414 | 22.6 | 1 | 0,0,0 |
| font-heavy | font_heavy | render_all | oxide@N | 0.2364 | 0.2364 | 0.2358 | 22.6 | 1 | 0,0,0 |
| font-heavy | font_heavy | render_all | poppler | 0.1212 | 0.1215 | 0.1200 | 31.0 | 1 | 0,0,0 |
| large-byte linearized | large_linearized | info | oxide@1 | 0.0182 | 0.0175 | 0.0182 | 8.4 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | info | poppler | 0.0301 | 0.0454 | 0.0293 | 10.9 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | text_page1 | oxide@1 | 0.0439 | 0.0421 | 0.0456 | 8.9 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | text_page1 | oxide@N | 0.0417 | 0.0417 | 0.0486 | 9.5 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | text_page1 | poppler | 0.0303 | 0.0398 | 0.0302 | 10.9 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | text_all | oxide@1 | 8.3380 | 9.0926 | 8.2086 | 12.9 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | text_all | oxide@N | 1.3679 | 1.0889 | 1.3819 | 35.8 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | text_all | poppler | 0.8013 | 1.0418 | 0.7755 | 13.6 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | render_page1 | oxide@1 | 0.2453 | 0.2516 | 0.2451 | 30.0 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | render_page1 | oxide@N | 0.2331 | 0.2331 | 0.2359 | 30.7 | 352 | 0,0,0 |
| large-byte linearized | large_linearized | render_page1 | poppler | 0.1655 | 0.2294 | 0.1644 | 26.2 | 352 | 0,0,0 |
| incremental/XFA form | incremental_xfa | info | oxide@1 | 0.0122 | 0.0130 | 0.0117 | 7.7 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | info | poppler | 0.0237 | 0.0430 | 0.0231 | 10.8 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | text_page1 | oxide@1 | 0.0148 | 0.0138 | 0.0163 | 8.1 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | text_page1 | oxide@N | 0.0161 | 0.0142 | 0.0169 | 8.8 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | text_page1 | poppler | 0.0257 | 0.0270 | 0.0250 | 11.1 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | text_all | oxide@1 | 0.0147 | 0.0145 | 0.0148 | 8.2 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | text_all | oxide@N | 0.0148 | 0.0176 | 0.0146 | 8.8 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | text_all | poppler | 0.0268 | 0.0268 | 0.0321 | 11.1 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | render_page1 | oxide@1 | 0.0257 | 0.0257 | 0.0253 | 23.4 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | render_page1 | oxide@N | 0.0256 | 0.0251 | 0.0326 | 23.3 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | render_page1 | poppler | 0.0616 | 0.0616 | 0.0669 | 22.0 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | render_all | oxide@1 | 0.0231 | 0.0243 | 0.0230 | 23.4 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | render_all | oxide@N | 0.0237 | 0.0250 | 0.0232 | 23.4 | 1 | 0,0,0 |
| incremental/XFA form | incremental_xfa | render_all | poppler | 0.0577 | 0.0571 | 0.0581 | 22.0 | 1 | 0,0,0 |

## Speed Ratios

`speed_ratio_poppler_over_oxide` is Poppler median time divided by Oxide median time; values above 1 mean Oxide is faster.

| case | op | comparison | ratio | winner |
|---|---|---|---:|---|
| font_heavy | info | oxide@1 | 1.970 | oxide |
| font_heavy | render_all | oxide@1 | 0.494 | poppler |
| font_heavy | render_all | oxide@N | 0.513 | poppler |
| font_heavy | render_page1 | oxide@1 | 0.575 | poppler |
| font_heavy | render_page1 | oxide@N | 0.525 | poppler |
| font_heavy | text_all | oxide@1 | 1.183 | oxide |
| font_heavy | text_all | oxide@N | 1.196 | oxide |
| font_heavy | text_page1 | oxide@1 | 1.170 | oxide |
| font_heavy | text_page1 | oxide@N | 1.198 | oxide |
| image_heavy | images | oxide@1 | 10.450 | oxide |
| image_heavy | info | oxide@1 | 1.695 | oxide |
| image_heavy | render_all | oxide@1 | 0.881 | poppler |
| image_heavy | render_all | oxide@N | 0.953 | poppler |
| image_heavy | render_page1 | oxide@1 | 0.913 | poppler |
| image_heavy | render_page1 | oxide@N | 0.919 | poppler |
| incremental_xfa | info | oxide@1 | 1.949 | oxide |
| incremental_xfa | render_all | oxide@1 | 2.496 | oxide |
| incremental_xfa | render_all | oxide@N | 2.433 | oxide |
| incremental_xfa | render_page1 | oxide@1 | 2.399 | oxide |
| incremental_xfa | render_page1 | oxide@N | 2.409 | oxide |
| incremental_xfa | text_all | oxide@1 | 1.828 | oxide |
| incremental_xfa | text_all | oxide@N | 1.810 | oxide |
| incremental_xfa | text_page1 | oxide@1 | 1.735 | oxide |
| incremental_xfa | text_page1 | oxide@N | 1.595 | oxide |
| large_linearized | info | oxide@1 | 1.655 | oxide |
| large_linearized | render_page1 | oxide@1 | 0.675 | poppler |
| large_linearized | render_page1 | oxide@N | 0.710 | poppler |
| large_linearized | text_all | oxide@1 | 0.096 | poppler |
| large_linearized | text_all | oxide@N | 0.586 | poppler |
| large_linearized | text_page1 | oxide@1 | 0.690 | poppler |
| large_linearized | text_page1 | oxide@N | 0.726 | poppler |
| multi_120p_text | info | oxide@1 | 1.645 | oxide |
| multi_120p_text | render_all | oxide@1 | 0.814 | poppler |
| multi_120p_text | render_all | oxide@N | 0.798 | poppler |
| multi_120p_text | render_page1 | oxide@1 | 1.681 | oxide |
| multi_120p_text | render_page1 | oxide@N | 1.721 | oxide |
| multi_120p_text | text_all | oxide@1 | 0.094 | poppler |
| multi_120p_text | text_all | oxide@N | 0.458 | poppler |
| multi_120p_text | text_page1 | oxide@1 | 1.426 | oxide |
| multi_120p_text | text_page1 | oxide@N | 1.476 | oxide |
| small_1p_text | info | oxide@1 | 2.196 | oxide |
| small_1p_text | render_all | oxide@1 | 2.053 | oxide |
| small_1p_text | render_all | oxide@N | 1.883 | oxide |
| small_1p_text | render_page1 | oxide@1 | 2.102 | oxide |
| small_1p_text | render_page1 | oxide@N | 2.139 | oxide |
| small_1p_text | text_all | oxide@1 | 1.800 | oxide |
| small_1p_text | text_all | oxide@N | 2.068 | oxide |
| small_1p_text | text_page1 | oxide@1 | 1.883 | oxide |
| small_1p_text | text_page1 | oxide@N | 1.776 | oxide |
| vector_heavy | info | oxide@1 | 1.659 | oxide |
| vector_heavy | render_all | oxide@1 | 0.474 | poppler |
| vector_heavy | render_all | oxide@N | 0.469 | poppler |
| vector_heavy | render_page1 | oxide@1 | 0.446 | poppler |
| vector_heavy | render_page1 | oxide@N | 0.454 | poppler |
