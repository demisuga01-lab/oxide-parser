#![no_main]
//! Fuzz the PNG/TIFF predictor stage in isolation.
//!
//! Predictors consume attacker-controlled Columns/Colors/BitsPerComponent
//! from DecodeParms; the first three bytes here drive those, the rest is the
//! data buffer. Targets the size-field overflow / row-alignment class of bugs.

use libfuzzer_sys::fuzz_target;
use oxide_engine::filters::fuzz_apply_predictor;

fuzz_target!(|data: &[u8]| {
    let _ = std::hint::black_box(fuzz_apply_predictor(data));
});
