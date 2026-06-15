#![no_main]
//! Fuzz the content-stream tokenizer + parser on arbitrary bytes.
//!
//! `ContentParser::parse` runs the tokenizer (including the inline-image
//! state machine, a classic fuzzing hotspot) and the iterative operand/array
//! stack parser. It must produce operations for any input without panicking,
//! hanging, or recursing unboundedly.

use libfuzzer_sys::fuzz_target;
use oxide_engine::ContentParser;

fuzz_target!(|data: &[u8]| {
    let _ = std::hint::black_box(ContentParser::parse(data));
});
