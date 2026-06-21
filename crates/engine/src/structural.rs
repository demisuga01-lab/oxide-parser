//! **Structural-write operations** — the qpdf-class document-mutating ops that
//! emit a new PDF: rotate-write, optimize, repair (and the encrypt driver lives
//! alongside in [`crate::crypto`] + the writer). These build on the
//! content-preserving [`crate::writer::rewrite_document`] base, which copies the
//! whole object graph (preserving forms / outlines / annotations / structure
//! tree) while letting an op mutate specific objects.
//!
//! Linearization (web-optimized output) is intentionally NOT implemented here:
//! it requires object-stream + cross-reference-stream + precise-offset two-xref
//! writer support the current classic-xref writer does not have. See the
//! module-level note in [`linearize`].

use std::collections::HashMap;

use crate::crypto::{build_encryption, EncryptParams};
use crate::engine::ContentEngine;
use crate::error::{OxideError, Result};
use crate::object::PdfObject;
use crate::writer::{rewrite_document, rewrite_document_objects, PdfWriter};

/// Normalize a rotation angle to one of {0, 90, 180, 270} (degrees clockwise).
/// Mirrors the read-side `normalize_rotate` so a written /Rotate round-trips to
/// the same effective value. Non-multiples of 90 snap to 0 (matching the reader,
/// which warns and falls back to 0).
fn normalize_rotate(value: i32) -> i32 {
    let v = value.rem_euclid(360);
    match v {
        0 | 90 | 180 | 270 => v,
        _ => 0,
    }
}

/// How to apply a rotation to a page: an absolute angle, or a delta added to the
/// page's current effective rotation.
#[derive(Debug, Clone, Copy)]
pub enum Rotation {
    /// Set the page's rotation to exactly this angle (normalized to a multiple
    /// of 90).
    Absolute(i32),
    /// Add this many degrees to the page's current effective rotation
    /// (e.g. `+90` turns a portrait page to landscape).
    Relative(i32),
}

/// Set `/Rotate` on the given 1-based `pages` (empty = all pages) and write a new
/// PDF. The rotation is applied to the **leaf** page objects and persisted; the
/// whole document is otherwise preserved (forms, outlines, annotations).
///
/// The read side resolves inherited `/Rotate`, so [`ContentEngine::get_page`]
/// already reports each page's *effective* current angle — relative rotation
/// offsets from that, and absolute replaces it. Because the writer flattens
/// every page to a leaf with its own `/Rotate`, setting it on the leaf is
/// correct regardless of where the source inherited it from.
pub fn rotate_pages(
    engine: &ContentEngine,
    pages: &[usize],
    rotation: Rotation,
) -> Result<Vec<u8>> {
    let total = engine.page_count()?;
    let targets: Vec<usize> = if pages.is_empty() {
        (1..=total).collect()
    } else {
        let mut p: Vec<usize> = pages.iter().copied().filter(|&n| n >= 1 && n <= total).collect();
        p.sort_unstable();
        p.dedup();
        p
    };
    if targets.is_empty() {
        return Err(OxideError::MalformedPdf(
            "rotate: no valid pages selected".to_string(),
        ));
    }

    // Map ORIGINAL page object-number -> desired /Rotate. The mutate hook keys
    // off the original number, which is what rewrite_document passes.
    let mut wanted: HashMap<u32, i32> = HashMap::new();
    for &page_no in &targets {
        let page = engine.get_page(page_no)?;
        let current = page.rotate.rem_euclid(360);
        let new_angle = match rotation {
            Rotation::Absolute(a) => normalize_rotate(a),
            Rotation::Relative(d) => normalize_rotate(current + d),
        };
        wanted.insert(page.object_number, new_angle);
    }

    rewrite_document(engine.document().reader(), |orig_num, obj| {
        let Some(&angle) = wanted.get(&orig_num) else {
            return;
        };
        if let PdfObject::Dictionary(dict) = obj {
            // Only touch actual page objects (defensive: an object number could
            // in theory be reused, though the writer's identity copy preserves it).
            if dict.get_name("Type") == Some("Page") {
                if angle == 0 {
                    // Normalize to "no /Rotate" rather than writing /Rotate 0.
                    // The writer has no remove, so write 0 explicitly only if the
                    // source had one; simplest correct behavior: always set it,
                    // which is spec-valid (0 is the default and harmless).
                    dict.insert("Rotate", PdfObject::Integer(0));
                } else {
                    dict.insert("Rotate", PdfObject::Integer(angle as i64));
                }
            }
        }
    })
}

/// Result of an [`optimize`] pass, for reporting.
#[derive(Debug, Clone, Default)]
pub struct OptimizeReport {
    /// Output size in bytes.
    pub output_bytes: usize,
    /// Number of streams recompressed with FlateDecode.
    pub streams_recompressed: usize,
}

/// Produce a smaller, cleaner PDF WITHOUT changing visible content.
///
/// Techniques applied (the safe, high-value ones):
/// - **Garbage collection**: objects unreachable from the document root are
///   dropped — the content-preserving rewrite enumerates only live objects, so
///   dead objects (and any old `/Type /XRef` streams) never make the copy.
/// - **Stream recompression**: uncompressed content streams (no `/Filter`) are
///   re-encoded with `FlateDecode` when that is smaller. Image-codec streams
///   (`DCTDecode`/`JPXDecode`/`CCITTFax`/`JBIG2`) and already-filtered streams
///   are left UNTOUCHED, so image fidelity is preserved and nothing lossy
///   happens.
///
/// Object-stream / cross-reference-stream packing (the largest size win for
/// object-heavy PDFs) is NOT yet implemented — it needs writer support that
/// does not exist (see [`linearize`]); recorded as future work.
///
/// Visually safe by construction: only the stream *container* compression
/// changes, never decoded content. The 0B render-equivalence harness
/// (`renderer-benchmark/scripts/run_0b_compression_safety.py`) is the
/// belt-and-braces check.
pub fn optimize(engine: &ContentEngine) -> Result<(Vec<u8>, OptimizeReport)> {
    let reader = engine.document().reader();
    let mut report = OptimizeReport::default();

    // Recompress uncompressed streams in place during the content-preserving
    // rewrite. We can only safely recompress a stream we can re-encode such that
    // the declared /Filter still decodes it: the unambiguous case is a stream
    // with NO existing filter — wrap it in FlateDecode. Filtered/streams that
    // decode only partially (image codecs) are left verbatim.
    let mut recompressed = 0usize;
    let result = rewrite_document(reader, |_orig, obj| {
        if let PdfObject::Stream { dict, raw } = obj {
            let has_filter = dict.contains_key("Filter");
            // Never touch streams that already carry a filter (incl. image
            // codecs) — re-deriving their decode chain is out of scope here.
            if has_filter || raw.is_empty() {
                return;
            }
            let compressed = crate::filters::flate_encode(raw, 9);
            // Only adopt it if it actually shrinks the stream (plus the small
            // /Filter entry overhead).
            if compressed.len() + 16 < raw.len() {
                *raw = compressed;
                dict.insert("Filter", PdfObject::Name("FlateDecode".to_string()));
                recompressed += 1;
            }
        }
    })?;
    report.streams_recompressed = recompressed;
    report.output_bytes = result.len();
    Ok((result, report))
}

/// Encrypt the document with the standard security handler and write a new PDF.
///
/// Reuses the read side's key-derivation primitives in the write direction
/// ([`build_encryption`]) and the writer's per-object encryption. The whole
/// document is preserved (forms, outlines, annotations) via the content-
/// preserving rewrite path. AES-256 (R6) is the secure default.
///
/// The output is encrypted with a fresh random file `/ID` (legacy RC4/AES-128
/// key derivation mixes the `/ID` into the key; AES-256 ignores it). The
/// encrypted bytes are NOT deterministic across runs (random IVs/salts/file
/// key) — that is correct; the DECRYPTED content is deterministic.
pub fn encrypt(engine: &ContentEngine, params: &EncryptParams) -> Result<Vec<u8>> {
    let reader = engine.document().reader();

    // The legacy file-key derivation mixes in the file /ID, which must equal the
    // /ID written to the trailer. Generate one fresh and use it consistently.
    let file_id = crate::crypto::random_bytes(16);
    let state = build_encryption(params, &file_id)?;

    // Collect the full (content-preserving) object set, unmutated.
    let mut noop = |_n: u32, _o: &mut PdfObject| {};
    let (objects, new_root, info_number) = rewrite_document_objects(reader, &mut noop)?;

    let writer = PdfWriter::new(objects, new_root)
        .with_info(info_number)
        .with_id(Some(file_id))
        .with_encryption(state);
    writer.write()
}

/// Write a clean, normalized copy of a (possibly damaged) PDF.
///
/// The read side already recovers from many malformed inputs — missing `%%EOF`,
/// stale classic-xref offsets, a misplaced `xref` keyword, and bad / oversized /
/// missing stream `/Length` (it rescans for the real `endstream`). REPAIR
/// PERSISTS that recovery: it re-serializes every recovered object with a fresh,
/// well-formed classic xref + trailer + correct `/Length`s, so a strict reader
/// accepts the result.
///
/// `bytes` is the raw (damaged) PDF; `password` is the open password if the file
/// is encrypted (the repaired copy is written UNENCRYPTED — the reader decrypts
/// on open and the writer does not re-encrypt; documented).
///
/// Best-effort salvage: objects that cannot be recovered at all are dropped
/// (logged). Inputs so damaged that the cross-reference/trailer cannot be
/// located (e.g. a `startxref` pointing past EOF, or a truncated file) currently
/// fail to open and therefore cannot be repaired — a from-scratch object scan +
/// trailer synthesis is recorded as future work. The error is returned honestly
/// rather than emitting a bogus file.
pub fn repair(bytes: Vec<u8>, password: &[u8]) -> Result<Vec<u8>> {
    let reader = crate::reader::PdfReader::from_bytes_with_password(bytes, password)?;
    crate::writer::write_document_roundtrip(&reader)
}

/// Linearization (fast-web-view) is **deferred** — see the crate's structural
/// docs and `docs/manipulation.md`. The current writer emits only a classic
/// cross-reference table; a linearized file needs object-stream + cross-
/// reference-stream output and a precise two-pass offset/​hint-stream layout that
/// does not exist yet. This function returns an explicit, actionable error
/// rather than emitting a non-linearized file that would fail `qpdf
/// --show-linearization`.
pub mod linearize {
    use crate::error::{OxideError, Result};

    /// Diagnosis of what linearization needs that the writer lacks today.
    pub const LINEARIZE_DEFERRED_REASON: &str = "\
linearization (fast web view) is not yet implemented: it requires \
cross-reference-stream output, object-stream packing, and a precise two-pass \
object/offset layout with hint streams (ISO 32000 Annex F). The current writer \
emits only a classic cross-reference table. Use `optimize` for size reduction; \
linearization is tracked as future work.";

    /// Always returns an error describing the deferral; never emits a file.
    pub fn linearize() -> Result<Vec<u8>> {
        Err(OxideError::UnsupportedFeature(
            LINEARIZE_DEFERRED_REASON.to_string(),
        ))
    }
}
