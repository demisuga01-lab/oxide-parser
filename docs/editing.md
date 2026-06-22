# PDF Editing

Oxide can add visible content to existing PDFs, write annotations, fill and
flatten AcroForms, and perform true redaction. Additive edits append new content
streams as overlays or prepend them as underlays, then merge only the needed page
resources. Redaction is different: it rewrites affected page content streams so
the removed text/image/path operators are no longer present.

```rust
use oxide_engine::{
    EditMode, HeaderFooterOptions, PdfEditor, WatermarkOptions,
};

let input = std::fs::read("input.pdf")?;
let mut editor = PdfEditor::open_bytes(input)?;
editor
    .add_watermark_text("CONFIDENTIAL", WatermarkOptions::default())?
    .add_footer("Page {page} of {total}", HeaderFooterOptions::default())?;

let rewritten = editor.save_to_bytes(EditMode::FullRewrite)?;
let incremental = editor.save_to_bytes(EditMode::Incremental)?;
# Ok::<(), oxide_engine::OxideError>(())
```

## Content Layers

`OverlayLayer::Overlay` draws after existing page content. `OverlayLayer::Underlay`
draws before existing page content. Existing content stream bytes are not edited;
the page `/Contents` array is rebuilt to include new stream references around the
original references.

The editor supports:

- diagonal text watermarks with opacity and rotation
- headers and footers with `{page}` and `{total}` tokens
- direct text additions
- rectangle fills/strokes
- RGBA image stamps with `/SMask`
- JPEG image stamps embedded with `DCTDecode`

Each generated content stream is wrapped in `q`/`Q`, sets its own graphics state,
and uses resource names prefixed with `OxEd` to avoid clobbering existing page
resources.

## Redaction

`PdfEditor::redact(page, rect, RedactionOptions::default())` removes content
whose text glyphs, image placement, path bounds, or annotation rectangles
intersect the redaction rectangle, then draws the redaction mark. Redaction is
full-rewrite only. Incremental updates preserve the old revision bytes by
design, so they would retain the sensitive content in the original byte prefix.

Validation should always re-extract text after redaction. The committed tests
assert that the redacted string is absent from Oxide extraction and that
intersecting image invocations are removed/blanked, not only covered.

`RedactionOptions::scrub_metadata` also removes redacted strings from PDF string
objects during the full rewrite, covering document info and other string
metadata reachable as normal PDF objects.

## Annotations

The editor can add highlight, text-note, stamp, and URI link annotations, edit
annotation contents by page index, and delete annotations intersecting a
rectangle. Added annotations include `/Rect`, `/Subtype`, metadata, color, and
normal `/AP` appearance streams where applicable. The visual highlight/stamp is
also appended to page content so Oxide's current renderer shows the result even
though non-widget annotation rendering is still conservative.

## Forms

`set_form_text`, `set_form_checkbox`, and `set_form_choice` update AcroForm
field values and regenerate explicit widget `/AP` appearances. `flatten_forms`
bakes the current widget values into static page content, removes widget
annotations from pages, and removes `/AcroForm` from the catalog. Flattened text
is extractable as page content.

## Incremental Updates

`EditMode::Incremental` writes:

1. the original PDF bytes unchanged
2. changed page dictionaries and new content/image objects
3. a classic xref section for only the appended objects
4. a trailer with `/Prev` pointing to the prior `startxref`
5. a new `startxref` and `%%EOF`

This preserves the original byte prefix exactly and makes the original revision
recoverable. It is the foundation required for signature-preserving workflows in
the signature prompt.

Encrypted inputs are rejected for incremental editing until encrypted appended
objects are implemented. Full-rewrite editing currently supports generation-0
source objects, matching the writer's existing generation-0 output model.

## Example

Run:

```bash
cargo run -p oxide-engine --example editing -- target/editing-demo
```

The example writes `editing-base.pdf`, `editing-full-rewrite.pdf`, and
`editing-incremental.pdf`, plus redaction, annotation, and flattened-form
outputs.

## Follow-Ups

Prompt 7 builds on this editing pillar with higher-level document assembly and
workflow APIs. Encrypted appended objects remain a future extension; encrypted
inputs are still rejected for incremental editing.
