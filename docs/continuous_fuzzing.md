# Continuous Private CI Fuzzing

Oxide uses private GitHub Actions fuzzing rather than OSS-Fuzz because the
repository is private and commercially distributed.

The workflow is `.github/workflows/fuzz.yml`.

The sanitizer gate is separate: `.github/workflows/sanitizers.yml` runs on a
schedule or manual dispatch so ASan/TSan/UB-check coverage can be heavier than
the pull-request regression gate.

## What Runs

Every pull request and push to `main` runs each cargo-fuzz target in its own
deterministic matrix job:

1. install nightly Rust;
2. restore the per-target persistent corpus cache;
3. build the target;
4. replay the committed regression/seed corpus with `-runs=0`;
5. upload crash artifacts on failure.

The scheduled weekly job and manual dispatch use the same target matrix, then
run the timed exploratory phase. They save the mutated corpus back to the
GitHub Actions cache and upload a corpus artifact for manual minimization.

The runner entry point is:

```powershell
python scripts\ci_fuzz.py --targets content_tokenizer --mode regression
python scripts\ci_fuzz.py --targets content_tokenizer --mode deep --seconds 900 --no-build
```

## Targets

The continuous job covers:

- `parse_pdf`
- `filters`
- `predictor`
- `content_tokenizer`
- `image_decoders`
- `fonts`
- `cmap`
- `crypto`
- `functions`
- `writer`
- `document_rewrite`
- `linearize`
- `pdfa`
- `editing`
- `signature_validation`
- `structured_pdf`

These include the GA5 attack surfaces: signature validation, modern writer
round-trip, PDF/A conversion, editing/redaction/forms, and linearization.

## Persistent Corpus

The corpus has two layers:

- committed reviewed seeds under `fuzz/corpus/<target>/`;
- private GitHub Actions cache entries produced by scheduled and manual deep
  fuzzing runs.

`fuzz/corpus/` is ignored because libFuzzer writes many generated files there.
Only small reviewed seeds should be committed:

```powershell
git add -f fuzz\corpus\<target>\<seed>
```

Scheduled runs upload `fuzz-corpus-<target>-<run_id>` artifacts. To promote
new seeds:

1. download the corpus artifact;
2. copy it to `fuzz/corpus/<target>/`;
3. run `cargo +nightly fuzz cmin <target>`;
4. keep only small high-value minimized seeds;
5. force-add them in a normal code review.

CI never auto-commits new corpus files.

## Regression Gate

Every fixed crash or hang should become both:

- a normal Rust regression test where practical; and
- a minimized fuzz seed under `fuzz/corpus/<target>/`.

The PR job replays the committed corpus with:

```powershell
cargo +nightly fuzz run <target> corpus/<target> -- -runs=0
```

If a previously fixed input crashes, hangs, or OOMs again, cargo-fuzz returns a
non-zero exit code and the CI job fails.

## Triage Workflow

When CI finds a crash:

1. Download the `fuzz-crash-<target>-<run_id>` artifact.
2. Reproduce locally:

   ```powershell
   cargo +nightly fuzz run <target> path\to\crash
   ```

3. Minimize:

   ```powershell
   cargo +nightly fuzz tmin <target> path\to\crash
   ```

4. Diagnose the root cause.
5. Fix the bug into a clean `Result::Err` or bounded output path. Do not hide
   it with `catch_unwind`.
6. Add the minimized input as a seed with `git add -f`.
7. Add or update a focused Rust regression test.
8. Rerun:

   ```powershell
   python scripts\ci_fuzz.py --targets <target> --mode regression
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   ```

## Budget

Pull request and push fuzzing is intentionally deterministic: build the target
and replay committed corpora only. The scheduled job defaults to 900 seconds per
target. The workflow exposes a `workflow_dispatch` `fuzz_seconds` input for ad
hoc longer runs without changing YAML.

The goal is not to prove every input safe in a single PR. The goal is to make
the GA5 clean sweep permanent: known regressions fail quickly, and scheduled
coverage keeps compounding in the private corpus over time.

## Sanitizer Gate

The Linux sanitizer workflow covers:

- C-ABI tests under AddressSanitizer;
- C-ABI tests under ThreadSanitizer because the workspace uses parallel code;
- C-ABI and crypto tests with nightly Rust UB runtime checks;
- all committed fuzz corpora replayed through cargo-fuzz with ASan.

Rust's sanitizer flag supports ASan and TSan directly. The undefined-behavior
lane uses nightly `-Zub-checks=yes` rather than claiming a separate Rust
`-Zsanitizer=undefined` mode.
