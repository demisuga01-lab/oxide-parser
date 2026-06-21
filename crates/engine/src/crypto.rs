//! Cryptographic primitives for the PDF Standard Security Handler.
//!
//! This module implements the PDF Standard Security Handler for all revisions
//! currently used in practice:
//!
//! - Standard Security Handler (`/Filter /Standard`)
//! - V1 (RC4 40-bit, R2), V2 (RC4 up to 128-bit, R3), V4 (RC4 or AES-128, R4)
//! - V5 (AES-256, R5/R6, PDF 2.0) — ISO 32000-2 §7.6.4 / §7.6.5
//! - Empty user password (permission-only encryption) and user-supplied passwords
//!
//! Deliberately **not** implemented here (return errors / are rejected upstream):
//!
//! - Public-key security handlers (`/Filter /Adobe.PubSec`) — certificate based.
//!   Requires PKCS#7/CMS parsing and an RSA private key supplied by the caller;
//!   documented as the explicit next crypto follow-up.
//!
//! RC4 is implemented from scratch (it is trivially small and no maintained
//! crate is worth the dependency). AES-128-CBC uses the `aes` + `cbc` crates.
//! AES-256-CBC and AES-256-ECB also use `aes` + `cbc`. MD5 (legacy key
//! derivation only) uses `md-5`. SHA-256/384/512 (R5/R6 key derivation) use
//! `sha2`.

use std::collections::HashMap;

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
        return Ok(Vec::new());
    }
    if data.len() % 16 != 0 {
        // Robustness: some malformed producers do not pad to a block boundary.
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
// 2.3  AES-256-CBC decryption (V5 / R5 / R6)
// ---------------------------------------------------------------------------

/// Decrypt data using AES-256-CBC as used by the PDF V5 Standard Security Handler.
///
/// The first 16 bytes of `ciphertext` are the IV; the remainder is the
/// PKCS#7-padded ciphertext. `key` must be exactly 32 bytes.
pub fn aes256_cbc_decrypt(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 32 {
        return Err(OxideError::MalformedPdf(format!(
            "AES-256: key must be 32 bytes, got {}",
            key.len()
        )));
    }
    if ciphertext.len() < 16 {
        return Err(OxideError::MalformedPdf(
            "AES-256: ciphertext shorter than IV (16 bytes)".to_string(),
        ));
    }
    let (iv, data) = ciphertext.split_at(16);
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if data.len() % 16 != 0 {
        let padded_len = data.len().div_ceil(16) * 16;
        let mut padded = data.to_vec();
        padded.resize(padded_len, 0);
        return aes256_cbc_no_pad(key, iv, &padded);
    }
    aes256_cbc_pkcs7(key, iv, data)
}

fn aes256_cbc_pkcs7(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let mut buf = data.to_vec();
    let decryptor = Aes256CbcDec::new_from_slices(key, iv).map_err(|_| {
        OxideError::MalformedPdf("AES-256-CBC: invalid key or IV length".to_string())
    })?;
    let result = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| {
            OxideError::MalformedPdf("AES-256-CBC: padding error during decryption".to_string())
        })?;
    Ok(result.to_vec())
}

fn aes256_cbc_no_pad(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let mut buf = data.to_vec();
    let decryptor = Aes256CbcDec::new_from_slices(key, iv).map_err(|_| {
        OxideError::MalformedPdf("AES-256-CBC: invalid key or IV length".to_string())
    })?;
    let result = decryptor
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| {
            OxideError::MalformedPdf("AES-256-CBC: block decryption failed".to_string())
        })?;
    Ok(result.to_vec())
}

/// Decrypt a single 16-byte block with AES-256-ECB (no IV, no padding).
/// Used exclusively for the /Perms verification block.
fn aes256_ecb_decrypt_block(key: &[u8], block: &[u8]) -> Result<[u8; 16]> {
    use aes::cipher::BlockDecrypt;
    use aes::cipher::KeyInit;

    if key.len() != 32 {
        return Err(OxideError::MalformedPdf(format!(
            "AES-256-ECB: key must be 32 bytes, got {}",
            key.len()
        )));
    }
    if block.len() != 16 {
        return Err(OxideError::MalformedPdf(format!(
            "AES-256-ECB: block must be 16 bytes, got {}",
            block.len()
        )));
    }
    let cipher = aes::Aes256::new_from_slice(key)
        .map_err(|_| OxideError::MalformedPdf("AES-256-ECB: invalid key".to_string()))?;
    let mut out = aes::Block::clone_from_slice(block);
    cipher.decrypt_block(&mut out);
    Ok(out.into())
}

/// Encrypt a single 16-byte block with AES-128-ECB (no IV, no padding).
/// Used internally by the R6 hash (Algorithm 2.B step b).
fn aes128_ecb_encrypt_block(key: &[u8], block: &[u8]) -> Result<[u8; 16]> {
    use aes::cipher::BlockEncrypt;
    use aes::cipher::KeyInit;

    if key.len() != 16 {
        return Err(OxideError::MalformedPdf(format!(
            "AES-128-ECB: key must be 16 bytes, got {}",
            key.len()
        )));
    }
    if block.len() != 16 {
        return Err(OxideError::MalformedPdf(format!(
            "AES-128-ECB: block must be 16 bytes, got {}",
            block.len()
        )));
    }
    let cipher = aes::Aes128::new_from_slice(key)
        .map_err(|_| OxideError::MalformedPdf("AES-128-ECB: invalid key".to_string()))?;
    let mut out = aes::Block::clone_from_slice(block);
    cipher.encrypt_block(&mut out);
    Ok(out.into())
}

/// AES-128-CBC encrypt without padding, used inside Algorithm 2.B.
/// `key` is 16 bytes, `iv` is 16 bytes, `data` must be a multiple of 16 bytes.
fn aes128_cbc_encrypt_no_pad(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    // Manual CBC: each plaintext block XOR'd with previous ciphertext block, then ECB-encrypted.
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    for chunk in data.chunks(16) {
        let mut block = [0u8; 16];
        let len = chunk.len().min(16);
        block[..len].copy_from_slice(&chunk[..len]);
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        let enc = aes128_ecb_encrypt_block(key, &block).unwrap_or(block);
        out.extend_from_slice(&enc);
        prev = enc;
    }
    out
}

// ---------------------------------------------------------------------------
// 2.4  MD5 helper
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
// 2.5  SHA-2 helpers (V5/R5/R6)
// ---------------------------------------------------------------------------

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

fn sha384(data: &[u8]) -> [u8; 48] {
    use sha2::{Digest, Sha384};
    let mut h = Sha384::new();
    h.update(data);
    h.finalize().into()
}

fn sha512(data: &[u8]) -> [u8; 64] {
    use sha2::{Digest, Sha512};
    let mut h = Sha512::new();
    h.update(data);
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// 3  R6 hash: ISO 32000-2 Algorithm 2.B
// ---------------------------------------------------------------------------
//
// This is the iterated hash used by R6 (PDF 2.0) for key derivation. R5 uses
// plain SHA-256 instead (no iteration). The algorithm is:
//
//   K = SHA-256(input)
//   loop (at least 64 rounds, until termination condition):
//     a. K1 = (password || K [|| U_48_bytes if owner path]) repeated 64×
//     b. E  = AES-128-CBC-nopad(key=K[0..16], iv=K[16..32], data=K1)
//     c. selector = (sum of first 16 bytes of E) mod 3
//        K = SHA-256/384/512 of E  (chosen by selector)
//     d. stop when round >= 64 AND last_byte(E) <= (round - 32)
//   return first 32 bytes of K
//
// `password`  — UTF-8 password bytes (already truncated to 127 bytes by caller)
// `salt`      — 8-byte validation or key salt from /U or /O
// `u_entry`   — Some(48-byte /U) when computing an OWNER-path hash; None for user path
//
// This function is the foundation of both verify_v5_user_password and
// verify_v5_owner_password as well as the key-derivation steps.

pub fn r6_hash(password: &[u8], salt: &[u8], u_entry: Option<&[u8]>) -> [u8; 32] {
    // Step 1: K = SHA-256(password || salt [|| u_entry])
    let mut seed = Vec::with_capacity(password.len() + salt.len() + 48);
    seed.extend_from_slice(password);
    seed.extend_from_slice(salt);
    if let Some(u) = u_entry {
        seed.extend_from_slice(u);
    }
    let mut k: Vec<u8> = sha256(&seed).to_vec();

    let mut round = 0usize;
    loop {
        // Step a: K1 = (password || K [|| U]) × 64
        let unit_len = password.len() + k.len() + u_entry.map_or(0, |u| u.len());
        let mut k1 = Vec::with_capacity(unit_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            if let Some(u) = u_entry {
                k1.extend_from_slice(u);
            }
        }

        // Step b: E = AES-128-CBC-nopad(key=K[0..16], iv=K[16..32], data=K1)
        // K is at least 32 bytes after the initial SHA-256.
        let aes_key = &k[..16];
        let aes_iv = &k[16..32];
        let e = aes128_cbc_encrypt_no_pad(aes_key, aes_iv, &k1);

        // Step c: selector = (sum of first 16 bytes of E) mod 3
        let selector: u64 = e[..16].iter().map(|&b| b as u64).sum::<u64>() % 3;
        k = match selector {
            0 => sha256(&e).to_vec(),
            1 => sha384(&e).to_vec(),
            _ => sha512(&e).to_vec(),
        };

        round += 1;

        // Step d: stop when round >= 64 AND last_byte(E) <= (round - 32)
        let last_byte = *e.last().unwrap_or(&0) as usize;
        if round >= 64 && last_byte <= round - 32 {
            break;
        }

        // Safety cap: the spec is bounded, but guard against infinite loops
        // from malformed inputs (not reachable with valid PDFs).
        if round >= 256 {
            break;
        }
    }

    let mut result = [0u8; 32];
    result.copy_from_slice(&k[..32]);
    result
}

// ---------------------------------------------------------------------------
// 4.1  Encryption metadata
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
    /// AES-256 (`/CFM /AESV3`, V5).
    AesV3,
}

/// V5-specific fields from the `/Encrypt` dictionary (R5 / R6, PDF 2.0).
#[derive(Debug, Clone)]
pub struct V5Fields {
    /// `/OE` (32 bytes): owner-key-encrypted file key.
    pub oe: Vec<u8>,
    /// `/UE` (32 bytes): user-key-encrypted file key.
    pub ue: Vec<u8>,
    /// `/Perms` (16 bytes): encrypted permissions block.
    pub perms: Vec<u8>,
}

/// Parsed contents of a Standard Security Handler `/Encrypt` dictionary.
#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    /// Algorithm version: 1, 2, 4, or 5.
    pub v: u8,
    /// Revision of the Standard Security Handler: 2, 3, 4, 5, or 6.
    pub r: u8,
    /// Key length in bits (40 or 128 for V1-V4; always 256 for V5).
    pub key_length: usize,
    /// `/O` entry: owner-password verifier (32 bytes for R2–R4; 48 bytes for R5/R6).
    pub o: Vec<u8>,
    /// `/U` entry: user-password verifier (32 bytes for R2–R4; 48 bytes for R5/R6).
    pub u: Vec<u8>,
    /// `/P`: permission flags (signed 32-bit bitmask).
    pub p: i32,
    /// `/EncryptMetadata` (default true).
    pub encrypt_metadata: bool,
    /// Legacy/default crypt-filter method retained for callers that only need
    /// the ordinary-stream method.
    pub cf_method: CryptMethod,
    /// Crypt-filter method applied to ordinary streams (`/StmF`).
    pub stream_method: CryptMethod,
    /// Crypt-filter method applied to strings (`/StrF`).
    pub string_method: CryptMethod,
    /// Crypt-filter method applied to embedded-file streams (`/EFF`).
    pub embedded_file_method: CryptMethod,
    /// Named crypt filters from `/CF`, used by explicit `/Filter /Crypt`
    /// stream filters.
    pub crypt_filters: HashMap<String, CryptMethod>,
    /// V5-specific fields (present only when v == 5).
    pub v5: Option<V5Fields>,
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
            return Self::parse_v5(dict, r);
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
        if !(40..=128).contains(&key_length) || key_length % 8 != 0 {
            return Err(OxideError::MalformedPdf(format!(
                "V{v} encryption /Length must be 40..128 bits in 8-bit increments, got {key_length}"
            )));
        }

        let o = extract_bytes(dict, "O")?;
        let u = extract_bytes(dict, "U")?;
        let p = dict.get_integer("P").unwrap_or(-4) as i32;
        let encrypt_metadata = dict
            .get("EncryptMetadata")
            .and_then(PdfObject::as_bool)
            .unwrap_or(true);

        let crypt_filters = collect_crypt_filters(dict);
        let (stream_method, string_method, embedded_file_method) = if v == 4 {
            resolve_crypt_methods(dict, "Identity", CryptMethod::V2)?
        } else {
            (CryptMethod::V2, CryptMethod::V2, CryptMethod::V2)
        };
        let cf_method = stream_method.clone();

        Ok(EncryptionInfo {
            v,
            r,
            key_length,
            o,
            u,
            p,
            encrypt_metadata,
            cf_method,
            stream_method,
            string_method,
            embedded_file_method,
            crypt_filters,
            v5: None,
        })
    }

    fn parse_v5(dict: &PdfDictionary, r: u8) -> Result<Self> {
        if r != 5 && r != 6 {
            return Err(OxideError::MalformedPdf(format!(
                "V=5 requires R=5 or R=6, got R={r}"
            )));
        }

        let o = extract_bytes(dict, "O")?;
        let u = extract_bytes(dict, "U")?;

        // R5/R6 require exactly 48-byte O and U. Some writers pad to 32; be lenient
        // on length but warn, since the structure matters.
        if o.len() < 48 {
            return Err(OxideError::MalformedPdf(format!(
                "V5 /O must be at least 48 bytes, got {}",
                o.len()
            )));
        }
        if u.len() < 48 {
            return Err(OxideError::MalformedPdf(format!(
                "V5 /U must be at least 48 bytes, got {}",
                u.len()
            )));
        }

        let oe = extract_bytes(dict, "OE")?;
        let ue = extract_bytes(dict, "UE")?;
        let perms = extract_bytes(dict, "Perms")?;

        if oe.len() != 32 {
            return Err(OxideError::MalformedPdf(format!(
                "V5 /OE must be 32 bytes, got {}",
                oe.len()
            )));
        }
        if ue.len() != 32 {
            return Err(OxideError::MalformedPdf(format!(
                "V5 /UE must be 32 bytes, got {}",
                ue.len()
            )));
        }
        if perms.len() != 16 {
            return Err(OxideError::MalformedPdf(format!(
                "V5 /Perms must be 16 bytes, got {}",
                perms.len()
            )));
        }

        let p = dict.get_integer("P").unwrap_or(-4) as i32;
        let encrypt_metadata = dict
            .get("EncryptMetadata")
            .and_then(PdfObject::as_bool)
            .unwrap_or(true);

        let crypt_filters = collect_crypt_filters(dict);
        let (stream_method, string_method, embedded_file_method) =
            resolve_crypt_methods(dict, "StdCF", CryptMethod::AesV3)?;
        let cf_method = stream_method.clone();

        Ok(EncryptionInfo {
            v: 5,
            r,
            key_length: 256,
            o,
            u,
            p,
            encrypt_metadata,
            cf_method,
            stream_method,
            string_method,
            embedded_file_method,
            crypt_filters,
            v5: Some(V5Fields { oe, ue, perms }),
        })
    }

    /// True if this object stream/string is encrypted with AES-128 (V4/AESV2).
    pub fn is_aes(&self) -> bool {
        self.cf_method == CryptMethod::AesV2
    }

    /// True if this is a V5 (AES-256) encrypted document.
    pub fn is_v5(&self) -> bool {
        self.v == 5
    }
}

/// Resolve stream/string/embedded-file crypt-filter methods from `/StmF`,
/// `/StrF`, `/EFF`, and `/CF`. PDF 2.0 files commonly use `/StmF /Identity`
/// and `/StrF /Identity` while encrypting only embedded files via `/EFF`.
fn resolve_crypt_methods(
    dict: &PdfDictionary,
    default_filter: &str,
    default_unknown_method: CryptMethod,
) -> Result<(CryptMethod, CryptMethod, CryptMethod)> {
    let stream_filter = dict.get_name("StmF").unwrap_or(default_filter);
    let string_filter = dict.get_name("StrF").unwrap_or(stream_filter);
    let embedded_filter = dict.get_name("EFF").unwrap_or(stream_filter);

    Ok((
        resolve_named_crypt_method(dict, stream_filter, default_unknown_method.clone()),
        resolve_named_crypt_method(dict, string_filter, default_unknown_method.clone()),
        resolve_named_crypt_method(dict, embedded_filter, default_unknown_method),
    ))
}

fn resolve_named_crypt_method(
    dict: &PdfDictionary,
    filter_name: &str,
    default_unknown_method: CryptMethod,
) -> CryptMethod {
    if filter_name == "Identity" {
        return CryptMethod::None;
    }
    if let Some(cf) = dict.get_dict("CF") {
        if let Some(filter) = cf.get_dict(filter_name) {
            return match filter.get_name("CFM") {
                Some("AESV2") => CryptMethod::AesV2,
                Some("AESV3") => CryptMethod::AesV3,
                Some("V2") => CryptMethod::V2,
                Some("Identity") | None => CryptMethod::None,
                Some(other) => {
                    log::warn!("unknown crypt filter method /CFM /{other}; assuming RC4");
                    CryptMethod::V2
                }
            };
        }
    }

    match filter_name {
        "AESV2" => CryptMethod::AesV2,
        "AESV3" => CryptMethod::AesV3,
        _ => default_unknown_method,
    }
}

fn collect_crypt_filters(dict: &PdfDictionary) -> HashMap<String, CryptMethod> {
    let mut out = HashMap::new();
    let Some(cf) = dict.get_dict("CF") else {
        return out;
    };
    for (name, obj) in cf.iter() {
        let Some(filter) = obj.as_dict() else {
            continue;
        };
        let method = match filter.get_name("CFM") {
            Some("AESV2") => CryptMethod::AesV2,
            Some("AESV3") => CryptMethod::AesV3,
            Some("V2") => CryptMethod::V2,
            Some("Identity") | None => CryptMethod::None,
            Some(other) => {
                log::warn!("unknown crypt filter method /CFM /{other}; assuming RC4");
                CryptMethod::V2
            }
        };
        out.insert(name.clone(), method);
    }
    out
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
// 5  Key derivation — V1/V2/V4 (PDF 32000-1 §7.6.3.3)
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

/// Compute the file encryption key from a user password (V1/V2/V4).
///
/// Algorithm (PDF 32000-1 §7.6.3.3, Algorithm 2):
///   1. Hash `padded_password + O + P(LE 4 bytes) + file_id`, plus
///      `0xFFFFFFFF` when `R >= 4` and metadata is not encrypted.
///   2. MD5 the result.
///   3. For `R >= 3`, repeat MD5 over the first `n` bytes 50 more times.
///   4. Take the first `key_length / 8` bytes as the key.
pub fn compute_encryption_key(password: &[u8], info: &EncryptionInfo, file_id: &[u8]) -> Vec<u8> {
    let key_len = (info.key_length / 8).clamp(1, 16);
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

/// Verify that `password` matches the `/U` user-password verifier (V1/V2/V4).
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
// 6  Key derivation — V5 / R5 / R6 (ISO 32000-2 §7.6.4 / §7.6.5)
// ---------------------------------------------------------------------------

/// Truncate a V5 password to at most 127 UTF-8 bytes (SASLprep simplified).
/// Full SASLprep normalisation is not implemented; this handles the common
/// ASCII case correctly. Non-ASCII passwords may not interoperate with some
/// writers if they rely on Unicode normalisation.
fn truncate_v5_password(password: &[u8]) -> &[u8] {
    if password.len() <= 127 {
        password
    } else {
        // Truncate at a UTF-8 character boundary ≤ 127 bytes.
        let mut end = 127;
        while end > 0 && (password[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        &password[..end]
    }
}

/// Verify a user password against the V5 `/U` entry (Algorithm 2.A, user path).
///
/// Returns `true` if the password matches.
pub fn verify_v5_user_password(password: &[u8], info: &EncryptionInfo) -> bool {
    if info.v5.is_none() {
        return false;
    }
    let pwd = truncate_v5_password(password);
    // /U layout: [0..32] = hash, [32..40] = validation_salt, [40..48] = key_salt
    let validation_salt = &info.u[32..40];
    let computed = if info.r == 6 {
        r6_hash(pwd, validation_salt, None)
    } else {
        // R5: plain SHA-256(password || validation_salt)
        let mut input = Vec::with_capacity(pwd.len() + 8);
        input.extend_from_slice(pwd);
        input.extend_from_slice(validation_salt);
        sha256(&input)
    };
    // Compare first 32 bytes (the hash portion of /U)
    info.u.len() >= 32 && computed == info.u[..32].try_into().unwrap_or([0u8; 32])
}

/// Verify an owner password against the V5 `/O` entry (Algorithm 2.A, owner path).
///
/// Returns `true` if the owner password matches.
pub fn verify_v5_owner_password(password: &[u8], info: &EncryptionInfo) -> bool {
    if info.v5.is_none() {
        return false;
    }
    let pwd = truncate_v5_password(password);
    // /O layout: [0..32] = hash, [32..40] = validation_salt, [40..48] = key_salt
    let validation_salt = &info.o[32..40];
    // Owner path includes the full 48-byte /U value in the hash input.
    let u48 = &info.u[..48.min(info.u.len())];
    let computed = if info.r == 6 {
        r6_hash(pwd, validation_salt, Some(u48))
    } else {
        let mut input = Vec::with_capacity(pwd.len() + 8 + u48.len());
        input.extend_from_slice(pwd);
        input.extend_from_slice(validation_salt);
        input.extend_from_slice(u48);
        sha256(&input)
    };
    info.o.len() >= 32 && computed == info.o[..32].try_into().unwrap_or([0u8; 32])
}

/// Derive the 32-byte file encryption key from a verified user password (V5).
///
/// Algorithm 2.A step 4: intermediate_key = hash(password || U_key_salt),
/// then file_key = AES-256-CBC-decrypt(key=intermediate_key, iv=zero, data=UE).
pub fn derive_v5_file_key_from_user(password: &[u8], info: &EncryptionInfo) -> Result<Vec<u8>> {
    let v5 = info.v5.as_ref().ok_or_else(|| {
        OxideError::MalformedPdf("derive_v5_file_key_from_user called on non-V5 info".to_string())
    })?;
    let pwd = truncate_v5_password(password);
    // /U layout: [40..48] = key_salt
    let key_salt = &info.u[40..48];
    let intermediate_key = if info.r == 6 {
        r6_hash(pwd, key_salt, None)
    } else {
        let mut input = Vec::with_capacity(pwd.len() + 8);
        input.extend_from_slice(pwd);
        input.extend_from_slice(key_salt);
        sha256(&input)
    };
    // Decrypt /UE with zero IV (no padding — 32-byte payload is exactly 2 blocks).
    let zero_iv = [0u8; 16];
    // UE is 32 bytes = 2 AES blocks, no padding needed
    let file_key = aes256_cbc_no_pad(&intermediate_key, &zero_iv, &v5.ue)?;
    Ok(file_key[..32].to_vec())
}

/// Derive the 32-byte file encryption key from a verified owner password (V5).
///
/// Algorithm 2.A step 7: intermediate_key = hash(password || O_key_salt || U_48),
/// then file_key = AES-256-CBC-decrypt(key=intermediate_key, iv=zero, data=OE).
pub fn derive_v5_file_key_from_owner(password: &[u8], info: &EncryptionInfo) -> Result<Vec<u8>> {
    let v5 = info.v5.as_ref().ok_or_else(|| {
        OxideError::MalformedPdf("derive_v5_file_key_from_owner called on non-V5 info".to_string())
    })?;
    let pwd = truncate_v5_password(password);
    // /O layout: [40..48] = key_salt
    let key_salt = &info.o[40..48];
    let u48 = &info.u[..48.min(info.u.len())];
    let intermediate_key = if info.r == 6 {
        r6_hash(pwd, key_salt, Some(u48))
    } else {
        let mut input = Vec::with_capacity(pwd.len() + 8 + u48.len());
        input.extend_from_slice(pwd);
        input.extend_from_slice(key_salt);
        input.extend_from_slice(u48);
        sha256(&input)
    };
    let zero_iv = [0u8; 16];
    let file_key = aes256_cbc_no_pad(&intermediate_key, &zero_iv, &v5.oe)?;
    Ok(file_key[..32].to_vec())
}

/// Verify the /Perms block and return `true` if the magic bytes are correct.
///
/// ISO 32000-2 §7.6.4.4: decrypt the 16-byte /Perms with AES-256-ECB using the
/// file key. Bytes `[9]`, `[10]`, `[11]` of the result must be ASCII 'a', 'd', 'b'.
pub fn verify_v5_perms(file_key: &[u8], info: &EncryptionInfo) -> bool {
    let v5 = match &info.v5 {
        Some(v) => v,
        None => return false,
    };
    let block = match aes256_ecb_decrypt_block(file_key, &v5.perms) {
        Ok(b) => b,
        Err(_) => return false,
    };
    block[9] == b'a' && block[10] == b'd' && block[11] == b'b'
}

// ---------------------------------------------------------------------------
// 7  Object-level decryption
// ---------------------------------------------------------------------------

/// Compute the per-object decryption key (V1/V2/V4 only — NOT used for V5).
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
/// For V5 (`is_v5 = true`): the file key is used DIRECTLY (no per-object
/// derivation), and the IV is the first 16 bytes of the ciphertext.
///
/// For V1/V2/V4: a per-object key is derived via [`object_key`].
///
/// On any AES failure the original bytes are returned unchanged, so a single
/// corrupt object can never poison the rest of the document.
pub fn decrypt_string(
    data: &[u8],
    file_key: &[u8],
    obj_num: u32,
    gen_num: u16,
    is_aes: bool,
    is_v5: bool,
) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    if is_v5 {
        // V5: use file_key directly with AES-256-CBC; IV prepended.
        aes256_cbc_decrypt(file_key, data).unwrap_or_else(|_| data.to_vec())
    } else {
        let key = object_key(file_key, obj_num, gen_num, is_aes);
        if is_aes {
            aes128_cbc_decrypt(&key, data).unwrap_or_else(|_| data.to_vec())
        } else {
            Rc4::apply(&key, data)
        }
    }
}

/// Decrypt a PDF stream's raw bytes. Streams use the same logic as strings.
pub fn decrypt_stream(
    data: &[u8],
    file_key: &[u8],
    obj_num: u32,
    gen_num: u16,
    is_aes: bool,
    is_v5: bool,
) -> Vec<u8> {
    decrypt_string(data, file_key, obj_num, gen_num, is_aes, is_v5)
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
        let hash = md5(b"abc");
        assert_eq!(hash[0], 0x90, "first byte of MD5(abc)");
        assert_eq!(hash[1], 0x01, "second byte of MD5(abc)");
    }

    // --- SHA-256 ---

    #[test]
    fn sha256_empty_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let h = sha256(b"");
        assert_eq!(h[0], 0xe3);
        assert_eq!(h[1], 0xb0);
        assert_eq!(h[31], 0x55);
    }

    // --- Key derivation (V1/V2/V4) ---

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
            stream_method: CryptMethod::V2,
            string_method: CryptMethod::V2,
            embedded_file_method: CryptMethod::V2,
            crypt_filters: HashMap::new(),
            v5: None,
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
            stream_method: CryptMethod::V2,
            string_method: CryptMethod::V2,
            embedded_file_method: CryptMethod::V2,
            crypt_filters: HashMap::new(),
            v5: None,
        };
        let fid = vec![0u8; 16];
        assert_eq!(compute_encryption_key(b"", &make_info(40), &fid).len(), 5);
        assert_eq!(compute_encryption_key(b"", &make_info(128), &fid).len(), 16);
    }

    #[test]
    fn compute_encryption_key_defensively_caps_invalid_legacy_length() {
        let info = EncryptionInfo {
            v: 4,
            r: 4,
            key_length: 256,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            cf_method: CryptMethod::V2,
            stream_method: CryptMethod::V2,
            string_method: CryptMethod::V2,
            embedded_file_method: CryptMethod::V2,
            crypt_filters: HashMap::new(),
            v5: None,
        };

        let key = compute_encryption_key(b"", &info, b"short-id");
        assert_eq!(key.len(), 16);
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
            stream_method: CryptMethod::V2,
            string_method: CryptMethod::V2,
            embedded_file_method: CryptMethod::V2,
            crypt_filters: HashMap::new(),
            v5: None,
        };
        let fid = vec![0xABu8; 16];
        let k_empty = compute_encryption_key(b"", &info, &fid);
        let k_secret = compute_encryption_key(b"secret", &info, &fid);
        assert_ne!(k_empty, k_secret);
    }

    // --- AES-128 ---

    #[test]
    fn aes128_cbc_round_trip_with_iv_prefix() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let key = [0x00u8; 16];
        let iv = [0x00u8; 16];
        let plain = b"0123456789ABCDEF";

        let mut buf = plain.to_vec();
        buf.resize(32, 0);
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

    // --- AES-256 ---

    #[test]
    fn aes256_cbc_round_trip() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plain = b"AES-256 PDF round-trip test data";

        let mut buf = plain.to_vec();
        buf.resize(plain.len() + 16, 0);
        let ct_len = Aes256CbcEnc::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .len();
        let ciphertext = &buf[..ct_len];

        let mut pdf_ct = iv.to_vec();
        pdf_ct.extend_from_slice(ciphertext);

        let decrypted = aes256_cbc_decrypt(&key, &pdf_ct).unwrap();
        assert_eq!(decrypted, plain.to_vec());
    }

    #[test]
    fn aes256_wrong_key_length_errors() {
        let ct = vec![0u8; 32];
        assert!(aes256_cbc_decrypt(&[0u8; 16], &ct).is_err());
    }

    #[test]
    fn aes256_ciphertext_shorter_than_iv_errors() {
        assert!(aes256_cbc_decrypt(&[0u8; 32], b"too short").is_err());
    }

    // --- R6 hash (Algorithm 2.B) ---

    #[test]
    fn r6_hash_empty_password_is_deterministic() {
        let salt = [0x01u8; 8];
        let h1 = r6_hash(b"", &salt, None);
        let h2 = r6_hash(b"", &salt, None);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    #[test]
    fn r6_hash_different_salts_differ() {
        let s1 = [0x01u8; 8];
        let s2 = [0x02u8; 8];
        let h1 = r6_hash(b"password", &s1, None);
        let h2 = r6_hash(b"password", &s2, None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn r6_hash_owner_path_differs_from_user_path() {
        let salt = [0xABu8; 8];
        let u_entry = [0u8; 48];
        let h_user = r6_hash(b"password", &salt, None);
        let h_owner = r6_hash(b"password", &salt, Some(&u_entry));
        assert_ne!(h_user, h_owner);
    }

    #[test]
    fn r6_hash_different_passwords_differ() {
        let salt = [0x77u8; 8];
        let h1 = r6_hash(b"", &salt, None);
        let h2 = r6_hash(b"userpass", &salt, None);
        assert_ne!(h1, h2);
    }

    // --- V5 verify / key-derive (self-consistent round-trip) ---

    fn make_v5_info(r: u8, password: &[u8]) -> EncryptionInfo {
        // Build a self-consistent V5 EncryptionInfo from scratch so we can
        // verify the verify + derive path is round-trip correct.
        use aes::cipher::{block_padding::NoPadding, BlockEncryptMut, KeyIvInit};
        type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

        let file_key: [u8; 32] = {
            let mut k = [0u8; 32];
            for (i, b) in k.iter_mut().enumerate() {
                *b = (i as u8).wrapping_add(0x11);
            }
            k
        };

        let zero_iv = [0u8; 16];

        // /U: hash(pwd || v_salt) || v_salt || k_salt
        let u_v_salt = [0x10u8, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17];
        let u_k_salt = [0x20u8, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27];

        let u_hash = if r == 6 {
            r6_hash(password, &u_v_salt, None)
        } else {
            let mut input = password.to_vec();
            input.extend_from_slice(&u_v_salt);
            sha256(&input)
        };

        let mut u = Vec::with_capacity(48);
        u.extend_from_slice(&u_hash);
        u.extend_from_slice(&u_v_salt);
        u.extend_from_slice(&u_k_salt);

        // /UE: AES-256-CBC(key=ue_intermediate, iv=zero, data=file_key), no padding
        let ue_intermediate = if r == 6 {
            r6_hash(password, &u_k_salt, None)
        } else {
            let mut input = password.to_vec();
            input.extend_from_slice(&u_k_salt);
            sha256(&input)
        };
        let ue = {
            let mut buf = file_key.to_vec();
            buf.resize(32, 0);
            Aes256CbcEnc::new_from_slices(&ue_intermediate, &zero_iv)
                .unwrap()
                .encrypt_padded_mut::<NoPadding>(&mut buf, 32)
                .unwrap()
                .to_vec()
        };

        // /O: hash(pwd || o_v_salt || U48) || o_v_salt || o_k_salt
        let o_v_salt = [0x30u8, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37];
        let o_k_salt = [0x40u8, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47];
        let u48 = &u[..48];

        let o_hash = if r == 6 {
            r6_hash(password, &o_v_salt, Some(u48))
        } else {
            let mut input = password.to_vec();
            input.extend_from_slice(&o_v_salt);
            input.extend_from_slice(u48);
            sha256(&input)
        };

        let mut o = Vec::with_capacity(48);
        o.extend_from_slice(&o_hash);
        o.extend_from_slice(&o_v_salt);
        o.extend_from_slice(&o_k_salt);

        // /OE: AES-256-CBC(key=o_intermediate, iv=0, data=file_key)
        let oe_intermediate = if r == 6 {
            r6_hash(password, &o_k_salt, Some(u48))
        } else {
            let mut input = password.to_vec();
            input.extend_from_slice(&o_k_salt);
            input.extend_from_slice(u48);
            sha256(&input)
        };
        let oe = {
            let mut buf = file_key.to_vec();
            buf.resize(32, 0);
            Aes256CbcEnc::new_from_slices(&oe_intermediate, &zero_iv)
                .unwrap()
                .encrypt_padded_mut::<NoPadding>(&mut buf, 32)
                .unwrap()
                .to_vec()
        };

        // /Perms: AES-256-ECB(key=file_key, plaintext)
        // plaintext: [0..4]=P LE, [4..8]=0xFF, [8]=T/F encrypt_metadata, [9..12]="adb", [12..16]=zeros
        let mut perms_plain = [0u8; 16];
        let p: i32 = -4;
        perms_plain[..4].copy_from_slice(&(p as u32).to_le_bytes());
        perms_plain[4..8].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        perms_plain[8] = b'T';
        perms_plain[9] = b'a';
        perms_plain[10] = b'd';
        perms_plain[11] = b'b';

        let perms = {
            use aes::cipher::{BlockEncrypt, KeyInit};
            let cipher = aes::Aes256::new_from_slice(&file_key).unwrap();
            let mut block = aes::Block::clone_from_slice(&perms_plain);
            cipher.encrypt_block(&mut block);
            block.to_vec()
        };

        EncryptionInfo {
            v: 5,
            r,
            key_length: 256,
            o,
            u,
            p,
            encrypt_metadata: true,
            cf_method: CryptMethod::AesV3,
            stream_method: CryptMethod::AesV3,
            string_method: CryptMethod::AesV3,
            embedded_file_method: CryptMethod::AesV3,
            crypt_filters: HashMap::new(),
            v5: Some(V5Fields { oe, ue, perms }),
        }
    }

    #[test]
    fn v5_r6_user_password_verify_and_key_derive() {
        let password = b"userpass";
        let info = make_v5_info(6, password);

        assert!(
            verify_v5_user_password(password, &info),
            "user password should verify"
        );
        assert!(
            !verify_v5_user_password(b"wrongpass", &info),
            "wrong password should fail"
        );

        let file_key = derive_v5_file_key_from_user(password, &info).unwrap();
        assert_eq!(file_key.len(), 32);
        assert!(
            verify_v5_perms(&file_key, &info),
            "perms should verify with correct file key"
        );
        assert!(
            !verify_v5_perms(&[0u8; 32], &info),
            "perms should fail with wrong key"
        );
    }

    #[test]
    fn v5_r5_user_password_verify_and_key_derive() {
        let password = b"";
        let info = make_v5_info(5, password);

        assert!(
            verify_v5_user_password(password, &info),
            "empty user password should verify for R5"
        );
        assert!(
            !verify_v5_user_password(b"wrong", &info),
            "wrong password should fail for R5"
        );

        let file_key = derive_v5_file_key_from_user(password, &info).unwrap();
        assert_eq!(file_key.len(), 32);
        assert!(verify_v5_perms(&file_key, &info), "perms R5");
    }

    #[test]
    fn v5_owner_password_verify_and_key_derive() {
        let password = b"ownerpass";
        let info = make_v5_info(6, password);

        assert!(
            verify_v5_owner_password(password, &info),
            "owner password should verify"
        );
        assert!(
            !verify_v5_owner_password(b"wrong", &info),
            "wrong owner password should fail"
        );

        let file_key = derive_v5_file_key_from_owner(password, &info).unwrap();
        assert_eq!(file_key.len(), 32);
        assert!(
            verify_v5_perms(&file_key, &info),
            "perms should verify via owner key"
        );
    }

    #[test]
    fn v5_decrypt_string_round_trip() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

        let file_key = [0x55u8; 32];
        let iv = [0xAAu8; 16];
        let plain = b"Hello AES-256 PDF string";

        let mut buf = plain.to_vec();
        buf.resize(plain.len() + 16, 0);
        let ct_len = Aes256CbcEnc::new_from_slices(&file_key, &iv)
            .unwrap()
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .len();

        let mut ciphertext = iv.to_vec();
        ciphertext.extend_from_slice(&buf[..ct_len]);

        let decrypted = decrypt_string(&ciphertext, &file_key, 1, 0, false, true);
        assert_eq!(decrypted, plain.to_vec());
    }

    // --- verify_user_password (V1/V2/V4) ---

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
            stream_method: CryptMethod::V2,
            string_method: CryptMethod::V2,
            embedded_file_method: CryptMethod::V2,
            crypt_filters: HashMap::new(),
            v5: None,
        };
        let file_id = vec![0x01u8; 16];
        assert!(!verify_user_password(b"wrong-password", &info, &file_id));
    }

    // --- decrypt_string (V1/V2/V4) ---

    #[test]
    fn decrypt_string_empty_data_returns_empty() {
        let result = decrypt_string(b"", &[0x01u8; 5], 1, 0, false, false);
        assert!(result.is_empty());
    }

    #[test]
    fn decrypt_string_rc4_round_trip() {
        let file_key = vec![0x2Bu8, 0xE9, 0xF7, 0xC3, 0xD5];
        let original = b"Hello PDF World";
        let key = object_key(&file_key, 7, 0, false);
        let encrypted = Rc4::apply(&key, original);
        let decrypted = decrypt_string(&encrypted, &file_key, 7, 0, false, false);
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
    fn from_dict_v5_r6_parses_successfully() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(5)),
            ("R", PdfObject::Integer(6)),
            ("O", PdfObject::String(vec![0u8; 48])),
            ("U", PdfObject::String(vec![0u8; 48])),
            ("OE", PdfObject::String(vec![0u8; 32])),
            ("UE", PdfObject::String(vec![0u8; 32])),
            ("Perms", PdfObject::String(vec![0u8; 16])),
            ("P", PdfObject::Integer(-4)),
        ]);
        let info = EncryptionInfo::from_dict(&d).unwrap();
        assert_eq!(info.v, 5);
        assert_eq!(info.r, 6);
        assert_eq!(info.key_length, 256);
        assert!(info.v5.is_some());
        assert_eq!(info.cf_method, CryptMethod::AesV3);
        assert!(info.is_v5());
    }

    #[test]
    fn from_dict_v5_r5_parses_successfully() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(5)),
            ("R", PdfObject::Integer(5)),
            ("O", PdfObject::String(vec![1u8; 48])),
            ("U", PdfObject::String(vec![2u8; 48])),
            ("OE", PdfObject::String(vec![3u8; 32])),
            ("UE", PdfObject::String(vec![4u8; 32])),
            ("Perms", PdfObject::String(vec![5u8; 16])),
            ("P", PdfObject::Integer(-3904)),
        ]);
        let info = EncryptionInfo::from_dict(&d).unwrap();
        assert_eq!(info.v, 5);
        assert_eq!(info.r, 5);
        assert!(info.is_v5());
    }

    #[test]
    fn from_dict_v5_bad_r_errors() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(5)),
            ("R", PdfObject::Integer(4)),
            ("O", PdfObject::String(vec![0u8; 48])),
            ("U", PdfObject::String(vec![0u8; 48])),
            ("OE", PdfObject::String(vec![0u8; 32])),
            ("UE", PdfObject::String(vec![0u8; 32])),
            ("Perms", PdfObject::String(vec![0u8; 16])),
            ("P", PdfObject::Integer(-4)),
        ]);
        assert!(matches!(
            EncryptionInfo::from_dict(&d),
            Err(OxideError::MalformedPdf(_))
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
        assert!(!info.is_v5());
    }

    #[test]
    fn from_dict_rejects_invalid_legacy_key_length() {
        let d = dict(&[
            ("Filter", PdfObject::Name("Standard".to_string())),
            ("V", PdfObject::Integer(4)),
            ("R", PdfObject::Integer(4)),
            ("Length", PdfObject::Integer(256)),
            ("O", PdfObject::String(vec![1u8; 32])),
            ("U", PdfObject::String(vec![2u8; 32])),
            ("P", PdfObject::Integer(-4)),
        ]);

        assert!(matches!(
            EncryptionInfo::from_dict(&d),
            Err(OxideError::MalformedPdf(_))
        ));
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
        assert!(!info.is_v5());
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
