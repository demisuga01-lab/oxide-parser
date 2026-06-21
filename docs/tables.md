# Table Detection & Structure Extraction

`oxide extract-tables` detects tables and emits either a flattened CSV view or a
span-aware structured model. Poppler CLIs expose text placement, but they do not
recover table structure such as merged cells, header hierarchy, or nested tables.

```
oxide extract-tables in.pdf                         # CSV to stdout
oxide extract-tables in.pdf --format json --structure
oxide extract-tables in.pdf --format html
oxide extract-tables in.pdf -p 2 -o tables.csv
oxide extract-tables in.pdf --min-confidence 0.8
```

The command is additive; normal text extraction is unchanged.

## Detection Sources

Tagged PDFs are preferred when an authored `/StructTreeRoot` exposes semantic
`Table` / `TR` / `TH` / `TD` elements. Those tables are returned with
`source: "semantic"` and `TH` cells are marked as headers.

Untagged PDFs use the MP25 geometry detector:

1. Ruled tables: horizontal and vertical drawn rules are clustered into row and
   column boundaries. Missing internal dividers join adjacent atomic slots into
   `rowspan` / `colspan` cells.
2. Borderless tables: baselines define rows, left-edge clusters define columns,
   and text crossing inferred gutters is treated as a colspan. Borderless
   rowspans remain a documented limitation.

## Structured Model

JSON includes the compatibility `rows` field plus span-aware `cells`:

```json
{
  "rows": [["Group", "", "Other"], ["Q1", "Q2", "Q3"]],
  "cells": [
    { "row": 0, "col": 0, "rowspan": 1, "colspan": 2,
      "text": "Group", "is_header": true, "header_scope": "both" }
  ],
  "header_hierarchy": [
    { "parent": { "row": 0, "col": 0, "text": "Group" },
      "children": [
        { "row": 1, "col": 0, "text": "Q1" },
        { "row": 1, "col": 1, "text": "Q2" }
      ] }
  ],
  "source": "ruled",
  "confidence": 1.0,
  "bbox": [50.0, 600.0, 350.0, 675.0]
}
```

Header detection is tags-first, then deterministic geometry heuristics:

- first row cells are column headers;
- a spanning first-row header promotes the next row beneath it as child headers;
- the first column is marked as row headers when body cells to the right are
  predominantly numeric.

Clear nested ruled tables inside a cell are attached as `nested_tables`.

## Output Formats

CSV is the flattened compatibility view. A spanning cell writes its text at the
origin slot and leaves covered slots blank, so the example above becomes:

```csv
Group,,Other
Q1,Q2,Q3
```

JSON emits the full structure. HTML emits a standalone document containing
`<table>` elements with `<thead>`, `<th>`, `scope`, `rowspan`, and `colspan`
attributes.

## Accuracy

The fast structure smoke test (`crates/engine/tests/table_extraction.rs`) builds
10 hand-authored PDFs and compares row/column counts, spans, headers, hierarchy,
nested tables, semantic tagged tables, and HTML serialization:

| Case | Result |
|---|---:|
| ruled simple | 100% |
| borderless simple | 100% |
| ruled colspan | 100% |
| ruled rowspan | 100% |
| two-level header | 100% |
| row-header column | 100% |
| tagged semantic table | 100% |
| nested ruled table | 100% |
| thin-rectangle rules | 100% |
| HTML escaping | 100% |

## Current Limits

- Ruled structure assumes a dominant grid per page. Very complex multi-table
  pages can still need table splitting.
- Borderless detection is heuristic and currently infers clear colspans, not
  rowspans.
- Header hierarchy is intentionally limited to the high-value two-level case.
- Nested table handling targets clear ruled tables inside a cell; deeply nested
  or irregular nested layouts are deferred.
