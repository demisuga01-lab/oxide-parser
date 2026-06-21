//! Serializers for the canonical [`Document`](super::Document) model: Markdown
//! and HTML. JSON is plain serde (see [`Document::to_json`](super::Document));
//! CSV stays on [`Table`](crate::analysis::tables::Table) as a per-table export.
//!
//! All three are written *once* against the model and iterate `body` in reading
//! order, so the same [`Document`](super::Document) yields byte-identical output.
//!
//! # Markdown conventions (documented; consumers rely on these)
//!
//! - **Headings**: `Title` → `#`; `Heading{level:n}` → `n+1` hashes (so a doc
//!   title and an `H1` don't collide), clamped to `######`.
//! - **Emphasis**: spans render `**bold**`, `*italic*`, `***bold italic***`, and
//!   `[text](href)` for links. Markdown metacharacters in span text are escaped.
//! - **Lists**: `- ` (unordered) / `1. ` (ordered, renumbered from 1).
//! - **Tables**: GitHub pipe tables from the flattened `rows` grid. Row/col
//!   spans are flattened per [`Table`](crate::analysis::tables::Table)'s
//!   blank-fill convention; a table that carries a span structure gets an HTML
//!   comment note so the lossy flattening is explicit.
//! - **Figures**: `![alt](image-ref)`, followed by the linked caption (italic)
//!   if present.
//! - **Furniture** (headers/footers/page numbers): omitted by default
//!   (`omit_furniture`); when included, emitted as HTML comments so they are
//!   visible but inert for RAG ingestion.

use super::{Block, BlockKind, Document, ImageRef, InlineText, ListEntry};
use crate::analysis::tables::Table;

/// Options shared by the Markdown and HTML serializers.
#[derive(Debug, Clone, Default)]
pub struct SerializeOptions {
    /// When `true`, page furniture present in the body is still rendered (as
    /// comments). When `false` (default), furniture is skipped entirely. This is
    /// independent of [`ParseOptions::omit_furniture`](super::ParseOptions): the
    /// parse step may already have stripped furniture from the body, in which
    /// case there is nothing to skip here.
    pub include_furniture: bool,
    /// When `true`, emit a page-boundary marker each time the page changes
    /// between consecutive blocks. In Markdown this is an HTML comment
    /// (`<!-- page N -->`) plus a `---` rule; in HTML a
    /// `<hr class="page-break" data-page="N">`. Off by default (page provenance
    /// is noise for some RAG flows, valued by others). Some pipelines want page
    /// provenance for citation; this provides it without polluting prose.
    pub mark_page_breaks: bool,
    /// When `true`, annotate each block with its source page + bounding box so a
    /// chunk can be traced back to its location (valuable for RAG citation). In
    /// HTML these become `data-page`/`data-bbox` attributes on the block element;
    /// in Markdown a trailing HTML comment (`<!-- @page=N bbox=... -->`). Off by
    /// default. Markdown provenance is opt-in because it is mild noise for pure
    /// prose ingestion.
    pub include_provenance: bool,
}

// ════════════════════════════════════════════════════════════════════════════
// Markdown
// ════════════════════════════════════════════════════════════════════════════

/// Render a single block to Markdown (the same rules as [`to_markdown`], minus
/// page-break/provenance/furniture-filtering). Used by the RAG chunker
/// ([`crate::chunk`]) to render one block's text. Trailing blank lines are
/// trimmed so the caller controls spacing.
pub(crate) fn serialize_block_markdown(b: &Block, doc: &Document) -> String {
    let mut out = String::new();
    render_block_md(b, doc, &mut out);
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

pub fn to_markdown(doc: &Document, opts: &SerializeOptions) -> String {
    let mut out = String::new();
    let mut last_page: Option<u32> = None;
    for b in &doc.body {
        if b.kind.is_furniture() && !opts.include_furniture {
            continue;
        }
        // Page-boundary marker when the page changes (after the first block).
        if opts.mark_page_breaks {
            if let Some(prev) = last_page {
                if b.page != 0 && b.page != prev {
                    out.push_str(&format!("<!-- page {} -->\n\n---\n\n", b.page));
                }
            }
            if b.page != 0 {
                last_page = Some(b.page);
            }
        }
        render_block_md(b, doc, &mut out);
        if opts.include_provenance && b.bbox != [0.0; 4] {
            // Trailing comment so prose stays clean but location is recoverable.
            out.push_str(&format!(
                "<!-- @page={} bbox=[{:.1},{:.1},{:.1},{:.1}] -->\n\n",
                b.page, b.bbox[0], b.bbox[1], b.bbox[2], b.bbox[3]
            ));
        }
    }
    // Trim trailing blank lines to a single newline for stable, clean output.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn render_block_md(b: &Block, doc: &Document, out: &mut String) {
    match &b.kind {
        BlockKind::Title { text } => {
            out.push_str("# ");
            out.push_str(&inline_md(text));
            out.push_str("\n\n");
        }
        BlockKind::Heading { level, text } => {
            let hashes = "#".repeat((*level as usize + 1).clamp(2, 6));
            out.push_str(&hashes);
            out.push(' ');
            out.push_str(&inline_md(text));
            out.push_str("\n\n");
        }
        BlockKind::Paragraph { text } | BlockKind::Text { text } => {
            out.push_str(&inline_md(text));
            out.push_str("\n\n");
        }
        BlockKind::List { ordered, items } => {
            render_list_md(*ordered, items, out);
            out.push('\n');
        }
        BlockKind::Figure { alt, image, caption } => {
            render_figure_md(alt.as_deref(), image.as_ref(), out);
            if let Some(cid) = caption {
                if let Some(cap) = doc.block(*cid) {
                    if let BlockKind::Caption { text, .. } = &cap.kind {
                        out.push('*');
                        out.push_str(&inline_md(text));
                        out.push_str("*\n");
                    }
                }
            }
            out.push('\n');
        }
        BlockKind::Caption { text, target } => {
            // A caption already emitted under its figure is skipped to avoid
            // duplication; a free-standing caption (no resolved target, or target
            // not a figure/table) is rendered italic on its own.
            let emitted_under_target = target
                .and_then(|t| doc.block(t))
                .map(|t| matches!(t.kind, BlockKind::Figure { .. } | BlockKind::Table { .. }))
                .unwrap_or(false);
            if !emitted_under_target {
                out.push('*');
                out.push_str(&inline_md(text));
                out.push_str("*\n\n");
            }
        }
        BlockKind::Table { table, .. } => {
            out.push_str(&table_md(table));
            out.push('\n');
        }
        BlockKind::Header { text } => {
            out.push_str(&format!("<!-- header: {} -->\n\n", inline_md(text)));
        }
        BlockKind::Footer { text } => {
            out.push_str(&format!("<!-- footer: {} -->\n\n", inline_md(text)));
        }
        BlockKind::PageNumber { text } => {
            out.push_str(&format!("<!-- page-number: {} -->\n\n", inline_md(text)));
        }
    }
}

fn render_list_md(ordered: bool, items: &[ListEntry], out: &mut String) {
    for (i, it) in items.iter().enumerate() {
        if ordered {
            out.push_str(&format!("{}. ", i + 1));
        } else {
            out.push_str("- ");
        }
        out.push_str(&inline_md(&it.text));
        out.push('\n');
    }
}

fn render_figure_md(alt: Option<&str>, image: Option<&ImageRef>, out: &mut String) {
    let alt = alt.filter(|s| !s.is_empty()).unwrap_or("Figure");
    let href = image
        .and_then(|im| im.path.clone())
        .unwrap_or_default();
    out.push_str(&format!("![{}]({})\n", md_escape(alt), href));
}

/// Render [`InlineText`] to Markdown, applying `**`/`*`/links per span and
/// escaping Markdown metacharacters in the literal text.
fn inline_md(text: &InlineText) -> String {
    let mut out = String::new();
    for span in &text.spans {
        if span.text.is_empty() {
            continue;
        }
        let body = md_escape(&span.text);
        // Wrap emphasis innermost-to-outermost: link wraps emphasis wraps text.
        let emphasized = match (span.bold, span.italic) {
            (true, true) => format!("***{body}***"),
            (true, false) => format!("**{body}**"),
            (false, true) => format!("*{body}*"),
            (false, false) => body,
        };
        match &span.link {
            Some(href) => out.push_str(&format!("[{emphasized}]({})", md_escape_url(href))),
            None => out.push_str(&emphasized),
        }
    }
    out
}

/// Escape Markdown metacharacters in literal text so they render as data, not
/// markup. Conservative: covers the characters that change inline structure.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

fn md_escape_url(s: &str) -> String {
    s.replace(' ', "%20").replace(')', "%29")
}

/// GitHub-flavored Markdown table from the flattened `rows` grid. When the table
/// carries a span structure (covered slots), a note records the lossy flatten.
fn table_md(t: &Table) -> String {
    let cols = t.num_cols();
    if cols == 0 || t.rows.is_empty() {
        // Degenerate table: fall back to a fenced CSV so nothing is lost.
        return format!("```csv\n{}```\n", t.to_csv());
    }
    let has_spans = t
        .cells
        .iter()
        .any(|c| c.rowspan > 1 || c.colspan > 1);
    let mut out = String::new();
    if has_spans {
        out.push_str("<!-- table: row/col spans flattened to a grid -->\n");
    }
    for (r, row) in t.rows.iter().enumerate() {
        out.push('|');
        for c in 0..cols {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&cell.replace('|', "\\|").replace('\n', " "));
            out.push_str(" |");
        }
        out.push('\n');
        if r == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out
}

// ════════════════════════════════════════════════════════════════════════════
// HTML
// ════════════════════════════════════════════════════════════════════════════

pub fn to_html(doc: &Document, opts: &SerializeOptions) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    if let Some(title) = &doc.metadata.title {
        out.push_str(&format!("<title>{}</title>\n", html_escape(title)));
    }
    out.push_str("</head>\n<body>\n<article>\n");
    let mut last_page: Option<u32> = None;
    for b in &doc.body {
        if b.kind.is_furniture() && !opts.include_furniture {
            continue;
        }
        if opts.mark_page_breaks {
            if let Some(prev) = last_page {
                if b.page != 0 && b.page != prev {
                    out.push_str(&format!(
                        "<hr class=\"page-break\" data-page=\"{}\">\n",
                        b.page
                    ));
                }
            }
            if b.page != 0 {
                last_page = Some(b.page);
            }
        }
        render_block_html(b, doc, opts, &mut out);
    }
    out.push_str("</article>\n</body>\n</html>\n");
    out
}

/// Provenance attributes (` data-page=".." data-bbox=".."`) for a block, or
/// empty when provenance is off / geometry unknown.
fn prov_attrs(b: &Block, opts: &SerializeOptions) -> String {
    if !opts.include_provenance || b.bbox == [0.0; 4] {
        return String::new();
    }
    format!(
        " data-page=\"{}\" data-bbox=\"{:.1},{:.1},{:.1},{:.1}\"",
        b.page, b.bbox[0], b.bbox[1], b.bbox[2], b.bbox[3]
    )
}

fn render_block_html(b: &Block, doc: &Document, opts: &SerializeOptions, out: &mut String) {
    let prov = prov_attrs(b, opts);
    match &b.kind {
        BlockKind::Title { text } => {
            out.push_str(&format!("<h1{prov}>{}</h1>\n", inline_html(text)));
        }
        BlockKind::Heading { level, text } => {
            let lvl = (*level).clamp(1, 6);
            out.push_str(&format!("<h{lvl}{prov}>{}</h{lvl}>\n", inline_html(text)));
        }
        BlockKind::Paragraph { text } | BlockKind::Text { text } => {
            out.push_str(&format!("<p{prov}>{}</p>\n", inline_html(text)));
        }
        BlockKind::List { ordered, items } => {
            let tag = if *ordered { "ol" } else { "ul" };
            out.push_str(&format!("<{tag}{prov}>\n"));
            for it in items {
                out.push_str(&format!("<li>{}</li>\n", inline_html(&it.text)));
            }
            out.push_str(&format!("</{tag}>\n"));
        }
        BlockKind::Figure { alt, image, caption } => {
            out.push_str(&format!("<figure{prov}>\n"));
            let src = image.as_ref().and_then(|im| im.path.clone());
            let alt_s = alt.as_deref().unwrap_or("Figure");
            out.push_str(&format!(
                "<img src=\"{}\" alt=\"{}\">\n",
                html_escape(src.as_deref().unwrap_or("")),
                html_escape(alt_s)
            ));
            if let Some(cid) = caption {
                if let Some(cap) = doc.block(*cid) {
                    if let BlockKind::Caption { text, .. } = &cap.kind {
                        out.push_str(&format!(
                            "<figcaption>{}</figcaption>\n",
                            inline_html(text)
                        ));
                    }
                }
            }
            out.push_str("</figure>\n");
        }
        BlockKind::Caption { text, target } => {
            let emitted_under_target = target
                .and_then(|t| doc.block(t))
                .map(|t| matches!(t.kind, BlockKind::Figure { .. } | BlockKind::Table { .. }))
                .unwrap_or(false);
            if !emitted_under_target {
                out.push_str(&format!(
                    "<p class=\"caption\"{prov}>{}</p>\n",
                    inline_html(text)
                ));
            }
        }
        BlockKind::Table { table, .. } => {
            // Reuse DI1's span/header-aware HTML table serialization. Wrap with a
            // provenance-bearing <div> only when provenance is requested, so the
            // default output is unchanged.
            if prov.is_empty() {
                out.push_str(&table.to_html());
            } else {
                out.push_str(&format!("<div{prov}>\n"));
                out.push_str(&table.to_html());
                out.push_str("</div>\n");
            }
        }
        BlockKind::Header { text } => {
            out.push_str(&format!("<!-- header: {} -->\n", inline_html(text)));
        }
        BlockKind::Footer { text } => {
            out.push_str(&format!("<!-- footer: {} -->\n", inline_html(text)));
        }
        BlockKind::PageNumber { text } => {
            out.push_str(&format!("<!-- page-number: {} -->\n", inline_html(text)));
        }
    }
}

/// Render [`InlineText`] to HTML with `<strong>`/`<em>`/`<a>` wrappers.
fn inline_html(text: &InlineText) -> String {
    let mut out = String::new();
    for span in &text.spans {
        if span.text.is_empty() {
            continue;
        }
        let mut body = html_escape(&span.text);
        if span.italic {
            body = format!("<em>{body}</em>");
        }
        if span.bold {
            body = format!("<strong>{body}</strong>");
        }
        if let Some(href) = &span.link {
            body = format!("<a href=\"{}\">{body}</a>", html_escape(href));
        }
        out.push_str(&body);
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
