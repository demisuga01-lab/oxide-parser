# Property-Based Testing

Property tests assert invariants that must hold for broad classes of inputs, not
just hand-picked fixtures. The fast-bounded suite lives in
`crates/engine/tests/property_invariants.rs` and runs under normal `cargo test`.
A heavier scheduled/manual GitHub Actions job increases `PROPTEST_CASES`.

## Invariants Covered

- Authoring determinism: the same builder calls produce byte-identical PDFs.
- Authoring round-trip: generated authored PDFs parse back with the same page
  count and authored text.
- Writer-mode equivalence: classic xref, xref stream, and object stream output
  are representation choices; parsed text must match across modes.
- AES-256 encryption/open-with-password preserves page count and text content.
- Optimize preserves generated document page count and extractable text while
  keeping output bounded.
- Arbitrary byte input returns `Ok` or a clean `Err`; it must not panic.
- Document-model reading order is total for generated documents, and serialized
  model JSON round-trips through `serde_json::Value`.

## Generators

The current generator builds small, meaningful documents: one to three pages,
each with one to three WinAnsi text lines. This keeps properties fast and gives
the writer, parser, text extraction, encryption, optimize, and document-model
layers valid PDFs to exercise. A separate arbitrary-byte generator covers the
no-panic structural guarantee.

## Running

Fast local run:

```powershell
cargo test -p oxide-engine --test property_invariants
```

Heavier local run:

```powershell
$env:PROPTEST_CASES = "512"
cargo test -p oxide-engine --test property_invariants -- --nocapture
```

When a property fails, proptest writes a shrunk failing case. Diagnose the
invariant violation, fix it, and keep the shrunk case as a normal regression
test if it represents a real bug.
