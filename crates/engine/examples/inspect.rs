use oxide_engine::content::{ContentParser, GraphicsState};
use oxide_engine::{
    decode_stream, ContentEngine, OxideError, PdfDocument, PdfObject, Result, TextExtractor,
};

fn main() -> Result<()> {
    env_logger::init();
    let path = std::env::args().nth(1).ok_or_else(|| {
        OxideError::MalformedPdf("usage: cargo run --example inspect -- <pdf-path>".to_string())
    })?;
    let document = PdfDocument::open_path(&path)?;
    let engine = ContentEngine::open_path(&path)?;
    let reader = document.reader();

    println!("version: {}", reader.version());
    println!(
        "size: {}",
        reader
            .size()
            .map(|size| size.to_string())
            .unwrap_or_else(|| "missing".to_string())
    );
    println!(
        "root: {}",
        format_reference(reader.root_reference()).unwrap_or_else(|| "missing".to_string())
    );

    for number in [1u32, 2] {
        match reader.get_object(number, 0) {
            Ok(object) => println!("object {number} 0: {}", object.variant_name()),
            Err(err) => println!("object {number} 0: {err}"),
        }
    }

    let pages_ref = page_tree_root(&document)?;
    println!(
        "pages: {}",
        format_reference(Some(pages_ref)).unwrap_or_default()
    );

    let pages = document.get_pages()?;
    for page in &pages {
        println!(
            "page {}: object {} {}",
            page.page_number, page.object_number, page.generation_number
        );
        println!("  media_box: {:?}", page.media_box);
        println!("  rotate: {}", page.rotate);
        if page.contents.is_empty() {
            println!("  content streams: none");
        }
        for (number, generation) in &page.contents {
            let object = reader.get_object(*number, *generation)?;
            println!(
                "  content stream: {} {} {}",
                number,
                generation,
                object.variant_name()
            );
            if let PdfObject::Stream { raw, .. } = &object {
                let decoded = decode_stream(&object, reader)?;
                println!("    raw length: {}", raw.len());
                println!("    decoded length: {}", decoded.len());
            }
        }
        let content = document.get_page_content_bytes(page.page_number)?;
        let operations = ContentParser::parse(&content)?;
        let mut state = GraphicsState::new();
        for operation in &operations {
            state.process(operation);
        }
        println!("  parsed {} content operations", operations.len());
        println!("  text matrix: {:?}", state.text.tm);
        let text = engine.get_page_text(page.page_number)?;
        println!("  text: {:?}", text);
    }

    let text = TextExtractor::extract_default(&engine)?;
    println!(
        "extracted (first 200 chars): {:?}",
        &text[..text.len().min(200)]
    );

    Ok(())
}

fn page_tree_root(document: &PdfDocument) -> Result<(u32, u16)> {
    let reader = document.reader();
    let (root_number, root_generation) = reader
        .root_reference()
        .ok_or_else(|| OxideError::MalformedPdf("trailer is missing /Root".to_string()))?;
    let catalog = document.get_catalog()?;
    let _ = (root_number, root_generation);
    catalog
        .get_reference("Pages")
        .ok_or_else(|| OxideError::MalformedPdf("/Catalog is missing /Pages".to_string()))
}

fn format_reference(reference: Option<(u32, u16)>) -> Option<String> {
    reference.map(|(number, generation)| format!("{number} {generation} R"))
}
