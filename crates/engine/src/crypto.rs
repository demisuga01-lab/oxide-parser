//! Cryptographic primitives for the PDF Standard Security Handler.
//!
//! This module implements the subset of PDF encryption needed to read the
//! overwhelming majority of encrypted PDFs found in the wild:
//!
//! - Standard Security Handler (`/Filter /Standard`)
//! - V1 (RC4 40-bit, R2), V2 (RC4 up to 128-bit, R3), V4 (RC4 or AES-128, R4)
//! - Empty user password (permission-only encryption) and user-supplied passwords
//!
//! Deliberately **not** implemented here (return errors / are rejected upstream):
//!
//! - V5 / R5 / R6 (AES-256, PDF 2.0) — entirely different SHA-256 key derivation.
//!   TODO(aes256): the `aes` crate already supports AES-256; only the key
//!   derivation (PDF 32000-2 §7.6.4.3.3/4) and the `/Perms` check are missing.
//! - Public-key security handlers (`/Filter /Adobe.PubSec`) — certificate based.
//!
//! RC4 is implemented from scratch (it is trivially small and no maintained
//! crate is worth the dependency). AES-128-CBC uses the `aes` + `cbc` crates,
//! and MD5 (used only for legacy key derivation, never for security decisions of
//! our own) uses the `md-5` crate.

use crate::error::{OxideError, Result};
use crate::object::{PdfDictionary, PdfObject};

// ---------------------------------------------------------------------------
// 2.1  RC4 stream cipher
// ---------------------------------------------------------------------------

/// Minimal RC4 stream cipher (Rivest Cipher 4).
///
/// RC4 is symmetric: the same operation encrypts and decrypts. It is
/// cryptographically broken, but the PDF Standard Security Handler (V1–V4)
/// mandates it, so we implement it faithfully for compatibility only.
pub struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    /// Initialise the RC4 state from a key (1..=256 bytes).
    pub fn new(key: &[u8]) -> Self {
        assert!(!key.is_empty() && key.len() <= 256);
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0u8..=255 {
            j = j
                .wrapping_add(s[i as usize])
                .wrapping_add(key[i as usize % key.len()]);
            s.swap(i as usize, j as usize);
        }
        Self { s, i: 0, j: 0 }
    }

    /// Produce the next keystream byte.
    pub fn next_byte(&mut self) -> u8 {
        self.i = self.i.wrapping_add(1);
        self.j = self.j.wrapping_add(self.s[self.i as usize]);
        self.s.swap(self.i as usize, self.j as usize);
        self.s[(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize]
    }

    /// XOR the keystream into `data` in place.
    pub fn process(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            *byte ^= self.next_byte();
        }
    }

    /// Encrypt/decrypt a slice, returning the result as a `Vec<u8>`.
    pub fn apply(key: &[u8], data: &[u8]) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }
        let mut rc4 = Self::new(key);
        let mut out = data.to_vec();
        rc4.process(&mut out);
        out
    }
}

// ---------------------------------------------------------------------------
// 2.2  AES-128-CBC decryption
// ---------------------------------------------------------------------------

/// Decrypt data using AES-128-CBC as used by the PDF Standard Security Handler.
///
/// In PDF, the first 16 bytes of the ciphertext are the IV and the key is
/// exactly 16 bytes. The remaining ciphertext is PKCS#7 padded.
pub fn aes128_cbc_decrypt(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 16 {
        return Err(OxideError::MalformedPdf(format!(
            "AES-128: key must be 16 bytes, got {}",
            key.len()
        )));
    }
    if ciphertext.len() < 16 {
        return Err(OxideError::MalformedPdf(
            "AES-128: ciphertext shorter than IV (16 bytes)".to_string(),
        ));
    }
    let (iv, data) = ciphertext.split_at(16);
    if data.is_empty() {
        // IV only, no payload: decrypts to the empty string.
        return Ok(Vec::new());
    }
    if data.len() % 16 != 0 {
        // Robustness: some malformed producers do not pad to a block boundary.
        // Zero-extend to the next block and decrypt without unpadding so we at
        // least surface the leading plaintext instead of failing outright.
        let padded_len = data.len().div_ceil(16) * 16;
        let mut padded = data.to_vec();
        padded.resize(padded_len, 0);
        return decrypt_aes128_cbc_no_pad(key, iv, &padded);
    }
    decrypt_aes128_cbc_pkcs7(key, iv, data)
}

fn decrypt_aes128_cbc_pkcs7(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    let mut buf = data.to_vec();
    let decryptor = Aes128CbcDec::new_from_slices(key, iv).map_err(|_| {
        OxideError::MalformedPdf("AES-128-CBC: invalid key or IV length".to_string())
    })?;
    let result = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| {
            OxideError::MalformedPdf("AES-128-CBC: padding error during decryption".to_string())
        })?;
    Ok(result.to_vec())
}

fn decrypt_aes128_cbc_no_pad(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    let mut buf = data.to_vec();
    let decryptor = Aes128CbcDec::new_from_slices(key, iv).map_err(|_| {
        OxideError::MalformedPdf("AES-128-CBC: invalid key or IV length".to_string())
    })?;
    let result = decryptor
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| {
            OxideError::MalformedPdf("AES-128-CBC: block decryption failed".to_string())
        })?;
    Ok(result.to_vec())
}

// ---------------------------------------------------------------------------
// 2.3  MD5 helper
// ---------------------------------------------------------------------------

/// Compute the MD5 digest of `data`.
///
/// MD5 is used only because the legacy PDF key-derivation algorithm requires
/// it; it is never relied on for any security property of our own.
pub fn md5(data: &[u8]) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// 3.1  Encryption metadata
// ---------------------------------------------------------------------------

/// The encryption method used for a given object (PDF V4 crypt filters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptMethod {
    /// Identity filter — no encryption applied.
    None,
    /// RC4 (the only method for V1/V2/V3, and `/CFM /V2` under V4).
    V2,
    /// AES-128 in CBC mode (`/CFM /AESV2` under V4).
    AesV2,
    /// AES-256 (`/CFM /AESV3`, V5) — not implemented; rejected during parsing.
    AesV3,
}

/// Parsed contents of a Standard Security Handler `/Encrypt` dictionary.
#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    /// Algorithm version: 1, 2, or 4.
    pub v: u8,
    /// Revision of the Standard Security Handler: 2, 3, or 4.
    pub r: u8,
    /// Key length in bits (40 or 128).
    pub key_length: usize,
    /// `/O` entry: owner-password verifier (32 bytes for R2–R4).
    pub o: Vec<u8>,
    /// `/U` entry: user-password verifier (32 bytes for R2–R4).
    pub u: Vec<u8>,
    /// `/P`: permission flags (signed 32-bit bitmask).
    pub p: i32,
    /// `/EncryptMetadata` (default true).
    pub encrypt_metadata: bool,
    /// For V4: the crypt-filter method applied to streams and strings.
    pub cf_method: CryptMethod,
}

impl EncryptionInfo {
    /// Parse an [`EncryptionInfo`] from a resolved `/Encrypt` dictionary.
    pub fn from_dict(dict: &PdfDictionary) -> Result<Self> {
        let filter = dict.get_name("Filter").unwrap_or("Standard");
        if filter != "Standard" {
            return Err(OxideError::UnsupportedFeature(format!(
                "Encryption filter '{filter}' is not supported; only /Standard is implemented"
            )));
        }

        let v = dict.get_integer("V").unwrap_or(0) as u8;
        let r = dict.get_integer("R").unwrap_or(0) as u8;

        if v == 5 {
            return Err(OxideError::UnsupportedFeature(
                "PDF 2.0 AES-256 encryption (V=5/R=6) is not yet supported".to_string(),
            ));
        }
        if v == 0 || v > 4 {
            return Err(OxideError::MalformedPdf(format!(
                "Unsupported encryption version V={v}"
            )));
        }

        let key_length = if v == 1 {
            40
        } else {
            dict.get_integer("Length").unwrap_or(128) as usize
        };

        let o = extract_bytes(dict, "O")?;
        let u = extract_bytes(dict, "U")?;
        let p = dict.get_integer("P").unwrap_or(-4) as i32;
        let encrypt_metadata = dict
            .get("EncryptMetadata")
            .and_then(PdfObject::as_bool)
            .unwrap_or(true);

        // For V4, the per-object method is named by the crypt filter referenced
        // by /StmF (streams) and /StrF (strings). In practice both reference the
        // same filter for the Standard handler; we read /StmF and look up its
        // /CFM in /CF, falling back to RC4.
        let cf_method = if v == 4 {
            resolve_v4_method(dict)?
        } else {
            // V1/V2/V3 always use RC4.
            CryptMethod::V2
        };

        Ok(EncryptionInfo {
            v,
            r,
            key_length,
            o,
            u,
            p,
            encrypt_metadata,
            cf_method,
        })
    }

    /// True if this object stream/string is encrypted with AES-128.
    pub fn is_aes(&self) -> bool {
        self.cf_method == CryptMethod::AesV2
    }
}

/// Resolve the V4 crypt-filter method from `/StmF` + `/CF`.
fn resolve_v4_method(dict: &PdfDictionary) -> Result<CryptMethod> {
    let stm_f = dict.get_name("StmF").unwrap_or("Identity");
    if stm_f == "Identity" {
        return Ok(CryptMethod::None);
    }

    // Look up the named crypt filter in /CF and read its /CFM.
    if let Some(cf) = dict.get_dict("CF") {
        if let Some(filter) = cf.get_dict(stm_f) {
            return Ok(match filter.get_name("CFM") {
                Some("AESV2") => CryptMethod::AesV2,
                Some("AESV3") => CryptMethod::AesV3,
                Some("V2") => CryptMethod::V2,
                Some("Identity") | None => CryptMethod::None,
                Some(other) => {
                    log::warn!("unknown crypt filter method /CFM /{other}; assuming RC4");
                    CryptMethod::V2
                }
            });
        }
    }

    // Some producers name AESV2 directly in /StmF instead of via /CF.
    Ok(match stm_f {
        "AESV2" => CryptMethod::AesV2,
        "AESV3" => CryptMethod::AesV3,
        _ => CryptMethod::V2,
    })
}

/// Read a required PDF string entry as raw bytes.
fn extract_bytes(dict: &PdfDictionary, key: &str) -> Result<Vec<u8>> {
    match dict.get(key) {
        Some(PdfObject::String(bytes)) => Ok(bytes.clone()),
        Some(other) => Err(OxideError::MalformedPdf(format!(
            "Encryption /{key}: expected string, got {}",
            other.variant_name()
        ))),
        None => Err(OxideError::MalformedPdf(format!(
            "Encryption: missing /{key} entry"
        ))),
    }
}

// ---------------------------------------------------------------------------
// 4  Key derivation (PDF 32000-1 §7.6.3.3)
// ---------------------------------------------------------------------------

/// The 32-byte password-padding string defined by the PDF specification.
pub const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Pad (or truncate) a password to the fixed 32-byte form the algorithm needs.
pub fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; 32];
    let copy_len = password.len().min(32);
    padded[..copy_len].copy_from_slice(&password[..copy_len]);
    padded[copy_len..].copy_from_slice(&PADDING[copy_len..]);
    padded
}

/// Compute the file encryption key from a user password.
///
/// Algorithm (PDF 32000-1 §7.6.3.3, Algorithm 2):
///   1. Hash `padded_password + O + P(LE 4 bytes) + file_id`, plus
///      `0xFFFFFFFF` when `R >= 4` and metadata is not encrypted.
///   2. MD5 the result.
///   3. For `R >= 3`, repeat MD5 over the first `n` bytes 50 more times.
///   4. Take the first `key_length / 8` bytes as the key.
pub fn compute_encryption_key(password: &[u8], info: &EncryptionInfo, file_id: &[u8]) -> Vec<u8> {
    let key_len = info.key_length / 8;
    let mut input = Vec::with_capacity(128);
    input.extend_from_slice(&pad_password(password));
    input.extend_from_slice(&info.o);
    input.extend_from_slice(&(info.p as u32).to_le_bytes());
    input.extend_from_slice(file_id);
    if info.r >= 4 && !info.encrypt_metadata {
        input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    let mut hash = md5(&input);

    if info.r >= 3 {
        for _ in 0..50 {
            hash = md5(&hash[..key_len]);
        }
    }

    hash[..key_len].to_vec()
}

/// Verify that `password` matches the `/U` user-password verifier.
///
/// For `R >= 3` (Algorithm 5): RC4-encrypt `MD5(PADDING + file_id)` with the
/// file key, iterate 19 more times with the key XORed by the iteration number,
/// and compare the first 16 bytes against `/U`.
///
/// For `R == 2` (Algorithm 4): RC4-encrypt the padding string with the file key
/// and compare all 32 bytes against `/U`.
pub fn verify_user_password(password: &[u8], info: &EncryptionInfo, file_id: &[u8]) -> bool {
    let key = compute_encryption_key(password, info, file_id);

    if info.r >= 3 {
        let mut hash_input = Vec::with_capacity(32 + file_id.len());
        hash_input.extend_from_slice(&PADDING);
        hash_input.extend_from_slice(file_id);
        let hash = md5(&hash_input);

        let mut result = Rc4::apply(&key, &hash);
        for i in 1u8..=19 {
            let xor_key: Vec<u8> = key.iter().map(|&b| b ^ i).collect();
            result = Rc4::apply(&xor_key, &result);
        }

        result.len() >= 16 && info.u.len() >= 16 && result[..16] == info.u[..16]
    } else {
        let result = Rc4::apply(&key, &PADDING);
        result.len() == 32 && info.u.len() >= 32 && result == info.u[..32]
    }
}

// ---------------------------------------------------------------------------
// 5  Object-level decryption
// ---------------------------------------------------------------------------

/// Compute the per-object decryption key.
///
/// PDF 32000-1 §7.6.2 Algorithm 1: append `obj_num` (3 bytes LE) and `gen_num`
/// (2 bytes LE) to the file key, plus the literal `sAlT` for AES, then MD5. The
/// key length is `min(file_key.len() + 5, 16)` bytes.
pub fn object_key(file_key: &[u8], obj_num: u32, gen_num: u16, is_aes: bool) -> Vec<u8> {
    let mut input = Vec::with_capacity(file_key.len() + 9);
    input.extend_from_slice(file_key);
    input.push((obj_num & 0xFF) as u8);
    input.push(((obj_num >> 8) & 0xFF) as u8);
    input.push(((obj_num >> 16) & 0xFF) as u8);
    input.push((gen_num & 0xFF) as u8);
    input.push(((gen_num >> 8) & 0xFF) as u8);
    if is_aes {
        input.extend_from_slice(b"sAlT");
    }
    let hash = md5(&input);
    let key_len = (file_key.len() + 5).min(16);
    hash[..key_len].to_vec()
}

/// Decrypt a PDF string value belonging to object `obj_num`/`gen_num`.
///
/// On any AES failure the original bytes are returned unchanged, so a single
/// corrupt object can never poison the rest of the document.
pub fn decrypt_string(
    data: &[u8],
    file_key: &[u8],
    obj_num: u32,
    gen_num: u16,
    is_aes: bool,
) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let key = object_key(file_key, obj_num, gen_num, is_aes);
    if is_aes {
        aes128_cbc_decrypt(&key, data).unwrap_or_else(|_| data.to_vec())
    } else {
        Rc4::apply(&key, data)
    }
}

/// Decrypt a PDF stream's raw bytes. Streams use the same per-object key as
/// strings.
pub fn decrypt_stream(
    data: &[u8],
    file_key: &[u8],
    obj_num: u32,
    gen_num: u16,
    is_aes: bool,
) -> Vec<u8> {
    decrypt_string(data, file_key, obj_num, gen_num, is_aes)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    // --- RC4 ---

    #[test]
    fn rc4_known_vector_key_plaintext() {
        // Classic RC4 test vector: RC4("Key", "Plaintext") = BBF316E8D940AF0AD3.
        let out = Rc4::apply(b"Key", b"Plaintext");
        assert_eq!(
            out,
            vec![0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
    }

    #[test]
    fn rc4_is_symmetric() {
        let key = b"test-key";
        let plain = b"Hello World from RC4";
        let enc = Rc4::apply(key, plain);
        let dec = Rc4::apply(key, &enc);
        assert_eq!(dec, plain.to_vec());
    }

    #[test]
    fn rc4_empty_data() {
        let out = Rc4::apply(b"key", b"");
        assert!(out.is_empty());
    }

    #[test]
    fn rc4_key_length_boundary() {
        for len in 1..=16usize {
            let key = vec![0x42u8; len];
            let data = b"test data";
            let enc = Rc4::apply(&key, data);
            let dec = Rc4::apply(&key, &enc);
            assert_eq!(
                dec,
                data.to_vec(),
                "RC4 round-trip failed for key len {len}"
            );
        }
    }

    // --- MD5 ---

    #[test]
    fn md5_empty_string_vector() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        let hash = md5(b"");
        assert_eq!(
            hash,
            [
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
                0x42, 0x7e,
            ]
        );
    }

    #[test]
    fn md5_abc_vector() {
        // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let hash = md5(b"abc");
        assert_eq!(hash[0], 0x90, "first byte of MD5(abc)");
        assert_eq!(hash[1], 0x01, "second byte of MD5(abc)");
    }

    // --- Key derivation ---

    #[test]
    fn pad_password_empty_equals_padding() {
        assert_eq!(pad_password(b""), PADDING);
    }

    #[test]
    fn pad_password_full_length_ignores_padding() {
        let pwd = [0xABu8; 32];
        assert_eq!(pad_password(&pwd), pwd);
    }

    #[test]
    fn pad_password_short_appends_padding() {
        let padded = pad_password(b"abc");
        assert_eq!(&padded[..3], b"abc");
        assert_eq!(&padded[3..], &PADDING[3..]);
    }

    #[test]
    fn object_key_is_deterministic() {
        let file_key = vec![0x12u8, 0x34, 0x56, 0x78, 0x9A];
        let k1 = object_key(&file_key, 5, 0, false);
        let k2 = object_key(&file_key, 5, 0, false);
        assert_eq!(k1, k2);
    }

    #[test]
    fn object_key_differs_per_object() {
        let file_key = vec![0x12u8, 0x34, 0x56, 0x78, 0x9A];
        let k5 = object_key(&file_key, 5, 0, false);
        let k6 = object_key(&file_key, 6, 0, false);
        assert_ne!(k5, k6);
    }

    #[test]
    fn object_key_aes_appends_salt() {
        let file_key = vec![0x01u8; 5];
        let rc4_key = object_key(&file_key, 1, 0, false);
        let aes_key = object_key(&file_key, 1, 0, true);
        assert_ne!(rc4_key, aes_key);
    }

    #[test]
    fn compute_encryption_key_is_deterministic_128() {
        let info = EncryptionInfo {
            v: 2,
            r: 3,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            cf_method: CryptMethod::V2,
        };
        let file_id = vec![0x42u8; 16];
        let k1 = compute_encryption_key(b"", &info, &file_id);
        let k2 = compute_encryption_key(b"", &info, &file_id);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 16);
    }

    #[test]
    fn compute_encryption_key_length_matches_key_length() {
        let make_info = |key_length: usize| EncryptionInfo {
            v: 1,
            r: 2,
            key_length,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            cf_method: CryptMethod::V2,
        };
        let fid = vec![0u8; 16];
        assert_eq!(compute_encryption_key(b"", &make_info(40), &fid).len(), 5);
        assert_eq!(compute_encryption_key(b"", &make_info(128), &fid).len(), 16);
    }

    #[test]
    fn different_passwords_produce_different_keys() {
        let info = EncryptionInfo {
            v: 2,
            r: 3,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            cf_method: CryptMethod::V2,
        };
        let fid = vec![0xABu8; 16];
        let k_empty = compute_encryption_key(b"", &info, &fid);
        let k_secret = compute_encryption_key(b"secret", &info, &fid);
        assert_ne!(k_empty, k_secret);
    }

    // --- AES ---

    #[test]
    fn aes128_cbc_round_trip_with_iv_prefix() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let key = [0x00u8; 16];
        let iv = [0x00u8; 16];
        let plain = b"0123456789ABCDEF"; // exactly 16 bytes

        let mut buf = plain.to_vec();
        buf.resize(32, 0); // room for PKCS#7 padding block
        let ct_len = Aes128CbcEnc::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_mut::<Pkcs7>(&mut buf, 16)
            .unwrap()
            .len();
        let ciphertext = &buf[..ct_len];

        let mut pdf_ct = iv.to_vec();
        pdf_ct.extend_from_slice(ciphertext);

        let decrypted = aes128_cbc_decrypt(&key, &pdf_ct).unwrap();
        assert_eq!(decrypted, plain.to_vec());
    }

    #[test]
    fn aes_ciphertext_shorter_than_iv_errors() {
        let result = aes128_cbc_decrypt(&[0u8; 16], b"too short");
        assert!(result.is_err());
    }

    #[test]
    fn aes_wrong_key_length_errors() {
        let mut ct = vec![0u8; 32];
        ct[0] = 1;
        assert!(aes128_cbc_decrypt(&[0u8; 8], &ct).is_err());
    }

    // --- verify_user_password ---

    #[test]
    fn verify_user_password_rejects_mismatch() {
        let info = EncryptionInfo {
            v: 2,
            r: 3,
            key_length: 128,
            o: PADDING.to_vec(),
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            cf_method: CryptMethod::V2,
        };
        let file_id = vec![0x01u8; 16];
        assert!(!verify_user_password(b"wrong-password", &info, &file_id));
    }

    // --- decrypt_string ---

    #[test]
    fn decrypt_string_empty_data_returns_empty() {
        let result = decrypt_string(b"", &[0x01u8; 5], 1, 0, false);
        assert!(result.is_empty());
    }

    #[test]
    fn decrypt_string_rc4_round_trip() {
        let file_key = vec![0x2Bu8, 0xE9, 0xF7, 0xC3, 0xD5];
        let original = b"Hello PDF World";
        let key = object_key(&file_key, 7, 0, false);
        let encrypted = Rc4::apply(&key, original);
        let decrypted = decrypt_string(&encrypted, &file_key, 7, 0, false);
        assert_eq!(decrypted, original.to_vec());
    }

    // --- EncryptionInfo::from_dict ---

    #[test]
    fn from_dict_rejects_unknown_filter() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Adobe.PubSec".to_string())),
            ("V", PdfObject::Integer(1)),
            ("R", PdfObject::Integer(2)),
            ("O", PdfObject::String(vec![0u8; 32])),
            ("U", PdfObject::String(vec![0u8; 32])),
            ("P", PdfObject::Integer(-4)),
        ]);
        assert!(matches!(
            EncryptionInfo::from_dict(&d),
            Err(OxideError::UnsupportedFeature(_))
        ));
    }

    #[test]
    fn from_dict_rejects_v5_aes256() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(5)),
            ("R", PdfObject::Integer(6)),
            ("O", PdfObject::String(vec![0u8; 48])),
            ("U", PdfObject::String(vec![0u8; 48])),
            ("P", PdfObject::Integer(-4)),
        ]);
        assert!(matches!(
            EncryptionInfo::from_dict(&d),
            Err(OxideError::UnsupportedFeature(_))
        ));
    }

    #[test]
    fn from_dict_parses_v2_r3() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(2)),
            ("R", PdfObject::Integer(3)),
            ("Length", PdfObject::Integer(128)),
            ("O", PdfObject::String(vec![1u8; 32])),
            ("U", PdfObject::String(vec![2u8; 32])),
            ("P", PdfObject::Integer(-3904)),
        ]);
        let info = EncryptionInfo::from_dict(&d).unwrap();
        assert_eq!(info.v, 2);
        assert_eq!(info.r, 3);
        assert_eq!(info.key_length, 128);
        assert_eq!(info.p, -3904);
        assert!(info.encrypt_metadata);
        assert_eq!(info.cf_method, CryptMethod::V2);
        assert!(!info.is_aes());
    }

    #[test]
    fn from_dict_v1_forces_40_bit() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(1)),
            ("R", PdfObject::Integer(2)),
            ("O", PdfObject::String(vec![1u8; 32])),
            ("U", PdfObject::String(vec![2u8; 32])),
            ("P", PdfObject::Integer(-4)),
        ]);
        let info = EncryptionInfo::from_dict(&d).unwrap();
        assert_eq!(info.key_length, 40);
    }

    #[test]
    fn from_dict_v4_aesv2_via_cf() {
        let cf_filter = dict(&[
            ("CFM", PdfObject::Name("AESV2".to_string())),
            ("Length", PdfObject::Integer(16)),
        ]);
        let cf = dict(&[("StdCF", PdfObject::Dictionary(cf_filter))]);
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(4)),
            ("R", PdfObject::Integer(4)),
            ("Length", PdfObject::Integer(128)),
            ("O", PdfObject::String(vec![1u8; 32])),
            ("U", PdfObject::String(vec![2u8; 32])),
            ("P", PdfObject::Integer(-4)),
            ("CF", PdfObject::Dictionary(cf)),
            ("StmF", PdfObject::Name("StdCF".to_string())),
            ("StrF", PdfObject::Name("StdCF".to_string())),
        ]);
        let info = EncryptionInfo::from_dict(&d).unwrap();
        assert_eq!(info.cf_method, CryptMethod::AesV2);
        assert!(info.is_aes());
    }

    #[test]
    fn from_dict_v4_identity_stmf_is_none() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(4)),
            ("R", PdfObject::Integer(4)),
            ("O", PdfObject::String(vec![1u8; 32])),
            ("U", PdfObject::String(vec![2u8; 32])),
            ("P", PdfObject::Integer(-4)),
            ("StmF", PdfObject::Name("Identity".to_string())),
        ]);
        let info = EncryptionInfo::from_dict(&d).unwrap();
        assert_eq!(info.cf_method, CryptMethod::None);
    }

    #[test]
    fn from_dict_missing_o_errors() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(2)),
            ("R", PdfObject::Integer(3)),
        ]);
        assert!(matches!(
            EncryptionInfo::from_dict(&d),
            Err(OxideError::MalformedPdf(_))
        ));
    }
}
