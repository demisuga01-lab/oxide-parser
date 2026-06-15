//! Fuzzing entry points (only compiled with the `fuzzing` feature).
//!
//! These thin wrappers expose internal decode/parse paths to the out-of-tree
//! `fuzz/` workspace member so they can be driven with arbitrary bytes. They
//! are NOT part of the normal public API (the whole module is gated behind
//! `#[cfg(feature = "fuzzing")]`) and add no behavior to the shipped library.
//!
//! The contract every wrapped path must satisfy: for ANY input it returns
//! (Ok/Err/None) — never panics, hangs, or allocates unboundedly.

/// Drive the image decoders with arbitrary bytes. The first input byte selects
/// the codec; the remainder is the encoded stream payload. Covers the
/// previously-unfuzzed CCITT / JBIG2 / JPEG2000 / DCT(JPEG) decode paths.
pub fn fuzz_decode_image(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let selector = data[0];
    let payload = &data[1..];

    match selector % 5 {
        0 => {
            // CCITT G4/G3. Derive small, bounded dimensions from two payload
            // bytes so the decoder loop is bounded but still exercised.
            let columns = 1 + (*payload.first().unwrap_or(&8) as u32 % 256);
            let rows = 1 + (*payload.get(1).unwrap_or(&8) as u32 % 256);
            let params = crate::images::ccitt::CcittDecodeParams {
                k: match selector / 5 % 3 {
                    0 => -1, // G4
                    1 => 0,  // G3 1D
                    _ => 1,  // G3 2D
                },
                columns,
                rows,
                black_is_1: selector & 0x20 != 0,
                encoded_byte_align: selector & 0x40 != 0,
                end_of_line: false,
                end_of_block: true,
            };
            let _ = std::hint::black_box(crate::images::ccitt::decode(payload, params));
        }
        1 => {
            // JBIG2 generic/symbol (no globals).
            let _ = std::hint::black_box(crate::images::jbig2::decode(payload, None));
        }
        2 => {
            // JPEG 2000 (JPXDecode).
            let _ = std::hint::black_box(crate::images::jpx::decode(payload));
        }
        3 => {
            // DCT / baseline JPEG.
            let _ = std::hint::black_box(
                crate::images::decoder::ImageDecoder::decode_jpeg_with_info(payload),
            );
        }
        _ => {
            // Split the payload: drive JBIG2 with a globals segment too, since
            // globals are a separate attacker-controlled input.
            let mid = payload.len() / 2;
            let (globals, rest) = payload.split_at(mid);
            let _ = std::hint::black_box(crate::images::jbig2::decode(rest, Some(globals)));
        }
    }
}

/// Drive the font-program parsers with arbitrary bytes (TrueType / CFF /
/// OpenType / bare-CFF paths via the glyph-outline extractor). Font parsing is
/// a classic crash source; this exercises `ttf-parser` + the bare-CFF fallback
/// through Oxide's wrappers, plus a few glyph lookups.
pub fn fuzz_parse_font(data: &[u8]) {
    // units-per-em probe (sfnt + bare-CFF detection).
    let _ = std::hint::black_box(crate::render::glyph_outline::get_upem(data));

    // Outline a handful of characters by Unicode and by glyph id. The first
    // byte (when present) varies the gid/char so the corpus explores different
    // glyph table entries without an unbounded loop.
    let seed = data.first().copied().unwrap_or(0);
    for ch in ['A', 'g', '中', '\u{0}'] {
        let _ = std::hint::black_box(crate::render::glyph_outline::extract_glyph_path(data, ch));
    }
    for gid_base in [0u16, 1, 0xFFFF] {
        let gid = gid_base ^ u16::from(seed);
        let _ = std::hint::black_box(crate::render::glyph_outline::extract_glyph_path_by_gid(
            data, gid,
        ));
    }
}
