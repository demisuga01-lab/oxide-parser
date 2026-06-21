#![no_main]
//! Fuzz Standard Security Handler parsing and decrypt primitives.
//!
//! The harness builds malformed encryption dictionaries from arbitrary bytes
//! and drives parser, password verification, key derivation, and stream decrypt
//! paths. Any unsupported or malformed encryption state must become a clean
//! error or false verification result, never a panic/hang/OOM.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_crypto;

fuzz_target!(|data: &[u8]| {
    fuzz_crypto(data);
});
