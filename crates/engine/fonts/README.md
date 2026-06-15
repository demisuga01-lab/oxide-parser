# Bundled fonts

These font programs are embedded into `oxide-engine` (via `include_bytes!` in
`src/render/font_rasterizer.rs`) and used as **fallback / substitution fonts**
when a PDF references a non-embedded font (e.g. the standard-14 families, or a
symbolic font Oxide cannot otherwise resolve). They are *not* redistributed as
standalone fonts — they ship as substitution data inside the library.

Both families are open fonts under permissive licenses. Their full license
texts are included in this directory as the licenses require.

| File(s) | Family | License | Text |
|---|---|---|---|
| `DejaVuSans.ttf` | DejaVu Sans | DejaVu / Bitstream Vera (permissive, MIT-like) + Arev; DejaVu changes public domain | [`LICENSE-DejaVu.txt`](LICENSE-DejaVu.txt) |
| `LiberationSans-*.ttf`, `LiberationSerif-*.ttf`, `LiberationMono-*.ttf` | Liberation Sans / Serif / Mono | SIL Open Font License (OFL) 1.1 | [`LICENSE-Liberation.txt`](LICENSE-Liberation.txt) |

## Provenance

- **DejaVu Sans** — <https://dejavu-fonts.github.io/> — derives from Bitstream
  Vera (© 2003 Bitstream, Inc.) and Arev (© 2006 Tavmjong Bah); the DejaVu
  modifications are released into the public domain. The bundled
  `LICENSE-DejaVu.txt` is reproduced verbatim from the font's own embedded
  license (OpenType `name` table, ID 13).
- **Liberation** — <https://github.com/liberationfonts/liberation-fonts> —
  metric-compatible substitutes for Arial / Times New Roman / Courier New.
  Digitized data © 2010 Google Corporation; © 2012 Red Hat, Inc. Licensed
  under the SIL OFL 1.1 (Reserved Font Name: "Liberation").

## License-compliance notes

- The OFL permits bundling/embedding and redistribution (including in
  commercial software) provided the copyright notice + license travel with the
  fonts and the Reserved Font Name is not used for modified versions. We ship
  the unmodified fonts with their license, so this is satisfied.
- The Bitstream Vera / DejaVu terms are MIT-like (use/copy/merge/distribute
  freely) with a rename-on-modification clause and a "not sold by itself"
  clause; we ship the unmodified font with its license, so this is satisfied.
- Neither family is copyleft; both are compatible with this project's permissive
  (MIT-or-Apache-class) positioning. See the repository `NOTICE` /
  `docs/licenses.md` for the consolidated third-party attribution.
