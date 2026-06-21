# Geometric Layout Analysis & Structured Extraction

`oxide extract-text --structured` recovers **document structure** (columns,
blocks, reading order) from a page's positioned text, rather than dumping text
top-to-bottom. This is a capability Poppler's CLIs largely lack: `pdftotext`
(even `-layout`) does spatial text placement but does **not** segment a page
into a logical structure or recover reading order across columns.

```
oxide extract-text in.pdf --structured                 # reading-order text
oxide extract-text in.pdf --structured --format json   # block tree + bboxes
oxide extract-text in.pdf -p 1-3 --structured          # a page range
```

This is **additive**. The default `oxide extract-text` path is byte-for-byte
unchanged, so the parity-harness numbers (which compare against plain
`pdftotext`) are unaffected.

## The algorithm (XY-cut + Docstrum-style spacing)

Implemented in `crates/engine/src/analysis/layout.rs`, operating purely on the
positioned text chunks (`TextChunk { x, y, width, font_size, … }` in PDF user
space). All thresholds are **document-relative** — scaled to the median font
size and the estimated line pitch — so the analysis generalises across DPIs and
font sizes (no absolute-pixel constants).

1. **Box construction.** Each non-vertical, non-empty chunk becomes an
   axis-aligned box `[x, x+width] × [y, y+font_size]`.

2. **Spacing estimate (Docstrum-style).** The characteristic line height is the
   median font size; the typical inter-line *pitch* is a low percentile (25th)
   of the gaps between adjacent text rows — deliberately the *common small* gap,
   so normal line spacing is never mistaken for a block boundary.

3. **Recursive XY-cut.** Project the boxes onto the X and Y axes; find the
   widest empty band (valley) in either projection that exceeds its threshold:
   - a **vertical** cut (gap along X, wider than ~1.2 line heights) separates
     **columns**;
   - a **horizontal** cut (gap along Y, wider than a normal line gap) separates
     stacked **blocks**.
   Cut at the *larger* qualifying gap, then recurse on each side. The recursion
   yields a region tree whose leaves are blocks.

4. **Reading-order traversal.** Columns are emitted left-to-right (or
   right-to-left for an RTL-dominant page — see below); stacked blocks
   top-to-bottom. This is what produces correct multi-column reading order: the
   column gutter is cut *first*, so each column is read fully before the next.

5. **Block & line formation.** Within each leaf region, items are grouped into
   lines by baseline proximity and joined with inferred word spacing; lines are
   split into paragraph blocks at vertical gaps larger than the typical pitch.

### RTL support

When the page is RTL-dominant (most chunks are right-to-left, detected via the
chunk's `is_rtl` flag from the BiDi-aware collector), column reading order is
reversed (right column first) and each RTL line's token order is reversed for
display. (Carried over from the BiDi work; see `text/reading_order.rs`.)

## Output

`--format text` emits the page text in reading order, blocks separated by a
blank line. `--format json` emits the structured tree:

```json
{ "pages": [ { "page": 1, "blocks": [
  { "bbox": {"x0":…,"y0":…,"x1":…,"y1":…}, "font_size": 10.0,
    "lines": [ { "text": "…", "bbox": {…}, "is_rtl": false }, … ] }
] } ] }
```

The bounding boxes are in PDF user space (y-up). This structure feeds the table
extractor and is useful for downstream consumers (chunking for RAG, layout-aware
diffing, HTML reflow).

## The win — reading-order correctness (measured)

Because the standard harness compares against plain `pdftotext` (which
interleaves columns), the structured improvement does **not** show in that
similarity metric — it is measured **directly** against hand-authored
ground-truth reading order on synthetic multi-column fixtures
(`crates/engine/tests/layout_analysis.rs`).

On a **three-column** page drawn in interleaved row-major order (the worst case
for a naive reader):

| Extraction | Reading-order correctness |
|---|---:|
| Default (`extract-text`) | **88%** (interleaves the 3rd column) |
| Structured (`--structured`) | **100%** (reads each column fully, in order) |

The default path's column heuristic only handles two columns, so a third column
interleaves exactly as plain `pdftotext` does; the XY-cut analyzer reads C1, then
C2, then C3 — the correct order. Single-column pages are unchanged.

## Honest limitations

- **Not a substitute for tagged-PDF structure.** This is geometric inference.
  Use `oxide extract-text --semantic` to prefer `/StructTreeRoot` when present
  and fall back to this analyzer when absent.
- **Tables inside text** are segmented as blocks, not parsed as tables, by the
  layout pass — table *parsing* is the separate `extract-tables` tool
  (`docs/tables.md`).
- **Pathological layouts** (overlapping columns, rotated text blocks, dense
  figures with captions) can still mis-segment; figure/caption detection is not
  implemented.
- **Vertical (CJK) text** is excluded from the XY-cut (the existing
  vertical reading-order path handles it); a mixed vertical+horizontal page
  analyses only its horizontal text here.
