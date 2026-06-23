# Crypto Review Preparation

This document is the starting point for an external cryptography review.

## Algorithms And Crates

| Area | Algorithms | Crates |
| --- | --- | --- |
| PDF encryption read/write | RC4-40/128 legacy, AES-128-CBC, AES-256-CBC, PDF R2-R6 key derivation, MD5/SHA-256/384/512 | `aes`, `cbc`, `md-5`, `sha2`, internal RC4, `zeroize` |
| Signatures | CMS/PKCS#7 SignedData, RSA/SHA-256, ByteRange verification. ECDSA/EdDSA/RSA-PSS are reported as unsupported. | `cms`, `rsa`, `sha2`, `sha1`, `x509-cert`, `der`, `spki`, `const-oid` |
| Randomness | IVs, salts, file keys, PDF file IDs | `getrandom` OS CSPRNG |
| Constant-time comparisons | Server API keys; PDF password verifier hashes | `subtle` |

## Key Handling

- Passwords and private keys are caller-supplied and are not logged.
- Server API keys are read from environment variables and compared without
  early-exit timing leakage.
- PDF encryption uses random IVs/salts/file keys from `getrandom`.
- PDF encryption passwords in `EncryptParams`, derived file keys, per-object
  keys, reader `EncryptionContext` keys, writer `EncryptState` keys, R6
  intermediate buffers, and password-verifier scratch buffers use
  `zeroize::Zeroizing<Vec<u8>>` so heap buffers are wiped on drop.
- Serialized verifier fields (`/O`, `/U`, `/OE`, `/UE`, `/Perms`) remain normal
  `Vec<u8>` because they are written into the PDF encryption dictionary.
- RSA signing keys are parsed into RustCrypto `rsa::RsaPrivateKey`. That type
  does not implement `Zeroize` in the current dependency line, so Oxide cannot
  honestly claim private-key heap wiping until the dependency supports it or the
  signer storage is redesigned. Private-key operations are local API/CLI
  operations and are not exposed as a built-in network signing oracle.

## Self-Review Results

| Check | Result |
| --- | --- |
| CSPRNG for IVs/salts/file keys | Confirmed: `random_bytes` delegates to `getrandom`. |
| IV reuse | No fixed IV for stream/string encryption; AES-128/256 CBC uses fresh random IVs. V5 key wrapping uses the spec-required zero IV for `/UE` and `/OE`. |
| Constant-time secret comparison | PDF user/owner password verifier hashes and server API keys use constant-time comparison. |
| Key zeroization | PDF encryption key material and password scratch buffers now use zeroizing wrapper types on returned/internal paths; serialized public verifier bytes remain ordinary vectors by design. RSA private-key heap wiping remains dependency-limited. |
| Padding oracle exposure | Decryption is local library processing, not a network oracle by itself. Server errors are sanitized and do not expose padding detail. |
| Signature ByteRange integrity | Tests cover valid signatures, tampering inside signed range, and appending after signing. |
| Trust-chain/LTV | Offline embedded LTV material is tested. Live TSA/OCSP/CRL/system trust-store policy remains deployment-specific and should be reviewed externally. |
| RustSec advisory review | `rsa 0.9.10` is affected by `RUSTSEC-2023-0071` (Marvin timing side channel) and has no fixed RustCrypto upgrade in the current dependency line. It is an explicit cargo-audit/cargo-deny exception, not an unreviewed pass. Do not expose RSA private-key operations as a remotely timed signing oracle; external crypto audit should prioritize replacement or mitigation. |

## Known Crypto Limitations

- RC4 and AES-128 are supported for interoperability with existing PDFs, not
  recommended for new sensitive documents.
- Full live PAdES LTA/document timestamp refresh is not claimed.
- Public-key PDF encryption handlers are not implemented.
- ECDSA, EdDSA, and RSA-PSS signing/verification are not implemented yet; the
  current signer applies RSA/SHA-256 for compatibility.
- RustCrypto `rsa` currently carries `RUSTSEC-2023-0071` with no fixed upgrade;
  this is documented as an explicit advisory exception until the dependency can
  be replaced or patched.
- RustCrypto `RsaPrivateKey` does not implement `Zeroize` in this dependency
  line, so private-key object memory wiping remains a residual dependency item.

## Audit Questions

- Are all PDF R2-R6 key derivation edge cases interoperable and constant-time
  enough for the threat model?
- Are CMS signed attributes, digest computation, and ByteRange handling correct
  for adversarial PDFs?
- Is trust-chain policy explicit enough for integrators?
- Should the RSA signing implementation be replaced, gated, or redesigned to
  eliminate the Marvin advisory and private-key zeroization limitation?
