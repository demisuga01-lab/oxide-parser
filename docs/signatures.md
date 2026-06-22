# Digital Signatures (`sign` API + `verify-sig`, `pdfsig`-equivalent)

Oxide can apply a standard RSA/SHA-256 detached CMS signature and can report
and cryptographically verify digital signatures in a PDF. Signing is exposed as
the Rust API `ContentEngine::sign` / `sign_document`; verification is exposed as
`oxide verify-sig`.

```
oxide verify-sig signed.pdf            # human-readable, pdfsig-style
oxide verify-sig signed.pdf --json     # machine-readable
oxide verify-sig signed.pdf --password p   # encrypted+signed PDF
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

```
cargo run -p oxide-engine --example sign_document -- input.pdf key.pem cert.pem signed.pdf
```

## Verification Pipeline

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

All pure-Rust (RustCrypto), no C toolchain: `cms` 0.2 with its builder,
`x509-cert` 0.2, `rsa` 0.9 with `sha2`, `der`/`spki` 0.7, `const-oid` 0.9,
plus `sha1` (with `oid`) and `sha2` (with `oid`). No `ring`/`openssl`/`-sys`
crates are pulled in.

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
- **Timestamp tokens** (RFC 3161) and PAdES/LTV DSS revocation embedding.
- **ECDSA / EdDSA / RSA-PSS** verification — only RSA PKCS#1 v1.5 is verified;
  others are reported as `unsupported_algorithm`. The signer currently applies
  RSA/SHA-256.

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
- **Signing**: `minimal.pdf` signed with the committed test-only RSA key/cert
  → original bytes preserved as a prefix, the produced signature verifies,
  covers the whole file, reports signer metadata, and tampering is detected.

**Cross-check note:** Poppler was installed for this prompt, but the Windows
package does not include `pdfsig`. The generated signed sample was therefore
cross-checked with `qpdf --check`, Oxide verification, and Poppler render/text
tools (`pdftoppm`, `pdftotext`). Existing verification fixtures remain grounded
in independent pyHanko output.

## Future enhancements

- Trust-chain validation against a configurable trust store; revocation
  (OCSP/CRL); validity-period enforcement as a verdict gate.
- ECDSA / EdDSA / RSA-PSS verification and ECDSA signing.
- PAdES/CAdES specifics, RFC 3161 timestamp-token verification, and LTV/DSS.
- Server endpoint (`POST /api/v1/verify-sig`) — deferred; the CLI is complete.
