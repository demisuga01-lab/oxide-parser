# HTML / XML Output (`to-html`, `pdftohtml`-equivalent)

`oxide to-html` converts a PDF to HTML or XML, reusing the text pipeline, the
raster renderer, and the image encoder — it is **assembly**, not new parsing or
rendering.

```
oxide to-html in.pdf -o out.html                 # complex (default), stdout if no -o
oxide to-html in.pdf --background -o out.html     # raster bg + selectable text overlay
oxide to-html in.pdf --xml -o out.xml             # positioned text fragments
oxide to-html in.pdf --simple -o out.html         # flowing paragraphs
oxide to-html in.pdf -p 1,3-5 --password p -o out.html
```

Output is a single self-contained HTML/XML document for the selected pages.

## Approach chosen (and why)

The round offered three fidelity backends (positioned-text, raster-background +
overlay, embedded-SVG). Two findings drove the choice:

1. The text pipeline exposes **per-fragment position + size + writing
   direction** (`TextChunk`/`TextLine`: `x`, `y`, `font_size`, `is_rtl`,
   reading-order- and BiDi-corrected `text`) — ideal for positioned HTML.
2. The engine does **not** separately expose per-image **device placement** (an
   `ImageReference` carries the bytes and intrinsic size, but not where the
   image is drawn on the page).

So the implemented modes are:

- **Complex** (default): a per-page container sized to the page, with
  absolutely-positioned text laid out from the reading-order pipeline's
  `TextLine`s (one `<div class="t">` per line, `left`/`top`/`font-size` from the
  line box, `dir="rtl"` for RTL lines). Reading order and BiDi are already
  correct because the lines come straight from the text pipeline.
- **Complex + `--background`** (the **highest-fidelity** mode): the page is also
  rendered to a PNG (existing raster path) and placed behind the text. The
  background reproduces **every** image, shading, and vector mark in its exact
  position — which neatly **sidesteps the missing per-image-placement problem**
  (the rendered page already contains the images where they belong) and handles
  all graphics the pure-HTML approach can't. The overlaid text stays
  positioned and selectable; `--invisible-text` makes it an OCR-style invisible
  layer over the raster.
- **XML** (`pdftohtml -xml` analogue): `<pdf2xml>` → `<page>` → `<text top left
  width height font-size>` fragments, top-left origin.
- **Simple**: flowing `<p>` paragraphs from the reading-order text — readable,
  low fidelity, no positioning.

The embedded-SVG option was considered but **not** chosen as the default: the
Mega-Prompt 19 SVG sink rasterizes whole pages that contain images/shadings
anyway, so `--background` gives the same fidelity more directly. Embedded-SVG
remains a possible future backend.

## Text positioning, reading order, escaping, images

- **Positioning**: PDF uses a bottom-left origin where `y` is the baseline; the
  exporter flips to a top-left CSS `top` (`page_height − (y − y0) − font_size`)
  and scales points→px (default `96/72`). MediaBox lower-left (`x0`,`y0`) is
  normalized out.
- **Reading order / BiDi**: lines come from `ReadingOrderReconstructor`, which
  already applies column ordering and UAX#9 BiDi (Mega-Prompt 7), so RTL text is
  in display order; RTL lines also get `dir="rtl"`.
- **Escaping / Unicode**: the five HTML-significant characters are escaped
  (`& < > " '`); XML text escapes `& < >`. Output declares
  `<meta charset="UTF-8">` / `encoding="UTF-8"` and passes all Unicode through
  unchanged.
- **Images**: handled via the `--background` raster mode (the page render
  contains every image positioned correctly), rather than per-image `<img>`
  placement — chosen because per-image device placement isn't separately
  available (see above). Per-image `<img>` placement is a noted follow-up.

## Validation

`crates/engine/tests/html_output.rs` (no headless browser in this environment,
so text-content + structural-position checks, plus the inherent fidelity of the
raster-background mode):

- **Well-formedness + positioning**: complex output is a valid HTML5 document
  with UTF-8, page-sized containers, and `class="t"` fragments carrying
  `left`/`top`/`font-size`; the "Hello"/"World" fixture fragments land at
  plausible coordinates (left ≈ 66.7px, font ≈ 16px at the default scale).
- **Background mode**: embeds the exact raster PNG as a data URI and keeps the
  text.
- **Escaping**: every `<` in the output begins a real tag (no stray brackets).
- **RTL**: the Arabic `ArabicCIDTrueType.pdf` text appears and an RTL line is
  marked `dir="rtl"`.
- **Multi-column**: both columns' text appears (`generated_two_columns.pdf`).
- **XML / simple** modes emit the expected structures.
- **`pdftohtml` text cross-check**: on `tracemonkey.pdf` p3, Oxide's HTML shares
  **100%** of Poppler `pdftohtml`'s words (markup differs, as expected; the text
  content agrees).

## Future enhancements (honest)

- **Per-fragment colour**: `TextChunk` doesn't currently carry fill colour, so
  complex-mode text is emitted black (the overwhelmingly common case). Threading
  the fill colour through would let coloured text render in pure-HTML mode.
- **Per-image `<img>` placement** (vs whole-page raster background): would need
  the content-stream image-placement CTM surfaced (the raster/SVG path computes
  it internally but doesn't expose it).
- **Selectable-font fidelity**: embed/subset the actual typefaces (`@font-face`)
  for exact glyph shapes instead of `font-family:sans-serif`.
- **Multi-file output** (an index + per-page files + sidecar images, like
  `pdftohtml`'s default) — currently a single self-contained document.
- **Reflowable / semantic HTML** and **table detection**.
- Server endpoint (`POST /api/v1/to-html`) — deferred; the CLI is complete.
