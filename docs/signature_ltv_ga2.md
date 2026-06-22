# GA2 Signature LTV / PAdES Notes

GA Prompt 2 extends the core CMS signing work with the deterministic offline
PAdES/LTV substrate.

## What Landed

- CMS timestamp-token embedding: callers can pass
  `SignatureOptions::timestamp_token_der`, a DER RFC 3161 `TimeStampToken`
  (`ContentInfo`), and Oxide embeds it as the CMS
  `signatureTimeStampToken` unsigned attribute.
- DSS append: `ContentEngine::add_ltv_material` / `add_ltv_material` writes a
  catalog `/DSS` dictionary through an incremental update. It emits `/Certs`,
  `/OCSPs`, `/CRLs`, and per-signature `/VRI` entries keyed by the SHA-1 hash
  of the signature `/Contents`.
- Offline validation reporting: `verify_signatures` now reports a structured
  `LtvReport` with PAdES level, timestamp counts, DSS/VRI match, embedded
  validation material counts, and revocation status.
- Revocation fixture coverage: embedded CRLs are parsed with `x509-cert` and
  checked for the signer certificate serial. The regression test covers a
  revoked signer serial.

## Validation

- `cargo test -p oxide-engine --test signatures -- --nocapture`
  - 8 tests passed.
  - The LTV regression signs `basicapi.pdf`, embeds a timestamp token, appends
    a DSS with CRL material, verifies `baseline_lt`, checks the matching VRI,
    detects `revoked_by_embedded_crl`, and runs `qpdf --check` when qpdf is
    available.

## Honest Remainder

The current implementation is offline-first and policy-neutral. It does not
perform live TSA HTTP, OCSP requests, CRL distribution-point fetching,
trust-chain validation to a root store, OCSP freshness/signature policy,
CRL issuer/signature policy, timestamp imprint validation, TSA trust
validation, or PAdES-B-LTA document timestamps. Those remain the network/trust
policy layer above the now-available pure-Rust CMS/DSS substrate.
