# Packaging and Distribution

This document records the installable surfaces and release-readiness checks for
Oxide.

## Feature Flags

`oxide-engine` now exposes capability-group features:

| Feature | Enables |
| --- | --- |
| `parse` | Reader, canonical parse model, text foundations |
| `extract` | Field/chunk extraction helpers over parse |
| `render` | Raster/SVG/PS rendering APIs |
| `create` | Authoring builder and flow layout |
| `edit` | Watermark/redaction/annotation/form editing APIs |
| `structural` | Merge/split/rotate/optimize/repair/encrypt/linearize |
| `sign` | PDF signature apply/verify APIs |
| `pdfa` | PDF/A and PDF/UA validation/conversion APIs |
| `ocr` | OCR-facing API types |
| `full` | All capability groups |

Current limitation: these features are capability labels and compile gates for
future API organization. The dependency graph is not yet aggressively slimmed by
feature because the modules are still tightly connected. Prompt 10 therefore
lands the documented matrix and independent compile checks; dependency slimming
is a packaging follow-up before a `1.0` release.

`oxide-cli` has `ocr` and `full`; OCR remains off by default.

## Publishable Crates

| Crate | Publish status |
| --- | --- |
| `oxide-engine` | Publishable library crate |
| `oxide-cli` | Publishable CLI crate |
| `oxide-server` | Publishable HTTP server crate |
| `oxide-capi` | `publish = false`; distributed as built C ABI artifacts |
| `oxide-wasm` | `publish = false`; distributed as an npm-style wasm-bindgen package |
| `oxide-ocr-tesseract` | `publish = false`; optional backend crate for this workspace |

Dry-run commands:

```sh
cargo publish -p oxide-engine --dry-run
cargo publish -p oxide-cli --dry-run
cargo publish -p oxide-server --dry-run
```

For a first crates.io release, publish `oxide-engine` first. The CLI and server
dry-runs require `oxide-engine = 0.1.0` to exist in the crates.io index because
Cargo verifies publishable dependencies against the registry rather than the
workspace path.

## License Audit

The project is dual licensed `MIT OR Apache-2.0`. Bundled fonts and third-party
license status are documented in `NOTICE` and `docs/licenses.md`.

`deny.toml` contains the allowlist for cargo-deny. Run:

```sh
cargo install cargo-deny
cargo deny check licenses sources
```

## Distribution Artifacts

CLI:

```sh
cargo build --release -p oxide-cli
target/release/oxide --version
```

C ABI:

```sh
cargo build --release -p oxide-capi
```

Header: `crates/oxide-capi/include/oxide.h`.

WASM:

```sh
rustup target add wasm32-unknown-unknown
cargo build -p oxide-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir crates/oxide-wasm/examples/browser/pkg target/wasm32-unknown-unknown/release/oxide_wasm.wasm
```

Docker:

```sh
docker build -t oxide-server:latest .
docker compose up
```

The compose file is local-development oriented and explicitly sets
`OXIDE_ALLOW_UNAUTHENTICATED=true` so it boots with empty `OXIDE_API_KEYS`.
Production deployments must set strong `OXIDE_API_KEYS` and remove that dev
override.

## Release Checklist

1. Update crate versions and `CHANGELOG.md`.
2. Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.
3. Run feature matrix checks listed below.
4. Run `cargo publish --dry-run` for publishable crates.
5. Run `cargo deny check licenses sources` if cargo-deny is installed.
6. Build release CLI, C ABI, WASM package, and Docker image.
7. Tag the release after dry-runs and artifacts are verified.

Feature matrix:

```sh
cargo check -p oxide-engine --no-default-features --features parse
cargo check -p oxide-engine --no-default-features --features render
cargo check -p oxide-engine --no-default-features --features create
cargo check -p oxide-engine --no-default-features --features edit
cargo check -p oxide-engine --no-default-features --features structural
cargo check -p oxide-engine --no-default-features --features sign
cargo check -p oxide-engine --no-default-features --features pdfa
cargo check -p oxide-engine --no-default-features --features extract,ocr
cargo check -p oxide-engine --no-default-features --features full
cargo check -p oxide-cli --no-default-features
cargo check -p oxide-cli --features full
cargo check -p oxide-wasm --target wasm32-unknown-unknown
```
