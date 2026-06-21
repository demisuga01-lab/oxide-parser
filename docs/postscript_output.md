# PostScript / EPS Output (`pdftops` / `pdftocairo -ps`/`-eps`-equivalent)

`oxide render --format ps` and `--format eps` emit PostScript. This closes the
last Poppler CLI tool-surface gap — Oxide now has an equivalent for **all 12**
Poppler command-line utilities.

```
oxide render in.pdf --format ps  -o out.ps            # multi-page DSC PostScript
oxide render in.pdf --format ps  -p 1,3-5 -o out.ps   # a page range
oxide render in.pdf --format eps -o pages.zip         # one EPS per page (ZIP)
oxide render in.pdf --format ps  --dpi 150 -o out.ps  # device scale
```

- **`--format ps`** writes a single, DSC-conformant multi-page `.ps` document
  directly to `--output` (matching `pdftops`). If `--output` still has the
  default `.zip` extension it is retargeted to `.ps`.
- **`--format eps`** writes one EPSF-conformant `.eps` per page into the output
  ZIP (EPS is single-page by definition, matching `pdftops -eps` /
  `pdftocairo -eps`).

## Architecture — a third output of the SAME interpretation

The PostScript emitter (`crates/engine/src/render/postscript.rs`) is a **sibling
renderer**, exactly like the SVG sink (`render/svg.rs`). It does **not**
introduce a third independent content-stream walker; it reuses:

- `GraphicsState` for every state operator (`cm`, `q`/`Q`, `Tf`, `Td`, colour
  ops, …) — identical interpretation to the raster and SVG backends, for free;
- `flatten_path` for user→device geometry, producing the **same device-pixel
  polylines** the raster and SVG paths use (so the emitted PS rasterises
  pixel-for-pixel like the raster render);
- the shared `glyph_outline` / `text_decode` helpers for text-as-outlines.

**Consequence:** the raster and SVG paths are untouched; PS is purely additive.

### Device-pixel coordinates in a bottom-left PostScript world

PostScript's default user space is bottom-left / y-up; our flattened device
coordinates are top-left / y-down (image space). Rather than maintain a second
coordinate convention, each page body is wrapped in a single flip prologue:

```
gsave
0 <height> translate
1 -1 scale
… device-space path/text/clip operators …
grestore
```

so the device polylines are emitted verbatim and land correctly on the page.

## What the PS sink emits

For pages using only natively-representable operations:

- **Paths** → `moveto`/`lineto`/`closepath` + `fill` (nonzero) / `eofill`
  (even-odd) / `stroke` (after `setlinewidth`/`setlinecap`/`setlinejoin`/
  `setdash`, all in device-pixel units).
- **Colour** → `setrgbcolor` (named Separation/DeviceN spaces resolved through
  the page resources, same as SVG). Constant alpha `< 1` is approximated by
  blending toward the white page background (PS Level 2 has no constant-alpha
  operator).
- **Clipping** → the clip path + `clip`/`eoclip`, composed with `gsave`/
  `grestore` (PDF `q`/`Q` map to `gsave`/`grestore`), reproducing the PDF
  clip-stack semantics.
- **Text** → one filled (or stroked) path per glyph (text-as-**outlines**): the
  glyph outline scaled by font size and transformed by text-matrix × CTM. Same
  outlines as the raster/SVG renderers, so text is pixel-faithful.

### Rasterize-embed fallback

Pages using operations PostScript can't faithfully express *here* — images
(`Do`/inline), shadings (`sh`), tiling/shading patterns, Form XObjects, soft
masks, non-trivial blend modes — fall back to embedding the **whole page as one
rasterised image** drawn with the `colorimage` operator (ASCII-hex RGB samples,
so the output is 7-bit-clean conforming PostScript). This is the same strategy
the SVG sink uses, and it guarantees visual correctness everywhere.

The trigger is reported via `PsPage::is_rasterized` (and surfaced in the CLI's
"N page(s) used the raster-embed fallback" message).

## Document structure (DSC / EPSF conformance)

Multi-page PostScript:

```
%!PS-Adobe-3.0
%%Creator: Oxide PDF Toolkit
%%LanguageLevel: 2
%%BoundingBox: 0 0 <maxW> <maxH>
%%Pages: <n>
%%EndComments … %%Page: i i … showpage … %%Trailer %%EOF
```

EPS (single page):

```
%!PS-Adobe-3.0 EPSF-3.0
%%BoundingBox: 0 0 <W> <H>
%%HiResBoundingBox: 0 0 <W>.0 <H>.0
%%EndComments … <page body in gsave/grestore> … %%EOF
```

EPS conformance: **no `setpagedevice`, no `showpage`** — the page body is wrapped
in its own `gsave`/`grestore` so it never leaks graphics state to an embedding
document.

## Validation

Ghostscript (`gswin64c` / `gs`) is available in the dev/test environment and is
used to validate the output — it is a **dev/test tool only**, NOT a runtime
dependency (the crate remains pure-Rust). The validation philosophy mirrors the
SVG backend's (rasterise the vector output, compare PSNR to Oxide's own raster
render):

| Case | Validation | Result |
|---|---|---|
| `multi_stream.pdf` p1 (true vector) | PS rasterised by Ghostscript vs Oxide raster | **35.24 dB** |
| `multi_stream.pdf` p1 (true vector) | EPS rasterised by Ghostscript vs Oxide raster | **35.24 dB** |
| `image_only.pdf` p1 (rasterize-embed) | `colorimage` PS rasterised vs Oxide raster | **99 dB** (exact) |

The 35 dB true-vector figure reflects only cross-rasteriser AA differences
between Oxide's rasteriser and Ghostscript; the fallback is pixel-exact because
it embeds the raster itself. Structural tests assert DSC/EPSF conformance
(`%!PS-Adobe-3.0`, `%%BoundingBox`, `%%Pages`, per-page `showpage`, EPSF has no
`setpagedevice`/`showpage`). See `crates/engine/tests/ps_output.rs`.

## Future enhancements (honest)

- **Selectable text** via embedded/subset fonts (text is currently emitted as
  outlines — faithful, but not selectable/searchable in a PS viewer).
- **Native `shfill`** (PostScript Level 3) for axial/radial shadings instead of
  the whole-page rasterize-embed fallback.
- **Per-region image placement** via the `image`/`colorimage` operator under
  each image XObject's transform (with `DCTDecode` JPEG passthrough), so a page
  mixing text and one image stays mostly vector.
