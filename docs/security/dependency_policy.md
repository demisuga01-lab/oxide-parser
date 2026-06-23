# Dependency Security Policy

Dependency checks are enforced by `.github/workflows/security-audit.yml`.

## Tools

- `cargo audit --deny warnings` checks RustSec advisories.
- `cargo deny check advisories licenses bans sources` enforces advisory,
  license, duplicate-version, and source policy.

## License Policy

The SDK allows permissive licenses suitable for commercial distribution:

- MIT
- Apache-2.0
- BSD-2-Clause / BSD-3-Clause
- ISC
- IJG
- Zlib
- CC0-1.0
- Unicode-3.0 / Unicode-DFS-2016

Copyleft dependencies are not allowed in the shipped dependency graph without
explicit legal review.

## Advisory Exceptions

`RUSTSEC-2023-0071` for RustCrypto `rsa` is an explicit, documented exception
because no fixed upgrade is currently available. This exception must stay
visible in `deny.toml`, `.github/workflows/security-audit.yml`, and
`crypto_review.md`; any additional advisory must fail CI unless separately
reviewed and documented.

## Source Policy

Crates must come from crates.io unless explicitly reviewed. Unknown registries
and unknown git dependencies are denied by `deny.toml`.

## Review Process

When adding a dependency:

1. Confirm the crate is maintained and appropriate for untrusted input if it is
   on a parsing/crypto/rendering path.
2. Check license compatibility.
3. Run `cargo audit` and `cargo deny check`.
4. Document any exception in `deny.toml` with a reason.
