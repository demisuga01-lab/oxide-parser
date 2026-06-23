//! **Semantic chunking for RAG** — splitting the canonical document into
//! retrieval-sized passages that respect structure.
//!
//! A RAG/embedding pipeline does not ingest a whole document; it ingests
//! *chunks* that get embedded and retrieved. Chunk quality largely determines
//! retrieval quality. Naive splitting (every N characters) cuts sentences,
//! tables, and sections in half. This chunker uses the canonical
//! [`Document`](crate::parse::Document) blocks to split at **meaningful
//! boundaries** and keep related content together:
//!
//! - splits prefer heading/section boundaries and never break mid-table,
//!   mid-list-item, or (for an oversized paragraph) mid-sentence;
//! - chunks target a configurable size in **tokens** (not characters);
//! - configurable **overlap** carries trailing context across boundaries;
//! - the active **heading hierarchy** is prepended to each chunk so a retrieved
//!   passage carries its context (a known retrieval-quality win);
//! - tables and figures become their own chunks with section + caption context;
//! - each chunk carries **RAG metadata** (pages, section path, block kinds,
//!   index, token count) for filtering and citation.
//!
//! It consumes the canonical model, so it works identically on digital-born and
//! OCR'd documents. Deterministic: same document + options → identical chunks.
//!
//! # Token counting (pure-Rust, documented approximation)
//!
//! Embedding models are token-limited, so chunk sizes target *tokens*. A faithful
//! BPE tokenizer (e.g. tiktoken `cl100k_base`) needs a multi-megabyte merge
//! table and is not pure-Rust-without-deps. Instead [`estimate_tokens`] uses a
//! calibrated structural heuristic that tracks real BPE closely on typical
//! English prose and code (see its docs for the model and measured error).

use serde::{Deserialize, Serialize};

use crate::parse::{Block, BlockKind, Document};

mod tokens;
pub use tokens::estimate_tokens;

// ════════════════════════════════════════════════════════════════════════════
// Options
// ════════════════════════════════════════════════════════════════════════════

/// How to split a document into chunks. Defaults are sensible RAG settings
/// (~512-token chunks, 64-token overlap, heading context on).
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkOptions {
    /// Target chunk size in (estimated) tokens. A chunk is closed when adding the
    /// next block would exceed this, at a block boundary. Default 512.
    pub target_tokens: usize,
    /// Overlap in tokens carried from the end of one chunk into the start of the
    /// next, so context is not lost at boundaries. Default 64. `0` disables.
    pub overlap_tokens: usize,
    /// Prepend the active heading hierarchy (e.g. `# A > ## B`) to each chunk.
    /// Default `true` — a strong retrieval-quality lever.
    pub heading_context: bool,
    /// Start a new chunk at every heading boundary (a heading begins a logical
    /// section). Default `true`.
    pub split_on_headings: bool,
    /// Include page furniture (headers/footers/page numbers) in chunk text.
    /// Default `false` (furniture is noise for RAG).
    pub include_furniture: bool,
    /// A table or figure becomes its own chunk (with caption + section context)
    /// rather than being packed with surrounding prose. Default `true`.
    pub isolate_tables: bool,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        ChunkOptions {
            target_tokens: 512,
            overlap_tokens: 64,
            heading_context: true,
            split_on_headings: true,
            include_furniture: false,
            isolate_tables: true,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Chunk + metadata
// ════════════════════════════════════════════════════════════════════════════

/// One retrieval-sized passage plus the metadata a RAG store keeps alongside its
/// embedding (for filtering + citation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// 0-based position in the document's chunk stream.
    pub index: usize,
    /// The chunk's clean Markdown text (with the heading-context prefix, when
    /// enabled).
    pub text: String,
    /// Estimated token count of [`Chunk::text`] (see [`estimate_tokens`]).
    pub tokens: usize,
    /// The pages this chunk's content spans (1-based, ascending, deduped).
    pub pages: Vec<u32>,
    /// The active heading hierarchy at this chunk, outermost first
    /// (e.g. `["Introduction", "Background"]`). Empty at the document root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub section_path: Vec<String>,
    /// The block kinds packed into this chunk (e.g. `["heading","paragraph"]`),
    /// for type-aware filtering.
    pub block_kinds: Vec<String>,
    /// Bounding boxes of the source blocks `[x0,y0,x1,y1]` (parallel to the
    /// blocks packed), for fine-grained citation. Empty entries omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bboxes: Vec<[f64; 4]>,
    /// `true` when this chunk is a single table/figure isolated as its own chunk.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_table_or_figure: bool,
    /// `true` when the chunk exceeds the token target and could not be split
    /// further (an oversized table, or a single sentence longer than the target).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub oversized: bool,
}

/// Schema version of the serialized chunk array.
pub const CHUNK_SCHEMA_VERSION: &str = "1.0";

/// The serializable result of chunking — a versioned envelope around the chunk
/// array (so the JSON is self-describing for embedding pipelines).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkSet {
    pub schema_version: String,
    /// Source document title, when known (a convenient citation root).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub target_tokens: usize,
    pub overlap_tokens: usize,
    pub chunks: Vec<Chunk>,
}

impl ChunkSet {
    /// Serialize the chunk set to pretty JSON (the embedding-pipeline output).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// The chunker
// ════════════════════════════════════════════════════════════════════════════

/// One block's contribution to chunking: its rendered Markdown text, token cost,
/// and the metadata needed to assemble a chunk.
struct Piece {
    text: String,
    tokens: usize,
    page: u32,
    bbox: [f64; 4],
    kind: &'static str,
    /// `true` for tables/figures (kept whole; isolated when configured).
    atomic: bool,
    /// Section nesting level when this piece is a heading: `0` for a Title,
    /// `1..` for `Heading{level}`; `None` for non-heading pieces.
    heading_level: Option<u8>,
}

/// Chunk a document into RAG-ready passages.
pub fn chunk(doc: &Document, opts: &ChunkOptions) -> ChunkSet {
    let target = opts.target_tokens.max(1);
    let pieces = collect_pieces(doc, opts);

    // Heading-context tracking: a stack of (level, text). A Title is level 0.
    let mut heading_stack: Vec<(u8, String)> = Vec::new();

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut cur: Vec<&Piece> = Vec::new();
    let mut cur_section: Vec<String> = Vec::new();

    // Helper to flush the current accumulation into a chunk.
    macro_rules! flush {
        () => {
            if !cur.is_empty() {
                let section = cur_section.clone();
                push_chunk(&mut chunks, &cur, &section, opts, false);
                // Seed overlap from the tail of the just-closed chunk.
                let carry = overlap_pieces(&cur, opts.overlap_tokens);
                cur = carry;
            }
        };
    }

    for piece in &pieces {
        // Maintain the heading stack + section path on heading/title pieces.
        if let Some(level) = piece.heading_level {
            if opts.split_on_headings {
                flush!();
                // A heading that starts a new section should not inherit overlap
                // prose from the previous section.
                cur.clear();
            }
            // Pop deeper-or-equal headings, then push this one.
            heading_stack.retain(|(l, _)| *l < level);
            heading_stack.push((level, strip_md_heading(&piece.text)));
            cur_section = heading_stack.iter().map(|(_, t)| t.clone()).collect();
        }

        // Atomic block (table/figure) isolated as its own chunk.
        if piece.atomic && opts.isolate_tables {
            flush!();
            cur.clear();
            push_chunk(&mut chunks, &[piece], &cur_section, opts, true);
            continue;
        }

        let cur_tokens: usize = cur.iter().map(|p| p.tokens).sum();

        // A single piece larger than the target: split it (sentences) if text,
        // else emit it whole as an oversized chunk.
        if piece.tokens > target && !piece.atomic {
            flush!();
            cur.clear();
            for sub in split_oversized(piece, target) {
                push_owned_chunk(&mut chunks, sub, &cur_section, opts);
            }
            continue;
        }

        // Packing: if adding this piece would exceed the target, close first.
        if cur_tokens + piece.tokens > target && !cur.is_empty() {
            flush!();
        }
        cur.push(piece);
    }
    // Final flush (no overlap carry needed after the last chunk).
    if !cur.is_empty() {
        push_chunk(&mut chunks, &cur, &cur_section, opts, false);
    }

    // Re-index (overlap seeding can leave a trailing carry-only chunk identical
    // to nothing; push_chunk already skips empties). Assign final indices.
    for (i, c) in chunks.iter_mut().enumerate() {
        c.index = i;
    }

    ChunkSet {
        schema_version: CHUNK_SCHEMA_VERSION.to_string(),
        title: doc.metadata.title.clone(),
        target_tokens: opts.target_tokens,
        overlap_tokens: opts.overlap_tokens,
        chunks,
    }
}

/// Flatten the document body into rendered pieces, honoring furniture handling.
fn collect_pieces(doc: &Document, opts: &ChunkOptions) -> Vec<Piece> {
    let mut pieces = Vec::new();
    for b in &doc.body {
        if b.kind.is_furniture() && !opts.include_furniture {
            continue;
        }
        let Some((text, kind, atomic)) = render_piece(b, doc) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let heading_level = match &b.kind {
            BlockKind::Title { .. } => Some(0),
            BlockKind::Heading { level, .. } => Some((*level).max(1)),
            _ => None,
        };
        let tokens = estimate_tokens(&text);
        pieces.push(Piece {
            text,
            tokens,
            page: b.page,
            bbox: b.bbox,
            kind,
            atomic,
            heading_level,
        });
    }
    pieces
}

/// Render a single block to its chunk text + kind tag + atomicity. Returns
/// `None` for blocks that contribute nothing (e.g. a caption already attached to
/// its figure/table).
fn render_piece(b: &Block, doc: &Document) -> Option<(String, &'static str, bool)> {
    use crate::parse::serialize_block_markdown;
    match &b.kind {
        BlockKind::Caption { target, .. } => {
            // Skip captions that are emitted under their figure/table.
            let attached = target
                .and_then(|t| doc.block(t))
                .map(|t| matches!(t.kind, BlockKind::Figure { .. } | BlockKind::Table { .. }))
                .unwrap_or(false);
            if attached {
                return None;
            }
            Some((serialize_block_markdown(b, doc), "caption", false))
        }
        BlockKind::Table { .. } => Some((serialize_block_markdown(b, doc), "table", true)),
        BlockKind::Figure { .. } => Some((serialize_block_markdown(b, doc), "figure", true)),
        BlockKind::Title { .. } => Some((serialize_block_markdown(b, doc), "title", false)),
        BlockKind::Heading { .. } => Some((serialize_block_markdown(b, doc), "heading", false)),
        BlockKind::Paragraph { .. } => Some((serialize_block_markdown(b, doc), "paragraph", false)),
        BlockKind::Text { .. } => Some((serialize_block_markdown(b, doc), "text", false)),
        BlockKind::List { .. } => Some((serialize_block_markdown(b, doc), "list", false)),
        BlockKind::Header { .. } => Some((serialize_block_markdown(b, doc), "header", false)),
        BlockKind::Footer { .. } => Some((serialize_block_markdown(b, doc), "footer", false)),
        BlockKind::PageNumber { .. } => {
            Some((serialize_block_markdown(b, doc), "page_number", false))
        }
    }
}

/// Strip leading Markdown `#`s (and one space) from a heading line to get its
/// plain text for the section path.
fn strip_md_heading(s: &str) -> String {
    s.trim_start_matches('#').trim().to_string()
}

/// Build a chunk from borrowed pieces and append it (skipping empties).
fn push_chunk(
    chunks: &mut Vec<Chunk>,
    pieces: &[&Piece],
    section: &[String],
    opts: &ChunkOptions,
    is_table_or_figure: bool,
) {
    if pieces.is_empty() {
        return;
    }
    let body = pieces
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.trim().is_empty() {
        return;
    }
    // Heading-context prefix gives a chunk its *ancestor* context. If the chunk
    // already begins with its own deepest heading (the body opens with it), drop
    // that last path entry so the heading is not printed twice.
    let prefix = if opts.heading_context && !section.is_empty() {
        let starts_with_own_heading = pieces
            .first()
            .map(|p| {
                p.heading_level.is_some() && strip_md_heading(&p.text) == *section.last().unwrap()
            })
            .unwrap_or(false);
        let ctx = if starts_with_own_heading {
            &section[..section.len() - 1]
        } else {
            section
        };
        if ctx.is_empty() {
            String::new()
        } else {
            format!("{}\n\n", heading_prefix(ctx))
        }
    } else {
        String::new()
    };
    let text = format!("{prefix}{body}");
    let tokens = estimate_tokens(&text);
    let mut pages: Vec<u32> = pieces.iter().map(|p| p.page).filter(|p| *p != 0).collect();
    pages.sort_unstable();
    pages.dedup();
    let block_kinds: Vec<String> = pieces.iter().map(|p| p.kind.to_string()).collect();
    let bboxes: Vec<[f64; 4]> = pieces
        .iter()
        .map(|p| p.bbox)
        .filter(|b| *b != [0.0; 4])
        .collect();

    chunks.push(Chunk {
        index: 0, // assigned at the end
        text,
        tokens,
        pages,
        section_path: section.to_vec(),
        block_kinds,
        bboxes,
        is_table_or_figure,
        oversized: tokens > opts.target_tokens,
    });
}

/// Append a pre-rendered owned chunk (from oversized-piece splitting).
fn push_owned_chunk(
    chunks: &mut Vec<Chunk>,
    piece: Piece,
    section: &[String],
    opts: &ChunkOptions,
) {
    let prefix = if opts.heading_context && !section.is_empty() {
        format!("{}\n\n", heading_prefix(section))
    } else {
        String::new()
    };
    let text = format!("{prefix}{}", piece.text);
    let tokens = estimate_tokens(&text);
    chunks.push(Chunk {
        index: 0,
        text,
        tokens,
        pages: if piece.page != 0 {
            vec![piece.page]
        } else {
            vec![]
        },
        section_path: section.to_vec(),
        block_kinds: vec![piece.kind.to_string()],
        bboxes: if piece.bbox != [0.0; 4] {
            vec![piece.bbox]
        } else {
            vec![]
        },
        is_table_or_figure: piece.atomic,
        oversized: tokens > opts.target_tokens,
    });
}

/// The heading-context prefix line: `# A > ## B > ### C`.
fn heading_prefix(section: &[String]) -> String {
    section
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}", "#".repeat((i + 1).min(6)), t))
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Choose the trailing pieces of a closed chunk to carry into the next as
/// overlap, up to `overlap_tokens`. Returns owned references re-borrowed by the
/// caller. We carry whole pieces from the end (never split a block for overlap).
fn overlap_pieces<'a>(cur: &[&'a Piece], overlap_tokens: usize) -> Vec<&'a Piece> {
    if overlap_tokens == 0 {
        return Vec::new();
    }
    let mut carry: Vec<&Piece> = Vec::new();
    let mut total = 0usize;
    for p in cur.iter().rev() {
        // Never carry an atomic table/figure as overlap.
        if p.atomic {
            break;
        }
        if total + p.tokens > overlap_tokens && !carry.is_empty() {
            break;
        }
        carry.push(p);
        total += p.tokens;
        if total >= overlap_tokens {
            break;
        }
    }
    carry.reverse();
    carry
}

/// Split a single oversized text piece into target-sized sub-pieces at sentence
/// boundaries (never mid-sentence). A single sentence longer than the target is
/// emitted whole and flagged oversized.
fn split_oversized(piece: &Piece, target: usize) -> Vec<Piece> {
    let sentences = split_sentences(&piece.text);
    let mut out: Vec<Piece> = Vec::new();
    let mut buf = String::new();
    let mut buf_tokens = 0usize;
    for s in sentences {
        let st = estimate_tokens(&s);
        if buf_tokens + st > target && !buf.is_empty() {
            out.push(mk_sub(piece, std::mem::take(&mut buf)));
            buf_tokens = 0;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(s.trim());
        buf_tokens += st;
    }
    if !buf.is_empty() {
        out.push(mk_sub(piece, buf));
    }
    if out.is_empty() {
        out.push(mk_sub(piece, piece.text.clone()));
    }
    out
}

fn mk_sub(parent: &Piece, text: String) -> Piece {
    let tokens = estimate_tokens(&text);
    Piece {
        text,
        tokens,
        page: parent.page,
        bbox: parent.bbox,
        kind: parent.kind,
        atomic: false,
        heading_level: None,
    }
}

/// Split text into sentences at `.`/`!`/`?` followed by whitespace. Conservative
/// — keeps the terminator with its sentence. Good enough for chunk packing
/// (we only need not-mid-sentence boundaries, not linguistic perfection).
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') {
            let next_ws = chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(true);
            if next_ws {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

impl Document {
    /// Split this document into RAG-ready [`Chunk`]s. See [`crate::chunk`].
    pub fn chunk(&self, opts: &ChunkOptions) -> ChunkSet {
        chunk(self, opts)
    }
}

#[cfg(test)]
mod tests;
