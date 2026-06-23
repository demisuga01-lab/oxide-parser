# Oxide fuzz targets

Coverage-guided fuzzing of Oxide's untrusted-input parsing paths, using
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) + libFuzzer.

This crate is **deliberately excluded from the root workspace** (see the empty
`[workspace]` table in `Cargo.toml`). The root workspace is engine/server/cli
only, so `cargo build` / `cargo test` on stable never touch this crate and stay
green. Fuzzing requires a nightly toolchain.

## Targets

| Target              | Entry point                                   | What it exercises |
|---------------------|-----------------------------------------------|-------------------|
| `parse_pdf`         | `ContentEngine::open_bytes`                   | Header, xref/trailer, object streams, recursive object parser |
| `filters`           | `oxide_engine::filters::fuzz_decode_filter`   | Flate / LZW / ASCIIHex / ASCII85 / RunLength decoders (selector byte picks one) |
| `predictor`         | `oxide_engine::filters::fuzz_apply_predictor` | PNG/TIFF predictor with attacker-controlled Columns/Colors/BitsPerComponent |
| `content_tokenizer` | `ContentParser::parse`                        | Content-stream tokenizer + inline-image state machine + operand stack |
| `image_decoders`    | `oxide_engine::fuzz::fuzz_decode_image`       | CCITT / JBIG2 / JPX / DCT image decoders |
| `fonts`             | `oxide_engine::fuzz::fuzz_parse_font`         | TrueType / CFF / OpenType / bare-CFF font parsers through outline extraction |
| `cmap`              | `oxide_engine::fuzz::fuzz_parse_cmap`         | ToUnicode CMap parsing and lookup |
| `crypto`            | `oxide_engine::fuzz::fuzz_crypto`             | Encryption dictionary parsing, password verification, key derivation, decrypt primitives |
| `functions`         | `oxide_engine::fuzz::fuzz_functions`          | Function Types 0/2/3/4, sampled-function bit reader, Type 4 PostScript calculator |
| `writer`            | `oxide_engine::fuzz::fuzz_writer`             | Object serialization, string/name/stream escaping, tiny output PDF generation |
| `document_rewrite`  | `oxide_engine::fuzz::fuzz_document_rewrite`   | Full-document rewrite through classic xref, xref stream, and object stream writer modes |
| `linearize`         | `oxide_engine::fuzz::fuzz_linearize`          | Linearized output layout from successfully parsed untrusted PDFs |
| `pdfa`              | `oxide_engine::fuzz::fuzz_pdfa`               | PDF/A validation and conversion over parsed untrusted PDFs |
| `editing`           | `oxide_engine::fuzz::fuzz_editing`            | Additive editing, redaction, form flattening, and full rewrite |
| `signature_validation` | `oxide_engine::fuzz::fuzz_signature_validation` | Signature/DSS/LTV-like parsing reachable from untrusted signed PDFs |
| `structured_pdf`    | `oxide_engine::fuzz::fuzz_structured_pdf`     | Grammar-aware valid PDFs with adversarial content, then render/text/model/edit/PDF-A/linearize/signature paths |

The `fuzz_*` entry points are gated behind the engine's `fuzzing` feature
(enabled here via the `oxide-engine` dependency) so they are not part of the
normal public API.

## Prerequisites

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Running

```sh
# From the repo root:
cargo +nightly fuzz run parse_pdf
cargo +nightly fuzz run filters
cargo +nightly fuzz run predictor
cargo +nightly fuzz run content_tokenizer
cargo +nightly fuzz run image_decoders
cargo +nightly fuzz run fonts
cargo +nightly fuzz run cmap
cargo +nightly fuzz run crypto
cargo +nightly fuzz run functions
cargo +nightly fuzz run writer
cargo +nightly fuzz run document_rewrite
cargo +nightly fuzz run linearize
cargo +nightly fuzz run pdfa
cargo +nightly fuzz run editing
cargo +nightly fuzz run signature_validation
cargo +nightly fuzz run structured_pdf

# Time-box a run (e.g. 15 minutes) and cap input size:
cargo +nightly fuzz run parse_pdf -- -max_total_time=900 -max_len=65536
```

Seed corpora live in `corpus/<target>/`. cargo-fuzz seeds from there and writes
newly-discovered interesting inputs back into it.

### Reproducing / minimising a crash

```sh
cargo +nightly fuzz run parse_pdf path/to/crash-input      # replay one input
cargo +nightly fuzz tmin parse_pdf path/to/crash-input     # minimise it
```

### Replaying the regression corpus without mutation (smoke test)

Every fixed crash is preserved under `corpus/<target>/` and as a Rust
regression test in the engine. To re-run the whole saved corpus quickly
(coverage replay, no mutation):

```sh
cargo +nightly fuzz run parse_pdf corpus/parse_pdf -- -runs=0
```

The private CI workflow runs this replay step for every target that has a
committed seed directory, then starts a short time-boxed fuzz run. Scheduled
runs restore and save the per-target corpus through the GitHub Actions cache
and upload the corpus as an artifact for manual minimization/review.

### Persistent corpus policy

`fuzz/corpus/` is ignored by default because libFuzzer writes many generated
files there. Only small, reviewed seeds and minimized regression inputs should
be committed, using `git add -f fuzz/corpus/<target>/<seed>`. CI may discover
new coverage inputs, but it never auto-commits them. Download the scheduled
corpus artifact, run `cargo +nightly fuzz cmin <target>`, keep only small
high-value seeds, then force-add them in a normal review.

## AddressSanitizer (for `unsafe` code paths)

The fuzzed modules (`parser`, `filters`, `content`) are safe Rust, so panics /
hangs / OOM are the only failure modes and plain libFuzzer catches them. If a
future target reaches `unsafe` code (e.g. image bit-readers), run it under ASan
to catch genuine memory errors:

```sh
cargo +nightly fuzz run --sanitizer address <target>
```

## Findings

See [`docs/robustness.md`](../docs/robustness.md) for the catalogue of bugs
found and fixed, and the overall safety posture.
