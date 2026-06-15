//! Digital signature verification (`pdfsig`-equivalent).
//!
//! Read/verify only — no PDF writing. For each signature field this:
//!   1. reads the `/ByteRange` and hashes those exact original file bytes,
//!   2. parses `/Contents` as a PKCS#7 / CMS `SignedData` (RFC 5652),
//!   3. verifies the signer's RSA signature over the signed attributes (or the
//!      content digest directly), checking the `messageDigest` signed-attribute
//!      against the byte-range digest, and
//!   4. reports the signer certificate's details and whether the signature
//!      covers the whole file or the document was modified after signing.
//!
//! # Honest scope
//!
//! "Valid" here means **cryptographically valid**: the signature math checks
//! out against the signer certificate's public key and the signed digest
//! matches the `/ByteRange` bytes. This round does **NOT** perform trust-chain
//! validation to a trusted root CA, nor revocation (OCSP/CRL) checking, nor
//! certificate-validity-period enforcement as a pass/fail gate (the validity
//! dates are reported, not enforced). RSA (PKCS#1 v1.5) signatures are
//! supported; ECDSA/EdDSA and RSA-PSS are not yet (reported as
//! `unsupported_algorithm`). Timestamp tokens (RFC 3161) are not verified.

use cms::cert::x509::Certificate;
use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerInfo};
use const_oid::ObjectIdentifier;
use der::{Decode, Encode};
use serde::Serialize;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::document::PdfDocument;
use crate::error::Result;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

// OIDs we care about.
const OID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const OID_RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const OID_SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
const OID_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const OID_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const OID_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");

/// Overall cryptographic verdict for one signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureValidity {
    /// Signature math verifies and the signed digest matches the byte ranges.
    Valid,
    /// Parsed fine but the signature/digest does not verify (tampering or
    /// wrong key) — the document content within the signed ranges changed, or
    /// the signature is corrupt.
    Invalid,
    /// The signature algorithm is not supported (e.g. ECDSA, RSA-PSS).
    UnsupportedAlgorithm,
    /// The signature dictionary or CMS blob could not be parsed.
    Error,
}

/// How much of the file the signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// The `/ByteRange` (its two ranges plus the `/Contents` gap) spans the
    /// entire file — nothing was appended after signing.
    WholeFile,
    /// Bytes exist after the signed ranges — an incremental update was appended
    /// after this signature, i.e. the document was modified after signing.
    ModifiedAfterSigning,
}

/// Signer certificate details (reported, not trust-validated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub serial_hex: String,
    pub not_before: String,
    pub not_after: String,
}

/// A single signature field's verification report.
#[derive(Debug, Clone, Serialize)]
pub struct SignatureReport {
    /// 1-based signature index in discovery order.
    pub index: usize,
    /// Field name (`/T`), if present.
    pub field_name: Option<String>,
    /// `/Name` (signer name as stated in the signature dict), if present.
    pub signer_name: Option<String>,
    /// `/M` signing time (raw PDF date string), if present.
    pub signing_time: Option<String>,
    pub reason: Option<String>,
    pub location: Option<String>,
    pub contact_info: Option<String>,
    /// `/SubFilter` (e.g. adbe.pkcs7.detached, ETSI.CAdES.detached).
    pub sub_filter: Option<String>,
    /// Digest algorithm named in the CMS, e.g. "SHA-256".
    pub digest_algorithm: Option<String>,
    pub validity: SignatureValidity,
    pub coverage: Coverage,
    /// Signer certificate details (when a cert was present and parsed).
    pub certificate: Option<CertInfo>,
    /// Human-readable note on what was/wasn't checked.
    pub note: String,
}

/// Verify every signature field in the document.
pub fn verify_signatures(doc: &PdfDocument) -> Result<Vec<SignatureReport>> {
    let reader = doc.reader();
    let file = reader.file_bytes();
    let mut reports = Vec::new();

    for (idx, field) in find_signature_fields(doc).into_iter().enumerate() {
        reports.push(verify_one(&field, file, idx + 1));
    }
    Ok(reports)
}

/// A located signature field with its signature dictionary.
struct SigField {
    field_name: Option<String>,
    sig_dict: PdfDictionary,
}

/// Walk `/AcroForm /Fields` collecting `/FT /Sig` fields that carry a `/V`
/// signature dictionary. Inherited `/FT` and nested `/Kids` are handled.
fn find_signature_fields(doc: &PdfDocument) -> Vec<SigField> {
    let reader = doc.reader();
    let mut out = Vec::new();
    let Ok(catalog) = doc.get_catalog() else {
        return out;
    };
    let Some(acroform) = resolve_dict(catalog.get("AcroForm"), reader) else {
        return out;
    };
    let Some(fields) = resolve_array(acroform.get("Fields"), reader) else {
        return out;
    };
    let mut visited = std::collections::HashSet::new();
    for field in &fields {
        walk_field(field, None, reader, &mut out, &mut visited, 0);
    }
    out
}

fn walk_field(
    field_obj: &PdfObject,
    inherited_ft: Option<&str>,
    reader: &PdfReader,
    out: &mut Vec<SigField>,
    visited: &mut std::collections::HashSet<u32>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    if let Some((num, _)) = field_obj.as_reference() {
        if !visited.insert(num) {
            return;
        }
    }
    let Ok(PdfObject::Dictionary(field)) = reader.resolve(field_obj.clone()) else {
        return;
    };
    let ft = field.get_name("FT").or(inherited_ft);

    // A signature field: /FT /Sig with a /V signature dictionary.
    if ft == Some("Sig") {
        if let Some(sig_dict) = resolve_dict(field.get("V"), reader) {
            out.push(SigField {
                field_name: field.get("T").and_then(decode_text_string),
                sig_dict,
            });
        }
    }

    // Recurse into /Kids.
    if let Some(kids) = resolve_array(field.get("Kids"), reader) {
        for kid in &kids {
            walk_field(kid, ft, reader, out, visited, depth + 1);
        }
    }
}

fn verify_one(field: &SigField, file: &[u8], index: usize) -> SignatureReport {
    let sig = &field.sig_dict;
    let mut report = SignatureReport {
        index,
        field_name: field.field_name.clone(),
        signer_name: sig.get("Name").and_then(decode_text_string),
        signing_time: sig.get("M").and_then(decode_text_string),
        reason: sig.get("Reason").and_then(decode_text_string),
        location: sig.get("Location").and_then(decode_text_string),
        contact_info: sig.get("ContactInfo").and_then(decode_text_string),
        sub_filter: sig.get_name("SubFilter").map(str::to_string),
        digest_algorithm: None,
        validity: SignatureValidity::Error,
        coverage: Coverage::ModifiedAfterSigning,
        certificate: None,
        note: String::new(),
    };

    // /ByteRange = [a b c d]; signed data = file[a..a+b] ++ file[c..c+d].
    let byte_range = match parse_byte_range(sig) {
        Some(br) => br,
        None => {
            report.note = "missing or malformed /ByteRange".to_string();
            return report;
        }
    };

    let signed_data_bytes = match extract_signed_bytes(file, &byte_range) {
        Some(b) => b,
        None => {
            report.note = "/ByteRange out of bounds for file".to_string();
            return report;
        }
    };

    // Coverage: do the ranges + the /Contents gap reach the end of the file?
    report.coverage = compute_coverage(&byte_range, file.len());

    // /Contents = DER CMS blob (a hex/binary string).
    let contents = match sig.get("Contents").and_then(PdfObject::as_string) {
        Some(c) => c.to_vec(),
        None => {
            report.note = "missing /Contents".to_string();
            return report;
        }
    };

    match verify_cms(&contents, &signed_data_bytes) {
        Ok(result) => {
            report.validity = result.validity;
            report.digest_algorithm = result.digest_algorithm;
            report.certificate = result.certificate;
        }
        Err(msg) => {
            report.validity = SignatureValidity::Error;
            report.note = msg;
            return report;
        }
    }

    report.note = scope_note(&report.validity);
    report
}

fn scope_note(validity: &SignatureValidity) -> String {
    let base = match validity {
        SignatureValidity::Valid => "cryptographically valid signature over the signed byte ranges",
        SignatureValidity::Invalid => "signature/digest did not verify (content within the signed ranges changed, or the signature is corrupt)",
        SignatureValidity::UnsupportedAlgorithm => "signature algorithm not supported (only RSA PKCS#1 v1.5 is verified)",
        SignatureValidity::Error => "could not verify",
    };
    format!(
        "{base}. NOT checked: trust chain to a root CA, revocation (OCSP/CRL), \
         certificate validity period, and timestamp tokens."
    )
}

struct CmsResult {
    validity: SignatureValidity,
    digest_algorithm: Option<String>,
    certificate: Option<CertInfo>,
}

/// Verify a detached CMS SignedData blob over `content` (the signed file bytes).
fn verify_cms(der: &[u8], content: &[u8]) -> std::result::Result<CmsResult, String> {
    // Trim trailing zero padding PDF writers add to fill the /Contents slot.
    let der = trim_trailing_zeros(der);

    let ci = ContentInfo::from_der(der).map_err(|e| format!("CMS parse: {e}"))?;
    let signed: SignedData = ci
        .content
        .decode_as()
        .map_err(|e| format!("SignedData decode: {e}"))?;

    let signer = signed
        .signer_infos
        .0
        .as_slice()
        .first()
        .ok_or_else(|| "no SignerInfo in CMS".to_string())?;

    let digest_oid = signer.digest_alg.oid;
    let digest_name = digest_oid_name(&digest_oid);

    // Find the signer certificate.
    let cert = find_signer_cert(&signed, signer);
    let cert_info = cert.as_ref().map(cert_to_info);

    // Compute the digest of the signed content.
    let content_digest = match digest_bytes(&digest_oid, content) {
        Some(d) => d,
        None => {
            return Ok(CmsResult {
                validity: SignatureValidity::UnsupportedAlgorithm,
                digest_algorithm: digest_name,
                certificate: cert_info,
            })
        }
    };

    // Determine what is actually signed:
    //  - with signed attributes: messageDigest attr must equal content_digest,
    //    and the signature is over DER(SET OF signed attributes);
    //  - without: the signature is over the content directly (its digest).
    let (signed_payload_digest_input, attrs_ok) = match &signer.signed_attrs {
        Some(attrs) => {
            // messageDigest attribute check.
            let md_matches = signed_attr_message_digest(attrs)
                .map(|md| md == content_digest.as_slice())
                .unwrap_or(false);
            // The signature input is the DER re-encoding of the attributes as
            // an explicit SET OF (tag 0x31), per RFC 5652 §5.4.
            let der_attrs = match reencode_signed_attrs_as_set(attrs) {
                Some(b) => b,
                None => return Err("could not re-encode signed attributes".to_string()),
            };
            (der_attrs, md_matches)
        }
        None => {
            // No signed attrs: signature is over the content's digest directly.
            (content.to_vec(), true)
        }
    };

    if !attrs_ok {
        // messageDigest mismatch ⇒ the signed content doesn't match the bytes.
        return Ok(CmsResult {
            validity: SignatureValidity::Invalid,
            digest_algorithm: digest_name,
            certificate: cert_info,
        });
    }

    // Verify the RSA signature over the payload, using the cert's public key.
    let Some(cert) = cert else {
        return Err("no signer certificate in CMS".to_string());
    };

    // Only RSA PKCS#1 v1.5 is supported this round.
    let sig_alg = signer.signature_algorithm.oid;
    if sig_alg != OID_RSA_ENCRYPTION && sig_alg != OID_SHA256_RSA && sig_alg != OID_SHA1_RSA
        && sig_alg != OID_SHA384_RSA && sig_alg != OID_SHA512_RSA
    {
        return Ok(CmsResult {
            validity: SignatureValidity::UnsupportedAlgorithm,
            digest_algorithm: digest_name,
            certificate: cert_info,
        });
    }

    let validity = match verify_rsa(&cert, &digest_oid, &signed_payload_digest_input, signer.signature.as_bytes()) {
        Ok(true) => SignatureValidity::Valid,
        Ok(false) => SignatureValidity::Invalid,
        Err(_) => SignatureValidity::Invalid,
    };

    Ok(CmsResult {
        validity,
        digest_algorithm: digest_name,
        certificate: cert_info,
    })
}

// RSA-with-hash signature algorithm OIDs (some PDFs name these instead of plain rsaEncryption).
const OID_SHA1_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.5");
const OID_SHA256_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const OID_SHA384_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const OID_SHA512_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");

/// Verify an RSA PKCS#1 v1.5 signature: `RSA_verify(pubkey, H(payload), sig)`.
fn verify_rsa(
    cert: &Certificate,
    digest_oid: &ObjectIdentifier,
    payload: &[u8],
    signature: &[u8],
) -> std::result::Result<bool, String> {
    use rsa::pkcs1v15::Pkcs1v15Sign;
    use rsa::RsaPublicKey;

    let spki = &cert.tbs_certificate.subject_public_key_info;
    let spki_der = spki.to_der().map_err(|e| format!("spki encode: {e}"))?;
    let pubkey = RsaPublicKey::try_from(
        spki::SubjectPublicKeyInfoRef::try_from(spki_der.as_slice())
            .map_err(|e| format!("spki parse: {e}"))?,
    )
    .map_err(|e| format!("rsa key: {e}"))?;

    // The signature is over H(payload); pick the scheme matching the digest OID
    // (it prepends the correct DigestInfo prefix internally).
    let ok = match *digest_oid {
        OID_SHA256 => {
            let h = Sha256::digest(payload);
            pubkey.verify(Pkcs1v15Sign::new::<Sha256>(), &h, signature).is_ok()
        }
        OID_SHA384 => {
            let h = Sha384::digest(payload);
            pubkey.verify(Pkcs1v15Sign::new::<Sha384>(), &h, signature).is_ok()
        }
        OID_SHA512 => {
            let h = Sha512::digest(payload);
            pubkey.verify(Pkcs1v15Sign::new::<Sha512>(), &h, signature).is_ok()
        }
        OID_SHA1 => {
            let h = Sha1::digest(payload);
            pubkey.verify(Pkcs1v15Sign::new::<Sha1>(), &h, signature).is_ok()
        }
        _ => return Err("unsupported digest".to_string()),
    };
    Ok(ok)
}

fn digest_bytes(oid: &ObjectIdentifier, data: &[u8]) -> Option<Vec<u8>> {
    Some(match *oid {
        OID_SHA256 => Sha256::digest(data).to_vec(),
        OID_SHA384 => Sha384::digest(data).to_vec(),
        OID_SHA512 => Sha512::digest(data).to_vec(),
        OID_SHA1 => Sha1::digest(data).to_vec(),
        _ => return None,
    })
}

fn digest_oid_name(oid: &ObjectIdentifier) -> Option<String> {
    Some(
        match *oid {
            OID_SHA256 => "SHA-256",
            OID_SHA384 => "SHA-384",
            OID_SHA512 => "SHA-512",
            OID_SHA1 => "SHA-1",
            _ => return None,
        }
        .to_string(),
    )
}

/// Extract the `messageDigest` signed-attribute value (an OCTET STRING).
fn signed_attr_message_digest(attrs: &x509_cert::attr::Attributes) -> Option<Vec<u8>> {
    for attr in attrs.iter() {
        if attr.oid == OID_MESSAGE_DIGEST {
            let any = attr.values.as_slice().first()?;
            // The value is an OCTET STRING; its inner bytes are the digest.
            let octets = der::asn1::OctetString::from_der(&any.to_der().ok()?).ok()?;
            return Some(octets.as_bytes().to_vec());
        }
    }
    None
}

/// Re-encode the signed attributes as an explicit `SET OF Attribute` (tag
/// 0x31) for signature verification. In the CMS structure they are stored
/// IMPLICIT [0]; the signature is computed over the EXPLICIT SET encoding.
fn reencode_signed_attrs_as_set(attrs: &x509_cert::attr::Attributes) -> Option<Vec<u8>> {
    // `Attributes` is a SetOfVec<Attribute>; DER-encoding it yields the SET OF
    // body. der's encoder writes it with the SET tag (0x31) already.
    let der = attrs.to_der().ok()?;
    // Ensure the leading tag is SET (0x31), not [0] (0xA0). `to_der` on the
    // SetOfVec emits SET, which is exactly what we need.
    if der.first() == Some(&0x31) {
        Some(der)
    } else {
        // Defensive: force the SET tag.
        let mut fixed = der.clone();
        if let Some(b) = fixed.first_mut() {
            *b = 0x31;
        }
        Some(fixed)
    }
}

/// Find the certificate in the SignedData whose issuer+serial (or SKI) matches
/// the SignerInfo's `sid`. Falls back to the first certificate.
fn find_signer_cert(signed: &SignedData, signer: &SignerInfo) -> Option<Certificate> {
    let certs = signed.certificates.as_ref()?;
    use cms::cert::CertificateChoices;
    use cms::signed_data::SignerIdentifier;

    let mut first: Option<Certificate> = None;
    for choice in certs.0.iter() {
        if let CertificateChoices::Certificate(cert) = choice {
            if first.is_none() {
                first = Some(cert.clone());
            }
            if let SignerIdentifier::IssuerAndSerialNumber(ias) = &signer.sid {
                if cert.tbs_certificate.serial_number == ias.serial_number
                    && cert.tbs_certificate.issuer == ias.issuer
                {
                    return Some(cert.clone());
                }
            }
        }
    }
    first
}

fn cert_to_info(cert: &Certificate) -> CertInfo {
    let tbs = &cert.tbs_certificate;
    CertInfo {
        subject: tbs.subject.to_string(),
        issuer: tbs.issuer.to_string(),
        serial_hex: hex_upper(tbs.serial_number.as_bytes()),
        not_before: tbs.validity.not_before.to_string(),
        not_after: tbs.validity.not_after.to_string(),
    }
}

// ---------------------------------------------------------------------------
// ByteRange + coverage
// ---------------------------------------------------------------------------

/// `/ByteRange` as four offsets `[a, b, c, d]`.
struct ByteRange {
    a: usize,
    b: usize,
    c: usize,
    d: usize,
}

fn parse_byte_range(sig: &PdfDictionary) -> Option<ByteRange> {
    let arr = sig.get_array("ByteRange")?;
    if arr.len() != 4 {
        return None;
    }
    let n = |i: usize| -> Option<usize> {
        arr[i].as_integer().filter(|v| *v >= 0).map(|v| v as usize)
    };
    Some(ByteRange {
        a: n(0)?,
        b: n(1)?,
        c: n(2)?,
        d: n(3)?,
    })
}

fn extract_signed_bytes(file: &[u8], br: &ByteRange) -> Option<Vec<u8>> {
    let end1 = br.a.checked_add(br.b)?;
    let end2 = br.c.checked_add(br.d)?;
    if end1 > file.len() || end2 > file.len() || br.c < end1 {
        return None;
    }
    let mut out = Vec::with_capacity(br.b + br.d);
    out.extend_from_slice(&file[br.a..end1]);
    out.extend_from_slice(&file[br.c..end2]);
    Some(out)
}

/// The signature covers the whole file iff the second range ends at (or within
/// a trailing-whitespace margin of) EOF. Trailing bytes beyond it mean a later
/// incremental update was appended after signing.
fn compute_coverage(br: &ByteRange, file_len: usize) -> Coverage {
    let signed_end = br.c.saturating_add(br.d);
    // Allow a tiny trailing-whitespace/newline margin some writers leave.
    if signed_end + 3 >= file_len {
        Coverage::WholeFile
    } else {
        Coverage::ModifiedAfterSigning
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn trim_trailing_zeros(der: &[u8]) -> &[u8] {
    let mut end = der.len();
    while end > 0 && der[end - 1] == 0 {
        end -= 1;
    }
    &der[..end]
}

fn resolve_dict(obj: Option<&PdfObject>, reader: &PdfReader) -> Option<PdfDictionary> {
    match obj? {
        PdfObject::Dictionary(d) => Some(d.clone()),
        r @ PdfObject::Reference { .. } => match reader.resolve(r.clone()).ok()? {
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_array(obj: Option<&PdfObject>, reader: &PdfReader) -> Option<Vec<PdfObject>> {
    match obj? {
        PdfObject::Array(a) => Some(a.clone()),
        r @ PdfObject::Reference { .. } => match reader.resolve(r.clone()).ok()? {
            PdfObject::Array(a) => Some(a),
            _ => None,
        },
        _ => None,
    }
}

fn decode_text_string(obj: &PdfObject) -> Option<String> {
    match obj {
        PdfObject::String(bytes) => {
            let s = crate::info::decode_pdf_text_string(bytes);
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

fn hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn br(a: usize, b: usize, c: usize, d: usize) -> ByteRange {
        ByteRange { a, b, c, d }
    }

    #[test]
    fn extract_signed_bytes_concatenates_two_ranges() {
        let file = b"AAAA<SIG>BBBB".to_vec();
        // range1 = [0,4) "AAAA"; gap [4,9) is the <SIG>; range2 = [9,13) "BBBB"
        let bytes = extract_signed_bytes(&file, &br(0, 4, 9, 4)).unwrap();
        assert_eq!(bytes, b"AAAABBBB");
    }

    #[test]
    fn extract_signed_bytes_rejects_out_of_bounds() {
        let file = b"short".to_vec();
        assert!(extract_signed_bytes(&file, &br(0, 100, 0, 0)).is_none());
    }

    #[test]
    fn coverage_whole_file_vs_modified() {
        // Signed end reaches EOF -> whole file.
        assert_eq!(compute_coverage(&br(0, 4, 9, 4), 13), Coverage::WholeFile);
        // Trailing bytes after signed end -> modified after signing.
        assert_eq!(
            compute_coverage(&br(0, 4, 9, 4), 100),
            Coverage::ModifiedAfterSigning
        );
    }

    #[test]
    fn trim_trailing_zeros_works() {
        assert_eq!(trim_trailing_zeros(&[1, 2, 3, 0, 0]), &[1, 2, 3]);
        assert_eq!(trim_trailing_zeros(&[1, 2, 3]), &[1, 2, 3]);
    }

    #[test]
    fn hex_upper_formats() {
        assert_eq!(hex_upper(&[0x0a, 0xff, 0x10]), "0AFF10");
    }
}
