# Stability and SemVer Policy

Oxide currently publishes workspace crates at version `0.1.0`. That is an
honest pre-1.0 signal: the stable integration path is documented, but some
low-level PDF internals can still move before a `1.0` commitment.

## What Is Stable

The following are the intended stable surfaces during the `0.x` line:

- `oxide_engine::prelude`
- `ContentEngine`
- canonical parse types: `Document`, `Page`, `Block`, `ParseOptions`
- authoring/editing/compliance/signature option structs documented in
  `docs/api_overview.md`
- `OxideError`, `ErrorKind`, `Result<T>`, and `OxideError::code()`
- CLI command names documented in README and `oxide --help`
- C ABI symbols in `crates/oxide-capi/include/oxide.h`
- HTTP `/api/v1/*` endpoint paths and documented JSON response shapes

## What Is Experimental

The crate root exposes a broad set of PDF internals for power users. Modules
such as `content`, `filters`, `fonts`, `images`, `object`, `parser`, `reader`,
`render`, and `writer` are public but lower-level. They may change before 1.0
when needed to keep the high-level SDK coherent.

## SemVer Rules

- Patch releases fix bugs and may add non-breaking APIs.
- Minor `0.x` releases may adjust experimental internals.
- Stable-surface removals or behavior changes require a changelog entry and a
  migration note.
- Deprecated stable APIs should remain for at least one minor release before
  removal, unless they are unsound or security-sensitive.

## MSRV

Minimum supported Rust version: current stable Rust for the active development
cycle. The repository currently uses edition 2021 and does not pin
`rust-version` yet. Prompt 10 packaging should set explicit per-crate
`rust-version` values once the release target is chosen.

## API Drift Checks

Recommended future CI guard:

```sh
cargo install cargo-public-api
cargo public-api -p oxide-engine --simplified > docs/public-api-oxide-engine.txt
```

Run the command before releases and review the diff. `cargo-semver-checks` can
be added once the crate commits to a `1.0` stable surface.
