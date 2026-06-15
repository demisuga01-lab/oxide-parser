#![no_main]
//! Fuzz the stream filters (FlateDecode, LZWDecode, ASCIIHexDecode,
//! ASCII85Decode, RunLengthDecode) on arbitrary bytes.
//!
//! The first input byte selects which decoder runs; the rest is fed to it as
//! raw filter bytes. Every decoder must return `Ok`/`Err` for any input and
//! never panic, hang, or allocate unboundedly from an attacker-controlled
//! size field.

use libfuzzer_sys::fuzz_target;
use oxide_engine::filters::fuzz_decode_filter;

fuzz_target!(|data: &[u8]| {
    let _ = std::hint::black_box(fuzz_decode_filter(data));
});
