# Security Policy

## Supported Versions

Security fixes are provided for the current `main` branch and the latest tagged
release line. Older unreleased snapshots are not supported.

## Reporting a Vulnerability

Report suspected vulnerabilities privately to the repository owner or the
commercial support contact for your license. Do not open a public issue for
vulnerabilities that may expose exploit details.

Please include:

- Affected commit or release.
- A minimal reproducer PDF or request, if shareable.
- The observed impact: crash, hang, memory exhaustion, wrong signature result,
  data disclosure, auth bypass, or other behavior.
- Any logs or command output needed to reproduce.

We will acknowledge receipt, triage severity, and coordinate a fix and
disclosure timeline before public details are published.

## Security Guarantees

Oxide is designed for untrusted PDFs:

- Pure-Rust core with no C/C++ PDF engine dependency.
- JavaScript in PDFs is not executed.
- External URLs embedded in PDFs are not fetched during parsing/rendering.
- Parser, renderer, writer, editing, PDF/A, signature, and server paths are
  covered by unit, integration, property, fuzz, differential, and corpus tests.
- Server deployments default to API-key authentication, restrictive CORS,
  request timeouts, rate limits, file-size caps, page caps, DPI caps, render
  pixel caps, and output-size caps.

## Known Security Limitations

- A paid third-party security audit has not yet been completed.
- In-process TLS is not provided; terminate TLS at a reverse proxy or load
  balancer.
- PDF signature LTV supports embedded/offline material in tests, but live TSA,
  OCSP, CRL fetching and system trust-store integration remain deployment
  specific follow-ups.
- Legacy RC4 PDF encryption is supported only for compatibility with existing
  PDFs and should not be used for new sensitive documents.
- The C ABI necessarily contains `unsafe` pointer boundaries; these are
  documented in `docs/security/attack_surface.md`.
