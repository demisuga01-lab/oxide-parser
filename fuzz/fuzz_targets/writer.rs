#![no_main]
//! Fuzz PDF writer serialization from arbitrary parsed objects.
//!
//! Parses one PDF object from arbitrary bytes, serializes it back to PDF syntax,
//! reparses the serialization, and wraps it in a tiny output PDF. This covers
//! string/name/stream escaping, xref serialization, and basic writer robustness
//! without adding the fuzz crate to the stable workspace.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_writer;

fuzz_target!(|data: &[u8]| {
    fuzz_writer(data);
});
