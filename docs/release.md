# Release Process

Oxide releases are tag-driven. A version tag builds verified CLI artifacts,
runs the full quality gate, and creates a GitHub Release with SHA-256 checksum
files next to every binary archive.

## Versioning

Use semantic versions with a `v` prefix:

```sh
git tag -s v0.1.0
git push origin v0.1.0
```

Pre-release identifiers are allowed, for example `v0.1.0-rc.1`. Tags that
contain a hyphen are published as GitHub pre-releases.

Before tagging:

1. Update crate versions and `CHANGELOG.md`.
2. Keep the public API stability notes in `docs/stability.md` current.
3. Run the local release gate:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo audit --deny warnings --ignore RUSTSEC-2023-0071
cargo deny check advisories licenses bans sources
```

## GitHub Actions Release Pipeline

`.github/workflows/release.yml` runs on version tags matching `v*.*.*`.

The pipeline:

1. Runs the full release gate: formatting, workspace tests, clippy, cargo-audit,
   and cargo-deny.
2. Runs `cargo publish --dry-run --locked` for publishable crates. The
   dependent `oxide-cli` and `oxide-server` dry-runs run automatically once the
   matching `oxide-engine` version exists in crates.io.
3. Builds release CLI binaries:
   `x86_64-unknown-linux-musl`, `x86_64-apple-darwin`,
   `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`.
4. Packages archives and emits `.sha256` checksum files for each archive.
5. Creates a GitHub Release for the tag and attaches the archives and checksums.

Manual dispatch can be used as a dry run. By default, manual dispatch builds
and uploads workflow artifacts but does not create a GitHub Release. Set
`create_github_release=true` only when the named `release_tag` exists and
publishing that release is intentional.

## Crate Publishing

The release workflow intentionally does not publish to crates.io. It only runs
publish dry-runs.

For the first crates.io release, Cargo cannot dry-run `oxide-cli` or
`oxide-server` until the matching `oxide-engine` version exists in the crates.io
index. The workflow therefore dry-runs `oxide-engine` first and automatically
dry-runs `oxide-cli` and `oxide-server` when that engine version is already in
the registry. Before that first engine publication, the workflow records a
summary notice and leaves the dependent dry-runs to the manual publish sequence.

When a real crates.io release is approved, publish manually in dependency order:

```sh
cargo publish -p oxide-engine --locked
cargo publish -p oxide-cli --locked
cargo publish -p oxide-server --locked
```

The crates.io token must be stored outside the repository. If a future manual
publish job is added, it must read the token from a GitHub Actions secret, for
example `CARGO_REGISTRY_TOKEN`, and must require a protected environment or
manual approval.

## Optional Docker Image

The release workflow includes a manual, off-by-default Docker path for the
server image:

1. Run the workflow manually.
2. Set `build_docker=true`.
3. Set `push_docker=true` only when pushing `ghcr.io/<owner>/oxide-server` is
   intended.

The Docker path uses `GITHUB_TOKEN` for GHCR authentication. Additional registry
credentials must be configured as GitHub Actions secrets, never committed.

## Secret Safety

Do not commit tokens, registry passwords, private keys, or local release
credentials. The release workflow requires no crates.io token because it only
performs dry-runs. Any future real publish step must:

- use GitHub Actions secrets,
- avoid printing token values,
- run behind a manual approval gate, and
- keep third-party actions pinned to immutable revisions.

The current workflows use pinned action revisions for checkout, cache, and
artifact transfer.
