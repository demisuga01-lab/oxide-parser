# Document parsing: the canonical model and the `parse` command

Oxide parses a PDF into **one canonical document model** and serializes it to
clean, structured output for AI/RAG pipelines and data automation:

```
PDF ──► parse ──► Document (canonical model) ──► Markdown │ JSON │ HTML │ (CSV per table)
```

Every extraction path converges on the same [`Document`] model, and the output
stage is written **once** against it. The digital-born path (tags-first when the
PDF is tagged, geometric otherwise) and — in a later release — the OCR path both
produce the *same* model, so a heading is a heading regardless of how it was
recovered.

## CLI

```
oxide parse --input f.pdf --format markdown|json|html [options]
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--format` | `markdown` | `markdown` (RAG/AI-facing), `json` (full faithful model), or `html` (semantic) |
| `--pages` | `all` | Page range: `all`, `1`, `2-5`, or `1,3,7` |
| `--keep-furniture` | off | Keep running headers/footers and page numbers in the body/output (off → omitted, since they are usually noise for RAG) |
| `--images-dir DIR` | — | Write extracted figure images to `DIR` and reference them by path *(reserved; image bytes are surfaced in a later release)* |
| `--dehyphenate` | off | Join words split across line ends (`compi-\nlation` → `compilation`). RAG-friendly; mutates characters, so off by default |
| `--normalize-ligatures` | off | Map ligature codepoints to plain letters (`ﬁ`→`fi`). Off by default |
| `--min-confidence N` | `0.0` | Drop blocks classified below confidence `N` |
| `--password PW` | — | Password for an encrypted PDF |
| `--output FILE` | stdout | Where to write the serialized output |

> `document-model` remains as a thin back-compat alias for `parse` (JSON/Markdown
> only). New integrations should use `parse`.

## Page classifier & per-page routing

Parsing is routed **per page**, not per document, so a *mixed* PDF (a born-digital
report with a few scanned inserts, or a scan with a digital cover) is handled
correctly. A classifier labels each page from combined signals — extractable
text coverage, image coverage, and an existing/invisible (`Tr 3`) text layer:

| `PageSource` | meaning | routing |
| --- | --- | --- |
| `digital_born` | real extractable text | digital-born extraction pass |
| `digital_born_over_image` | full-page image **with** a text layer (a searchable scan the producer already OCR'd) | **uses the existing text layer** — never re-OCR'd |
| `scanned` | (almost) no text + a dominant full-page image | routed to OCR; until OCR exists, emits a placeholder (a note + the full-page scan as a figure) so the pipeline runs end-to-end |

The document-level `source` rolls these up: `tagged` if the model came from
`/StructTreeRoot`, else `digital_born` if every page is, else `mixed`.

## The model

`Document` is the public, **versioned** contract (`schema_version`, currently
`1.1` — `1.1` added per-page provenance):

```jsonc
{
  "schema_version": "1.1",
  "metadata": { "title": "...", "author": "...", "page_count": 14,
                "pdf_version": "1.4", "producer": "...", "is_tagged": false,
                "is_encrypted": false, "creation_date": "...", ... },
  "source":   { "kind": "digital_born" },          // or "tagged" | "mixed" | "ocr"
  "body":     [ /* the reading-ordered Block stream — the primary consumable */ ],
  "pages":    [ { "number": 1, "width": 612, "height": 792,
                  "source": "digital_born",        // per-page routing decision (1.1)
                  "classification": { "text_coverage": 0.12, "image_coverage": 0.0,
                                      "char_count": 1840, "has_invisible_text": false,
                                      "confidence": 0.95 },
                  "block_ids": [0, 1, 2, ...] } ]   // per-page view; indexes body by id
}
```

- **`body`** is the flattened, cross-page reading-ordered block list (what
  Markdown/RAG ingest). With furniture omitted (the default), furniture blocks
  are excluded here.
- **`pages`** preserves page boundaries + geometry + **per-page provenance**, and
  references blocks by id — including furniture, so **no information is lost**
  even when the body omits it.

### Block

Every block keeps geometry, order, and confidence (lossless-enough), plus a
typed payload tagged by `kind`:

```jsonc
{
  "id": 0,                       // stable; referenced by caption↔figure/table links
  "page": 1,
  "bbox": [x0, y0, x1, y1],      // PDF user space, y-up; [0,0,0,0] when unknown
  "reading_order": 0,            // global index, ascending
  "confidence": 0.73,            // 0..1; low → generic "text"
  "kind": "heading", "level": 1, // the typed payload (flattened)
  "text": [ { "text": "...", "bold": true } ]
}
```

`kind` is one of: `title`, `heading` (`level` 1–6), `paragraph`, `list`
(`ordered` + `items`), `table` (full [DI1 structure](tables.md)), `figure`
(`alt`, `image`, `caption`), `caption` (`target`), `header`, `footer`,
`page_number`, `text` (honest low-confidence fallback). `code` and `quote` are
reserved.

### Inline text (emphasis + links survive)

Block text is **not** a bare string — it is a run-list of styled spans, so
`**bold**`, `*italic*`, and `[text](href)` survive into Markdown/HTML:

```jsonc
"text": [
  { "text": "see " },
  { "text": "the site", "bold": true, "link": "https://example.com" }
]
```

Bold/italic are derived from font flags. Link extraction (`/Link` annotations +
URI actions) is wired through the model and serializers and is populated by the
digital-born consolidation release.

## Serializer conventions

**Markdown** (RAG/AI-facing):

- `Title` → `#`; `Heading{level:n}` → `n+1` hashes (so a doc title and an `H1`
  don't collide), clamped to `######`.
- Spans render `**bold**` / `*italic*` / `***bold italic***` / `[text](href)`;
  Markdown metacharacters in literal text are escaped.
- Lists: `- ` (unordered) / `1. ` (ordered).
- Tables: GitHub pipe tables from the flattened grid; row/col spans are
  flattened per the [table blank-fill convention](tables.md) and annotated with
  an HTML comment so the lossy flatten is explicit.
- Figures: `![alt](image-ref)` followed by the linked caption (italic).
- Furniture: omitted by default; emitted as HTML comments when kept.

**HTML**: semantic `<h1>`…`<h6>`, `<p>`, `<ul>`/`<ol>`, `<figure>`/`<figcaption>`,
and span/header-aware `<table>` (reusing the [DI1 table serializer](tables.md)).

**JSON**: the full faithful model (the lossless format), `serde`-serialized.

**CSV**: kept per table as a flattened convenience export (see [tables.md](tables.md)).

## Robustness (digital-born pass)

The digital-born extraction pass handles the things that break naive extraction:

- **Rotated pages** (`/Rotate` 90/180/270): text and graphics coordinates are
  normalized into upright reading orientation before layout/ordering, so a
  landscape-rotated page reads correctly (always on).
- **Multi-column with a spanning title**, **figures + captions**, **furniture**
  (running headers/footers/page numbers detected by margin position + cross-page
  repetition, marked so Markdown can omit them), and **paragraph grouping**
  (wrapped lines are merged into one paragraph, not split per line) — all via the
  reused layout/reading-order/semantics stages.
- **Hyperlinks**: `/Link` annotations with URI actions are matched to the text
  blocks their rectangles overlap, so `[text](href)` survives into Markdown/HTML.
- **De-hyphenation** and **ligature normalization** are opt-in (`--dehyphenate`,
  `--normalize-ligatures`) — they clean text for RAG but mutate the extracted
  characters, so they default off for JSON fidelity.

## OCR seam (for the OCR stage)

`scanned` pages currently emit a placeholder (a note block + the full-page scan
as a figure) and the pipeline runs end-to-end without OCR. The OCR stage plugs in
behind the same gate: for each `scanned` page it produces **positioned text**
(the same shape the digital-born collector emits), feeds it through the shared
builder, and replaces the placeholder with the recovered blocks — leaving the
model source-agnostic (an OCR'd heading is the same `heading` block as a
digital-born one).

## Determinism

The same PDF yields **byte-identical** serialization: ids and reading order come
from the deterministic builder, the serializers iterate `body` in order, and no
`HashMap` is iterated to produce output. This makes the output safe to cache,
diff, and snapshot-test.

## Engine API

```rust
use oxide_engine::{ContentEngine, ParseOptions, SerializeOptions};

let engine = ContentEngine::open_path("input.pdf")?;
let doc = engine.parse_document(&ParseOptions::default())?;   // -> Document

let md   = doc.to_markdown(&SerializeOptions::default());
let json = doc.to_json();
let html = doc.to_html(&SerializeOptions::default());
```
