# GA5 Release Hardening

This report records the GA Prompt 5 whole-SDK robustness pass. It extends the
earlier parser/filter/font/image fuzzing posture to the post-capstone pillars:
modern writer modes, linearization, PDF/A conversion, editing/redaction/forms,
and signature validation.

Measurements were taken on 2026-06-23 in `E:\wellpdfsdk` on Windows with
nightly cargo-fuzz available. The tree was intentionally dirty from prior
pillar work; the committed GA5 changes are limited to the fuzz entry points,
the corpus harness, and this report.

## New Fuzz Targets

Five new libFuzzer targets were added under `fuzz/fuzz_targets/`:

| Target | Entry point | Attack surface |
| --- | --- | --- |
| `signature_validation` | `ContentEngine::verify_signatures()` | Untrusted signature dictionaries, CMS/X.509/DSS/LTV-like material reachable through signed PDFs |
| `document_rewrite` | `rewrite_document_with_mode` for classic, xref-stream, and object-stream modes | Reader-to-writer traversal, xref streams, object streams, output reparsing |
| `linearize` | `structural::linearize::linearize` | Linearization layout and final output reparsing from malformed parsed inputs |
| `pdfa` | `validate_pdfa` and `convert_to_pdfa` for 1b/2b/2a/3b/3a | Compliance validation/conversion over hostile parsed documents |
| `editing` | `PdfEditor` draw/redact/flatten/save | Additive editing, redaction, form flattening, full rewrite |

The targets are exposed through `crates/engine/src/fuzz.rs` behind the existing
`fuzzing` feature. They are not part of the stable library surface.

Build proof:

```powershell
$targets = @('document_rewrite','linearize','pdfa','editing','signature_validation')
foreach ($t in $targets) { cargo +nightly fuzz build $t }
```

Result: all five targets built successfully.

## Fuzz Runs

Fresh short GA5 runs were taken on the new targets:

| Target | Duration cap | Executions | Result |
| --- | ---: | ---: | --- |
| `signature_validation` | 10 s | 831,518 | clean |
| `document_rewrite` | 20 s | 1,793,572 | clean |
| `linearize` | 20 s | 1,686,655 | clean |
| `pdfa` | 20 s | 1,599,022 | clean |
| `editing` | 20 s | 1,584,156 | clean |

The existing targets were also smoke-run briefly after the new targets were in
place:

```text
parse_pdf, filters, predictor, content_tokenizer, image_decoders, fonts,
cmap, crypto, functions, writer
```

All exited cleanly. No new crashers, hangs, or OOM cases were found in this GA5
pass.

The normal cargo-fuzz runs use the nightly libFuzzer/instrumented build path.
A separate broad `cargo +nightly test -p oxide-engine --features fuzzing
--no-run` attempt with `RUSTFLAGS=-Zsanitizer=address` did not complete within
the local Windows command timebox and left cargo coordinator processes after
rustc finished, so no standalone workspace-wide ASan result is claimed here.

## Real-World Corpus Harness

The new `scripts/ga5_corpus_hardening.py` harness runs key SDK operations in
isolated subprocesses with a per-operation timeout:

```powershell
python scripts\ga5_corpus_hardening.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --oxide-bin target\release\oxide.exe `
  --output-dir target\ga5-corpus-hardening-60s `
  --limit 265 `
  --timeout-sec 60 `
  --include-hostile
```

Operations exercised:

- `info --json`
- `parse -f json`
- `verify-sig --json`
- first-page `render`
- `optimize`
- `linearize`

The harness also runs `qpdf --check` on each source input when qpdf is
available. Output qpdf failures from qpdf-clean inputs are counted as release
findings; output failures from inputs qpdf already repairs or rejects are
reported separately as inherited source damage.

Fresh 265-file result:

| Metric | Result |
| --- | ---: |
| Files | 265 |
| qpdf-clean inputs | 213 |
| qpdf-not-clean inputs | 52 |
| Operations run | 1,590 |
| Crashes | 0 |
| Timeouts | 0 |
| Crash-free | 100.0% |
| Timeout-free | 100.0% |
| Output qpdf checks | 475 |
| Output qpdf checks passed | 458 |
| Output failures from qpdf-clean inputs | 0 |
| Inherited source-not-clean output failures | 17 |

Per-operation summary:

| Operation | OK | Clean error | Timeout | Crash |
| --- | ---: | ---: | ---: | ---: |
| `info` | 239 | 26 | 0 | 0 |
| `parse` | 239 | 26 | 0 | 0 |
| `verify_sig` | 239 | 26 | 0 | 0 |
| `render_p1` | 239 | 26 | 0 | 0 |
| `optimize` | 239 | 26 | 0 | 0 |
| `linearize` | 236 | 29 | 0 | 0 |

The earlier 20-second run exposed one timeout in `parse` on
`tests/corpus/pdfs/pdfjs/freeculture.pdf`, a 352-page document. A direct parse
completed in 41.27 s and wrote a 17.4 MB JSON file, so the 60-second GA5 cap is
the recorded whole-corpus safety threshold for this slice.

The 17 inherited output qpdf failures map to inputs qpdf itself reports as
damaged or warning-repaired, including hostile bad-filter fixtures,
`scan-bad.pdf`, `TAMReview.pdf`, and the deliberately minimal existing
fixtures with missing page resources. No qpdf-clean source produced a
qpdf-invalid `optimize` or `linearize` output.

## Resource Limits and Server Posture

This pass re-used the existing engine/server resource guardrails rather than
adding new runtime behavior:

- parser and renderer subprocess operations were bounded by the corpus harness
  timeout;
- oversized render pages remain guarded by the engine's render-pixel cap;
- malformed documents returned clean CLI errors rather than panics;
- signature validation over unsigned or malformed PDFs returned structured
  reports/errors;
- active-content fixtures are parsed as inert PDF structure; Oxide still has no
  JavaScript or Launch execution path.

Server endpoint resource enforcement remains covered by the existing server
integration suite and the security posture in `docs/security.md`; GA5 did not
add a long-running live-server hostile-upload campaign.

## Findings

No code crashers were found in the GA5 fuzz or corpus pass. The one harness
calibration issue was the 20-second parse cap on a large 352-page input; the
final corpus evidence uses 60 seconds and has no timeouts.

## Verification Commands

Completed:

```powershell
cargo +nightly fuzz build document_rewrite
cargo +nightly fuzz build linearize
cargo +nightly fuzz build pdfa
cargo +nightly fuzz build editing
cargo +nightly fuzz build signature_validation

cargo +nightly fuzz run signature_validation -- -max_total_time=10
cargo +nightly fuzz run document_rewrite -- -max_total_time=20
cargo +nightly fuzz run linearize -- -max_total_time=20
cargo +nightly fuzz run pdfa -- -max_total_time=20
cargo +nightly fuzz run editing -- -max_total_time=20

python scripts\ga5_corpus_hardening.py `
  --manifest renderer-benchmark\corpus\manifest.json `
  --oxide-bin target\release\oxide.exe `
  --output-dir target\ga5-corpus-hardening-60s `
  --limit 265 `
  --timeout-sec 60 `
  --include-hostile
```

Final `cargo test --workspace` and `cargo clippy --workspace --all-targets
-- -D warnings` are part of the GA5 commit gate.

## Limitations

- The fuzzing durations here are short release-gate runs, not multi-hour or
  multi-day campaigns.
- The real-world corpus slice is the same 265-entry benchmark slice used for
  GA4, with 60 hostile entries and 75 real-world files in the front of the
  manifest. It is broad enough for release evidence, not a complete web-scale
  PDF crawl.
- Standalone workspace-wide ASan was not completed on this Windows run. The
  cargo-fuzz targets built and ran cleanly under nightly instrumentation.
- Output validity is asserted only for qpdf-clean inputs. For already-damaged
  inputs, the robustness bar is clean error/no crash/no hang/no OOM; preserving
  a damaged stream can still make qpdf reject the transformed output.

## GA5 Verdict

GA5 extends the safety story from parser/renderer robustness to the whole SDK
surface introduced by the enterprise prompts. The new writer, linearization,
PDF/A, editing, and signature-validation fuzz targets all build and run cleanly,
and the 265-file cross-pillar corpus sweep is 100% crash-free and
timeout-free with zero invalid outputs from qpdf-clean sources.
