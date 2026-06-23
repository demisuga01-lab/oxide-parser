#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    oxide_engine::fuzz::fuzz_structured_pdf(data);
});
