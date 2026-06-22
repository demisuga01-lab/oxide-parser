use oxide_engine::{
    convert_to_pdfa, convert_to_pdfa_checked, get_fallback_font, improve_pdfua_best_effort,
    validate_pdfa, validate_pdfua, AuthorPageSize as PageSize, ComplianceSeverity, ContentEngine,
    PdfAProfile, PdfBuilder, PdfDocument, StandardFont, TextStyle,
};

fn standard_font_pdf() -> Vec<u8> {
    let mut doc = PdfBuilder::new();
    doc.set_title("Non compliant");
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Uses a Standard 14 font without PDF/A metadata.",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 12.0),
        )
        .unwrap();
    doc.to_bytes().unwrap()
}

fn embedded_font_pdf() -> Vec<u8> {
    let mut doc = PdfBuilder::new();
    doc.set_title("Embedded font source");
    doc.set_author("Archive Team");
    doc.set_creator("oxide-engine compliance test");
    let font = doc
        .register_truetype_font_bytes(
            "LiberationSans",
            get_fallback_font("Helvetica").unwrap().to_vec(),
        )
        .unwrap();
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Embedded text for archival conversion.",
            72.0,
            720.0,
            &TextStyle::new(font, 12.0),
        )
        .unwrap();
    doc.to_bytes().unwrap()
}

#[test]
fn pdfa_validator_reports_missing_archival_requirements() {
    let doc = PdfDocument::open_bytes(standard_font_pdf()).unwrap();
    let report = validate_pdfa(&doc, PdfAProfile::PdfA2B).unwrap();
    assert!(!report.compliant);
    assert!(report
        .violations
        .iter()
        .any(|v| v.rule == "pdfa.output_intent"));
    assert!(report.violations.iter().any(|v| v.rule == "pdfa.xmp"));
    assert!(report
        .violations
        .iter()
        .any(|v| v.rule == "pdfa.font.embedded" && v.severity == ComplianceSeverity::Error));
}

#[test]
fn pdfa_conversion_blocks_unembedded_fonts() {
    let doc = PdfDocument::open_bytes(standard_font_pdf()).unwrap();
    let err = convert_to_pdfa(&doc, PdfAProfile::PdfA2B).unwrap_err();
    assert!(err.to_string().contains("source fonts are not embedded"));
}

#[test]
fn pdfa_conversion_adds_xmp_output_intent_and_validates_clean() {
    let doc = PdfDocument::open_bytes(embedded_font_pdf()).unwrap();
    assert!(!validate_pdfa(&doc, PdfAProfile::PdfA2B).unwrap().compliant);
    let (bytes, conversion) = convert_to_pdfa_checked(&doc, PdfAProfile::PdfA2B).unwrap();
    assert!(
        conversion.validation.compliant,
        "{:?}",
        conversion.validation
    );
    let converted = PdfDocument::open_bytes(bytes.clone()).unwrap();
    assert!(
        converted.reader().first_file_id().is_some(),
        "PDF/A conversion must write a trailer ID for veraPDF"
    );
    assert!(
        validate_pdfa(&converted, PdfAProfile::PdfA2B)
            .unwrap()
            .compliant
    );
    let catalog = converted.get_catalog().unwrap();
    let metadata = converted
        .reader()
        .resolve(catalog.get("Metadata").unwrap().clone())
        .unwrap();
    let (_, xmp) = metadata.as_stream().unwrap();
    let xmp = String::from_utf8_lossy(xmp);
    assert!(xmp.contains("Embedded font source"));
    assert!(xmp.contains("Archive Team"));
    assert!(xmp.contains("oxide-engine compliance test"));
    assert!(ContentEngine::open_bytes(bytes)
        .unwrap()
        .get_page_text(1)
        .unwrap()
        .contains("Embedded text"));
}

#[test]
fn pdfa1_conversion_uses_classic_pdf14_output() {
    let doc = PdfDocument::open_bytes(embedded_font_pdf()).unwrap();
    let (bytes, conversion) = convert_to_pdfa_checked(&doc, PdfAProfile::PdfA1B).unwrap();
    assert!(
        conversion.validation.compliant,
        "{:?}",
        conversion.validation
    );
    assert!(bytes.starts_with(b"%PDF-1.4"));
    assert!(String::from_utf8_lossy(&bytes).contains("\nxref\n"));
    let converted = PdfDocument::open_bytes(bytes).unwrap();
    assert!(
        validate_pdfa(&converted, PdfAProfile::PdfA1B)
            .unwrap()
            .compliant
    );
}

#[test]
fn pdfa_matrix_conversion_sets_profile_metadata_and_level_a_tags() {
    let doc = PdfDocument::open_bytes(embedded_font_pdf()).unwrap();
    for (profile, part, conformance) in [
        (PdfAProfile::PdfA2A, "2", "A"),
        (PdfAProfile::PdfA3B, "3", "B"),
        (PdfAProfile::PdfA3A, "3", "A"),
    ] {
        let (bytes, conversion) = convert_to_pdfa_checked(&doc, profile).unwrap();
        assert!(
            conversion.validation.compliant,
            "{}: {:?}",
            profile.label(),
            conversion.validation
        );
        let converted = PdfDocument::open_bytes(bytes).unwrap();
        let catalog = converted.get_catalog().unwrap();
        let metadata = converted
            .reader()
            .resolve(catalog.get("Metadata").unwrap().clone())
            .unwrap();
        let (_, xmp) = metadata.as_stream().unwrap();
        let xmp = String::from_utf8_lossy(xmp);
        assert!(xmp.contains(&format!("<pdfaid:part>{part}</pdfaid:part>")));
        assert!(xmp.contains(&format!(
            "<pdfaid:conformance>{conformance}</pdfaid:conformance>"
        )));
        if profile.conformance() == "A" {
            assert!(catalog.contains_key("Lang"));
            assert!(catalog.contains_key("MarkInfo"));
            assert!(catalog.contains_key("StructTreeRoot"));
            assert!(validate_pdfua(&converted).unwrap().compliant);
        }
    }
}

#[test]
fn pdfa3_allows_only_associated_embedded_files() {
    let bytes = std::fs::read("tests/fixtures/attach_nametree.pdf").unwrap();
    let doc = PdfDocument::open_bytes(bytes).unwrap();

    let pdfa2 = validate_pdfa(&doc, PdfAProfile::PdfA2B).unwrap();
    assert!(pdfa2
        .violations
        .iter()
        .any(|violation| violation.rule == "pdfa.embedded_file"));

    let pdfa3 = validate_pdfa(&doc, PdfAProfile::PdfA3B).unwrap();
    assert!(!pdfa3
        .violations
        .iter()
        .any(|violation| violation.rule == "pdfa.embedded_file"));
    assert!(pdfa3
        .violations
        .iter()
        .any(|violation| violation.rule == "pdfa3.embedded_file.afrelationship"));
}

#[test]
fn pdfa3_conversion_repairs_embedded_file_relationships() {
    let bytes = std::fs::read("tests/fixtures/attach_nametree.pdf").unwrap();
    let doc = PdfDocument::open_bytes(bytes).unwrap();
    let (converted, report) = convert_to_pdfa_checked(&doc, PdfAProfile::PdfA3B).unwrap();
    assert!(report.validation.compliant, "{:?}", report.validation);

    let converted = PdfDocument::open_bytes(converted).unwrap();
    let mut saw_associated_file = false;
    for (number, generation) in converted.reader().object_ids() {
        let object = converted.reader().get_object(number, generation).unwrap();
        if let Some(dict) = object.as_dict() {
            if dict.get_name("Type") == Some("Filespec") || dict.contains_key("EF") {
                saw_associated_file = true;
                assert_eq!(dict.get_name("AFRelationship"), Some("Unspecified"));
            }
        }
    }
    assert!(saw_associated_file);
}

#[test]
fn pdfua_validation_and_best_effort_improvement_are_scoped() {
    let doc = PdfDocument::open_bytes(standard_font_pdf()).unwrap();
    let report = validate_pdfua(&doc).unwrap();
    assert!(!report.compliant);
    assert!(report.violations.iter().any(|v| v.rule == "pdfua.lang"));
    assert!(report
        .violations
        .iter()
        .any(|v| v.rule == "pdfua.structure"));

    let improved = improve_pdfua_best_effort(&doc, "en-US").unwrap();
    let improved_doc = PdfDocument::open_bytes(improved).unwrap();
    let report = validate_pdfua(&improved_doc).unwrap();
    assert!(report.compliant, "{:?}", report);
}
