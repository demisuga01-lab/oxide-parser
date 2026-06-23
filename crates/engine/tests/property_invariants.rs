use std::collections::BTreeSet;

use oxide_engine::authoring::{PageSize, PdfBuilder};
use oxide_engine::crypto::{secret_bytes, EncryptAlgorithm, EncryptParams};
use oxide_engine::structural::{encrypt, optimize};
use oxide_engine::{ContentEngine, StandardFont, TextStyle, WriterMode};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::string::string_regex;
use proptest::test_runner::{Config as ProptestConfig, TestCaseResult};

fn property_config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(32);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

fn text_line() -> impl Strategy<Value = String> {
    string_regex("[A-Za-z0-9][A-Za-z0-9 .,;:!?-]{0,35}").expect("valid text regex")
}

fn generated_pages() -> impl Strategy<Value = Vec<Vec<String>>> {
    vec(vec(text_line(), 1..4), 1..4)
}

fn password() -> impl Strategy<Value = String> {
    string_regex("[A-Za-z0-9!@#$%^&*_-]{1,24}").expect("valid password regex")
}

fn authored_bytes(pages: &[Vec<String>], mode: WriterMode) -> Vec<u8> {
    let mut doc = PdfBuilder::new().with_writer_mode(mode);
    let style = TextStyle::standard(StandardFont::Helvetica, 12.0);
    for lines in pages {
        let page = doc.add_page(PageSize::LETTER);
        for (line_index, line) in lines.iter().enumerate() {
            page.draw_text(line, 72.0, 720.0 - (line_index as f64 * 18.0), &style)
                .expect("generated WinAnsi text should be encodable");
        }
    }
    doc.to_bytes().expect("author generated PDF")
}

fn page_texts(engine: &ContentEngine) -> Vec<String> {
    let page_count = engine.page_count().expect("page count");
    (1..=page_count)
        .map(|page| engine.get_page_text(page).expect("page text"))
        .collect()
}

fn assert_authored_text(engine: &ContentEngine, pages: &[Vec<String>]) -> TestCaseResult {
    prop_assert_eq!(engine.page_count().expect("page count"), pages.len());
    for (page_index, lines) in pages.iter().enumerate() {
        let text = engine
            .get_page_text(page_index + 1)
            .expect("generated page text");
        for line in lines {
            prop_assert!(text.contains(line), "missing {line:?} in {text:?}");
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(property_config())]

    #[test]
    fn authored_documents_parse_back_and_are_deterministic(pages in generated_pages()) {
        let first = authored_bytes(&pages, WriterMode::XrefStreamWithObjStm);
        let second = authored_bytes(&pages, WriterMode::XrefStreamWithObjStm);
        prop_assert_eq!(&first, &second);

        let engine = ContentEngine::open_bytes(first).expect("open generated authored PDF");
        assert_authored_text(&engine, &pages)?;
    }

    #[test]
    fn writer_modes_are_representation_equivalent(pages in generated_pages()) {
        let modes = [
            WriterMode::ClassicXref,
            WriterMode::XrefStream,
            WriterMode::XrefStreamWithObjStm,
        ];
        let mut baseline: Option<Vec<String>> = None;
        for mode in modes {
            let engine = ContentEngine::open_bytes(authored_bytes(&pages, mode))
                .expect("open generated writer-mode PDF");
            assert_authored_text(&engine, &pages)?;
            let texts = page_texts(&engine);
            if let Some(base) = &baseline {
                prop_assert_eq!(&texts, base, "writer mode {:?} changed extractable text", mode);
            } else {
                baseline = Some(texts);
            }
        }
    }

    #[test]
    fn aes256_encrypt_open_with_password_preserves_content(
        pages in generated_pages(),
        user_password in password(),
        owner_password in password(),
    ) {
        let bytes = authored_bytes(&pages, WriterMode::XrefStreamWithObjStm);
        let engine = ContentEngine::open_bytes(bytes).expect("open generated PDF");
        let before = page_texts(&engine);
        let params = EncryptParams {
            user_password: secret_bytes(user_password.as_bytes().to_vec()),
            owner_password: secret_bytes(owner_password.as_bytes().to_vec()),
            algorithm: EncryptAlgorithm::Aes256,
            ..Default::default()
        };

        let encrypted = encrypt(&engine, &params).expect("encrypt generated PDF");
        let reopened = ContentEngine::open_bytes_with_password(encrypted, user_password.as_bytes())
            .expect("open encrypted generated PDF");
        prop_assert_eq!(reopened.page_count().expect("page count"), pages.len());
        prop_assert_eq!(page_texts(&reopened), before);
    }

    #[test]
    fn optimize_preserves_generated_content(pages in generated_pages()) {
        let bytes = authored_bytes(&pages, WriterMode::XrefStreamWithObjStm);
        let engine = ContentEngine::open_bytes(bytes).expect("open generated PDF");
        let before = page_texts(&engine);

        let (optimized, report) = optimize(&engine).expect("optimize generated PDF");
        prop_assert!(report.output_bytes > 0);
        prop_assert!(optimized.len() < 4 * 1024 * 1024, "bounded generated output");
        let reopened = ContentEngine::open_bytes(optimized).expect("open optimized generated PDF");
        prop_assert_eq!(reopened.page_count().expect("page count"), pages.len());
        prop_assert_eq!(page_texts(&reopened), before);
    }

    #[test]
    fn arbitrary_bytes_return_cleanly(data in vec(any::<u8>(), 0..2048)) {
        let _ = ContentEngine::open_bytes(data);
    }

    #[test]
    fn document_model_has_total_reading_order_and_json_roundtrips(pages in generated_pages()) {
        let bytes = authored_bytes(&pages, WriterMode::XrefStreamWithObjStm);
        let engine = ContentEngine::open_bytes(bytes).expect("open generated PDF");
        let page_list: Vec<usize> = (1..=pages.len()).collect();
        let model = engine.build_document_model(&page_list).expect("build generated model");

        let mut order = BTreeSet::new();
        for block in &model.blocks {
            prop_assert!(order.insert(block.reading_order_index), "duplicate reading order index");
        }
        for expected in 0..model.blocks.len() {
            prop_assert!(order.contains(&expected), "reading order gap at {expected}");
        }

        let json = serde_json::to_string(&model).expect("serialize model");
        let value: serde_json::Value = serde_json::from_str(&json).expect("deserialize model JSON");
        let encoded_again = serde_json::to_string(&value).expect("serialize model value");
        let value_again: serde_json::Value =
            serde_json::from_str(&encoded_again).expect("deserialize model value JSON");
        prop_assert_eq!(value, value_again);
    }
}
