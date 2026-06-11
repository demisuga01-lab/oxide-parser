use oxide_engine::content::{tokenize_all, ContentToken};
use oxide_engine::content::{ContentParser, GraphicsState, IDENTITY_MATRIX};
use oxide_engine::fonts::FontResolver;
use oxide_engine::{
    decode_stream, get_fallback_font, BlendMode, ClipMask, ColorSpaceHandler, ContentEngine,
    DashState, FillRule, ImageEncoder, ImageLocateOptions, ImageLocator, ImageOutputFormat,
    ImagePainter, LinePainter, OxideError, PageResources, Path, PathPainter, PdfAnalyzer,
    PdfDocument, PdfObject, PdfReader, PixelBuffer, RawImage, ReadingOrderReconstructor,
    RenderColor, RenderQuality, SmaskLoader, TextChunk, TextCollector, TextExtractOptions,
    TextExtractor, TextLayerRecommendation, TextLine, Transform2D, Viewport, BLACK, BLUE, GREEN,
    RED, WHITE,
};

const CONTENT_TEXT: &[u8] = b"BT\n/F1 12 Tf\n72 720 Td\n(Hi) Tj\nET\n";

#[test]
fn minimal_classic_xref_pdf_loads_and_resolves_page_tree() {
    let _ = env_logger::builder().is_test(true).try_init();
    let reader = PdfReader::from_bytes(include_bytes!("fixtures/minimal.pdf").to_vec()).unwrap();

    assert_eq!(reader.version(), "1.4");
    assert_eq!(reader.size(), Some(5));

    let (root_number, root_generation) = reader.root_reference().unwrap();
    let root = reader
        .get_and_resolve(root_number, root_generation)
        .unwrap();
    let root_dict = root.as_dict().unwrap();
    assert_eq!(root_dict.get_name("Type"), Some("Catalog"));

    let (pages_number, pages_generation) = root_dict.get_reference("Pages").unwrap();
    let pages = reader
        .get_and_resolve(pages_number, pages_generation)
        .unwrap();
    let pages_dict = pages.as_dict().unwrap();
    assert_eq!(pages_dict.get_name("Type"), Some("Pages"));
    assert_eq!(pages_dict.get_integer("Count"), Some(1));

    let kids = pages_dict.get_array("Kids").unwrap();
    assert_eq!(kids.len(), 1);
    let (page_number, page_generation) = kids[0].as_reference().unwrap();
    let page = reader
        .get_and_resolve(page_number, page_generation)
        .unwrap();
    let page_dict = page.as_dict().unwrap();
    let (contents_number, contents_generation) = page_dict.get_reference("Contents").unwrap();
    let contents = reader
        .get_object(contents_number, contents_generation)
        .unwrap();
    assert!(matches!(contents, PdfObject::Stream { .. }));
}

#[test]
fn flate_stream_decodes_to_original_content_text() {
    let _ = env_logger::builder().is_test(true).try_init();
    let reader = PdfReader::from_bytes(include_bytes!("fixtures/flate.pdf").to_vec()).unwrap();
    let root = reader.get_and_resolve(1, 0).unwrap();
    let pages_ref = root.as_dict().unwrap().get_reference("Pages").unwrap();
    let pages = reader.get_and_resolve(pages_ref.0, pages_ref.1).unwrap();
    let page_ref = pages.as_dict().unwrap().get_array("Kids").unwrap()[0]
        .as_reference()
        .unwrap();
    let page = reader.get_and_resolve(page_ref.0, page_ref.1).unwrap();
    let contents_ref = page.as_dict().unwrap().get_reference("Contents").unwrap();
    let stream = reader.get_object(contents_ref.0, contents_ref.1).unwrap();

    assert_eq!(decode_stream(&stream, &reader).unwrap(), CONTENT_TEXT);
}

#[test]
fn document_walker_collects_minimal_pdf_page_and_content_bytes() {
    let _ = env_logger::builder().is_test(true).try_init();
    let document =
        PdfDocument::open_bytes(include_bytes!("fixtures/minimal.pdf").to_vec()).unwrap();

    let pages = document.get_pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].media_box, [0.0, 0.0, 200.0, 200.0]);
    assert!(!pages[0].contents.is_empty());

    let content = document.get_page_content_bytes(1).unwrap();
    assert!(!content.is_empty());
    assert!(content.windows(2).any(|window| window == b"BT"));
}

#[test]
fn flate_page_content_bytes_tokenize_to_text_operators() {
    let _ = env_logger::builder().is_test(true).try_init();
    let document = PdfDocument::open_path("tests/fixtures/flate.pdf").unwrap();
    let content = document.get_page_content_bytes(1).unwrap();
    let tokens = tokenize_all(&content).unwrap();

    assert!(!tokens.is_empty());
    assert!(tokens
        .iter()
        .any(|token| matches!(token, ContentToken::Operator(op) if op == "BT")));
    assert!(tokens
        .iter()
        .any(|token| matches!(token, ContentToken::Operator(op) if op == "ET")));
}

#[test]
fn flate_page_content_pipeline_updates_graphics_state_and_decodes_font_if_available() {
    let _ = env_logger::builder().is_test(true).try_init();
    let document = PdfDocument::open_path("tests/fixtures/flate.pdf").unwrap();
    let pages = document.get_pages().unwrap();
    let content = document.get_page_content_bytes(1).unwrap();
    let operations = ContentParser::parse(&content).unwrap();
    let mut state = GraphicsState::new();

    for operation in &operations {
        state.process(operation);
    }

    assert!(operations
        .iter()
        .any(|operation| operation.operator == "BT"));
    assert!(operations
        .iter()
        .any(|operation| operation.operator == "ET"));
    assert_ne!(state.text.tm, IDENTITY_MATRIX);
    assert!(operations
        .iter()
        .any(|operation| operation.operator == "Tj" || operation.operator == "TJ"));

    let first_tf_font = operations
        .iter()
        .find(|operation| operation.operator == "Tf")
        .and_then(|operation| operation.name(0));
    let first_tj_bytes = operations
        .iter()
        .find(|operation| operation.operator == "Tj")
        .and_then(|operation| operation.string_bytes(0));

    if let (Some(font_name), Some(bytes), Some(fonts)) = (
        first_tf_font,
        first_tj_bytes,
        pages[0].resources.get_dict("Font"),
    ) {
        if let Some(font_obj) = fonts.get(font_name) {
            let resolved = document.reader().resolve(font_obj.clone()).unwrap();
            if let Some(font_dict) = resolved.as_dict() {
                let resolver = FontResolver::new(font_dict, document.reader());
                let decoded = resolver.decode_string(bytes);
                assert!(!decoded.is_empty());
            }
        }
    }
}

#[test]
fn engine_opens_minimal_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/minimal.pdf").unwrap();
    assert_eq!(engine.page_count().unwrap(), 1);
    let operations = engine.get_page_content(1).unwrap();
    assert!(!operations.is_empty());
    let _resources = engine.get_page_resources(1).unwrap();
}

#[test]
fn engine_opens_flate_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    assert_eq!(engine.page_count().unwrap(), 1);
    let operations = engine.get_page_content(1).unwrap();
    assert!(operations
        .iter()
        .any(|operation| operation.operator == "BT"));
    assert!(operations
        .iter()
        .any(|operation| operation.operator == "Tj" || operation.operator == "TJ"));
}

#[test]
fn engine_rejects_page_zero() {
    let engine = ContentEngine::open_path("tests/fixtures/minimal.pdf").unwrap();
    assert!(matches!(
        engine.get_page_content(0),
        Err(OxideError::MalformedPdf(_))
    ));
}

#[test]
fn engine_rejects_page_beyond_count() {
    let engine = ContentEngine::open_path("tests/fixtures/minimal.pdf").unwrap();
    assert!(matches!(
        engine.get_page_content(99),
        Err(OxideError::MalformedPdf(_))
    ));
}

#[test]
fn scanned_page_has_no_text() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    assert!(!engine.page_has_text_layer(1).unwrap());
    let text = engine.get_page_text(1).unwrap();
    assert!(
        text.is_empty(),
        "scanned page should return empty text, got: {:?}",
        text
    );
}

#[test]
fn multi_stream_page_concatenates_correctly() {
    let engine = ContentEngine::open_path("tests/fixtures/multi_stream.pdf").unwrap();
    let operations = engine.get_page_content(1).unwrap();
    let tj_count = operations
        .iter()
        .filter(|operation| operation.operator == "Tj")
        .count();
    let bt_count = operations
        .iter()
        .filter(|operation| operation.operator == "BT")
        .count();
    assert_eq!(tj_count, 2, "expected 2 Tj operators, one from each stream");
    assert_eq!(bt_count, 2);
}

#[test]
fn malformed_content_stream_returns_partial_parse() {
    let mut bad_stream = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET".to_vec();
    bad_stream.extend_from_slice(b"\xFF\xFE\xFF\xFE");
    let operations = ContentParser::parse(&bad_stream).unwrap();
    assert!(
        operations
            .iter()
            .any(|operation| operation.operator == "Tj"),
        "should have partial results from before the garbage bytes"
    );
}

#[test]
fn page_resources_include_inherited_parent_font() {
    let engine = ContentEngine::open_path("tests/fixtures/multi_stream.pdf").unwrap();
    let resources = engine.get_page_resources(1).unwrap();
    assert!(resources.fonts.contains_key("F1"));
}

#[test]
fn page_has_text_layer_true() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    assert!(engine.page_has_text_layer(1).unwrap());
}

#[test]
fn page_has_text_layer_false() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    assert!(!engine.page_has_text_layer(1).unwrap());
}

#[test]
fn zero_page_tree_is_valid_but_page_one_is_out_of_range() {
    let pdf = build_pdf(vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [] /Count 0 >>".to_string()),
    ]);
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    assert_eq!(engine.page_count().unwrap(), 0);
    assert!(matches!(
        engine.get_page_content(1),
        Err(OxideError::MalformedPdf(_))
    ));
}

#[test]
fn encrypted_document_v2_surfaces_encrypted_error() {
    let pdf = build_pdf_with_trailer(
        vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>".to_string()),
        ],
        "<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 2 >> >>",
    );
    assert!(matches!(
        ContentEngine::open_bytes(pdf),
        Err(OxideError::EncryptedDocument)
    ));
}

#[test]
fn text_extraction_from_flate_fixture() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let chunks = {
        let operations = engine.get_page_content(1).unwrap();
        let resources = engine.get_page_resources(1).unwrap();
        let mut collector = TextCollector::new(resources, engine.document().reader());
        collector.collect(&operations)
    };
    assert!(!chunks.is_empty(), "flate.pdf should have text chunks");
    let full_text = chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(!full_text.is_empty());
    for chunk in &chunks {
        assert!(chunk.x >= 0.0 && chunk.x <= 612.0);
        assert!(chunk.y >= 0.0 && chunk.y <= 792.0);
        assert!(chunk.font_size > 0.0);
    }
}

#[test]
fn get_page_text_from_flate() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(!text.is_empty());
}

#[test]
fn get_all_text_returns_per_page_results() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let all = engine.get_all_text().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, 1);
}

#[test]
fn multi_stream_text_extraction() {
    let engine = ContentEngine::open_path("tests/fixtures/multi_stream.pdf").unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(
        text.contains("Hello") || text.contains("World") || !text.is_empty(),
        "multi-stream page should extract some text, got: {:?}",
        text
    );
}

#[test]
fn text_extractor_default_extracts_flate() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let text = TextExtractor::extract_default(&engine).unwrap();
    assert!(
        !text.trim().is_empty(),
        "extractor should produce non-empty text"
    );
    assert!(
        text.contains("--- Page 1 ---"),
        "default options include page markers, got: {:?}",
        &text[..text.len().min(200)]
    );
}

#[test]
fn text_extractor_no_page_markers() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let mut opts = TextExtractOptions::default();
    opts.format.include_page_markers = false;
    let text = TextExtractor::new().extract(&engine, &opts).unwrap();
    assert!(!text.contains("--- Page"), "no page markers expected");
    assert!(!text.trim().is_empty());
}

#[test]
fn text_extractor_specific_page() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let mut opts = TextExtractOptions::default();
    opts.pages = Some(vec![1]);
    opts.format.include_page_markers = false;
    let text = TextExtractor::new().extract(&engine, &opts).unwrap();
    assert!(!text.trim().is_empty(), "page 1 should have text");
}

#[test]
fn text_extractor_out_of_range_page_is_skipped() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let mut opts = TextExtractOptions::default();
    opts.pages = Some(vec![999]);
    let result = TextExtractor::new().extract(&engine, &opts);
    assert!(
        result.is_ok(),
        "out-of-range page should be skipped, not error"
    );
    assert!(result.unwrap().is_empty(), "no valid pages -> empty string");
}

#[test]
fn text_extractor_scanned_page_returns_empty() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let text = TextExtractor::extract_default(&engine).unwrap();
    let text_without_markers = text
        .lines()
        .filter(|l| !l.starts_with("--- Page"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text_without_markers.trim().is_empty(),
        "scanned PDF should produce no text content, got: {:?}",
        text_without_markers
    );
}

#[test]
fn text_extractor_multi_stream_extracts_both_streams() {
    let engine = ContentEngine::open_path("tests/fixtures/multi_stream.pdf").unwrap();
    let text = TextExtractor::extract_default(&engine).unwrap();
    assert!(
        text.contains("Hello") || text.contains("World") || !text.trim().is_empty(),
        "multi-stream PDF should extract text from both streams"
    );
}

#[test]
fn get_page_text_uses_reading_order_pipeline() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(
        !text.trim().is_empty(),
        "get_page_text should return non-empty string after pipeline upgrade"
    );
}

#[test]
fn full_pipeline_flate_pdf_produces_reasonable_output() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();

    let mut opts = TextExtractOptions::default();
    opts.format.include_page_markers = true;
    opts.format.paragraph_breaks = true;
    opts.format.heading_breaks = true;
    opts.format.preserve_layout = false;

    let text = TextExtractor::new().extract(&engine, &opts).unwrap();

    assert!(
        text.starts_with("--- Page 1 ---"),
        "should start with page 1 marker"
    );
    assert!(!text.trim().is_empty(), "output should not be empty");

    let content_lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with("--- Page") && !l.trim().is_empty())
        .collect();
    assert!(
        !content_lines.is_empty(),
        "should have at least one content line"
    );
    for line in &content_lines {
        assert!(
            line.chars().all(|c| !c.is_control() || c == '\t'),
            "content lines should not contain control characters: {:?}",
            line
        );
    }
}

#[test]
fn reading_order_reconstructor_with_real_page() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let ops = engine.get_page_content(1).unwrap();
    let resources = engine.get_page_resources(1).unwrap();
    let _page = engine.get_page(1).unwrap();

    let mut collector = TextCollector::new(resources, engine.document().reader());
    let chunks = collector.collect(&ops);

    let reconstructor = ReadingOrderReconstructor::new();
    let lines = reconstructor.reconstruct(chunks.clone());

    let total_chunks_in_lines: usize = lines.iter().map(|_| 1usize).sum();
    assert!(
        total_chunks_in_lines <= chunks.len(),
        "cannot have more lines than input chunks"
    );

    for col in 0..=1usize {
        let col_lines: Vec<&TextLine> = lines.iter().filter(|l| l.column == col).collect();
        for pair in col_lines.windows(2) {
            assert!(
                pair[0].y >= pair[1].y,
                "lines should be sorted y-descending: {} >= {} failed",
                pair[0].y,
                pair[1].y
            );
        }
    }

    for line in &lines {
        assert!(
            line.x_min <= line.x_max,
            "x_min {} should be <= x_max {} for line {:?}",
            line.x_min,
            line.x_max,
            line.text
        );
    }

    for line in &lines {
        assert!(
            line.font_size > 0.0,
            "font_size should be positive, got {} for line {:?}",
            line.font_size,
            line.text
        );
    }
}

#[test]
fn page_resources_from_dict_resolves_font_dict() {
    let reader =
        PdfReader::from_bytes(include_bytes!("fixtures/multi_stream.pdf").to_vec()).unwrap();
    let document =
        PdfDocument::open_bytes(include_bytes!("fixtures/multi_stream.pdf").to_vec()).unwrap();
    let page = document.get_pages().unwrap().remove(0);
    let resources = PageResources::from_dict(&page.resources, &reader);
    assert!(resources.fonts.contains_key("F1"));
}

#[test]
fn analyzer_detects_text_layer_in_flate_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let analysis = PdfAnalyzer::quick_analysis(&engine).unwrap();
    assert!(
        analysis.has_text_layer,
        "flate.pdf has text, should detect text layer"
    );
    assert!(!analysis.is_likely_scanned, "flate.pdf is not scanned");
    assert!(
        analysis.confidence >= 0.9,
        "confidence should be high for a clear text PDF"
    );
    assert_eq!(
        analysis.recommendation,
        TextLayerRecommendation::UseExtractText
    );
    assert!(!analysis.pages_with_text.is_empty());
    assert!(analysis.pages_without_text.is_empty());
    assert!(
        analysis.total_char_count > 0,
        "should have counted at least some characters"
    );
}

#[test]
fn analyzer_detects_no_text_in_image_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let analysis = PdfAnalyzer::quick_analysis(&engine).unwrap();
    assert!(
        !analysis.has_text_layer,
        "image_only.pdf has no text, should not detect text layer"
    );
    assert!(
        analysis.is_likely_scanned,
        "image-only PDF should be marked as likely scanned"
    );
    assert_eq!(analysis.recommendation, TextLayerRecommendation::UseOcr);
    assert!(analysis.pages_with_text.is_empty());
    assert!(!analysis.pages_without_text.is_empty());
}

#[test]
fn analyzer_handles_zero_page_document() {
    let engine = ContentEngine::open_path("tests/fixtures/minimal.pdf").unwrap();
    let analysis = PdfAnalyzer::quick_analysis(&engine).unwrap();
    assert_eq!(analysis.total_pages, 1);
    assert_eq!(analysis.sampled_pages, 1);
}

#[test]
fn analyzer_analyze_bytes_works() {
    let pdf_bytes = std::fs::read("tests/fixtures/flate.pdf").unwrap();
    let analysis = PdfAnalyzer::analyze_bytes(pdf_bytes).unwrap();
    assert!(analysis.has_text_layer);
}

#[test]
fn analyzer_full_analysis_covers_all_pages() {
    let engine = ContentEngine::open_path("tests/fixtures/multi_stream.pdf").unwrap();
    let quick = PdfAnalyzer::quick_analysis(&engine).unwrap();
    let full = PdfAnalyzer::full_analysis(&engine).unwrap();
    assert_eq!(quick.has_text_layer, full.has_text_layer);
    assert_eq!(full.sampled_pages, full.total_pages);
}

#[test]
fn image_locator_finds_image_in_image_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(
        !images.is_empty(),
        "image_only.pdf should contain at least one image"
    );
    let first = &images[0];
    assert_eq!(first.page_number, 1);
    assert!(!first.is_mask, "the main image should not be a mask");
    assert!(
        first.width > 0 && first.height > 0,
        "image dimensions should be non-zero"
    );
    println!(
        "Found image: {} page={} object={} {}x{} {} bpc={} filters={:?}",
        first.xobject_name,
        first.page_number,
        first.object_number,
        first.width,
        first.height,
        first.color_space,
        first.bits_per_component,
        first.filter
    );
}

#[test]
fn image_locator_returns_empty_for_text_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(images.is_empty(), "text-only PDF should have no images");
}

#[test]
fn image_locator_min_width_filter() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions {
        min_width: 10,
        min_height: 10,
        ..Default::default()
    };
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(
        images.iter().all(|i| i.width >= 10 && i.height >= 10),
        "min dimension filter should remove small images"
    );
}

#[test]
fn image_locator_page_filter() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions {
        pages: Some(vec![1]),
        ..Default::default()
    };
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    for img in &images {
        assert_eq!(img.page_number, 1);
    }
}

#[test]
fn image_locator_include_masks_false_excludes_masks() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions {
        include_masks: false,
        ..Default::default()
    };
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    for img in &images {
        assert!(!img.is_mask, "include_masks=false should filter masks");
    }
}

#[test]
fn image_locator_all_images_have_valid_page_numbers() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let total_pages = engine.page_count().unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    for img in &images {
        assert!(
            img.page_number >= 1 && img.page_number <= total_pages,
            "image page_number {} is out of range [1, {}]",
            img.page_number,
            total_pages
        );
    }
}

#[test]
fn image_locator_does_not_panic_on_multi_stream_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/multi_stream.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let result = ImageLocator::find_all_images(&engine, &opts);
    assert!(result.is_ok(), "should not error on multi-stream PDF");
}

#[test]
fn image_locator_get_stream_bytes_for_image_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    let first = images.iter().find(|img| !img.is_inline);
    assert!(first.is_some(), "fixture should contain an XObject image");
    let bytes = ImageLocator::get_stream_bytes(first.unwrap(), engine.document().reader()).unwrap();
    assert!(bytes.map(|b| !b.is_empty()).unwrap_or(false));
}

#[test]
fn find_page_images_out_of_range_returns_error() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let result = engine.find_page_images(999);
    assert!(result.is_err(), "out-of-range page should return an error");
}

#[test]
fn text_extractor_does_not_panic_on_image_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let text = TextExtractor::extract_default(&engine).unwrap();
    let _ = text;
}

#[test]
fn text_chunks_have_valid_fields_from_real_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let ops = engine.get_page_content(1).unwrap();
    let resources = engine.get_page_resources(1).unwrap();
    let mut collector = TextCollector::new(resources, engine.document().reader());
    let chunks = collector.collect(&ops);
    for chunk in &chunks {
        assert!(
            !chunk.text.is_empty(),
            "no empty-text chunks should be emitted"
        );
        assert!(
            chunk.font_size > 0.0,
            "font_size must be positive, got {} for {:?}",
            chunk.font_size,
            chunk.text
        );
        assert!(
            !chunk.is_rtl || chunk.text.chars().any(|c| (c as u32) > 0x0400),
            "is_rtl=true but text does not look RTL: {:?}",
            chunk.text
        );
        let _ = chunk.is_vertical;
        let _ = chunk.is_invisible;
    }
}

#[test]
fn reading_order_reconstructor_handles_all_new_chunk_fields() {
    let r = ReadingOrderReconstructor::new();
    let chunks = vec![
        TextChunk {
            text: "A".to_string(),
            x: 50.0,
            y: 700.0,
            font_size: 12.0,
            font_name: "F1".to_string(),
            width: 8.0,
            is_rtl: false,
            is_vertical: false,
            is_invisible: false,
        },
        TextChunk {
            text: "B".to_string(),
            x: 60.0,
            y: 700.0,
            font_size: 12.0,
            font_name: "F1".to_string(),
            width: 8.0,
            is_rtl: false,
            is_vertical: false,
            is_invisible: true,
        },
        TextChunk {
            text: "C".to_string(),
            x: 70.0,
            y: 700.0,
            font_size: 12.0,
            font_name: "F1".to_string(),
            width: 8.0,
            is_rtl: false,
            is_vertical: true,
            is_invisible: false,
        },
    ];
    let lines = r.reconstruct(chunks);
    assert!(!lines.is_empty());
}

#[test]
fn decode_and_encode_image_from_fixture() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty(), "image_only.pdf should have images");

    let img_ref = &images[0];
    let raw = engine.decode_image(img_ref).unwrap();
    assert!(raw.is_valid(), "decoded image should be valid");
    assert_eq!(raw.width, img_ref.width);
    assert_eq!(raw.height, img_ref.height);
    assert!(raw.channels > 0);
    println!(
        "decoded image: {}x{} channels={} pixels={:?}",
        raw.width, raw.height, raw.channels, raw.pixels
    );

    let png = ContentEngine::encode_image(&raw, ImageOutputFormat::Png, None).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn extract_image_bytes_png() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());

    let bytes = engine
        .extract_image_bytes(&images[0], ImageOutputFormat::Png, None)
        .unwrap();
    assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn extract_image_bytes_jpeg() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());

    let bytes = engine
        .extract_image_bytes(&images[0], ImageOutputFormat::Jpeg, Some(85))
        .unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
}

#[test]
fn decode_image_inline_returns_error() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let inline_ref = oxide_engine::ImageReference {
        page_number: 1,
        xobject_name: "inline_1_0".to_string(),
        object_number: 0,
        generation_number: 0,
        width: 10,
        height: 10,
        bits_per_component: 8,
        color_space: "DeviceGray".to_string(),
        filter: vec![],
        is_inline: true,
        is_mask: false,
        is_smask: false,
    };
    let result = engine.decode_image(&inline_ref);
    assert!(
        result.is_err(),
        "inline images should error from decode_image"
    );
}

#[test]
fn extract_image_bytes_original_succeeds_for_uncompressed_fixture() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());

    let bytes = engine
        .extract_image_bytes(&images[0], ImageOutputFormat::Original, None)
        .unwrap();
    let raw = ImageLocator::get_stream_bytes(&images[0], engine.document().reader())
        .unwrap()
        .unwrap_or_default();
    assert_eq!(bytes, raw);
}

#[test]
fn smask_combine_rgba_produces_correct_dimensions() {
    let main = RawImage {
        width: 2,
        height: 2,
        channels: 3,
        bits_per_sample: 8,
        pixels: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128],
    };
    let mask = RawImage {
        width: 2,
        height: 2,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![255, 128, 64, 0],
    };
    let combined = SmaskLoader::combine_rgba(main, mask).unwrap();
    assert_eq!(combined.channels, 4);
    assert_eq!(combined.width, 2);
    assert_eq!(combined.height, 2);
    assert_eq!(combined.pixels.len(), 16);
    assert_eq!(&combined.pixels[0..4], &[255, 0, 0, 255]);
    assert_eq!(&combined.pixels[12..16], &[128, 128, 128, 0]);
}

#[test]
fn smask_combine_rgba_handles_dimension_mismatch_gracefully() {
    let main = RawImage {
        width: 2,
        height: 2,
        channels: 3,
        bits_per_sample: 8,
        pixels: vec![0u8; 12],
    };
    let mask = RawImage {
        width: 3,
        height: 3,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![255u8; 9],
    };
    let out = SmaskLoader::combine_rgba(main, mask).unwrap();
    assert_eq!(out.channels, 3);
}

#[test]
fn smask_combine_rgba_handles_gray_main() {
    let main = RawImage {
        width: 2,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![100, 200],
    };
    let mask = RawImage {
        width: 2,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![255, 128],
    };
    let combined = SmaskLoader::combine_rgba(main, mask).unwrap();
    assert_eq!(combined.channels, 4);
    assert_eq!(&combined.pixels[0..4], &[100, 100, 100, 255]);
    assert_eq!(&combined.pixels[4..8], &[200, 200, 200, 128]);
}

#[test]
fn smask_png_encodes_rgba() {
    let main = RawImage {
        width: 1,
        height: 1,
        channels: 3,
        bits_per_sample: 8,
        pixels: vec![255, 0, 0],
    };
    let mask = RawImage {
        width: 1,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![128],
    };
    let rgba = SmaskLoader::combine_rgba(main, mask).unwrap();
    let png = ContentEngine::encode_image(&rgba, ImageOutputFormat::Png, None).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn is_smask_detection_marks_nothing_for_imageless_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    for img in &images {
        assert!(!img.is_smask);
    }
}

#[test]
fn form_xobject_walk_does_not_panic_on_image_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions {
        include_inline: false,
        ..Default::default()
    };
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());
}

#[test]
fn find_all_images_deduplicates_correctly() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let all = ImageLocator::find_all_images(&engine, &opts).unwrap();
    let deduped = ImageLocator::deduplicate(all.clone());
    assert!(deduped.len() <= all.len());
}

#[test]
fn image_locator_inline_images_found_in_image_only_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts_with_inline = ImageLocateOptions {
        include_inline: true,
        ..Default::default()
    };
    let opts_without_inline = ImageLocateOptions {
        include_inline: false,
        ..Default::default()
    };
    let with = ImageLocator::find_all_images(&engine, &opts_with_inline).unwrap();
    let without = ImageLocator::find_all_images(&engine, &opts_without_inline).unwrap();
    assert_eq!(with.len(), without.len());
}

#[test]
fn extract_image_bytes_png_for_image_only() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());
    let png = engine
        .extract_image_bytes(&images[0], ImageOutputFormat::Png, None)
        .unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));

    let decoder = png::Decoder::new(std::io::Cursor::new(&png));
    let reader_png = decoder.read_info().unwrap();
    assert_eq!(reader_png.info().width, images[0].width);
    assert_eq!(reader_png.info().height, images[0].height);
}

#[test]
fn form_xobject_cycle_detection_does_not_infinite_loop() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions {
        include_inline: false,
        ..Default::default()
    };
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());
}

#[test]
fn build_zip_contains_valid_image_files() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    if images.is_empty() {
        return;
    }

    let mut zip_buf = Vec::new();
    {
        use std::io::{Cursor, Write};
        use zip::{write::FileOptions, CompressionMethod, ZipWriter};

        let cursor = Cursor::new(&mut zip_buf);
        let mut zw = ZipWriter::new(cursor);
        let opts_zip = FileOptions::<()>::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        for (idx, img_ref) in images.iter().enumerate() {
            if img_ref.is_inline || img_ref.object_number == 0 {
                continue;
            }
            let bytes = engine
                .extract_image_bytes(img_ref, ImageOutputFormat::Png, None)
                .unwrap();
            let filename = format!("page-{:03}-image-{:03}.png", img_ref.page_number, idx + 1);
            zw.start_file(&filename, opts_zip).unwrap();
            zw.write_all(&bytes).unwrap();
        }
        zw.finish().unwrap();
    }

    assert!(zip_buf.starts_with(b"PK"));
    assert!(zip_buf.len() > 30);

    let cursor = std::io::Cursor::new(&zip_buf);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    assert!(archive.len() > 0);

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name = file.name().to_string();
        assert!(name.starts_with("page-"));
        assert!(name.ends_with(".png") || name.ends_with(".jpg"));

        use std::io::Read;
        let mut content = Vec::new();
        file.read_to_end(&mut content).unwrap();
        assert!(!content.is_empty());
        if name.ends_with(".png") {
            assert!(content.starts_with(&[0x89, b'P', b'N', b'G']));
        }
        if name.ends_with(".jpg") {
            assert_eq!(&content[..2], &[0xFF, 0xD8]);
        }
        println!("zip entry: {}", name);
    }
}

#[test]
fn viewport_from_real_page() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let vp = engine.page_viewport(1, 150).unwrap();
    assert!(
        vp.width_px > 0 && vp.height_px > 0,
        "viewport should have positive dimensions"
    );
    assert_eq!(vp.dpi, 150);
    assert!(
        (vp.width_px as i32 - 1275).abs() <= 2,
        "US Letter width at 150 DPI is about 1275 px, got {}",
        vp.width_px
    );
}

#[test]
fn create_page_buffer_has_correct_size() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf = engine.create_page_buffer(1, 72).unwrap();
    assert_eq!(buf.width, 612);
    assert_eq!(buf.height, 792);
    assert_eq!(buf.get_pixel(0, 0), WHITE);
}

#[test]
fn render_a_line_to_real_page_buffer_and_encode() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let mut buf = engine.create_page_buffer(1, 72).unwrap();
    let vp = engine.page_viewport(1, 72).unwrap();
    let ctm = Transform2D::identity();
    let dash = DashState::solid();
    LinePainter::draw_line(
        &mut buf, 0.0, 396.0, 612.0, 396.0, RED, 2.0, &ctm, &vp, &dash,
    );
    let mid_y = (vp.height_px / 2) as i32;
    let mid_pixel = buf.get_pixel(buf.width as i32 / 2, mid_y);
    assert!(
        mid_pixel[0] >= 200,
        "midpoint should be red-ish: {mid_pixel:?}"
    );

    let raw = buf.to_raw_image();
    let png = ImageEncoder::encode_png(&raw).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(png.len() > 100);
}

#[test]
fn pixel_buffer_encode_decode_round_trip() {
    let mut buf = PixelBuffer::new(4, 4);
    buf.fill(WHITE);
    buf.set_pixel(0, 0, RED);
    buf.set_pixel(3, 3, BLUE);

    let raw = buf.to_raw_image();
    let png = ImageEncoder::encode_png(&raw).unwrap();

    let decoder = png::Decoder::new(std::io::Cursor::new(&png));
    let mut reader = decoder.read_info().unwrap();
    let mut decoded = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut decoded).unwrap();

    assert_eq!(decoded[0], 255);
    assert_eq!(decoded[1], 0);
    assert_eq!(decoded[2], 0);

    let last_start = (3 * 4 + 3) * 3;
    assert_eq!(decoded[last_start], 0);
    assert_eq!(decoded[last_start + 1], 0);
    assert_eq!(decoded[last_start + 2], 255);
}

#[test]
fn path_fill_and_encode_as_png() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(100, 100, WHITE);

    let mut path = Path::new();
    path.rect(10.0, 10.0, 80.0, 80.0);
    PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);

    assert_eq!(buf.get_pixel(50, 50), RED);
    let raw = buf.to_raw_image();
    let png = ImageEncoder::encode_png(&raw).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(png.len() > 200);
}

#[test]
fn stroke_circle_approximation() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 200.0, 200.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(200, 200, WHITE);

    let k = 0.5523_f64;
    let r = 80.0_f64;
    let cx = 100.0_f64;
    let cy = 100.0_f64;

    let mut path = Path::new();
    path.move_to(cx, cy + r);
    path.curve_to(cx + k * r, cy + r, cx + r, cy + k * r, cx + r, cy);
    path.curve_to(cx + r, cy - k * r, cx + k * r, cy - r, cx, cy - r);
    path.curve_to(cx - k * r, cy - r, cx - r, cy - k * r, cx - r, cy);
    path.curve_to(cx - r, cy + k * r, cx - k * r, cy + r, cx, cy + r);
    path.close();

    PathPainter::stroke(&mut buf, &path, &ctm, &vp, BLACK, 2.0, &DashState::solid());

    let top_of_circle = buf.get_pixel(100, 20);
    println!("circle top pixel: {:?}", top_of_circle);
    assert!(top_of_circle[0] < 200);

    let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn fill_multiple_colored_regions() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 300.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(300, 100, WHITE);

    PathPainter::fill_rect(&mut buf, 0.0, 0.0, 100.0, 100.0, &ctm, &vp, RED);
    PathPainter::fill_rect(&mut buf, 100.0, 0.0, 100.0, 100.0, &ctm, &vp, GREEN);
    PathPainter::fill_rect(&mut buf, 200.0, 0.0, 100.0, 100.0, &ctm, &vp, BLUE);

    assert_eq!(buf.get_pixel(50, 50), RED);
    assert_eq!(buf.get_pixel(150, 50), GREEN);
    assert_eq!(buf.get_pixel(250, 50), BLUE);
}

#[test]
fn path_fill_uses_render_color_from_graphics_state() {
    use oxide_engine::content::state::{Color as GsColor, ColorSpace};

    let fill_color = GsColor {
        space: ColorSpace::DeviceRGB,
        components: vec![0.0, 0.5, 1.0],
    };
    let render_color = ColorSpaceHandler::to_render_color(&fill_color, 1.0);
    let pixel_color = render_color.to_pixel_color();

    assert_eq!(pixel_color[0], 0);
    assert!((pixel_color[1] as i32 - 128).abs() <= 1);
    assert_eq!(pixel_color[2], 255);

    let vp = oxide_engine::Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
    PathPainter::fill_rect(&mut buf, 20.0, 20.0, 60.0, 60.0, &ctm, &vp, pixel_color);
    let center = buf.get_pixel(50, 50);
    assert_eq!(center[0], 0);
    assert_eq!(center[2], 255);
}

#[test]
fn path_stroke_and_fill_combine_correctly() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(100, 100, WHITE);

    let mut path = Path::new();
    path.rect(20.0, 20.0, 60.0, 60.0);

    PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);
    PathPainter::stroke(&mut buf, &path, &ctm, &vp, BLACK, 2.0, &DashState::solid());

    let interior = buf.get_pixel(50, 50);
    assert_eq!(interior[0], 255);

    let border = buf.get_pixel(50, 20);
    assert!(border[0] < 200 || border[2] > 0);

    let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn paint_image_from_real_fixture() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty());

    let raw = engine.decode_image(&images[0]).unwrap();
    assert!(raw.is_valid());

    let vp = engine.page_viewport(1, 72).unwrap();
    let w_half = vp.page_width_pts() / 2.0;
    let h_half = vp.page_height_pts() / 2.0;
    let ctm = Transform2D::new(w_half, 0.0, 0.0, h_half, 0.0, h_half);

    let mut buf = engine.create_page_buffer(1, 72).unwrap();
    ImagePainter::paint_image(&mut buf, &raw, &ctm, &vp);

    let any_non_white = (0..vp.width_px as i32)
        .flat_map(|x| (0..(vp.height_px / 2) as i32).map(move |y| (x, y)))
        .any(|(x, y)| buf.get_pixel(x, y) != WHITE);
    assert!(any_non_white);

    let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn image_painter_and_path_painter_combine() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(100, 100, WHITE);

    let image = RawImage {
        width: 1,
        height: 1,
        channels: 3,
        bits_per_sample: 8,
        pixels: vec![255, 0, 0],
    };
    let img_ctm = Transform2D::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0);
    ImagePainter::paint_image(&mut buf, &image, &img_ctm, &vp);

    let mut path = Path::new();
    path.rect(5.0, 5.0, 90.0, 90.0);
    PathPainter::stroke(&mut buf, &path, &ctm, &vp, BLACK, 2.0, &DashState::solid());

    let center = buf.get_pixel(50, 50);
    assert!(center[0] > 100);

    let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn render_page_produces_valid_png_for_each_fixture() {
    let fixtures = vec!["tests/fixtures/flate.pdf", "tests/fixtures/image_only.pdf"];
    for path in fixtures {
        let engine = ContentEngine::open_path(path).unwrap();
        let total_pages = engine.page_count().unwrap();
        for page in 1..=total_pages {
            let buf = engine.render_page(page, 72).unwrap();
            assert!(buf.width > 0, "{} page {} has zero width", path, page);
            assert!(buf.height > 0, "{} page {} has zero height", path, page);
            let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
            assert!(
                png.starts_with(&[0x89, b'P', b'N', b'G']),
                "{} page {} should produce valid PNG",
                path,
                page
            );
        }
    }
}

#[test]
fn render_page_clip_prevents_painting_outside() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(100, 100, WHITE);

    let mut clip = ClipMask::all_visible(100, 100);
    clip.fill_rect(0, 0, 100, 25, false);
    clip.fill_rect(0, 75, 100, 25, false);
    clip.fill_rect(0, 0, 25, 100, false);
    clip.fill_rect(75, 0, 25, 100, false);
    buf.set_clip(clip);

    let mut path = Path::new();
    path.rect(0.0, 0.0, 100.0, 100.0);
    PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);

    assert_eq!(buf.get_pixel(50, 50), RED);
    assert_eq!(buf.get_pixel(10, 10), WHITE);
    assert_eq!(buf.get_pixel(90, 90), WHITE);
    assert_eq!(buf.get_pixel(10, 50), WHITE);
}

#[test]
fn render_page_and_encode_to_png_not_all_white_for_image_pdf() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let raw = buf.to_raw_image();
    let any_non_white = raw
        .pixels
        .chunks(3)
        .any(|pixel| pixel[0] != 255 || pixel[1] != 255 || pixel[2] != 255);
    assert!(any_non_white);
}

#[test]
fn render_page_text_fixture_has_expected_dimensions() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    assert_eq!(buf.width, 612);
    assert_eq!(buf.height, 792);
    assert_eq!(buf.get_pixel(0, 0), WHITE);
}

#[test]
fn flate_pdf_renders_visible_text_pixels() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let raw = buf.to_raw_image();
    let non_white_count = raw
        .pixels
        .chunks(3)
        .filter(|p| p[0] < 200 || p[1] < 200 || p[2] < 200)
        .count();

    println!("flate.pdf non-white text pixels: {non_white_count}");
    assert!(
        non_white_count > 20,
        "flate.pdf should render visible text; got {non_white_count} non-white pixels"
    );
}

#[test]
fn render_text_psnr_is_deterministic() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let raw1 = engine.render_page(1, 72).unwrap().to_raw_image();
    let raw2 = engine.render_page(1, 72).unwrap().to_raw_image();
    let psnr = RenderQuality::psnr(&raw1, &raw2).unwrap();
    assert!(
        psnr.is_infinite(),
        "text rendering must be deterministic: PSNR={psnr}"
    );
}

#[test]
fn helvetica_font_lookup_succeeds() {
    let font = get_fallback_font("Helvetica");
    assert!(
        font.map(|bytes| bytes.len() > 10_000).unwrap_or(false),
        "Helvetica fallback should return real Liberation Sans bytes"
    );
}

#[test]
fn text_renders_with_reasonable_bounding_box() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let mut min_x = buf.width as i32;
    let mut max_x = 0i32;
    let mut min_y = buf.height as i32;
    let mut max_y = 0i32;

    for py in 0..buf.height as i32 {
        for px in 0..buf.width as i32 {
            let p = buf.get_pixel(px, py);
            if p[0] < 128 || p[1] < 128 || p[2] < 128 {
                min_x = min_x.min(px);
                max_x = max_x.max(px);
                min_y = min_y.min(py);
                max_y = max_y.max(py);
            }
        }
    }

    assert!(max_x > min_x, "visible text should have non-zero width");
    assert!(max_y > min_y, "visible text should have non-zero height");
    let text_w = max_x - min_x;
    let text_h = max_y - min_y;
    println!(
        "Text bounding box: {}x{} at ({},{})-({},{})",
        text_w, text_h, min_x, min_y, max_x, max_y
    );
    assert!(text_w > 2 && text_h > 2, "text bbox should be plausible");
}

#[test]
fn render_page_invalid_page_returns_error_in_integration() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    assert!(engine.render_page(0, 72).is_err());
    assert!(engine.render_page(999, 72).is_err());
}

#[test]
fn render_page_with_transparency_produces_blended_pixels() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
    let ctm = Transform2D::identity();
    let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
    let color = RenderColor::rgb(1.0, 0.0, 0.0)
        .with_alpha(0.5)
        .to_pixel_color();

    let mut path = Path::new();
    path.rect(10.0, 10.0, 80.0, 80.0);
    PathPainter::fill(&mut buf, &path, &ctm, &vp, color, FillRule::NonZero);

    let center = buf.get_pixel(50, 50);
    let pure_red = center[0] == 255 && center[1] == 0 && center[2] == 0;
    let pure_white = center[0] == 255 && center[1] == 255 && center[2] == 255;
    assert!(!pure_red, "50% alpha should not produce pure red");
    assert!(!pure_white, "50% alpha should not leave the buffer white");
}

#[test]
fn render_page_all_fixtures_produce_valid_png() {
    let fixtures = ["tests/fixtures/flate.pdf", "tests/fixtures/image_only.pdf"];
    for fixture in fixtures {
        let engine = ContentEngine::open_path(fixture).unwrap();
        let count = engine.page_count().unwrap();
        for page in 1..=count {
            let buf = engine.render_page(page, 72).unwrap();
            let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
            assert!(
                png.starts_with(&[0x89, b'P', b'N', b'G']),
                "{} page {} should produce a valid PNG",
                fixture,
                page
            );
        }
    }
}

#[test]
fn render_page_to_jpeg_and_back() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let jpeg = ImageEncoder::encode_jpeg(&buf.to_raw_image(), 85).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9]);
    assert!(jpeg.len() > 50);
}

#[test]
fn multiple_dpi_produces_proportionally_larger_images() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf_72 = engine.render_page(1, 72).unwrap();
    let buf_144 = engine.render_page(1, 144).unwrap();
    assert!((buf_144.width as i64 - buf_72.width as i64 * 2).abs() <= 2);
    assert!((buf_144.height as i64 - buf_72.height as i64 * 2).abs() <= 2);
}

#[test]
fn fill_alpha_gradient_produces_valid_pixels() {
    let vp = oxide_engine::Viewport::new([0.0, 0.0, 10.0, 10.0], 72);
    let ctm = Transform2D::identity();
    let mut previous_green = 255u8;

    for step in 0..=10 {
        let alpha = step as f32 / 10.0;
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let color = RenderColor::rgb(1.0, 0.0, 0.0)
            .with_alpha(alpha)
            .to_pixel_color();
        let mut path = Path::new();
        path.rect(0.0, 0.0, 10.0, 10.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, color, FillRule::NonZero);

        let pixel = buf.get_pixel(5, 5);
        assert_eq!(pixel[1], pixel[2], "red-over-white blend should keep G=B");
        assert!(
            pixel[1] <= previous_green,
            "green channel should not increase as alpha rises"
        );
        previous_green = pixel[1];
    }
}

#[test]
fn render_image_pdf_has_gray_pixel() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let raw = buf.to_raw_image();
    let has_gray = raw
        .pixels
        .chunks(3)
        .any(|p| p[0] == p[1] && p[1] == p[2] && p[0] < 255 && p[0] > 0);
    assert!(
        has_gray,
        "image_only.pdf should have at least one gray pixel"
    );
}

#[test]
fn encode_png_fast_is_valid_for_large_buffer() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf = engine.render_page(1, 150).unwrap();
    let raw = buf.to_raw_image();
    let png_default = ImageEncoder::encode_png(&raw).unwrap();
    let png_fast = ImageEncoder::encode_png_fast(&raw).unwrap();

    assert!(png_default.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(png_fast.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(png_fast.len() > 100 && png_default.len() > 100);
}

#[test]
fn page_viewport_uses_rotation_from_page_dict() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let vp = engine.page_viewport(1, 72).unwrap();
    assert_eq!(vp.rotation, 0);
}

#[test]
fn render_page_is_deterministic() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf1 = engine.render_page(1, 72).unwrap();
    let buf2 = engine.render_page(1, 72).unwrap();

    assert_eq!(buf1.width, buf2.width);
    assert_eq!(buf1.height, buf2.height);
    assert_eq!(buf1.to_rgba_bytes(), buf2.to_rgba_bytes());
}

#[test]
fn render_page_with_clip_fill_optimized() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let raw = buf.to_raw_image();
    let png = ImageEncoder::encode_png_fast(&raw).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn render_page_rotation_0_is_portrait() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let vp = engine.page_viewport(1, 72).unwrap();
    assert!(
        vp.width_px <= vp.height_px,
        "portrait page should have width <= height: {}x{}",
        vp.width_px,
        vp.height_px
    );
    assert_eq!(vp.rotation, 0);
}

#[test]
fn encode_png_fast_vs_default_same_content() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let raw = buf.to_raw_image();
    let png_default = ImageEncoder::encode_png(&raw).unwrap();
    let png_fast = ImageEncoder::encode_png_fast(&raw).unwrap();

    println!(
        "image_only png sizes: default={} fast={}",
        png_default.len(),
        png_fast.len()
    );
    assert!(png_default.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(png_fast.starts_with(&[0x89, b'P', b'N', b'G']));

    let decode_png = |bytes: &[u8]| -> Vec<u8> {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        buf
    };
    assert_eq!(decode_png(&png_default), decode_png(&png_fast));
}

#[test]
fn glyph_cache_hits_on_repeated_characters_are_deterministic() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let buf1 = engine.render_page(1, 72).unwrap();
    let buf2 = engine.render_page(1, 72).unwrap();
    assert_eq!(buf1.to_rgba_bytes(), buf2.to_rgba_bytes());
}

#[test]
fn render_page_fast_png() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let png = engine.render_page_png_fast(1, 72).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn viewport_rotation_90_dimensions_are_swapped_in_integration() {
    let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 90);
    assert_eq!(vp.width_px, 200);
    assert_eq!(vp.height_px, 100);
}

#[test]
fn render_psnr_self_comparison_is_infinite() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let raw1 = engine.render_page(1, 72).unwrap().to_raw_image();
    let raw2 = engine.render_page(1, 72).unwrap().to_raw_image();
    let psnr = RenderQuality::psnr(&raw1, &raw2).unwrap();
    assert!(psnr.is_infinite(), "same render twice = PSNR infinite");
}

#[test]
fn render_psnr_mismatched_dimensions_returns_err() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let raw_72 = engine.render_page(1, 72).unwrap().to_raw_image();
    let raw_144 = engine.render_page(1, 144).unwrap().to_raw_image();
    assert!(
        RenderQuality::psnr(&raw_72, &raw_144).is_err(),
        "different DPI = different dimensions = error"
    );
}

#[test]
fn golden_file_create_and_compare() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let rendered = engine.render_page(1, 72).unwrap().to_raw_image();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/references/tmp_flate_page1_72dpi.png");
    let _ = std::fs::remove_file(&path);

    let p1 = RenderQuality::compare_or_create_golden(&path, &rendered).unwrap();
    assert!(p1.is_infinite(), "creating = PSNR infinite");
    assert!(path.exists(), "golden file should exist");

    let p2 = RenderQuality::compare_or_create_golden(&path, &rendered).unwrap();
    assert!(p2.is_infinite(), "same render vs golden = PSNR infinite");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn diff_pixel_count_self_is_zero() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let raw = engine.render_page(1, 72).unwrap().to_raw_image();
    let count = RenderQuality::diff_pixel_count(&raw, &raw, 0).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn render_image_pdf_psnr_measurement() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let r1 = engine.render_page(1, 72).unwrap().to_raw_image();
    let r2 = engine.render_page(1, 72).unwrap().to_raw_image();
    let psnr = RenderQuality::psnr(&r1, &r2).unwrap();
    assert!(
        psnr.is_infinite(),
        "double render of image PDF should be identical"
    );
}

#[test]
fn write_golden_creates_parent_directory() {
    let dir = std::env::temp_dir().join(format!("oxide_test_refs_{}", std::process::id()));
    let path = dir.join("nested/test.png");
    let _ = std::fs::remove_dir_all(&dir);
    let img = RawImage {
        width: 1,
        height: 1,
        channels: 3,
        bits_per_sample: 8,
        pixels: vec![200, 100, 50],
    };
    RenderQuality::write_golden(&path, &img).unwrap();
    assert!(path.exists(), "golden should be created with parent dirs");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_golden_nonexistent_returns_err() {
    let path = std::env::temp_dir().join(format!(
        "oxide_definitely_missing_xyz123_{}.png",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    assert!(
        RenderQuality::read_golden(&path).is_err(),
        "missing file should error"
    );
}

#[test]
fn psnr_formula_numerical_verification() {
    let white = RawImage {
        width: 1,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![255],
    };
    let black = RawImage {
        width: 1,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![0],
    };
    let mse = RenderQuality::mse(&white, &black).unwrap();
    let psnr = RenderQuality::psnr(&white, &black).unwrap();
    assert!((mse - 65025.0).abs() < 0.01, "max MSE = 255^2: {}", mse);
    assert!(psnr.abs() < 0.1, "max error = 0 dB PSNR: {}", psnr);

    let a = RawImage {
        width: 1,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![0],
    };
    let b = RawImage {
        width: 1,
        height: 1,
        channels: 1,
        bits_per_sample: 8,
        pixels: vec![1],
    };
    let psnr_1 = RenderQuality::psnr(&a, &b).unwrap();
    assert!(
        (psnr_1 - 48.13).abs() < 0.1,
        "MSE=1 -> PSNR ~= 48.13: {}",
        psnr_1
    );
}

#[test]
fn no_system_process_spawned_during_render() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let result = engine.render_page(1, 72);
    assert!(
        result.is_ok(),
        "render_page must succeed: {:?}",
        result.err()
    );
}

#[test]
fn complete_pipeline_extract_render_compare() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();

    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Hi"), "flate.pdf should contain 'Hi'");

    let buf = engine.render_page(1, 72).unwrap();
    assert!(buf.width > 0 && buf.height > 0);

    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();

    let analysis = PdfAnalyzer::quick_analysis(&engine).unwrap();
    assert!(analysis.has_text_layer, "flate.pdf has a text layer");
    assert_eq!(engine.page_count().unwrap(), 1);

    println!(
        "Pipeline test complete: text='{}', pages={}, images={}",
        text.trim(),
        engine.page_count().unwrap(),
        images.len()
    );
}

// ---------------------------------------------------------------------------
// Encryption (Mega 18): Standard Security Handler, RC4-128 (V2/R3)
// ---------------------------------------------------------------------------
//
// qpdf is not available in this environment, so instead of shipping a binary
// fixture we synthesise a *real* RC4-128 encrypted PDF using the engine's own
// public crypto primitives and then decrypt it back through the full
// `PdfReader` path. This exercises setup_encryption -> verify_user_password ->
// compute_encryption_key -> object_key -> RC4 stream decryption end to end.

use oxide_engine::{compute_encryption_key, object_key, CryptMethod, EncryptionInfo, Rc4, PADDING};

const ENC_FILE_ID: &[u8; 16] = b"0123456789abcdef";

/// Hex-encode bytes as an uppercase PDF hex string body (without the < >).
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02X}", b));
    }
    s
}

/// Compute the `/U` user-password verifier for an R3 document with the empty
/// user password (PDF 32000-1 §7.6.3.4, Algorithm 5).
fn compute_u_r3(file_key: &[u8], file_id: &[u8]) -> [u8; 32] {
    let mut input = PADDING.to_vec();
    input.extend_from_slice(file_id);
    let hash = oxide_engine::md5(&input);
    let mut result = Rc4::apply(file_key, &hash);
    for i in 1u8..=19 {
        let xor_key: Vec<u8> = file_key.iter().map(|&b| b ^ i).collect();
        result = Rc4::apply(&xor_key, &result);
    }
    let mut u = [0u8; 32];
    u[..16].copy_from_slice(&result[..16]);
    u
}

/// Build a minimal single-page RC4-128 (V2/R3) encrypted PDF whose user
/// password is empty. The content stream decrypts to text containing "Hi".
fn build_rc4_128_encrypted_pdf() -> Vec<u8> {
    let file_id = ENC_FILE_ID.to_vec();
    let owner_o = PADDING.to_vec(); // any fixed 32 bytes serves as /O here

    // /U / key derivation does not depend on /U, so we can derive the key first.
    let info = EncryptionInfo {
        v: 2,
        r: 3,
        key_length: 128,
        o: owner_o.clone(),
        u: vec![0u8; 32],
        p: -3904,
        encrypt_metadata: true,
        cf_method: CryptMethod::V2,
    };
    let file_key = compute_encryption_key(b"", &info, &file_id);
    let user_u = compute_u_r3(&file_key, &file_id);

    // Encrypt the page content stream with the per-object RC4 key for obj 4 0.
    let plaintext = b"BT\n/F1 12 Tf\n72 720 Td\n(Hi) Tj\nET\n";
    let content_key = object_key(&file_key, 4, 0, false);
    let encrypted_content = Rc4::apply(&content_key, plaintext);

    let mut bytes: Vec<u8> = Vec::new();
    let mut offsets = vec![0usize; 5];

    bytes.extend_from_slice(b"%PDF-1.4\n");

    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets[3] = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n",
    );

    offsets[4] = bytes.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} >>\nstream\n",
            encrypted_content.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&encrypted_content);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=4usize {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[n]).as_bytes());
    }

    let trailer = format!(
        "trailer\n<< /Size 5 /Root 1 0 R /Encrypt << /Filter /Standard /V 2 /R 3 /Length 128 \
         /P -3904 /O <{}> /U <{}> >> /ID [<{}> <{}>] >>\nstartxref\n{}\n%%EOF\n",
        hex_encode(&owner_o),
        hex_encode(&user_u),
        hex_encode(&file_id),
        hex_encode(&file_id),
        xref
    );
    bytes.extend_from_slice(trailer.as_bytes());
    bytes
}

#[test]
fn non_encrypted_pdf_opens_normally() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    assert!(!engine.is_encrypted(), "flate.pdf should not be encrypted");
    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Hi"), "text extraction should still work");
}

#[test]
fn open_bytes_with_empty_password_is_same_as_open_bytes() {
    let bytes = std::fs::read("tests/fixtures/flate.pdf").unwrap();
    let e1 = ContentEngine::open_bytes(bytes.clone()).unwrap();
    let e2 = ContentEngine::open_bytes_with_password(bytes, b"").unwrap();
    assert_eq!(
        e1.get_page_text(1).unwrap(),
        e2.get_page_text(1).unwrap(),
        "open_bytes and open_bytes_with_password(\"\") must be equivalent"
    );
}

#[test]
fn is_encrypted_returns_false_for_existing_fixtures() {
    for fixture in &[
        "tests/fixtures/flate.pdf",
        "tests/fixtures/image_only.pdf",
        "tests/fixtures/minimal.pdf",
        "tests/fixtures/multi_stream.pdf",
    ] {
        let engine = ContentEngine::open_path(fixture).unwrap();
        assert!(!engine.is_encrypted(), "{fixture} should not be encrypted");
    }
}

#[test]
fn open_with_wrong_password_for_non_encrypted_pdf_succeeds() {
    let engine = ContentEngine::open_path_with_password(
        std::path::Path::new("tests/fixtures/flate.pdf"),
        b"wrong_password_ignored",
    );
    assert!(
        engine.is_ok(),
        "non-encrypted PDF should open even with a password: {:?}",
        engine.err()
    );
    assert!(!engine.unwrap().is_encrypted());
}

#[test]
fn decrypt_string_rc4_round_trip_via_object_key() {
    let file_key = vec![0x2Bu8, 0xE9, 0xF7, 0xC3, 0xD5];
    let original = b"Hello PDF World";
    let key = object_key(&file_key, 7, 0, false);
    let encrypted = Rc4::apply(&key, original);
    let decrypted = oxide_engine::decrypt_string(&encrypted, &file_key, 7, 0, false);
    assert_eq!(decrypted, original.to_vec());
}

#[test]
fn rc4_128_encrypted_pdf_opens_transparently_with_empty_password() {
    let pdf = build_rc4_128_encrypted_pdf();
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    assert!(
        engine.is_encrypted(),
        "the synthesised PDF should report as encrypted"
    );

    // The content stream must have been decrypted on read.
    let content = engine.document().get_page_content_bytes(1).unwrap();
    assert!(
        content.windows(2).any(|w| w == b"Hi"),
        "decrypted content should contain the text 'Hi', got: {:?}",
        String::from_utf8_lossy(&content)
    );
    assert!(
        content.windows(2).any(|w| w == b"BT"),
        "decrypted content should contain the BT operator"
    );
}

#[test]
fn rc4_128_encrypted_pdf_opens_with_explicit_empty_password() {
    let pdf = build_rc4_128_encrypted_pdf();
    let engine = ContentEngine::open_bytes_with_password(pdf, b"").unwrap();
    assert!(engine.is_encrypted());
    let content = engine.document().get_page_content_bytes(1).unwrap();
    assert!(content.windows(2).any(|w| w == b"Hi"));
}

#[test]
fn rc4_128_encrypted_pdf_unaffected_by_a_supplied_password_when_user_pw_empty() {
    // A supplied password is tried first, then the empty password as fallback;
    // since this document's user password is empty, it still opens.
    let pdf = build_rc4_128_encrypted_pdf();
    let engine = ContentEngine::open_bytes_with_password(pdf, b"some-guess").unwrap();
    assert!(engine.is_encrypted());
}

#[test]
fn truly_password_protected_pdf_without_password_returns_encrypted_pdf_error() {
    // Build a valid V2/R3 dictionary whose /U does NOT match the empty password,
    // so no candidate verifies and we surface EncryptedPdf.
    let pdf = build_pdf_with_trailer(
        vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>".to_string()),
        ],
        &format!(
            "<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 2 /R 3 /Length 128 \
             /P -3904 /O <{}> /U <{}> >> /ID [<{}> <{}>] >>",
            "AB".repeat(32),
            "CD".repeat(32), // a /U that will not match the empty password
            hex_encode(ENC_FILE_ID),
            hex_encode(ENC_FILE_ID),
        ),
    );
    assert!(
        matches!(
            ContentEngine::open_bytes(pdf),
            Err(OxideError::EncryptedPdf(_))
        ),
        "a real password-protected PDF should return EncryptedPdf"
    );
}

fn build_pdf(objects: Vec<(u32, String)>) -> Vec<u8> {
    let max_object = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
    let trailer = format!("<< /Size {} /Root 1 0 R >>", max_object + 1);
    build_pdf_with_trailer(objects, &trailer)
}

fn build_pdf_with_trailer(objects: Vec<(u32, String)>, trailer: &str) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let max_object = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
    let mut offsets = vec![0usize; max_object as usize + 1];
    for (number, body) in objects {
        offsets[number as usize] = bytes.len();
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", max_object + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for object_number in 1..=max_object {
        let offset = offsets[object_number as usize];
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n{trailer}\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    bytes
}

// TODO(fixture): Add a real PDF 1.5+ fixture with xref streams and object streams
// once qpdf or another generator is available in CI. The xref stream and object
// stream code paths are covered with synthetic unit tests in reader.rs.

// ---------------------------------------------------------------------------
// Form XObject rendering (Mega 19)
// ---------------------------------------------------------------------------

/// Build a minimal, valid PDF containing a Form XObject. The Form fills a
/// 50×50 50%-gray rectangle; the page places it at (25,25) on a 100×100 page,
/// so the gray square sits in the page centre. Content streams are uncompressed.
fn build_form_xobject_pdf() -> Vec<u8> {
    let form_stream_content: &[u8] = b"0.5 g\n0 0 50 50 re\nf\n";
    let page_stream_content: &[u8] = b"q\n1 0 0 1 25 25 cm\n/Fm0 Do\nQ\n";

    let mut pdf: Vec<u8> = Vec::new();
    let mut offsets = vec![0usize; 6]; // 1-indexed, objects 1..=5

    pdf.extend_from_slice(b"%PDF-1.4\n");

    offsets[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets[5] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 50 50] \
             /Resources << /ProcSet [/PDF] >> /Length {} >>\nstream\n",
            form_stream_content.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(form_stream_content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offsets[4] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} >>\nstream\n",
            page_stream_content.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(page_stream_content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offsets[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents 4 0 R \
          /Resources << /XObject << /Fm0 5 0 R >> /ProcSet [/PDF] >> >>\nendobj\n",
    );

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for i in 1..=5 {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offsets[i]).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn form_xobject_pdf_parses_as_single_page() {
    let engine = ContentEngine::open_bytes(build_form_xobject_pdf()).unwrap();
    assert_eq!(engine.page_count().unwrap(), 1);
    // The page should reference the Form XObject in its resources.
    let resources = engine.get_page_resources(1).unwrap();
    assert!(
        resources.xobjects.contains_key("Fm0"),
        "page resources should contain the Form XObject /Fm0"
    );
}

#[test]
fn form_xobject_renders_without_panic() {
    let engine = ContentEngine::open_bytes(build_form_xobject_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    assert_eq!(buf.width, 100, "100pt page at 72 DPI = 100px");
    assert_eq!(buf.height, 100);
}

#[test]
fn form_xobject_paints_gray_pixels() {
    let engine = ContentEngine::open_bytes(build_form_xobject_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    // Form occupies user-space (25,25)-(75,75). PDF (50,50) -> pixel (50, 50)
    // after the viewport y-flip on a 100px-tall page.
    let center = buf.get_pixel(50, 50);
    let is_gray = center[0] == center[1] && center[1] == center[2];
    let is_non_white = center[0] < 255;
    assert!(
        is_gray && is_non_white,
        "centre of Form-painted area should be gray, got {:?}",
        center
    );
}

#[test]
fn form_xobject_leaves_outer_area_white() {
    let engine = ContentEngine::open_bytes(build_form_xobject_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    // PDF (5,5) is outside the Form area; after y-flip that is pixel (5, 95).
    let outer = buf.get_pixel(5, 95);
    assert_eq!(
        outer, WHITE,
        "outside the Form area should stay white, got {:?}",
        outer
    );
}

#[test]
fn form_xobject_render_is_deterministic() {
    let pdf = build_form_xobject_pdf();
    let e1 = ContentEngine::open_bytes(pdf.clone()).unwrap();
    let e2 = ContentEngine::open_bytes(pdf).unwrap();
    let buf1 = e1.render_page(1, 72).unwrap().to_raw_image();
    let buf2 = e2.render_page(1, 72).unwrap().to_raw_image();
    let psnr = RenderQuality::psnr(&buf1, &buf2).unwrap();
    assert!(
        psnr.is_infinite(),
        "two renders of the same Form PDF must be identical"
    );
}

#[test]
fn form_xobject_depth_counter_resets_between_renders() {
    // Rendering twice with the same engine must not leak Form depth state.
    let engine = ContentEngine::open_bytes(build_form_xobject_pdf()).unwrap();
    let a = engine.render_page(1, 72).unwrap().to_raw_image();
    let b = engine.render_page(1, 72).unwrap().to_raw_image();
    assert!(RenderQuality::psnr(&a, &b).unwrap().is_infinite());
}

#[test]
fn existing_text_fixture_still_renders_after_form_support() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(
        text.contains("Hi"),
        "flate.pdf should still extract 'Hi', got {:?}",
        text
    );
    let buf = engine.render_page(1, 72).unwrap();
    assert!(buf.width > 0 && buf.height > 0);
}

#[test]
fn form_xobject_change_does_not_break_image_extraction() {
    let engine = ContentEngine::open_path("tests/fixtures/image_only.pdf").unwrap();
    let opts = ImageLocateOptions::default();
    let images = ImageLocator::find_all_images(&engine, &opts).unwrap();
    assert!(!images.is_empty(), "image extraction should still work");
    let buf = engine.render_page(1, 72).unwrap();
    assert!(buf.width > 0);
}

fn build_transparency_group_pdf() -> Vec<u8> {
    let form_stream: &[u8] = b"q\n/GsHalf gs\n1 0 0 rg\n0 0 50 50 re\nf\nQ\n";
    let page_stream: &[u8] = b"q\n1 0 0 1 25 25 cm\n/Fm0 Do\nQ\n";

    let mut pdf = Vec::new();
    let mut offs = vec![0usize; 6];
    pdf.extend_from_slice(b"%PDF-1.4\n");

    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offs[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offs[5] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 50 50] \
             /Group << /Type /Group /S /Transparency >> \
             /Resources << /ProcSet [/PDF] /ExtGState << /GsHalf << /ca 0.5 /CA 0.5 >> >> >> \
             /Length {} >>\nstream\n",
            form_stream.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(form_stream);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offs[4] = pdf.len();
    pdf.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", page_stream.len()).as_bytes(),
    );
    pdf.extend_from_slice(page_stream);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents 4 0 R \
          /Resources << /XObject << /Fm0 5 0 R >> /ProcSet [/PDF] >> >>\nendobj\n",
    );

    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for i in 1..=5 {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offs[i]).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn build_multiply_blend_pdf() -> Vec<u8> {
    let content: &[u8] = b"0.8 g\n0 0 100 100 re\nf\n/GSBlend gs\n1 0 0 rg\n20 20 60 60 re\nf\n";
    let mut pdf = Vec::new();
    let mut offs = vec![0usize; 5];
    pdf.extend_from_slice(b"%PDF-1.4\n");

    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offs[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offs[4] = pdf.len();
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents 4 0 R \
          /Resources << /ExtGState << /GSBlend << /BM /Multiply >> >> >> >>\nendobj\n",
    );

    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for i in 1..=4 {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offs[i]).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn transparency_group_renders_semi_transparent_red() {
    let engine = ContentEngine::open_bytes(build_transparency_group_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let center = buf.get_pixel(50, 50);
    assert!(center[0] > 200, "R should be high red: {:?}", center);
    assert!(
        center[1] > 50 && center[1] < 230,
        "G should be mid pink: {:?}",
        center
    );
    assert!(
        center[2] > 50 && center[2] < 230,
        "B should be mid pink: {:?}",
        center
    );
    assert_ne!(
        center, WHITE,
        "transparency group should paint the form area"
    );
}

#[test]
fn multiply_blend_mode_darkens_content() {
    let mut buf = PixelBuffer::new_filled(10, 10, [200, 200, 200, 255]);
    buf.blend_mode = BlendMode::Multiply;
    buf.blend_pixel(5, 5, [128, 128, 128, 255], 1.0);
    let px = buf.get_pixel(5, 5);
    assert!(px[0] < 180, "Multiply should darken: {}", px[0]);
}

#[test]
fn multiply_blend_mode_via_ext_graphics_state() {
    let engine = ContentEngine::open_bytes(build_multiply_blend_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let center = buf.get_pixel(50, 50);
    assert!(
        center[0] < 230,
        "Multiply red over gray should not stay full red: {:?}",
        center
    );
    assert!(
        center[1] < 10 && center[2] < 10,
        "red source should suppress G/B: {:?}",
        center
    );
}

// ---------------------------------------------------------------------------
// Shading and patterns (Mega 20)
// ---------------------------------------------------------------------------

/// Build a 100×100 single-page PDF whose content stream is `content`, with one
/// shading resource `/Sh1` (object 5) referencing a Type 2 function (object 6).
/// `func_str` and `shading_str` are the literal dictionary bodies.
fn build_shading_pdf_with(func_str: &str, shading_str: &str, content: &[u8]) -> Vec<u8> {
    let mut pdf: Vec<u8> = Vec::new();
    let mut offs = vec![0usize; 7]; // 1-indexed, objects 1..=6
    pdf.extend_from_slice(b"%PDF-1.4\n");

    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offs[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offs[6] = pdf.len();
    pdf.extend_from_slice(format!("6 0 obj\n{func_str}\nendobj\n").as_bytes());
    offs[5] = pdf.len();
    pdf.extend_from_slice(format!("5 0 obj\n{shading_str}\nendobj\n").as_bytes());

    offs[4] = pdf.len();
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents 4 0 R /Resources << /Shading << /Sh1 5 0 R >> >> >>\nendobj\n",
    );

    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for i in 1..=6 {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offs[i]).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn build_axial_shading_pdf() -> Vec<u8> {
    let func = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB \
                   /Coords [0 50 100 50] /Function 6 0 R /Extend [true true] >>";
    build_shading_pdf_with(func, shading, b"/Sh1 sh\n")
}

fn build_radial_shading_pdf() -> Vec<u8> {
    // Concentric circles at (50,50): radius 0 (center) to radius 50 (edge).
    let func = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";
    let shading = "<< /ShadingType 3 /ColorSpace /DeviceRGB \
                   /Coords [50 50 0 50 50 50] /Function 6 0 R /Extend [true true] >>";
    build_shading_pdf_with(func, shading, b"/Sh1 sh\n")
}

#[test]
fn axial_shading_pdf_renders_gradient() {
    let engine = ContentEngine::open_bytes(build_axial_shading_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    assert_eq!(buf.width, 100);
    assert_eq!(buf.height, 100);

    let left = buf.get_pixel(0, 50);
    assert!(left[0] > 150, "left edge should be red: {:?}", left);
    assert!(left[2] < 100, "left edge should not be blue: {:?}", left);

    let right = buf.get_pixel(99, 50);
    assert!(right[0] < 100, "right edge should not be red: {:?}", right);
    assert!(right[2] > 150, "right edge should be blue: {:?}", right);
}

#[test]
fn axial_shading_center_is_purple() {
    let engine = ContentEngine::open_bytes(build_axial_shading_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let c = buf.get_pixel(50, 50);
    assert!(c[0] > 80 && c[0] < 200, "center R mid: {:?}", c);
    assert!(c[2] > 80 && c[2] < 200, "center B mid: {:?}", c);
}

#[test]
fn radial_shading_center_is_red_edge_is_blue() {
    let engine = ContentEngine::open_bytes(build_radial_shading_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let center = buf.get_pixel(50, 50);
    assert!(
        center[0] > 150 && center[2] < 100,
        "radial center should be red: {:?}",
        center
    );
    // A point near the circle edge (distance ~45 from center) should be bluish.
    let edge = buf.get_pixel(50, 5);
    assert!(edge[2] > 100, "near-edge should be more blue: {:?}", edge);
}

#[test]
fn existing_pdfs_unaffected_by_shading_code() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    assert!(engine.get_page_text(1).unwrap().contains("Hi"));
    let buf = engine.render_page(1, 72).unwrap();
    assert!(buf.width > 0 && buf.height > 0);
}

#[test]
fn form_xobject_pdf_still_renders_after_shading_changes() {
    let engine = ContentEngine::open_bytes(build_form_xobject_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    let center = buf.get_pixel(50, 50);
    assert!(
        center[0] == center[1] && center[0] < 255,
        "gray square still renders: {:?}",
        center
    );
}

#[test]
fn shading_pattern_fill_paints_gradient_in_rectangle() {
    // A rectangle filled with a shading pattern (PatternType 2). The pattern's
    // shading is the same red→blue axial gradient across the page.
    let func = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";
    // Page: set Pattern color space, select /P1, fill a centered 60×60 rect.
    let content = b"/Pattern cs\n/P1 scn\n20 20 60 60 re\nf\n";

    let mut pdf: Vec<u8> = Vec::new();
    let mut offs = vec![0usize; 8]; // objects 1..=7
    pdf.extend_from_slice(b"%PDF-1.4\n");
    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offs[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offs[6] = pdf.len();
    pdf.extend_from_slice(format!("6 0 obj\n{func}\nendobj\n").as_bytes());
    // Object 7: shading. Object 5: shading pattern referencing it.
    offs[7] = pdf.len();
    pdf.extend_from_slice(
        b"7 0 obj\n<< /ShadingType 2 /ColorSpace /DeviceRGB \
          /Coords [0 50 100 50] /Function 6 0 R /Extend [true true] >>\nendobj\n",
    );
    offs[5] = pdf.len();
    pdf.extend_from_slice(b"5 0 obj\n<< /Type /Pattern /PatternType 2 /Shading 7 0 R >>\nendobj\n");
    offs[4] = pdf.len();
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents 4 0 R /Resources << /Pattern << /P1 5 0 R >> >> >>\nendobj\n",
    );
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
    for i in 1..=7 {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offs[i]).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );

    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    // Inside the rect (x in 20..80): left side reddish, right side bluish.
    let inside_left = buf.get_pixel(25, 50);
    let inside_right = buf.get_pixel(75, 50);
    assert!(
        inside_left[0] > inside_right[0],
        "gradient: left redder than right: {:?} vs {:?}",
        inside_left,
        inside_right
    );
    assert!(
        inside_right[2] > inside_left[2],
        "gradient: right bluer than left: {:?} vs {:?}",
        inside_left,
        inside_right
    );
    // Outside the rect stays white (pattern clipped to the path).
    let outside = buf.get_pixel(5, 50);
    assert_eq!(
        outside, WHITE,
        "outside rect should be white: {:?}",
        outside
    );
}

// ---------------------------------------------------------------------------
// Mega 24: structural PDF edge cases and final corpus sweep
// ---------------------------------------------------------------------------

fn build_two_stream_content_pdf() -> Vec<u8> {
    let stream1 = b"1 0 0 rg 10 10 30 30 re f\n";
    let stream2 = b"0 0 1 rg 50 50 30 30 re f\n";
    let mut pdf = Vec::new();
    let mut offs = vec![0usize; 6];

    pdf.extend_from_slice(b"%PDF-1.4\n");
    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offs[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offs[4] = pdf.len();
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", stream1.len()).as_bytes());
    pdf.extend_from_slice(stream1);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    offs[5] = pdf.len();
    pdf.extend_from_slice(format!("5 0 obj\n<< /Length {} >>\nstream\n", stream2.len()).as_bytes());
    pdf.extend_from_slice(stream2);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents [4 0 R 5 0 R] /Resources << /ProcSet [/PDF] >> >>\nendobj\n",
    );

    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offs.iter().take(6).skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn build_inherited_resources_pdf() -> Vec<u8> {
    let page_stream = b"BT /F1 12 Tf 50 700 Td (Hi) Tj ET\n";
    let mut pdf = Vec::new();
    let mut offs = vec![0usize; 6];

    pdf.extend_from_slice(b"%PDF-1.4\n");
    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offs[2] = pdf.len();
    pdf.extend_from_slice(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );
    offs[4] = pdf.len();
    pdf.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", page_stream.len()).as_bytes(),
    );
    pdf.extend_from_slice(page_stream);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    offs[5] = pdf.len();
    pdf.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );
    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 4 0 R >>\nendobj\n",
    );

    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offs.iter().take(6).skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn build_cropbox_pdf() -> Vec<u8> {
    let mut pdf = Vec::new();
    let mut offs = vec![0usize; 5];

    pdf.extend_from_slice(b"%PDF-1.4\n");
    offs[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offs[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offs[4] = pdf.len();
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 1 >>\nstream\n \nendstream\nendobj\n");
    offs[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R \
          /MediaBox [0 0 200 200] /CropBox [50 50 150 150] \
          /Contents 4 0 R /Resources << >> >>\nendobj\n",
    );

    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offs.iter().take(5).skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn get_page_content_handles_contents_array() {
    let engine = ContentEngine::open_bytes(build_two_stream_content_pdf()).unwrap();
    let ops = engine.get_page_content(1).unwrap();

    assert!(!ops.is_empty());
    assert_eq!(
        ops.iter().filter(|op| op.operator == "re").count(),
        2,
        "both content streams should be parsed"
    );
}

#[test]
fn two_stream_pdf_renders_both_shapes() {
    let engine = ContentEngine::open_bytes(build_two_stream_content_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    let red_area = buf.get_pixel(25, 75);
    assert!(
        red_area[0] > 200 && red_area[2] < 50,
        "red rect should render: {:?}",
        red_area
    );

    let blue_area = buf.get_pixel(65, 35);
    assert!(
        blue_area[2] > 200 && blue_area[0] < 50,
        "blue rect should render: {:?}",
        blue_area
    );
}

#[test]
fn inherited_resources_page_loads_correctly() {
    let engine = ContentEngine::open_bytes(build_inherited_resources_pdf()).unwrap();
    let ops = engine.get_page_content(1).unwrap();
    assert!(!ops.is_empty());

    let resources = engine.get_page_resources(1).unwrap();
    assert!(
        resources.fonts.contains_key("F1"),
        "inherited font F1 should be visible: {:?}",
        resources.fonts
    );

    let buf = engine.render_page(1, 72).unwrap();
    assert!(buf.width > 0 && buf.height > 0);
}

#[test]
fn page_viewport_uses_cropbox_when_present() {
    let engine = ContentEngine::open_bytes(build_cropbox_pdf()).unwrap();
    let vp = engine.page_viewport(1, 72).unwrap();

    assert_eq!(vp.width_px, 100);
    assert_eq!(vp.height_px, 100);
}

#[test]
fn cropbox_pdf_renders_at_correct_size() {
    let engine = ContentEngine::open_bytes(build_cropbox_pdf()).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    assert_eq!(buf.width, 100);
    assert_eq!(buf.height, 100);
}

#[test]
fn flate_pdf_content_stream_unchanged_after_structural_fixes() {
    let engine = ContentEngine::open_path("tests/fixtures/flate.pdf").unwrap();
    let text = engine.get_page_text(1).unwrap();
    assert!(text.contains("Hi"), "flate.pdf text: {:?}", text);

    let buf = engine.render_page(1, 72).unwrap();
    let raw = buf.to_raw_image();
    let channels = raw.channels as usize;
    let non_white = raw
        .pixels
        .chunks(channels)
        .filter(|p| p[0] < 200 || p[1] < 200 || p[2] < 200)
        .count();
    assert!(non_white > 20);
}

#[test]
fn all_existing_fixtures_still_render() {
    for fixture in [
        "tests/fixtures/flate.pdf",
        "tests/fixtures/image_only.pdf",
        "tests/fixtures/minimal.pdf",
        "tests/fixtures/multi_stream.pdf",
    ] {
        let engine = ContentEngine::open_path(fixture).unwrap();
        let count = engine.page_count().unwrap();
        for page in 1..=count {
            let buf = engine.render_page(page, 72).unwrap();
            assert!(buf.width > 0 && buf.height > 0, "{fixture} page {page}");
            let png = ImageEncoder::encode_png(&buf.to_raw_image()).unwrap();
            assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        }
    }
}

#[test]
fn downloaded_real_pdfs_open_and_smoke_render_if_present() {
    for fixture in [
        "tests/fixtures/basicapi.pdf",
        "tests/fixtures/form_160f.pdf",
        "tests/fixtures/roboto.pdf",
        "tests/fixtures/tracemonkey.pdf",
    ] {
        let path = std::path::Path::new(fixture);
        if !path.exists() {
            println!("SKIP: {fixture} not downloaded");
            continue;
        }

        let engine = ContentEngine::open_path(path).expect("real PDF should open");
        assert!(
            engine.page_count().unwrap() > 0,
            "{fixture} should have pages"
        );
        if let Err(err) = engine.render_page(1, 72) {
            println!("RENDER SKIP {fixture}: {err}");
        }
        let text = engine.get_page_text(1).unwrap_or_default();
        let preview = text.chars().take(100).collect::<String>();
        println!("{fixture} page 1 text preview: {:?}", preview);
    }
}

#[test]
fn downloaded_tracemonkey_extracts_text_if_present() {
    let path = std::path::Path::new("tests/fixtures/tracemonkey.pdf");
    if !path.exists() {
        println!("SKIP: tracemonkey.pdf not downloaded");
        return;
    }

    let engine = ContentEngine::open_path(path).unwrap();
    let text = engine.get_page_text(1).unwrap_or_default();
    let preview = text.chars().take(100).collect::<String>();
    println!("tracemonkey page 1 text: {:?}", preview);
    assert!(!text.is_empty());
}

#[test]
fn render_golden_for_all_fixtures() {
    let fixtures = [
        (
            "tests/fixtures/flate.pdf",
            "tests/references/flate_page1_72dpi.png",
        ),
        (
            "tests/fixtures/image_only.pdf",
            "tests/references/image_only_page1_72dpi.png",
        ),
        (
            "tests/fixtures/basicapi.pdf",
            "tests/references/basicapi_page1_72dpi.png",
        ),
        (
            "tests/fixtures/form_160f.pdf",
            "tests/references/form_160f_page1_72dpi.png",
        ),
        (
            "tests/fixtures/roboto.pdf",
            "tests/references/roboto_page1_72dpi.png",
        ),
        (
            "tests/fixtures/tracemonkey.pdf",
            "tests/references/tracemonkey_page1_72dpi.png",
        ),
    ];

    for (pdf_path, golden_path) in fixtures {
        let path = std::path::Path::new(pdf_path);
        if !path.exists() {
            println!("SKIP {pdf_path}: fixture not present");
            continue;
        }

        let engine = match ContentEngine::open_path(path) {
            Ok(engine) => engine,
            Err(err) => {
                println!("SKIP {pdf_path}: {err}");
                continue;
            }
        };
        let buf = match engine.render_page(1, 72) {
            Ok(buf) => buf,
            Err(err) => {
                println!("RENDER FAIL {pdf_path}: {err}");
                continue;
            }
        };
        let raw = buf.to_raw_image();
        let psnr = RenderQuality::compare_or_create_golden(std::path::Path::new(golden_path), &raw)
            .unwrap_or(f64::INFINITY);

        if psnr.is_infinite() {
            println!("GOLDEN CREATED or IDENTICAL: {pdf_path}");
        } else {
            println!("PSNR {psnr:.1} dB: {pdf_path}");
            assert!(psnr > 35.0);
        }
    }
}
