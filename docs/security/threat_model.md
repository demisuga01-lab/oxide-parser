# Threat Model

This document describes Oxide's security model for untrusted PDFs, network
service deployments, crypto/signature workflows, and supply-chain risk.

## Trust Boundaries

- PDF bytes are untrusted, whether they arrive through Rust APIs, CLI, C ABI,
  WASM, or server upload endpoints.
- Passwords, private keys, API keys, and trust anchors are trusted secrets
  supplied by the integrator or deployment environment.
- Reference tools used by tests (`qpdf`, Poppler, veraPDF, cargo-audit,
  cargo-deny) are development dependencies, not runtime trust anchors.
- OCR subprocesses and network calls for TSA/OCSP/CRL are deployment-controlled
  integrations, not automatic actions triggered by arbitrary PDF URLs.

## Attackers And Scenarios

| Attacker | Scenario | Current Mitigations | Residual Risk |
| --- | --- | --- | --- |
| Malicious PDF author | Crafted PDF crashes parser, renderer, filters, font parser, image decoder, writer, editing, PDF/A, linearization, or signature validation. | Pure-Rust core, no PDF JavaScript execution, resource caps, cargo-fuzz targets, grammar-aware fuzzing, property tests, real-world/hostile corpus sweeps. | Logic bugs may remain; third-party audit and pilot usage are still recommended. |
| Malicious PDF author | Decompression, image, page-tree, or geometry bomb exhausts memory/CPU. | Max decompression, render pixel caps, page caps, DPI caps, timeout/resource tests, hostile corpus. | New feature paths must preserve caps; continuous fuzzing and property tests guard regressions. |
| Malicious signed PDF author | Malformed CMS/X.509/DSS/LTV data attacks ASN.1 parsing or causes false signature validity. | Signature validation fuzz target, signed fixtures, ByteRange/tamper tests, LTV fixture tests, no OpenSSL/C binding. | Live trust-store and revocation policy are deployment-sensitive; external crypto audit recommended. |
| Network attacker | Server auth bypass, rate-limit bypass, CORS abuse, or upload DoS. | API keys fail closed, constant-time key comparison in server auth, restrictive CORS, rate limit, file/time/output caps, sanitized errors. | TLS termination and key storage/rotation are deployment responsibilities. |
| SSRF attacker | PDF content tries to trigger outbound URL fetches. | Parser/render/edit/PDF-A paths do not fetch URLs from PDFs or execute JavaScript. TSA/OCSP/CRL network calls are explicit caller-configured operations. | Future network integrations must preserve explicit opt-in and timeouts. |
| Supply-chain attacker | Vulnerable or compromised crate enters dependency graph. | `cargo audit` and `cargo deny` CI, permissive-license policy, lockfile review. | CI detects known advisories, not unknown malicious code. |

## Guarantees

- Bad input returns `Ok` or a classified error; it must not panic, hang, or
  allocate unboundedly.
- The server does not expose unauthenticated document operations unless a
  dev-only opt-in is explicitly set.
- PDF JavaScript is not executed.
- External URLs embedded in untrusted PDFs are not fetched by parse/render/edit
  paths.

## Non-Guarantees

- No claim of formal verification.
- No claim that auto-generated accessibility tags are semantically perfect.
- No claim that legacy RC4 encryption is modern security.
- No claim that a third-party security audit has already happened.
