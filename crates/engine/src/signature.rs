//! Digital signature creation and verification (`pdfsig`-equivalent).
//!
//! Signing appends a signature field and detached CMS `SignedData` in an
//! incremental-update revision, preserving the original file bytes as an exact
//! prefix. Verification of each signature field:
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
//! matches the `/ByteRange` bytes. LTV support embeds RFC 3161 timestamp
//! tokens supplied by the caller, writes PAdES-style DSS validation material
//! (`/Certs`, `/OCSPs`, `/CRLs`, `/VRI`) as an incremental update, and reports
//! the PAdES baseline level implied by embedded material. Trust-chain
//! validation to a configured root, live TSA/OCSP/CRL fetching, OCSP response
//! policy validation, and timestamp imprint/TSA trust are intentionally outside
//! this core primitive. RSA (PKCS#1 v1.5) signatures are supported;
//! ECDSA/EdDSA and RSA-PSS are not yet (reported as `unsupported_algorithm`).

use cms::builder::{create_signing_time_attribute, SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{x509::Certificate, CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::ContentInfo;
use cms::signed_data::{EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo};
use const_oid::ObjectIdentifier;
use der::asn1::SetOfVec;
use der::{Decode, DecodePem, Encode};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use serde::Serialize;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
use spki::AlgorithmIdentifierOwned;
use x509_cert::attr::{Attribute, AttributeValue};
use x509_cert::crl::CertificateList;

use crate::document::PdfDocument;
use crate::error::{OxideError, Result};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::writer::{serialize_object, write_incremental_update_raw, RawIncrementalObject};

// OIDs we care about.
const OID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const OID_RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const OID_SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
const OID_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const OID_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const OID_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");
const OID_SIGNATURE_TIMESTAMP_TOKEN: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");

const BYTE_RANGE_PLACEHOLDER: &[u8] = b"[9999999999 9999999999 9999999999 9999999999]";
const MAX_BYTE_RANGE_FIELD: u64 = 9_999_999_999;
const DEFAULT_CONTENTS_RESERVED_BYTES: usize = 16 * 1024;

/// RSA signing identity used by [`sign_document`].
///
/// The first certificate is the signer certificate; remaining certificates are
/// embedded as chain material for validators. The current writer applies RSA
/// PKCS#1 v1.5 with SHA-256.
#[derive(Clone)]
pub struct PdfSigner {
    private_key: RsaPrivateKey,
    certificates: Vec<Certificate>,
}

impl PdfSigner {
    /// Build a signer from DER-encoded RSA private key and X.509 certificates.
    ///
    /// `private_key_der` may be PKCS#8 or PKCS#1. `certificate_der` is the
    /// signer certificate; `chain_der` are optional issuer certificates.
    pub fn from_der(
        private_key_der: &[u8],
        certificate_der: &[u8],
        chain_der: &[&[u8]],
    ) -> Result<Self> {
        let private_key = RsaPrivateKey::from_pkcs8_der(private_key_der)
            .or_else(|_| RsaPrivateKey::from_pkcs1_der(private_key_der))
            .map_err(|e| OxideError::UnsupportedFeature(format!("signature RSA key: {e}")))?;
        let mut certificates = Vec::with_capacity(chain_der.len() + 1);
        certificates.push(
            Certificate::from_der(certificate_der)
                .map_err(|e| OxideError::MalformedPdf(format!("signature certificate: {e}")))?,
        );
        for cert in chain_der {
            certificates.push(Certificate::from_der(cert).map_err(|e| {
                OxideError::MalformedPdf(format!("signature chain certificate: {e}"))
            })?);
        }
        Ok(Self {
            private_key,
            certificates,
        })
    }

    /// Build a signer from PEM-encoded RSA private key and X.509 certificates.
    pub fn from_pem(
        private_key_pem: &str,
        certificate_pem: &str,
        chain_pem: &[&str],
    ) -> Result<Self> {
        let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
            .map_err(|e| OxideError::UnsupportedFeature(format!("signature RSA key: {e}")))?;
        let mut certificates = Vec::with_capacity(chain_pem.len() + 1);
        certificates.push(
            Certificate::from_pem(certificate_pem.as_bytes())
                .map_err(|e| OxideError::MalformedPdf(format!("signature certificate PEM: {e}")))?,
        );
        for cert in chain_pem {
            certificates.push(Certificate::from_pem(cert.as_bytes()).map_err(|e| {
                OxideError::MalformedPdf(format!("signature chain certificate PEM: {e}"))
            })?);
        }
        Ok(Self {
            private_key,
            certificates,
        })
    }

    pub fn signer_certificate(&self) -> &Certificate {
        &self.certificates[0]
    }
}

/// Options for [`sign_document`].
#[derive(Debug, Clone)]
pub struct SignatureOptions {
    /// Signature field name (`/T`). Defaults to `Sig1`.
    pub field_name: String,
    /// 1-based page for the visible widget. Defaults to page 1.
    pub page: usize,
    /// Widget rectangle `[x0, y0, x1, y1]`. `None` creates an invisible field.
    pub rect: Option<[f64; 4]>,
    pub signer_name: Option<String>,
    pub reason: Option<String>,
    pub location: Option<String>,
    pub contact_info: Option<String>,
    /// Raw PDF date string for `/M`, e.g. `D:20260622000000Z`.
    pub signing_time: Option<String>,
    /// DER-encoded RFC 3161 `TimeStampToken` (`ContentInfo`) to embed as the
    /// CMS `signatureTimeStampToken` unsigned attribute.
    ///
    /// The core signer does not contact a TSA. Callers that need PAdES-B-T
    /// obtain a token from their TSA/policy layer and pass the DER token here.
    /// Verification reports the token's presence and parseability; imprint and
    /// TSA trust validation are intentionally left to the caller's trust policy.
    pub timestamp_token_der: Option<Vec<u8>>,
    /// Reserved CMS size in bytes. The DER CMS must fit in this placeholder.
    pub contents_reserved_bytes: usize,
}

impl Default for SignatureOptions {
    fn default() -> Self {
        Self {
            field_name: "Sig1".to_string(),
            page: 1,
            rect: None,
            signer_name: None,
            reason: None,
            location: None,
            contact_info: None,
            signing_time: None,
            timestamp_token_der: None,
            contents_reserved_bytes: DEFAULT_CONTENTS_RESERVED_BYTES,
        }
    }
}

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

/// PAdES baseline level inferred from the signature and embedded LTV material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PadesLevel {
    /// Core CMS signature only.
    BaselineB,
    /// CMS signature contains a parseable RFC 3161 timestamp token.
    BaselineT,
    /// Timestamp plus matching DSS validation material (`/Certs` and
    /// `/OCSPs` or `/CRLs`) is embedded for offline validation.
    BaselineLT,
    /// Document/archive timestamp over the DSS. Not emitted by the current
    /// writer, but reported for future-compatible readers if detected later.
    BaselineLTA,
}

/// Revocation status derived from embedded DSS material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationStatus {
    /// No embedded revocation material was available.
    NotChecked,
    /// OCSP/CRL bytes were embedded but this verifier did not derive a
    /// definitive signer status from them.
    EmbeddedMaterial,
    /// A parseable embedded CRL did not list the signer certificate serial.
    GoodFromEmbeddedCrl,
    /// A parseable embedded CRL listed the signer certificate serial.
    RevokedByEmbeddedCrl,
    /// Revocation material was present but malformed or not usable.
    Unknown,
}

/// Long-term-validation material supplied by a caller and embedded in `/DSS`.
///
/// `signature_index` is 1-based and matches [`SignatureReport::index`]. When
/// omitted, the material is associated with every signature in the document.
/// Certificate DER is supplemented with the signer certificates already present
/// in each CMS signature so the DSS always carries the signer chain known to
/// Oxide.
#[derive(Debug, Clone, Default)]
pub struct LtvMaterial {
    pub signature_index: Option<usize>,
    pub certificates_der: Vec<Vec<u8>>,
    pub ocsp_responses_der: Vec<Vec<u8>>,
    pub crls_der: Vec<Vec<u8>>,
}

impl LtvMaterial {
    fn is_empty(&self) -> bool {
        self.certificates_der.is_empty()
            && self.ocsp_responses_der.is_empty()
            && self.crls_der.is_empty()
    }
}

/// Per-signature LTV/PAdES validation report.
#[derive(Debug, Clone, Serialize)]
pub struct LtvReport {
    pub pades_level: PadesLevel,
    pub timestamp_token_count: usize,
    pub invalid_timestamp_token_count: usize,
    pub dss_present: bool,
    pub vri_key: Option<String>,
    pub vri_matched: bool,
    pub embedded_certs: usize,
    pub embedded_ocsp_responses: usize,
    pub embedded_crls: usize,
    pub revocation_status: RevocationStatus,
    pub note: String,
}

impl Default for LtvReport {
    fn default() -> Self {
        Self {
            pades_level: PadesLevel::BaselineB,
            timestamp_token_count: 0,
            invalid_timestamp_token_count: 0,
            dss_present: false,
            vri_key: None,
            vri_matched: false,
            embedded_certs: 0,
            embedded_ocsp_responses: 0,
            embedded_crls: 0,
            revocation_status: RevocationStatus::NotChecked,
            note: "no timestamp token or DSS validation material found".to_string(),
        }
    }
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
    /// PAdES/LTV material discovered for this signature.
    pub ltv: LtvReport,
    /// Human-readable note on what was/wasn't checked.
    pub note: String,
}

/// Verify every signature field in the document.
pub fn verify_signatures(doc: &PdfDocument) -> Result<Vec<SignatureReport>> {
    let reader = doc.reader();
    let file = reader.file_bytes();
    let dss = read_dss_index(reader);
    let mut reports = Vec::new();

    for (idx, field) in find_signature_fields(doc).into_iter().enumerate() {
        reports.push(verify_one(&field, file, idx + 1, &dss));
    }
    Ok(reports)
}

/// Embed PAdES long-term-validation material in a catalog `/DSS` dictionary.
///
/// This is an incremental update: original bytes stay untouched, existing
/// signatures remain cryptographically valid for their signed byte ranges, and
/// their coverage will correctly report `modified_after_signing` after the DSS
/// append. The writer embeds caller-supplied OCSP/CRL bytes as opaque streams
/// and adds signer certificates already present in CMS signatures.
pub fn add_ltv_material(doc: &PdfDocument, material: &LtvMaterial) -> Result<Vec<u8>> {
    let reader = doc.reader();
    if reader.is_encrypted() {
        return Err(OxideError::UnsupportedFeature(
            "embedding LTV/DSS material in encrypted inputs is not yet supported".to_string(),
        ));
    }
    if material.is_empty() {
        return Err(OxideError::MalformedPdf(
            "LTV/DSS material must include at least one cert, OCSP response, or CRL".to_string(),
        ));
    }

    let fields = find_signature_fields(doc);
    if fields.is_empty() {
        return Err(OxideError::MalformedPdf(
            "LTV/DSS embedding requires at least one signature field".to_string(),
        ));
    }

    if let Some(index) = material.signature_index {
        if index == 0 || index > fields.len() {
            return Err(OxideError::MalformedPdf(format!(
                "LTV signature_index {index} is out of range for {} signature(s)",
                fields.len()
            )));
        }
    }

    let selected = fields
        .iter()
        .enumerate()
        .filter(|(idx, _)| material.signature_index.is_none_or(|n| n == idx + 1))
        .collect::<Vec<_>>();

    let (root_number, root_generation) = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("LTV/DSS writer: trailer is missing /Root".to_string())
    })?;
    let mut catalog = reader
        .get_object(root_number, root_generation)?
        .as_dict()
        .cloned()
        .ok_or_else(|| OxideError::MalformedPdf("/Root is not a dictionary".to_string()))?;

    let mut certs = material.certificates_der.clone();
    for (_, field) in &selected {
        if let Some(contents) = field
            .sig_dict
            .get("Contents")
            .and_then(PdfObject::as_string)
        {
            for der in cms_certificate_der(contents) {
                push_unique_bytes(&mut certs, der);
            }
        }
    }
    for cert in &certs {
        Certificate::from_der(cert)
            .map_err(|e| OxideError::MalformedPdf(format!("LTV certificate DER: {e}")))?;
    }
    for crl in &material.crls_der {
        CertificateList::from_der(crl)
            .map_err(|e| OxideError::MalformedPdf(format!("LTV CRL DER: {e}")))?;
    }

    let next = next_free_object_number(reader);
    let dss_number = next;
    let mut number = next + 1;
    let mut raw_objects = Vec::new();

    let cert_refs = append_dss_streams(&mut raw_objects, &mut number, &certs);
    let ocsp_refs = append_dss_streams(&mut raw_objects, &mut number, &material.ocsp_responses_der);
    let crl_refs = append_dss_streams(&mut raw_objects, &mut number, &material.crls_der);

    let mut vri = PdfDictionary::empty();
    for (_, field) in selected {
        let Some(contents) = field
            .sig_dict
            .get("Contents")
            .and_then(PdfObject::as_string)
        else {
            continue;
        };
        let key = signature_vri_key(contents);
        let mut entry = PdfDictionary::empty();
        if !cert_refs.is_empty() {
            entry.insert("Cert", PdfObject::Array(cert_refs.clone()));
        }
        if !ocsp_refs.is_empty() {
            entry.insert("OCSP", PdfObject::Array(ocsp_refs.clone()));
        }
        if !crl_refs.is_empty() {
            entry.insert("CRL", PdfObject::Array(crl_refs.clone()));
        }
        vri.insert(key, PdfObject::Dictionary(entry));
    }

    let mut dss = PdfDictionary::empty();
    if !cert_refs.is_empty() {
        dss.insert("Certs", PdfObject::Array(cert_refs));
    }
    if !ocsp_refs.is_empty() {
        dss.insert("OCSPs", PdfObject::Array(ocsp_refs));
    }
    if !crl_refs.is_empty() {
        dss.insert("CRLs", PdfObject::Array(crl_refs));
    }
    dss.insert("VRI", PdfObject::Dictionary(vri));

    catalog.insert("DSS", reference(dss_number, 0));
    raw_objects.push(raw_object(
        root_number,
        root_generation,
        &PdfObject::Dictionary(catalog),
    ));
    raw_objects.push(raw_object(dss_number, 0, &PdfObject::Dictionary(dss)));

    write_incremental_update_raw(reader, raw_objects)
}

/// Apply an RSA/SHA-256 detached CMS signature as an incremental update.
///
/// The returned bytes preserve the original input as an exact prefix, append a
/// signature field/widget plus signature dictionary, patch `/ByteRange`, and
/// fill the `/Contents` placeholder with DER CMS. A caller-supplied timestamp
/// token can be embedded in CMS; DSS revocation/certificate material is added
/// afterward with [`add_ltv_material`]. Trust-chain and network policy remain
/// caller-owned.
pub fn sign_document(
    doc: &PdfDocument,
    signer: &PdfSigner,
    options: &SignatureOptions,
) -> Result<Vec<u8>> {
    let reader = doc.reader();
    if reader.is_encrypted() {
        return Err(OxideError::UnsupportedFeature(
            "digital signing encrypted inputs is not yet supported".to_string(),
        ));
    }
    if signer.certificates.is_empty() {
        return Err(OxideError::MalformedPdf(
            "digital signing requires a signer certificate".to_string(),
        ));
    }
    if options.contents_reserved_bytes == 0 {
        return Err(OxideError::MalformedPdf(
            "signature /Contents placeholder must reserve at least one byte".to_string(),
        ));
    }

    let page_index = options.page.checked_sub(1).ok_or_else(|| {
        OxideError::MalformedPdf("signature page numbers are 1-based".to_string())
    })?;
    let pages = doc.get_pages()?;
    let page = pages.get(page_index).ok_or_else(|| {
        OxideError::MalformedPdf(format!(
            "signature target page {} is out of range",
            options.page
        ))
    })?;

    let (root_number, root_generation) = reader.root_reference().ok_or_else(|| {
        OxideError::MalformedPdf("signature writer: trailer is missing /Root".to_string())
    })?;
    let mut catalog = reader
        .get_object(root_number, root_generation)?
        .as_dict()
        .cloned()
        .ok_or_else(|| OxideError::MalformedPdf("/Root is not a dictionary".to_string()))?;
    let mut page_dict = reader
        .get_object(page.object_number, page.generation_number)?
        .as_dict()
        .cloned()
        .ok_or_else(|| OxideError::MalformedPdf("target page is not a dictionary".to_string()))?;

    let next = next_free_object_number(reader);
    let sig_number = next;
    let field_number = next + 1;
    let appearance_number = options.rect.map(|_| next + 2);
    let acroform_number = match catalog.get_reference("AcroForm") {
        Some((number, _)) => number,
        None => appearance_number.map_or(next + 2, |n| n + 1),
    };

    let sig_ref = reference(sig_number, 0);
    let field_ref = reference(field_number, 0);
    let page_ref = reference(page.object_number, page.generation_number);

    let (mut acroform, acroform_ref) = match catalog.get("AcroForm") {
        Some(PdfObject::Reference { number, generation }) => {
            let dict = reader
                .get_object(*number, *generation)?
                .as_dict()
                .cloned()
                .ok_or_else(|| OxideError::MalformedPdf("/AcroForm is not a dictionary".into()))?;
            (dict, reference(*number, *generation))
        }
        Some(PdfObject::Dictionary(dict)) => (dict.clone(), reference(acroform_number, 0)),
        Some(_) | None => (PdfDictionary::empty(), reference(acroform_number, 0)),
    };

    let mut fields = resolve_array(acroform.get("Fields"), reader).unwrap_or_default();
    fields.push(field_ref.clone());
    acroform.insert("Fields", PdfObject::Array(fields));
    acroform.insert("SigFlags", PdfObject::Integer(3));
    catalog.insert("AcroForm", acroform_ref.clone());

    let mut annots = resolve_array(page_dict.get("Annots"), reader).unwrap_or_default();
    annots.push(field_ref.clone());
    page_dict.insert("Annots", PdfObject::Array(annots));

    let rect = options.rect.unwrap_or([0.0, 0.0, 0.0, 0.0]);
    let mut field = PdfDictionary::empty();
    field.insert("Type", PdfObject::Name("Annot".to_string()));
    field.insert("Subtype", PdfObject::Name("Widget".to_string()));
    field.insert("FT", PdfObject::Name("Sig".to_string()));
    field.insert(
        "T",
        PdfObject::String(options.field_name.as_bytes().to_vec()),
    );
    field.insert("F", PdfObject::Integer(132));
    field.insert("Rect", rect_object(rect));
    field.insert("P", page_ref);
    field.insert("V", sig_ref.clone());
    if let Some(ap_number) = appearance_number {
        let mut ap = PdfDictionary::empty();
        ap.insert("N", reference(ap_number, 0));
        field.insert("AP", PdfObject::Dictionary(ap));
    }

    let mut raw_objects = vec![
        raw_object(
            root_number,
            root_generation,
            &PdfObject::Dictionary(catalog),
        ),
        raw_object(
            page.object_number,
            page.generation_number,
            &PdfObject::Dictionary(page_dict),
        ),
        raw_object(
            acroform_ref.as_reference().unwrap().0,
            acroform_ref.as_reference().unwrap().1,
            &PdfObject::Dictionary(acroform),
        ),
        raw_object(field_number, 0, &PdfObject::Dictionary(field)),
        RawIncrementalObject {
            number: sig_number,
            generation: 0,
            body: signature_dictionary_body(options, options.contents_reserved_bytes),
        },
    ];

    if let (Some(ap_number), Some(rect)) = (appearance_number, options.rect) {
        raw_objects.push(raw_object(ap_number, 0, &appearance_stream(options, rect)));
    }

    let mut staged = write_incremental_update_raw(reader, raw_objects)?;
    let byte_range_start = find_unique(&staged, BYTE_RANGE_PLACEHOLDER)?;
    let contents_marker = contents_placeholder(options.contents_reserved_bytes);
    let contents_marker_start = find_unique(&staged, &contents_marker)?;
    let contents_hex_start = contents_marker_start + 1;
    let contents_after = contents_marker_start + contents_marker.len();
    let byte_range = ByteRange {
        a: 0,
        b: contents_marker_start,
        c: contents_after,
        d: staged.len().saturating_sub(contents_after),
    };
    patch_byte_range(&mut staged, byte_range_start, &byte_range)?;

    let signed_bytes = extract_signed_bytes(&staged, &byte_range).ok_or_else(|| {
        OxideError::MalformedPdf("signature writer produced an invalid /ByteRange".to_string())
    })?;
    let digest = Sha256::digest(&signed_bytes);
    let cms = build_detached_cms(signer, &digest, options.timestamp_token_der.as_deref())?;
    if cms.len() > options.contents_reserved_bytes {
        return Err(OxideError::ResourceLimit(format!(
            "CMS signature is {} bytes but /Contents reserved only {} bytes",
            cms.len(),
            options.contents_reserved_bytes
        )));
    }
    patch_contents_hex(
        &mut staged,
        contents_hex_start,
        options.contents_reserved_bytes,
        &cms,
    );

    Ok(staged)
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

#[derive(Default)]
struct DssIndex {
    present: bool,
    certs: Vec<Vec<u8>>,
    ocsp: Vec<Vec<u8>>,
    crls: Vec<Vec<u8>>,
    vri: std::collections::BTreeMap<String, DssVriEntry>,
}

#[derive(Default)]
struct DssVriEntry {
    certs: Vec<Vec<u8>>,
    ocsp: Vec<Vec<u8>>,
    crls: Vec<Vec<u8>>,
}

fn read_dss_index(reader: &PdfReader) -> DssIndex {
    let mut index = DssIndex::default();
    let Some((root, root_generation)) = reader.root_reference() else {
        return index;
    };
    let Ok(PdfObject::Dictionary(catalog)) = reader.get_object(root, root_generation) else {
        return index;
    };
    let Some(dss) = resolve_dict(catalog.get("DSS"), reader) else {
        return index;
    };
    index.present = true;
    index.certs = resolve_stream_array(dss.get("Certs"), reader);
    index.ocsp = resolve_stream_array(dss.get("OCSPs"), reader);
    index.crls = resolve_stream_array(dss.get("CRLs"), reader);

    if let Some(vri_dict) = resolve_dict(dss.get("VRI"), reader) {
        for (key, value) in vri_dict.entries() {
            let Some(entry_dict) = resolve_dict(Some(value), reader) else {
                continue;
            };
            index.vri.insert(
                key.clone(),
                DssVriEntry {
                    certs: resolve_stream_array(entry_dict.get("Cert"), reader),
                    ocsp: resolve_stream_array(entry_dict.get("OCSP"), reader),
                    crls: resolve_stream_array(entry_dict.get("CRL"), reader),
                },
            );
        }
    }

    index
}

fn resolve_stream_array(obj: Option<&PdfObject>, reader: &PdfReader) -> Vec<Vec<u8>> {
    resolve_array(obj, reader)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|obj| resolve_stream_bytes(&obj, reader))
        .collect()
}

fn resolve_stream_bytes(obj: &PdfObject, reader: &PdfReader) -> Option<Vec<u8>> {
    match reader.resolve(obj.clone()).ok()? {
        PdfObject::Stream { raw, .. } => Some(raw),
        PdfObject::String(bytes) => Some(bytes),
        _ => None,
    }
}

fn build_ltv_report(
    contents: &[u8],
    dss: &DssIndex,
    cms: &CmsResult,
    cert: Option<&CertInfo>,
) -> LtvReport {
    let vri_key = signature_vri_key(contents);
    let vri_entry = dss.vri.get(&vri_key);
    let (embedded_certs, embedded_ocsp, embedded_crls, crl_bytes) = if let Some(entry) = vri_entry {
        (
            entry.certs.len(),
            entry.ocsp.len(),
            entry.crls.len(),
            entry.crls.as_slice(),
        )
    } else {
        (
            dss.certs.len(),
            dss.ocsp.len(),
            dss.crls.len(),
            dss.crls.as_slice(),
        )
    };

    let revocation_status = revocation_status_from_crls(crl_bytes, cert);
    let has_timestamp = cms.timestamp_token_count > 0;
    let has_validation_material =
        embedded_certs > 0 && (embedded_ocsp > 0 || embedded_crls > 0) && vri_entry.is_some();
    let pades_level = if has_timestamp && has_validation_material {
        PadesLevel::BaselineLT
    } else if has_timestamp {
        PadesLevel::BaselineT
    } else {
        PadesLevel::BaselineB
    };

    let note = match pades_level {
        PadesLevel::BaselineLT => {
            "PAdES B-LT material present: timestamp token plus matching DSS VRI cert/revocation streams; embedded CRLs are checked for signer serial, but trust-chain/OCSP policy/TSA imprint validation is caller policy".to_string()
        }
        PadesLevel::BaselineT => {
            "PAdES B-T material present: CMS has a parseable signature timestamp token; timestamp imprint and TSA trust are not validated here".to_string()
        }
        PadesLevel::BaselineB if dss.present => {
            if vri_entry.is_some() {
                "DSS VRI material present, but no parseable CMS timestamp token; not promoted beyond PAdES B-B by this verifier".to_string()
            } else {
                "DSS present but no VRI entry matched this signature's /Contents hash".to_string()
            }
        }
        PadesLevel::BaselineB => {
            "no parseable CMS timestamp token or matching DSS validation material found".to_string()
        }
        PadesLevel::BaselineLTA => {
            "PAdES B-LTA document timestamp material detected".to_string()
        }
    };

    LtvReport {
        pades_level,
        timestamp_token_count: cms.timestamp_token_count,
        invalid_timestamp_token_count: cms.invalid_timestamp_token_count,
        dss_present: dss.present,
        vri_key: Some(vri_key),
        vri_matched: vri_entry.is_some(),
        embedded_certs,
        embedded_ocsp_responses: embedded_ocsp,
        embedded_crls,
        revocation_status,
        note,
    }
}

fn revocation_status_from_crls(crls: &[Vec<u8>], cert: Option<&CertInfo>) -> RevocationStatus {
    if crls.is_empty() {
        return RevocationStatus::NotChecked;
    }
    let Some(cert) = cert else {
        return RevocationStatus::EmbeddedMaterial;
    };
    let mut parsed_any = false;
    for crl in crls {
        let Ok(list) = CertificateList::from_der(crl) else {
            continue;
        };
        parsed_any = true;
        if let Some(revoked) = &list.tbs_cert_list.revoked_certificates {
            if revoked
                .iter()
                .any(|entry| hex_upper(entry.serial_number.as_bytes()) == cert.serial_hex)
            {
                return RevocationStatus::RevokedByEmbeddedCrl;
            }
        }
    }
    if parsed_any {
        RevocationStatus::GoodFromEmbeddedCrl
    } else {
        RevocationStatus::Unknown
    }
}

fn append_dss_streams(
    raw_objects: &mut Vec<RawIncrementalObject>,
    next_number: &mut u32,
    streams: &[Vec<u8>],
) -> Vec<PdfObject> {
    let mut refs = Vec::with_capacity(streams.len());
    for bytes in streams {
        let number = *next_number;
        *next_number += 1;
        refs.push(reference(number, 0));
        raw_objects.push(raw_object(number, 0, &dss_stream(bytes.clone())));
    }
    refs
}

fn dss_stream(raw: Vec<u8>) -> PdfObject {
    PdfObject::Stream {
        dict: PdfDictionary::empty(),
        raw,
    }
}

fn cms_certificate_der(contents: &[u8]) -> Vec<Vec<u8>> {
    let der = trim_trailing_zeros(contents);
    let Ok(ci) = ContentInfo::from_der(der) else {
        return Vec::new();
    };
    let Ok(signed) = ci.content.decode_as::<SignedData>() else {
        return Vec::new();
    };
    let Some(certs) = signed.certificates else {
        return Vec::new();
    };
    certs
        .0
        .iter()
        .filter_map(|choice| match choice {
            CertificateChoices::Certificate(cert) => cert.to_der().ok(),
            _ => None,
        })
        .collect()
}

fn push_unique_bytes(out: &mut Vec<Vec<u8>>, bytes: Vec<u8>) {
    if !out.iter().any(|existing| existing == &bytes) {
        out.push(bytes);
    }
}

fn signature_vri_key(contents: &[u8]) -> String {
    let digest = Sha1::digest(trim_trailing_zeros(contents));
    hex_upper(&digest)
}

fn verify_one(field: &SigField, file: &[u8], index: usize, dss: &DssIndex) -> SignatureReport {
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
        ltv: LtvReport::default(),
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
            let ltv = build_ltv_report(&contents, dss, &result, result.certificate.as_ref());
            report.validity = result.validity;
            report.digest_algorithm = result.digest_algorithm;
            report.certificate = result.certificate;
            report.ltv = ltv;
        }
        Err(msg) => {
            report.validity = SignatureValidity::Error;
            report.note = msg;
            return report;
        }
    }

    report.note = format!("{}. {}", scope_note(&report.validity), report.ltv.note);
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
        "{base}. Trust-chain validation to a root CA, certificate validity-period \
         enforcement, OCSP response policy validation, and timestamp imprint/TSA trust are \
         not verdict gates"
    )
}

struct CmsResult {
    validity: SignatureValidity,
    digest_algorithm: Option<String>,
    certificate: Option<CertInfo>,
    timestamp_token_count: usize,
    invalid_timestamp_token_count: usize,
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
    let (timestamp_token_count, invalid_timestamp_token_count) = cms_timestamp_token_counts(signer);

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
                timestamp_token_count,
                invalid_timestamp_token_count,
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
            timestamp_token_count,
            invalid_timestamp_token_count,
        });
    }

    // Verify the RSA signature over the payload, using the cert's public key.
    let Some(cert) = cert else {
        return Err("no signer certificate in CMS".to_string());
    };

    // Only RSA PKCS#1 v1.5 is supported this round.
    let sig_alg = signer.signature_algorithm.oid;
    if sig_alg != OID_RSA_ENCRYPTION
        && sig_alg != OID_SHA256_RSA
        && sig_alg != OID_SHA1_RSA
        && sig_alg != OID_SHA384_RSA
        && sig_alg != OID_SHA512_RSA
    {
        return Ok(CmsResult {
            validity: SignatureValidity::UnsupportedAlgorithm,
            digest_algorithm: digest_name,
            certificate: cert_info,
            timestamp_token_count,
            invalid_timestamp_token_count,
        });
    }

    let validity = match verify_rsa(
        &cert,
        &digest_oid,
        &signed_payload_digest_input,
        signer.signature.as_bytes(),
    ) {
        Ok(true) => SignatureValidity::Valid,
        Ok(false) => SignatureValidity::Invalid,
        Err(_) => SignatureValidity::Invalid,
    };

    Ok(CmsResult {
        validity,
        digest_algorithm: digest_name,
        certificate: cert_info,
        timestamp_token_count,
        invalid_timestamp_token_count,
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
            pubkey
                .verify(Pkcs1v15Sign::new::<Sha256>(), &h, signature)
                .is_ok()
        }
        OID_SHA384 => {
            let h = Sha384::digest(payload);
            pubkey
                .verify(Pkcs1v15Sign::new::<Sha384>(), &h, signature)
                .is_ok()
        }
        OID_SHA512 => {
            let h = Sha512::digest(payload);
            pubkey
                .verify(Pkcs1v15Sign::new::<Sha512>(), &h, signature)
                .is_ok()
        }
        OID_SHA1 => {
            let h = Sha1::digest(payload);
            pubkey
                .verify(Pkcs1v15Sign::new::<Sha1>(), &h, signature)
                .is_ok()
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

fn cms_timestamp_token_counts(signer: &SignerInfo) -> (usize, usize) {
    let mut valid = 0usize;
    let mut invalid = 0usize;
    let Some(attrs) = &signer.unsigned_attrs else {
        return (valid, invalid);
    };
    for attr in attrs.iter() {
        if attr.oid != OID_SIGNATURE_TIMESTAMP_TOKEN {
            continue;
        }
        for value in attr.values.iter() {
            let Ok(der) = value.to_der() else {
                invalid += 1;
                continue;
            };
            if ContentInfo::from_der(&der).is_ok() {
                valid += 1;
            } else {
                invalid += 1;
            }
        }
    }
    (valid, invalid)
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
// Signing helpers
// ---------------------------------------------------------------------------

fn next_free_object_number(reader: &PdfReader) -> u32 {
    let max_seen = reader
        .object_ids()
        .into_iter()
        .map(|(number, _)| number)
        .max()
        .unwrap_or(0);
    let trailer_size = reader.size().unwrap_or(0).max(0) as u32;
    max_seen.max(trailer_size.saturating_sub(1)) + 1
}

fn reference(number: u32, generation: u16) -> PdfObject {
    PdfObject::Reference { number, generation }
}

fn raw_object(number: u32, generation: u16, object: &PdfObject) -> RawIncrementalObject {
    let mut body = Vec::new();
    serialize_object(object, &mut body);
    RawIncrementalObject {
        number,
        generation,
        body,
    }
}

fn rect_object(rect: [f64; 4]) -> PdfObject {
    PdfObject::Array(rect.into_iter().map(PdfObject::Real).collect::<Vec<_>>())
}

fn signature_dictionary_body(options: &SignatureOptions, reserved_bytes: usize) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"<<\n");
    body.extend_from_slice(b"/Type /Sig\n");
    body.extend_from_slice(b"/Filter /Adobe.PPKLite\n");
    body.extend_from_slice(b"/SubFilter /adbe.pkcs7.detached\n");
    body.extend_from_slice(b"/ByteRange ");
    body.extend_from_slice(BYTE_RANGE_PLACEHOLDER);
    body.extend_from_slice(b"\n/Contents ");
    body.extend_from_slice(&contents_placeholder(reserved_bytes));
    body.extend_from_slice(b"\n");
    push_optional_pdf_string(&mut body, "Name", options.signer_name.as_deref());
    push_optional_pdf_string(&mut body, "Reason", options.reason.as_deref());
    push_optional_pdf_string(&mut body, "Location", options.location.as_deref());
    push_optional_pdf_string(&mut body, "ContactInfo", options.contact_info.as_deref());
    push_optional_pdf_string(&mut body, "M", options.signing_time.as_deref());
    body.extend_from_slice(b">>");
    body
}

fn push_optional_pdf_string(out: &mut Vec<u8>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        out.extend_from_slice(format!("/{key} ").as_bytes());
        out.extend_from_slice(pdf_literal_string(value).as_bytes());
        out.extend_from_slice(b"\n");
    }
}

fn pdf_literal_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('(');
    for byte in value.as_bytes() {
        match *byte {
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(*byte as char),
            b => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

fn appearance_stream(options: &SignatureOptions, rect: [f64; 4]) -> PdfObject {
    let width = (rect[2] - rect[0]).abs().max(1.0);
    let height = (rect[3] - rect[1]).abs().max(1.0);
    let signer = options
        .signer_name
        .as_deref()
        .unwrap_or(options.field_name.as_str());
    let reason = options.reason.as_deref().unwrap_or("Signed");
    let raw = format!(
        "q\n1 1 1 rg 0 0 {} {} re f\n0 0 0 RG 0.75 w 0 0 {} {} re S\nBT /Helv 10 Tf 8 {} Td {} Tj\n0 -14 Td {} Tj\nET\nQ",
        pdf_number(width),
        pdf_number(height),
        pdf_number(width),
        pdf_number(height),
        pdf_number((height - 16.0).max(8.0)),
        pdf_literal_string(&format!("Digitally signed by {signer}")),
        pdf_literal_string(reason),
    )
    .into_bytes();

    let mut font = PdfDictionary::empty();
    font.insert("Type", PdfObject::Name("Font".to_string()));
    font.insert("Subtype", PdfObject::Name("Type1".to_string()));
    font.insert("BaseFont", PdfObject::Name("Helvetica".to_string()));

    let mut fonts = PdfDictionary::empty();
    fonts.insert("Helv", PdfObject::Dictionary(font));

    let mut resources = PdfDictionary::empty();
    resources.insert("Font", PdfObject::Dictionary(fonts));

    let mut dict = PdfDictionary::empty();
    dict.insert("Type", PdfObject::Name("XObject".to_string()));
    dict.insert("Subtype", PdfObject::Name("Form".to_string()));
    dict.insert("BBox", rect_object([0.0, 0.0, width, height]));
    dict.insert("Resources", PdfObject::Dictionary(resources));
    PdfObject::Stream { dict, raw }
}

fn pdf_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{value:.3}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

fn contents_placeholder(reserved_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(reserved_bytes * 2 + 2);
    out.push(b'<');
    out.resize(reserved_bytes * 2 + 1, b'0');
    out.push(b'>');
    out
}

fn find_unique(haystack: &[u8], needle: &[u8]) -> Result<usize> {
    let mut found = None;
    for (idx, window) in haystack.windows(needle.len()).enumerate() {
        if window == needle {
            if found.is_some() {
                return Err(OxideError::MalformedPdf(
                    "signature writer found a non-unique placeholder".to_string(),
                ));
            }
            found = Some(idx);
        }
    }
    found.ok_or_else(|| {
        OxideError::MalformedPdf("signature writer placeholder was not found".to_string())
    })
}

fn patch_byte_range(out: &mut [u8], start: usize, br: &ByteRange) -> Result<()> {
    for value in [br.a, br.b, br.c, br.d] {
        if value as u64 > MAX_BYTE_RANGE_FIELD {
            return Err(OxideError::ResourceLimit(
                "signature ByteRange exceeds fixed 10-digit placeholder".to_string(),
            ));
        }
    }
    let replacement = format!("[{:>10} {:>10} {:>10} {:>10}]", br.a, br.b, br.c, br.d);
    debug_assert_eq!(replacement.len(), BYTE_RANGE_PLACEHOLDER.len());
    out[start..start + BYTE_RANGE_PLACEHOLDER.len()].copy_from_slice(replacement.as_bytes());
    Ok(())
}

fn patch_contents_hex(out: &mut [u8], hex_start: usize, reserved_bytes: usize, cms: &[u8]) {
    let hex_len = reserved_bytes * 2;
    for byte in &mut out[hex_start..hex_start + hex_len] {
        *byte = b'0';
    }
    for (idx, byte) in cms.iter().enumerate() {
        out[hex_start + idx * 2] = b"0123456789ABCDEF"[(byte >> 4) as usize];
        out[hex_start + idx * 2 + 1] = b"0123456789ABCDEF"[(byte & 0x0f) as usize];
    }
}

fn build_detached_cms(
    signer: &PdfSigner,
    content_digest: &[u8],
    timestamp_token_der: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let content = EncapsulatedContentInfo {
        econtent_type: const_oid::db::rfc5911::ID_DATA,
        econtent: None,
    };
    let digest_algorithm = AlgorithmIdentifierOwned {
        oid: OID_SHA256,
        parameters: None,
    };
    let signing_key = SigningKey::<Sha256>::new(signer.private_key.clone());
    let cert = signer.signer_certificate();
    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: cert.tbs_certificate.issuer.clone(),
        serial_number: cert.tbs_certificate.serial_number.clone(),
    });
    let mut signer_info = SignerInfoBuilder::new(
        &signing_key,
        sid,
        digest_algorithm.clone(),
        &content,
        Some(content_digest),
    )
    .map_err(|e| OxideError::MalformedPdf(format!("CMS signer info: {e}")))?;
    signer_info
        .add_signed_attribute(
            create_signing_time_attribute()
                .map_err(|e| OxideError::MalformedPdf(format!("CMS signing time: {e}")))?,
        )
        .map_err(|e| OxideError::MalformedPdf(format!("CMS signed attribute: {e}")))?;
    if let Some(token_der) = timestamp_token_der {
        signer_info
            .add_unsigned_attribute(signature_timestamp_attribute(token_der)?)
            .map_err(|e| OxideError::MalformedPdf(format!("CMS unsigned attribute: {e}")))?;
    }

    let mut builder = SignedDataBuilder::new(&content);
    builder
        .add_digest_algorithm(digest_algorithm)
        .map_err(|e| OxideError::MalformedPdf(format!("CMS digest algorithm: {e}")))?;
    for cert in &signer.certificates {
        builder
            .add_certificate(CertificateChoices::Certificate(cert.clone()))
            .map_err(|e| OxideError::MalformedPdf(format!("CMS certificate: {e}")))?;
    }
    builder
        .add_signer_info::<SigningKey<Sha256>, rsa::pkcs1v15::Signature>(signer_info)
        .map_err(|e| OxideError::MalformedPdf(format!("CMS signature: {e}")))?;
    let content_info = builder
        .build()
        .map_err(|e| OxideError::MalformedPdf(format!("CMS build: {e}")))?;
    content_info
        .to_der()
        .map_err(|e| OxideError::MalformedPdf(format!("CMS encode: {e}")))
}

fn signature_timestamp_attribute(token_der: &[u8]) -> Result<Attribute> {
    ContentInfo::from_der(token_der)
        .map_err(|e| OxideError::MalformedPdf(format!("timestamp token ContentInfo: {e}")))?;
    let value = AttributeValue::from_der(token_der)
        .map_err(|e| OxideError::MalformedPdf(format!("timestamp token attribute: {e}")))?;
    let mut values = SetOfVec::new();
    values
        .insert(value)
        .map_err(|e| OxideError::MalformedPdf(format!("timestamp token set: {e}")))?;
    Ok(Attribute {
        oid: OID_SIGNATURE_TIMESTAMP_TOKEN,
        values,
    })
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
    let n =
        |i: usize| -> Option<usize> { arr[i].as_integer().filter(|v| *v >= 0).map(|v| v as usize) };
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
