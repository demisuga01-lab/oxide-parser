# PDF Authoring

Oxide can create new PDFs from scratch with `PdfBuilder`. The authoring layer
builds a normal PDF object graph and serializes it through the existing writer,
so authored output uses the same xref-stream/object-stream machinery as the
structural writer.

```rust
use oxide_engine::authoring::{PageSize, PdfBuilder};
use oxide_engine::{Color, GraphicsStyle, StandardFont, TextStyle};

let mut doc = PdfBuilder::new();
doc.set_title("Report").set_author("Oxide");

let page = doc.add_page(PageSize::LETTER);
page.draw_text(
    "Quarterly report",
    72.0,
    720.0,
    &TextStyle::standard(StandardFont::HelveticaBold, 18.0),
)?;
page.draw_rect(
    72.0,
    680.0,
    180.0,
    24.0,
    &GraphicsStyle::fill_stroke(
        Color::device_rgb(0.92, 0.95, 0.98),
        Color::device_rgb(0.1, 0.2, 0.3),
        1.0,
    ),
);

doc.save("report.pdf")?;
# Ok::<(), oxide_engine::OxideError>(())
```

## Coordinates

Authoring uses native PDF user space: the origin is the bottom-left corner of
the page, x grows right, and y grows upward. `draw_text("x", 72, 720, ...)`
places the text baseline one inch from the left edge and ten inches above the
bottom of a US Letter page.

For UI-style top-left positioning, use `PdfPageBuilder::pdf_y_from_top()` or
`draw_text_from_top()`.

## Pages

`PageSize` provides common sizes (`LETTER`, `LEGAL`, `A3`, `A4`, `A5`), custom
point sizes, and inch/mm helpers:

```rust
let portrait = PageSize::A4;
let landscape = PageSize::A4.landscape();
let badge = PageSize::inches(3.5, 2.0);
let custom = PageSize::custom(300.0, 200.0);
```

Margins can be attached to a page for layout helpers with
`add_page_with_margins`, but primitive drawing APIs always accept explicit PDF
coordinates.

## Text And Fonts

The authoring API supports:

- All PDF Standard-14 faces via `StandardFont`: Helvetica, Times, Courier,
  Symbol, and ZapfDingbats.
- A bundled Unicode baseline via `FontFace::BuiltinUnicode`, embedded as a
  Type0 TrueType font with ToUnicode and CIDToGIDMap.
- Document-registered TrueType fonts via `PdfBuilder::register_font_bytes`.
  Custom fonts are embedded as whole Type0/CIDFontType2 fonts with Identity-H,
  CIDToGIDMap, widths, and ToUnicode so authored text is extractable.
- `draw_text`, `draw_text_line`, `draw_text_from_top`.
- `wrap_text` and `draw_paragraph` with left, center, and right alignment.
- Gray, RGB, and CMYK fill colors through the shared `Color` type.

Standard fonts use WinAnsi encoding. If text contains characters outside
WinAnsi, use `TextStyle::unicode(...)` or `FontFace::BuiltinUnicode`.

```rust
let serif = doc.register_font_bytes(
    "LiberationSerif",
    include_bytes!("../crates/engine/fonts/LiberationSerif-Regular.ttf").as_slice(),
)?;
doc.add_page(PageSize::LETTER).draw_text(
    "Unicode custom font: cafe \u{03c0}",
    72.0,
    720.0,
    &TextStyle::new(serif, 12.0),
)?;
# Ok::<(), oxide_engine::OxideError>(())
```

Custom font subsetting is intentionally deferred. Current output is correct but
larger because the complete TrueType program is embedded. CFF/OpenType
`FontFile3` embedding is also a follow-up.

## Graphics

The graphics API supports line, rectangle, rounded rectangle, circle, ellipse, polygon,
and arbitrary paths. Each draw is wrapped in `q`/`Q` and can set stroke color,
fill color, line width, line cap/join, and dash pattern through `GraphicsStyle`.

## Images

Register images on the document and place them on pages with `draw_image`.

- JPEG bytes are embedded directly with `DCTDecode`; they are decoded only to
  read dimensions and channel count.
- PNG bytes and raw RGB/RGBA samples are embedded as Flate-compressed image
  XObjects.
- Gray/RGB alpha channels are emitted as grayscale `/SMask` image XObjects.

```rust
let jpeg = doc.add_jpeg_image(std::fs::read("photo.jpg")?)?;
let rgba = doc.add_rgba_image(2, 2, vec![
    255, 0, 0, 255, 0, 255, 0, 180,
    0, 0, 255, 120, 255, 255, 0, 64,
])?;

let page = doc.add_page(PageSize::LETTER);
page.draw_image(jpeg, 72.0, 560.0, 144.0, 96.0);
page.draw_image(rgba, 240.0, 560.0, 96.0, 96.0);
# Ok::<(), oxide_engine::OxideError>(())
```

## Tables

`TableBuilder` renders fixed-width columns, optional header rows, borders,
fills, padding, wrapped cell text, and per-column alignment. `draw_on_page`
draws a table at an explicit top-left anchor; `FlowDocument::add_table` handles
page breaks and repeats the header row on continuation pages.

```rust
use oxide_engine::{TableBuilder, TableColumn, TextAlign};

let mut table = TableBuilder::new(vec![
    TableColumn::new(96.0),
    TableColumn::new(260.0).align(TextAlign::Left),
]);
table.set_header(["Metric", "Notes"]);
table.add_row(["Throughput", "Wrapped text is measured from glyph widths."]);
```

## Flow Layout

`FlowDocument` is a single-column layout helper over `PdfBuilder`. It tracks a
cursor, wraps paragraphs, inserts headings, lists, images, tables, spacers, and
creates new pages automatically when content reaches the bottom margin.

```rust
use oxide_engine::{FlowDocument, Margins};

let mut flow = FlowDocument::new(PageSize::LETTER, Margins::all(72.0));
flow.add_heading("Report", 1)?;
flow.add_paragraph(
    "Flowed text wraps within the page margins and continues on new pages.",
    &TextStyle::standard(StandardFont::Helvetica, 11.0),
    &ParagraphStyle::new(),
)?;
flow.add_table(&table)?;
flow.save("flow-report.pdf")?;
# Ok::<(), oxide_engine::OxideError>(())
```
