//! Fuzzing entry points (only compiled with the `fuzzing` feature).
//!
//! These thin wrappers expose internal decode/parse paths to the out-of-tree
//! `fuzz/` workspace member so they can be driven with arbitrary bytes. They
//! are NOT part of the normal public API (the whole module is gated behind
//! `#[cfg(feature = "fuzzing")]`) and add no behavior to the shipped library.
//!
//! The contract every wrapped path must satisfy: for ANY input it returns
//! (Ok/Err/None) — never panics, hangs, or allocates unboundedly.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::crypto::{
    compute_encryption_key, decrypt_stream, derive_v5_file_key_from_owner,
    derive_v5_file_key_from_user, verify_user_password, verify_v5_owner_password, verify_v5_perms,
    verify_v5_user_password, EncryptionInfo,
};
use crate::fonts::cmap::{parse_to_unicode_cmap, ToUnicodeCMap};
use crate::object::{PdfDictionary, PdfObject};
use crate::parser::PdfParser;
use crate::reader::PdfReader;
use crate::writer::{serialize_object, OutputObject, PdfWriter};

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

/// Drive both ToUnicode CMap parsers with arbitrary bytes. CMaps are
/// PostScript-like programs embedded in fonts and are attacker-controlled in
/// malformed PDFs.
pub fn fuzz_parse_cmap(data: &[u8]) {
    let cmap = ToUnicodeCMap::parse(data);
    let _ = std::hint::black_box(cmap.code_size());
    let _ = std::hint::black_box(cmap.is_empty());
    for code in [0u16, 1, 0x20, 0x41, 0x100, 0xFFFF] {
        let _ = std::hint::black_box(cmap.lookup(code));
    }

    let parsed = parse_to_unicode_cmap(data);
    let _ = std::hint::black_box(parsed.get(&0));
    let _ = std::hint::black_box(parsed.len());
}

/// Drive encryption-dictionary parsing and decrypt/password primitives with
/// attacker-controlled dictionary fields and ciphertext. The constructed
/// dictionary mirrors the Standard Security Handler shape while keeping all
/// buffers bounded by the fuzz input length.
pub fn fuzz_crypto(data: &[u8]) {
    if data.is_empty() {
        return;
    }

    let mut dict = PdfDictionary::empty();
    dict.insert("Filter", PdfObject::Name("Standard".to_string()));
    dict.insert("V", PdfObject::Integer(i64::from(1 + (data[0] % 5))));
    dict.insert(
        "R",
        PdfObject::Integer(i64::from(2 + (data.get(1).copied().unwrap_or(0) % 5))),
    );
    dict.insert(
        "Length",
        PdfObject::Integer(match data.get(2).copied().unwrap_or(0) % 4 {
            0 => 40,
            1 => 64,
            2 => 128,
            _ => 256,
        }),
    );
    dict.insert(
        "P",
        PdfObject::Integer(i64::from(data.get(3).copied().unwrap_or(0)) - 128),
    );
    dict.insert(
        "EncryptMetadata",
        PdfObject::Boolean(data.get(4).copied().unwrap_or(0) & 1 == 0),
    );

    let payload = data.get(5..).unwrap_or_default();
    let third = payload.len() / 3;
    let two_thirds = third.saturating_mul(2);
    let o = payload.get(..third).unwrap_or_default().to_vec();
    let u = payload.get(third..two_thirds).unwrap_or_default().to_vec();
    let rest = payload.get(two_thirds..).unwrap_or_default();
    dict.insert("O", PdfObject::String(o));
    dict.insert("U", PdfObject::String(u));
    dict.insert("OE", PdfObject::String(take_padded(rest, 0, 32)));
    dict.insert("UE", PdfObject::String(take_padded(rest, 32, 32)));
    dict.insert("Perms", PdfObject::String(take_padded(rest, 64, 16)));

    let crypt_method = match data[0] % 4 {
        0 => "Identity",
        1 => "V2",
        2 => "AESV2",
        _ => "AESV3",
    };
    let mut filter = PdfDictionary::empty();
    filter.insert("CFM", PdfObject::Name(crypt_method.to_string()));
    let mut cf = BTreeMap::new();
    cf.insert("StdCF".to_string(), PdfObject::Dictionary(filter));
    dict.insert("CF", PdfObject::Dictionary(PdfDictionary::new(cf)));
    dict.insert("StmF", PdfObject::Name("StdCF".to_string()));
    dict.insert("StrF", PdfObject::Name("StdCF".to_string()));
    dict.insert("EFF", PdfObject::Name("StdCF".to_string()));

    let Ok(info) = EncryptionInfo::from_dict(&dict) else {
        return;
    };

    let password = data.get(..data.len().min(32)).unwrap_or_default();
    let file_id = data
        .get(data.len().saturating_sub(32)..)
        .unwrap_or_default();
    if info.v == 5 {
        let _ = std::hint::black_box(verify_v5_user_password(password, &info));
        let _ = std::hint::black_box(verify_v5_owner_password(password, &info));
        if let Ok(file_key) = derive_v5_file_key_from_user(password, &info) {
            let _ = std::hint::black_box(verify_v5_perms(&file_key, &info));
            let _ = std::hint::black_box(decrypt_stream(rest, &file_key, 1, 0, false, true));
        }
        if let Ok(file_key) = derive_v5_file_key_from_owner(password, &info) {
            let _ = std::hint::black_box(verify_v5_perms(&file_key, &info));
            let _ = std::hint::black_box(decrypt_stream(rest, &file_key, 2, 0, false, true));
        }
    } else {
        let _ = std::hint::black_box(verify_user_password(password, &info, file_id));
        let key = compute_encryption_key(password, &info, file_id);
        let _ = std::hint::black_box(decrypt_stream(rest, &key, 1, 0, info.is_aes(), false));
    }
}

/// Drive PDF Function Types 0, 2, 3 and 4 with bounded, attacker-controlled
/// stream/program bytes. This reaches the PostScript calculator interpreter and
/// sampled-function bit reader used by shadings and color spaces.
pub fn fuzz_functions(data: &[u8]) {
    let selector = data.first().copied().unwrap_or(0);
    let payload = data.get(1..).unwrap_or_default();
    let reader = minimal_reader();

    let function = match selector % 4 {
        0 => sampled_function(payload, selector),
        1 => exponential_function(selector),
        2 => stitching_function(selector),
        _ => postscript_function(payload),
    };
    let inputs = [
        f64::from(data.get(2).copied().unwrap_or(0)) / 255.0,
        f64::from(data.get(3).copied().unwrap_or(0)) / 255.0,
    ];
    let _ = std::hint::black_box(crate::render::function::eval_function_n(
        &function, &inputs, reader,
    ));
}

/// Drive writer serialization with arbitrary parsed objects, then wrap the
/// object in a tiny PDF and parse it again. This exercises name/string/stream
/// escaping and xref serialization without requiring a malicious object graph
/// to be manually constructed.
pub fn fuzz_writer(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let Ok(mut parser) = PdfParser::new(data, 0) else {
        return;
    };
    let Ok(object) = parser.parse_object() else {
        return;
    };

    let mut serialized = Vec::new();
    serialize_object(&object, &mut serialized);
    let _ = std::hint::black_box(PdfParser::new(&serialized, 0).and_then(|mut p| p.parse_object()));

    let objects = vec![OutputObject { number: 1, object }];
    if let Ok(pdf) = PdfWriter::new(objects, 1).write() {
        let _ = std::hint::black_box(PdfReader::from_bytes(pdf));
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

fn take_padded(data: &[u8], offset: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    if let Some(slice) = data.get(offset..offset.saturating_add(len)) {
        let copy = slice.len().min(len);
        out[..copy].copy_from_slice(&slice[..copy]);
    }
    out
}

fn minimal_reader() -> &'static PdfReader {
    static READER: OnceLock<PdfReader> = OnceLock::new();
    READER.get_or_init(|| {
        let mut catalog = PdfDictionary::empty();
        catalog.insert("Type", PdfObject::Name("Catalog".to_string()));
        let bytes = PdfWriter::new(
            vec![OutputObject {
                number: 1,
                object: PdfObject::Dictionary(catalog),
            }],
            1,
        )
        .write()
        .expect("embedded minimal PDF must serialize");
        PdfReader::from_bytes(bytes).expect("embedded minimal PDF must parse")
    })
}

fn sampled_function(payload: &[u8], selector: u8) -> PdfObject {
    let dimensions = 1 + usize::from(selector & 1);
    let outputs = 1 + usize::from((selector >> 1) % 3);
    let sample_size = 1 + usize::from((selector >> 3) % 4);
    let bps = match (selector >> 5) % 4 {
        0 => 1,
        1 => 4,
        2 => 8,
        _ => 16,
    };

    let mut dict = PdfDictionary::empty();
    dict.insert("FunctionType", PdfObject::Integer(0));
    dict.insert(
        "Domain",
        PdfObject::Array(
            (0..dimensions)
                .flat_map(|_| [PdfObject::Real(0.0), PdfObject::Real(1.0)])
                .collect(),
        ),
    );
    dict.insert(
        "Range",
        PdfObject::Array(
            (0..outputs)
                .flat_map(|_| [PdfObject::Real(0.0), PdfObject::Real(1.0)])
                .collect(),
        ),
    );
    dict.insert(
        "Size",
        PdfObject::Array(
            (0..dimensions)
                .map(|_| PdfObject::Integer(sample_size as i64))
                .collect(),
        ),
    );
    dict.insert("BitsPerSample", PdfObject::Integer(bps));
    PdfObject::Stream {
        dict,
        raw: payload.to_vec(),
    }
}

fn exponential_function(selector: u8) -> PdfObject {
    let mut dict = PdfDictionary::empty();
    dict.insert("FunctionType", PdfObject::Integer(2));
    dict.insert(
        "Domain",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    dict.insert(
        "Range",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    dict.insert("N", PdfObject::Real(f64::from(selector % 8)));
    dict.insert(
        "C0",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(0.2)]),
    );
    dict.insert(
        "C1",
        PdfObject::Array(vec![PdfObject::Real(1.0), PdfObject::Real(0.8)]),
    );
    PdfObject::Dictionary(dict)
}

fn stitching_function(selector: u8) -> PdfObject {
    let mut sub_a = PdfDictionary::empty();
    sub_a.insert("FunctionType", PdfObject::Integer(2));
    sub_a.insert(
        "Domain",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    sub_a.insert(
        "Range",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    sub_a.insert("N", PdfObject::Real(1.0));

    let mut sub_b = sub_a.clone();
    sub_b.insert("N", PdfObject::Real(f64::from(1 + (selector % 3))));

    let mut dict = PdfDictionary::empty();
    dict.insert("FunctionType", PdfObject::Integer(3));
    dict.insert(
        "Domain",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    dict.insert(
        "Range",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    dict.insert(
        "Functions",
        PdfObject::Array(vec![
            PdfObject::Dictionary(sub_a),
            PdfObject::Dictionary(sub_b),
        ]),
    );
    dict.insert("Bounds", PdfObject::Array(vec![PdfObject::Real(0.5)]));
    dict.insert(
        "Encode",
        PdfObject::Array(vec![
            PdfObject::Real(0.0),
            PdfObject::Real(1.0),
            PdfObject::Real(0.0),
            PdfObject::Real(1.0),
        ]),
    );
    PdfObject::Dictionary(dict)
}

fn postscript_function(payload: &[u8]) -> PdfObject {
    let mut dict = PdfDictionary::empty();
    dict.insert("FunctionType", PdfObject::Integer(4));
    dict.insert(
        "Domain",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    dict.insert(
        "Range",
        PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
    );
    PdfObject::Stream {
        dict,
        raw: payload.to_vec(),
    }
}
