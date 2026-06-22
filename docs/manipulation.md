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

## Structural-write operations (Bucket 2)

These document-mutating ops build on a **content-preserving** rewrite
(`writer::rewrite_document`) that copies the whole object graph — so unlike
merge/split they **keep** AcroForm fields, outlines, annotations, and the
structure tree. CLI + library API for each:

### `oxide encrypt` (`oxide_engine::encrypt`)

Encrypts with the Standard Security Handler, reusing the read-side key-
derivation primitives in the write direction. `--algo aes256` (V5/R6, the
secure **default**), `aes128` (V4/R4), or `rc4` (V2/R3); `--user-pw`,
`--owner-pw`, `--permissions` (signed `/P` bitmask).

- **AES-256 is the verified, interoperable default**: qpdf and Poppler decrypt
  Oxide's AES-256 output with both the user and owner password, and Oxide reads
  theirs. Round-trip content is exact.
- **RC4-128 / AES-128 are legacy/compat** and round-trip through Oxide's own
  reader, but Oxide's legacy V4 crypt-filter handling has a deviation other
  readers don't accept, so cross-reader interop is **not** guaranteed for the
  legacy algorithms — the CLI warns when one is selected. Prefer AES-256.
- Encrypted output is **not byte-deterministic** (random IV/salt/file key per
  the spec); the **decrypted content** is deterministic.

### `oxide rotate` (`oxide_engine::rotate_pages`)

Sets `/Rotate` (0/90/180/270, normalized) on `--pages` (default all),
`--angle N` absolute or `--relative` (offset from each page's current effective
rotation). Written on the leaf page objects; the whole document is otherwise
preserved. Re-parsing shows the new rotation and the read-side honors it, so
render/extract stay consistent.

### `oxide optimize` (`oxide_engine::optimize`)

Produces a smaller, cleaner PDF **without changing visible content**:
- garbage-collects objects unreachable from the catalog (the rewrite copies
  only live objects, dropping dead ones and stale `/Type /XRef` streams);
- recompresses **uncompressed** content streams with `FlateDecode` when smaller.
  Image-codec streams (`DCTDecode`/`JPX`/`CCITT`/`JBIG2`) and already-filtered
  streams are left untouched — no lossy re-encoding, image fidelity preserved.
- **packs eligible non-stream objects into compressed object streams** and emits
  a cross-reference stream (the modern writer, see below), the main structural
  size win — so optimize now **shrinks even files that were already xref/object-
  stream based** instead of growing them (the Bucket-2 regression, fixed).
- **Visually safe**: a rendered page of the output is byte-identical to the
  original under the same renderer (asserted in `tests/structural_ops.rs`; the
  0B harness is the belt-and-braces check). qpdf `--check` is clean and reports
  the object streams as compressed (type-2) entries.

### Writer modes (`oxide_engine::WriterMode`)

The writer can emit three cross-reference structures:

- **`ClassicXref`** (default) — a PDF 1.x `xref` table + `trailer`. Maximum
  reader compatibility. This is the default for `PdfWriter` / `rewrite_document`
  and therefore for `rotate` / `repair` / `encrypt`, whose output is unchanged.
- **`XrefStream`** — a PDF 1.5+ `/Type /XRef` cross-reference stream (smaller;
  prerequisite for object streams and linearization).
- **`XrefStreamWithObjStm`** — xref stream **plus** object-stream packing
  (`/Type /ObjStm`): eligible non-stream objects (not streams, not the
  `/Encrypt` dict) are packed into FlateDecode'd object streams and referenced
  as type-2 entries. The main file-size win; the default for `optimize`.

Encryption interaction: an object stream is encrypted as a **whole stream**
(compressed first, then encrypted — encryption is the outermost layer); the
objects packed inside it are **not** individually encrypted, and the `/Encrypt`
dictionary and the cross-reference stream itself are never encrypted. Validated
by `tests/modern_writer.rs::encrypted_objstm_roundtrips`. All three modes
round-trip through Oxide, qpdf `--check`, and Poppler; output is deterministic
per mode.

### `oxide repair` (`oxide_engine::repair`)

Persists the reader's recovery (missing `%%EOF`, stale classic-xref offsets,
misplaced `xref`, bad/oversized/missing stream `/Length`) as a clean, normalized
PDF with a fresh xref + trailer + corrected lengths. Best-effort salvage:
unrecoverable objects are dropped; inputs so damaged the xref/trailer can't be
located at all (e.g. `startxref` past EOF, truncated files) currently fail to
open and so can't be repaired — a from-scratch object scan + trailer synthesis
is recorded as future work. Repaired output is unencrypted.

### `oxide linearize`

Produces a qpdf-validated Fast Web View PDF for the supported structural
subset, including single-page files, realistic multi-page files with shared
resources, page annotations, and AcroForm/form fixtures. The implementation
builds on the modern cross-reference-stream writer and adds the linearization
layer:

- a fixed-width linearization parameter dictionary (`/Linearized`, `/L`, `/H`,
  `/O`, `/E`, `/N`, `/T`) immediately after the PDF header;
- a front cross-reference stream plus the main cross-reference stream chain;
- page-offset and shared-object hint stream data encoded in the Annex-F order
  qpdf validates;
- dependency analysis that groups the first page and its render dependencies at
  the front, emits later page groups in page order, and places page-shared
  dependencies in the shared-object section qpdf expects;
- iterative fixed-width/padded offset resolution so `/L`, `/H`, `/O`, `/E`, and
  `/T` stabilize exactly.

Catalog navigation/tagging metadata that would need additional open-document
hint coverage is intentionally removed before writing: outlines, open actions,
named destinations/name trees, page mode, page labels, and structure trees. Page
thumbnails still return `UnsupportedFeature` and do not write an output file.
Linearized output uses xref streams; packing regular objects into `/ObjStm`
inside the linearized layout remains a size-optimization follow-up. The command
will not emit a `/Linearized` file unless qpdf accepts it.

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

- **Linearization size refinement** (fast web view) — pack eligible regular
  objects into `/ObjStm` inside the already qpdf-valid linearized layout.
- **Object-stream / cross-reference-stream output** (PDF 1.5+) for more
  structural operations. `optimize` already uses this path so object-heavy /
  already-packed files shrink instead of growing.
- **Cross-reader-interoperable legacy (V4) encryption** — AES-256 is verified;
  the RC4/AES-128 V4 crypt-filter path needs a spec-conformance fix for qpdf/
  Poppler interop.
- **From-scratch xref/trailer rebuild** in `repair` for inputs where the
  cross-reference can't be located at all.
- Carrying AcroForm/outlines/annotations through **merge/split** (the Bucket-2
  ops already preserve them via the content-preserving rewrite).
- Server endpoints for the structural ops — deferred; the CLI + library are
  complete.

Done this round (Bucket 2): `encrypt` (AES-256 verified), `rotate`, `optimize`
(GC + recompression, visually safe), `repair` (persisted recovery), and the
qpdf-valid linearization implementation for the supported structural subset.
