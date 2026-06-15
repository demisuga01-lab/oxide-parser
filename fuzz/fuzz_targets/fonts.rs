#![no_main]
//! Fuzz the font-program parsers (TrueType / CFF / OpenType / bare-CFF) via the
//! glyph-outline extractor.
//!
//! Font parsing is a classic crash source (FreeType/Poppler have a long CVE
//! history here). This drives `ttf-parser` plus Oxide's bare-CFF fallback with
//! arbitrary bytes: units-per-em probe + glyph outline extraction by char and
//! by glyph id. Any panic/hang/OOM on arbitrary "font" bytes is a finding;
//! crashes that originate in `ttf-parser` itself are reported upstream, but
//! Oxide's wrapper must degrade gracefully regardless.

use libfuzzer_sys::fuzz_target;
use oxide_engine::fuzz::fuzz_parse_font;

fuzz_target!(|data: &[u8]| {
    fuzz_parse_font(data);
});
