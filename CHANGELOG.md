# Changelog

All notable changes to Oxide are recorded here. The project follows the
stability policy in `docs/stability.md`.

## Unreleased

- Added the main CI gate, scheduled/manual sanitizer gate, and tag-driven
  release pipeline with checksummed CLI artifacts and crate publish dry-runs.
- Added the documented public API overview and stability policy.
- Added `ErrorKind` plus `OxideError::kind()`, `OxideError::code()`, and
  `OxideError::is_input_error()` for programmatic error handling.
- Added RSA/SHA-256 PDF signing through incremental update and the
  `sign_document` example.
