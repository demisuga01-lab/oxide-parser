# Document Info & Font Reporting (`info` / `fonts`)

Two pure **reporting** tools that surface data the engine already parses —
they never transform the PDF.

- `oxide info` — document metadata and structural facts (`pdfinfo`-equivalent).
- `oxide fonts` — every font used in the document (`pdffonts`-equivalent).

Both accept `--json` for machine-readable output and `--password` for encrypted
inputs (encrypted PDFs are decrypted on open).

## `oxide info`

```
oxide info report.pdf
oxide info report.pdf --json
oxide info secret.pdf --password hunter2
```

### Fields reported

From the `/Info` dictionary (all optional; absent fields omitted): **Title,
Author, Subject, Keywords, Creator, Producer, CreationDate, ModDate**.

Structural facts:

- **PDF version** — the catalog `/Version` overrides the header `%PDF-x.y` when
  present (PDF 32000-1 §7.5.5).
- **Page count**.
- **Page size(s)** — first page's MediaBox in points and millimetres, plus
  rotation when non-zero. When pages differ, reports `varies` and lists each
  distinct `(width, height, rotation)` with its page count.
- **Encryption** — encrypted yes/no; when encrypted, the algorithm
  (`RC4 40-bit` / `RC4 128-bit` / `AES-128` / `AES-256`), the `/V`/`/R`
  versions, key length, and the `/P` permission bits **decoded into named
  flags** (print, copy, modify, annotate, fill-forms, accessibility-extract,
  assemble, high-quality-print).
- **Tagged** — `/MarkInfo /Marked true` or a `/StructTreeRoot`.
- **Linearized** ("optimized for fast web view") — best-effort: probes the
  first few low-numbered objects for a `/Linearized` parameter dictionary.
- **File size** (input bytes), **File ID** (`/ID[0]` as uppercase hex), and
  **XMP metadata** presence (`/Metadata` in the catalog).

### Date and string decoding

- **Dates**: PDF dates `D:YYYYMMDDHHmmSSOHH'mm'` are parsed into a readable
  `YYYY-MM-DD HH:MM:SS ±HH'mm'`, preserving the document's own local time and
  offset; the raw string is kept too (in JSON). Partial dates and `Z` (Zulu)
  offsets are handled; unparseable input is passed through verbatim.
- **Strings**: `/Info` text strings are decoded per PDF 32000-1 §7.9.2.2 —
  UTF-16BE when prefixed with a `FE FF` BOM (UTF-16LE `FF FE` is tolerated for
  non-conformant producers), otherwise PDFDocEncoding.

### `pdfinfo` cross-check

Validated field-for-field against Poppler 26.02.0 `pdfinfo` on
`tracemonkey.pdf`, `basicapi.pdf`, and `form_160f.pdf`: **page count, PDF
version, encryption status, and page size agree**; metadata fields (Creator,
Producer, …) match. On the AES-256 fixtures `empty_protected.pdf` /
`secHandler.pdf`, the algorithm and decoded permission flags agree with
`pdfinfo` (`print/copy/change/addNotes` ↔ our `print/copy/modify/annotate`).

**One deliberate formatting difference:** dates. Poppler converts the stored
date to the *local machine* timezone (e.g. showing India Standard Time on this
host); Oxide preserves the document's own recorded local time and UTC offset
(`-07'00'`). Both denote the same instant; preserving the document's value is
the more faithful choice for a reporting tool.

**A real bug this cross-check surfaced and fixed:** the `/Encrypt` dictionary's
verifier strings (`/O`, `/U`, `/OE`, `/UE`, `/Perms`) are *not* encrypted, but
the reader's normal object fetch applies the per-object decryption pass — which
was AES-decrypting them and corrupting the 16-byte `/Perms` to empty, making
`EncryptionInfo::from_dict` fail and the algorithm/permissions go unreported.
Fixed by reading `/Encrypt` straight from the file bytes without decryption
(`PdfReader::encrypt_dictionary`).

## `oxide fonts`

```
oxide fonts report.pdf
oxide fonts report.pdf --json
```

### Columns

For each **distinct** font (deduped by object id):

- **name** — `/BaseFont` (e.g. `ABCDEF+Helvetica`).
- **type** — pdffonts-style label: `Type 1`, `TrueType`, `Type 0`, `Type 3`,
  `CID Type 0`, `CID TrueType`, …
- **encoding** — `WinAnsi` / `MacRoman` / `Standard` / `Identity-H` / `Custom`
  / `Builtin`, normalized to Poppler's short labels. When `/Encoding` is absent
  the implicit encoding is inferred the way pdffonts reports it (non-symbolic
  TrueType ⇒ `WinAnsi`, Type1 ⇒ `Standard`, symbolic ⇒ `Builtin`).
- **emb** — embedded? (a `FontFile`/`FontFile2`/`FontFile3` in the relevant
  `FontDescriptor`, including the descendant CIDFont's for Type0).
- **sub** — subset? (the 6-uppercase-letter `XXXXXX+` BaseFont prefix).
- **uni** — has a `/ToUnicode` CMap?
- **object id** — the font dictionary's indirect object number/generation.

### Resource-scope walk + dedupe

The walk visits **every scope a font can hide in**, deduping font references by
object id (a font shared across pages is reported once) and using a
visited-set on resource-carrying objects to stop cycles:

1. Each page's resolved `/Resources` (inheritance already applied).
2. **Page `/Annots` appearance streams** (`/AP /N` /`/D`/`/R`) — Form XObjects
   whose `/Resources` carry fonts used only in widget/annotation appearances.
3. **Form XObjects** (`/Subtype /Form`) referenced from any scope, recursively.
4. **Tiling patterns** (their `/Resources`).
5. **Type3 fonts'** own `/Resources` (used by `CharProcs`).

### `pdffonts` cross-check

Validated against Poppler `pdffonts` on `tracemonkey.pdf` (24 fonts),
`basicapi.pdf` (6 fonts), and `form_160f.pdf` (3 fonts): the **set of font
object ids is identical**, and per font the **name, type, encoding, emb, sub,
and uni** columns agree.

**A real missing-font bug this cross-check surfaced and fixed:** on
`form_160f.pdf`, Poppler listed an `Arial` font (object 403) that Oxide initially
missed — it lives in a form field's **annotation appearance stream** (a Form
XObject reached via the page `/Annots`, not the page `/Resources`). Adding the
`/Annots` appearance-stream walk fixed it. (A first over-correction also walked
the AcroForm `/DR` default resources and *over*-reported three unused template
fonts that pdffonts does not list; that was backed out — only fonts actually
drawn are reported.)

## Future enhancements

- Full XMP metadata parsing (currently presence-only).
- Broader linearization reporting beyond the current first-kilobyte
  `/Linearized` marker scan.
- Server endpoints (`POST /api/v1/info`, `/api/v1/fonts`) — deferred this round;
  the CLI is complete. These are lightweight (no large output) so they need no
  async-job machinery.
