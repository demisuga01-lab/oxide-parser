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
