# Differential Fuzzing

Continuous crash fuzzing proves Oxide does not crash or hang. Differential
fuzzing adds a correctness gate: it feeds the same generated or mutated PDFs to
Oxide and mature reference tools, then fails on high-signal disagreements.

## References

The CI harness uses developer/test tools only:

- `qpdf` for structural validity and page count.
- Poppler `pdftotext` for tolerant text-extraction comparison.
- Oxide's own CLI for info, extraction, and writer round-trip output.

These are not runtime dependencies of the SDK.

## What Is Compared

The default harness in [`scripts/differential_fuzz.py`](../scripts/differential_fuzz.py)
checks:

- Page count: `oxide info --json` must agree with `qpdf --show-npages` when
  qpdf accepts the file.
- Structural validity: qpdf-valid files must be accepted by Oxide. Oxide
  accepting a qpdf-invalid file is logged as a leniency note, not a failure.
- Text extraction: Oxide and Poppler text are compared by normalized token
  overlap, not byte-for-byte whitespace or reading-order exactness.
- Writer round-trip: `oxide optimize` output must pass `qpdf --check` and keep
  the original page count.

Render differential fuzzing is intentionally not part of the default gate; the
renderer has legitimate antialiasing and rasterization differences. Gross render
checks belong in the renderer benchmark harness.

## Input Generation

The harness combines:

- Exact seed PDFs from the checked-in fixture and corpus directories.
- Deterministic byte-level mutations of valid seeds.
- Grammar-aware tiny PDFs with valid xref tables and text content.

This keeps the signal higher than arbitrary random bytes, which are usually
rejected by every parser.

## CI

`.github/workflows/differential-fuzz.yml` runs a lightweight PR/push job and a
larger scheduled/manual job. Any high-signal disagreement uploads the report and
failing input under `target/differential-fuzz`.

Local smoke:

```powershell
python scripts\differential_fuzz.py --cases 20 --output target\differential-fuzz
```

Regression replay:

```powershell
python scripts\differential_fuzz.py --cases 0 --output target\differential-regression
```

## Triage

For every disagreement:

1. Reproduce locally from the saved input.
2. Decide whether Oxide or the reference is correct against the PDF spec.
3. If Oxide is wrong, fix the bug and add the minimized PDF to
   `differential/regressions/`.
4. If the difference is legitimate, document or suppress that false-positive
   class so the harness remains signal-rich.

Confirmed Oxide regressions are permanent CI seeds.
