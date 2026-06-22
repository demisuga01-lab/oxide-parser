use std::path::PathBuf;

use oxide_engine::crypto::{EncryptAlgorithm, EncryptParams};
use oxide_engine::structural::{encrypt, linearize};
use oxide_engine::{
    convert_to_pdfa_checked, get_fallback_font, AuthorPageSize as PageSize, ContentEngine,
    EditMode, ExtractOptions, ImageRect, PdfAProfile, PdfBuilder, PdfDocument, PdfEditor,
    PdfSigner, RedactionOptions, SignatureOptions, SignatureValidity, StandardFont, TextStyle,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_pem(name: &str) -> String {
    let raw = std::fs::read_to_string(fixture(name)).expect("PEM fixture present");
    let mut pem = raw
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    pem.push('\n');
    pem
}

fn test_signer() -> PdfSigner {
    PdfSigner::from_pem(
        &fixture_pem("sign_test_rsa_key.pem"),
        &fixture_pem("sign_test_rsa_cert.pem"),
        &[],
    )
    .expect("test signer parses")
}

#[test]
fn create_edit_encrypt_decrypt_extract_roundtrip() {
    let mut doc = PdfBuilder::new();
    doc.set_title("Capstone create/edit/encrypt");
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Capstone original text",
            72.0,
            720.0,
            &TextStyle::standard(StandardFont::Helvetica, 14.0),
        )
        .unwrap();
    let authored = doc.to_bytes().unwrap();

    let mut editor = PdfEditor::open_bytes(authored).unwrap();
    editor
        .add_watermark_text("CAPSTONE WATERMARK", Default::default())
        .unwrap();
    let edited = editor.save_to_bytes(EditMode::FullRewrite).unwrap();

    let edited_engine = ContentEngine::open_bytes(edited).unwrap();
    let encrypted = encrypt(
        &edited_engine,
        &EncryptParams {
            user_password: b"capstone-user".to_vec(),
            owner_password: b"capstone-owner".to_vec(),
            algorithm: EncryptAlgorithm::Aes256,
            ..Default::default()
        },
    )
    .unwrap();

    let decrypted = ContentEngine::open_bytes_with_password(encrypted, b"capstone-user").unwrap();
    let text = decrypted.get_page_text(1).unwrap();
    assert!(text.contains("Capstone original text"), "{text}");
    assert!(text.contains("CAPSTONE WATERMARK"), "{text}");
}

#[test]
fn pdfa_linearize_sign_verify_workflow_composes() {
    let mut doc = PdfBuilder::new();
    doc.set_title("Capstone archival source")
        .set_author("Oxide")
        .set_creator("capstone integration test");
    let font = doc
        .register_truetype_font_bytes(
            "LiberationSansCapstone",
            get_fallback_font("Helvetica").unwrap().to_vec(),
        )
        .unwrap();
    doc.add_page(PageSize::LETTER)
        .draw_text(
            "Archival signed content",
            72.0,
            720.0,
            &TextStyle::new(font, 12.0),
        )
        .unwrap();

    let source = PdfDocument::open_bytes(doc.to_bytes().unwrap()).unwrap();
    let (pdfa, conversion) = convert_to_pdfa_checked(&source, PdfAProfile::PdfA2B).unwrap();
    assert!(conversion.validation.compliant, "{conversion:?}");

    let linearized = linearize::linearize(&ContentEngine::open_bytes(pdfa).unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&linearized).contains("/Linearized"));

    let signed = ContentEngine::open_bytes(linearized)
        .unwrap()
        .sign(
            &test_signer(),
            &SignatureOptions {
                field_name: "CapstoneSig".to_string(),
                signer_name: Some("Oxide Capstone".to_string()),
                reason: Some("release readiness integration".to_string()),
                signing_time: Some("D:20260622000000Z".to_string()),
                ..SignatureOptions::default()
            },
        )
        .unwrap();

    let reports = ContentEngine::open_bytes(signed)
        .unwrap()
        .verify_signatures()
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].field_name.as_deref(), Some("CapstoneSig"));
    assert_eq!(reports[0].validity, SignatureValidity::Valid);
}

#[test]
fn fill_flatten_redact_removes_value_and_fields() {
    let mut filled_editor = PdfEditor::open_bytes(acroform_pdf()).unwrap();
    filled_editor
        .set_form_text("name", "Alice Secret")
        .set_form_checkbox("agree", true)
        .set_form_choice("plan", "Gold");
    let filled = filled_editor.save_to_bytes(EditMode::FullRewrite).unwrap();

    let fields = ContentEngine::open_bytes(filled.clone())
        .unwrap()
        .extract_fields(&ExtractOptions::default())
        .unwrap()
        .fields;
    assert!(fields
        .iter()
        .any(|field| field.key == "name" && field.raw == "Alice Secret"));

    let mut flatten_editor = PdfEditor::open_bytes(filled).unwrap();
    flatten_editor.flatten_forms();
    let flattened = flatten_editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    assert!(PdfDocument::open_bytes(flattened.clone())
        .unwrap()
        .get_catalog()
        .unwrap()
        .get("AcroForm")
        .is_none());

    let mut redact_editor = PdfEditor::open_bytes(flattened).unwrap();
    redact_editor
        .redact(
            1,
            ImageRect::new(78.0, 128.0, 145.0, 24.0),
            RedactionOptions::default(),
        )
        .unwrap();
    let redacted = redact_editor.save_to_bytes(EditMode::FullRewrite).unwrap();
    let text = ContentEngine::open_bytes(redacted)
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert!(!text.contains("Alice Secret"), "{text}");
    assert!(text.contains("Gold"), "{text}");
}

struct TinyPdf {
    objects: Vec<Vec<u8>>,
}

impl TinyPdf {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    fn add(&mut self, body: &str) {
        self.objects.push(body.as_bytes().to_vec());
    }

    fn add_raw(&mut self, body: Vec<u8>) {
        self.objects.push(body);
    }

    fn build(&self) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (idx, body) in self.objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                self.objects.len() + 1,
                xref
            )
            .as_bytes(),
        );
        pdf
    }
}

fn acroform_pdf() -> Vec<u8> {
    let mut b = TinyPdf::new();
    b.add("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R] /DA (/Helv 10 Tf 0 g) >> >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >> >> /Contents 4 0 R /Annots [5 0 R 6 0 R 7 0 R] >>");
    b.add_raw(
        b"<< /Length 46 >>\nstream\nBT /Helv 10 Tf 20 170 Td (Form fixture) Tj ET\nendstream"
            .to_vec(),
    );
    b.add("<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [80 130 220 150] /V () /DA (/Helv 10 Tf 0 g) >>");
    b.add("<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [80 95 100 115] /V /Off /AS /Off >>");
    b.add("<< /Type /Annot /Subtype /Widget /FT /Ch /T (plan) /Rect [80 60 160 80] /Opt [(Silver) (Gold)] /V (Silver) /DA (/Helv 10 Tf 0 g) >>");
    b.build()
}
