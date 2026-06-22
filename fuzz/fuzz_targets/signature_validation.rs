#![no_main]
//! Fuzz signature validation over attacker-controlled signed-PDF structures.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_signature_validation;

fuzz_target!(|data: &[u8]| {
    fuzz_signature_validation(data);
});
