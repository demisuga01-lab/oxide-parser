#![no_main]
//! Fuzz PDF/A validation and conversion from arbitrary parsed PDFs.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_pdfa;

fuzz_target!(|data: &[u8]| {
    fuzz_pdfa(data);
});
