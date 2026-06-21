# Tagged-PDF Semantic Extraction

`oxide extract-text --semantic` extracts authored semantic structure from tagged
PDFs. It reads `/StructTreeRoot`, walks `/StructElem` nodes in tree order, and
links structure elements to page content through marked-content IDs:

```pdf
/P <</MCID 3>> BDC
  ... text drawing operators ...
EMC
```

When a structure element's `/K` references MCID `3` on a page, Oxide collects
the text chunks emitted inside that marked-content range and attaches that text
to the semantic element.

```sh
oxide extract-text in.pdf --semantic
oxide extract-text in.pdf --semantic --format json
oxide extract-text in.pdf -p 1-3 --semantic --format json
```

## Output

JSON output is a document with:

- `tagged`: whether `/StructTreeRoot` was used.
- `source`: `tagged_pdf` or `geometric_fallback`.
- `elements`: ordered semantic elements with `type`, `text`, optional
  `alt_text`, `actual_text`, `lang`, `page`, `mcids`, and `children`.
- `tables`: semantic tables recovered from tagged `Table` / `TR` / `TH` / `TD`
  elements, serialized with the same table model used by `extract-tables`.

Text output is a readable outline:

```text
H1: Semantic Title
P: Intro paragraph
Table
  TR
    TH: Name
    TH: Age
Figure: Revenue chart
```

## Precedence

Semantic mode is tags-first:

1. If `/StructTreeRoot` is present, Oxide trusts the authored tag order. This is
   the best available reading order for tagged multi-column documents.
2. If no structure tree is present, Oxide falls back to the geometric XY-cut
   layout analyzer from `--structured`.

The default `extract-text` path is unchanged.

## Validation

`crates/engine/tests/semantic_extraction.rs` builds a minimal tagged PDF with:

- an `H1`;
- paragraphs whose physical draw order differs from authored tag order;
- a list;
- a semantic table with `TH`/`TD` cells;
- a figure with `/Alt`.

The test asserts element types, MCID text, authored reading order, table cells,
CSV serialization, and alt text. A second untagged fixture asserts semantic mode
falls back to geometric layout instead of failing.

## Limitations

- Role maps (`/RoleMap`) and class maps are not expanded yet.
- Object references for annotations/figures are not deeply interpreted; figure
  `/Alt` is surfaced.
- Tagged table cell spanning is not modelled yet; rows/cells are dense strings.
- Malformed tag trees are depth-limited and cycle-protected, but heavily broken
  tags can still produce partial semantic output.
