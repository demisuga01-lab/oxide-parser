use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::RwLock;

use crate::crypto::{
    compute_encryption_key, decrypt_string, derive_v5_file_key_from_owner,
    derive_v5_file_key_from_user, verify_user_password, verify_v5_owner_password, verify_v5_perms,
    verify_v5_user_password, CryptMethod, EncryptionInfo,
};
use crate::error::{OxideError, Result};
use crate::filters::decode_stream_from_dict;
use crate::object::{PdfDictionary, PdfObject};
use crate::parser::{ParserResolver, PdfParser};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XrefEntry {
    Free,
    Uncompressed { offset: usize },
    Compressed { stream_obj: u32, index: u32 },
}

/// Active decryption state for a Standard-Security-Handler encrypted PDF.
///
/// Built once during [`PdfReader::from_bytes_with_password`] after the user
/// password is verified. Every object read through [`PdfReader::get_object`]
/// has its strings and stream bytes decrypted transparently.
#[derive(Clone, Debug)]
pub struct EncryptionContext {
    /// The file-wide encryption key (5 bytes for 40-bit, 16 bytes for 128-bit,
    /// or 32 bytes for V5/AES-256).
    pub file_key: Vec<u8>,
    /// True when streams and strings are encrypted with AES-128 (`/CFM /AESV2`).
    pub is_aes: bool,
    /// True when this is a V5 (AES-256) document. For V5 the file key is used
    /// directly for every object — no per-object key derivation.
    pub is_v5: bool,
    /// Crypt filter method for ordinary streams (`/StmF`).
    pub stream_method: CryptMethod,
    /// Crypt filter method for strings (`/StrF`).
    pub string_method: CryptMethod,
    /// Crypt filter method for embedded-file streams (`/EFF`).
    pub embedded_file_method: CryptMethod,
    /// Named crypt filters from `/CF`, used by explicit `/Filter /Crypt`
    /// stream filters.
    pub crypt_filters: HashMap<String, CryptMethod>,
    /// Mirrors `/EncryptMetadata`; when false, `/Type /Metadata` streams are
    /// left as plaintext.
    pub encrypt_metadata: bool,
}

type ObjectStreamCache = HashMap<u32, HashMap<u32, (u32, PdfObject)>>;

pub struct PdfReader {
    data: Vec<u8>,
    version: String,
    xref: HashMap<(u32, u16), XrefEntry>,
    trailer: PdfDictionary,
    /// Cache of decoded object streams (`/Type /ObjStm`). Wrapped in an
    /// `RwLock` rather than a `RefCell` so the whole `PdfReader` — and therefore
    /// `ContentEngine` — is `Send + Sync`. This lets a single parsed engine be
    /// shared across rayon threads via `Arc` for parallel page extraction and
    /// rendering instead of cloning/reparsing the PDF per
    /// thread. Reads dominate; the lock is only taken for writing the first time
    /// a given object stream is decoded.
    object_stream_cache: RwLock<ObjectStreamCache>,
    encryption: Option<EncryptionContext>,
}

impl PdfReader {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_path_with_password(path, b"")
    }

    /// Open a PDF from a file path, supplying a user password for encrypted
    /// documents. For non-encrypted PDFs the password is ignored.
    pub fn from_path_with_password(path: impl AsRef<Path>, password: &[u8]) -> Result<Self> {
        Self::from_bytes_with_password(fs::read(path)?, password)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        Self::from_bytes_with_password(data, b"")
    }

    /// Open a PDF from bytes, supplying a user password for encrypted
    /// documents. For non-encrypted PDFs the password is ignored.
    ///
    /// For encrypted PDFs the password is verified against the `/U` entry; the
    /// supplied password is tried first, then the empty password as a fallback
    /// (the most common case in the wild — permission-only encryption). If no
    /// password verifies, [`OxideError::EncryptedPdf`] is returned.
    pub fn from_bytes_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self> {
        let version = parse_header_version(&data)?;
        let startxref = find_startxref(&data)?;
        let mut xref = HashMap::new();
        let mut trailer = None;
        let mut visited = HashSet::new();

        read_xref_chain(&data, startxref, &mut xref, &mut trailer, &mut visited)?;
        repair_uncompressed_xref_offsets(&data, &mut xref);

        let trailer = trailer.ok_or_else(|| {
            OxideError::MalformedPdf("PDF did not contain a trailer dictionary".to_string())
        })?;

        let encryption = setup_encryption(&data, &xref, &trailer, password)?;

        Ok(Self {
            data,
            version,
            xref,
            trailer,
            object_stream_cache: RwLock::new(HashMap::new()),
            encryption,
        })
    }

    /// The active encryption context, if this document is encrypted and was
    /// successfully unlocked.
    pub fn encryption(&self) -> Option<&EncryptionContext> {
        self.encryption.as_ref()
    }

    /// True when the document is encrypted and a decryption context is active.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.is_some()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Total size of the input PDF in bytes (the length of the parsed buffer).
    /// Reported by the `info` tool.
    pub fn file_size(&self) -> usize {
        self.data.len()
    }

    /// The exact original file bytes, as opened. Digital-signature verification
    /// hashes the bytes selected by a signature's `/ByteRange` against these —
    /// it must use the raw bytes, never a re-serialization.
    pub fn file_bytes(&self) -> &[u8] {
        &self.data
    }

    /// The raw, resolved `/Encrypt` dictionary, if the document is encrypted.
    ///
    /// The `/Encrypt` dictionary's own verifier strings (`/O`, `/U`, `/OE`,
    /// `/UE`, `/Perms`) are **not** encrypted (PDF 32000-1 §7.6.1). They must
    /// therefore be read WITHOUT the per-object decryption pass that
    /// [`Self::get_object`] applies — otherwise the reader would AES/RC4-decrypt
    /// the plaintext verifiers and corrupt them (e.g. a 16-byte `/Perms`
    /// decrypts to empty). We parse the `/Encrypt` object straight from the file
    /// bytes, exactly as encryption setup does. Returns `None` for unencrypted
    /// documents and on any parse failure.
    pub fn encrypt_dictionary(&self) -> Option<PdfDictionary> {
        let encrypt = self.trailer.get("Encrypt")?;
        resolve_encrypt_dict(&self.data, &self.xref, encrypt)
            .ok()
            .flatten()
    }

    pub fn trailer(&self) -> &PdfDictionary {
        &self.trailer
    }

    pub fn size(&self) -> Option<i64> {
        self.trailer.get_integer("Size")
    }

    pub fn root_reference(&self) -> Option<(u32, u16)> {
        self.trailer.get_reference("Root")
    }

    /// The first element of the trailer `/ID` array, if present. The PDF
    /// writer copies this into manipulated output so the produced file keeps a
    /// stable identifier derived from a source document.
    pub fn first_file_id(&self) -> Option<Vec<u8>> {
        match self.trailer.get("ID") {
            Some(PdfObject::Array(arr)) => match arr.first() {
                Some(PdfObject::String(bytes)) => Some(bytes.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The trailer `/Info` reference, if present. The document information
    /// dictionary holds metadata (Title, Author, Producer, …); the PDF writer
    /// copies it into rewritten/manipulated output when available.
    pub fn info_reference(&self) -> Option<(u32, u16)> {
        self.trailer.get_reference("Info")
    }

    /// Enumerate every in-use indirect object id `(number, generation)` known
    /// to the cross-reference table, in ascending object-number order.
    ///
    /// Free entries (object 0 and any other `f` entries) are excluded, as are
    /// compressed *container* streams' sub-objects' duplicates — each logical
    /// object appears once. Objects stored inside an object stream
    /// (`XrefEntry::Compressed`) are reported with the generation `0` the xref
    /// stream assigns them, so they can be fetched via [`Self::get_object`].
    ///
    /// This is the enumeration the [`crate::writer`] uses for a faithful
    /// whole-document round-trip (copy every object, identity-renumber, emit a
    /// fresh file). For page-level manipulation (merge/split/extract) the
    /// writer instead walks a dependency closure and never needs this.
    pub fn object_ids(&self) -> Vec<(u32, u16)> {
        let mut ids: Vec<(u32, u16)> = self
            .xref
            .iter()
            .filter(|(_, entry)| !matches!(entry, XrefEntry::Free))
            .map(|((number, generation), _)| (*number, *generation))
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn get_object(&self, number: u32, generation: u16) -> Result<PdfObject> {
        let entry = self
            .xref
            .get(&(number, generation))
            .cloned()
            .ok_or(OxideError::MissingObject { number, generation })?;

        match entry {
            XrefEntry::Free => Err(OxideError::MissingObject { number, generation }),
            XrefEntry::Uncompressed { offset } => {
                let mut parser = PdfParser::with_resolver(&self.data, offset, Some(self))?;
                let parsed = parser.parse_indirect_object()?;
                if parsed.number != number || parsed.generation != generation {
                    return Err(OxideError::MissingObject { number, generation });
                }
                Ok(self.decrypt_object(parsed.object, number, generation))
            }
            XrefEntry::Compressed { stream_obj, index } => {
                // Objects stored inside an object stream are decrypted as part
                // of decrypting the containing ObjStm, so they must NOT be
                // decrypted again here (PDF 32000-1 §7.6.2 note).
                self.ensure_object_stream_cached(stream_obj)?;
                let cache = self
                    .object_stream_cache
                    .read()
                    .expect("object stream cache lock poisoned");
                let objects = cache
                    .get(&stream_obj)
                    .ok_or(OxideError::MissingObject { number, generation })?;
                let (actual_index, object) = objects
                    .get(&number)
                    .ok_or(OxideError::MissingObject { number, generation })?;
                if *actual_index != index {
                    return Err(OxideError::MissingObject { number, generation });
                }
                Ok(object.clone())
            }
        }
    }

    /// Recursively decrypt the strings and stream bytes inside a freshly-parsed
    /// top-level (uncompressed) object.
    ///
    /// No-op when the document is not encrypted, so the non-encrypted code path
    /// is unchanged. Structural cross-reference streams (`/Type /XRef`) are
    /// never encrypted and are left untouched; object streams (`/Type /ObjStm`)
    /// ARE encrypted and are decrypted here (their sub-objects are then parsed
    /// from the already-decrypted bytes and not decrypted again).
    fn decrypt_object(&self, obj: PdfObject, obj_num: u32, gen_num: u16) -> PdfObject {
        let ctx = match &self.encryption {
            None => return obj,
            Some(ctx) => ctx,
        };
        self.decrypt_object_inner(obj, obj_num, gen_num, ctx)
    }

    fn decrypt_object_inner(
        &self,
        obj: PdfObject,
        obj_num: u32,
        gen_num: u16,
        ctx: &EncryptionContext,
    ) -> PdfObject {
        match obj {
            PdfObject::String(bytes) => PdfObject::String(decrypt_bytes_by_method(
                &bytes,
                ctx,
                obj_num,
                gen_num,
                &ctx.string_method,
            )),
            PdfObject::Stream { dict, raw } => {
                match dict.get_name("Type") {
                    // Cross-reference streams are never encrypted.
                    Some("XRef") => PdfObject::Stream { dict, raw },
                    // Metadata streams stay plaintext when /EncryptMetadata is false.
                    Some("Metadata") if !ctx.encrypt_metadata => PdfObject::Stream { dict, raw },
                    _ => {
                        let method = stream_crypt_method(&dict, ctx);
                        let decrypted =
                            decrypt_bytes_by_method(&raw, ctx, obj_num, gen_num, &method);
                        // String values inside the stream dictionary are also
                        // encrypted; decrypt them too.
                        let dict = match self.decrypt_object_inner(
                            PdfObject::Dictionary(dict),
                            obj_num,
                            gen_num,
                            ctx,
                        ) {
                            PdfObject::Dictionary(d) => d,
                            // decrypt_object_inner on a Dictionary always yields
                            // a Dictionary; this arm is unreachable in practice.
                            _ => PdfDictionary::empty(),
                        };
                        PdfObject::Stream {
                            dict,
                            raw: decrypted,
                        }
                    }
                }
            }
            PdfObject::Array(items) => PdfObject::Array(
                items
                    .into_iter()
                    .map(|item| self.decrypt_object_inner(item, obj_num, gen_num, ctx))
                    .collect(),
            ),
            PdfObject::Dictionary(dict) => {
                let mut out = PdfDictionary::empty();
                for (key, value) in dict.iter() {
                    out.insert(
                        key.clone(),
                        self.decrypt_object_inner(value.clone(), obj_num, gen_num, ctx),
                    );
                }
                PdfObject::Dictionary(out)
            }
            // Integers, reals, booleans, names, references, null: unchanged.
            other => other,
        }
    }

    pub fn resolve(&self, object: PdfObject) -> Result<PdfObject> {
        let mut visited = HashSet::new();
        self.resolve_inner(object, &mut visited, 0)
    }

    pub fn get_and_resolve(&self, number: u32, generation: u16) -> Result<PdfObject> {
        let object = self.get_object(number, generation)?;
        self.resolve(object)
    }

    fn resolve_inner(
        &self,
        object: PdfObject,
        visited: &mut HashSet<(u32, u16)>,
        depth: usize,
    ) -> Result<PdfObject> {
        if depth > 64 {
            return Err(OxideError::MalformedPdf(
                "reference resolution exceeded depth limit".to_string(),
            ));
        }
        match object {
            PdfObject::Reference { number, generation } => {
                if !visited.insert((number, generation)) {
                    return Err(OxideError::MalformedPdf(format!(
                        "reference cycle at {number} {generation}"
                    )));
                }
                let resolved = self.get_object(number, generation)?;
                self.resolve_inner(resolved, visited, depth + 1)
            }
            other => Ok(other),
        }
    }

    fn ensure_object_stream_cached(&self, stream_obj: u32) -> Result<()> {
        // Fast path: already cached. Release the read lock before doing any
        // parsing work.
        if self
            .object_stream_cache
            .read()
            .expect("object stream cache lock poisoned")
            .contains_key(&stream_obj)
        {
            return Ok(());
        }
        // Parse WITHOUT holding the lock: `parse_object_stream` calls back into
        // `get_object`, which may itself acquire this lock for a *different*
        // object stream. Holding the write lock across that recursion would
        // deadlock. Parsing the same stream twice under a race is harmless and
        // idempotent (the result is value-identical), so we accept that and let
        // the last writer win.
        let objects = self.parse_object_stream(stream_obj)?;
        self.object_stream_cache
            .write()
            .expect("object stream cache lock poisoned")
            .insert(stream_obj, objects);
        Ok(())
    }

    fn parse_object_stream(&self, stream_obj: u32) -> Result<HashMap<u32, (u32, PdfObject)>> {
        let stream = self.get_object(stream_obj, 0)?;
        let (dict, raw) = stream.as_stream().ok_or_else(|| {
            OxideError::MalformedPdf(format!("object {stream_obj} 0 is not an object stream"))
        })?;
        if dict.get_name("Type") != Some("ObjStm") {
            return Err(OxideError::MalformedPdf(format!(
                "object {stream_obj} 0 is not /Type /ObjStm"
            )));
        }
        let decoded = crate::filters::decode_stream(&stream, self)?;
        let n = required_positive_usize(dict, "N")?;
        let first = required_nonnegative_usize(dict, "First")?;
        let _ = raw;
        parse_object_stream_data(&decoded, n, first, Some(self))
    }
}

fn setup_encryption(
    data: &[u8],
    xref: &HashMap<(u32, u16), XrefEntry>,
    trailer: &PdfDictionary,
    password: &[u8],
) -> Result<Option<EncryptionContext>> {
    let Some(encrypt_obj) = trailer.get("Encrypt") else {
        return Ok(None); // not encrypted
    };

    let encrypt_dict = match resolve_encrypt_dict(data, xref, encrypt_obj)? {
        Some(dict) => dict,
        None => return Err(OxideError::EncryptedDocument),
    };

    let info = match EncryptionInfo::from_dict(&encrypt_dict) {
        Ok(info) => info,
        Err(_) => return Err(OxideError::EncryptedDocument),
    };

    // V5 (AES-256, R5/R6) — entirely different key derivation path.
    if info.is_v5() {
        return setup_encryption_v5(password, &info);
    }

    let file_id = extract_file_id(trailer);

    // Try the supplied password first, then the empty password (permission-only
    // encryption, the common case).
    let candidates: Vec<&[u8]> = if password.is_empty() {
        vec![b""]
    } else {
        vec![password, b""]
    };

    let make_ctx = |file_key: Vec<u8>| EncryptionContext {
        file_key,
        is_aes: info.is_aes(),
        is_v5: false,
        stream_method: info.stream_method.clone(),
        string_method: info.string_method.clone(),
        embedded_file_method: info.embedded_file_method.clone(),
        crypt_filters: info.crypt_filters.clone(),
        encrypt_metadata: info.encrypt_metadata,
    };

    for pwd in &candidates {
        if verify_user_password(pwd, &info, &file_id) {
            let file_key = compute_encryption_key(pwd, &info, &file_id);
            return Ok(Some(make_ctx(file_key)));
        }
    }

    // Try the supplied password as an OWNER password: recover the user-password
    // equivalent from /O (Algorithm 3 reverse), then derive the file key from it.
    if !password.is_empty() {
        let recovered = crate::crypto::recover_user_password_from_owner(password, &info);
        if verify_user_password(&recovered, &info, &file_id) {
            let file_key = compute_encryption_key(&recovered, &info, &file_id);
            return Ok(Some(make_ctx(file_key)));
        }
    }

    if info.stream_method == CryptMethod::None && info.string_method == CryptMethod::None {
        return Ok(None);
    }

    Err(OxideError::EncryptedPdf(
        "PDF is password-protected; provide the correct password".to_string(),
    ))
}

/// Set up decryption for a V5 (AES-256, R5/R6) document.
///
/// Tries the supplied password as a user password, then as an owner password,
/// then both again with the empty password as a fallback.
fn setup_encryption_v5(
    password: &[u8],
    info: &EncryptionInfo,
) -> Result<Option<EncryptionContext>> {
    // Build candidate list: supplied pwd first (user then owner), then empty pwd fallback.
    struct Candidate<'a> {
        pwd: &'a [u8],
        is_owner: bool,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    if !password.is_empty() {
        candidates.push(Candidate {
            pwd: password,
            is_owner: false,
        });
        candidates.push(Candidate {
            pwd: password,
            is_owner: true,
        });
    }
    // Always try empty password as fallback (permission-only encryption).
    candidates.push(Candidate {
        pwd: b"",
        is_owner: false,
    });
    candidates.push(Candidate {
        pwd: b"",
        is_owner: true,
    });

    for c in &candidates {
        let verified = if c.is_owner {
            verify_v5_owner_password(c.pwd, info)
        } else {
            verify_v5_user_password(c.pwd, info)
        };

        if !verified {
            continue;
        }

        let file_key_result = if c.is_owner {
            derive_v5_file_key_from_owner(c.pwd, info)
        } else {
            derive_v5_file_key_from_user(c.pwd, info)
        };

        let file_key = match file_key_result {
            Ok(k) => k,
            Err(_) => continue,
        };

        // /Perms verification: confirms the file key is correct and
        // permissions haven't been tampered with. Log a warning on failure
        // but don't reject — some writers produce slightly non-conformant
        // /Perms blocks while the key itself is correct.
        if !verify_v5_perms(&file_key, info) {
            log::warn!("V5 /Perms magic-byte check failed; proceeding with derived key");
        }

        return Ok(Some(EncryptionContext {
            file_key,
            is_aes: false, // V5 uses AES-256 directly, not the is_aes (AES-128) flag
            is_v5: true,
            stream_method: info.stream_method.clone(),
            string_method: info.string_method.clone(),
            embedded_file_method: info.embedded_file_method.clone(),
            crypt_filters: info.crypt_filters.clone(),
            encrypt_metadata: info.encrypt_metadata,
        }));
    }

    if info.stream_method == CryptMethod::None && info.string_method == CryptMethod::None {
        return Ok(None);
    }

    Err(OxideError::EncryptedPdf(
        "PDF is password-protected; provide the correct password".to_string(),
    ))
}

/// Extract the first element of the trailer `/ID` array (used in key
/// derivation). Returns an empty vector when `/ID` is absent.
fn extract_file_id(trailer: &PdfDictionary) -> Vec<u8> {
    match trailer.get("ID") {
        Some(PdfObject::Array(arr)) => match arr.first() {
            Some(PdfObject::String(bytes)) => bytes.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn resolve_encrypt_dict(
    data: &[u8],
    xref: &HashMap<(u32, u16), XrefEntry>,
    object: &PdfObject,
) -> Result<Option<PdfDictionary>> {
    match object {
        PdfObject::Dictionary(dict) => Ok(Some(dict.clone())),
        PdfObject::Reference { number, generation } => {
            let Some(XrefEntry::Uncompressed { offset }) = xref.get(&(*number, *generation)) else {
                return Ok(None);
            };
            let mut parser = PdfParser::new(data, *offset)?;
            let parsed = parser.parse_indirect_object()?;
            match parsed.object {
                PdfObject::Dictionary(dict) => Ok(Some(dict)),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

impl ParserResolver for PdfReader {
    fn resolve_for_parser(&self, object: &PdfObject) -> Result<PdfObject> {
        self.resolve(object.clone())
    }
}

fn stream_crypt_method(dict: &PdfDictionary, ctx: &EncryptionContext) -> CryptMethod {
    if let Some(name) = explicit_crypt_filter_name(dict) {
        return if name == "Identity" {
            CryptMethod::None
        } else {
            ctx.crypt_filters.get(name).cloned().unwrap_or_else(|| {
                if dict.get_name("Type") == Some("EmbeddedFile") {
                    ctx.embedded_file_method.clone()
                } else {
                    ctx.stream_method.clone()
                }
            })
        };
    }
    if dict.get_name("Type") == Some("EmbeddedFile") {
        ctx.embedded_file_method.clone()
    } else {
        ctx.stream_method.clone()
    }
}

fn explicit_crypt_filter_name(dict: &PdfDictionary) -> Option<&str> {
    let filter_obj = dict.get("Filter").or_else(|| dict.get("F"))?;
    let has_crypt = match filter_obj {
        PdfObject::Name(name) => name == "Crypt",
        PdfObject::Array(items) => items
            .iter()
            .any(|item| matches!(item, PdfObject::Name(name) if name == "Crypt")),
        _ => false,
    };
    if !has_crypt {
        return None;
    }

    let params_obj = dict.get("DecodeParms").or_else(|| dict.get("DP"));
    match params_obj {
        Some(PdfObject::Dictionary(params)) => params.get_name("Name"),
        Some(PdfObject::Array(items)) => {
            let idx = crypt_filter_index(filter_obj)?;
            items
                .get(idx)
                .and_then(PdfObject::as_dict)?
                .get_name("Name")
        }
        _ => None,
    }
}

fn crypt_filter_index(filter_obj: &PdfObject) -> Option<usize> {
    match filter_obj {
        PdfObject::Name(name) if name == "Crypt" => Some(0),
        PdfObject::Array(items) => items
            .iter()
            .position(|item| matches!(item, PdfObject::Name(name) if name == "Crypt")),
        _ => None,
    }
}

fn method_is_aes128(method: &CryptMethod) -> bool {
    matches!(method, CryptMethod::AesV2)
}

fn method_is_aes256(method: &CryptMethod) -> bool {
    matches!(method, CryptMethod::AesV3)
}

fn decrypt_bytes_by_method(
    data: &[u8],
    ctx: &EncryptionContext,
    obj_num: u32,
    gen_num: u16,
    method: &CryptMethod,
) -> Vec<u8> {
    match method {
        CryptMethod::None => data.to_vec(),
        CryptMethod::V2 | CryptMethod::AesV2 | CryptMethod::AesV3 => decrypt_string(
            data,
            &ctx.file_key,
            obj_num,
            gen_num,
            method_is_aes128(method),
            method_is_aes256(method),
        ),
    }
}

fn read_xref_chain(
    data: &[u8],
    startxref: usize,
    xref: &mut HashMap<(u32, u16), XrefEntry>,
    trailer: &mut Option<PdfDictionary>,
    visited: &mut HashSet<usize>,
) -> Result<()> {
    let mut next = Some(startxref);
    while let Some(offset) = next {
        if !visited.insert(offset) {
            return Err(OxideError::MalformedPdf(format!(
                "cyclic xref chain at offset {offset}"
            )));
        }
        let section = read_xref_section(data, offset, xref)?;
        if trailer.is_none() {
            *trailer = Some(section.trailer.clone());
        }

        if let Some(xref_stm) = section.xref_stm {
            if visited.insert(xref_stm) {
                let _ = read_xref_section(data, xref_stm, xref)?;
            }
        }

        next = section.prev;
    }
    Ok(())
}

fn repair_uncompressed_xref_offsets(data: &[u8], xref: &mut HashMap<(u32, u16), XrefEntry>) {
    let needs_repair = xref.iter().any(|(&(number, generation), entry)| {
        matches!(
            entry,
            XrefEntry::Uncompressed { offset }
                if !indirect_object_header_at_matches(data, *offset, number, generation)
        )
    });
    if !needs_repair {
        return;
    }

    let scanned = scan_indirect_object_headers(data);
    for (&(number, generation), entry) in xref.iter_mut() {
        let XrefEntry::Uncompressed { offset } = entry else {
            continue;
        };
        if indirect_object_header_at_matches(data, *offset, number, generation) {
            continue;
        }
        if let Some(repaired) = scanned.get(&(number, generation)) {
            *offset = *repaired;
        }
    }
}

fn scan_indirect_object_headers(data: &[u8]) -> HashMap<(u32, u16), usize> {
    let mut offsets = HashMap::new();
    for (rel, window) in data.windows(b" obj".len()).enumerate() {
        if window != b" obj" {
            continue;
        }
        let line_start = data[..rel]
            .iter()
            .rposition(|byte| *byte == b'\r' || *byte == b'\n')
            .map_or(0, |pos| pos + 1);
        let object_start = skip_ws_and_comments(data, line_start);
        let Some((number, generation)) = parse_indirect_object_header(data, object_start) else {
            continue;
        };
        offsets.insert((number, generation), object_start);
    }
    offsets
}

fn indirect_object_header_at_matches(
    data: &[u8],
    offset: usize,
    number: u32,
    generation: u16,
) -> bool {
    parse_indirect_object_header(data, offset) == Some((number, generation))
}

fn parse_indirect_object_header(data: &[u8], offset: usize) -> Option<(u32, u16)> {
    let mut pos = offset;
    let number = u32::try_from(read_u64_token(data, &mut pos).ok()?).ok()?;
    let generation = u16::try_from(read_u64_token(data, &mut pos).ok()?).ok()?;
    let token = read_token(data, &mut pos).ok()?;
    (token == b"obj").then_some((number, generation))
}

#[derive(Clone, Debug)]
struct XrefSection {
    trailer: PdfDictionary,
    prev: Option<usize>,
    xref_stm: Option<usize>,
}

fn read_xref_section(
    data: &[u8],
    offset: usize,
    xref: &mut HashMap<(u32, u16), XrefEntry>,
) -> Result<XrefSection> {
    let offset = skip_ws_and_comments(data, offset);
    if bytes_at(data, offset, b"xref") {
        read_classic_xref(data, offset, xref)
    } else if let Some(repaired) = nearby_classic_xref_offset(data, offset) {
        read_classic_xref(data, repaired, xref)
    } else {
        read_xref_stream(data, offset, xref)
    }
}

fn nearby_classic_xref_offset(data: &[u8], offset: usize) -> Option<usize> {
    let start = offset.saturating_sub(64);
    let end = offset.saturating_add(1024).min(data.len());
    data.get(start..end).and_then(|slice| {
        slice
            .windows(b"xref".len())
            .enumerate()
            .filter_map(|(rel, window)| {
                if window != b"xref" {
                    return None;
                }
                let pos = start + rel;
                let is_word = pos == 0
                    || data
                        .get(pos - 1)
                        .copied()
                        .is_none_or(|b| !b.is_ascii_alphabetic());
                is_word.then_some(pos)
            })
            .min_by_key(|candidate| candidate.abs_diff(offset))
    })
}

fn read_classic_xref(
    data: &[u8],
    mut pos: usize,
    xref: &mut HashMap<(u32, u16), XrefEntry>,
) -> Result<XrefSection> {
    if !bytes_at(data, pos, b"xref") {
        return Err(OxideError::MalformedPdf(format!(
            "xref table expected at offset {pos}"
        )));
    }
    pos += b"xref".len();

    loop {
        pos = skip_ws_and_comments(data, pos);
        if bytes_at(data, pos, b"trailer") {
            pos += b"trailer".len();
            break;
        }

        let start = read_u64_token(data, &mut pos)?;
        let count = read_u64_token(data, &mut pos)?;
        for i in 0..count {
            let object_number = u32::try_from(start + i).map_err(|_| {
                OxideError::MalformedPdf("xref object number does not fit in u32".to_string())
            })?;
            let byte_offset = read_u64_token(data, &mut pos)?;
            let generation = read_u64_token(data, &mut pos)?;
            let status = read_token(data, &mut pos)?;
            let entry = match status.as_slice() {
                b"n" => XrefEntry::Uncompressed {
                    offset: usize::try_from(byte_offset).map_err(|_| {
                        OxideError::MalformedPdf(
                            "xref offset is too large for this platform".to_string(),
                        )
                    })?,
                },
                b"f" => XrefEntry::Free,
                other => {
                    return Err(OxideError::MalformedPdf(format!(
                        "invalid xref entry status {}",
                        String::from_utf8_lossy(other)
                    )));
                }
            };
            let generation = match status.as_slice() {
                b"f" => u16::try_from(generation).unwrap_or(u16::MAX),
                _ => u16::try_from(generation).map_err(|_| {
                    OxideError::MalformedPdf("xref generation does not fit in u16".to_string())
                })?,
            };
            xref.entry((object_number, generation)).or_insert(entry);
        }
    }

    let mut parser = PdfParser::new(data, pos)?;
    let trailer_obj = parser.parse_object()?;
    let trailer = match trailer_obj {
        PdfObject::Dictionary(dict) => dict,
        other => {
            return Err(OxideError::MalformedPdf(format!(
                "classic xref trailer must be a dictionary, got {}",
                other.variant_name()
            )));
        }
    };
    Ok(XrefSection {
        prev: optional_offset(&trailer, "Prev")?,
        xref_stm: optional_offset(&trailer, "XRefStm")?,
        trailer,
    })
}

fn read_xref_stream(
    data: &[u8],
    offset: usize,
    xref: &mut HashMap<(u32, u16), XrefEntry>,
) -> Result<XrefSection> {
    let mut parser = PdfParser::new(data, offset)?;
    let parsed = parser.parse_indirect_object()?;
    let PdfObject::Stream { dict, raw } = parsed.object else {
        return Err(OxideError::MalformedPdf(format!(
            "xref stream offset {offset} did not point to a stream"
        )));
    };
    if dict.get_name("Type") != Some("XRef") {
        return Err(OxideError::MalformedPdf(format!(
            "xref stream object {} {} is not /Type /XRef",
            parsed.number, parsed.generation
        )));
    }
    let decoded = decode_stream_from_dict(&dict, &raw)?;
    for (object_number, generation, entry) in parse_xref_stream_entries(&dict, &decoded)? {
        xref.entry((object_number, generation)).or_insert(entry);
    }
    Ok(XrefSection {
        prev: optional_offset(&dict, "Prev")?,
        xref_stm: optional_offset(&dict, "XRefStm")?,
        trailer: dict,
    })
}

pub(crate) fn parse_xref_stream_entries(
    dict: &PdfDictionary,
    raw: &[u8],
) -> Result<Vec<(u32, u16, XrefEntry)>> {
    let widths = required_integer_array(dict, "W")?;
    if widths.len() != 3 {
        return Err(OxideError::MalformedPdf(
            "xref stream /W must contain three integers".to_string(),
        ));
    }
    let w0 = nonnegative_usize(widths[0], "xref W[0]")?;
    let w1 = nonnegative_usize(widths[1], "xref W[1]")?;
    let w2 = nonnegative_usize(widths[2], "xref W[2]")?;
    let entry_len = w0
        .checked_add(w1)
        .and_then(|v| v.checked_add(w2))
        .ok_or_else(|| OxideError::MalformedPdf("xref entry width overflows".to_string()))?;
    if entry_len == 0 {
        return Err(OxideError::MalformedPdf(
            "xref stream entry width cannot be zero".to_string(),
        ));
    }

    let ranges = if let Some(index) = dict.get_array("Index") {
        parse_index_array(index)?
    } else {
        let size = required_positive_usize(dict, "Size")?;
        vec![(0u32, size)]
    };

    let mut entries = Vec::new();
    let mut cursor = 0usize;
    for (start, count) in ranges {
        for relative in 0..count {
            let end = cursor.checked_add(entry_len).ok_or_else(|| {
                OxideError::MalformedPdf("xref stream cursor overflows".to_string())
            })?;
            if end > raw.len() {
                return Err(OxideError::MalformedPdf(
                    "xref stream ended before all entries were read".to_string(),
                ));
            }
            let entry_bytes = &raw[cursor..end];
            let field0 = if w0 == 0 {
                1
            } else {
                read_big_endian_field(&entry_bytes[0..w0])?
            };
            let field1_start = w0;
            let field2_start = w0 + w1;
            let field1 = read_big_endian_field(&entry_bytes[field1_start..field2_start])?;
            let field2 = read_big_endian_field(&entry_bytes[field2_start..])?;
            let relative = u32::try_from(relative).map_err(|_| {
                OxideError::MalformedPdf("xref stream /Index count exceeds u32".to_string())
            })?;
            let object_number = start.checked_add(relative).ok_or_else(|| {
                OxideError::MalformedPdf("xref stream object number overflows".to_string())
            })?;
            match field0 {
                0 => {
                    let generation = u16::try_from(field2).unwrap_or(u16::MAX);
                    entries.push((object_number, generation, XrefEntry::Free));
                }
                1 => {
                    let generation = u16::try_from(field2).map_err(|_| {
                        OxideError::MalformedPdf("xref generation does not fit in u16".to_string())
                    })?;
                    entries.push((
                        object_number,
                        generation,
                        XrefEntry::Uncompressed {
                            offset: usize::try_from(field1).map_err(|_| {
                                OxideError::MalformedPdf(
                                    "xref offset is too large for this platform".to_string(),
                                )
                            })?,
                        },
                    ));
                }
                2 => {
                    entries.push((
                        object_number,
                        0,
                        XrefEntry::Compressed {
                            stream_obj: u32::try_from(field1).map_err(|_| {
                                OxideError::MalformedPdf(
                                    "object stream number does not fit in u32".to_string(),
                                )
                            })?,
                            index: u32::try_from(field2).map_err(|_| {
                                OxideError::MalformedPdf(
                                    "object stream index does not fit in u32".to_string(),
                                )
                            })?,
                        },
                    ));
                }
                other => {
                    return Err(OxideError::MalformedPdf(format!(
                        "unsupported xref stream entry type {other}"
                    )));
                }
            }
            cursor = end;
        }
    }
    Ok(entries)
}

pub(crate) fn parse_object_stream_data(
    decoded: &[u8],
    n: usize,
    first: usize,
    resolver: Option<&dyn ParserResolver>,
) -> Result<HashMap<u32, (u32, PdfObject)>> {
    if first > decoded.len() {
        return Err(OxideError::MalformedPdf(
            "object stream /First exceeds decoded length".to_string(),
        ));
    }
    let header = &decoded[..first];
    let mut pos = 0usize;
    // `n` is the attacker-controlled `/N` count. Each table entry consumes at
    // least one byte of the `first`-byte header (two whitespace-separated
    // integer tokens), so a genuine stream can hold at most `first` entries.
    // Cap the preallocation hint at that bound: a crafted `/N 4000000000` in a
    // tiny stream must not reserve gigabytes before the per-entry loop rejects
    // the truncated header. The loop below still reads exactly `n` entries and
    // errors cleanly once the header runs out of tokens.
    let mut table = Vec::with_capacity(n.min(first));
    for index in 0..n {
        let object_number = read_u64_token(header, &mut pos)?;
        let offset = read_u64_token(header, &mut pos)?;
        table.push((
            u32::try_from(object_number).map_err(|_| {
                OxideError::MalformedPdf(
                    "object stream object number does not fit in u32".to_string(),
                )
            })?,
            u32::try_from(index).map_err(|_| {
                OxideError::MalformedPdf("object stream index does not fit in u32".to_string())
            })?,
            usize::try_from(offset).map_err(|_| {
                OxideError::MalformedPdf(
                    "object stream offset is too large for this platform".to_string(),
                )
            })?,
        ));
    }

    let mut objects = HashMap::new();
    for (object_number, index, relative_offset) in table {
        let object_offset = first.checked_add(relative_offset).ok_or_else(|| {
            OxideError::MalformedPdf("object stream offset overflows".to_string())
        })?;
        if object_offset >= decoded.len() {
            return Err(OxideError::MalformedPdf(format!(
                "object stream offset for object {object_number} exceeds decoded length"
            )));
        }
        let mut parser = PdfParser::with_resolver(decoded, object_offset, resolver)?;
        let object = parser.parse_object()?;
        objects.insert(object_number, (index, object));
    }
    Ok(objects)
}

fn required_integer_array(dict: &PdfDictionary, key: &str) -> Result<Vec<i64>> {
    let array = dict.get_array(key).ok_or_else(|| {
        OxideError::MalformedPdf(format!("required dictionary key /{key} is missing"))
    })?;
    let mut values = Vec::with_capacity(array.len());
    for object in array {
        match object {
            PdfObject::Integer(value) => values.push(*value),
            other => {
                return Err(OxideError::MalformedPdf(format!(
                    "/{key} array contains {}",
                    other.variant_name()
                )));
            }
        }
    }
    Ok(values)
}

fn parse_index_array(index: &[PdfObject]) -> Result<Vec<(u32, usize)>> {
    if !index.len().is_multiple_of(2) {
        return Err(OxideError::MalformedPdf(
            "xref stream /Index must contain pairs".to_string(),
        ));
    }
    let mut ranges = Vec::new();
    for pair in index.chunks(2) {
        let start = pair[0].as_integer().ok_or_else(|| {
            OxideError::MalformedPdf("xref /Index start must be an integer".to_string())
        })?;
        let count = pair[1].as_integer().ok_or_else(|| {
            OxideError::MalformedPdf("xref /Index count must be an integer".to_string())
        })?;
        if start < 0 || count < 0 {
            return Err(OxideError::MalformedPdf(
                "xref /Index values must be nonnegative".to_string(),
            ));
        }
        ranges.push((
            u32::try_from(start).map_err(|_| {
                OxideError::MalformedPdf("xref /Index start does not fit in u32".to_string())
            })?,
            usize::try_from(count).map_err(|_| {
                OxideError::MalformedPdf("xref /Index count is too large".to_string())
            })?,
        ));
    }
    Ok(ranges)
}

fn read_big_endian_field(bytes: &[u8]) -> Result<u64> {
    if bytes.len() > 8 {
        return Err(OxideError::UnsupportedFeature(
            "xref field wider than 64 bits".to_string(),
        ));
    }
    let mut value = 0u64;
    for &byte in bytes {
        value = (value << 8) | u64::from(byte);
    }
    Ok(value)
}

fn optional_offset(dict: &PdfDictionary, key: &str) -> Result<Option<usize>> {
    match dict.get(key) {
        Some(PdfObject::Integer(value)) => {
            if *value < 0 {
                return Err(OxideError::MalformedPdf(format!(
                    "/{key} offset cannot be negative"
                )));
            }
            Ok(Some(usize::try_from(*value).map_err(|_| {
                OxideError::MalformedPdf(format!("/{key} offset is too large"))
            })?))
        }
        Some(PdfObject::Null) | None => Ok(None),
        Some(other) => Err(OxideError::MalformedPdf(format!(
            "/{key} offset must be an integer, got {}",
            other.variant_name()
        ))),
    }
}

fn required_nonnegative_usize(dict: &PdfDictionary, key: &str) -> Result<usize> {
    let value = dict.get_integer(key).ok_or_else(|| {
        OxideError::MalformedPdf(format!("required dictionary key /{key} is missing"))
    })?;
    nonnegative_usize(value, key)
}

fn required_positive_usize(dict: &PdfDictionary, key: &str) -> Result<usize> {
    let value = required_nonnegative_usize(dict, key)?;
    if value == 0 {
        return Err(OxideError::MalformedPdf(format!("/{key} must be positive")));
    }
    Ok(value)
}

fn nonnegative_usize(value: i64, label: &str) -> Result<usize> {
    if value < 0 {
        return Err(OxideError::MalformedPdf(format!(
            "{label} must be nonnegative"
        )));
    }
    usize::try_from(value).map_err(|_| OxideError::MalformedPdf(format!("{label} is too large")))
}

fn parse_header_version(data: &[u8]) -> Result<String> {
    let search_len = data.len().min(1024);
    let header_offset = data[..search_len]
        .windows(b"%PDF-".len())
        .position(|window| window == b"%PDF-")
        .ok_or_else(|| OxideError::MalformedPdf("missing PDF header".to_string()))?;
    let version_start = header_offset + b"%PDF-".len();
    let mut version_end = version_start;
    while let Some(byte) = data.get(version_end).copied() {
        if is_pdf_whitespace(byte) {
            break;
        }
        version_end += 1;
    }
    let version = std::str::from_utf8(&data[version_start..version_end])
        .map_err(|err| OxideError::MalformedPdf(format!("PDF version is not UTF-8: {err}")))?;
    let valid = (version.len() == 3
        && version.as_bytes()[0] == b'1'
        && version.as_bytes()[1] == b'.'
        && version.as_bytes()[2].is_ascii_digit())
        || version == "2.0";
    if !valid {
        return Err(OxideError::MalformedPdf(format!(
            "unsupported PDF version header {version}"
        )));
    }
    Ok(version.to_string())
}

fn find_startxref(data: &[u8]) -> Result<usize> {
    let marker = b"startxref";
    let marker_pos = data
        .windows(marker.len())
        .rposition(|window| window == marker)
        .ok_or_else(|| OxideError::MalformedPdf("missing startxref".to_string()))?;
    let mut pos = marker_pos + marker.len();
    pos = skip_ws_and_comments(data, pos);
    let offset = read_u64_token(data, &mut pos)?;
    usize::try_from(offset)
        .map_err(|_| OxideError::MalformedPdf("startxref is too large".to_string()))
}

fn read_u64_token(data: &[u8], pos: &mut usize) -> Result<u64> {
    let token = read_token(data, pos)?;
    let text = std::str::from_utf8(&token)
        .map_err(|err| OxideError::ParseError(format!("invalid integer token: {err}")))?;
    text.parse::<u64>()
        .map_err(|err| OxideError::ParseError(format!("invalid unsigned integer: {err}")))
}

fn read_token(data: &[u8], pos: &mut usize) -> Result<Vec<u8>> {
    *pos = skip_ws_and_comments(data, *pos);
    let start = *pos;
    while let Some(byte) = data.get(*pos).copied() {
        if is_pdf_whitespace(byte) || is_delimiter(byte) {
            break;
        }
        *pos += 1;
    }
    if *pos == start {
        return Err(OxideError::ParseError(
            "expected token while reading PDF bytes".to_string(),
        ));
    }
    Ok(data[start..*pos].to_vec())
}

fn skip_ws_and_comments(data: &[u8], mut pos: usize) -> usize {
    loop {
        while matches!(data.get(pos), Some(byte) if is_pdf_whitespace(*byte)) {
            pos += 1;
        }
        if data.get(pos).copied() == Some(b'%') {
            while let Some(byte) = data.get(pos).copied() {
                pos += 1;
                if byte == b'\r' || byte == b'\n' {
                    break;
                }
            }
        } else {
            break;
        }
    }
    pos
}

fn bytes_at(data: &[u8], pos: usize, bytes: &[u8]) -> bool {
    data.get(pos..pos + bytes.len())
        .is_some_and(|slice| slice == bytes)
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;

    fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn parses_xref_stream_entries_from_widths() {
        let dict = dict(&[
            (
                "W",
                PdfObject::Array(vec![
                    PdfObject::Integer(1),
                    PdfObject::Integer(2),
                    PdfObject::Integer(1),
                ]),
            ),
            (
                "Index",
                PdfObject::Array(vec![PdfObject::Integer(1), PdfObject::Integer(2)]),
            ),
        ]);
        let raw = [1, 0, 42, 0, 2, 0, 5, 3];
        let entries = parse_xref_stream_entries(&dict, &raw).unwrap();
        assert_eq!(
            entries,
            vec![
                (1, 0, XrefEntry::Uncompressed { offset: 42 }),
                (
                    2,
                    0,
                    XrefEntry::Compressed {
                        stream_obj: 5,
                        index: 3
                    }
                )
            ]
        );
    }

    #[test]
    fn classic_xref_tolerates_overlarge_free_generation() {
        let data = b"xref
0 2
0000000000 65536 f
0000000015 00000 n
trailer
<< /Size 2 /Root 1 0 R >>
";
        let mut xref = HashMap::new();

        read_classic_xref(data, 0, &mut xref).unwrap();

        assert!(matches!(xref.get(&(0, u16::MAX)), Some(XrefEntry::Free)));
        assert!(matches!(
            xref.get(&(1, 0)),
            Some(XrefEntry::Uncompressed { offset: 15 })
        ));
    }

    #[test]
    fn xref_section_repairs_forward_classic_xref_offset() {
        let mut data = vec![b' '; 192];
        let xref_offset = 64usize;
        let xref = b"xref
0 2
0000000000 65535 f
0000000015 00000 n
trailer
<< /Size 2 /Root 1 0 R >>
";
        data[xref_offset..xref_offset + xref.len()].copy_from_slice(xref);
        let mut xref_map = HashMap::new();

        read_xref_section(&data, xref_offset - 40, &mut xref_map).unwrap();

        assert!(matches!(
            xref_map.get(&(1, 0)),
            Some(XrefEntry::Uncompressed { offset: 15 })
        ));
    }

    #[test]
    fn reader_repairs_bad_uncompressed_xref_offsets() {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n\n");
        let obj1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\n");
        let obj2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n\n");
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f\n{obj1:010} 00000 n\n{:010} 00000 n\ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                obj2 - 1
            )
            .as_bytes(),
        );

        let reader = PdfReader::from_bytes(pdf).unwrap();

        assert!(matches!(
            reader.get_object(2, 0).unwrap(),
            PdfObject::Dictionary(_)
        ));
    }

    #[test]
    fn xref_stream_tolerates_overlarge_free_generation() {
        let dict = dict(&[
            (
                "W",
                PdfObject::Array(vec![
                    PdfObject::Integer(1),
                    PdfObject::Integer(1),
                    PdfObject::Integer(3),
                ]),
            ),
            (
                "Index",
                PdfObject::Array(vec![PdfObject::Integer(0), PdfObject::Integer(1)]),
            ),
        ]);
        let raw = [0, 0, 1, 0, 0];

        let entries = parse_xref_stream_entries(&dict, &raw).unwrap();

        assert_eq!(entries, vec![(0, u16::MAX, XrefEntry::Free)]);
    }

    #[test]
    fn parses_object_stream_data_by_object_number() {
        let decoded = b"10 0 11 5 true /Name";
        let objects = parse_object_stream_data(decoded, 2, 10, None).unwrap();
        assert_eq!(objects.get(&10).unwrap().1, PdfObject::Boolean(true));
        assert_eq!(
            objects.get(&11).unwrap().1,
            PdfObject::Name("Name".to_string())
        );
    }

    #[test]
    fn object_stream_huge_n_does_not_allocate_or_panic() {
        // A crafted object stream declaring a colossal /N count in a tiny
        // buffer must NOT preallocate gigabytes (OOM) — the capacity hint is
        // bounded by the header length — and must return a clean error once
        // the header runs out of tokens. Regression for the unbounded
        // `Vec::with_capacity(n)` allocation (fuzz finding: ObjStm /N OOM).
        let decoded = b"10 0 true";
        let result = parse_object_stream_data(decoded, usize::MAX, 5, None);
        assert!(
            result.is_err(),
            "huge /N over a short header must error, not allocate or panic"
        );
    }
}
