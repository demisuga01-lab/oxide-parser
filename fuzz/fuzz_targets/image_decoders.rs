#![no_main]
//! Fuzz the image decoders (CCITT G3/G4, JBIG2, JPEG2000/JPX, DCT/JPEG).
//!
//! The first input byte selects the codec (and some decode params); the rest is
//! the encoded stream payload. Every decoder must return `Ok`/`Err` for any
//! input and never panic, hang, or allocate unboundedly from an
//! attacker-controlled size field. Some decode work happens in third-party
//! backend crates (hayro-ccitt / hayro-jbig2 / hayro-jpeg2000 / jpeg-decoder);
//! a crash there is still a finding — Oxide's wrapper must guard it.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_decode_image;

fuzz_target!(|data: &[u8]| {
    fuzz_decode_image(data);
});
