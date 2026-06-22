# PDF Editing

Oxide can add visible content to existing PDFs without rewriting existing page
operators. The editing API appends new content streams as overlays or prepends
them as underlays, then merges only the needed page resources.

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
`editing-incremental.pdf`.

## Follow-Ups

Prompt 6 adds destructive redaction, annotations, and form filling. This prompt
is intentionally additive: it does not remove existing page content or rewrite
operator streams in place.
