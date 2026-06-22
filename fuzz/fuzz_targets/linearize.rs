#![no_main]
//! Fuzz Fast Web View linearization from arbitrary parsed PDFs.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_linearize;

fuzz_target!(|data: &[u8]| {
    fuzz_linearize(data);
});
