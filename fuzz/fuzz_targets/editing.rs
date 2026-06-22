#![no_main]
//! Fuzz page-content editing, redaction, and form flattening paths.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_editing;

fuzz_target!(|data: &[u8]| {
    fuzz_editing(data);
});
