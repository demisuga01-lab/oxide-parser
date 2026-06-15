//! Embedded file attachments (`pdfdetach`-equivalent).
//!
//! PDFs can carry arbitrary embedded files (spreadsheets, XML e-invoices like
//! ZUGFeRD/Factur-X, other PDFs, …). They live in two places:
//!
//! 1. The catalog's `/Names /EmbeddedFiles` **name tree** — a balanced tree of
//!    `/Kids`/`/Names` nodes mapping name strings to file-specification
//!    dictionaries.
//! 2. Page `/Annots` of `/Subtype /FileAttachment` — "paperclip" attachments
//!    whose `/FS` is a file specification.
//!
//! This module locates embedded files from both sources, dedupes by the
//! embedded-file stream's object id, and extracts their (filter-decoded)
//! contents. Extraction sanitizes the attacker-controlled embedded filename so
//! it cannot escape a chosen output directory (path-traversal defence).

use std::collections::HashSet;

use serde::Serialize;

use crate::crypto::md5;
use crate::document::PdfDocument;
use crate::error::{OxideError, Result};
use crate::filters::decode_stream;
use crate::info::decode_pdf_text_string;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

/// Maximum name-tree recursion depth (intermediate `/Kids` nesting). Real name
/// trees are shallow; this bounds a malformed/cyclic tree alongside the
/// node-visited set.
const MAX_NAME_TREE_DEPTH: usize = 64;

/// One embedded file attachment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attachment {
    /// 1-based index in discovery order (name tree first, then annotations).
    pub index: usize,
    /// File name (`/UF` preferred, else `/F`), decoded from PDF text-string form.
    pub name: String,
    /// Optional `/Desc` description.
    pub description: Option<String>,
    /// Decoded (uncompressed) size in bytes, if known — from `/Params /Size`
    /// when present, else the actual decoded length once extracted.
    pub size: Option<usize>,
    /// `/Params /CreationDate` (raw `D:...` string).
    pub creation_date: Option<String>,
    /// `/Params /ModDate` (raw `D:...` string).
    pub mod_date: Option<String>,
    /// `/Params /CheckSum` (MD5 of the uncompressed data) as uppercase hex.
    pub checksum_md5: Option<String>,
    /// Object number of the embedded-file stream (the dedupe key).
    pub stream_object: u32,
    /// Generation number of the embedded-file stream.
    pub stream_generation: u16,
    /// Source: where this attachment was found.
    pub source: AttachmentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSource {
    /// Catalog `/Names /EmbeddedFiles` name tree.
    NameTree,
    /// A page `/FileAttachment` annotation.
    Annotation,
}

/// Enumerate every embedded file in the document, from both the name tree and
/// file-attachment annotations, deduped by embedded-file stream object id.
pub fn list_attachments(doc: &PdfDocument) -> Result<Vec<Attachment>> {
    let reader = doc.reader();
    let mut seen: HashSet<(u32, u16)> = HashSet::new();
    let mut out: Vec<Attachment> = Vec::new();

    // 1. Catalog /Names /EmbeddedFiles name tree.
    if let Ok(catalog) = doc.get_catalog() {
        if let Some(names) = resolve_dict(catalog.get("Names"), reader) {
            if let Some(embedded_files) = names.get("EmbeddedFiles") {
                let resolved = reader
                    .resolve(embedded_files.clone())
                    .unwrap_or(PdfObject::Null);
                if let PdfObject::Dictionary(root) = resolved {
                    let mut pairs = Vec::new();
                    let mut visited = HashSet::new();
                    walk_name_tree(&root, reader, &mut pairs, &mut visited, 0);
                    for (_key, value) in pairs {
                        if let Some(att) = build_attachment(
                            &value,
                            reader,
                            &mut seen,
                            out.len() + 1,
                            AttachmentSource::NameTree,
                        ) {
                            out.push(att);
                        }
                    }
                }
            }
        }
    }

    // 2. Page /FileAttachment annotations.
    if let Ok(pages) = doc.get_pages() {
        for page in &pages {
            let Ok(PdfObject::Dictionary(page_dict)) =
                reader.get_and_resolve(page.object_number, page.generation_number)
            else {
                continue;
            };
            let annots = match page_dict.get("Annots") {
                Some(obj) => match reader.resolve(obj.clone()) {
                    Ok(PdfObject::Array(items)) => items,
                    _ => continue,
                },
                None => continue,
            };
            for annot in &annots {
                let Ok(PdfObject::Dictionary(annot_dict)) = reader.resolve(annot.clone()) else {
                    continue;
                };
                if annot_dict.get_name("Subtype") != Some("FileAttachment") {
                    continue;
                }
                let Some(fs) = annot_dict.get("FS") else {
                    continue;
                };
                if let Some(att) = build_attachment(
                    fs,
                    reader,
                    &mut seen,
                    out.len() + 1,
                    AttachmentSource::Annotation,
                ) {
                    out.push(att);
                }
            }
        }
    }

    Ok(out)
}

/// Resolve a file-specification object into an [`Attachment`], deduping by the
/// embedded-file stream object id. Returns `None` if the filespec has no
/// embedded-file stream or it was already seen.
fn build_attachment(
    filespec_obj: &PdfObject,
    reader: &PdfReader,
    seen: &mut HashSet<(u32, u16)>,
    index: usize,
    source: AttachmentSource,
) -> Option<Attachment> {
    let filespec = match reader.resolve(filespec_obj.clone()).ok()? {
        PdfObject::Dictionary(dict) => dict,
        _ => return None,
    };

    // /EF embedded-file dictionary → /F (or /UF) reference to the stream.
    let ef = resolve_dict(filespec.get("EF"), reader)?;
    // Prefer /F, fall back to /UF (some producers store the stream under /UF).
    let stream_ref = ef
        .get("F")
        .and_then(PdfObject::as_reference)
        .or_else(|| ef.get("UF").and_then(PdfObject::as_reference))?;

    if !seen.insert(stream_ref) {
        return None; // already collected this embedded-file stream
    }

    // File name: /UF (Unicode) preferred, else /F.
    let name = filespec
        .get("UF")
        .and_then(PdfObject::as_string)
        .or_else(|| filespec.get("F").and_then(PdfObject::as_string))
        .map(decode_pdf_text_string)
        .unwrap_or_else(|| format!("attachment-{index}"));

    let description = filespec
        .get("Desc")
        .and_then(PdfObject::as_string)
        .map(decode_pdf_text_string)
        .filter(|s| !s.is_empty());

    // /Params metadata from the embedded-file stream dictionary.
    let (size, creation_date, mod_date, checksum_md5) =
        match reader.get_and_resolve(stream_ref.0, stream_ref.1) {
            Ok(PdfObject::Stream { dict, .. }) => {
                let params = resolve_dict(dict.get("Params"), reader);
                let size = params
                    .as_ref()
                    .and_then(|p| p.get_integer("Size"))
                    .filter(|s| *s >= 0)
                    .map(|s| s as usize);
                let creation = params
                    .as_ref()
                    .and_then(|p| p.get("CreationDate"))
                    .and_then(PdfObject::as_string)
                    .map(decode_pdf_text_string);
                let modified = params
                    .as_ref()
                    .and_then(|p| p.get("ModDate"))
                    .and_then(PdfObject::as_string)
                    .map(decode_pdf_text_string);
                let checksum = params
                    .as_ref()
                    .and_then(|p| p.get("CheckSum"))
                    .and_then(PdfObject::as_string)
                    .map(to_hex_upper);
                (size, creation, modified, checksum)
            }
            _ => (None, None, None, None),
        };

    Some(Attachment {
        index,
        name,
        description,
        size,
        creation_date,
        mod_date,
        checksum_md5,
        stream_object: stream_ref.0,
        stream_generation: stream_ref.1,
        source,
    })
}

/// Extract an attachment's file content: fetch its embedded-file stream and
/// decode it through the filter pipeline. Returns the raw file bytes.
///
/// When the filespec carries a `/Params /CheckSum` (MD5 of the uncompressed
/// data), the decoded bytes are checked against it; a mismatch is logged as a
/// warning (not an error — some producers store a wrong/absent checksum).
pub fn extract_attachment(doc: &PdfDocument, attachment: &Attachment) -> Result<Vec<u8>> {
    let reader = doc.reader();
    let stream_obj =
        reader.get_and_resolve(attachment.stream_object, attachment.stream_generation)?;
    if !matches!(stream_obj, PdfObject::Stream { .. }) {
        return Err(OxideError::MalformedPdf(format!(
            "embedded file object {} {} is not a stream",
            attachment.stream_object, attachment.stream_generation
        )));
    }
    let bytes = decode_stream(&stream_obj, reader)?;

    // Verify the stored MD5 checksum if present.
    if let Some(expected_hex) = &attachment.checksum_md5 {
        let actual = to_hex_upper(&md5(&bytes));
        if !expected_hex.is_empty() && !actual.eq_ignore_ascii_case(expected_hex) {
            log::warn!(
                "attachment '{}' checksum mismatch: stored {}, computed {}",
                attachment.name,
                expected_hex,
                actual
            );
        }
    }

    Ok(bytes)
}

/// Sanitize an attacker-controlled embedded filename into a single safe path
/// component suitable for joining onto a chosen output directory.
///
/// PDF embedded filenames are arbitrary attacker-controlled strings and may
/// contain `../`, absolute paths, drive letters, or path separators. This
/// reduces the name to its final component and strips anything dangerous, so
/// the result can never escape the output directory. Falls back to a default
/// when nothing safe remains.
pub fn sanitize_filename(name: &str) -> String {
    // PDF paths use '/' as the separator (PDF 32000-1 §7.11.2); also defend
    // against backslashes (Windows) and drive/colon components.
    let last_component = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name);

    let mut cleaned: String = last_component
        .chars()
        .filter(|c| {
            // Drop control chars and path/separator-significant characters.
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '\0')
        })
        .collect();

    // Trim leading dots/spaces and reject pure-dot names ("." / "..").
    cleaned = cleaned.trim_matches(|c: char| c == ' ').to_string();
    let trimmed_dots = cleaned.trim_matches('.');
    if trimmed_dots.is_empty() {
        return "attachment.bin".to_string();
    }

    if cleaned.is_empty() {
        "attachment.bin".to_string()
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------------------
// Generic name-tree walker
// ---------------------------------------------------------------------------

/// Walk a PDF **name tree** rooted at `node`, appending every
/// `(key_string, value_object)` leaf to `out`.
///
/// A name-tree node has either `/Kids` (intermediate nodes — recurse) or
/// `/Names` (an array alternating `[key1 value1 key2 value2 …]`). This walker
/// is generic and reusable for any name tree (EmbeddedFiles, Dests,
/// JavaScript, …). It is cycle-safe (a visited-set keyed on intermediate node
/// object ids) and depth-bounded.
pub fn walk_name_tree(
    node: &PdfDictionary,
    reader: &PdfReader,
    out: &mut Vec<(String, PdfObject)>,
    visited: &mut HashSet<(u32, u16)>,
    depth: usize,
) {
    if depth > MAX_NAME_TREE_DEPTH {
        log::warn!("name tree exceeded depth {MAX_NAME_TREE_DEPTH}; truncating");
        return;
    }

    // Leaf data: /Names array [key value key value …].
    if let Some(PdfObject::Array(names)) = node.get("Names").map(|n| {
        reader.resolve(n.clone()).unwrap_or_else(|_| n.clone())
    }) {
        let mut i = 0;
        while i + 1 < names.len() {
            let key = match &names[i] {
                PdfObject::String(bytes) => decode_pdf_text_string(bytes),
                other => match reader.resolve(other.clone()) {
                    Ok(PdfObject::String(bytes)) => decode_pdf_text_string(&bytes),
                    _ => String::new(),
                },
            };
            out.push((key, names[i + 1].clone()));
            i += 2;
        }
    }

    // Intermediate node: /Kids array of child node references.
    if let Some(kids_obj) = node.get("Kids") {
        let kids = match reader.resolve(kids_obj.clone()) {
            Ok(PdfObject::Array(items)) => items,
            _ => return,
        };
        for kid in &kids {
            // Cycle protection: track intermediate node object ids.
            if let Some(id) = kid.as_reference() {
                if !visited.insert(id) {
                    continue;
                }
            }
            if let Ok(PdfObject::Dictionary(child)) = reader.resolve(kid.clone()) {
                walk_name_tree(&child, reader, out, visited, depth + 1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_dict(obj: Option<&PdfObject>, reader: &PdfReader) -> Option<PdfDictionary> {
    match obj? {
        PdfObject::Dictionary(dict) => Some(dict.clone()),
        r @ PdfObject::Reference { .. } => match reader.resolve(r.clone()).ok()? {
            PdfObject::Dictionary(dict) => Some(dict),
            _ => None,
        },
        _ => None,
    }
}

fn to_hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(hex_digit(b >> 4));
        s.push(hex_digit(b & 0x0F));
    }
    s
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize_filename("/absolute/path/file.txt"), "file.txt");
        assert_eq!(sanitize_filename("C:\\Users\\x\\secret.dat"), "secret.dat");
        assert_eq!(sanitize_filename("normal.pdf"), "normal.pdf");
    }

    #[test]
    fn sanitize_rejects_dot_names() {
        assert_eq!(sanitize_filename(".."), "attachment.bin");
        assert_eq!(sanitize_filename("."), "attachment.bin");
        assert_eq!(sanitize_filename("../.."), "attachment.bin");
        assert_eq!(sanitize_filename(""), "attachment.bin");
    }

    #[test]
    fn sanitize_drops_control_and_separators() {
        assert_eq!(sanitize_filename("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_filename("inv\u{0000}oice.xml"), "invoice.xml");
        // A name that is only separators/dots collapses to the default.
        assert_eq!(sanitize_filename("///"), "attachment.bin");
    }

    // The name-tree walker takes a &PdfReader (to resolve indirect /Kids and
    // /Names entries), so it is exercised end-to-end against real fixtures in
    // tests/attachments.rs rather than in a reader-free unit test.

    #[test]
    fn hex_upper_works() {
        assert_eq!(to_hex_upper(&[0xDE, 0xAD, 0xBE, 0xEF]), "DEADBEEF");
    }
}
