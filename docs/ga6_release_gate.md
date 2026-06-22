# GA6 Final Release Gate

This is the final GA Prompt 6 verification record. It is measurement and
release judgment only; no engine feature work is claimed here.

## Provenance

Date: 2026-06-23.

Workspace: `E:\wellpdfsdk`.

Evidence was gathered after GA5 commit `3042550`. The worktree was dirty before
GA6 started from pre-existing, unrelated source changes. GA6 itself updates this
report, the capstone positioning doc, the extraction benchmark report/results,
the SDK operation benchmark JSON, and a small extraction-harness `OXIDE_BIN`
override so the benchmark can use an isolated OCR-enabled CLI.

Tools used:

| Tool | Evidence |
| --- | --- |
| qpdf | `qpdf --check` and `qpdf --show-linearization` |
| veraPDF | `target\tools\verapdf\app\verapdf.bat`, veraPDF 1.30.2 |
| Poppler | `target\tools\poppler\poppler-26.02.0\Library\bin` |
| cargo-fuzz | GA5 evidence, nightly targets |
| PyMuPDF | extraction benchmark competitor |
| Tesseract OCR | extraction benchmark OCR path through `oxide-ocr` |

Docling was not installed and no Docling numbers are fabricated.

## Blocker Verification

| Blocker | Status | Fresh evidence |
| --- | --- | --- |
| Linearization hint tables | Cleared | Freshly linearized `minimal`, `flate`, `multi_stream`, `basicapi`, `tracemonkey`, `form_160f`, and `synthetic100`; all `qpdf --check` and `qpdf --show-linearization` exits were 0. Summary: `target\ga6\linearization\summary.json`. |
| Signature LTV / PAdES | Partially cleared | `cargo test -p oxide-engine --test signatures -- --nocapture` passed 8/8, including timestamp+DSS offline material and revoked CRL detection. The implemented layer is offline PAdES-B-T/B-LT substrate. Live TSA HTTP, OCSP/CRL fetching, trust-store policy, Adobe/Poppler LTV recognition, and B-LTA document timestamps remain not claimed. |
| PDF/A matrix | Cleared | Regenerated `target\ga6\pdfa`; qpdf exited 0 and veraPDF 1.30.2 passed PDF/A-1b, 2b, 2a, 3b, and 3a. Summary: `target\ga6\pdfa\summary.json`. |
| Renderer fidelity | Cleared as improvement, not visual-proof | Fresh final 265-entry run: 86.12% visual pass, 91.29 weighted score, 100% hostile crash/timeout/memory safety, 24/24 determinism stable. Baseline was 78.37% / 87.19. Report: `renderer-benchmark\results\ga6-final-265\aggregate.md`. |
| Whole-SDK hardening | Cleared for the measured slice | Fresh 265-file cross-pillar sweep: 1,590 subprocessed operations, 0 crashes, 0 timeouts, 213 qpdf-clean inputs, 0 invalid outputs from qpdf-clean sources. Report: `target\ga6\corpus-hardening-60s\aggregate.json`. |

## Full Regression and Integration

Fresh checks:

| Check | Result |
| --- | --- |
| `cargo test --workspace` | Passed before GA6 doc updates; rerun required after final doc commit gate |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed before GA6 doc updates; rerun required after final doc commit gate |
| `cargo test -p oxide-engine --test signatures -- --nocapture` | Passed 8/8 |
| `python extraction-benchmark\scripts\test_harness.py` | Passed |
| `python scripts\capstone_surface_check.py` | Passed under escalation after sandbox blocked debug incremental writes; library, CLI, C ABI, and server all produced SHA-256 `43c9f91f575550430b4790e76fa65f8e6fbdbece01e517bc0eeb414c11a29e10`. |

Cross-pillar integration remains covered by `crates/engine/tests/capstone_integration.rs`
and the full workspace suite.

## Final Benchmarks

### Extraction

Fresh command used an OCR-enabled CLI built into `E:\tmp\wellpdfsdk-ga6-build`
via `OXIDE_BIN` because the default debug target was locked by Windows:

```powershell
$env:OXIDE_BIN='E:\tmp\wellpdfsdk-ga6-build\debug\oxide.exe'
python extraction-benchmark\scripts\extraction_benchmark.py
python extraction-benchmark\scripts\write_report.py
```

Result report: `docs\parser_benchmark.md`.

Key fresh extraction metrics:

| Metric | Result |
| --- | ---: |
| Oxide startup | 6.9 ms |
| Python + PyMuPDF startup/import | 139.1 ms |
| Oxide binary size | 12.7 MB |
| Mean digital text extraction, Oxide CLI | 31.3 ms/doc |
| Mean digital text extraction, PyMuPDF in-process | 10.8 ms/doc |
| Mean digital text extraction, pdftotext CLI | 150.2 ms/doc |
| Digital invoice field F1 | 1.000 |
| Scanned invoice field F1 | 0.400 |
| Clean table cell-F1 | 1.000 |
| OCR scanned table cell-F1 | 0.000 |

### Renderer

Fresh final run:

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
  --output-dir renderer-benchmark\results\ga6-final-265
```

| Metric | Result |
| --- | ---: |
| Files | 265 |
| Visual pages compared | 245 |
| Weighted score | 91.29 |
| Visual pass | 86.12% |
| Hostile crash-free | 100.0% |
| Hostile timeout-safe | 100.0% |
| Hostile memory-bounded | 100.0% |
| Median Poppler/Oxide speed ratio | 1.9107 |
| Determinism | 24/24 stable |

### SDK Operations

Fresh command:

```powershell
python scripts\capstone_bench.py
```

Raw data: `docs\capstone_sdk_operation_benchmarks.json`.

| Operation | Best ms | Peak MB | Result |
| --- | ---: | ---: | --- |
| parse JSON CLI | 95.9 | 19.00 | OK |
| extract text CLI | 38.0 | 19.92 | OK |
| render PNG CLI | 59.1 | 18.49 | OK |
| authoring example | 23.3 | 7.67 | OK |
| PDF/A conversion example | 16.7 | 8.96 | OK |
| RSA signing example | 12.7 | 5.07 | OK |
| optimize CLI | 9.5 | 6.05 | OK |
| linearize CLI | 12.5 | 6.82 | OK |
| AES-256 encrypt CLI | 20.2 | 6.55 | OK |

### Compliance and Structural

| Check | Result |
| --- | --- |
| PDF/A-1b, 2b, 2a, 3b, 3a | qpdf clean and veraPDF PASS |
| Linearized breadth | qpdf check/show-linearization clean on 7 fixtures |
| Signature regression | 8/8 tests pass, qpdf path covered by regression when available |
| Optimize/linearize corpus outputs | 0 invalid outputs from qpdf-clean sources |

## Known Limitations

- The worktree is still dirty from pre-existing unrelated source changes. Do
  not tag a release from this exact worktree until those changes are either
  committed intentionally or separated.
- Signature LTV is offline-first. It embeds timestamp tokens and DSS material
  and validates embedded CRL revocation, but it does not fetch live TSA/OCSP/CRL
  material, validate trust to a system/root store, prove Adobe/Poppler LTV UI
  recognition, or implement PAdES-B-LTA document timestamps.
- Renderer fidelity is materially improved and safe, but remains preview/OCR
  grade. PDFium/Poppler/MuPDF remain the visual-proof class.
- PDF/UA remains best-effort. PDF/A Level A outputs are veraPDF-clean for the
  tested profiles, but full accessibility certification still needs human
  semantic review, especially figure alt text and reading-order quality.
- OCR/scanned table reconstruction remains weak: scanned table text is
  recovered, but table cell reconstruction is still 0.000 on the measured OCR
  table case.
- `cargo-deny` is not installed in this environment. The repo has `deny.toml`
  and `docs/licenses.md` records the permissive license posture, but the checker
  was not rerun during GA6.
- Standalone workspace-wide ASan was not completed in GA5 on Windows. The
  cargo-fuzz targets built and ran cleanly under nightly instrumentation.
- The renderer and corpus safety runs are the 265-entry release-gate slice, not
  the full 1,335-entry manifest or a web-scale corpus.

## Verdict

Oxide is shippable as a v1.0 release candidate or controlled enterprise pilot
from a clean release branch. The feature pillars are present, qpdf/veraPDF
structural and compliance checks are green for the measured outputs, renderer
quality is materially above the Prompt 11 baseline, and the whole-SDK safety
slice is crash/timeout clean.

It is not responsible to tag v1.0 GA directly from this dirty worktree, and a
strict enterprise-signature buyer would still require the remaining live
TSA/OCSP/trust-store/B-LTA layer. If that LTV policy layer is scoped as a known
limitation, the SDK is ready for pilot release with the limitations above. If
full externally recognized LTV is a hard v1.0 requirement, that remains the
specific open blocker.
