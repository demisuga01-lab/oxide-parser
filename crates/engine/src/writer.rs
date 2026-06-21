//! PDF writer / serializer.
//!
//! The reader/parser side of the engine turns PDF bytes into an in-memory
//! object model ([`PdfObject`] / [`PdfDictionary`] / streams). This module does
//! the reverse: it takes a set of indirect objects and emits a syntactically
//! valid, openable PDF file (header, body, classic cross-reference table,
//! trailer, `startxref`, `%%EOF`).
//!
//! # Stream data
//!
//! Streams are copied **verbatim**: the original, still-filter-encoded `raw`
//! bytes are written unchanged together with their existing `/Filter` and
//! `/DecodeParms` entries, and `/Length` is reset to the exact number of bytes
//! emitted. The engine never re-encodes stream data when writing — that keeps
//! output faithful and small and avoids a decode/re-encode round-trip that
//! could lose information. (Re-compression of uncompressed streams is a
//! possible future optimization; correctness does not need it.)
//!
//! Note that the reader *decrypts* strings and stream bytes as objects are
//! fetched, so the bytes handed to the writer are already plaintext. Output is
//! therefore **always unencrypted** — manipulating an encrypted input decrypts
//! it. Re-encryption of output is a future enhancement.
//!
//! # Object numbering and references
//!
//! When objects are copied out of one or more source documents their object
//! numbers will collide, so the writer assigns a fresh, contiguous numbering
//! for the output (object 1, 2, 3, …) and rewrites every
//! [`PdfObject::Reference`] using a remap built during the copy. See
//! [`build_subset`] / [`build_merged`] and [`rewrite_references`].

use std::collections::{BTreeMap, HashMap};

use crate::document::PdfDocument;
use crate::error::{OxideError, Result};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

/// Maximum number of objects a single dependency-closure walk will copy.
///
/// A pathological or hostile page tree could, in principle, reference an
/// enormous object graph; this bounds the work (and output size) of a single
/// page's closure to a generous-but-finite ceiling rather than letting a
/// crafted file drive unbounded allocation.
const MAX_CLOSURE_OBJECTS: usize = 5_000_000;

/// An indirect object destined for the output file: its (already final) object
/// number plus the object body.
///
/// Generation numbers in writer output are always `0` — renumbering collapses
/// every copied object into a fresh generation-0 numbering, which is valid and
/// is what every PDF producer emits for freshly written files.
#[derive(Clone, Debug)]
pub struct OutputObject {
    pub number: u32,
    pub object: PdfObject,
}

/// Low-level serializer: turns a set of [`OutputObject`]s plus a root reference
/// (and optional `/Info`) into PDF file bytes.
///
/// Most callers want the higher-level [`build_subset`] / [`build_merged`]
/// helpers, which compute object closures and renumbering for you and then call
/// this. `PdfWriter` is public for tests and for callers that have already
/// assembled a renumbered object set themselves.
pub struct PdfWriter {
    version: String,
    objects: Vec<OutputObject>,
    root: u32,
    info: Option<u32>,
    /// First element of the file `/ID` array, when carried over from a source
    /// document. Both `/ID` elements are written equal to this value (a fresh
    /// document has no update history, so the two ids coincide).
    id: Option<Vec<u8>>,
}

impl PdfWriter {
    /// Create a writer for the given objects. `root` is the output object
    /// number of the document catalog; `info`, when `Some`, is the output
    /// object number of the document information dictionary.
    pub fn new(objects: Vec<OutputObject>, root: u32) -> Self {
        Self {
            version: "1.7".to_string(),
            objects,
            root,
            info: None,
            id: None,
        }
    }

    /// Set the output PDF header version (e.g. `"1.7"`). Defaults to `1.7`.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Set the `/Info` object number written into the trailer.
    pub fn with_info(mut self, info: Option<u32>) -> Self {
        self.info = info;
        self
    }

    /// Set the first `/ID` array element (carried from a source document).
    pub fn with_id(mut self, id: Option<Vec<u8>>) -> Self {
        self.id = id;
        self
    }

    /// Serialize the whole document to PDF bytes.
    pub fn write(&self) -> Result<Vec<u8>> {
        // Sort by object number so the body is written in ascending order and
        // the xref table (which is offset-indexed by object number) is simple
        // to build. The highest object number determines /Size.
        let mut objects: Vec<&OutputObject> = self.objects.iter().collect();
        objects.sort_by_key(|o| o.number);

        for obj in &objects {
            if obj.number == 0 {
                return Err(OxideError::MalformedPdf(
                    "writer: object number 0 is reserved for the free-list head".to_string(),
                ));
            }
        }
        // Reject duplicate object numbers — the renumbering layer must hand us a
        // bijective numbering, so a duplicate is a bug we want surfaced rather
        // than silently emitting a corrupt xref.
        for pair in objects.windows(2) {
            if pair[0].number == pair[1].number {
                return Err(OxideError::MalformedPdf(format!(
                    "writer: duplicate output object number {}",
                    pair[0].number
                )));
            }
        }

        let max_number = objects.last().map(|o| o.number).unwrap_or(0);
        let size = max_number as usize + 1;

        let mut out = Vec::new();
        // Header: version line plus a binary-marker comment so naive
        // "is this a binary file" heuristics treat the output as binary.
        out.extend_from_slice(format!("%PDF-{}\n", self.version).as_bytes());
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        // Body. Track the byte offset of each object number for the xref table.
        // A free entry maps to offset 0; in-use entries to their real offset.
        let mut offsets: Vec<Option<usize>> = vec![None; size];
        for obj in &objects {
            let offset = out.len();
            offsets[obj.number as usize] = Some(offset);
            out.extend_from_slice(format!("{} 0 obj\n", obj.number).as_bytes());
            serialize_object(&obj.object, &mut out);
            out.extend_from_slice(b"\nendobj\n");
        }

        // Cross-reference table (classic format).
        let xref_offset = out.len();
        out.extend_from_slice(b"xref\n");
        out.extend_from_slice(format!("0 {}\n", size).as_bytes());
        // Object 0 is the head of the free list.
        out.extend_from_slice(b"0000000000 65535 f \n");
        // Entries for object numbers 1..size, in order (index 0 handled above).
        for slot in &offsets[1..] {
            match slot {
                Some(offset) => {
                    // Each entry is exactly 20 bytes: 10-digit offset, space,
                    // 5-digit generation, space, 'n', and a 2-byte EOL.
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
                }
                None => {
                    // A gap in the numbering (object number with no object).
                    // Emit it as a free entry so the table stays well-formed
                    // and /Size-consistent. In practice the renumbering layer
                    // produces contiguous numbers so this is rare.
                    out.extend_from_slice(b"0000000000 65535 f \n");
                }
            }
        }

        // Trailer.
        out.extend_from_slice(b"trailer\n");
        let mut trailer = PdfDictionary::empty();
        trailer.insert("Size", PdfObject::Integer(size as i64));
        trailer.insert(
            "Root",
            PdfObject::Reference {
                number: self.root,
                generation: 0,
            },
        );
        if let Some(info) = self.info {
            trailer.insert(
                "Info",
                PdfObject::Reference {
                    number: info,
                    generation: 0,
                },
            );
        }
        if let Some(id) = &self.id {
            trailer.insert(
                "ID",
                PdfObject::Array(vec![
                    PdfObject::String(id.clone()),
                    PdfObject::String(id.clone()),
                ]),
            );
        }
        serialize_dictionary(&trailer, &mut out);
        out.extend_from_slice(b"\nstartxref\n");
        out.extend_from_slice(format!("{xref_offset}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");

        Ok(out)
    }
}

/// Serialize a single [`PdfObject`] to PDF syntax, appending to `out`.
pub fn serialize_object(object: &PdfObject, out: &mut Vec<u8>) {
    match object {
        PdfObject::Null => out.extend_from_slice(b"null"),
        PdfObject::Boolean(true) => out.extend_from_slice(b"true"),
        PdfObject::Boolean(false) => out.extend_from_slice(b"false"),
        PdfObject::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        PdfObject::Real(value) => out.extend_from_slice(format_real(*value).as_bytes()),
        PdfObject::String(bytes) => serialize_string(bytes, out),
        PdfObject::Name(name) => serialize_name(name, out),
        PdfObject::Array(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b' ');
                }
                serialize_object(item, out);
            }
            out.push(b']');
        }
        PdfObject::Dictionary(dict) => serialize_dictionary(dict, out),
        PdfObject::Reference { number, generation } => {
            out.extend_from_slice(format!("{number} {generation} R").as_bytes());
        }
        PdfObject::Stream { dict, raw } => serialize_stream(dict, raw, out),
    }
}

fn serialize_dictionary(dict: &PdfDictionary, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (key, value) in dict.iter() {
        out.push(b' ');
        serialize_name(key, out);
        out.push(b' ');
        serialize_object(value, out);
    }
    out.extend_from_slice(b" >>");
}

fn serialize_stream(dict: &PdfDictionary, raw: &[u8], out: &mut Vec<u8>) {
    // Re-set /Length to the exact byte count we are about to emit. Every other
    // dictionary entry (notably /Filter and /DecodeParms) is preserved so the
    // copied raw bytes decode correctly.
    let mut dict = dict.clone();
    dict.insert("Length", PdfObject::Integer(raw.len() as i64));
    serialize_dictionary(&dict, out);
    // The parser strips exactly one EOL after `stream` and one before
    // `endstream`, so wrap the raw bytes in `\n…\n`.
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(raw);
    out.extend_from_slice(b"\nendstream");
}

/// Serialize a PDF name (`/Foo`), hex-escaping (`#XX`) any byte that is a
/// delimiter, whitespace, the `#` escape character itself, or outside the
/// printable ASCII range, per PDF 32000-1 §7.3.5.
fn serialize_name(name: &str, out: &mut Vec<u8>) {
    out.push(b'/');
    for &byte in name.as_bytes() {
        if byte == b'#' || byte <= b' ' || byte >= 0x7F || is_delimiter(byte) {
            out.push(b'#');
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0F));
        } else {
            out.push(byte);
        }
    }
}

/// Serialize a PDF string. Chooses literal `(…)` form for mostly-printable
/// content (escaping `(`, `)`, `\`, and control bytes) and hex `<…>` form when
/// the content is heavily binary, which keeps text strings readable while
/// guaranteeing binary strings survive intact.
/// True for bytes outside the printable-ASCII range `0x20..=0x7E`.
fn is_nonprintable(byte: u8) -> bool {
    !(0x20..=0x7E).contains(&byte)
}

fn serialize_string(bytes: &[u8], out: &mut Vec<u8>) {
    let nonprintable = bytes.iter().filter(|&&b| is_nonprintable(b)).count();
    // Heuristic: if more than a quarter of the bytes are non-printable, hex is
    // both smaller and clearer. Either form round-trips losslessly.
    if !bytes.is_empty() && nonprintable * 4 > bytes.len() {
        out.push(b'<');
        for &byte in bytes {
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0F));
        }
        out.push(b'>');
        return;
    }

    out.push(b'(');
    for &byte in bytes {
        match byte {
            b'(' => out.extend_from_slice(b"\\("),
            b')' => out.extend_from_slice(b"\\)"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            // Remaining control / high bytes: octal escape, always 3 digits so
            // a following digit can't be misparsed as part of the escape.
            b if is_nonprintable(b) => {
                out.push(b'\\');
                out.push(b'0' + ((b >> 6) & 0x07));
                out.push(b'0' + ((b >> 3) & 0x07));
                out.push(b'0' + (b & 0x07));
            }
            b => out.push(b),
        }
    }
    out.push(b')');
}

/// Format a real number without a trailing exponent (PDF reals have no
/// exponent form) and without a misleading `.0` for integral values written as
/// reals. Mirrors common producer output closely enough to round-trip.
fn format_real(value: f64) -> String {
    if !value.is_finite() {
        // PDF has no representation for inf/NaN; emit 0 rather than something
        // unparseable. (The reader never produces these; this is defensive.)
        return "0".to_string();
    }
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    // Up to 6 fractional digits is ample precision for coordinates; trim
    // trailing zeros so values stay compact.
    let mut s = format!("{value:.6}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'A' + (nibble - 10),
    }
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Rewrite every [`PdfObject::Reference`] within `object` according to `remap`
/// (old object number → new object number). References whose target is not in
/// the map are replaced with `null`, because a dangling reference to an object
/// that was deliberately not copied (e.g. a /Parent pointer up an old tree, or
/// a document-level feature we drop) must not point at an unrelated object in
/// the new file.
pub fn rewrite_references(object: PdfObject, remap: &HashMap<u32, u32>) -> PdfObject {
    match object {
        PdfObject::Reference { number, .. } => match remap.get(&number) {
            Some(&new_number) => PdfObject::Reference {
                number: new_number,
                generation: 0,
            },
            None => PdfObject::Null,
        },
        PdfObject::Array(items) => PdfObject::Array(
            items
                .into_iter()
                .map(|item| rewrite_references(item, remap))
                .collect(),
        ),
        PdfObject::Dictionary(dict) => PdfObject::Dictionary(rewrite_dict(dict, remap)),
        PdfObject::Stream { dict, raw } => PdfObject::Stream {
            dict: rewrite_dict(dict, remap),
            raw,
        },
        other => other,
    }
}

fn rewrite_dict(dict: PdfDictionary, remap: &HashMap<u32, u32>) -> PdfDictionary {
    let mut out = PdfDictionary::empty();
    for (key, value) in dict.iter() {
        out.insert(key.clone(), rewrite_references(value.clone(), remap));
    }
    out
}

/// Per-document object copier and renumberer.
///
/// Tracks, for each source object number it has seen, the new (output) object
/// number assigned to it, and accumulates the copied (not-yet-reference-
/// rewritten) objects keyed by their *new* number. Deduplication is automatic:
/// requesting the same source object twice returns the same new number and
/// copies it once.
struct DocCopier<'a> {
    reader: &'a PdfReader,
    /// old source object number → new output object number.
    remap: HashMap<u32, u32>,
    /// new output object number → copied object body (references still in old
    /// numbering; rewritten in a final pass once all numbers are assigned).
    copied: BTreeMap<u32, PdfObject>,
}

impl<'a> DocCopier<'a> {
    fn new(reader: &'a PdfReader) -> Self {
        Self {
            reader,
            remap: HashMap::new(),
            copied: BTreeMap::new(),
        }
    }

    /// Allocate (or look up) the new number for a source object number, given a
    /// shared `next_number` counter that spans all documents being combined.
    fn assign(&mut self, old_number: u32, next_number: &mut u32) -> u32 {
        if let Some(&new) = self.remap.get(&old_number) {
            return new;
        }
        let new = *next_number;
        *next_number += 1;
        self.remap.insert(old_number, new);
        new
    }
}

/// Compute the transitive dependency closure of a set of root references within
/// a single source document, copy every reachable object into `copier` under a
/// fresh numbering, and return nothing (the copier holds the results).
///
/// Cycle-safe (an object already assigned a new number is not re-copied) and
/// bounded by [`MAX_CLOSURE_OBJECTS`]. Each indirect object is fetched via
/// [`PdfReader::get_object`] (which yields decrypted bytes); direct sub-objects
/// are walked in place. References that fail to resolve are left as references
/// and later rewritten to `null` if their target was never copied.
fn copy_closure(copier: &mut DocCopier, roots: &[(u32, u16)], next_number: &mut u32) -> Result<()> {
    // Work list of source object numbers to copy. We assign a new number when
    // first enqueuing so cycles terminate.
    let mut stack: Vec<u32> = Vec::new();
    for &(number, _gen) in roots {
        if !copier.remap.contains_key(&number) {
            copier.assign(number, next_number);
        }
        stack.push(number);
    }

    while let Some(old_number) = stack.pop() {
        let new_number = match copier.remap.get(&old_number) {
            Some(&n) => n,
            None => copier.assign(old_number, next_number),
        };
        // Already copied? (Assigned-but-not-yet-copied numbers are exactly the
        // ones still on the stack.)
        if copier.copied.contains_key(&new_number) {
            continue;
        }

        let object = match copier.reader.get_object(old_number, 0) {
            Ok(object) => object,
            Err(err) => {
                // A missing/broken dependency is non-fatal: record a null in
                // its place so references to it resolve to null rather than
                // dangling. This mirrors the reader's lenient page walking.
                log::warn!(
                    "writer closure: object {old_number} 0 unreadable ({err}); writing null"
                );
                copier.copied.insert(new_number, PdfObject::Null);
                continue;
            }
        };

        if copier.copied.len() >= MAX_CLOSURE_OBJECTS {
            return Err(OxideError::UnsupportedFeature(format!(
                "writer closure exceeded {MAX_CLOSURE_OBJECTS} objects"
            )));
        }

        // Discover references inside this object and enqueue any not-yet-seen
        // targets, assigning them new numbers now (so cycles terminate).
        let mut refs = Vec::new();
        collect_references(&object, &mut refs);
        for r in refs {
            if !copier.remap.contains_key(&r) {
                copier.assign(r, next_number);
                stack.push(r);
            } else if !copier.copied.contains_key(&copier.remap[&r]) {
                // Assigned but not yet copied and not on the stack any more
                // (can happen when the same object is referenced from multiple
                // places); make sure it gets processed.
                stack.push(r);
            }
        }

        copier.copied.insert(new_number, object);
    }

    Ok(())
}

/// Append every indirect object number referenced anywhere inside `object`
/// (recursively through arrays, dictionaries, and stream dictionaries) to
/// `out`. Duplicates are allowed; the caller dedupes via the remap.
fn collect_references(object: &PdfObject, out: &mut Vec<u32>) {
    match object {
        PdfObject::Reference { number, .. } => out.push(*number),
        PdfObject::Array(items) => {
            for item in items {
                collect_references(item, out);
            }
        }
        PdfObject::Dictionary(dict) => {
            for (_, value) in dict.iter() {
                collect_references(value, out);
            }
        }
        PdfObject::Stream { dict, .. } => {
            for (_, value) in dict.iter() {
                collect_references(value, out);
            }
        }
        _ => {}
    }
}

/// A page selected for output, described by its source object number and the
/// inherited attributes resolved onto it (so it renders identically once the
/// ancestor /Pages chain is gone).
struct SelectedPage {
    /// Source object number of the page leaf.
    source_number: u32,
    media_box: [f64; 4],
    crop_box: [f64; 4],
    rotate: i32,
    /// Resolved /Resources dictionary (inherited or own).
    resources: PdfDictionary,
}

/// Build a new PDF from a selection of pages drawn from a single source
/// document, in the given order. `page_indices` are 1-based page numbers.
///
/// This underlies both page extraction (a subset, any order) and the
/// single-document case of split. Shared resources are copied once (the closure
/// dedupes by source object number). Inherited page attributes
/// (`/MediaBox`, `/Resources`, `/Rotate`) are resolved onto each output page so
/// it renders the same without its old ancestor chain.
///
/// Document-level features (AcroForm, outlines, named destinations, document
/// JavaScript, structure tree) are intentionally **not** carried over; only
/// page content and the resources it needs are copied.
pub fn build_subset(doc: &PdfDocument, page_indices: &[usize]) -> Result<Vec<u8>> {
    build_merged_internal(&[(doc, page_indices.to_vec())])
}

/// Build a new PDF by concatenating pages from several source documents.
///
/// Each entry is `(reader, page_indices)` — a source document and the 1-based
/// page numbers to take from it, in order. Pages appear in the output in the
/// order given: all of document 0's selected pages, then document 1's, etc.
/// Within a single source document shared resources are deduped; across
/// documents objects are kept distinct (different documents are never merged at
/// the object level even if coincidentally identical).
pub fn build_merged(inputs: &[(&PdfDocument, Vec<usize>)]) -> Result<Vec<u8>> {
    build_merged_internal(inputs)
}

fn build_merged_internal(inputs: &[(&PdfDocument, Vec<usize>)]) -> Result<Vec<u8>> {
    // Reserve object 1 for the catalog and object 2 for the root /Pages node;
    // page objects and their closures get numbers from 3 upward.
    let catalog_number = 1u32;
    let pages_number = 2u32;
    let mut next_number = 3u32;

    let mut all_objects: Vec<OutputObject> = Vec::new();
    // New object numbers of every page leaf, in output order, for the /Kids array.
    let mut page_new_numbers: Vec<u32> = Vec::new();
    // Carry an /Info and /ID from the first document if available.
    let mut info_number: Option<u32> = None;
    let mut file_id: Option<Vec<u8>> = None;

    for (doc_index, (doc, page_indices)) in inputs.iter().enumerate() {
        let reader = doc.reader();
        let pages = doc.get_pages()?;

        let mut copier = DocCopier::new(reader);

        // First, resolve each selected page's inherited attributes and copy its
        // closure (contents + resources + everything they reference).
        let mut selected: Vec<SelectedPage> = Vec::new();
        for &page_index in page_indices {
            let page = pages.get(page_index - 1).ok_or_else(|| {
                OxideError::MalformedPdf(format!(
                    "page {page_index} is out of range (document has {} pages)",
                    pages.len()
                ))
            })?;
            selected.push(SelectedPage {
                source_number: page.object_number,
                media_box: page.media_box,
                crop_box: page.crop_box,
                rotate: page.rotate,
                resources: page.resources.clone(),
            });
        }

        // Copy the closure of every selected page's content + resources. We do
        // NOT copy the page dictionaries themselves verbatim — we synthesize
        // fresh page dictionaries below with inherited attributes resolved and
        // /Parent pointing at the new /Pages node. But we DO need the closure of
        // each page's /Contents and /Resources.
        for sel in &selected {
            // Fetch the source page dict to find its /Contents and /Annots.
            let page_obj = reader.get_object(sel.source_number, 0)?;
            let page_dict = page_obj
                .as_dict()
                .cloned()
                .unwrap_or_else(PdfDictionary::empty);

            // Contents: copy the stream(s) closure.
            if let Some(contents) = page_dict.get("Contents") {
                let mut content_refs = Vec::new();
                collect_references(contents, &mut content_refs);
                let roots: Vec<(u32, u16)> = content_refs.iter().map(|&n| (n, 0)).collect();
                copy_closure(&mut copier, &roots, &mut next_number)?;
            }

            // Resources: copy the resolved resource dictionary's closure. The
            // resolved resources may contain inline dictionaries plus indirect
            // references (fonts, XObjects, …). Copy every referenced object.
            let mut res_refs = Vec::new();
            collect_references(&PdfObject::Dictionary(sel.resources.clone()), &mut res_refs);
            let res_roots: Vec<(u32, u16)> = res_refs.iter().map(|&n| (n, 0)).collect();
            copy_closure(&mut copier, &res_roots, &mut next_number)?;
        }

        // Now assign output numbers to the page leaves and synthesize fresh
        // page dictionaries.
        for sel in &selected {
            let new_page_number = next_number;
            next_number += 1;
            page_new_numbers.push(new_page_number);

            let page_obj = reader.get_object(sel.source_number, 0)?;
            let page_dict = page_obj
                .as_dict()
                .cloned()
                .unwrap_or_else(PdfDictionary::empty);

            let mut new_page = PdfDictionary::empty();
            new_page.insert("Type", PdfObject::Name("Page".to_string()));
            new_page.insert(
                "Parent",
                PdfObject::Reference {
                    number: pages_number,
                    generation: 0,
                },
            );
            new_page.insert("MediaBox", box_array(sel.media_box));
            // Only emit /CropBox when it differs from /MediaBox (the common case
            // is they coincide and the reader defaults CropBox to MediaBox).
            if sel.crop_box != sel.media_box {
                new_page.insert("CropBox", box_array(sel.crop_box));
            }
            if sel.rotate != 0 {
                new_page.insert("Rotate", PdfObject::Integer(sel.rotate as i64));
            }
            // Resolve inherited /Resources onto the page (rewriting references
            // into the new numbering).
            let resources =
                rewrite_references(PdfObject::Dictionary(sel.resources.clone()), &copier.remap);
            new_page.insert("Resources", resources);

            // Carry /Contents, rewriting its references into the new numbering.
            if let Some(contents) = page_dict.get("Contents") {
                let new_contents = rewrite_references(contents.clone(), &copier.remap);
                new_page.insert("Contents", new_contents);
            }

            all_objects.push(OutputObject {
                number: new_page_number,
                object: PdfObject::Dictionary(new_page),
            });
        }

        // Emit the copied closure objects with references rewritten into the
        // new numbering.
        for (new_number, object) in copier.copied {
            let rewritten = rewrite_references(object, &copier.remap);
            all_objects.push(OutputObject {
                number: new_number,
                object: rewritten,
            });
        }

        // Carry /Info and /ID from the first document only.
        if doc_index == 0 {
            if let Some((info_old, info_gen)) = reader.info_reference() {
                if let Ok(info_obj) = reader.get_object(info_old, info_gen) {
                    if matches!(info_obj, PdfObject::Dictionary(_)) {
                        let n = next_number;
                        next_number += 1;
                        // The info dict can itself reference nothing important;
                        // copy it directly (rewrite any references to null).
                        let empty = HashMap::new();
                        let rewritten = rewrite_references(info_obj, &empty);
                        all_objects.push(OutputObject {
                            number: n,
                            object: rewritten,
                        });
                        info_number = Some(n);
                    }
                }
            }
            file_id = reader.first_file_id();
        }
    }

    // Build the root /Pages node.
    let mut pages_node = PdfDictionary::empty();
    pages_node.insert("Type", PdfObject::Name("Pages".to_string()));
    pages_node.insert(
        "Kids",
        PdfObject::Array(
            page_new_numbers
                .iter()
                .map(|&n| PdfObject::Reference {
                    number: n,
                    generation: 0,
                })
                .collect(),
        ),
    );
    pages_node.insert("Count", PdfObject::Integer(page_new_numbers.len() as i64));
    all_objects.push(OutputObject {
        number: pages_number,
        object: PdfObject::Dictionary(pages_node),
    });

    // Build the catalog.
    let mut catalog = PdfDictionary::empty();
    catalog.insert("Type", PdfObject::Name("Catalog".to_string()));
    catalog.insert(
        "Pages",
        PdfObject::Reference {
            number: pages_number,
            generation: 0,
        },
    );
    all_objects.push(OutputObject {
        number: catalog_number,
        object: PdfObject::Dictionary(catalog),
    });

    let writer = PdfWriter::new(all_objects, catalog_number)
        .with_info(info_number)
        .with_id(file_id);
    writer.write()
}

/// Round-trip a whole document: copy every in-use object under an identity-ish
/// renumbering and emit a fresh file with the same catalog. Primarily a writer
/// correctness probe (parse → write → re-parse should preserve page count,
/// sizes, and text). Encrypted inputs are decrypted on read, so the output is
/// unencrypted.
pub fn write_document_roundtrip(reader: &PdfReader) -> Result<Vec<u8>> {
    let root = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("cannot round-trip: trailer is missing /Root".to_string())
    })?;

    let ids = reader.object_ids();
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut next = 1u32;
    for &(number, _gen) in &ids {
        remap.entry(number).or_insert_with(|| {
            let n = next;
            next += 1;
            n
        });
    }

    let mut objects: Vec<OutputObject> = Vec::new();
    for &(number, generation) in &ids {
        let object = match reader.get_object(number, generation) {
            Ok(object) => object,
            Err(err) => {
                log::warn!("roundtrip: skipping object {number} {generation}: {err}");
                continue;
            }
        };
        // Skip /Type /XRef streams — the writer produces its own classic xref
        // table, so old cross-reference streams must not be re-emitted as
        // ordinary objects (they'd be dead weight and reference stale offsets).
        if let PdfObject::Stream { dict, .. } = &object {
            if dict.get_name("Type") == Some("XRef") {
                continue;
            }
        }
        let new_number = remap[&number];
        let rewritten = rewrite_references(object, &remap);
        objects.push(OutputObject {
            number: new_number,
            object: rewritten,
        });
    }

    let new_root = remap[&root.0];
    let info_number = reader
        .info_reference()
        .and_then(|(n, _)| remap.get(&n).copied());

    let writer = PdfWriter::new(objects, new_root)
        .with_info(info_number)
        .with_id(reader.first_file_id());
    writer.write()
}

fn box_array(values: [f64; 4]) -> PdfObject {
    PdfObject::Array(values.iter().map(|&v| number_object(v)).collect())
}

/// Emit a box coordinate as an integer when it is integral (the overwhelmingly
/// common case for /MediaBox) and as a real otherwise.
fn number_object(value: f64) -> PdfObject {
    if value == value.trunc() && value.abs() < 1e15 {
        PdfObject::Integer(value as i64)
    } else {
        PdfObject::Real(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::PdfObject;

    fn ser(object: &PdfObject) -> Vec<u8> {
        let mut out = Vec::new();
        serialize_object(object, &mut out);
        out
    }

    fn ser_str(object: &PdfObject) -> String {
        String::from_utf8(ser(object)).unwrap()
    }

    #[test]
    fn serializes_scalars() {
        assert_eq!(ser_str(&PdfObject::Null), "null");
        assert_eq!(ser_str(&PdfObject::Boolean(true)), "true");
        assert_eq!(ser_str(&PdfObject::Boolean(false)), "false");
        assert_eq!(ser_str(&PdfObject::Integer(-42)), "-42");
        assert_eq!(ser_str(&PdfObject::Real(1.5)), "1.5");
        assert_eq!(ser_str(&PdfObject::Real(3.0)), "3");
        assert_eq!(
            ser_str(&PdfObject::Reference {
                number: 12,
                generation: 0
            }),
            "12 0 R"
        );
    }

    #[test]
    fn serializes_name_with_hex_escapes() {
        assert_eq!(ser_str(&PdfObject::Name("Foo".to_string())), "/Foo");
        // Space and '#' must be escaped.
        assert_eq!(ser_str(&PdfObject::Name("A Name".to_string())), "/A#20Name");
        assert_eq!(ser_str(&PdfObject::Name("a#b".to_string())), "/a#23b");
    }

    #[test]
    fn serializes_literal_string_with_escapes() {
        assert_eq!(
            ser_str(&PdfObject::String(b"a(b)c\\d".to_vec())),
            "(a\\(b\\)c\\\\d)"
        );
    }

    #[test]
    fn serializes_binary_string_as_hex() {
        let bytes = vec![0x00, 0x01, 0xFF, 0xFE, 0x80, 0x90];
        let out = ser_str(&PdfObject::String(bytes));
        assert!(out.starts_with('<') && out.ends_with('>'), "got {out}");
        assert_eq!(out, "<0001FFFE8090>");
    }

    #[test]
    fn each_scalar_roundtrips_through_parser() {
        use crate::parser::PdfParser;
        let cases = vec![
            PdfObject::Boolean(true),
            PdfObject::Integer(0),
            PdfObject::Integer(-1234567),
            PdfObject::Real(1.25),
            PdfObject::Name("Weird /Name#here".to_string()),
            PdfObject::String(b"plain text".to_vec()),
            PdfObject::String(b"with (parens) and \\backslash".to_vec()),
            PdfObject::String(vec![0, 1, 2, 250, 251, 255]),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Real(2.5),
                PdfObject::Name("X".to_string()),
                PdfObject::Reference {
                    number: 9,
                    generation: 0,
                },
            ]),
        ];
        for case in cases {
            let bytes = ser(&case);
            let mut parser = PdfParser::new(&bytes, 0).unwrap();
            let parsed = parser.parse_object().unwrap();
            assert_eq!(parsed, case, "roundtrip mismatch for {case:?}");
        }
    }

    #[test]
    fn nested_dict_and_stream_roundtrip() {
        use crate::parser::PdfParser;
        let mut inner = PdfDictionary::empty();
        inner.insert("Key", PdfObject::Integer(7));
        let mut dict = PdfDictionary::empty();
        dict.insert("Nested", PdfObject::Dictionary(inner));
        dict.insert(
            "Arr",
            PdfObject::Array(vec![PdfObject::Boolean(false), PdfObject::Null]),
        );

        let stream = PdfObject::Stream {
            dict: dict.clone(),
            raw: vec![1, 2, 3, b'\n', 4, 5],
        };
        let bytes = ser(&stream);
        let mut parser = PdfParser::new(&bytes, 0).unwrap();
        let parsed = parser.parse_object().unwrap();
        match parsed {
            PdfObject::Stream { dict: pd, raw } => {
                assert_eq!(raw, vec![1, 2, 3, b'\n', 4, 5]);
                assert_eq!(pd.get_integer("Length"), Some(6));
                assert_eq!(pd.get("Nested"), dict.get("Nested"));
            }
            other => panic!("expected stream, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_references_remaps_and_nulls_unknown() {
        let mut remap = HashMap::new();
        remap.insert(5u32, 10u32);
        let obj = PdfObject::Array(vec![
            PdfObject::Reference {
                number: 5,
                generation: 0,
            },
            PdfObject::Reference {
                number: 6,
                generation: 0,
            },
        ]);
        let rewritten = rewrite_references(obj, &remap);
        assert_eq!(
            rewritten,
            PdfObject::Array(vec![
                PdfObject::Reference {
                    number: 10,
                    generation: 0
                },
                PdfObject::Null,
            ])
        );
    }

    #[test]
    fn writer_emits_parseable_minimal_file() {
        use crate::reader::PdfReader;
        // Catalog -> Pages -> Page -> Contents, hand-built and written out.
        let mut catalog = PdfDictionary::empty();
        catalog.insert("Type", PdfObject::Name("Catalog".to_string()));
        catalog.insert(
            "Pages",
            PdfObject::Reference {
                number: 2,
                generation: 0,
            },
        );
        let mut pages = PdfDictionary::empty();
        pages.insert("Type", PdfObject::Name("Pages".to_string()));
        pages.insert(
            "Kids",
            PdfObject::Array(vec![PdfObject::Reference {
                number: 3,
                generation: 0,
            }]),
        );
        pages.insert("Count", PdfObject::Integer(1));
        let mut page = PdfDictionary::empty();
        page.insert("Type", PdfObject::Name("Page".to_string()));
        page.insert(
            "Parent",
            PdfObject::Reference {
                number: 2,
                generation: 0,
            },
        );
        page.insert(
            "MediaBox",
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(200),
                PdfObject::Integer(200),
            ]),
        );
        page.insert(
            "Contents",
            PdfObject::Reference {
                number: 4,
                generation: 0,
            },
        );
        let content = b"BT /F1 12 Tf 72 100 Td (Hi) Tj ET".to_vec();
        let stream = PdfObject::Stream {
            dict: PdfDictionary::empty(),
            raw: content,
        };

        let objects = vec![
            OutputObject {
                number: 1,
                object: PdfObject::Dictionary(catalog),
            },
            OutputObject {
                number: 2,
                object: PdfObject::Dictionary(pages),
            },
            OutputObject {
                number: 3,
                object: PdfObject::Dictionary(page),
            },
            OutputObject {
                number: 4,
                object: stream,
            },
        ];
        let bytes = PdfWriter::new(objects, 1).write().unwrap();

        // Re-parse with the real reader.
        let reader = PdfReader::from_bytes(bytes).unwrap();
        let root = reader.root_reference().unwrap();
        let catalog = reader.get_and_resolve(root.0, root.1).unwrap();
        assert_eq!(catalog.as_dict().unwrap().get_name("Type"), Some("Catalog"));
        let pages_ref = catalog.as_dict().unwrap().get_reference("Pages").unwrap();
        let pages = reader.get_and_resolve(pages_ref.0, pages_ref.1).unwrap();
        assert_eq!(pages.as_dict().unwrap().get_integer("Count"), Some(1));
    }
}
