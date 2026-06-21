#![no_main]
//! Fuzz ToUnicode CMap parsing on arbitrary PostScript-like CMap bytes.
//!
//! CMaps drive code/CID-to-Unicode mapping and are embedded attacker-controlled
//! programs in fonts. The parser must return a map or an empty map, never panic
//! or loop forever on malformed ranges, arrays, comments, or hex strings.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_parse_cmap;

fuzz_target!(|data: &[u8]| {
    fuzz_parse_cmap(data);
});
