//! PDF compliance validation and bounded conversion helpers.
//!
//! The PDF/A converter is intentionally conservative: it can add the required
//! XMP metadata, output intent, and strip disallowed actions, but it will not
//! claim success when source fonts are not embedded. Reconstructing unavailable
//! font programs requires an explicit substitution policy, so that remains a
//! reported conversion blocker.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::document::PdfDocument;
use crate::error::{OxideError, Result};
use crate::fonts_report::{list_fonts, FontInfo};
use crate::info::{decode_pdf_text_string, DocumentInfo};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::writer::{rewrite_document_objects, OutputObject, PdfWriter, WriterMode};

/// Supported PDF/A validation/conversion profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfAProfile {
    /// PDF/A-1b: basic visual archival preservation, no transparency.
    PdfA1B,
    /// PDF/A-2b: basic visual archival preservation, transparency allowed.
    PdfA2B,
}

impl PdfAProfile {
    pub fn part(self) -> i32 {
        match self {
            Self::PdfA1B => 1,
            Self::PdfA2B => 2,
        }
    }

    pub fn conformance(self) -> &'static str {
        "B"
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PdfA1B => "PDF/A-1b",
            Self::PdfA2B => "PDF/A-2b",
        }
    }
}

/// Severity for compliance findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceSeverity {
    Error,
    Warning,
}

/// A structured compliance finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComplianceViolation {
    pub rule: String,
    pub location: String,
    pub severity: ComplianceSeverity,
    pub message: String,
}

impl ComplianceViolation {
    fn error(rule: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            rule: rule.to_string(),
            location: location.into(),
            severity: ComplianceSeverity::Error,
            message: message.into(),
        }
    }

    fn warning(rule: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            rule: rule.to_string(),
            location: location.into(),
            severity: ComplianceSeverity::Warning,
            message: message.into(),
        }
    }
}

/// Result of a PDF/A validation pass.
#[derive(Debug, Clone, Serialize)]
pub struct PdfAValidationReport {
    pub profile: PdfAProfile,
    pub compliant: bool,
    pub violations: Vec<ComplianceViolation>,
}

/// Result of a PDF/UA validation pass.
#[derive(Debug, Clone, Serialize)]
pub struct PdfUaValidationReport {
    pub compliant: bool,
    pub violations: Vec<ComplianceViolation>,
}

/// Summary of a PDF/A conversion.
#[derive(Debug, Clone, Serialize)]
pub struct PdfAConversionReport {
    pub profile: PdfAProfile,
    pub validation: PdfAValidationReport,
    pub blocked_fonts: Vec<FontInfo>,
}

/// Validate a document against the implemented PDF/A profile rules.
pub fn validate_pdfa(doc: &PdfDocument, profile: PdfAProfile) -> Result<PdfAValidationReport> {
    let reader = doc.reader();
    let mut violations = Vec::new();
    let catalog = doc.get_catalog()?;

    if reader.is_encrypted() {
        violations.push(ComplianceViolation::error(
            "pdfa.encryption",
            "trailer",
            "PDF/A documents must not be encrypted",
        ));
    }
    if profile == PdfAProfile::PdfA1B && pdf_version_gt(reader.version(), "1.4") {
        violations.push(ComplianceViolation::error(
            "pdfa1.version",
            "header",
            "PDF/A-1 is based on PDF 1.4 and must not use later PDF features",
        ));
    }
    if reader
        .first_file_id()
        .map(|id| id.is_empty())
        .unwrap_or(true)
    {
        violations.push(ComplianceViolation::error(
            "pdfa.file_id",
            "trailer/ID",
            "PDF/A requires a non-empty trailer ID array",
        ));
    }

    validate_output_intent(&catalog, reader, &mut violations);
    validate_xmp(&catalog, reader, profile, &mut violations);
    validate_fonts(doc, &mut violations)?;
    validate_disallowed_objects(reader, profile, &mut violations)?;

    Ok(PdfAValidationReport {
        profile,
        compliant: !violations
            .iter()
            .any(|v| v.severity == ComplianceSeverity::Error),
        violations,
    })
}

/// Convert a document to the requested PDF/A profile where the existing font
/// programs are already embedded.
pub fn convert_to_pdfa(doc: &PdfDocument, profile: PdfAProfile) -> Result<Vec<u8>> {
    let report = validate_embedded_fonts(doc)?;
    if !report.is_empty() {
        let names = report
            .iter()
            .map(|font| font.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(OxideError::UnsupportedFeature(format!(
            "PDF/A conversion blocked: source fonts are not embedded ({names})"
        )));
    }
    if doc.reader().is_encrypted() {
        return Err(OxideError::UnsupportedFeature(
            "PDF/A conversion requires an already-decrypted source document".to_string(),
        ));
    }

    let (mut objects, root, info) = rewrite_document_objects(doc.reader(), &mut |_, object| {
        strip_disallowed_actions(object);
    })?;
    drop_copied_structural_artifacts(&mut objects);
    let mut document_info = DocumentInfo::gather(doc)?;
    document_info.producer = Some("Oxide PDF SDK".to_string());
    let file_id = pdfa_file_id(doc, profile, &document_info);
    let next = objects.iter().map(|obj| obj.number).max().unwrap_or(0) + 1;
    let metadata_number = next;
    let icc_number = next + 1;
    let output_intent_number = next + 2;
    let info_number = info.unwrap_or(next + 3);

    upsert_catalog_compliance(
        &mut objects,
        root,
        metadata_number,
        output_intent_number,
        profile,
    )?;
    upsert_info(&mut objects, info_number);
    objects.push(OutputObject {
        number: metadata_number,
        object: xmp_metadata_stream(profile, &document_info),
    });
    objects.push(OutputObject {
        number: icc_number,
        object: srgb_output_profile_stream(),
    });
    objects.push(OutputObject {
        number: output_intent_number,
        object: output_intent_dictionary(icc_number),
    });
    objects.sort_by_key(|obj| obj.number);

    let (writer_mode, version) = writer_for_profile(profile);
    PdfWriter::new(objects, root)
        .with_version(version)
        .with_info(Some(info_number))
        .with_id(Some(file_id))
        .with_mode(writer_mode)
        .write()
}

/// Convert and immediately validate with Oxide's validator.
pub fn convert_to_pdfa_checked(
    doc: &PdfDocument,
    profile: PdfAProfile,
) -> Result<(Vec<u8>, PdfAConversionReport)> {
    let bytes = convert_to_pdfa(doc, profile)?;
    let converted = PdfDocument::open_bytes(bytes.clone())?;
    let validation = validate_pdfa(&converted, profile)?;
    Ok((
        bytes,
        PdfAConversionReport {
            profile,
            blocked_fonts: Vec::new(),
            validation,
        },
    ))
}

/// Validate basic PDF/UA accessibility requirements.
pub fn validate_pdfua(doc: &PdfDocument) -> Result<PdfUaValidationReport> {
    let catalog = doc.get_catalog()?;
    let reader = doc.reader();
    let mut violations = Vec::new();

    if !catalog.contains_key("Lang") {
        violations.push(ComplianceViolation::error(
            "pdfua.lang",
            "Catalog",
            "PDF/UA documents must declare a document language",
        ));
    }
    let marked = catalog
        .get("MarkInfo")
        .and_then(|obj| reader.resolve(obj.clone()).ok())
        .and_then(|obj| obj.as_dict().cloned())
        .and_then(|dict| dict.get_bool("Marked"))
        .unwrap_or(false);
    if !marked {
        violations.push(ComplianceViolation::error(
            "pdfua.marked",
            "Catalog/MarkInfo",
            "PDF/UA documents must be marked/tagged",
        ));
    }
    let Some(root_obj) = catalog.get("StructTreeRoot") else {
        violations.push(ComplianceViolation::error(
            "pdfua.structure",
            "Catalog",
            "PDF/UA documents require a StructTreeRoot",
        ));
        return Ok(PdfUaValidationReport {
            compliant: false,
            violations,
        });
    };
    let root = reader.resolve(root_obj.clone())?;
    if root.as_dict().is_none() {
        violations.push(ComplianceViolation::error(
            "pdfua.structure",
            "StructTreeRoot",
            "StructTreeRoot must resolve to a dictionary",
        ));
    } else if let Some(root_dict) = root.as_dict() {
        validate_structure_alt_text(reader, root_dict, &mut violations, &mut BTreeSet::new(), 0);
    }

    Ok(PdfUaValidationReport {
        compliant: !violations
            .iter()
            .any(|v| v.severity == ComplianceSeverity::Error),
        violations,
    })
}

/// Add language, MarkInfo, and a minimal structure root when missing.
///
/// This is assistive tagging only; it is not a full PDF/UA conformance
/// guarantee because human-reviewed reading order and alt text are still needed.
pub fn improve_pdfua_best_effort(doc: &PdfDocument, lang: &str) -> Result<Vec<u8>> {
    let (mut objects, root, info) = rewrite_document_objects(doc.reader(), &mut |_, _| {})?;
    let struct_number = objects.iter().map(|obj| obj.number).max().unwrap_or(0) + 1;
    let root_obj = objects
        .iter_mut()
        .find(|obj| obj.number == root)
        .ok_or_else(|| OxideError::MalformedPdf("PDF/UA improve: missing catalog".to_string()))?;
    let PdfObject::Dictionary(catalog) = &mut root_obj.object else {
        return Err(OxideError::MalformedPdf(
            "PDF/UA improve: catalog is not a dictionary".to_string(),
        ));
    };
    catalog.insert("Lang", PdfObject::String(lang.as_bytes().to_vec()));
    catalog.insert(
        "MarkInfo",
        PdfObject::Dictionary(dict(&[("Marked", PdfObject::Boolean(true))])),
    );
    if !catalog.contains_key("StructTreeRoot") {
        catalog.insert("StructTreeRoot", reference(struct_number));
        objects.push(OutputObject {
            number: struct_number,
            object: PdfObject::Dictionary(dict(&[
                ("Type", PdfObject::Name("StructTreeRoot".to_string())),
                ("K", PdfObject::Array(Vec::new())),
            ])),
        });
    }
    objects.sort_by_key(|obj| obj.number);
    PdfWriter::new(objects, root)
        .with_info(info)
        .with_id(doc.reader().first_file_id())
        .with_mode(WriterMode::XrefStreamWithObjStm)
        .write()
}

fn validate_output_intent(
    catalog: &PdfDictionary,
    reader: &PdfReader,
    violations: &mut Vec<ComplianceViolation>,
) {
    let Some(output_intents) = catalog
        .get("OutputIntents")
        .and_then(|obj| reader.resolve(obj.clone()).ok())
    else {
        violations.push(ComplianceViolation::error(
            "pdfa.output_intent",
            "Catalog",
            "PDF/A requires an OutputIntent with an ICC profile",
        ));
        return;
    };
    let Some(items) = output_intents.as_array() else {
        violations.push(ComplianceViolation::error(
            "pdfa.output_intent",
            "Catalog/OutputIntents",
            "OutputIntents must be an array",
        ));
        return;
    };
    if items.is_empty() {
        violations.push(ComplianceViolation::error(
            "pdfa.output_intent",
            "Catalog/OutputIntents",
            "OutputIntents must not be empty",
        ));
    }
    for (idx, item) in items.iter().enumerate() {
        let Ok(intent) = reader.resolve(item.clone()) else {
            violations.push(ComplianceViolation::error(
                "pdfa.output_intent",
                format!("Catalog/OutputIntents[{idx}]"),
                "OutputIntent could not be resolved",
            ));
            continue;
        };
        let Some(dict) = intent.as_dict() else {
            violations.push(ComplianceViolation::error(
                "pdfa.output_intent",
                format!("Catalog/OutputIntents[{idx}]"),
                "OutputIntent must be a dictionary",
            ));
            continue;
        };
        if dict.get_name("S") != Some("GTS_PDFA1") {
            violations.push(ComplianceViolation::error(
                "pdfa.output_intent.s",
                format!("Catalog/OutputIntents[{idx}]/S"),
                "OutputIntent /S must be /GTS_PDFA1",
            ));
        }
        let Some(dest) = dict.get("DestOutputProfile") else {
            violations.push(ComplianceViolation::error(
                "pdfa.output_intent.icc",
                format!("Catalog/OutputIntents[{idx}]"),
                "OutputIntent must reference DestOutputProfile",
            ));
            continue;
        };
        let Ok(profile) = reader.resolve(dest.clone()) else {
            continue;
        };
        let Some((profile_dict, raw)) = profile.as_stream() else {
            violations.push(ComplianceViolation::error(
                "pdfa.output_intent.icc",
                format!("Catalog/OutputIntents[{idx}]/DestOutputProfile"),
                "ICC profile must be a stream",
            ));
            continue;
        };
        if profile_dict.get_integer("N") != Some(3) {
            violations.push(ComplianceViolation::warning(
                "pdfa.output_intent.icc",
                format!("Catalog/OutputIntents[{idx}]/DestOutputProfile/N"),
                "Only RGB OutputIntent profiles are validated by Oxide",
            ));
        }
        if raw.len() < 128 || raw.get(36..40) != Some(b"acsp") {
            violations.push(ComplianceViolation::error(
                "pdfa.output_intent.icc",
                format!("Catalog/OutputIntents[{idx}]/DestOutputProfile"),
                "ICC profile stream must contain an ICC header signature",
            ));
        }
    }
}

fn validate_xmp(
    catalog: &PdfDictionary,
    reader: &PdfReader,
    profile: PdfAProfile,
    violations: &mut Vec<ComplianceViolation>,
) {
    let Some(metadata) = catalog.get("Metadata") else {
        violations.push(ComplianceViolation::error(
            "pdfa.xmp",
            "Catalog",
            "PDF/A requires a catalog Metadata XMP stream",
        ));
        return;
    };
    let Ok(metadata) = reader.resolve(metadata.clone()) else {
        violations.push(ComplianceViolation::error(
            "pdfa.xmp",
            "Catalog/Metadata",
            "Metadata could not be resolved",
        ));
        return;
    };
    let Some((dict, raw)) = metadata.as_stream() else {
        violations.push(ComplianceViolation::error(
            "pdfa.xmp",
            "Catalog/Metadata",
            "Metadata must be an XMP stream",
        ));
        return;
    };
    if dict.get_name("Subtype") != Some("XML") {
        violations.push(ComplianceViolation::error(
            "pdfa.xmp.subtype",
            "Catalog/Metadata/Subtype",
            "Metadata stream subtype must be /XML",
        ));
    }
    let xmp = String::from_utf8_lossy(raw);
    if !xmp.contains(&format!("<pdfaid:part>{}</pdfaid:part>", profile.part())) {
        violations.push(ComplianceViolation::error(
            "pdfa.xmp.pdfaid_part",
            "Catalog/Metadata",
            format!("XMP must declare pdfaid:part {}", profile.part()),
        ));
    }
    if !xmp.contains(&format!(
        "<pdfaid:conformance>{}</pdfaid:conformance>",
        profile.conformance()
    )) {
        violations.push(ComplianceViolation::error(
            "pdfa.xmp.pdfaid_conformance",
            "Catalog/Metadata",
            "XMP must declare pdfaid:conformance",
        ));
    }
    validate_xmp_info_sync(reader, &xmp, violations);
}

fn validate_xmp_info_sync(
    reader: &PdfReader,
    xmp: &str,
    violations: &mut Vec<ComplianceViolation>,
) {
    let Some((number, generation)) = reader.info_reference() else {
        return;
    };
    let Ok(PdfObject::Dictionary(info)) = reader.get_and_resolve(number, generation) else {
        return;
    };
    for key in [
        "Title", "Author", "Subject", "Keywords", "Creator", "Producer",
    ] {
        let Some(value) = info
            .get(key)
            .and_then(|obj| obj.as_string())
            .map(decode_pdf_text_string)
        else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let escaped = xml_escape(&value);
        if !xmp.contains(&escaped) {
            violations.push(ComplianceViolation::error(
                "pdfa.xmp.info_sync",
                format!("Info/{key}"),
                "PDF/A requires document information fields to be synchronized with XMP metadata",
            ));
        }
    }
}

fn validate_fonts(doc: &PdfDocument, violations: &mut Vec<ComplianceViolation>) -> Result<()> {
    for font in list_fonts(doc)? {
        if !font.embedded {
            violations.push(ComplianceViolation::error(
                "pdfa.font.embedded",
                format!("object {} {}", font.object_number, font.generation),
                format!("font '{}' is not embedded", font.name),
            ));
        }
    }
    Ok(())
}

fn validate_disallowed_objects(
    reader: &PdfReader,
    profile: PdfAProfile,
    violations: &mut Vec<ComplianceViolation>,
) -> Result<()> {
    for (number, generation) in reader.object_ids() {
        let object = reader.get_object(number, generation)?;
        scan_disallowed(
            &object,
            &format!("object {number} {generation}"),
            profile,
            violations,
            0,
        );
    }
    Ok(())
}

fn scan_disallowed(
    object: &PdfObject,
    location: &str,
    profile: PdfAProfile,
    violations: &mut Vec<ComplianceViolation>,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    match object {
        PdfObject::Dictionary(dict) => {
            for key in ["JavaScript", "JS", "AA", "OpenAction", "Launch"] {
                if dict.contains_key(key) {
                    violations.push(ComplianceViolation::error(
                        "pdfa.action.disallowed",
                        format!("{location}/{key}"),
                        "PDF/A forbids JavaScript, launch, and automatic actions",
                    ));
                }
            }
            if dict.get_name("Type") == Some("EmbeddedFile") {
                violations.push(ComplianceViolation::error(
                    "pdfa.embedded_file",
                    location,
                    "PDF/A-1/2 conversion profile does not allow EmbeddedFile streams",
                ));
            }
            if profile == PdfAProfile::PdfA1B {
                if matches!(dict.get_name("Type"), Some("ObjStm" | "XRef")) {
                    violations.push(ComplianceViolation::error(
                        "pdfa1.xref_structure",
                        location,
                        "PDF/A-1 forbids PDF 1.5 cross-reference and object stream structures",
                    ));
                }
                if dict.get_name("S") == Some("Transparency") || dict.contains_key("Group") {
                    violations.push(ComplianceViolation::error(
                        "pdfa1.transparency",
                        location,
                        "PDF/A-1 forbids transparency groups",
                    ));
                }
                for alpha_key in ["ca", "CA"] {
                    if dict
                        .get(alpha_key)
                        .and_then(PdfObject::as_number)
                        .map(|v| v < 1.0)
                        .unwrap_or(false)
                    {
                        violations.push(ComplianceViolation::error(
                            "pdfa1.transparency",
                            format!("{location}/{alpha_key}"),
                            "PDF/A-1 forbids transparency",
                        ));
                    }
                }
            }
            for (key, value) in dict.entries() {
                scan_disallowed(
                    value,
                    &format!("{location}/{key}"),
                    profile,
                    violations,
                    depth + 1,
                );
            }
        }
        PdfObject::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                scan_disallowed(
                    item,
                    &format!("{location}[{idx}]"),
                    profile,
                    violations,
                    depth + 1,
                );
            }
        }
        PdfObject::Stream { dict, .. } => {
            scan_disallowed(
                &PdfObject::Dictionary(dict.clone()),
                location,
                profile,
                violations,
                depth + 1,
            );
        }
        _ => {}
    }
}

fn validate_embedded_fonts(doc: &PdfDocument) -> Result<Vec<FontInfo>> {
    Ok(list_fonts(doc)?
        .into_iter()
        .filter(|font| !font.embedded)
        .collect())
}

fn drop_copied_structural_artifacts(objects: &mut Vec<OutputObject>) {
    objects.retain(|object| !is_pdf15_structural_artifact(&object.object));
}

fn is_pdf15_structural_artifact(object: &PdfObject) -> bool {
    let dict = match object {
        PdfObject::Dictionary(dict) => Some(dict),
        PdfObject::Stream { dict, .. } => Some(dict),
        _ => None,
    };
    matches!(
        dict.and_then(|dict| dict.get_name("Type")),
        Some("ObjStm" | "XRef")
    )
}

fn writer_for_profile(profile: PdfAProfile) -> (WriterMode, &'static str) {
    match profile {
        PdfAProfile::PdfA1B => (WriterMode::ClassicXref, "1.4"),
        PdfAProfile::PdfA2B => (WriterMode::XrefStreamWithObjStm, "1.7"),
    }
}

fn pdf_version_gt(actual: &str, max: &str) -> bool {
    parse_pdf_version(actual) > parse_pdf_version(max)
}

fn parse_pdf_version(version: &str) -> (u16, u16) {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    (major, minor)
}

fn upsert_catalog_compliance(
    objects: &mut [OutputObject],
    root: u32,
    metadata_number: u32,
    output_intent_number: u32,
    _profile: PdfAProfile,
) -> Result<()> {
    let root_obj = objects
        .iter_mut()
        .find(|obj| obj.number == root)
        .ok_or_else(|| OxideError::MalformedPdf("PDF/A conversion: missing catalog".to_string()))?;
    let PdfObject::Dictionary(catalog) = &mut root_obj.object else {
        return Err(OxideError::MalformedPdf(
            "PDF/A conversion: catalog is not a dictionary".to_string(),
        ));
    };
    catalog.insert("Metadata", reference(metadata_number));
    catalog.insert(
        "OutputIntents",
        PdfObject::Array(vec![reference(output_intent_number)]),
    );
    strip_disallowed_actions(&mut root_obj.object);
    Ok(())
}

fn upsert_info(objects: &mut Vec<OutputObject>, info_number: u32) {
    if let Some(existing) = objects.iter_mut().find(|obj| obj.number == info_number) {
        if let PdfObject::Dictionary(info) = &mut existing.object {
            info.insert("Producer", pdf_text("Oxide PDF SDK"));
        }
        return;
    }
    objects.push(OutputObject {
        number: info_number,
        object: PdfObject::Dictionary(dict(&[("Producer", pdf_text("Oxide PDF SDK"))])),
    });
}

fn pdfa_file_id(doc: &PdfDocument, profile: PdfAProfile, info: &DocumentInfo) -> Vec<u8> {
    if let Some(id) = doc.reader().first_file_id() {
        if !id.is_empty() {
            return id;
        }
    }

    let mut seed = Vec::new();
    seed.extend_from_slice(b"oxide-pdfa-file-id\0");
    seed.extend_from_slice(profile.label().as_bytes());
    seed.push(0);
    seed.extend_from_slice(doc.reader().version().as_bytes());
    seed.push(0);
    if let Some(size) = doc.reader().size() {
        seed.extend_from_slice(size.to_string().as_bytes());
    }
    seed.push(0);
    if let Some((root, generation)) = doc.reader().root_reference() {
        seed.extend_from_slice(root.to_string().as_bytes());
        seed.push(b' ');
        seed.extend_from_slice(generation.to_string().as_bytes());
    }
    for value in [
        &info.title,
        &info.author,
        &info.subject,
        &info.keywords,
        &info.creator,
        &info.producer,
    ]
    .into_iter()
    .flatten()
    {
        seed.push(0);
        seed.extend_from_slice(value.as_bytes());
    }
    crate::crypto::md5(&seed).to_vec()
}

fn strip_disallowed_actions(object: &mut PdfObject) {
    match object {
        PdfObject::Dictionary(dict) => {
            for key in ["JavaScript", "JS", "AA", "OpenAction", "Launch"] {
                dict.remove(key);
            }
            if let Some(PdfObject::Dictionary(names)) = dict.get("Names").cloned() {
                let mut names = names;
                names.remove("JavaScript");
                dict.insert("Names", PdfObject::Dictionary(names));
            }
            let keys: Vec<String> = dict.entries().map(|(key, _)| key.clone()).collect();
            for key in keys {
                if let Some(mut value) = dict.get(&key).cloned() {
                    strip_disallowed_actions(&mut value);
                    dict.insert(key, value);
                }
            }
        }
        PdfObject::Array(items) => {
            for item in items {
                strip_disallowed_actions(item);
            }
        }
        PdfObject::Stream { dict, .. } => {
            let mut wrapper = PdfObject::Dictionary(dict.clone());
            strip_disallowed_actions(&mut wrapper);
            if let PdfObject::Dictionary(clean) = wrapper {
                *dict = clean;
            }
        }
        _ => {}
    }
}

fn xmp_metadata_stream(profile: PdfAProfile, info: &DocumentInfo) -> PdfObject {
    let title = info.title.as_deref().map(xmp_title).unwrap_or_default();
    let author = info.author.as_deref().map(xmp_author).unwrap_or_default();
    let subject = info.subject.as_deref().map(xmp_subject).unwrap_or_default();
    let keywords = info
        .keywords
        .as_deref()
        .map(|value| format!("      <pdf:Keywords>{}</pdf:Keywords>\n", xml_escape(value)))
        .unwrap_or_default();
    let creator = info.creator.as_deref().unwrap_or("Oxide PDF SDK");
    let producer = info.producer.as_deref().unwrap_or("Oxide PDF SDK");
    let xml = format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/"
      xmlns:dc="http://purl.org/dc/elements/1.1/"
      xmlns:xmp="http://ns.adobe.com/xap/1.0/"
      xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
      <pdfaid:part>{}</pdfaid:part>
      <pdfaid:conformance>{}</pdfaid:conformance>
{}{}{}{}      <pdf:Producer>{}</pdf:Producer>
      <xmp:CreatorTool>{}</xmp:CreatorTool>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        profile.part(),
        profile.conformance(),
        title,
        author,
        subject,
        keywords,
        xml_escape(producer),
        xml_escape(creator),
    );
    PdfObject::Stream {
        dict: dict(&[
            ("Type", PdfObject::Name("Metadata".to_string())),
            ("Subtype", PdfObject::Name("XML".to_string())),
        ]),
        raw: xml.into_bytes(),
    }
}

fn xmp_title(value: &str) -> String {
    format!(
        "      <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n",
        xml_escape(value)
    )
}

fn xmp_author(value: &str) -> String {
    format!(
        "      <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n",
        xml_escape(value)
    )
}

fn xmp_subject(value: &str) -> String {
    format!(
        "      <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>\n",
        xml_escape(value)
    )
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn output_intent_dictionary(icc_number: u32) -> PdfObject {
    PdfObject::Dictionary(dict(&[
        ("Type", PdfObject::Name("OutputIntent".to_string())),
        ("S", PdfObject::Name("GTS_PDFA1".to_string())),
        ("OutputConditionIdentifier", pdf_text("sRGB")),
        ("Info", pdf_text("Generated sRGB ICC profile")),
        ("DestOutputProfile", reference(icc_number)),
    ]))
}

fn srgb_output_profile_stream() -> PdfObject {
    PdfObject::Stream {
        dict: dict(&[
            ("N", PdfObject::Integer(3)),
            ("Alternate", PdfObject::Name("DeviceRGB".to_string())),
        ]),
        raw: generated_srgb_icc_profile(),
    }
}

fn generated_srgb_icc_profile() -> Vec<u8> {
    const TAG_TABLE_OFFSET: usize = 132;
    const TAG_DATA_OFFSET: usize = 204;
    const XYZ_TAG_LEN: u32 = 20;
    const TRC_TAG_LEN: u32 = 14;
    const PROFILE_LEN: usize = 312;

    let mut profile = vec![0u8; PROFILE_LEN];
    write_icc_u32(&mut profile, 0, PROFILE_LEN as u32);
    write_icc_signature(&mut profile, 4, b"Oxid");
    write_icc_u32(&mut profile, 8, 0x0210_0000);
    write_icc_signature(&mut profile, 12, b"mntr");
    write_icc_signature(&mut profile, 16, b"RGB ");
    write_icc_signature(&mut profile, 20, b"XYZ ");
    write_icc_u16(&mut profile, 24, 2026);
    write_icc_u16(&mut profile, 26, 1);
    write_icc_u16(&mut profile, 28, 1);
    write_icc_signature(&mut profile, 36, b"acsp");
    write_icc_signature(&mut profile, 40, b"APPL");
    write_icc_signature(&mut profile, 48, b"Oxid");
    write_icc_signature(&mut profile, 52, b"sRGB");
    write_icc_s15_fixed(&mut profile, 68, 0.9642);
    write_icc_s15_fixed(&mut profile, 72, 1.0);
    write_icc_s15_fixed(&mut profile, 76, 0.8249);
    write_icc_signature(&mut profile, 80, b"Oxid");
    write_icc_u32(&mut profile, 128, 6);

    let xyz_tags = [
        (b"rXYZ", (0.436_074_7, 0.222_504_5, 0.013_932_2)),
        (b"gXYZ", (0.385_064_9, 0.716_878_6, 0.097_104_5)),
        (b"bXYZ", (0.143_080_4, 0.060_616_9, 0.714_173_3)),
    ];
    let mut tag_table_offset = TAG_TABLE_OFFSET;
    let mut tag_data_offset = TAG_DATA_OFFSET;
    for (signature, xyz) in xyz_tags {
        write_icc_tag_record(
            &mut profile,
            tag_table_offset,
            signature,
            tag_data_offset as u32,
            XYZ_TAG_LEN,
        );
        write_icc_xyz_type(&mut profile, tag_data_offset, xyz);
        tag_table_offset += 12;
        tag_data_offset += XYZ_TAG_LEN as usize;
    }

    for signature in [b"rTRC", b"gTRC", b"bTRC"] {
        write_icc_tag_record(
            &mut profile,
            tag_table_offset,
            signature,
            tag_data_offset as u32,
            TRC_TAG_LEN,
        );
        write_icc_curve_type_gamma(&mut profile, tag_data_offset, 2.2);
        tag_table_offset += 12;
        tag_data_offset += 16;
    }

    profile
}

fn write_icc_tag_record(
    profile: &mut [u8],
    offset: usize,
    signature: &[u8; 4],
    data_offset: u32,
    data_len: u32,
) {
    write_icc_signature(profile, offset, signature);
    write_icc_u32(profile, offset + 4, data_offset);
    write_icc_u32(profile, offset + 8, data_len);
}

fn write_icc_xyz_type(profile: &mut [u8], offset: usize, xyz: (f32, f32, f32)) {
    write_icc_signature(profile, offset, b"XYZ ");
    write_icc_s15_fixed(profile, offset + 8, xyz.0);
    write_icc_s15_fixed(profile, offset + 12, xyz.1);
    write_icc_s15_fixed(profile, offset + 16, xyz.2);
}

fn write_icc_curve_type_gamma(profile: &mut [u8], offset: usize, gamma: f32) {
    write_icc_signature(profile, offset, b"curv");
    write_icc_u32(profile, offset + 8, 1);
    write_icc_u16(profile, offset + 12, (gamma * 256.0).round() as u16);
}

fn write_icc_signature(profile: &mut [u8], offset: usize, signature: &[u8; 4]) {
    profile[offset..offset + 4].copy_from_slice(signature);
}

fn write_icc_u32(profile: &mut [u8], offset: usize, value: u32) {
    profile[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn write_icc_u16(profile: &mut [u8], offset: usize, value: u16) {
    profile[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn write_icc_s15_fixed(profile: &mut [u8], offset: usize, value: f32) {
    let fixed = (value * 65_536.0).round() as i32;
    profile[offset..offset + 4].copy_from_slice(&fixed.to_be_bytes());
}

fn validate_structure_alt_text(
    reader: &PdfReader,
    dict: &PdfDictionary,
    violations: &mut Vec<ComplianceViolation>,
    visited: &mut BTreeSet<(u32, u16)>,
    depth: usize,
) {
    if depth > 128 {
        return;
    }
    if dict.get_name("S") == Some("Figure") && !dict.contains_key("Alt") {
        violations.push(ComplianceViolation::error(
            "pdfua.figure.alt",
            "StructElem/Figure",
            "Figure structure elements require alternate text",
        ));
    }
    if let Some(kids) = dict.get("K") {
        validate_structure_kids(reader, kids, violations, visited, depth + 1);
    }
}

fn validate_structure_kids(
    reader: &PdfReader,
    object: &PdfObject,
    violations: &mut Vec<ComplianceViolation>,
    visited: &mut BTreeSet<(u32, u16)>,
    depth: usize,
) {
    match object {
        PdfObject::Reference { number, generation } => {
            if !visited.insert((*number, *generation)) {
                return;
            }
            if let Ok(resolved) = reader.get_and_resolve(*number, *generation) {
                validate_structure_kids(reader, &resolved, violations, visited, depth + 1);
            }
            visited.remove(&(*number, *generation));
        }
        PdfObject::Dictionary(dict) => {
            validate_structure_alt_text(reader, dict, violations, visited, depth + 1)
        }
        PdfObject::Array(items) => {
            for item in items {
                validate_structure_kids(reader, item, violations, visited, depth + 1);
            }
        }
        _ => {}
    }
}

fn pdf_text(value: &str) -> PdfObject {
    PdfObject::String(value.as_bytes().to_vec())
}

fn reference(number: u32) -> PdfObject {
    PdfObject::Reference {
        number,
        generation: 0,
    }
}

fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
    let mut out = PdfDictionary::empty();
    for (key, value) in entries {
        out.insert(*key, value.clone());
    }
    out
}
