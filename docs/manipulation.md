# PDF Manipulation — Writer, Merge, Split, Page Extraction

This round added Oxide's first PDF **output** capability: a pure-Rust
writer/serializer in `oxide-engine`, and three document-manipulation tools
built on it — **merge** (`pdfunite`-equivalent), **split**
(`pdfseparate`-equivalent), and **page extraction** (a subset of pages into one
new PDF).

Until now every Oxide tool produced *non-PDF* output (text, images, raster
pages, JSON). The writer closes that gap: Oxide can now take its in-memory
object model and emit a syntactically valid, openable PDF.

## The writer (`oxide_engine::writer`)

### What it emits

A classic-structure PDF file:

```
%PDF-1.7
%<binary marker>
1 0 obj … endobj          ← body, in ascending object-number order
2 0 obj … endobj
…
xref                      ← classic cross-reference table
0 N
0000000000 65535 f
0000000017 00000 n
…
trailer
<< /Size N /Root R /Info R /ID [<id><id>] >>
startxref
<byte offset of xref>
%%EOF
```

We use a **classic xref table** (not an xref stream) for maximum compatibility;
an xref stream is a possible future enhancement.

### Stream-data approach (decided)

Streams are copied **verbatim**: the original, still-filter-encoded `raw` bytes
are written unchanged together with their existing `/Filter` and `/DecodeParms`
entries, and `/Length` is re-set to the exact byte count emitted. **No
re-encoding** happens — this is faithful (no decode/re-encode round-trip that
could lose information), smaller, and simpler. (Re-compression of uncompressed
streams is a possible future optimization; correctness does not need it.)

The reader decrypts strings and stream bytes as objects are fetched, so the
bytes handed to the writer are already plaintext. **Output is therefore always
unencrypted** — manipulating an encrypted input decrypts it. Re-encryption of
output is a future enhancement.

### Object numbering & reference rewriting

When objects are copied out of one or more source documents their object numbers
collide. The writer assigns a fresh, contiguous numbering (1, 2, 3, …) for the
output and rewrites every `PdfObject::Reference` via a remap built during the
copy (`rewrite_references`). A reference whose target was deliberately *not*
copied (e.g. a `/Parent` pointer up the old tree, or a dropped document-level
feature) is rewritten to `null` rather than left dangling at an unrelated
object.

### Dependency-closure copy

For page-level manipulation, each selected page's **dependency closure** is
computed: its `/Contents` stream(s) and its resolved `/Resources` and everything
they transitively reference (fonts, XObjects, images, color spaces, shadings,
patterns, ExtGStates), followed recursively with cycle detection and bounded by
a generous object ceiling. **Shared resources are deduped within a source
document** by `(source object number)` — if two copied pages reference the same
font, it is copied once and both pages point at the single copy. Across
*different* source documents, objects are kept distinct (different documents are
never merged at the object level even if coincidentally identical).

### Inherited page attributes

`/MediaBox`, `/CropBox`, `/Resources`, and `/Rotate` can be inherited from
ancestor `/Pages` nodes. When a page is copied into a fresh single-level page
tree, the reader's existing inheritance resolution is reused and the resolved
attributes are written **onto the new page**, so it renders identically without
its old ancestor chain. `/CropBox` is only emitted when it differs from
`/MediaBox`; `/Rotate` only when non-zero.

## Tools

### `oxide merge` (pdfunite-equivalent)

```
oxide merge a.pdf b.pdf c.pdf -o merged.pdf
oxide merge a.pdf b.pdf --passwords secret, -o merged.pdf   # positional passwords
```

Concatenates all pages of each input, in input order. Each page keeps its own
size/rotation. Encrypted inputs are decrypted on read (output is unencrypted).

### `oxide split` (pdfseparate-equivalent)

```
oxide split in.pdf -o "page-%d.pdf"           # all pages
oxide split in.pdf -o "out-%03d.pdf" -f 2 -l 4  # pages 2..4, zero-padded
```

Writes each page to a pattern-expanded path. `%d` → page number; `%0Nd` →
zero-padded to width N; a pattern with no `%` gets `-<page>` inserted before the
extension. `-f`/`-l` bound the range (default: all pages).

### `oxide extract-pages`

```
oxide extract-pages in.pdf "1,3,5-9" -o out.pdf
oxide extract-pages in.pdf "5,1,3" -o out.pdf   # ORDER PRESERVED
```

Builds one PDF containing exactly the selected pages, **in the order given**
(unlike text/render range parsing, this does not sort or dedupe — `"5,1,3"`
yields pages 5, 1, 3). Out-of-range pages are dropped with a warning.

## What is carried over vs dropped (honest)

**Carried over** (per selected page):

- Page content stream(s) and their full filter chain (copied verbatim).
- The page's resources and their transitive closure: fonts, XObjects (image &
  form), color spaces, shadings, patterns, ExtGStates — deduped within a source
  document.
- Resolved inherited `/MediaBox`, `/CropBox`, `/Resources`, `/Rotate`.
- `/Info` and `/ID` from the **first** input document (merge) or the single
  input (split/extract), when present.

**Dropped this round** (matching, or acknowledging, `pdfunite`'s own
limitations):

- **AcroForm / interactive form fields** — not carried.
- **Outlines / bookmarks** — not carried.
- **Named destinations** and document-level `/Dests` — not carried (so
  cross-page link destinations may not resolve in output).
- **Document JavaScript / OpenAction** — not carried.
- **Tagged-PDF / structure tree (`/StructTreeRoot`, `/MarkInfo`)** — not
  carried.
- **Page `/Annots`** — currently not copied onto the new page (link/widget
  annotations are dropped). Carrying annotation closures is a natural follow-up.
- **Output is unencrypted** even if the input was encrypted.

These are flagged as future enhancements, not silent omissions.

## Validation

The writer is validated three ways (see `crates/engine/tests/writer.rs`):

1. **Unit round-trip per object type** — every `PdfObject` variant (including
   names needing `#XX` escaping, strings needing literal/hex escaping, binary
   strings, nested arrays/dicts, and streams with binary data) serializes and
   re-parses to an equal value; reference renumbering remaps and nulls unknown
   targets correctly.
2. **Document round-trip** — parse a fixture, write it back (copy all objects,
   identity-renumber, drop old xref streams), re-parse, and assert identical
   page count, page sizes, and per-page extracted text. Done for small fixtures
   **and** the realistic multi-page `tracemonkey.pdf`.
3. **External Poppler validation** — written output is opened with bundled
   Poppler (`pdfinfo` / `pdftotext`): page counts must match, and for
   `tracemonkey` Poppler's extracted text from the round-tripped file must
   equal its text from the original. Merge/split/extract outputs are likewise
   `pdfinfo`-validated, and extract ordering is confirmed by comparing
   Poppler's per-page text against the source pages.

## Future enhancements

- Re-encryption of manipulation output.
- Carrying AcroForm fields, outlines, named destinations, and page annotations.
- Cross-reference **streams** (PDF 1.5+) and object streams in output (smaller
  files).
- Stream re-compression of uncompressed inputs.
- Server endpoints (`POST /api/v1/{merge,split,extract-pages}`) — deferred this
  round; the CLI is complete.
