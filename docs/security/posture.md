# Security and Robustness Posture

This is the consolidated hardening record for the post-GA hardening prompts.
It summarizes what is continuously checked, what was freshly verified in this
session, and what remains outside code-level hardening.

Fresh verification date: 2026-06-23.
Workspace: `E:\wellpdfsdk`.
Baseline note: the worktree had pre-existing unrelated source edits before this
consolidation pass. This document records the hardening evidence; it is not a
claim that those unrelated edits are release-ready.

## Hardening Stack

| Layer | What it covers | Current status |
| --- | --- | --- |
| Unit, integration, and doc tests | Public API behavior, CLI/server flows, parser/writer/render/edit/sign/compliance tests | `cargo test --workspace` passed in this session. |
| Clippy | Workspace lint gate for all targets | `cargo clippy --workspace --all-targets -- -D warnings` passed in this session. |
| Continuous private fuzzing | cargo-fuzz target matrix, persistent corpus cache, regression replay, scheduled deeper runs | `.github/workflows/fuzz.yml` is live. All 16 committed fuzz corpora replayed cleanly in this session. |
| Differential fuzzing | Wrong-output checks against qpdf and Poppler for page count, structural validity, text similarity, and writer round-trip | `.github/workflows/differential-fuzz.yml` is live. A 20-case smoke passed with 16 accepted notes and 0 high-signal disagreements. |
| Property-based testing | Round-trip identities, writer-mode equivalence, AES-256 preserve-content, no-panic arbitrary bytes, document-model invariants | `.github/workflows/property-tests.yml` is live. `cargo test -p oxide-engine --test property_invariants` passed 6 properties. |
| Grammar-aware fuzzing | Valid-but-adversarial PDFs that reach content interpretation, renderer, editing, linearization, PDF/A, and signature validation paths | `structured_pdf` is in the fuzz target matrix. Its committed corpus replay passed, reaching 10,945 coverage/features in the smoke output. |
| Real-world and hostile corpus sweep | Cross-pillar safety over parse, info, verify-sig, render, optimize, and linearize in isolated subprocesses | 265 files, 1,590 operations, 0 crashes, 0 timeouts, 100% crash-free and timeout-free. |
| Dependency security and licensing | RustSec advisories, license allowlist, source policy | `.github/workflows/security-audit.yml` is live. `cargo audit` and `cargo deny` passed with the documented `RUSTSEC-2023-0071` exception. |

## Fresh Verification Evidence

Commands run during this consolidation pass:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p oxide-engine --test property_invariants
python scripts\differential_fuzz.py --cases 20 --output target\hardening6-differential-smoke
python scripts\ci_fuzz.py --targets all --mode regression --no-build
python scripts\ga5_corpus_hardening.py --manifest renderer-benchmark\corpus\manifest.json --oxide-bin target\release\oxide.exe --output-dir target\hardening6-corpus-60s --limit 265 --timeout-sec 60 --include-hostile
cargo audit --deny warnings --ignore RUSTSEC-2023-0071
cargo deny check advisories licenses bans sources
```

Corpus sweep summary from `target\hardening6-corpus-60s\aggregate.json`:

| Metric | Result |
| --- | ---: |
| Files | 265 |
| Operations | 1,590 |
| qpdf-clean inputs | 213 |
| qpdf-not-clean inputs | 52 |
| Crashes | 0 |
| Timeouts | 0 |
| Crash-free | 100.0% |
| Timeout-free | 100.0% |
| Output qpdf checks | 475 |
| Output qpdf checks passed | 458 |
| Invalid transformed outputs from qpdf-clean inputs | 0 |
| Inherited output failures from already-damaged inputs | 17 |

The inherited output failures are limited to inputs that qpdf itself does not
accept cleanly. For qpdf-clean inputs, `optimize` and `linearize` did not
produce qpdf-invalid output in this sweep.

## Security Posture

Primary trust boundary: every PDF, font, image stream, content stream,
signature container, form, annotation, and server upload is untrusted.

Current guarantees and controls:

- Pure Rust core for memory-safety against buffer overflows, use-after-free,
  and type-confusion classes.
- Parser, filters, image decoders, font handling, content tokenization, writer,
  editing, PDF/A conversion, linearization, and signature validation have fuzz
  targets or regression tests.
- Server endpoints enforce fail-closed API key authentication, restrictive CORS
  by default, sanitized errors, rate limiting, job caps, timeouts, and resource
  limits.
- Rendering has DPI and pixel caps, hostile-input tests, and corpus safety
  coverage.
- PDF JavaScript, Launch actions, and PDF-triggered external fetches are not
  executed as active content.
- Signature/LTV network behavior is controlled by explicit signing/validation
  options rather than arbitrary PDF-driven fetches.
- Password verifier comparisons and server API-key checks use constant-time
  comparison.
- Encryption IVs, salts, file keys, and generated file IDs use OS CSPRNG-backed
  randomness.
- Temporary password verifier buffers are zeroed where they are not returned.
- The only current `unsafe` block inventory is the C ABI boundary documented in
  `docs/security/attack_surface.md`.

Supporting documents:

- Threat model: `docs/security/threat_model.md`
- Attack surface and unsafe inventory: `docs/security/attack_surface.md`
- Crypto review prep: `docs/security/crypto_review.md`
- Dependency policy: `docs/security/dependency_policy.md`
- Audit readiness checklist: `docs/security/audit_readiness.md`
- Disclosure policy: `SECURITY.md`

## Residual Risk and Known Limits

Code-level hardening cannot prove the absence of all bugs. The current residual
risk is explicit:

- A paid third-party security audit has not yet been performed. This remains
  the highest-value next step before broad commercial GA, especially because the
  SDK includes encryption and digital signatures.
- Real pilot usage has not yet replaced synthetic, fixture, fuzz, and corpus
  testing. A pilot integrator will exercise documents and workflows no local
  harness can predict.
- `RUSTSEC-2023-0071` remains an explicit exception for RustCrypto `rsa
  0.9.10`, which has no fixed upgrade in the current dependency line. Use of RSA
  signing should be reviewed in the external crypto audit and revisited when a
  fixed pure-Rust release is available.
- Returned key material still uses ordinary `Vec<u8>` values in some internal
  paths. Full zeroizing wrapper types are a future hardening option.
- Live TSA/OCSP/CRL fetching, system trust-store policy, and PAdES-B-LTA
  document timestamp refresh remain deployment-sensitive follow-ups.
- PDF/UA auto-tagging and figure alt text remain best-effort and require human
  accessibility review for certification claims.
- Renderer output is preview/OCR-grade, not visual-proof grade. PDFium/Poppler
  class renderers remain the reference for pixel-proof workflows.
- Standalone workspace-wide ASan was not completed on Windows. The cargo-fuzz
  targets built and replayed cleanly under nightly instrumentation.

## Verdict

The code-level hardening posture is now continuous and layered: normal tests,
property tests, persistent private fuzzing, differential fuzzing, grammar-aware
deep fuzzing, dependency auditing, and corpus sweeps all reinforce each other.

That is strong engineering evidence for robustness, but it does not replace a
human security audit or a real customer pilot. The recommended next trust
builders are:

1. Commission a third-party audit focused on parser/rendering safety,
   signatures/CMS/X.509/LTV, encryption, server exposure, C ABI unsafe boundary,
   and supply-chain policy.
2. Run a controlled pilot with real enterprise documents and workflows, feeding
   any findings back into the fuzz corpus, differential regressions, and normal
   tests.
