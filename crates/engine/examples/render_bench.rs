//! Render-mode benchmark helper.
//!
//! Renders every page of a PDF at a given DPI in one of two strategies so the
//! perf harness can measure the memory/throughput delta of the Arc-shared
//! engine versus the old per-page re-open behaviour:
//!
//!   shared  — parse the PDF ONCE into `Arc<ContentEngine>`, render all pages
//!             in parallel sharing that single parsed document (the fixed
//!             pdf2img design).
//!   perpage — re-open (re-parse + re-buffer) a fresh `ContentEngine` from the
//!             PDF bytes for every page, in parallel (the OLD pdf2img design
//!             this round removed). Holds O(num_pages) copies of the parsed
//!             document at peak.
//!
//! Usage:
//!   cargo run --release --example render_bench -- <pdf> <dpi> <shared|perpage>
//!
//! Output (pixels) is identical between modes; only the parse/memory strategy
//! differs. RAYON_NUM_THREADS controls parallelism.

use std::sync::Arc;

use oxide_engine::{ContentEngine, ImageEncoder, Result};
use rayon::prelude::*;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: render_bench <pdf> <dpi> <shared|perpage>");
        std::process::exit(2);
    });
    let dpi: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(150);
    let mode = args.get(3).map(String::as_str).unwrap_or("shared");

    let bytes = std::fs::read(&path)?;
    let probe = ContentEngine::open_bytes(bytes.clone())?;
    let page_count = probe.page_count()?;
    let pages: Vec<usize> = (1..=page_count).collect();

    // Sum encoded byte lengths so the optimiser cannot elide the render work,
    // and so we can assert both modes produce identical output sizes.
    let total: usize = match mode {
        "perpage" => pages
            .par_iter()
            .map(|&p| {
                // OLD behaviour: re-parse the whole PDF for every page.
                let engine = match ContentEngine::open_bytes(bytes.clone()) {
                    Ok(e) => e,
                    Err(_) => return 0,
                };
                render_one(&engine, p, dpi)
            })
            .sum(),
        _ => {
            // FIXED behaviour: parse once, share via Arc across threads.
            let engine = Arc::new(probe);
            pages
                .par_iter()
                .map(|&p| render_one(&engine, p, dpi))
                .sum()
        }
    };

    println!(
        "mode={mode} pages={page_count} dpi={dpi} total_encoded_bytes={total}"
    );
    Ok(())
}

fn render_one(engine: &ContentEngine, page: usize, dpi: u32) -> usize {
    let buf = match engine.render_page(page, dpi) {
        Ok(b) => b,
        Err(err) => {
            eprintln!("page {page}: render failed: {err}");
            return 0;
        }
    };
    match ImageEncoder::encode_png_fast(&buf.to_raw_image()) {
        Ok(bytes) => bytes.len(),
        Err(err) => {
            eprintln!("page {page}: encode failed: {err}");
            0
        }
    }
}
