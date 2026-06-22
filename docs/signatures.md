# Digital Signatures (`sign` API + `verify-sig`, `pdfsig`-equivalent)

Oxide can apply a standard RSA/SHA-256 detached CMS signature, embed
caller-supplied PAdES timestamp/LTV material, and cryptographically verify
digital signatures in a PDF. Signing is exposed as the Rust API
`ContentEngine::sign` / `sign_document`; verification is exposed as
`oxide verify-sig`.

```text
oxide verify-sig signed.pdf
oxide verify-sig signed.pdf --json
oxide verify-sig signed.pdf --password p
```

## Signing API

```rust
use oxide_engine::{ContentEngine, PdfSigner, SignatureOptions};

# fn main() -> oxide_engine::Result<()> {
let input = std::fs::read("input.pdf")?;
let key_pem = std::fs::read_to_string("signer-key.pem")?;
let cert_pem = std::fs::read_to_string("signer-cert.pem")?;

let engine = ContentEngine::open_bytes(input)?;
let signer = PdfSigner::from_pem(&key_pem, &cert_pem, &[])?;
let signed = engine.sign(
    &signer,
    &SignatureOptions {
        field_name: "ApprovalSig1".to_string(),
        signer_name: Some("Example Signer".to_string()),
        reason: Some("approved".to_string()),
        location: Some("HQ".to_string()),
        rect: Some([36.0, 36.0, 280.0, 96.0]),
        ..SignatureOptions::default()
    },
)?;
std::fs::write("signed.pdf", signed)?;
# Ok(())
# }
```

Signing is incremental: the original file bytes remain an exact prefix, a
signature form field/widget is appended, `/ByteRange` is patched around a fixed
hex `/Contents` placeholder, and the placeholder is filled with DER CMS
`SignedData`.

The committed example `crates/engine/examples/sign_document.rs` signs a PDF from
PEM key/cert files:

```text
cargo run -p oxide-engine --example sign_document -- input.pdf key.pem cert.pem signed.pdf
```

## Verification Pipeline

1. Discovery: walk the catalog `/AcroForm /Fields` for `/FT /Sig` fields
   carrying a `/V` signature dictionary.
2. Byte-range hashing: read `/ByteRange [a b c d]` and hash the exact original
   file bytes `file[a..a+b] ++ file[c..c+d]`.
3. CMS parsing: parse `/Contents` as PKCS#7 / CMS `SignedData` and match the
   signer certificate by issuer and serial.
4. Verification: check the CMS `messageDigest` signed attribute and verify the
   RSA PKCS#1 v1.5 signature over the signed attributes or content digest.
5. Coverage: report whether the signature covers the whole file or whether an
   incremental update was appended after signing.
6. LTV/PAdES reporting: parse timestamp-token unsigned attributes and catalog
   `/DSS` material.

## PAdES / LTV

Oxide includes the offline PAdES/LTV substrate:

- `SignatureOptions::timestamp_token_der` embeds a caller-supplied DER RFC 3161
  `TimeStampToken` (`ContentInfo`) as the CMS `signatureTimeStampToken`
  unsigned attribute. This is the deterministic, no-network path used in CI.
- `ContentEngine::add_ltv_material` / `add_ltv_material` appends a catalog
  `/DSS` in an incremental update. It writes `/Certs`, `/OCSPs`, `/CRLs`, and
  per-signature `/VRI` entries keyed by the SHA-1 hash of the signature
  `/Contents`, preserving the signed prefix exactly.
- `verify_signatures` reports `SignatureReport::ltv`: PAdES level
  (`baseline_b`, `baseline_t`, `baseline_lt`), timestamp-token counts,
  DSS/VRI match status, embedded cert/OCSP/CRL counts, and revocation status.
  Embedded CRLs are parsed with the pure-Rust `x509-cert` CRL parser and checked
  for the signer certificate serial, so a revoked-cert fixture reports
  `revoked_by_embedded_crl`.

This is enough for offline B-T/B-LT material embedding and deterministic
validation reporting without a live network dependency.

## Crates

All pure-Rust (RustCrypto), no C toolchain: `cms` 0.2 with its builder,
`x509-cert` 0.2, `rsa` 0.9 with `sha2`, `der`/`spki` 0.7, `const-oid` 0.9,
plus `sha1` (with `oid`) and `sha2` (with `oid`). No `ring`/`openssl`/`-sys`
crates are pulled in.

## What "valid" means

**Valid** means cryptographically valid: the RSA signature verifies against the
signer certificate's public key, and the signed digest matches the `/ByteRange`
bytes. The output also reports coverage, signer cert details, and embedded
PAdES/DSS material.

Still caller policy / follow-up:

- Trust-chain validation to a configured root CA or system trust store.
- Live TSA HTTP, OCSP fetching, CRL distribution-point fetching, and network
  retry/timeout policy. The APIs are offline-first; callers supply DER
  tokens/responses.
- OCSP response signature/freshness policy and CRL issuer/signature validation.
  Oxide parses embedded CRLs to check whether the signer serial is listed, but
  it does not turn CRL trust into a final legal trust verdict.
- Timestamp imprint and TSA trust validation. Tokens are embedded and parsed as
  CMS `ContentInfo`; imprint/TSA trust remains policy-owned.
- PAdES-B-LTA document/archive timestamps. Oxide reports B-B/B-T/B-LT today.
- ECDSA / EdDSA / RSA-PSS verification and signing. The signer currently
  applies RSA/SHA-256 and the verifier supports RSA PKCS#1 v1.5.

## Reported fields

`SignatureReport { index, field_name, signer_name, signing_time, reason,
location, contact_info, sub_filter, digest_algorithm, validity, coverage,
certificate, ltv, note }`. `validity` is `valid`, `invalid`,
`unsupported_algorithm`, or `error`; `coverage` is `whole_file` or
`modified_after_signing`. `ltv` includes `pades_level`, timestamp-token counts,
DSS/VRI status, embedded material counts, and `revocation_status`.

## Validation

`crates/engine/tests/signatures.rs`, with fixtures from
`scripts/make_signature_fixtures.py`:

- Valid: `sig_valid.pdf` is cryptographically valid, covers the whole file,
  uses SHA-256, and reports signer certificate details.
- Tampered: flipping a byte inside a signed range reports `invalid`.
- Modified after signing: appending bytes after the signed ranges still leaves
  the signature valid for what it covered, but reports `modified_after_signing`.
- Multiple signatures: `sig_two.pdf` reports both signatures and distinguishes
  the earlier modified-after-signing revision from the later whole-file
  signature.
- Signing: `minimal.pdf` signed with the committed test-only RSA key/cert
  preserves original bytes as a prefix and detects tampering.
- LTV: `basicapi.pdf` signed with a deterministic offline timestamp token, then
  `add_ltv_material` appends a DSS with the signer cert and a synthetic DER CRL.
  The signed bytes remain an exact prefix, qpdf is clean when available, the
  report promotes to `baseline_lt`, the VRI matches, and the CRL-listed signer
  serial reports `revoked_by_embedded_crl`.

## Future enhancements

- Trust-chain validation against a configurable trust store; revocation
  freshness/trust policy; validity-period enforcement as a verdict gate.
- ECDSA / EdDSA / RSA-PSS verification and signing.
- Live RFC 3161 TSA and OCSP/CRL fetching; PAdES-B-LTA document timestamps.
- Server endpoint (`POST /api/v1/verify-sig`) remains deferred; the CLI is
  complete.
