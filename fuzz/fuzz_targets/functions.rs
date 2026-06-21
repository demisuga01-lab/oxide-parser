#![no_main]
//! Fuzz PDF function evaluation.
//!
//! Exercises Function Types 0, 2, 3, and 4 with bounded dictionaries and
//! attacker-controlled sample/program bytes. This reaches the sampled-function
//! bit reader and the Type 4 PostScript calculator interpreter used by
//! shadings, color spaces, and transfer functions.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_functions;

fuzz_target!(|data: &[u8]| {
    fuzz_functions(data);
});
