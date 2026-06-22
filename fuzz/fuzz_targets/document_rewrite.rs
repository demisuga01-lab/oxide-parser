#![no_main]
//! Fuzz full-document rewrite modes from arbitrary parsed PDFs.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_document_rewrite;

fuzz_target!(|data: &[u8]| {
    fuzz_document_rewrite(data);
});
