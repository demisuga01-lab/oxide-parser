# Digital Signature Verification (`verify-sig`, `pdfsig`-equivalent)

`oxide verify-sig` reports and cryptographically verifies the digital
signatures in a PDF. Read/verify only — no signing, no PDF writing.

```
oxide verify-sig signed.pdf            # human-readable, pdfsig-style
oxide verify-sig signed.pdf --json     # machine-readable
oxide verify-sig signed.pdf --password p   # encrypted+signed PDF
```

## Pipeline

1. **Discovery** — walk the catalog `/AcroForm /Fields` for `/FT /Sig` fields
   (inherited `/FT` and nested `/Kids` handled) carrying a `/V` signature
   dictionary.
2. **Byte-range hashing** — read `/ByteRange [a b c d]` and hash the **exact
   original file bytes** `file[a..a+b] ++ file[c..c+d]` (the whole file except
   the `/Contents` gap). Verification uses `PdfReader::file_bytes()` — the raw
   bytes as opened, never a re-serialization.
3. **CMS parsing** — parse `/Contents` as a PKCS#7 / CMS `SignedData`
   (RFC 5652) via the `cms` crate; extract the first `SignerInfo`, its digest
   algorithm, signed attributes, signature algorithm, and the signer
   certificate (matched by issuer+serial against the embedded `certificates`).
4. **Verification** (RFC 5652 §5.4):
   - with **signed attributes** (the common case): the `messageDigest`
     signed-attribute must equal the digest of the signed bytes, and the
     signature is verified over the DER `SET OF` re-encoding of the signed
     attributes;
   - without: the signature is verified over the content digest directly.
   - The RSA PKCS#1 v1.5 signature is checked against the certificate's public
     key (`rsa` + `x509-cert` + `spki`) using the matching SHA-1/256/384/512
     scheme.
5. **Coverage** — if the signed ranges plus the `/Contents` gap reach EOF, the
   signature **covers the whole file**; trailing bytes mean an incremental
   update was appended → **modified after signing**.
6. **Certificate details** — subject, issuer, serial, and validity period are
   parsed from the X.509 cert and reported.

## Crates

All pure-Rust (RustCrypto), no C toolchain — Mega-Prompt 9's "public-key crypto"
turned out **not** to have been added, so these were introduced this round:
`cms` 0.2, `x509-cert` 0.2, `rsa` 0.9, `der`/`spki` 0.7, `const-oid` 0.9, plus
`sha1` (with `oid`) and `sha2` (with `oid`, for the PKCS#1 v1.5 DigestInfo
prefix). No `ring`/`openssl`/`-sys` crates were pulled in.

## What "valid" means — and what is NOT checked (honest scope)

**Valid** = *cryptographically* valid: the RSA signature verifies against the
signer certificate's public key, and the signed digest matches the
`/ByteRange` bytes. The output also reports coverage and the signer cert
details.

**Explicitly NOT performed this round** (stated in every report's `note`):

- **Trust-chain validation** to a trusted root CA — the cert is reported, not
  trusted. A self-signed signature is "cryptographically valid" but not
  "trusted". (Poppler's `pdfsig` itself depends on a configured trust store for
  the trust verdict.)
- **Revocation** (OCSP / CRL).
- **Certificate validity-period enforcement** — the dates are reported, not
  used as a pass/fail gate.
- **Timestamp tokens** (RFC 3161).
- **ECDSA / EdDSA / RSA-PSS** signatures — only RSA PKCS#1 v1.5 is verified;
  others are reported as `unsupported_algorithm`.

## Reported fields

`SignatureReport { index, field_name, signer_name, signing_time, reason,
location, contact_info, sub_filter, digest_algorithm, validity, coverage,
certificate, note }`. `validity` ∈ `valid | invalid | unsupported_algorithm |
error`; `coverage` ∈ `whole_file | modified_after_signing`.

## Validation

`crates/engine/tests/signatures.rs`, with fixtures from
`scripts/make_signature_fixtures.py` (pyHanko + a self-signed RSA/SHA-256 cert
**we control**, so the ground truth is known; pyHanko independently confirms
`intact=valid`):

- **Valid**: `sig_valid.pdf` → cryptographically valid, covers whole file,
  SHA-256, cert subject `CN=Oxide Test Signer`, serial `1234ABCD`.
- **Tampered**: flipping a byte inside a signed range → `invalid` (digest
  mismatch).
- **Modified after signing**: appending bytes after the signed ranges → still
  `valid` for what it covered, but `modified_after_signing`.
- **Multiple signatures**: `sig_two.pdf` → both reported; the earlier signature
  is `modified_after_signing` (the second signature's incremental update is
  appended after it), the later one covers the whole file — exactly the
  distinction `pdfsig` draws.
- **Unsigned**: a normal PDF reports no signatures (not an error).

**Cross-check note:** Poppler's `pdfsig` is **not bundled** in this environment,
so the cross-check is against the **known ground truth** (controlled cert/key +
independent pyHanko confirmation) rather than `pdfsig` directly. The objective
findings a `pdfsig` cross-check would compare — cryptographic validity,
coverage, and signer details — are exactly what these tests assert.

## Future enhancements

- Trust-chain validation against a configurable trust store; revocation
  (OCSP/CRL); validity-period enforcement as a verdict gate.
- ECDSA / EdDSA / RSA-PSS signature algorithms.
- PAdES/CAdES specifics and RFC 3161 timestamp-token verification.
- Signature **creation** (would need the Mega-Prompt 16 writer).
- Server endpoint (`POST /api/v1/verify-sig`) — deferred; the CLI is complete.
