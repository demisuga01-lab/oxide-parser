# GA Prompt 3 PDF/A Matrix Evidence

GA Prompt 3 broadened the compliance API beyond PDF/A-1b and PDF/A-2b.

## Implemented

- `PdfAProfile::PdfA2A`, `PdfA3B`, and `PdfA3A`.
- PDF/A validator checks for profile-correct XMP `pdfaid:part` and
  `pdfaid:conformance`.
- PDF/A Level A profiles require `/Lang`, `/MarkInfo`, a resolvable non-empty
  `/StructTreeRoot`, standard structure roles, and Figure `/Alt` text.
- PDF/A-3 allows embedded files only when represented by FileSpec dictionaries
  with valid `/AFRelationship`; conversion preserves attachments and repairs
  missing relationships as `/Unspecified`.
- Conversion adds deterministic Level A tagging scaffolding when the source is
  untagged.
- `validate_pdfua` and `improve_pdfua_best_effort` now use a non-empty
  structure tree rather than an empty placeholder.

## External Validation

Generated with:

```powershell
cargo run -p oxide-engine --example compliance -- target\tmp\pdfa_ga3
```

qpdf 12.3.2:

```powershell
qpdf --check target\tmp\pdfa_ga3\compliance-pdfa-1b.pdf
qpdf --check target\tmp\pdfa_ga3\compliance-pdfa-2b.pdf
qpdf --check target\tmp\pdfa_ga3\compliance-pdfa-2a.pdf
qpdf --check target\tmp\pdfa_ga3\compliance-pdfa-3b.pdf
qpdf --check target\tmp\pdfa_ga3\compliance-pdfa-3a.pdf
```

All five reported no syntax or stream encoding errors.

Bundled veraPDF 1.30.2:

| Command | Result |
| --- | --- |
| `verapdf -f 1b compliance-pdfa-1b.pdf` | PASS |
| `verapdf -f 2b compliance-pdfa-2b.pdf` | PASS |
| `verapdf -f 2a compliance-pdfa-2a.pdf` | PASS |
| `verapdf -f 3b compliance-pdfa-3b.pdf` | PASS |
| `verapdf -f 3a compliance-pdfa-3a.pdf` | PASS |

## PDF/UA Scope

PDF/A Level A outputs are veraPDF-clean for their PDF/A profiles. PDF/UA is
still an assistive best-effort feature, not a certification claim. Running
veraPDF UA-1 on `compliance-ua-best-effort.pdf` fails three checks in this
smoke: untagged/unartifacted content, missing `DisplayDocTitle`, and missing
PDF/UA identification XMP. Full PDF/UA certification remains dependent on
semantically correct content tagging, reading order, headings, tables, and
figure alternate text.
