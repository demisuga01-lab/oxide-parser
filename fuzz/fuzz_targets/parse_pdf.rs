#![no_main]
//! Fuzz the PDF document parser end-to-end from raw bytes.
//!
//! Feeds arbitrary bytes to `ContentEngine::open_bytes` — the same in-memory
//! entry point real callers use — exercising the xref/trailer/object-stream
//! parsing, object tokenizer, and recursive object parser. Any panic, abort,
//! OOM, or hang on arbitrary input is a bug: a well-behaved parser must return
//! `Err` for every malformed input, never crash.

use libfuzzer_sys::fuzz_target;
use oxide_engine::ContentEngine;

fuzz_target!(|data: &[u8]| {
    // open_bytes takes ownership of a Vec; the result (Ok or Err) is
    // black-boxed so the call isn't optimized away. We only care that it
    // returns rather than panicking/hanging.
    let _ = std::hint::black_box(ContentEngine::open_bytes(data.to_vec()));
});
