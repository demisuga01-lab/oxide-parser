# Vector Output — SVG (`pdftocairo -svg`-equivalent)

`oxide render --format svg` emits one SVG document per page. SVG preserves
scalability for web, print-prep, and vector-editing workflows.

```
oxide render in.pdf --format svg -o pages.zip            # all pages, ZIP of .svg
oxide render in.pdf --format svg -p 1,3-5 -o pages.zip   # a page range
oxide render in.pdf --format svg --dpi 150 -o pages.zip  # device scale
```

Output follows the existing `render` convention: one `page-NNN.svg` per page
inside the output ZIP. (PostScript / EPS are deferred — see the end.)

## Architecture (Part A finding)

The raster interpreter (`render/page_renderer.rs`, ~2750 lines) is **tightly
coupled** to its `PixelBuffer`: the `RenderState` owns the buffer and every
draw operation paints into it directly. A full `RenderSink` trait refactor of
that interpreter would be large and would put the verified raster path at risk
— which the round spec explicitly warns against.

However, the **geometry seam is clean**: `flatten_path(path, ctm, viewport)`
already produces *device-space* polylines, and glyph **outlines** are available
(`ttf-parser` / bare-CFF). So instead of an invasive refactor, the SVG backend
is a **sibling renderer** (`render/svg.rs`) that reuses the same primitives:

- `GraphicsState` for every state operator (`cm`, `q`/`Q`, `Tf`, `Td`, colour
  ops, …) — identical interpretation, for free;
- `flatten_path` for user→device geometry, so SVG paths live in the **same
  device-pixel space** as the raster output (visual equivalence by
  construction);
- shared `render/glyph_outline.rs` + `render/text_decode.rs` helpers for
  text-as-outlines.

**Consequence:** the raster path is **untouched** — `page_renderer.rs`,
`path.rs`, `buffer.rs` were not modified — so the raster golden-image / quality
regressions pass identically (verified).

## What the SVG sink emits

For pages using only natively-representable operations:

- **Paths** → `<path d="…">` (device-space polylines) with `fill`, `fill-rule`
  (nonzero/evenodd), `stroke`, `stroke-width` (CTM+viewport-scaled),
  `stroke-dasharray`, and `fill-opacity`/`stroke-opacity` (from `ca`/`CA`).
- **Text** → one `<path>` per glyph (text-as-**outlines**): the glyph outline,
  scaled by font size and transformed by the text matrix × CTM, filled (or
  stroked) with the text colour. Identical outlines to the raster renderer, so
  text is pixel-faithful. (Selectable `<text>` with embedded/subset fonts is a
  future enhancement.)
- **Clipping** → `<clipPath>` defs referenced via `clip-path`, tracking the
  `q`/`Q` clip stack.
- **Colour** → device Gray/RGB/CMYK and Separation/DeviceN tint transforms,
  resolved to `#rrggbb` the same way the raster path does.

## Rasterize-and-embed fallback (the prompt's prescribed fallback)

Some operations have no faithful native-SVG mapping *here*: **images** (XObject
`Do` / inline `BI`), **shadings** (`sh`), **shading/tiling pattern fills**
(`scn`/`SCN` with a pattern name), Form XObjects, and soft masks. When a page
uses any of these, the whole page is emitted as a **single rasterized PNG
`<image>`** (pixel-identical to the raster render). `SvgPage::is_rasterized`
reports this, and the CLI prints how many pages used the fallback.

This guarantees **visual correctness everywhere** while delivering real,
scalable vector SVG for the common path/text page. Pure-vector pages and
fallback pages are decided per page by a one-pass content scan
(`needs_raster_fallback`).

## Validation

`crates/engine/tests/svg_output.rs` rasterizes Oxide's SVG with the pure-Rust
`resvg`/`usvg`/`tiny-skia` stack (a **dev/test-only** dependency — never linked
into the product binaries) and compares it (PSNR, alpha-composited on white)
against Oxide's own raster render, on pages with **actual visible marks**:

| Page | Mode | PSNR vs Oxide raster |
|---|---|---:|
| `multi_stream.pdf` p1 | true vector | **38.82 dB** |
| `tracemonkey.pdf` p3 | true vector | **32.41 dB** |
| `tracemonkey.pdf` p2 | raster-embed fallback | **99 dB** (exact) |

Other checks: the SVG is well-formed and correctly sized; a text-only page is
emitted as true vector (`<path>`, no `<image>`); an image page takes the
fallback (`data:image/png;base64,…`); and Poppler's `pdftocairo -svg` output is
parsed by `resvg` as a structural cross-check.

## Future enhancements (honest)

- **PostScript / EPS** (`pdftops`, `pdftocairo -ps/-eps`) — **deferred this
  round** per the spec's priority guidance (SVG first, fully). The sibling-sink
  architecture and shared geometry/text helpers make a `PostScriptSink` a
  follow-up that reuses the same seam: PS path construction + `fill`/`eofill`/
  `stroke`, text-as-path-outlines, `image` operator for rasters, `shfill`
  (PS L3) or rasterize-embed for shadings, DSC `%%Page`/`%%BoundingBox`
  comments, and an EPS single-page variant.
- **Selectable text** via embedded/subset fonts in the SVG (`<text>`/`@font-face`).
- **Native axial/radial SVG gradients** (`<linearGradient>`/`<radialGradient>`)
  for ShadingType 2/3 instead of the whole-page fallback; mesh shadings (4–7)
  would still fall back.
- **Per-region** image embedding (an `<image>` per image XObject under its
  transform) instead of whole-page rasterization, so a page mixing text and one
  image stays mostly vector.
- **PDF-to-PDF** (`pdftocairo -pdf`) via the Mega-Prompt 16 writer.
