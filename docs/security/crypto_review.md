# Crypto Review Preparation

This document is the starting point for an external cryptography review.

## Algorithms And Crates

| Area | Algorithms | Crates |
| --- | --- | --- |
| PDF encryption read/write | RC4-40/128 legacy, AES-128-CBC, AES-256-CBC, PDF R2-R6 key derivation, MD5/SHA-256/384/512 | `aes`, `cbc`, `md-5`, `sha2`, internal RC4 |
| Signatures | CMS/PKCS#7 SignedData, RSA/SHA-256, ECDSA where configured, ByteRange verification | `cms`, `rsa`, `sha2`, `sha1`, `x509-cert`, `der`, `spki`, `const-oid` |
| Randomness | IVs, salts, file keys, PDF file IDs | `getrandom` OS CSPRNG |
| Constant-time comparisons | Server API keys; PDF password verifier hashes after this hardening pass | `subtle` |

## Key Handling

- Passwords and private keys are caller-supplied and are not logged.
- Server API keys are read from environment variables and compared without
  early-exit timing leakage.
- PDF encryption uses random IVs/salts/file keys from `getrandom`.
- Temporary password-verifier buffers in `crypto.rs` are explicitly zeroed
  before return where they are not returned to the caller.
- Returned file keys and caller-owned private keys are still caller-managed;
  full zeroizing wrapper types are a future hardening option.

## Self-Review Results

| Check | Result |
| --- | --- |
| CSPRNG for IVs/salts/file keys | Confirmed: `random_bytes` delegates to `getrandom`. |
| IV reuse | No fixed IV for stream/string encryption; AES-128/256 CBC uses fresh random IVs. V5 key wrapping uses the spec-required zero IV for `/UE` and `/OE`. |
| Constant-time secret comparison | Fixed in this pass for PDF user/owner password verifier hashes. Server API keys were already constant-time. |
| Key zeroization | Improved in this pass for temporary verifier buffers. Returned keys remain caller-managed residual risk. |
| Padding oracle exposure | Decryption is local library processing, not a network oracle by itself. Server errors are sanitized and do not expose padding detail. |
| Signature ByteRange integrity | Tests cover valid signatures, tampering inside signed range, and appending after signing. |
| Trust-chain/LTV | Offline embedded LTV material is tested. Live TSA/OCSP/CRL/system trust-store policy remains deployment-specific and should be reviewed externally. |
| RustSec advisory review | `rsa 0.9.10` is affected by `RUSTSEC-2023-0071` (Marvin timing side channel) and has no fixed RustCrypto upgrade. It is an explicit cargo-audit/cargo-deny exception, not an unreviewed pass. Avoid exposing RSA private-key operations as a remotely timed signing oracle; external crypto audit should prioritize replacement or mitigation. |

## Known Crypto Limitations

- RC4 and AES-128 are supported for interoperability with existing PDFs, not
  recommended for new sensitive documents.
- Full live PAdES LTA/document timestamp refresh is not claimed.
- Public-key PDF encryption handlers are not implemented.
- Full key zeroization for all returned key material is not yet enforced by
  type system wrappers.
- RustCrypto `rsa` currently carries `RUSTSEC-2023-0071` with no fixed upgrade;
  this is documented as an explicit advisory exception until the dependency can
  be replaced or patched.

## Audit Questions

- Are all PDF R2-R6 key derivation edge cases interoperable and constant-time
  enough for the threat model?
- Are CMS signed attributes, digest computation, and ByteRange handling correct
  for adversarial PDFs?
- Is trust-chain policy explicit enough for integrators?
- Should returned key material move to zeroizing wrapper types before GA?
