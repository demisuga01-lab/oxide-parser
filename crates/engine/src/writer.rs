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

use std::collections::{BTreeMap, BTreeSet, HashMap};

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

/// An indirect object appended by an incremental update.
#[derive(Clone, Debug)]
pub struct IncrementalObject {
    pub number: u32,
    pub generation: u16,
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
    /// When set, the output is ENCRYPTED: every string and stream is encrypted
    /// per-object with the standard security handler, and an `/Encrypt`
    /// dictionary object is added (its number recorded so it is itself NOT
    /// encrypted). See [`PdfWriter::with_encryption`].
    encryption: Option<WriterEncryption>,
    /// Cross-reference structure to emit. See [`WriterMode`].
    mode: WriterMode,
}

/// The cross-reference structure the writer emits.
///
/// `ClassicXref` (the default) writes a PDF 1.x classic `xref` table + trailer
/// — maximum reader compatibility. `XrefStream` writes a PDF 1.5+ cross-
/// reference stream (`/Type /XRef`) instead, which is smaller and is the
/// prerequisite for object streams and linearization. `XrefStreamWithObjStm`
/// additionally packs eligible non-stream objects into compressed object
/// streams (`/Type /ObjStm`) — the main file-size win for object-heavy PDFs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriterMode {
    /// Classic `xref` table + `trailer` (PDF 1.x). Maximum compatibility.
    #[default]
    ClassicXref,
    /// A `/Type /XRef` cross-reference stream (PDF 1.5+).
    XrefStream,
    /// Cross-reference stream + object-stream packing (smallest output).
    XrefStreamWithObjStm,
}

/// Encryption configuration for [`PdfWriter`]: the derived key/params plus the
/// output object number reserved for the `/Encrypt` dictionary.
struct WriterEncryption {
    state: crate::crypto::EncryptState,
    encrypt_obj_number: u32,
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
            encryption: None,
            mode: WriterMode::ClassicXref,
        }
    }

    /// Select the cross-reference structure to emit (see [`WriterMode`]).
    /// Defaults to [`WriterMode::ClassicXref`]. The modern modes bump the header
    /// version to 1.5 when it would otherwise be lower.
    pub fn with_mode(mut self, mode: WriterMode) -> Self {
        self.mode = mode;
        if mode != WriterMode::ClassicXref && version_lt_1_5(&self.version) {
            self.version = "1.5".to_string();
        }
        self
    }

    /// Encrypt the output with the standard security handler. `state` carries the
    /// derived file key + `/Encrypt` parameters (build it via
    /// [`crate::crypto::build_encryption`]). This reserves the next free object
    /// number for the `/Encrypt` dictionary and forces the file `/ID` to a fresh
    /// random value (required: AES-256 ignores it, but legacy keys depend on it,
    /// and a stable `/ID` is needed for the dict). All strings/streams except the
    /// `/Encrypt` dict are encrypted on write. AES-256 output bumps the header to
    /// 2.0; AES-128/RC4 to 1.6/1.4 as appropriate.
    pub fn with_encryption(mut self, state: crate::crypto::EncryptState) -> Self {
        // Reserve an object number for the /Encrypt dict (one past the max).
        let max = self.objects.iter().map(|o| o.number).max().unwrap_or(0);
        let encrypt_obj_number = max + 1;
        // A deterministic-but-unique /ID isn't available without RNG; use random.
        // (The /Encrypt dict + legacy key derivation need a stable /ID; AES-256
        // ignores it. We always set one so the file is well-formed.)
        if self.id.is_none() {
            self.id = Some(crate::crypto::random_bytes(16));
        }
        self.version = match state.info.v {
            5 => "2.0".to_string(),
            4 => "1.6".to_string(),
            _ => "1.4".to_string(),
        };
        self.encryption = Some(WriterEncryption {
            state,
            encrypt_obj_number,
        });
        self
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

    /// Serialize the whole document to PDF bytes, using the configured
    /// [`WriterMode`].
    pub fn write(&self) -> Result<Vec<u8>> {
        let pack = self.mode == WriterMode::XrefStreamWithObjStm;

        // Encryption interaction with object streams: objects packed INTO an
        // /ObjStm are NOT individually encrypted (only the ObjStm stream is, as
        // a whole). So when packing, we must work from PLAINTEXT objects and let
        // the modern writer encrypt the right granularity. For the classic and
        // plain-xref-stream paths, every object is a top-level indirect object,
        // so the existing per-object pre-encryption is correct.
        let owned: Vec<OutputObject>;
        let objects_src: Vec<&OutputObject> = if let Some(enc) = &self.encryption {
            if pack {
                // Plaintext + the /Encrypt dict; write_modern applies encryption.
                owned = self.objects_with_encrypt_dict(enc);
                owned.iter().collect()
            } else {
                owned = self.build_encrypted_objects(enc)?;
                owned.iter().collect()
            }
        } else {
            self.objects.iter().collect()
        };

        let mut objects: Vec<&OutputObject> = objects_src;
        objects.sort_by_key(|o| o.number);

        for obj in &objects {
            if obj.number == 0 {
                return Err(OxideError::MalformedPdf(
                    "writer: object number 0 is reserved for the free-list head".to_string(),
                ));
            }
        }
        for pair in objects.windows(2) {
            if pair[0].number == pair[1].number {
                return Err(OxideError::MalformedPdf(format!(
                    "writer: duplicate output object number {}",
                    pair[0].number
                )));
            }
        }

        match self.mode {
            WriterMode::ClassicXref => self.write_classic(&objects),
            WriterMode::XrefStream => self.write_modern(&objects, false),
            WriterMode::XrefStreamWithObjStm => self.write_modern(&objects, true),
        }
    }

    /// The plaintext object set plus the (unencrypted) `/Encrypt` dict object —
    /// used by the packing path, which encrypts at write time so ObjStm inner
    /// objects are not individually encrypted.
    fn objects_with_encrypt_dict(&self, enc: &WriterEncryption) -> Vec<OutputObject> {
        let mut out: Vec<OutputObject> = self.objects.clone();
        out.push(OutputObject {
            number: enc.encrypt_obj_number,
            object: PdfObject::Dictionary(encryption_info_to_dict(&enc.state.info)),
        });
        out
    }

    /// Classic `xref` table + `trailer` output (PDF 1.x).
    fn write_classic(&self, objects: &[&OutputObject]) -> Result<Vec<u8>> {
        let max_number = objects.last().map(|o| o.number).unwrap_or(0);
        let size = max_number as usize + 1;

        let mut out = Vec::new();
        out.extend_from_slice(format!("%PDF-{}\n", self.version).as_bytes());
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        // Body. Track the byte offset of each object number for the xref table.
        let mut offsets: Vec<Option<usize>> = vec![None; size];
        for obj in objects {
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
        out.extend_from_slice(b"0000000000 65535 f \n");
        for slot in &offsets[1..] {
            match slot {
                Some(offset) => {
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
                }
                None => {
                    out.extend_from_slice(b"0000000000 65535 f \n");
                }
            }
        }

        // Trailer.
        out.extend_from_slice(b"trailer\n");
        let trailer = self.build_trailer_dict(size, None);
        serialize_dictionary(&trailer, &mut out);
        out.extend_from_slice(b"\nstartxref\n");
        out.extend_from_slice(format!("{xref_offset}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");

        Ok(out)
    }

    /// Build the trailer key set shared by the classic trailer and the xref
    /// stream dictionary (`/Size /Root /Info /ID /Encrypt`). For the xref stream
    /// `/Size` is the object count INCLUDING the xref stream object itself, so
    /// it is passed in explicitly.
    fn build_trailer_dict(&self, size: usize, extra: Option<&PdfDictionary>) -> PdfDictionary {
        let mut trailer = if let Some(d) = extra {
            d.clone()
        } else {
            PdfDictionary::empty()
        };
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
        if let Some(enc) = &self.encryption {
            trailer.insert(
                "Encrypt",
                PdfObject::Reference {
                    number: enc.encrypt_obj_number,
                    generation: 0,
                },
            );
        }
        trailer
    }

    /// Modern PDF 1.5+ output: a `/Type /XRef` cross-reference stream, with
    /// optional object-stream (`/Type /ObjStm`) packing when `pack` is set.
    ///
    /// `objects` are the sorted output objects. When encrypting + packing they
    /// are PLAINTEXT (this function applies encryption at the right
    /// granularity); otherwise they are already in their final (possibly
    /// encrypted) form.
    fn write_modern(&self, objects: &[&OutputObject], pack: bool) -> Result<Vec<u8>> {
        let encrypting = self.encryption.is_some();
        let encrypt_obj_number = self.encryption.as_ref().map(|e| e.encrypt_obj_number);

        // Decide which objects go into object streams. Eligible: non-stream,
        // not the /Encrypt dict, not the document /ID-bearing trailer (n/a here),
        // and (for safety) not /Type /XRef (none present yet). Streams and the
        // /Encrypt dict stay as direct (type-1) objects.
        let mut direct: Vec<&OutputObject> = Vec::new();
        let mut packable: Vec<&OutputObject> = Vec::new();
        for obj in objects {
            let is_stream = matches!(obj.object, PdfObject::Stream { .. });
            let is_encrypt_dict = Some(obj.number) == encrypt_obj_number;
            if pack && !is_stream && !is_encrypt_dict {
                packable.push(obj);
            } else {
                direct.push(obj);
            }
        }

        // The xref stream object and any ObjStm objects need fresh numbers
        // beyond the current max.
        let max_number = objects.iter().map(|o| o.number).max().unwrap_or(0);
        let mut next_free = max_number + 1;

        // Group packable objects into object streams (cap per stream so a huge
        // doc doesn't make one enormous ObjStm). Deterministic: objects are
        // already sorted by number, and we chunk in that order.
        const OBJSTM_MAX_OBJECTS: usize = 200;
        struct ObjStmPlan {
            number: u32,
            members: Vec<u32>, // object numbers, in pack order
        }
        let mut objstms: Vec<ObjStmPlan> = Vec::new();
        if pack && !packable.is_empty() {
            for chunk in packable.chunks(OBJSTM_MAX_OBJECTS) {
                let number = next_free;
                next_free += 1;
                objstms.push(ObjStmPlan {
                    number,
                    members: chunk.iter().map(|o| o.number).collect(),
                });
            }
        }
        let xref_stream_number = next_free;
        // /Size is one past the highest object number actually used.
        let size = xref_stream_number as usize + 1;

        // Map object number -> its xref entry (computed as we lay out the file).
        let mut entries: Vec<(u32, XrefEntryOut)> = Vec::new();

        let mut out = Vec::new();
        out.extend_from_slice(format!("%PDF-{}\n", self.version).as_bytes());
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        // Helper to fetch an object body, applying per-object encryption for the
        // DIRECT path (packed objects are handled separately, unencrypted-inner).
        let enc_state = self.encryption.as_ref().map(|e| &e.state);
        let emit_direct = |out: &mut Vec<u8>, obj: &OutputObject| -> Result<usize> {
            let offset = out.len();
            let mut object = obj.object.clone();
            if let Some(state) = enc_state {
                if Some(obj.number) != encrypt_obj_number {
                    encrypt_object_in_place(
                        &mut object,
                        obj.number,
                        0,
                        &state.file_key,
                        state.is_aes(),
                        state.is_v5(),
                    )?;
                }
            }
            out.extend_from_slice(format!("{} 0 obj\n", obj.number).as_bytes());
            serialize_object(&object, out);
            out.extend_from_slice(b"\nendobj\n");
            Ok(offset)
        };

        // 1. Emit direct (type-1) objects.
        for obj in &direct {
            // When NOT packing and NOT encrypting-at-write, `objects` may already
            // be encrypted by the caller; emit verbatim in that case.
            let offset = if pack || !encrypting {
                emit_direct(&mut out, obj)?
            } else {
                // Caller already encrypted; emit as-is.
                let off = out.len();
                out.extend_from_slice(format!("{} 0 obj\n", obj.number).as_bytes());
                serialize_object(&obj.object, &mut out);
                out.extend_from_slice(b"\nendobj\n");
                off
            };
            entries.push((obj.number, XrefEntryOut::Uncompressed { offset }));
        }

        // 2. Build + emit each object stream; record type-2 entries for members.
        let obj_by_number: std::collections::HashMap<u32, &OutputObject> =
            objects.iter().map(|o| (o.number, *o)).collect();
        for plan in &objstms {
            let (objstm_bytes, member_indices) = build_objstm_body(&plan.members, &obj_by_number)?;
            // Filter order matters: the data is FIRST FlateDecode-compressed,
            // THEN (if encrypting) encrypted as a WHOLE stream — encryption is
            // the outermost layer, undone first on read, exactly mirroring how
            // the reader decrypts the raw stream bytes before FlateDecode. The
            // inner objects are NOT individually encrypted.
            let compressed = crate::filters::flate_encode(&objstm_bytes, 9);
            let payload = if let Some(state) = enc_state {
                crate::crypto::encrypt_bytes(
                    &compressed,
                    &state.file_key,
                    plan.number,
                    0,
                    state.is_aes(),
                    state.is_v5(),
                )?
            } else {
                compressed
            };
            let offset = out.len();
            let mut dict = PdfDictionary::empty();
            dict.insert("Type", PdfObject::Name("ObjStm".to_string()));
            dict.insert("N", PdfObject::Integer(plan.members.len() as i64));
            dict.insert(
                "First",
                PdfObject::Integer(member_indices.first_offset as i64),
            );
            dict.insert("Filter", PdfObject::Name("FlateDecode".to_string()));
            dict.insert("Length", PdfObject::Integer(payload.len() as i64));
            out.extend_from_slice(format!("{} 0 obj\n", plan.number).as_bytes());
            serialize_dictionary(&dict, &mut out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&payload);
            out.extend_from_slice(b"\nendstream\nendobj\n");
            entries.push((plan.number, XrefEntryOut::Uncompressed { offset }));
            // Type-2 entries for the members.
            for (idx, &member) in plan.members.iter().enumerate() {
                entries.push((
                    member,
                    XrefEntryOut::Compressed {
                        objstm: plan.number,
                        index: idx as u32,
                    },
                ));
            }
        }

        // 3. The xref stream object itself (type-1, at its own offset).
        let xref_offset = out.len();
        entries.push((
            xref_stream_number,
            XrefEntryOut::Uncompressed {
                offset: xref_offset,
            },
        ));
        // Object 0 is the free-list head (type 0).
        entries.push((0, XrefEntryOut::Free));

        let xref_dict_extra = self.build_trailer_dict(size, None);
        let xref_bytes =
            build_xref_stream(xref_stream_number, size, &mut entries, &xref_dict_extra)?;
        out.extend_from_slice(&xref_bytes);

        out.extend_from_slice(b"\nstartxref\n");
        out.extend_from_slice(format!("{xref_offset}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");

        Ok(out)
    }

    /// Build the encrypted object set: a deep copy of every output object with
    /// its strings + stream bytes encrypted per-object, plus the appended
    /// `/Encrypt` dictionary object (itself unencrypted).
    fn build_encrypted_objects(&self, enc: &WriterEncryption) -> Result<Vec<OutputObject>> {
        let is_aes = enc.state.is_aes();
        let is_v5 = enc.state.is_v5();
        let key = &enc.state.file_key;

        let mut out: Vec<OutputObject> = Vec::with_capacity(self.objects.len() + 1);
        for obj in &self.objects {
            let mut object = obj.object.clone();
            encrypt_object_in_place(&mut object, obj.number, 0, key, is_aes, is_v5)?;
            out.push(OutputObject {
                number: obj.number,
                object,
            });
        }
        // Append the /Encrypt dictionary object (NOT encrypted).
        out.push(OutputObject {
            number: enc.encrypt_obj_number,
            object: PdfObject::Dictionary(encryption_info_to_dict(&enc.state.info)),
        });
        Ok(out)
    }
}

/// Recursively encrypt every string and the stream raw bytes inside one indirect
/// object, keyed by its object number/generation. Dictionaries and arrays are
/// walked; references/names/numbers are untouched. Errors propagate (a cipher
/// failure must not silently emit plaintext).
fn encrypt_object_in_place(
    object: &mut PdfObject,
    obj_num: u32,
    gen_num: u16,
    key: &[u8],
    is_aes: bool,
    is_v5: bool,
) -> Result<()> {
    match object {
        PdfObject::String(bytes) => {
            *bytes = crate::crypto::encrypt_bytes(bytes, key, obj_num, gen_num, is_aes, is_v5)?;
        }
        PdfObject::Array(items) => {
            for item in items.iter_mut() {
                encrypt_object_in_place(item, obj_num, gen_num, key, is_aes, is_v5)?;
            }
        }
        PdfObject::Dictionary(dict) => {
            encrypt_dict_in_place(dict, obj_num, gen_num, key, is_aes, is_v5)?;
        }
        PdfObject::Stream { dict, raw } => {
            encrypt_dict_in_place(dict, obj_num, gen_num, key, is_aes, is_v5)?;
            *raw = crate::crypto::encrypt_bytes(raw, key, obj_num, gen_num, is_aes, is_v5)?;
        }
        _ => {}
    }
    Ok(())
}

fn encrypt_dict_in_place(
    dict: &mut PdfDictionary,
    obj_num: u32,
    gen_num: u16,
    key: &[u8],
    is_aes: bool,
    is_v5: bool,
) -> Result<()> {
    // Rebuild the dictionary with encrypted string values (BTreeMap iteration is
    // by key; we collect then reinsert to mutate values).
    let keys: Vec<String> = dict.iter().map(|(k, _)| k.clone()).collect();
    for k in keys {
        if let Some(mut value) = dict.get(&k).cloned() {
            encrypt_object_in_place(&mut value, obj_num, gen_num, key, is_aes, is_v5)?;
            dict.insert(k, value);
        }
    }
    Ok(())
}

/// Serialize an [`crate::crypto::EncryptionInfo`] into the `/Encrypt`
/// dictionary that a reader's `EncryptionInfo::from_dict` parses back.
fn encryption_info_to_dict(info: &crate::crypto::EncryptionInfo) -> PdfDictionary {
    use crate::crypto::CryptMethod;
    let mut d = PdfDictionary::empty();
    d.insert("Filter", PdfObject::Name("Standard".to_string()));
    d.insert("V", PdfObject::Integer(info.v as i64));
    d.insert("R", PdfObject::Integer(info.r as i64));
    d.insert("Length", PdfObject::Integer(info.key_length as i64));
    d.insert("O", PdfObject::String(info.o.clone()));
    d.insert("U", PdfObject::String(info.u.clone()));
    d.insert("P", PdfObject::Integer(info.p as i64));
    if !info.encrypt_metadata {
        d.insert("EncryptMetadata", PdfObject::Boolean(false));
    }
    if let Some(v5) = &info.v5 {
        d.insert("OE", PdfObject::String(v5.oe.clone()));
        d.insert("UE", PdfObject::String(v5.ue.clone()));
        d.insert("Perms", PdfObject::String(v5.perms.clone()));
    }
    // V4/V5 require crypt filters (/CF, /StmF, /StrF) naming the method.
    if info.v >= 4 {
        let cfm = match info.stream_method {
            CryptMethod::AesV3 => "AESV3",
            CryptMethod::AesV2 => "AESV2",
            _ => "V2",
        };
        let mut stdcf = PdfDictionary::empty();
        stdcf.insert("Type", PdfObject::Name("CryptFilter".to_string()));
        stdcf.insert("CFM", PdfObject::Name(cfm.to_string()));
        // AuthEvent defaults to DocOpen; Length in bytes for the filter.
        let cf_len = if info.v == 5 { 32 } else { info.key_length / 8 };
        stdcf.insert("Length", PdfObject::Integer(cf_len as i64));
        let mut cf = PdfDictionary::empty();
        cf.insert("StdCF", PdfObject::Dictionary(stdcf));
        d.insert("CF", PdfObject::Dictionary(cf));
        d.insert("StmF", PdfObject::Name("StdCF".to_string()));
        d.insert("StrF", PdfObject::Name("StdCF".to_string()));
    }
    d
}

// ===========================================================================
// Modern PDF 1.5+ output: cross-reference streams + object streams
// ===========================================================================

/// An xref entry as the writer computes it before encoding into the binary
/// cross-reference-stream payload.
#[derive(Debug, Clone, Copy)]
enum XrefEntryOut {
    /// Type 0: free object (only object 0, the free-list head).
    Free,
    /// Type 1: uncompressed object at a byte offset.
    Uncompressed { offset: usize },
    /// Type 2: object inside an object stream, at the given index.
    Compressed { objstm: u32, index: u32 },
}

/// Byte offsets recorded while building an object stream's header.
struct ObjStmOffsets {
    /// The `/First` value: the byte offset where the first packed object body
    /// begins (i.e. the header length).
    first_offset: usize,
}

/// Build the (decoded) body of an object stream: a header of `objnum offset`
/// integer-token pairs, then the concatenated object bodies. Offsets in the
/// header are relative to `/First` (the header length). Returns the decoded
/// bytes plus the `/First` offset. Mirrors exactly what
/// `reader::parse_object_stream_data` expects.
fn build_objstm_body(
    members: &[u32],
    obj_by_number: &std::collections::HashMap<u32, &OutputObject>,
) -> Result<(Vec<u8>, ObjStmOffsets)> {
    // First serialize each member body so we know its length, then build the
    // header (which needs the relative offsets), then concatenate.
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(members.len());
    for &num in members {
        let obj = obj_by_number.get(&num).ok_or_else(|| {
            OxideError::MalformedPdf(format!("objstm member {num} missing from object set"))
        })?;
        let mut body = Vec::new();
        serialize_object(&obj.object, &mut body);
        // Object bodies in an ObjStm are whitespace-separated.
        body.push(b' ');
        bodies.push(body);
    }

    // Build the header: "objnum reloffset " pairs. The relative offsets depend
    // on the header length, so compute body offsets first (relative to 0), then
    // the header, then the /First offset is the header length.
    let mut rel_offsets = Vec::with_capacity(members.len());
    let mut acc = 0usize;
    for body in &bodies {
        rel_offsets.push(acc);
        acc += body.len();
    }
    let mut header = Vec::new();
    for (i, &num) in members.iter().enumerate() {
        header.extend_from_slice(format!("{} {} ", num, rel_offsets[i]).as_bytes());
    }
    let first_offset = header.len();

    let mut decoded = header;
    for body in &bodies {
        decoded.extend_from_slice(body);
    }
    Ok((decoded, ObjStmOffsets { first_offset }))
}

/// Build the `/Type /XRef` cross-reference stream object (its full
/// `N 0 obj … endobj` text). `entries` is the complete entry set (object 0 +
/// all objects + the xref stream itself); it is sorted here. `dict_keys`
/// carries the trailer keys (/Root /Info /ID /Size /Encrypt). The payload is
/// FlateDecode'd. Field widths `/W` are chosen to fit the largest values.
fn build_xref_stream(
    xref_number: u32,
    size: usize,
    entries: &mut [(u32, XrefEntryOut)],
    dict_keys: &PdfDictionary,
) -> Result<Vec<u8>> {
    build_xref_stream_with_index(xref_number, size, entries, dict_keys, None)
}

fn build_xref_stream_with_index(
    xref_number: u32,
    size: usize,
    entries: &mut [(u32, XrefEntryOut)],
    dict_keys: &PdfDictionary,
    index_ranges: Option<Vec<(u32, u32)>>,
) -> Result<Vec<u8>> {
    entries.sort_by_key(|(n, _)| *n);

    // Choose field widths. W[0] = type (1 byte is plenty: types 0/1/2).
    // W[1] = max(offset, objstm number). W[2] = max(generation=0, objstm index).
    let mut max_f1: u64 = 0;
    let mut max_f2: u64 = 0;
    for (_, e) in entries.iter() {
        match e {
            XrefEntryOut::Free => {}
            XrefEntryOut::Uncompressed { offset } => {
                max_f1 = max_f1.max(*offset as u64);
            }
            XrefEntryOut::Compressed { objstm, index } => {
                max_f1 = max_f1.max(*objstm as u64);
                max_f2 = max_f2.max(*index as u64);
            }
        }
    }
    let w0 = 1usize;
    let w1 = byte_width(max_f1).max(1);
    let w2 = byte_width(max_f2).max(1);

    let index_ranges = index_ranges.unwrap_or_else(|| vec![(0, size as u32)]);

    // Build the binary payload for the requested /Index ranges. Any gap inside
    // a range is emitted as a free entry.
    let mut by_number: std::collections::HashMap<u32, XrefEntryOut> =
        std::collections::HashMap::with_capacity(entries.len());
    for (n, e) in entries.iter() {
        by_number.insert(*n, *e);
    }
    let entry_count: usize = index_ranges.iter().map(|(_, count)| *count as usize).sum();
    let mut payload = Vec::with_capacity(entry_count * (w0 + w1 + w2));
    for (start, count) in &index_ranges {
        for n in *start..start.saturating_add(*count) {
            let entry = by_number.get(&n).copied().unwrap_or(XrefEntryOut::Free);
            let (t, f1, f2): (u64, u64, u64) = match entry {
                XrefEntryOut::Free => (0, 0, 0),
                XrefEntryOut::Uncompressed { offset } => (1, offset as u64, 0),
                XrefEntryOut::Compressed { objstm, index } => (2, objstm as u64, index as u64),
            };
            write_be_field(&mut payload, t, w0);
            write_be_field(&mut payload, f1, w1);
            write_be_field(&mut payload, f2, w2);
        }
    }

    let compressed = crate::filters::flate_encode(&payload, 9);

    let mut dict = dict_keys.clone();
    dict.insert("Size", PdfObject::Integer(size as i64));
    dict.insert("Type", PdfObject::Name("XRef".to_string()));
    dict.insert(
        "W",
        PdfObject::Array(vec![
            PdfObject::Integer(w0 as i64),
            PdfObject::Integer(w1 as i64),
            PdfObject::Integer(w2 as i64),
        ]),
    );
    dict.insert("Filter", PdfObject::Name("FlateDecode".to_string()));
    dict.insert("Length", PdfObject::Integer(compressed.len() as i64));
    let mut index_array = Vec::with_capacity(index_ranges.len() * 2);
    for (start, count) in &index_ranges {
        index_array.push(PdfObject::Integer(*start as i64));
        index_array.push(PdfObject::Integer(*count as i64));
    }
    dict.insert("Index", PdfObject::Array(index_array));

    let mut out = Vec::new();
    out.extend_from_slice(format!("{xref_number} 0 obj\n").as_bytes());
    serialize_dictionary(&dict, &mut out);
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    Ok(out)
}

/// Minimum number of bytes needed to hold `value` big-endian (0 -> 1).
fn byte_width(value: u64) -> usize {
    if value == 0 {
        return 1;
    }
    let bits = 64 - value.leading_zeros() as usize;
    bits.div_ceil(8)
}

/// Append `value` as a big-endian field of exactly `width` bytes.
fn write_be_field(out: &mut Vec<u8>, value: u64, width: usize) {
    let bytes = value.to_be_bytes();
    out.extend_from_slice(&bytes[8 - width..]);
}

/// True if a PDF version string is below 1.5 (so the modern modes must bump it).
fn version_lt_1_5(version: &str) -> bool {
    // Versions are like "1.4", "1.7", "2.0". Compare (major, minor) numerically.
    let parse = |v: &str| -> (u32, u32) {
        let mut it = v.split('.');
        let major = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let minor = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (major, minor)
    };
    parse(version) < (1, 5)
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
            for (key, value) in dict.iter() {
                if key == "Length" {
                    continue;
                }
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
    rewrite_document(reader, |_orig, _obj| {})
}

/// Append an incremental update revision to `reader`'s original bytes.
///
/// Only the supplied objects are written, followed by a classic xref section
/// and trailer with `/Prev` pointing at the previous `startxref`. This leaves
/// the original byte prefix untouched, which is the required shape for later
/// signature-preserving updates.
pub fn write_incremental_update(
    reader: &PdfReader,
    changed_objects: Vec<IncrementalObject>,
) -> Result<Vec<u8>> {
    if reader.is_encrypted() {
        return Err(OxideError::UnsupportedFeature(
            "incremental writer: encrypted inputs require encrypted appended objects".to_string(),
        ));
    }
    if changed_objects.is_empty() {
        return Ok(reader.file_bytes().to_vec());
    }

    let mut objects = changed_objects;
    objects.sort_by_key(|obj| (obj.number, obj.generation));
    for pair in objects.windows(2) {
        if pair[0].number == pair[1].number && pair[0].generation == pair[1].generation {
            return Err(OxideError::MalformedPdf(format!(
                "incremental writer: duplicate object {} {}",
                pair[0].number, pair[0].generation
            )));
        }
    }
    for obj in &objects {
        if obj.number == 0 {
            return Err(OxideError::MalformedPdf(
                "incremental writer: object number 0 is reserved".to_string(),
            ));
        }
    }

    let mut out = reader.file_bytes().to_vec();
    if !out.ends_with(b"\n") && !out.ends_with(b"\r") {
        out.push(b'\n');
    }

    let mut offsets = Vec::with_capacity(objects.len());
    for obj in &objects {
        let offset = out.len();
        offsets.push((obj.number, obj.generation, offset));
        out.extend_from_slice(format!("{} {} obj\n", obj.number, obj.generation).as_bytes());
        serialize_object(&obj.object, &mut out);
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n");
    for group in contiguous_xref_groups(&offsets) {
        out.extend_from_slice(format!("{} {}\n", group.start, group.entries.len()).as_bytes());
        for (_, generation, offset) in group.entries {
            out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
    }

    out.extend_from_slice(b"trailer\n");
    let trailer = incremental_trailer_dict(reader, &objects)?;
    serialize_dictionary(&trailer, &mut out);
    out.extend_from_slice(b"\nstartxref\n");
    out.extend_from_slice(format!("{xref_offset}\n").as_bytes());
    out.extend_from_slice(b"%%EOF\n");

    Ok(out)
}

/// Whole-document copy with a per-object MUTATION HOOK — the content-preserving
/// base shared by the structural-write ops (rotate, optimize, encrypt, repair).
///
/// Walks every live object (`reader.object_ids()`), re-fetches each (which
/// re-applies the parser's stream-length recovery), identity-renumbers to a
/// contiguous gen-0 space, rewrites references, and emits a fresh classic
/// xref + trailer. Unlike [`build_subset`]/[`build_merged`] this preserves the
/// ORIGINAL catalog (AcroForm, outlines, named destinations, annotations,
/// structure tree all survive) — it mutates objects in place rather than
/// synthesizing a fresh page tree.
///
/// `mutate` is called for every object after reference-rewriting, with its
/// ORIGINAL (source) object number and a mutable handle, so an op can adjust
/// specific objects (e.g. set `/Rotate` on a page) by their identity. `/Type
/// /XRef` streams are dropped (the writer emits its own classic xref).
struct XrefGroup<'a> {
    start: u32,
    entries: &'a [(u32, u16, usize)],
}

fn contiguous_xref_groups(entries: &[(u32, u16, usize)]) -> Vec<XrefGroup<'_>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut groups = Vec::new();
    let mut start_idx = 0usize;
    for idx in 1..entries.len() {
        if entries[idx].0 != entries[idx - 1].0 + 1 {
            groups.push(XrefGroup {
                start: entries[start_idx].0,
                entries: &entries[start_idx..idx],
            });
            start_idx = idx;
        }
    }
    groups.push(XrefGroup {
        start: entries[start_idx].0,
        entries: &entries[start_idx..],
    });
    groups
}

fn incremental_trailer_dict(
    reader: &PdfReader,
    changed_objects: &[IncrementalObject],
) -> Result<PdfDictionary> {
    let mut trailer = PdfDictionary::empty();
    let max_changed = changed_objects
        .iter()
        .map(|obj| i64::from(obj.number))
        .max()
        .unwrap_or(0);
    let existing_size = reader.size().unwrap_or(0);
    trailer.insert(
        "Size",
        PdfObject::Integer(existing_size.max(max_changed + 1)),
    );
    let (root, root_generation) = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("incremental writer: trailer is missing /Root".to_string())
    })?;
    trailer.insert(
        "Root",
        PdfObject::Reference {
            number: root,
            generation: root_generation,
        },
    );
    if let Some((info, generation)) = reader.info_reference() {
        trailer.insert(
            "Info",
            PdfObject::Reference {
                number: info,
                generation,
            },
        );
    }
    if let Some(PdfObject::Array(id)) = reader.trailer().get("ID") {
        trailer.insert("ID", PdfObject::Array(id.clone()));
    }
    if let Some(encrypt) = reader.trailer().get("Encrypt") {
        trailer.insert("Encrypt", encrypt.clone());
    }
    trailer.insert("Prev", PdfObject::Integer(reader.startxref_offset() as i64));
    Ok(trailer)
}

pub fn rewrite_document(
    reader: &PdfReader,
    mutate: impl FnMut(u32, &mut PdfObject),
) -> Result<Vec<u8>> {
    rewrite_document_with_mode(reader, WriterMode::ClassicXref, mutate)
}

/// [`rewrite_document`] with a selectable [`WriterMode`] (classic xref, xref
/// stream, or xref stream + object streams). The content-preserving copy is
/// identical; only the cross-reference structure of the output differs.
pub fn rewrite_document_with_mode(
    reader: &PdfReader,
    mode: WriterMode,
    mut mutate: impl FnMut(u32, &mut PdfObject),
) -> Result<Vec<u8>> {
    let (objects, new_root, info_number) = rewrite_document_objects(reader, &mut mutate)?;
    let writer = PdfWriter::new(objects, new_root)
        .with_info(info_number)
        .with_id(reader.first_file_id())
        .with_mode(mode);
    writer.write()
}

/// The object-collection half of [`rewrite_document`], returning the renumbered
/// objects + new root + new info number. Separated so ops that need to add
/// objects (e.g. encrypt appends an `/Encrypt` dict) or drive the writer with
/// extra configuration can do so before calling [`PdfWriter::write`].
pub fn rewrite_document_objects(
    reader: &PdfReader,
    mutate: &mut impl FnMut(u32, &mut PdfObject),
) -> Result<(Vec<OutputObject>, u32, Option<u32>)> {
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
        let mut rewritten = rewrite_references(object, &remap);
        mutate(number, &mut rewritten);
        objects.push(OutputObject {
            number: new_number,
            object: rewritten,
        });
    }

    let new_root = remap[&root.0];
    let info_number = reader
        .info_reference()
        .and_then(|(n, _)| remap.get(&n).copied());

    Ok((objects, new_root, info_number))
}

fn rewrite_document_objects_with_remap(
    reader: &PdfReader,
    remap: &HashMap<u32, u32>,
    mutate: &mut impl FnMut(u32, &mut PdfObject),
) -> Result<(Vec<OutputObject>, u32, Option<u32>)> {
    let root = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("cannot round-trip: trailer is missing /Root".to_string())
    })?;

    let mut objects: Vec<OutputObject> = Vec::new();
    for (number, generation) in reader.object_ids() {
        let Some(&new_number) = remap.get(&number) else {
            continue;
        };
        let object = match reader.get_object(number, generation) {
            Ok(object) => object,
            Err(err) => {
                log::warn!("roundtrip: skipping object {number} {generation}: {err}");
                continue;
            }
        };
        if let PdfObject::Stream { dict, .. } = &object {
            if dict.get_name("Type") == Some("XRef") {
                continue;
            }
        }
        let mut rewritten = rewrite_references(object, remap);
        mutate(number, &mut rewritten);
        objects.push(OutputObject {
            number: new_number,
            object: rewritten,
        });
    }

    let new_root = remap[&root.0];
    let info_number = reader
        .info_reference()
        .and_then(|(n, _)| remap.get(&n).copied());

    Ok((objects, new_root, info_number))
}

/// Write a linearized (Fast Web View) PDF using PDF 1.5 cross-reference
/// streams. The content-preserving object copy is the same base used by the
/// other structural operations; this layer changes object ordering, writes the
/// linearization parameter dictionary, emits page/shared-object hint tables,
/// and iterates until the offset-bearing structures are stable.
pub fn write_document_linearized(doc: &PdfDocument) -> Result<Vec<u8>> {
    let reader = doc.reader();
    let pages = doc.get_pages()?;
    if pages.is_empty() {
        return Err(OxideError::MalformedPdf(
            "linearize: document has no pages".to_string(),
        ));
    }
    ensure_linearization_supported(doc, &pages)?;

    let source_plan = build_linearization_source_plan(reader, &pages)?;
    let remap = source_plan.remap.clone();
    let page_by_source: HashMap<u32, crate::document::PdfPage> = pages
        .iter()
        .cloned()
        .map(|page| (page.object_number, page))
        .collect();
    let mut normalize_pages = |orig: u32, object: &mut PdfObject| {
        let PdfObject::Dictionary(dict) = object else {
            return;
        };
        if dict.get_name("Type") == Some("Catalog") {
            dict.remove("Outlines");
            dict.remove("OpenAction");
            dict.remove("Dests");
            dict.remove("Names");
            dict.remove("PageMode");
            dict.remove("StructTreeRoot");
            dict.remove("PageLabels");
        }
        if dict.get_name("Type") == Some("Pages") {
            dict.remove("MediaBox");
            dict.remove("CropBox");
            dict.remove("Rotate");
            dict.remove("Resources");
            return;
        }
        let Some(page) = page_by_source.get(&orig) else {
            return;
        };
        if dict.get_name("Type") != Some("Page") {
            return;
        }
        dict.insert("MediaBox", box_array(page.media_box));
        if page.crop_box != page.media_box {
            dict.insert("CropBox", box_array(page.crop_box));
        } else {
            dict.remove("CropBox");
        }
        if page.rotate != 0 {
            dict.insert("Rotate", PdfObject::Integer(page.rotate as i64));
        } else {
            dict.remove("Rotate");
        }
        let resources = rewrite_references(PdfObject::Dictionary(page.resources.clone()), &remap);
        dict.insert("Resources", resources);
    };
    let (mut objects, new_root, info_number) =
        rewrite_document_objects_with_remap(reader, &remap, &mut normalize_pages)?;
    retain_reachable_linearized_objects(&mut objects, new_root, info_number);
    let page_groups = source_plan.output_page_groups(&remap)?;
    let shared_objects = source_plan.output_shared_objects(&remap)?;
    let opening_objects = source_plan.output_opening_objects(&remap)?;
    let layout =
        LinearizedObjectLayout::new(&objects, opening_objects, &page_groups, shared_objects)?;

    let max_regular = objects.iter().map(|obj| obj.number).max().unwrap_or(0);
    let linearization_number = max_regular + 1;
    let front_xref_number = max_regular + 2;
    let hint_number = max_regular + 3;
    let main_xref_number = max_regular + 4;
    let max_number = main_xref_number;

    let version = if version_lt_1_5(reader.version()) {
        "1.5".to_string()
    } else {
        reader.version().to_string()
    };
    let header = pdf_header(&version);
    let placeholder_params = LinearizationParams {
        file_length: 0,
        hint_offset: 0,
        hint_length: 0,
        first_page_object: page_groups[0].objects[0],
        first_page_end: 0,
        page_count: pages.len(),
        main_xref_offset: 0,
    };
    let linearization_placeholder =
        build_linearization_dictionary(linearization_number, &placeholder_params);
    let front_xref_offset = header.len() + linearization_placeholder.len();

    let mut state = LinearizedBuildState {
        front_xref_len: 256,
        hint_len: 128,
    };

    for _ in 0..30 {
        let positions = compute_linearized_positions(&layout, &state, front_xref_offset);
        let hint_bytes = build_hint_stream(hint_number, &page_groups, &positions)?;

        let mut front_entries = positions.front_xref_entries.clone();
        front_entries.push((
            linearization_number,
            XrefEntryOut::Uncompressed {
                offset: header.len(),
            },
        ));
        front_entries.push((
            front_xref_number,
            XrefEntryOut::Uncompressed {
                offset: front_xref_offset,
            },
        ));
        front_entries.push((
            hint_number,
            XrefEntryOut::Uncompressed {
                offset: positions.hint_offset,
            },
        ));

        let mut front_dict = trailer_id_dict(reader.first_file_id());
        front_dict.insert(
            "Root",
            PdfObject::Reference {
                number: new_root,
                generation: 0,
            },
        );
        if let Some(info) = info_number {
            front_dict.insert(
                "Info",
                PdfObject::Reference {
                    number: info,
                    generation: 0,
                },
            );
        }
        front_dict.insert(
            "Prev",
            PdfObject::Integer(positions.main_xref_offset as i64),
        );
        let front_index = xref_index_ranges(front_entries.iter().map(|(n, _)| *n));
        let front_xref_bytes = build_xref_stream_with_index(
            front_xref_number,
            max_number as usize + 1,
            &mut front_entries,
            &front_dict,
            Some(front_index),
        )?;

        let mut main_entries = vec![
            (0, XrefEntryOut::Free),
            (
                main_xref_number,
                XrefEntryOut::Uncompressed {
                    offset: positions.main_xref_offset,
                },
            ),
        ];
        let main_index = xref_index_ranges(main_entries.iter().map(|(n, _)| *n));
        let main_dict = trailer_id_dict(reader.first_file_id());
        let main_xref_bytes = build_xref_stream_with_index(
            main_xref_number,
            max_number as usize + 1,
            &mut main_entries,
            &main_dict,
            Some(main_index),
        )?;

        if front_xref_bytes.len() > state.front_xref_len || hint_bytes.len() > state.hint_len {
            state.front_xref_len = state.front_xref_len.max(front_xref_bytes.len() + 64);
            state.hint_len = state.hint_len.max(hint_bytes.len() + 64);
            continue;
        }

        let startxref = build_startxref(front_xref_offset);
        let file_length = positions.main_xref_offset + main_xref_bytes.len() + startxref.len();
        let params = LinearizationParams {
            file_length,
            hint_offset: positions.hint_offset,
            hint_length: state.hint_len,
            first_page_object: page_groups[0].objects[0],
            first_page_end: positions.first_page_end,
            page_count: pages.len(),
            main_xref_offset: positions.main_xref_offset,
        };
        let linearization_dict = build_linearization_dictionary(linearization_number, &params);
        debug_assert_eq!(linearization_dict.len(), linearization_placeholder.len());

        let output = assemble_linearized_output(LinearizedOutputParts {
            header: &header,
            linearization_dict: &linearization_dict,
            front_xref: &front_xref_bytes,
            front_xref_reserved_len: state.front_xref_len,
            layout: &layout,
            positions: &positions,
            hint: &hint_bytes,
            hint_reserved_len: state.hint_len,
            main_xref: &main_xref_bytes,
            startxref: &startxref,
        });

        return Ok(output);
    }

    Err(OxideError::UnsupportedFeature(
        "linearize: offset layout did not stabilize".to_string(),
    ))
}

fn ensure_linearization_supported(
    doc: &PdfDocument,
    pages: &[crate::document::PdfPage],
) -> Result<()> {
    let reader = doc.reader();
    for page in pages {
        let page_obj = reader.get_object(page.object_number, page.generation_number)?;
        if let Some(dict) = page_obj.as_dict() {
            if dict.contains_key("Thumb") {
                return Err(OxideError::UnsupportedFeature(
                    "linearize: qpdf-valid output for page thumbnails is still deferred"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

struct LinearizationSourcePlan {
    opening_objects: Vec<u32>,
    page_groups: Vec<LinearizedPageGroup>,
    shared_objects: Vec<u32>,
    remap: HashMap<u32, u32>,
}

impl LinearizationSourcePlan {
    fn output_opening_objects(&self, remap: &HashMap<u32, u32>) -> Result<Vec<u32>> {
        remap_numbers(&self.opening_objects, remap)
    }

    fn output_shared_objects(&self, remap: &HashMap<u32, u32>) -> Result<Vec<u32>> {
        remap_numbers(&self.shared_objects, remap)
    }

    fn output_page_groups(&self, remap: &HashMap<u32, u32>) -> Result<Vec<LinearizedPageGroup>> {
        self.page_groups
            .iter()
            .map(|group| {
                Ok(LinearizedPageGroup {
                    objects: remap_numbers(&group.objects, remap)?,
                    shared_identifiers: group.shared_identifiers.clone(),
                })
            })
            .collect()
    }
}

fn remap_numbers(numbers: &[u32], remap: &HashMap<u32, u32>) -> Result<Vec<u32>> {
    numbers
        .iter()
        .map(|number| {
            remap.get(number).copied().ok_or_else(|| {
                OxideError::MalformedPdf(format!(
                    "linearize: object {number} was not assigned an output number"
                ))
            })
        })
        .collect()
}

fn build_linearization_source_plan(
    reader: &PdfReader,
    pages: &[crate::document::PdfPage],
) -> Result<LinearizationSourcePlan> {
    let object_map = original_object_map(reader);
    let mut closures: Vec<BTreeSet<u32>> = Vec::with_capacity(pages.len());
    let mut object_pages: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();

    for (idx, page) in pages.iter().enumerate() {
        let mut roots = BTreeSet::new();
        roots.insert(page.object_number);

        if let Some(page_obj) = object_map.get(&page.object_number) {
            collect_page_local_references(page_obj, &mut roots);
        }

        collect_references_into_set(&PdfObject::Dictionary(page.resources.clone()), &mut roots);
        for (content, _) in &page.contents {
            roots.insert(*content);
        }

        let top_pages = BTreeSet::from([page.object_number]);
        let mut closure = dependency_closure_for_linearization(&roots, &object_map, &top_pages);
        closure.insert(page.object_number);
        closure.retain(|number| object_map.contains_key(number));
        for &number in &closure {
            object_pages.entry(number).or_default().insert(idx);
        }
        closures.push(closure);
    }

    let root = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("cannot linearize: trailer is missing /Root".to_string())
    })?;
    let opening_objects = build_opening_source_objects(root.0, &object_map);
    let opening_set: BTreeSet<u32> = opening_objects.iter().copied().collect();

    let mut assigned = BTreeSet::new();
    let first_page_number = pages[0].object_number;
    let first_group: BTreeSet<u32> = closures[0]
        .iter()
        .copied()
        .filter(|number| !opening_set.contains(number))
        .collect();
    let first_objects = ordered_group_with_first(first_page_number, &first_group);
    assigned.extend(first_objects.iter().copied());

    let mut source_page_groups = Vec::with_capacity(pages.len());
    source_page_groups.push(LinearizedPageGroup {
        objects: first_objects.clone(),
        shared_identifiers: Vec::new(),
    });

    let mut shared_objects: Vec<u32> = object_pages
        .iter()
        .filter_map(|(&number, users)| {
            if users.len() > 1 && !assigned.contains(&number) && !opening_set.contains(&number) {
                Some(number)
            } else {
                None
            }
        })
        .collect();
    shared_objects.sort_unstable();

    let mut shared_index: HashMap<u32, usize> = HashMap::new();
    for (idx, &number) in first_objects.iter().enumerate() {
        shared_index.insert(number, idx);
    }
    for &number in &shared_objects {
        let idx = shared_index.len();
        shared_index.insert(number, idx);
    }

    for (idx, page) in pages.iter().enumerate().skip(1) {
        let page_number = page.object_number;
        let mut owned = BTreeSet::new();
        owned.insert(page_number);
        for &number in &closures[idx] {
            let Some(users) = object_pages.get(&number) else {
                continue;
            };
            if users.len() == 1 && users.contains(&idx) && !opening_set.contains(&number) {
                owned.insert(number);
            }
        }
        let objects = ordered_group_with_first(page_number, &owned);
        assigned.extend(objects.iter().copied());

        let mut shared_identifiers: Vec<usize> = closures[idx]
            .iter()
            .filter(|number| {
                object_pages
                    .get(number)
                    .map(|users| users.len() > 1 && !opening_set.contains(number))
                    .unwrap_or(false)
            })
            .filter_map(|number| shared_index.get(number).copied())
            .collect();
        shared_identifiers.sort_unstable();
        shared_identifiers.dedup();

        source_page_groups.push(LinearizedPageGroup {
            objects,
            shared_identifiers,
        });
    }

    assigned.extend(shared_objects.iter().copied());
    assigned.extend(opening_objects.iter().copied());

    let mut remap = HashMap::new();
    let mut next = 1u32;
    for group in &source_page_groups {
        for &number in &group.objects {
            assign_linearized_number(&mut remap, &mut next, number);
        }
    }
    for &number in &shared_objects {
        assign_linearized_number(&mut remap, &mut next, number);
    }

    let mut remaining: Vec<u32> = reader
        .object_ids()
        .into_iter()
        .map(|(number, _)| number)
        .filter(|number| object_map.contains_key(number))
        .filter(|number| !assigned.contains(number))
        .collect();
    remaining.sort_unstable();
    remaining.dedup();
    for number in remaining {
        assign_linearized_number(&mut remap, &mut next, number);
    }
    for &number in &opening_objects {
        assign_linearized_number(&mut remap, &mut next, number);
    }

    Ok(LinearizationSourcePlan {
        opening_objects,
        page_groups: source_page_groups,
        shared_objects,
        remap,
    })
}

fn original_object_map(reader: &PdfReader) -> HashMap<u32, PdfObject> {
    let mut object_map = HashMap::new();
    for (number, generation) in reader.object_ids() {
        let Ok(object) = reader.get_object(number, generation) else {
            continue;
        };
        if let PdfObject::Stream { dict, .. } = &object {
            if dict.get_name("Type") == Some("XRef") {
                continue;
            }
        }
        object_map.insert(number, object);
    }
    object_map
}

fn build_opening_source_objects(root: u32, object_map: &HashMap<u32, PdfObject>) -> Vec<u32> {
    let mut roots = BTreeSet::new();
    if let Some(PdfObject::Dictionary(dict)) = object_map.get(&root) {
        for key in ["ViewerPreferences", "Threads", "AcroForm"] {
            if let Some(value) = dict.get(key) {
                collect_references_into_set(value, &mut roots);
            }
        }
    }
    let closure = dependency_closure_all(&roots, object_map);
    let mut out = vec![root];
    out.extend(closure.into_iter().filter(|number| *number != root));
    out
}

fn assign_linearized_number(remap: &mut HashMap<u32, u32>, next: &mut u32, number: u32) {
    remap.entry(number).or_insert_with(|| {
        let assigned = *next;
        *next += 1;
        assigned
    });
}

fn retain_reachable_linearized_objects(
    objects: &mut Vec<OutputObject>,
    root: u32,
    info: Option<u32>,
) {
    let object_map: HashMap<u32, PdfObject> = objects
        .iter()
        .map(|obj| (obj.number, obj.object.clone()))
        .collect();
    let mut reachable = BTreeSet::new();
    let mut stack = vec![root];
    if let Some(info) = info {
        stack.push(info);
    }
    while let Some(number) = stack.pop() {
        if !reachable.insert(number) {
            continue;
        }
        let Some(object) = object_map.get(&number) else {
            continue;
        };
        let mut refs = Vec::new();
        collect_references(object, &mut refs);
        for reference in refs {
            if !reachable.contains(&reference) {
                stack.push(reference);
            }
        }
    }
    objects.retain(|obj| reachable.contains(&obj.number));
}

#[derive(Debug, Clone)]
struct LinearizedPageGroup {
    objects: Vec<u32>,
    shared_identifiers: Vec<usize>,
}

fn collect_page_local_references(object: &PdfObject, out: &mut BTreeSet<u32>) {
    match object {
        PdfObject::Dictionary(dict) => {
            let is_page = dict.get_name("Type") == Some("Page");
            let is_pages = dict.get_name("Type") == Some("Pages");
            if is_page {
                for (key, value) in dict.iter() {
                    if key == "Parent" || key == "Resources" || key == "Thumb" {
                        continue;
                    }
                    collect_references_into_set(value, out);
                }
                return;
            }
            for (key, value) in dict.iter() {
                if is_pages && (key == "Parent" || key == "Kids") {
                    continue;
                }
                collect_references_into_set(value, out);
            }
        }
        other => collect_references_into_set(other, out),
    }
}

fn dependency_closure_for_linearization(
    roots: &BTreeSet<u32>,
    object_map: &HashMap<u32, PdfObject>,
    top_pages: &BTreeSet<u32>,
) -> BTreeSet<u32> {
    let mut closure = BTreeSet::new();
    let mut stack: Vec<u32> = roots.iter().copied().collect();
    while let Some(number) = stack.pop() {
        if !closure.insert(number) {
            continue;
        }
        let Some(object) = object_map.get(&number) else {
            continue;
        };
        if !top_pages.contains(&number)
            && matches!(
                object,
                PdfObject::Dictionary(dict) if dict.get_name("Type") == Some("Page")
            )
        {
            continue;
        }
        let mut refs = BTreeSet::new();
        collect_page_local_references(object, &mut refs);
        for reference in refs {
            if !closure.contains(&reference) {
                stack.push(reference);
            }
        }
    }
    closure
}

fn dependency_closure_all(
    roots: &BTreeSet<u32>,
    object_map: &HashMap<u32, PdfObject>,
) -> BTreeSet<u32> {
    let mut closure = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack: Vec<u32> = roots.iter().copied().collect();
    while let Some(number) = stack.pop() {
        if !visited.insert(number) {
            continue;
        }
        let Some(object) = object_map.get(&number) else {
            continue;
        };
        if matches!(
            object,
            PdfObject::Dictionary(dict) if dict.get_name("Type") == Some("Page")
        ) {
            continue;
        }
        closure.insert(number);
        let mut refs = BTreeSet::new();
        collect_references_into_set(object, &mut refs);
        for reference in refs {
            if !closure.contains(&reference) {
                stack.push(reference);
            }
        }
    }
    closure
}

fn collect_references_into_set(object: &PdfObject, out: &mut BTreeSet<u32>) {
    let mut refs = Vec::new();
    collect_references(object, &mut refs);
    out.extend(refs);
}

fn ordered_group_with_first(first: u32, group: &BTreeSet<u32>) -> Vec<u32> {
    let mut out = Vec::with_capacity(group.len());
    if group.contains(&first) {
        out.push(first);
    }
    out.extend(group.iter().copied().filter(|&number| number != first));
    out
}

struct LinearizedObjectLayout {
    opening_objects: Vec<u32>,
    page_groups: Vec<Vec<u32>>,
    shared_objects: Vec<u32>,
    leftovers: Vec<u32>,
    object_bytes: HashMap<u32, Vec<u8>>,
}

impl LinearizedObjectLayout {
    fn new(
        objects: &[OutputObject],
        opening_objects: Vec<u32>,
        page_groups: &[LinearizedPageGroup],
        shared_objects: Vec<u32>,
    ) -> Result<Self> {
        let mut object_bytes = HashMap::new();
        for object in objects {
            object_bytes.insert(
                object.number,
                indirect_object_bytes(object.number, &object.object),
            );
        }

        let mut assigned = BTreeSet::new();
        for &number in &opening_objects {
            ensure_layout_object(&object_bytes, number)?;
            assigned.insert(number);
        }

        let mut page_group_numbers = Vec::with_capacity(page_groups.len());
        for group in page_groups {
            let mut numbers = Vec::new();
            for &number in &group.objects {
                ensure_layout_object(&object_bytes, number)?;
                if assigned.insert(number) {
                    numbers.push(number);
                }
            }
            page_group_numbers.push(numbers);
        }

        for &number in &shared_objects {
            ensure_layout_object(&object_bytes, number)?;
            assigned.insert(number);
        }

        let mut leftovers: Vec<u32> = objects
            .iter()
            .map(|object| object.number)
            .filter(|number| !assigned.contains(number))
            .collect();
        leftovers.sort_unstable();

        Ok(Self {
            opening_objects,
            page_groups: page_group_numbers,
            shared_objects,
            leftovers,
            object_bytes,
        })
    }

    fn object_len(&self, number: u32) -> usize {
        self.object_bytes
            .get(&number)
            .map(|bytes| bytes.len())
            .unwrap_or(0)
    }
}

fn ensure_layout_object(object_bytes: &HashMap<u32, Vec<u8>>, number: u32) -> Result<()> {
    if object_bytes.contains_key(&number) {
        Ok(())
    } else {
        Err(OxideError::MalformedPdf(format!(
            "linearize: planned object {number} was not copied"
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinearizedBuildState {
    front_xref_len: usize,
    hint_len: usize,
}

struct LinearizedPositions {
    opening_offsets: Vec<(u32, usize)>,
    hint_offset: usize,
    page_offsets: Vec<Vec<(u32, usize)>>,
    shared_offsets: Vec<(u32, usize)>,
    page_lengths: Vec<usize>,
    first_page_end: usize,
    leftover_offsets: Vec<(u32, usize)>,
    main_xref_offset: usize,
    front_xref_entries: Vec<(u32, XrefEntryOut)>,
}

fn compute_linearized_positions(
    layout: &LinearizedObjectLayout,
    state: &LinearizedBuildState,
    front_xref_offset: usize,
) -> LinearizedPositions {
    let mut pos = front_xref_offset + state.front_xref_len;

    let mut opening_offsets = Vec::with_capacity(layout.opening_objects.len());
    for &number in &layout.opening_objects {
        opening_offsets.push((number, pos));
        pos += layout.object_len(number);
    }

    let hint_offset = pos;
    pos += state.hint_len;

    let mut page_offsets = Vec::with_capacity(layout.page_groups.len());
    let mut page_lengths = Vec::with_capacity(layout.page_groups.len());
    let mut first_page_end = pos;
    for (idx, group) in layout.page_groups.iter().enumerate() {
        let start = pos;
        let mut offsets = Vec::with_capacity(group.len());
        for &number in group {
            offsets.push((number, pos));
            pos += layout.object_len(number);
        }
        if idx == 0 {
            first_page_end = pos;
        }
        page_lengths.push(pos - start);
        page_offsets.push(offsets);
    }

    let mut shared_offsets = Vec::with_capacity(layout.shared_objects.len());
    for &number in &layout.shared_objects {
        shared_offsets.push((number, pos));
        pos += layout.object_len(number);
    }

    let mut leftover_offsets = Vec::with_capacity(layout.leftovers.len());
    for &number in &layout.leftovers {
        leftover_offsets.push((number, pos));
        pos += layout.object_len(number);
    }

    let main_xref_offset = pos;
    let mut front_xref_entries = Vec::new();
    for (number, offset) in opening_offsets
        .iter()
        .chain(page_offsets.iter().flatten())
        .chain(shared_offsets.iter())
        .chain(leftover_offsets.iter())
    {
        front_xref_entries.push((*number, XrefEntryOut::Uncompressed { offset: *offset }));
    }

    LinearizedPositions {
        opening_offsets,
        hint_offset,
        page_offsets,
        shared_offsets,
        page_lengths,
        first_page_end,
        leftover_offsets,
        main_xref_offset,
        front_xref_entries,
    }
}

#[derive(Debug, Clone, Copy)]
struct LinearizationParams {
    file_length: usize,
    hint_offset: usize,
    hint_length: usize,
    first_page_object: u32,
    first_page_end: usize,
    page_count: usize,
    main_xref_offset: usize,
}

fn build_linearization_dictionary(number: u32, params: &LinearizationParams) -> Vec<u8> {
    format!(
        "{number} 0 obj\n<< /Linearized 1 /L {file_length:>20} /H [ {hint_offset:>20} {hint_length:>20} ] /O {first_page_object:>20} /E {first_page_end:>20} /N {page_count:>20} /T {main_xref_offset:>20} >>\nendobj\n",
        file_length = params.file_length,
        hint_offset = params.hint_offset,
        hint_length = params.hint_length,
        first_page_object = params.first_page_object,
        first_page_end = params.first_page_end,
        page_count = params.page_count,
        main_xref_offset = params.main_xref_offset,
    )
    .into_bytes()
}

fn build_hint_stream(
    number: u32,
    page_groups: &[LinearizedPageGroup],
    positions: &LinearizedPositions,
) -> Result<Vec<u8>> {
    let raw = build_hint_stream_data(page_groups, positions)?;
    let shared_offset = raw.shared_table_offset;
    let compressed = crate::filters::flate_encode(&raw.bytes, 9);

    let mut dict = PdfDictionary::empty();
    dict.insert("Filter", PdfObject::Name("FlateDecode".to_string()));
    dict.insert("S", PdfObject::Integer(shared_offset as i64));
    dict.insert("Length", PdfObject::Integer(compressed.len() as i64));

    let mut out = Vec::new();
    out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    serialize_dictionary(&dict, &mut out);
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    Ok(out)
}

struct HintStreamData {
    bytes: Vec<u8>,
    shared_table_offset: usize,
}

fn build_hint_stream_data(
    page_groups: &[LinearizedPageGroup],
    positions: &LinearizedPositions,
) -> Result<HintStreamData> {
    let page_object_counts: Vec<usize> = page_groups.iter().map(|g| g.objects.len()).collect();
    let page_lengths = &positions.page_lengths;
    let shared_counts: Vec<usize> = page_groups
        .iter()
        .map(|g| g.shared_identifiers.len())
        .collect();

    let min_objects = *page_object_counts.iter().min().unwrap_or(&0);
    let max_objects_delta = page_object_counts
        .iter()
        .map(|count| count - min_objects)
        .max()
        .unwrap_or(0);
    let min_page_length = *page_lengths.iter().min().unwrap_or(&0);
    let max_page_length_delta = page_lengths
        .iter()
        .map(|length| length - min_page_length)
        .max()
        .unwrap_or(0);
    let min_content_offset = 0usize;
    let min_content_length = min_page_length;
    let max_content_length_delta = max_page_length_delta;
    let max_shared_count = *shared_counts.iter().max().unwrap_or(&0);
    let nshared_first_page = page_groups[0].objects.len();
    let nshared_total = nshared_first_page + positions.shared_offsets.len();

    let nbits_objects = bits_required(max_objects_delta as u64);
    let nbits_page_length = bits_required(max_page_length_delta as u64);
    let nbits_content_offset = 0usize;
    let nbits_content_length = bits_required(max_content_length_delta as u64);
    let nbits_shared_count = bits_required(max_shared_count as u64);
    let nbits_shared_identifier = bits_required(nshared_total as u64).max(1);
    let nbits_shared_numerator = 0usize;
    let shared_denominator = 4usize;

    let mut page_table = Vec::new();
    write_u32(&mut page_table, min_objects)?;
    write_u32(&mut page_table, positions.hint_offset)?;
    write_u16(&mut page_table, nbits_objects)?;
    write_u32(&mut page_table, min_page_length)?;
    write_u16(&mut page_table, nbits_page_length)?;
    write_u32(&mut page_table, min_content_offset)?;
    write_u16(&mut page_table, nbits_content_offset)?;
    write_u32(&mut page_table, min_content_length)?;
    write_u16(&mut page_table, nbits_content_length)?;
    write_u16(&mut page_table, nbits_shared_count)?;
    write_u16(&mut page_table, nbits_shared_identifier)?;
    write_u16(&mut page_table, nbits_shared_numerator)?;
    write_u16(&mut page_table, shared_denominator)?;

    let mut page_bits = BitWriter::new();
    for count in &page_object_counts {
        page_bits.write(*count - min_objects, nbits_objects);
    }
    page_bits.align_byte();
    for length in page_lengths {
        page_bits.write(*length - min_page_length, nbits_page_length);
    }
    page_bits.align_byte();
    for group in page_groups {
        page_bits.write(group.shared_identifiers.len(), nbits_shared_count);
    }
    page_bits.align_byte();
    for group in page_groups {
        for &identifier in &group.shared_identifiers {
            page_bits.write(identifier, nbits_shared_identifier);
            page_bits.write(0, nbits_shared_numerator);
        }
    }
    page_bits.align_byte();
    for _ in page_groups {
        page_bits.write(0, nbits_content_offset);
    }
    page_bits.align_byte();
    for length in page_lengths {
        page_bits.write(*length - min_content_length, nbits_content_length);
    }
    page_bits.align_byte();
    page_table.extend_from_slice(&page_bits.finish());

    let shared_table_offset = page_table.len();
    let mut shared_lengths: Vec<usize> = Vec::with_capacity(nshared_total);
    if let Some(first_page_offsets) = positions.page_offsets.first() {
        for (idx, (_, start)) in first_page_offsets.iter().enumerate() {
            let end = if idx + 1 < first_page_offsets.len() {
                first_page_offsets[idx + 1].1
            } else {
                positions.first_page_end
            };
            shared_lengths.push(end - start);
        }
    }
    for (idx, (_, start)) in positions.shared_offsets.iter().enumerate() {
        let end = if idx + 1 < positions.shared_offsets.len() {
            positions.shared_offsets[idx + 1].1
        } else if let Some((_, offset)) = positions.leftover_offsets.first() {
            *offset
        } else {
            positions.main_xref_offset
        };
        shared_lengths.push(end - start);
    }

    let min_group_length = *shared_lengths.iter().min().unwrap_or(&0);
    let max_group_delta = shared_lengths
        .iter()
        .map(|length| length - min_group_length)
        .max()
        .unwrap_or(0);
    let nbits_group_length = bits_required(max_group_delta as u64);

    let mut bytes = page_table;
    let first_shared_obj = positions
        .shared_offsets
        .first()
        .map(|(number, _)| *number as usize)
        .unwrap_or(0);
    let hint_len = positions
        .page_offsets
        .first()
        .and_then(|offsets| offsets.first())
        .map(|(_, first_page_offset)| first_page_offset.saturating_sub(positions.hint_offset))
        .unwrap_or(0);
    let first_shared_offset = if positions.shared_offsets.is_empty() {
        0
    } else {
        positions.shared_offsets[0].1.saturating_sub(hint_len)
    };
    write_u32(&mut bytes, first_shared_obj)?;
    write_u32(&mut bytes, first_shared_offset)?;
    write_u32(&mut bytes, nshared_first_page)?;
    write_u32(&mut bytes, nshared_total)?;
    write_u16(&mut bytes, 0)?;
    write_u32(&mut bytes, min_group_length)?;
    write_u16(&mut bytes, nbits_group_length)?;

    let mut shared_bits = BitWriter::new();
    for length in shared_lengths {
        shared_bits.write(length - min_group_length, nbits_group_length);
    }
    shared_bits.align_byte();
    for _ in 0..nshared_total {
        shared_bits.write(0, 1);
    }
    shared_bits.align_byte();
    bytes.extend_from_slice(&shared_bits.finish());
    while (bytes.len() - shared_table_offset) % 4 != 0 {
        bytes.push(0);
    }

    Ok(HintStreamData {
        bytes,
        shared_table_offset,
    })
}

fn indirect_object_bytes(number: u32, object: &PdfObject) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    serialize_object(object, &mut out);
    out.extend_from_slice(b"\nendobj\n");
    out
}

struct LinearizedOutputParts<'a> {
    header: &'a [u8],
    linearization_dict: &'a [u8],
    front_xref: &'a [u8],
    front_xref_reserved_len: usize,
    layout: &'a LinearizedObjectLayout,
    positions: &'a LinearizedPositions,
    hint: &'a [u8],
    hint_reserved_len: usize,
    main_xref: &'a [u8],
    startxref: &'a [u8],
}

fn assemble_linearized_output(parts: LinearizedOutputParts<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(parts.header);
    out.extend_from_slice(parts.linearization_dict);
    out.extend_from_slice(parts.front_xref);
    out.extend(std::iter::repeat_n(
        b' ',
        parts.front_xref_reserved_len - parts.front_xref.len(),
    ));

    for (number, _) in &parts.positions.opening_offsets {
        out.extend_from_slice(&parts.layout.object_bytes[number]);
    }
    out.extend_from_slice(parts.hint);
    out.extend(std::iter::repeat_n(
        b' ',
        parts.hint_reserved_len - parts.hint.len(),
    ));
    for group in &parts.positions.page_offsets {
        for (number, _) in group {
            out.extend_from_slice(&parts.layout.object_bytes[number]);
        }
    }
    for (number, _) in &parts.positions.shared_offsets {
        out.extend_from_slice(&parts.layout.object_bytes[number]);
    }
    for (number, _) in &parts.positions.leftover_offsets {
        out.extend_from_slice(&parts.layout.object_bytes[number]);
    }
    out.extend_from_slice(parts.main_xref);
    out.extend_from_slice(parts.startxref);
    out
}

fn trailer_id_dict(id: Option<Vec<u8>>) -> PdfDictionary {
    let mut dict = PdfDictionary::empty();
    if let Some(id) = id {
        dict.insert(
            "ID",
            PdfObject::Array(vec![PdfObject::String(id.clone()), PdfObject::String(id)]),
        );
    }
    dict
}

fn xref_index_ranges(numbers: impl Iterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut numbers: Vec<u32> = numbers.collect();
    numbers.sort_unstable();
    numbers.dedup();

    let mut ranges = Vec::new();
    let mut iter = numbers.into_iter();
    let Some(mut start) = iter.next() else {
        return ranges;
    };
    let mut last = start;
    for number in iter {
        if number == last + 1 {
            last = number;
        } else {
            ranges.push((start, last - start + 1));
            start = number;
            last = number;
        }
    }
    ranges.push((start, last - start + 1));
    ranges
}

fn pdf_header(version: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
    out
}

fn build_startxref(offset: usize) -> Vec<u8> {
    format!("\nstartxref\n{offset}\n%%EOF\n").into_bytes()
}

fn write_u32(out: &mut Vec<u8>, value: usize) -> Result<()> {
    let value = u32::try_from(value)
        .map_err(|_| OxideError::UnsupportedFeature("linearize: hint value exceeds u32".into()))?;
    out.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn write_u16(out: &mut Vec<u8>, value: usize) -> Result<()> {
    let value = u16::try_from(value).map_err(|_| {
        OxideError::UnsupportedFeature("linearize: hint bit width exceeds u16".into())
    })?;
    out.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn bits_required(value: u64) -> usize {
    if value == 0 {
        0
    } else {
        (64 - value.leading_zeros() as usize).max(1)
    }
}

struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            used: 0,
        }
    }

    fn write(&mut self, value: usize, width: usize) {
        if width == 0 {
            return;
        }
        for shift in (0..width).rev() {
            let bit = ((value >> shift) & 1) as u8;
            self.current = (self.current << 1) | bit;
            self.used += 1;
            if self.used == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
    }

    fn align_byte(&mut self) {
        if self.used == 0 {
            return;
        }
        self.current <<= 8 - self.used;
        self.bytes.push(self.current);
        self.current = 0;
        self.used = 0;
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.current <<= 8 - self.used;
            self.bytes.push(self.current);
        }
        self.bytes
    }
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

    // --- Modern writer unit tests (xref streams + object streams) ---

    #[test]
    fn byte_width_fits_values() {
        assert_eq!(byte_width(0), 1);
        assert_eq!(byte_width(255), 1);
        assert_eq!(byte_width(256), 2);
        assert_eq!(byte_width(65535), 2);
        assert_eq!(byte_width(65536), 3);
        assert_eq!(byte_width(0x00FF_FFFF), 3);
        assert_eq!(byte_width(0x0100_0000), 4);
    }

    #[test]
    fn write_be_field_is_big_endian_fixed_width() {
        let mut v = Vec::new();
        write_be_field(&mut v, 0x0102, 3);
        assert_eq!(v, vec![0x00, 0x01, 0x02]);
        let mut v2 = Vec::new();
        write_be_field(&mut v2, 1, 1);
        assert_eq!(v2, vec![0x01]);
    }

    #[test]
    fn objstm_header_offsets_match_reader_format() {
        // Two tiny objects packed: the header must be "num reloffset" pairs and
        // /First must equal the header length; the reader's parser must recover
        // both objects with their numbers.
        let o5 = OutputObject {
            number: 5,
            object: PdfObject::Integer(42),
        };
        let o7 = OutputObject {
            number: 7,
            object: PdfObject::Boolean(true),
        };
        let map: std::collections::HashMap<u32, &OutputObject> =
            [(5u32, &o5), (7u32, &o7)].into_iter().collect();
        let (decoded, off) = build_objstm_body(&[5, 7], &map).unwrap();
        let parsed =
            crate::reader::parse_object_stream_data(&decoded, 2, off.first_offset, None).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed.get(&5), Some((0, PdfObject::Integer(42)))));
        assert!(matches!(
            parsed.get(&7),
            Some((1, PdfObject::Boolean(true)))
        ));
    }

    #[test]
    fn xref_stream_entries_parse_back() {
        // Build an xref stream for a known entry set and confirm the reader
        // decodes the right types/fields. We construct the payload directly via
        // the same path build_xref_stream uses, then round-trip through the
        // reader's entry parser.
        let w1 = byte_width(17);
        let w2 = byte_width(3);
        let mut payload = Vec::new();
        // obj 0: free (0,0,0)
        write_be_field(&mut payload, 0, 1);
        write_be_field(&mut payload, 0, w1);
        write_be_field(&mut payload, 0, w2);
        // obj 1: uncompressed at offset 17 (1,17,0)
        write_be_field(&mut payload, 1, 1);
        write_be_field(&mut payload, 17, w1);
        write_be_field(&mut payload, 0, w2);
        // obj 2: in objstm 1 at index 3 (2,1,3)
        write_be_field(&mut payload, 2, 1);
        write_be_field(&mut payload, 1, w1);
        write_be_field(&mut payload, 3, w2);

        let mut d = PdfDictionary::empty();
        d.insert(
            "W",
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Integer(w1 as i64),
                PdfObject::Integer(w2 as i64),
            ]),
        );
        d.insert("Size", PdfObject::Integer(3));
        let parsed = crate::reader::parse_xref_stream_entries(&d, &payload).unwrap();
        assert!(parsed.iter().any(|(n, _, e)| *n == 1
            && matches!(e, crate::reader::XrefEntry::Uncompressed { offset } if *offset == 17)));
        assert!(parsed.iter().any(|(n, _, e)| *n == 2
            && matches!(e, crate::reader::XrefEntry::Compressed { stream_obj, index } if *stream_obj == 1 && *index == 3)));

        // And the full build_xref_stream emits a parseable /Type /XRef object.
        let mut entries = vec![
            (0u32, XrefEntryOut::Free),
            (1u32, XrefEntryOut::Uncompressed { offset: 17 }),
            (
                2u32,
                XrefEntryOut::Compressed {
                    objstm: 1,
                    index: 3,
                },
            ),
        ];
        let obj_text = build_xref_stream(3, 3, &mut entries, &PdfDictionary::empty()).unwrap();
        let s = String::from_utf8_lossy(&obj_text);
        assert!(s.contains("/Type /XRef") || s.contains("/Type/XRef"));
        assert!(s.contains("/W ["));
        assert!(s.contains("/Index ["));
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
