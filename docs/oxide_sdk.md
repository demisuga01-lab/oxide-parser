# Oxide Enterprise SDK Capstone

This is the capstone record for the 11-prompt enterprise SDK arc. It is a
measurement and release-readiness document, not a new feature plan.

Oxide is a pure-Rust, self-hostable PDF SDK spanning structured extraction,
authoring, editing, structural operations, PDF/A validation and conversion,
digital signatures, OCR, and embedding surfaces across Rust, CLI, C ABI,
WASM, and HTTP server.

## Fresh Provenance

Measurements were taken on 2026-06-22 on Windows 11 with a 20-core host. The
benchmark JSON records base commit `60b8f60` and `dirty: true` because this
capstone commit itself adds the integration tests, benchmark artifacts, and a
PDF/A trailer-ID fix found during veraPDF validation.

GA6 final-gate reruns were taken on 2026-06-23 after GA5 commit `3042550`.
The worktree was still dirty from pre-existing unrelated source changes, so the
final release-gate verdict is evidence for a release candidate from a clean
branch, not permission to tag this exact dirty worktree.

The post-GA hardening consolidation adds continuous fuzzing, differential
fuzzing, property tests, grammar-aware deep fuzzing, dependency audit gates, and
an audit-readiness packet. The current authoritative security/robustness
posture is `docs/security/posture.md`.

Tools used:

| Tool | Version |
| --- | --- |
| Oxide CLI | `oxide 0.1.0`, OCR feature compiled in |
| qpdf | 12.3.2 |
| Poppler | 26.02.0 |
| veraPDF | 1.30.2 |
| Tesseract | 5.5.0.20241111 |
| PyMuPDF | 1.27.2.3 |
| Python | 3.14.3 |

Docling was not installed locally, so no Docling numbers are reported as local
measurements.

## Integration Results

The capstone integration test suite is in
`crates/engine/tests/capstone_integration.rs`.

| Workflow | Result |
| --- | --- |
| Create PDF -> watermark edit -> encrypt -> decrypt -> extract | Passed. Authored text and watermark survived the full flow. |
| PDF/A conversion -> linearize -> sign -> verify | Passed at the library level. The signed output verifies cryptographically. GA Prompt 1 fixed the later qpdf hint-table warnings. |
| Fill form -> flatten -> redact | Passed. Filled values were visible, fields were removed by flattening, and the redacted value was no longer extractable. |

Cross-surface extraction consistency is recorded in
`docs/capstone_surface_consistency.json`.

| Surface | Fixture | Result |
| --- | --- | --- |
| Rust library | `basicapi.pdf`, page 1 | SHA-256 `43c9f91f575550430b4790e76fa65f8e6fbdbece01e517bc0eeb414c11a29e10` |
| CLI | Same | Same hash |
| C ABI | Same | Same hash |
| HTTP server | Same | Same hash |

The server was started by the capstone surface script and returned HTTP 200 for
the same page text. The C ABI example was compiled with MSVC against the release
library.

## External Validation

| Output | External check | Result |
| --- | --- | --- |
| Authored PDF | `qpdf --check`, Poppler render/extract | Clean. Poppler emitted font-substitution warnings for Symbol/ArialUnicode but exited successfully and extracted text. |
| PDF/A-1b conversion | veraPDF 1.30.2 | PASS |
| PDF/A-2b conversion | veraPDF 1.30.2 | PASS |
| PDF/A-2a conversion | veraPDF 1.30.2 | PASS after GA Prompt 3 |
| PDF/A-3b conversion | veraPDF 1.30.2 | PASS after GA Prompt 3 |
| PDF/A-3a conversion | veraPDF 1.30.2 | PASS after GA Prompt 3 |
| PDF/UA best-effort improvement | veraPDF 1.30.2 UA-1 | Not claimed compliant; veraPDF still reports semantic tagging/PDF-UA metadata requirements |
| Signed PDF | `qpdf --check`, Oxide verify-sig | Clean. One RSA/SHA-256 signature reported cryptographically valid with whole-file coverage. |
| Optimized PDF | `qpdf --check` | Clean |
| AES-256 encrypted PDF | `qpdf --check --password=capstone` | Clean, AESv3 reported |
| Linearized PDF | `qpdf --check`, `qpdf --show-linearization` | Clean after GA Prompt 1 across the supported fixture breadth. |

The Prompt 11 linearization warnings were:

```text
object count mismatch for page 0: hint table 25, computed 23
page length mismatch for page 1: hint table 303, computed 913
page 1 shared object 14 in hint table but not computed list
page length mismatch for page 2: hint table 651, computed 68403
page 2 shared object 15 in hint table but not computed list
qpdf: operation succeeded with warnings
```

GA Prompt 1 mapped these warnings to first-page dependency grouping: later
page dictionaries were inserted into the first-page closure before traversal
stopped. The fixed grouping is qpdf-clean on `minimal.pdf`, `flate.pdf`,
`multi_stream.pdf`, `basicapi.pdf`, `tracemonkey.pdf`, and `form_160f.pdf`.
See `docs/linearization_qpdf_clean_ga1.md`.

During capstone validation, veraPDF initially rejected PDF/A output because
the converted file did not have a non-empty trailer `/ID`. The conversion path
now writes a deterministic trailer ID when the source lacks one, and the PDF/A
validator reports missing or empty IDs as violations.

GA Prompt 3 broadened `PdfAProfile` to PDF/A-2a, PDF/A-3b, and PDF/A-3a. The
generated compliance example passes qpdf and bundled veraPDF 1.30.2 for
1b/2b/2a/3b/3a. PDF/A-3 FileSpecs are preserved and repaired with
`/AFRelationship` when missing. PDF/UA remains assistive best-effort rather
than a certification claim; veraPDF UA-1 still catches content-tagging and
metadata requirements that require richer semantic tagging.

## Extraction Benchmark

Full report: `docs/parser_benchmark.md`. Raw data:
`extraction-benchmark/results/results.json`.

Key text extraction scores:

| Document | Mode | Oxide char-acc | PyMuPDF | pdftotext | Oxide order |
| --- | --- | ---: | ---: | ---: | ---: |
| figure | digital | 0.598 | 0.990 | 0.833 | 1.000 |
| paper | digital | 0.993 | 0.998 | 0.956 | 1.000 |
| paper_scanned | scanned | 0.942 | 0.000 | 0.000 | 1.000 |
| report_multicol | digital | 0.605 | 0.669 | 0.596 | 1.000 |
| tables | digital | 1.000 | 0.877 | 0.088 | 1.000 |
| tables_scanned | scanned | 0.632 | 0.000 | 0.000 | 1.000 |

Field extraction:

| Document | Mode | Oxide F1 |
| --- | --- | ---: |
| invoice | digital | 1.000 |
| invoice_scanned | scanned | 0.400 |
| receipt | digital | 0.750 |

Speed and footprint:

| Metric | Result |
| --- | --- |
| Oxide startup | 7.5 ms |
| Python + PyMuPDF startup/import | 158.7 ms |
| Oxide release binary | 12.8 MB |
| Mean digital text extraction, Oxide CLI | 32.5 ms/doc |
| Mean digital text extraction, PyMuPDF in-process | 11.2 ms/doc |
| Mean digital text extraction, pdftotext CLI | 45.4 ms/doc |

Interpretation: Oxide is competitive on clean digital structure, perfect on
the measured reading-order cases, and strong for self-hosted deployment and
OCR-enabled text recovery. It trails PyMuPDF on some raw text fidelity cases
and trails ML-based systems on messy scanned structure.

## Renderer Benchmark

Baseline report: `docs/capstone_renderer_benchmark_prompt11.md`. GA4 follow-up:
`docs/ga4_renderer_fidelity.md`. Baseline raw data:
`docs/capstone_renderer_benchmark_prompt11.json`.

Command:

```powershell
python renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --oxide-bin target\release\oxide.exe `
  --dpi 144 `
  --timeout-sec 20 `
  --max-memory-mb 1024 `
  --max-pages-per-file 3 `
  --limit 265 `
  --output-dir renderer-benchmark\results\prompt11-0a-265
```

Results:

| Metric | Result |
| --- | ---: |
| Files run | 265 |
| Real-world files | 75 |
| Hostile files | 60 |
| Visual pages compared | 245 |
| Weighted score | 87.19 |
| Tier at this scale | Tier 0 |
| File pass | 82.64% |
| Visual pass | 78.37% |
| Hostile crash-free | 100.0% |
| Hostile timeout-safe | 100.0% |
| Hostile memory-bounded | 100.0% |
| Median Poppler/Oxide speed ratio | 2.7069 |
| Peak Oxide memory | 66.0 MB |
| Determinism sample | 24/24 stable |

GA4 follow-up result on the same 265-entry slice:

| Metric | Prompt 11 baseline | GA4 final |
| --- | ---: | ---: |
| Weighted score | 87.19 | 91.32 |
| Visual pass | 78.37% | 86.18% |
| File pass | 82.64% | 89.06% |
| Hostile safety | 100.0% crash/timeout/memory-safe | 100.0% crash/timeout/memory-safe |
| Determinism sample | 24/24 stable | 24/24 stable |

Weakest real-world categories:

| Category | Visual pass |
| --- | ---: |
| real-jpeg2000 | 0.00% |
| real-complex-vector | 13.33% |
| real-multi-column | 29.41% |
| real-scanned | 33.33% |
| real-rtl-text | 40.00% |
| real-forms | 57.14% |

The full 1,335-entry manifest run was not taken in this capstone because the
265-file slice already took roughly 16 minutes. Exact deferred full command:

```powershell
python renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --oxide-bin target\release\oxide.exe `
  --dpi 144 `
  --timeout-sec 20 `
  --max-memory-mb 1024 `
  --max-pages-per-file 3 `
  --output-dir renderer-benchmark\results\prompt11-0a-full-1335
```

Interpretation: the renderer is crash-safe on hostile input and fast in this
slice. GA4 raised the preview/OCR-grade visual pass materially, but Oxide still
is not Poppler/MuPDF/PDFium fidelity class. Rendering remains a known gap for
commercial visual-proof workflows.

## GA5 Whole-SDK Hardening

GA5 added fuzz targets for the post-capstone attack surfaces: signature
validation, full-document rewrite, linearization, PDF/A validation/conversion,
and editing/redaction/form flattening. See
`docs/ga5_release_hardening.md`.

The 265-file cross-pillar corpus run used a 60-second per-operation cap across
`info`, `parse`, `verify-sig`, first-page `render`, `optimize`, and
`linearize`. It ran 1,590 subprocessed operations with 0 crashes and 0
timeouts. qpdf was available: 213 inputs were qpdf-clean and 52 were already
damaged or warning-repaired by qpdf. Of 475 transformed-output qpdf checks,
458 passed and 17 inherited source damage from inputs that were not qpdf-clean;
no qpdf-clean source produced an invalid `optimize` or `linearize` output.

## SDK Operation Benchmarks

Full raw data: `docs/capstone_sdk_operation_benchmarks.json`.

Each operation used release binaries and three repetitions. The table reports
best elapsed time and max peak working set across the three runs.

| Operation | Best ms | Peak MB | Result |
| --- | ---: | ---: | --- |
| Parse to JSON, CLI | 95.9 | 19.00 | Passed |
| Extract text, CLI | 38.0 | 19.92 | Passed |
| Render PNG ZIP, CLI | 59.1 | 18.49 | Passed |
| Authoring example | 23.3 | 7.67 | Passed |
| PDF/A conversion example | 16.7 | 8.96 | Passed |
| RSA signing example | 12.7 | 5.07 | Passed |
| Optimize CLI | 9.5 | 6.05 | Passed |
| Linearize CLI | 12.5 | 6.82 | Passed; GA Prompt 1 made the hint tables qpdf-clean |
| Encrypt AES-256 CLI | 20.2 | 6.55 | Passed |

These are smoke operation benchmarks, not statistically rigorous throughput
claims.

## Capability Matrix

| Capability | Oxide status | Competitive positioning |
| --- | --- | --- |
| Structured extraction | Strong on digital-born structure, reading order, clean tables, KV fields, RAG chunks | More integrated than Poppler/qpdf. PyMuPDF is faster in-process for raw text. Docling likely leads on messy-scan ML, not measured locally. |
| OCR | Optional Tesseract-backed path | Useful self-hosted OCR without a cloud dependency. Trails ML layout systems for noisy scans and scanned tables. |
| Authoring | Builder, pages, text, vector graphics, images, whole TrueType embedding, tables, flow layout | Enough for programmatic document generation. Trails mature iText/PDFlib/Apryse layout breadth and font subsetting depth. |
| Editing | Watermarks, overlays, underlays, headers/footers, incremental updates, redaction, annotations, form fill/flatten | Practical editing surface exists. Redaction has extract-back tests. Advanced surgical content editing remains limited. |
| Structural ops | Merge/split/extract/rotate/repair/optimize/encrypt/decrypt plus qpdf-clean linearization for the supported subset | qpdf-class for the covered structural operations; object-stream packing inside linearized layout remains a size optimization follow-up. |
| PDF/A and PDF/UA | PDF/A-1b, 2b, 2a, 3b, and 3a validation/conversion examples pass qpdf and veraPDF 1.30.2; PDF/UA basic validation/best-effort tagging | Useful compliance foundation. PDF/UA remains best-effort and still needs manual semantic accessibility review before any full-conformance claim. |
| Signatures | RSA/SHA-256 signing and verification over ByteRange with incremental update; offline PAdES B-T/B-LT timestamp-token and DSS material embedding/reporting | Core signing plus deterministic LTV substrate exists. Live TSA/OCSP fetching, trust-store policy, timestamp imprint validation, PAdES-B-LTA, and ECDSA breadth remain follow-ups. |
| Surfaces | Rust library, CLI, C ABI, WASM, HTTP server | Strong embeddability and self-hosting story. |
| Packaging | Feature flags, dry-run packaging docs, license docs | Commercially friendly MIT OR Apache-2.0 posture. Some feature dependency slimming remains. |
| Rendering | 86.12% visual pass / 91.29 weighted on the final 265-entry slice, with 100% hostile safety | Materially improved from Prompt 11, but still trails Poppler/MuPDF/PDFium for visual-proof workflows. |

## Positioning

Oxide is best positioned for teams that need a self-hosted, memory-safe,
embeddable PDF SDK that can parse structured content, automate extraction,
author PDFs, apply common edits, perform structural operations, run compliance
checks, and sign documents without a Python runtime, native C++ rendering stack,
or per-page cloud fees.

It leads on:

- Pure-Rust core and single-binary deployment.
- Consistent model across Rust, CLI, C ABI, WASM, and server.
- Structured extraction plus KV fields and RAG chunks in the same stack.
- Self-hosted privacy and predictable deployment footprint.
- Hostile-input safety in the renderer benchmark slice.
- Permissive MIT OR Apache-2.0 licensing.

It trails on:

- Pixel-perfect rendering fidelity versus Poppler/MuPDF/PDFium.
- Messy scanned document understanding versus ML-heavy systems.
- Certified compliance and mature accessibility workflows.
- Signature live TSA/OCSP/trust-store policy, PAdES-B-LTA depth, and ECDSA breadth.
- Mature enterprise SDK breadth compared with iText, PDFlib, and Apryse.

## Release-Readiness Verdict

Detailed GA6 evidence: `docs/ga6_release_gate.md`.

Verdict: shippable as a v1.0 release candidate or controlled enterprise pilot
from a clean release branch. Do not tag v1.0 GA directly from this dirty
worktree.

Blocker status after GA1-GA5 plus GA6 verification:

| Area | Status |
| --- | --- |
| Linearization | Cleared: qpdf-clean check/show-linearization on the seven-fixture breadth. |
| PDF/A matrix | Cleared: qpdf + veraPDF PASS for 1b/2b/2a/3b/3a examples. |
| Renderer fidelity | Cleared as a meaningful improvement: 86.12% visual pass / 91.29 weighted, still preview/OCR-grade. |
| Whole-SDK hardening | Cleared for the measured slice: 1,590 operations, 0 crashes, 0 timeouts, 0 invalid transformed outputs from qpdf-clean inputs. |
| Signature LTV | Partially cleared: offline timestamp/DSS/CRL substrate works; live TSA/OCSP/trust-store/B-LTA and external LTV UI recognition remain known limitations. |
| Continuous hardening | Cleared as code-level posture: all 16 fuzz target corpora replayed, property tests passed, differential smoke passed, cargo-audit and cargo-deny passed with the documented RSA advisory exception. |

Known limitations for the release notes:

- Resolve or intentionally commit the pre-existing dirty worktree before a GA
  tag.
- Signature LTV is offline-first; live revocation/timestamp fetching and
  trust-policy validation remain follow-ups.
- RustCrypto `rsa 0.9.10` carries `RUSTSEC-2023-0071` with no fixed upgrade in
  the current pure-Rust line. It is explicitly ignored in `deny.toml` and
  should be reviewed during the external crypto audit.
- PDF/UA is assistive best-effort, not a certified accessibility claim.
- Rendering remains preview/OCR-grade, not visual-proof grade.
- Messy scanned tables and scanned KV extraction trail ML-heavy systems.
- A paid third-party security audit and a real pilot deployment remain the
  next trust-builders beyond code-level hardening.

Bottom line: Oxide is now a complete, self-hostable, pure-Rust enterprise PDF
SDK for parse/extract, authoring, editing, structural operations, PDF/A,
signatures, OCR, and multi-surface embedding. It is ready for pilot release and
release-candidate packaging; a strict GA tag should be cut only from a clean
branch with the signature-LTV scope accepted or completed.
