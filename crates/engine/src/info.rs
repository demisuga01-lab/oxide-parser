//! Document information reporting (`pdfinfo`-equivalent) and font analysis
//! (`pdffonts`-equivalent).
//!
//! These are pure **reporting** facilities: they aggregate and format data the
//! engine already parses (the `/Info` dictionary, catalog, page tree,
//! `/Encrypt` dictionary, and font resources) and never re-implement parsing.
//! Both structs derive `serde::Serialize` so the CLI can emit either a
//! human-readable report or `--json`.

use serde::Serialize;

use crate::crypto::{CryptMethod, EncryptionInfo};
use crate::document::PdfDocument;
use crate::error::Result;
use crate::fonts::encoding::Encoding;
use crate::object::{PdfDictionary, PdfObject};

// ---------------------------------------------------------------------------
// Document info
// ---------------------------------------------------------------------------

/// A page size in PostScript points, with the count of pages that have it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageSize {
    /// Width in points (MediaBox x2 - x0), rounded to 2 decimals.
    pub width_pts: f64,
    /// Height in points (MediaBox y3 - y1), rounded to 2 decimals.
    pub height_pts: f64,
    /// Page rotation in degrees (0/90/180/270); 0 unless the page is rotated.
    pub rotation: i32,
    /// Number of pages with exactly this (width, height, rotation).
    pub page_count: usize,
}

/// Decoded `/Encrypt` reporting block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EncryptionReport {
    /// Algorithm label, e.g. "RC4 40-bit", "AES-128", "AES-256".
    pub algorithm: String,
    /// `/V` (algorithm version) and `/R` (handler revision).
    pub version: u8,
    pub revision: u8,
    /// Key length in bits.
    pub key_length_bits: usize,
    /// The raw signed `/P` permission bitmask.
    pub permission_bits: i32,
    /// Human-readable permission flags (printing/copying/etc.), each `allowed`.
    pub permissions: Permissions,
}

/// Decoded `/P` permission flags (PDF 32000-1 Table 22). Bit positions are
/// 1-based in the spec; we mask the 0-based equivalents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Permissions {
    /// Bit 3: print the document (possibly degraded — see `high_quality_print`).
    pub print: bool,
    /// Bit 4: modify the document (other than the cases below).
    pub modify: bool,
    /// Bit 5: copy/extract text and graphics.
    pub copy: bool,
    /// Bit 6: add/modify annotations and fill form fields.
    pub annotate: bool,
    /// Bit 9: fill existing form fields (even if bit 6 is clear).
    pub fill_forms: bool,
    /// Bit 10: extract text/graphics for accessibility.
    pub extract_accessibility: bool,
    /// Bit 11: assemble (insert/rotate/delete pages).
    pub assemble: bool,
    /// Bit 12: high-quality printing (if clear, printing is degraded).
    pub high_quality_print: bool,
}

impl Permissions {
    /// Decode the standard permission bits from a signed `/P` value.
    pub fn from_p(p: i32) -> Self {
        let bit = |n: u32| (p & (1 << (n - 1))) != 0;
        Permissions {
            print: bit(3),
            modify: bit(4),
            copy: bit(5),
            annotate: bit(6),
            fill_forms: bit(9),
            extract_accessibility: bit(10),
            assemble: bit(11),
            high_quality_print: bit(12),
        }
    }
}

/// Everything the `info` tool reports about a document. Optional `/Info` fields
/// are `None` when absent (never an error).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DocumentInfo {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    /// Creation date: human-readable form (e.g. "2024-01-15 10:30:00 +00'00'").
    pub creation_date: Option<String>,
    /// Creation date: the raw `D:...` string as stored.
    pub creation_date_raw: Option<String>,
    pub mod_date: Option<String>,
    pub mod_date_raw: Option<String>,
    /// Effective PDF version (catalog `/Version` overrides the header).
    pub pdf_version: String,
    pub page_count: usize,
    /// Distinct page sizes. One entry when all pages match; more when they vary.
    pub page_sizes: Vec<PageSize>,
    /// True when pages have more than one distinct size/rotation.
    pub page_size_varies: bool,
    /// `true` if `/MarkInfo /Marked true` or a `/StructTreeRoot` is present.
    pub tagged: bool,
    /// Best-effort linearization ("fast web view") detection.
    pub linearized: bool,
    pub encrypted: bool,
    pub encryption: Option<EncryptionReport>,
    /// File identifier (`/ID[0]`) as an uppercase hex string, if present.
    pub file_id: Option<String>,
    /// Whether an XMP `/Metadata` stream is present in the catalog.
    pub has_xmp_metadata: bool,
    pub file_size_bytes: usize,
}

impl DocumentInfo {
    /// Gather a [`DocumentInfo`] from an already-opened (and decrypted) document.
    pub fn gather(doc: &PdfDocument) -> Result<Self> {
        let reader = doc.reader();
        let mut info = DocumentInfo {
            pdf_version: reader.version().to_string(),
            file_size_bytes: reader.file_size(),
            encrypted: reader.is_encrypted(),
            ..Default::default()
        };

        // /Info dictionary (all optional).
        if let Some((num, gen)) = reader.info_reference() {
            if let Ok(PdfObject::Dictionary(dict)) = reader.get_and_resolve(num, gen) {
                info.title = text_field(&dict, "Title");
                info.author = text_field(&dict, "Author");
                info.subject = text_field(&dict, "Subject");
                info.keywords = text_field(&dict, "Keywords");
                info.creator = text_field(&dict, "Creator");
                info.producer = text_field(&dict, "Producer");
                if let Some(raw) = text_field(&dict, "CreationDate") {
                    info.creation_date = Some(format_pdf_date(&raw));
                    info.creation_date_raw = Some(raw);
                }
                if let Some(raw) = text_field(&dict, "ModDate") {
                    info.mod_date = Some(format_pdf_date(&raw));
                    info.mod_date_raw = Some(raw);
                }
            }
        }

        // Catalog: effective version, tagged, XMP.
        if let Ok(catalog) = doc.get_catalog() {
            if let Some(version) = catalog.get_name("Version") {
                // A catalog /Version overrides the header version (PDF 32000-1
                // §7.5.5) when it is later; report it as effective.
                info.pdf_version = version.to_string();
            }
            info.tagged = is_tagged(&catalog, reader);
            info.has_xmp_metadata = catalog.contains_key("Metadata");
        }

        // Pages: count + distinct sizes.
        let pages = doc.get_pages()?;
        info.page_count = pages.len();
        info.page_sizes = aggregate_page_sizes(&pages);
        info.page_size_varies = info.page_sizes.len() > 1;

        // File identifier.
        info.file_id = reader.first_file_id().map(|bytes| to_hex_upper(&bytes));

        // Linearization (best effort).
        info.linearized = detect_linearized(reader);

        // Encryption details.
        if info.encrypted {
            if let Some(encrypt_dict) = reader.encrypt_dictionary() {
                if let Ok(enc) = EncryptionInfo::from_dict(&encrypt_dict) {
                    info.encryption = Some(build_encryption_report(&enc));
                }
            }
        }

        Ok(info)
    }
}

fn build_encryption_report(enc: &EncryptionInfo) -> EncryptionReport {
    let algorithm = if enc.is_v5() {
        "AES-256".to_string()
    } else {
        match enc.cf_method {
            CryptMethod::AesV2 => "AES-128".to_string(),
            CryptMethod::AesV3 => "AES-256".to_string(),
            CryptMethod::V2 => format!("RC4 {}-bit", enc.key_length),
            CryptMethod::None => format!("RC4 {}-bit", enc.key_length),
        }
    };
    EncryptionReport {
        algorithm,
        version: enc.v,
        revision: enc.r,
        key_length_bits: enc.key_length,
        permission_bits: enc.p,
        permissions: Permissions::from_p(enc.p),
    }
}

/// True if the catalog marks the document as tagged: `/MarkInfo /Marked true`
/// or a present `/StructTreeRoot`.
fn is_tagged(catalog: &PdfDictionary, reader: &crate::reader::PdfReader) -> bool {
    if catalog.contains_key("StructTreeRoot") {
        return true;
    }
    if let Some(mark_info) = catalog.get("MarkInfo") {
        let resolved = reader.resolve(mark_info.clone()).unwrap_or(PdfObject::Null);
        if let PdfObject::Dictionary(dict) = resolved {
            return dict.get_bool("Marked").unwrap_or(false);
        }
    }
    false
}

/// Best-effort linearization detection: a linearized PDF places a
/// linearization parameter dictionary (an indirect object with an `/Linearized`
/// key) as the very first object, right after the header. We scan the first
/// chunk of the file for the `/Linearized` marker.
fn detect_linearized(reader: &crate::reader::PdfReader) -> bool {
    let bytes = reader.file_bytes();
    let prefix = &bytes[..bytes.len().min(1024)];
    prefix
        .windows(b"/Linearized".len())
        .any(|window| window == b"/Linearized")
}

/// Collapse a page list into distinct `(width, height, rotation)` sizes,
/// preserving first-seen order, each with a page count.
fn aggregate_page_sizes(pages: &[crate::document::PdfPage]) -> Vec<PageSize> {
    let mut out: Vec<PageSize> = Vec::new();
    for page in pages {
        let w = round2(page.media_box[2] - page.media_box[0]);
        let h = round2(page.media_box[3] - page.media_box[1]);
        let rotation = page.rotate;
        if let Some(existing) = out
            .iter_mut()
            .find(|s| s.width_pts == w && s.height_pts == h && s.rotation == rotation)
        {
            existing.page_count += 1;
        } else {
            out.push(PageSize {
                width_pts: w,
                height_pts: h,
                rotation,
                page_count: 1,
            });
        }
    }
    out
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// PDF text-string decoding (/Info fields)
// ---------------------------------------------------------------------------

/// Read a `/Info` string field and decode it from PDF text-string form.
///
/// Per PDF 32000-1 §7.9.2.2, a text string is either UTF-16BE (introduced by a
/// `FE FF` byte-order mark) or PDFDocEncoding. We detect the BOM; otherwise we
/// decode each byte through PDFDocEncoding.
fn text_field(dict: &PdfDictionary, key: &str) -> Option<String> {
    let bytes = match dict.get(key)? {
        PdfObject::String(bytes) => bytes,
        _ => return None,
    };
    let decoded = decode_pdf_text_string(bytes);
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

/// Decode a PDF text string (UTF-16BE-with-BOM or PDFDocEncoding) to Rust UTF-8.
pub fn decode_pdf_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE with BOM.
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| {
                let hi = c[0];
                let lo = c.get(1).copied().unwrap_or(0);
                (u16::from(hi) << 8) | u16::from(lo)
            })
            .collect();
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect()
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        // UTF-16LE BOM — not standard for PDF but some producers emit it.
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| {
                let lo = c[0];
                let hi = c.get(1).copied().unwrap_or(0);
                (u16::from(hi) << 8) | u16::from(lo)
            })
            .collect();
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect()
    } else {
        // PDFDocEncoding: map each byte to its Unicode value. Printable ASCII is
        // identity; the high range follows the PDFDocEncoding glyph table.
        decode_pdf_doc_encoding(bytes)
    }
}

fn decode_pdf_doc_encoding(bytes: &[u8]) -> String {
    use crate::fonts::glyph_list::glyph_name_to_unicode;
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if (0x20..=0x7E).contains(&b) {
            out.push(b as char);
        } else {
            let name = Encoding::lookup("PDFDocEncoding", b);
            match glyph_name_to_unicode(name) {
                Some(ch) => out.push(ch),
                None => {
                    // Unmapped control bytes (tab/newline) pass through; others drop.
                    if b == b'\t' || b == b'\n' || b == b'\r' {
                        out.push(b as char);
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// PDF date parsing
// ---------------------------------------------------------------------------

/// Format a PDF date string `D:YYYYMMDDHHmmSSOHH'mm'` into a readable form
/// `YYYY-MM-DD HH:MM:SS ±HH'mm'`. Returns the input verbatim if it doesn't
/// parse (PDF dates are loosely specified and many fields are optional).
pub fn format_pdf_date(raw: &str) -> String {
    let s = raw.strip_prefix("D:").unwrap_or(raw);
    let digits: Vec<char> = s.chars().collect();

    // Helper to read N digits starting at index i, defaulting if absent.
    let read = |start: usize, len: usize, default: &str| -> String {
        if start + len <= digits.len()
            && digits[start..start + len]
                .iter()
                .all(|c| c.is_ascii_digit())
        {
            digits[start..start + len].iter().collect()
        } else {
            default.to_string()
        }
    };

    // Year is mandatory for a meaningful parse; if absent, return raw.
    if digits.len() < 4 || !digits[0..4].iter().all(|c| c.is_ascii_digit()) {
        return raw.to_string();
    }

    let year = read(0, 4, "0000");
    let month = read(4, 2, "01");
    let day = read(6, 2, "01");
    let hour = read(8, 2, "00");
    let minute = read(10, 2, "00");
    let second = read(12, 2, "00");

    // Timezone: at index 14 there may be 'Z', '+', or '-' then HH'mm'.
    let tz = if digits.len() > 14 {
        match digits[14] {
            'Z' => "+00'00'".to_string(),
            '+' | '-' => {
                let sign = digits[14];
                let tz_h = read(15, 2, "00");
                // PDF separates with apostrophes: +HH'mm'
                let tz_m = read(18, 2, "00");
                format!("{sign}{tz_h}'{tz_m}'")
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };

    let base = format!("{year}-{month}-{day} {hour}:{minute}:{second}");
    if tz.is_empty() {
        base
    } else {
        format!("{base} {tz}")
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    fn permissions_decode_all_set() {
        // -1 (all bits set) => everything allowed.
        let p = Permissions::from_p(-1);
        assert!(p.print && p.modify && p.copy && p.annotate);
        assert!(p.fill_forms && p.extract_accessibility && p.assemble && p.high_quality_print);
    }

    #[test]
    fn permissions_decode_print_only() {
        // Bit 3 set (print) only: value = 1 << 2 = 4.
        let p = Permissions::from_p(4);
        assert!(p.print);
        assert!(!p.copy);
        assert!(!p.modify);
    }

    #[test]
    fn pdf_date_full_with_tz() {
        assert_eq!(
            format_pdf_date("D:20240115103000+05'30'"),
            "2024-01-15 10:30:00 +05'30'"
        );
    }

    #[test]
    fn pdf_date_zulu() {
        assert_eq!(
            format_pdf_date("D:20240115103000Z"),
            "2024-01-15 10:30:00 +00'00'"
        );
    }

    #[test]
    fn pdf_date_partial_no_tz() {
        assert_eq!(format_pdf_date("D:202401"), "2024-01-01 00:00:00");
    }

    #[test]
    fn pdf_date_unparseable_returns_raw() {
        assert_eq!(format_pdf_date("not a date"), "not a date");
    }

    #[test]
    fn text_string_utf16be_bom() {
        // FE FF then UTF-16BE "Hi"
        let bytes = vec![0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
        assert_eq!(decode_pdf_text_string(&bytes), "Hi");
    }

    #[test]
    fn text_string_utf16be_non_ascii() {
        // "café": c a f then U+00E9
        let bytes = vec![0xFE, 0xFF, 0x00, b'c', 0x00, b'a', 0x00, b'f', 0x00, 0xE9];
        assert_eq!(decode_pdf_text_string(&bytes), "café");
    }

    #[test]
    fn text_string_pdfdoc_ascii() {
        assert_eq!(decode_pdf_text_string(b"Hello World"), "Hello World");
    }

    #[test]
    fn hex_upper_roundtrip() {
        assert_eq!(to_hex_upper(&[0x00, 0xAB, 0xFF]), "00ABFF");
    }

    fn enc(v: u8, r: u8, key_length: usize, cf: CryptMethod) -> EncryptionInfo {
        EncryptionInfo {
            v,
            r,
            key_length,
            o: vec![0; 32],
            u: vec![0; 32],
            p: -4,
            encrypt_metadata: true,
            cf_method: cf.clone(),
            stream_method: cf.clone(),
            string_method: cf.clone(),
            embedded_file_method: cf,
            crypt_filters: std::collections::HashMap::new(),
            v5: None,
        }
    }

    #[test]
    fn encryption_report_rc4_label() {
        // V2/R3, 128-bit RC4.
        let report = build_encryption_report(&enc(2, 3, 128, CryptMethod::V2));
        assert_eq!(report.algorithm, "RC4 128-bit");
        assert_eq!(report.version, 2);
        assert_eq!(report.revision, 3);

        // V1/R2, 40-bit RC4.
        let report = build_encryption_report(&enc(1, 2, 40, CryptMethod::V2));
        assert_eq!(report.algorithm, "RC4 40-bit");
    }

    #[test]
    fn encryption_report_aes128_label() {
        // V4/R4 with AESV2 crypt filter.
        let report = build_encryption_report(&enc(4, 4, 128, CryptMethod::AesV2));
        assert_eq!(report.algorithm, "AES-128");
        assert_eq!(report.version, 4);
    }

    #[test]
    fn encryption_report_aes256_label() {
        let mut info = enc(5, 6, 256, CryptMethod::AesV3);
        info.v5 = Some(crate::crypto::V5Fields {
            oe: vec![0; 32],
            ue: vec![0; 32],
            perms: vec![0; 16],
        });
        let report = build_encryption_report(&info);
        assert_eq!(report.algorithm, "AES-256");
        assert_eq!(report.version, 5);
        assert_eq!(report.revision, 6);
    }
}
