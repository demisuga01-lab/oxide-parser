//! Parallelism correctness tests.
//!
//! These guard the invariant that parallelising text extraction (Part B) and
//! sharing one parsed engine across render threads via `Arc` (Part C) changes
//! only HOW the work is scheduled, never the OUTPUT. Any divergence here is a
//! bug, not an acceptable perf trade-off.

use std::sync::Arc;
use std::thread;

use oxide_engine::{
    ContentEngine, TextExtractOptions, TextExtractor, TextFormatter,
};

const TRACEMONKEY: &str = "tests/fixtures/tracemonkey.pdf";
const MULTI_PAGE_120: &str = "../../tests/corpus/pdfs/generated/generated_120_pages.pdf";

fn open(path: &str) -> Option<ContentEngine> {
    if !std::path::Path::new(path).exists() {
        eprintln!("SKIP: fixture not present: {path}");
        return None;
    }
    Some(ContentEngine::open_path(path).expect("fixture should open"))
}

/// Reference serial extraction: formats each page in order on a single thread,
/// independent of `TextExtractor::extract`'s internal scheduling. The parallel
/// path must reproduce this byte-for-byte.
fn serial_reference(engine: &ContentEngine, opts: &TextExtractOptions) -> String {
    let total = engine.page_count().unwrap();
    let pages: Vec<usize> = match &opts.pages {
        Some(list) => list.clone(),
        None => (1..=total).collect(),
    };
    let extractor = TextExtractor::new();
    let formatter = TextFormatter::new();
    let mut out = String::new();
    for p in pages {
        if p == 0 || p > total {
            continue;
        }
        if let Ok((n, lines)) = extractor.extract_page(engine, p, opts) {
            out.push_str(&formatter.format_page(&lines, n, &opts.format));
        }
    }
    out
}

#[test]
fn parallel_extract_matches_serial_reference_tracemonkey() {
    let Some(engine) = open(TRACEMONKEY) else { return };
    // tracemonkey is 14 pages — comfortably above the parallel threshold.
    assert!(
        engine.page_count().unwrap() >= 4,
        "fixture must exceed parallel threshold to exercise the parallel path"
    );
    let opts = TextExtractOptions::default();
    let parallel = TextExtractor::new().extract(&engine, &opts).unwrap();
    let serial = serial_reference(&engine, &opts);
    assert_eq!(
        parallel, serial,
        "parallel extraction output must be byte-identical to serial"
    );
}

#[test]
fn parallel_extract_matches_serial_reference_120_pages() {
    let Some(engine) = open(MULTI_PAGE_120) else { return };
    let opts = TextExtractOptions::default();
    let parallel = TextExtractor::new().extract(&engine, &opts).unwrap();
    let serial = serial_reference(&engine, &opts);
    assert_eq!(parallel, serial, "120-page parallel output must match serial");
}

#[test]
fn parallel_extract_is_deterministic_across_repeated_runs() {
    let Some(engine) = open(TRACEMONKEY) else { return };
    let opts = TextExtractOptions::default();
    let first = TextExtractor::new().extract(&engine, &opts).unwrap();
    // Run many times: thread completion order varies, output must not.
    for i in 0..25 {
        let again = TextExtractor::new().extract(&engine, &opts).unwrap();
        assert_eq!(first, again, "extraction run {i} diverged — ordering bug");
    }
}

#[test]
fn page_order_is_preserved_with_shuffled_explicit_page_list() {
    let Some(engine) = open(TRACEMONKEY) else { return };
    let total = engine.page_count().unwrap();
    if total < 6 {
        return;
    }
    // An explicit, non-sorted page list must come out in the REQUESTED order,
    // not sorted and not in completion order.
    let requested = vec![5, 1, 4, 2, 6, 3];
    let mut opts = TextExtractOptions {
        pages: Some(requested.clone()),
        ..Default::default()
    };
    opts.format.include_page_markers = true;

    let parallel = TextExtractor::new().extract(&engine, &opts).unwrap();
    let serial = serial_reference(&engine, &opts);
    assert_eq!(parallel, serial, "explicit page order must be preserved");

    // The page markers must appear in the requested order.
    let positions: Vec<Option<usize>> = requested
        .iter()
        .map(|p| parallel.find(&format!("--- Page {p} ---")))
        .collect();
    let mut last = None;
    for (p, pos) in requested.iter().zip(&positions) {
        let pos = pos.unwrap_or_else(|| panic!("page marker {p} missing"));
        if let Some(prev) = last {
            assert!(pos > prev, "page {p} marker out of order");
        }
        last = Some(pos);
    }
}

#[test]
fn arc_engine_renders_identically_across_threads() {
    let Some(engine) = open(TRACEMONKEY) else { return };
    let engine = Arc::new(engine);
    let total = engine.page_count().unwrap();
    let pages: Vec<usize> = (1..=total.min(6)).collect();

    // Reference: render each page serially through the shared Arc.
    let reference: Vec<(usize, Vec<u8>)> = pages
        .iter()
        .map(|&p| {
            let buf = engine.render_page(p, 72).unwrap();
            (p, buf.to_raw_image().pixels)
        })
        .collect();

    // Now hammer the same Arc from several threads concurrently and confirm
    // every page renders to identical pixels (no shared-state races, no
    // deadlock on the object-stream RwLock).
    let mut handles = Vec::new();
    for _ in 0..4 {
        let engine = Arc::clone(&engine);
        let pages = pages.clone();
        handles.push(thread::spawn(move || {
            pages
                .iter()
                .map(|&p| {
                    let buf = engine.render_page(p, 72).unwrap();
                    (p, buf.to_raw_image().pixels)
                })
                .collect::<Vec<_>>()
        }));
    }
    for h in handles {
        let got = h.join().expect("render thread panicked");
        assert_eq!(got.len(), reference.len());
        for ((gp, gpix), (rp, rpix)) in got.iter().zip(&reference) {
            assert_eq!(gp, rp, "page index mismatch");
            assert_eq!(gpix, rpix, "page {gp} pixels diverged under concurrency");
        }
    }
}

#[test]
fn concurrent_text_and_render_on_shared_engine_do_not_race() {
    // Mix text extraction and rendering against ONE shared engine from
    // multiple threads — both paths read the object-stream cache, so this
    // exercises concurrent readers + the first-write of the RwLock cache.
    let Some(engine) = open(TRACEMONKEY) else { return };
    let engine = Arc::new(engine);
    let total = engine.page_count().unwrap().min(8);

    let text_ref = engine.get_page_text(1).unwrap();

    let mut handles = Vec::new();
    for t in 0..6 {
        let engine = Arc::clone(&engine);
        handles.push(thread::spawn(move || {
            if t % 2 == 0 {
                for p in 1..=total {
                    let _ = engine.get_page_text(p);
                }
                engine.get_page_text(1).unwrap()
            } else {
                for p in 1..=total {
                    let _ = engine.render_page(p, 72);
                }
                engine.get_page_text(1).unwrap()
            }
        }));
    }
    for h in handles {
        let got = h.join().expect("worker thread panicked");
        assert_eq!(got, text_ref, "page-1 text diverged under concurrent load");
    }
}
