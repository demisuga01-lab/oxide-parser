# Grammar-Aware Structured Fuzzing

Byte-level fuzzing is still valuable for parser rejection and decoder error
paths, but many mutated inputs never become valid PDFs. The structured fuzz
target adds a second lane: generate valid PDF object graphs with adversarial
content so the input gets past parsing and reaches deeper code.

## Target

`structured_pdf` is wired into:

- `fuzz/Cargo.toml`
- `fuzz/fuzz_targets/structured_pdf.rs`
- `scripts/ci_fuzz.py`
- `.github/workflows/fuzz.yml`

The target calls `oxide_engine::fuzz::fuzz_structured_pdf`, which is compiled
only with the `fuzzing` feature and is not part of the shipped public API.

## Input Classes

The generator has two valid-PDF producers:

- Authoring API producer: uses `PdfBuilder` to create bounded pages, text,
  lines, rectangles, circles, varied page sizes, and different writer modes.
- Raw operator producer: builds a valid catalog/pages/page/font/content graph
  with `PdfWriter`, then emits adversarial content streams containing q/Q
  nesting, transforms, clipping, paths, colors, dashes, and text positioning.

The raw producer also adds a valid text annotation to exercise annotation and
editing paths. The generated geometry is intentionally adversarial but bounded
so hostile inputs degrade gracefully instead of exhausting memory.

## Deep Code Driven

Every generated PDF is parsed and then sent through:

- content interpretation (`get_page_content`)
- text extraction
- raster rendering at a low bounded DPI
- document-model construction
- additive editing/redaction/form-flattening save path
- linearization
- PDF/A validation/conversion attempts
- signature validation

This complements the byte-level targets that mostly stress parser and decoder
front doors.

## Coverage Note

The local verification for this prompt proves reach by construction and smoke
execution of the `structured_pdf` target. A numeric LLVM coverage delta was not
collected in this run; the CI target exists so future coverage jobs can compare
`parse_pdf` against `structured_pdf` directly if required.

## Running

```powershell
cargo +nightly fuzz run structured_pdf fuzz/corpus/structured_pdf -- -runs=0
cargo +nightly fuzz run structured_pdf -- -max_total_time=300 -max_len=4096
```

Any crash should be minimized with `cargo +nightly fuzz tmin structured_pdf`,
fixed into a clean error/degradation path, and added as a committed regression
seed with `git add -f fuzz/corpus/structured_pdf/<name>`.
