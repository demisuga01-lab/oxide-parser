# Security Audit Readiness

Oxide is prepared for a third-party security audit with the following materials.

## Auditor Packet

- Threat model: [`threat_model.md`](threat_model.md)
- Attack surface and unsafe inventory: [`attack_surface.md`](attack_surface.md)
- Crypto review prep: [`crypto_review.md`](crypto_review.md)
- Consolidated security and robustness posture: [`posture.md`](posture.md)
- Server security posture: [`../security.md`](../security.md)
- Robustness and fuzzing posture: [`../robustness.md`](../robustness.md)
- Continuous fuzzing: [`../continuous_fuzzing.md`](../continuous_fuzzing.md)
- Differential fuzzing: [`../differential_fuzzing.md`](../differential_fuzzing.md)
- Property testing: [`../property_testing.md`](../property_testing.md)
- Grammar-aware fuzzing: [`../grammar_aware_fuzzing.md`](../grammar_aware_fuzzing.md)
- Top-level disclosure policy: [`../../SECURITY.md`](../../SECURITY.md)

## Recommended Audit Scope

1. Parser/rendering safety on hostile PDFs.
2. Signature validation and CMS/X.509/DSS handling.
3. PDF encryption/decryption key derivation and writer encryption.
4. Server auth, rate limiting, resource caps, and sanitized errors.
5. C ABI unsafe pointer boundary.
6. Supply-chain policy and CI enforcement.

## Pre-Audit Self-Review Findings

- Found and fixed ordinary equality comparisons in PDF password verifier paths;
  they now use constant-time comparison.
- Zeroed temporary password-verifier buffers where they are not returned.
- Moved PDF encryption passwords, derived file keys, per-object keys, reader
  contexts, writer states, and R6 intermediate buffers to zeroizing wrapper
  types.
- Confirmed OS CSPRNG use for encryption IVs, salts, file keys, and generated
  file IDs.
- Confirmed `unsafe` is isolated to the C ABI boundary and added
  `#![forbid(unsafe_code)]` to the engine crate.
- Confirmed CI wiring for cargo-audit and cargo-deny.
- Added scheduled/manual Linux sanitizer CI coverage for C-ABI tests, crypto
  regressions, Rust UB runtime checks, TSan checks, and ASan cargo-fuzz corpus
  replay.

## Remaining Human Review Items

- A paid external audit has not yet been completed.
- RustCrypto `rsa` remains on the documented Marvin advisory exception;
  auditors should review RSA signing exposure, replacement options, and the
  current `RsaPrivateKey` zeroization limitation.
- Live TSA/OCSP/CRL and system trust-store policies should be reviewed with the
  intended deployment model.
