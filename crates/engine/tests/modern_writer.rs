//! Modern writer tests: cross-reference streams + object streams (PDF 1.5+).
//! Each mode must round-trip through Oxide's own reader (which already parses
//! xref/object streams) with identical content; qpdf/Poppler validation lives
//! in the CLI smoke (they aren't cargo deps).

use std::path::PathBuf;

use oxide_engine::{rewrite_document_with_mode, ContentEngine, WriterMode};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn write_mode(name: &str, mode: WriterMode) -> Vec<u8> {
    let engine = ContentEngine::open_path(fixture(name)).expect("open");
    rewrite_document_with_mode(engine.document().reader(), mode, |_n, _o| {}).expect("write")
}

fn doc_signature(bytes: &[u8]) -> (usize, Vec<String>) {
    let e = ContentEngine::open_bytes(bytes.to_vec()).expect("re-open written bytes");
    let n = e.page_count().unwrap();
    let texts = (1..=n).map(|p| e.get_page_text(p).unwrap().trim().to_string()).collect();
    (n, texts)
}

#[test]
fn all_modes_roundtrip_identically() {
    for name in ["tracemonkey.pdf", "basicapi.pdf", "multi_stream.pdf", "form_160f.pdf"] {
        let classic = doc_signature(&write_mode(name, WriterMode::ClassicXref));
        let xref = doc_signature(&write_mode(name, WriterMode::XrefStream));
        let objstm = doc_signature(&write_mode(name, WriterMode::XrefStreamWithObjStm));

        assert_eq!(classic.0, xref.0, "{name}: page count classic vs xref-stream");
        assert_eq!(classic.0, objstm.0, "{name}: page count classic vs objstm");
        assert_eq!(classic.1, xref.1, "{name}: text classic vs xref-stream");
        assert_eq!(classic.1, objstm.1, "{name}: text classic vs objstm");
    }
}

#[test]
fn xref_stream_output_declares_xref_type() {
    // The output must contain a /Type /XRef stream and point startxref at it.
    let bytes = write_mode("tracemonkey.pdf", WriterMode::XrefStream);
    let s = String::from_utf8_lossy(&bytes);
    assert!(s.contains("/Type /XRef") || s.contains("/Type/XRef"), "has an XRef stream");
    assert!(s.contains("startxref"), "has startxref");
    // No classic xref table keyword line should head the cross-ref section.
    assert!(!s.contains("\nxref\n"), "must not emit a classic xref table");
}

#[test]
fn objstm_output_declares_objstm_and_is_smaller() {
    // form_160f is object-heavy (AcroForm) — object streams should shrink it
    // well below the classic-xref rewrite.
    let classic = write_mode("form_160f.pdf", WriterMode::ClassicXref);
    let objstm = write_mode("form_160f.pdf", WriterMode::XrefStreamWithObjStm);
    let s = String::from_utf8_lossy(&objstm);
    assert!(s.contains("/ObjStm") , "has an object stream");
    assert!(
        objstm.len() < classic.len(),
        "objstm output ({}) should be smaller than classic ({})",
        objstm.len(),
        classic.len()
    );
}

#[test]
fn encrypted_objstm_roundtrips() {
    // The Bucket-2 interaction: an encrypted file whose objects are packed into
    // object streams. The ObjStm is encrypted as a WHOLE stream; the inner
    // objects must NOT be double-encrypted. Round-trip through Oxide's reader
    // (which knows not to separately decrypt ObjStm members) must recover the
    // exact content.
    use oxide_engine::crypto::{build_encryption, EncryptAlgorithm, EncryptParams};
    use oxide_engine::{rewrite_document_objects, PdfObject, PdfWriter, WriterMode};

    let engine = ContentEngine::open_path(fixture("tracemonkey.pdf")).expect("open");
    let plain_text = engine.get_page_text(1).unwrap();
    let pages = engine.page_count().unwrap();

    let file_id = vec![0xABu8; 16]; // fixed for the test (real encrypt randomizes)
    let params = EncryptParams {
        user_password: b"pw".to_vec(),
        algorithm: EncryptAlgorithm::Aes256,
        ..Default::default()
    };
    let state = build_encryption(&params, &file_id).unwrap();

    let mut noop = |_n: u32, _o: &mut PdfObject| {};
    let (objects, root, info) =
        rewrite_document_objects(engine.document().reader(), &mut noop).unwrap();
    let bytes = PdfWriter::new(objects, root)
        .with_info(info)
        .with_id(Some(file_id))
        .with_encryption(state)
        .with_mode(WriterMode::XrefStreamWithObjStm)
        .write()
        .expect("encrypted objstm write");

    // Without a password: must not yield the plaintext.
    let no_pw_ok = ContentEngine::open_bytes(bytes.clone())
        .ok()
        .and_then(|e| e.get_page_text(1).ok())
        .map(|t| t.trim() == plain_text.trim())
        .unwrap_or(false);
    assert!(!no_pw_ok, "encrypted+objstm must require the password");

    // With the password: exact content recovered.
    let re = ContentEngine::open_bytes_with_password(bytes, b"pw").expect("open w/ pw");
    assert_eq!(re.page_count().unwrap(), pages);
    assert_eq!(re.get_page_text(1).unwrap().trim(), plain_text.trim(), "decrypted content matches");
}

#[test]
fn modern_modes_are_deterministic() {
    // Same input + mode -> identical bytes (object ordering is deterministic).
    for mode in [WriterMode::XrefStream, WriterMode::XrefStreamWithObjStm] {
        let a = write_mode("tracemonkey.pdf", mode);
        let b = write_mode("tracemonkey.pdf", mode);
        assert_eq!(a, b, "{mode:?} output must be deterministic");
    }
}
