//! Fuzzing entry points (only compiled with the `fuzzing` feature).
//!
//! These thin wrappers expose internal decode/parse paths to the out-of-tree
//! `fuzz/` workspace member so they can be driven with arbitrary bytes. They
//! are NOT part of the normal public API (the whole module is gated behind
//! `#[cfg(feature = "fuzzing")]`) and add no behavior to the shipped library.
//!
//! The contract every wrapped path must satisfy: for ANY input it returns
//! (Ok/Err/None) -- never panics, hangs, or allocates unboundedly.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::authoring::{GraphicsStyle, PageSize, PdfBuilder, StandardFont, TextStyle};
use crate::compliance::{convert_to_pdfa, validate_pdfa, PdfAProfile};
use crate::content::Color;
use crate::crypto::{
    compute_encryption_key, decrypt_stream, derive_v5_file_key_from_owner,
    derive_v5_file_key_from_user, verify_user_password, verify_v5_owner_password, verify_v5_perms,
    verify_v5_user_password, EncryptionInfo,
};
use crate::editing::{
    EditMode, EditRectStyle, EditTextStyle, ImageRect, OverlayLayer, PdfEditor, RedactionOptions,
};
use crate::engine::ContentEngine;
use crate::fonts::cmap::{parse_to_unicode_cmap, ToUnicodeCMap};
use crate::object::{PdfDictionary, PdfObject};
use crate::parser::PdfParser;
use crate::reader::PdfReader;
use crate::writer::{
    rewrite_document_with_mode, serialize_object, OutputObject, PdfWriter, WriterMode,
};

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

/// Parse attacker-controlled input and exercise the full-document writer modes.
///
/// This complements `fuzz_writer`, which serializes one parsed object. The
/// document-level path reaches xref streams, object streams, object remapping,
/// and reader-to-writer traversal. Malformed PDFs are expected to return `Err`.
pub fn fuzz_document_rewrite(data: &[u8]) {
    let Ok(engine) = ContentEngine::open_bytes(data.to_vec()) else {
        return;
    };
    for mode in [
        WriterMode::ClassicXref,
        WriterMode::XrefStream,
        WriterMode::XrefStreamWithObjStm,
    ] {
        if let Ok(bytes) = rewrite_document_with_mode(engine.document().reader(), mode, |_, _| {}) {
            let _ = std::hint::black_box(ContentEngine::open_bytes(bytes));
        }
    }
}

/// Fuzz linearization from arbitrary successfully-parsed documents.
///
/// The contract is intentionally conservative: unsupported inputs must return
/// `Err`; successful outputs must re-open without panicking.
pub fn fuzz_linearize(data: &[u8]) {
    let Ok(engine) = ContentEngine::open_bytes(data.to_vec()) else {
        return;
    };
    if let Ok(bytes) = crate::structural::linearize::linearize(&engine) {
        let _ = std::hint::black_box(ContentEngine::open_bytes(bytes));
    }
}

/// Fuzz PDF/A validation and conversion on untrusted parsed documents.
///
/// Conversion has many expected blockers (encryption, unembedded fonts, broken
/// structure). The safety property is that all cases return normally.
pub fn fuzz_pdfa(data: &[u8]) {
    let Ok(engine) = ContentEngine::open_bytes(data.to_vec()) else {
        return;
    };
    for profile in [
        PdfAProfile::PdfA1B,
        PdfAProfile::PdfA2B,
        PdfAProfile::PdfA2A,
        PdfAProfile::PdfA3B,
        PdfAProfile::PdfA3A,
    ] {
        let _ = std::hint::black_box(validate_pdfa(engine.document(), profile));
        if let Ok(bytes) = convert_to_pdfa(engine.document(), profile) {
            let _ = std::hint::black_box(ContentEngine::open_bytes(bytes));
        }
    }
}

/// Fuzz additive editing, redaction, and form handling on malformed documents.
///
/// Operations are bounded to one small page rectangle. Redactions use full
/// rewrite because incremental redaction is intentionally rejected.
pub fn fuzz_editing(data: &[u8]) {
    let Ok(mut editor) = PdfEditor::open_bytes(data.to_vec()) else {
        return;
    };
    let page_count = editor
        .document()
        .get_pages()
        .map(|pages| pages.len())
        .unwrap_or(0)
        .min(8);
    if page_count == 0 {
        return;
    }
    let page = 1 + usize::from(data.first().copied().unwrap_or(0)) % page_count;
    let x = f64::from(data.get(1).copied().unwrap_or(10) % 128);
    let y = f64::from(data.get(2).copied().unwrap_or(10) % 128);
    let w = 1.0 + f64::from(data.get(3).copied().unwrap_or(32) % 64);
    let h = 1.0 + f64::from(data.get(4).copied().unwrap_or(24) % 64);
    let rect = ImageRect::new(x, y, w, h);

    let _ = editor.draw_text(
        page,
        "fuzz",
        x,
        y,
        EditTextStyle::new(8.0),
        OverlayLayer::Overlay,
    );
    let _ = editor.draw_rect(page, rect, EditRectStyle::default(), OverlayLayer::Overlay);
    let _ = editor.redact(page, rect, RedactionOptions::default());
    editor.flatten_forms();
    if let Ok(bytes) = editor.save_to_bytes(EditMode::FullRewrite) {
        let _ = std::hint::black_box(ContentEngine::open_bytes(bytes));
    }
}

/// Fuzz signature validation and DSS/LTV material parsing from untrusted PDFs.
///
/// The CMS/X.509/OCSP/CRL/TSP payloads are attacker-controlled inside a signed
/// PDF. This target drives the validator through whatever signature-like
/// structures are present and expects structured reports/errors, never panics.
pub fn fuzz_signature_validation(data: &[u8]) {
    let Ok(engine) = ContentEngine::open_bytes(data.to_vec()) else {
        return;
    };
    let _ = std::hint::black_box(engine.verify_signatures());
}

/// Generate structurally valid PDFs with adversarial-but-bounded content and
/// drive them through deep operations.
///
/// Byte-level fuzzing mostly explores parser rejection paths. This target uses
/// the authoring API plus a raw valid-PDF generator to reach the renderer,
/// content interpreter, document model, editing, PDF/A, linearization, and
/// signature-validation code with PDFs that parse successfully.
pub fn fuzz_structured_pdf(data: &[u8]) {
    for bytes in structured_pdf_samples_for_seed(data) {
        drive_structured_pdf(&bytes, data);
    }
}

/// Materialize the grammar-aware samples for a seed input.
///
/// Exposed under the `fuzzing` feature so the structured generator can be
/// regression-tested for "valid PDF" claims without linking libFuzzer.
pub fn structured_pdf_samples_for_seed(data: &[u8]) -> Vec<Vec<u8>> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut samples = Vec::new();
    if let Some(bytes) = authored_adversarial_pdf(data) {
        samples.push(bytes);
    }
    if let Some(bytes) = raw_operator_pdf(data) {
        samples.push(bytes);
    }
    samples
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
    for ch in ['A', 'g', '\u{4E2D}', '\u{0}'] {
        let _ = std::hint::black_box(crate::render::glyph_outline::extract_glyph_path(data, ch));
    }
    for gid_base in [0u16, 1, 0xFFFF] {
        let gid = gid_base ^ u16::from(seed);
        let _ = std::hint::black_box(crate::render::glyph_outline::extract_glyph_path_by_gid(
            data, gid,
        ));
    }
}

fn drive_structured_pdf(bytes: &[u8], data: &[u8]) {
    let Ok(engine) = ContentEngine::open_bytes(bytes.to_vec()) else {
        return;
    };
    let page_count = engine.page_count().unwrap_or(0).min(4);
    for page in 1..=page_count {
        let _ = std::hint::black_box(engine.get_page_content(page));
        let _ = std::hint::black_box(engine.get_page_text(page));
        let _ = std::hint::black_box(engine.render_page_png_fast(page, 36));
    }
    if page_count > 0 {
        let pages: Vec<usize> = (1..=page_count).collect();
        let _ = std::hint::black_box(engine.build_document_model(&pages));
    }

    if let Ok(mut editor) = PdfEditor::open_bytes(bytes.to_vec()) {
        let rect = ImageRect::new(
            bounded_coord(data.get(0).copied().unwrap_or(0), 180.0),
            bounded_coord(data.get(1).copied().unwrap_or(0), 180.0),
            4.0 + f64::from(data.get(2).copied().unwrap_or(8) % 48),
            4.0 + f64::from(data.get(3).copied().unwrap_or(8) % 48),
        );
        let _ = editor.draw_rect(1, rect, EditRectStyle::default(), OverlayLayer::Overlay);
        let _ = editor.redact(1, rect, RedactionOptions::default());
        editor.flatten_forms();
        let _ = std::hint::black_box(editor.save_to_bytes(EditMode::FullRewrite));
    }

    let _ = std::hint::black_box(crate::structural::linearize::linearize(&engine));
    let _ = std::hint::black_box(engine.verify_signatures());
    for profile in [
        PdfAProfile::PdfA1B,
        PdfAProfile::PdfA2B,
        PdfAProfile::PdfA3B,
    ] {
        let _ = std::hint::black_box(validate_pdfa(engine.document(), profile));
        let _ = std::hint::black_box(convert_to_pdfa(engine.document(), profile));
    }
}

fn authored_adversarial_pdf(data: &[u8]) -> Option<Vec<u8>> {
    let mut doc = PdfBuilder::new().with_writer_mode(match data[0] % 3 {
        0 => WriterMode::ClassicXref,
        1 => WriterMode::XrefStream,
        _ => WriterMode::XrefStreamWithObjStm,
    });
    doc.set_title("structured fuzz");
    let page_count = 1 + usize::from(data.get(1).copied().unwrap_or(0) % 3);
    let text_style = TextStyle::standard(StandardFont::Helvetica, 8.0 + f64::from(data[0] % 24));
    let stroke = GraphicsStyle::stroke(Color::device_rgb(0.8, 0.1, 0.1), 0.25);
    let fill = GraphicsStyle::fill_stroke(
        Color::device_rgb(0.1, 0.35, 0.75),
        Color::device_rgb(0.0, 0.0, 0.0),
        0.5,
    );

    let mut cursor = 2usize;
    for page_index in 0..page_count {
        let width = 144.0 + f64::from(next_byte(data, &mut cursor) % 160);
        let height = 144.0 + f64::from(next_byte(data, &mut cursor) % 220);
        let page = doc.add_page(PageSize::custom(width, height));
        let lines = 1 + usize::from(next_byte(data, &mut cursor) % 5);
        for line in 0..lines {
            let text = adversarial_ascii(data, &mut cursor);
            let x = bounded_coord(next_byte(data, &mut cursor), width);
            let y = bounded_coord(next_byte(data, &mut cursor), height);
            let _ = page.draw_text(&text, x, y, &text_style);

            let x2 = bounded_coord(next_byte(data, &mut cursor), width);
            let y2 = bounded_coord(next_byte(data, &mut cursor), height);
            page.draw_line(x, y, x2, y2, &stroke);
            page.draw_rect(
                x.min(x2),
                y.min(y2),
                (x - x2).abs().max(1.0),
                (y - y2).abs().max(1.0),
                &fill,
            );
            if (line + page_index) % 2 == 0 {
                page.draw_circle(
                    x,
                    y,
                    1.0 + f64::from(next_byte(data, &mut cursor) % 24),
                    &stroke,
                );
            }
        }
    }

    doc.to_bytes().ok()
}

fn raw_operator_pdf(data: &[u8]) -> Option<Vec<u8>> {
    let mut objects = Vec::new();
    objects.push(OutputObject {
        number: 1,
        object: catalog_object(2),
    });

    let page_count = 1 + usize::from(data.get(2).copied().unwrap_or(0) % 3);
    let mut next_obj = 4u32;
    let mut page_refs = Vec::new();
    let mut cursor = 3usize;

    objects.push(OutputObject {
        number: 3,
        object: font_object(),
    });

    for _ in 0..page_count {
        let page_obj = next_obj;
        next_obj += 1;
        page_refs.push(ref_obj(page_obj));

        let stream_count = 1 + usize::from(next_byte(data, &mut cursor) % 4);
        let mut content_refs = Vec::new();
        for stream_index in 0..stream_count {
            let content_obj = next_obj;
            next_obj += 1;
            let raw = adversarial_content_stream(data, &mut cursor, stream_index);
            content_refs.push(ref_obj(content_obj));
            objects.push(OutputObject {
                number: content_obj,
                object: stream_object(raw),
            });
        }

        let annot_obj = next_obj;
        next_obj += 1;
        objects.push(OutputObject {
            number: annot_obj,
            object: text_annotation_object(data, &mut cursor),
        });

        objects.push(OutputObject {
            number: page_obj,
            object: page_object(2, &content_refs, annot_obj, data, &mut cursor),
        });
    }

    objects.insert(
        1,
        OutputObject {
            number: 2,
            object: pages_object(&page_refs),
        },
    );

    PdfWriter::new(objects, 1)
        .with_mode(match data[0] % 3 {
            0 => WriterMode::ClassicXref,
            1 => WriterMode::XrefStream,
            _ => WriterMode::XrefStreamWithObjStm,
        })
        .write()
        .ok()
}

fn catalog_object(pages: u32) -> PdfObject {
    let mut dict = PdfDictionary::empty();
    dict.insert("Type", PdfObject::Name("Catalog".to_string()));
    dict.insert("Pages", ref_obj(pages));
    PdfObject::Dictionary(dict)
}

fn pages_object(kids: &[PdfObject]) -> PdfObject {
    let mut dict = PdfDictionary::empty();
    dict.insert("Type", PdfObject::Name("Pages".to_string()));
    dict.insert("Kids", PdfObject::Array(kids.to_vec()));
    dict.insert("Count", PdfObject::Integer(kids.len() as i64));
    PdfObject::Dictionary(dict)
}

fn page_object(
    parent: u32,
    contents: &[PdfObject],
    annot: u32,
    data: &[u8],
    cursor: &mut usize,
) -> PdfObject {
    let mut font_map = PdfDictionary::empty();
    font_map.insert("F1", ref_obj(3));
    let mut resources = PdfDictionary::empty();
    resources.insert("Font", PdfObject::Dictionary(font_map));

    let width = 144.0 + f64::from(next_byte(data, cursor) % 180);
    let height = 144.0 + f64::from(next_byte(data, cursor) % 220);
    let mut dict = PdfDictionary::empty();
    dict.insert("Type", PdfObject::Name("Page".to_string()));
    dict.insert("Parent", ref_obj(parent));
    dict.insert(
        "MediaBox",
        PdfObject::Array(vec![
            PdfObject::Real(-12.0),
            PdfObject::Real(-12.0),
            PdfObject::Real(width),
            PdfObject::Real(height),
        ]),
    );
    dict.insert("Resources", PdfObject::Dictionary(resources));
    dict.insert("Contents", PdfObject::Array(contents.to_vec()));
    dict.insert("Annots", PdfObject::Array(vec![ref_obj(annot)]));
    PdfObject::Dictionary(dict)
}

fn font_object() -> PdfObject {
    let mut dict = PdfDictionary::empty();
    dict.insert("Type", PdfObject::Name("Font".to_string()));
    dict.insert("Subtype", PdfObject::Name("Type1".to_string()));
    dict.insert("BaseFont", PdfObject::Name("Helvetica".to_string()));
    dict.insert("Encoding", PdfObject::Name("WinAnsiEncoding".to_string()));
    PdfObject::Dictionary(dict)
}

fn text_annotation_object(data: &[u8], cursor: &mut usize) -> PdfObject {
    let x = bounded_coord(next_byte(data, cursor), 240.0);
    let y = bounded_coord(next_byte(data, cursor), 240.0);
    let mut dict = PdfDictionary::empty();
    dict.insert("Type", PdfObject::Name("Annot".to_string()));
    dict.insert("Subtype", PdfObject::Name("Text".to_string()));
    dict.insert(
        "Rect",
        PdfObject::Array(vec![
            PdfObject::Real(x),
            PdfObject::Real(y),
            PdfObject::Real(x + 12.0),
            PdfObject::Real(y + 12.0),
        ]),
    );
    dict.insert("Contents", PdfObject::String(b"structured fuzz".to_vec()));
    PdfObject::Dictionary(dict)
}

fn stream_object(raw: Vec<u8>) -> PdfObject {
    let mut dict = PdfDictionary::empty();
    dict.insert("Length", PdfObject::Integer(raw.len() as i64));
    PdfObject::Stream { dict, raw }
}

fn adversarial_content_stream(data: &[u8], cursor: &mut usize, stream_index: usize) -> Vec<u8> {
    let mut out = String::new();
    let mut q_depth = 0usize;
    let ops = 8 + usize::from(next_byte(data, cursor) % 56);
    for index in 0..ops {
        match next_byte(data, cursor) % 10 {
            0 if q_depth < 12 => {
                out.push_str("q\n");
                q_depth += 1;
            }
            1 if q_depth > 0 => {
                out.push_str("Q\n");
                q_depth -= 1;
            }
            2 => {
                let a = matrix_value(next_byte(data, cursor));
                let d = matrix_value(next_byte(data, cursor));
                let e = coord_value(next_byte(data, cursor));
                let f = coord_value(next_byte(data, cursor));
                out.push_str(&format!("{a:.4} 0 0 {d:.4} {e:.2} {f:.2} cm\n"));
            }
            3 => {
                let x = coord_value(next_byte(data, cursor));
                let y = coord_value(next_byte(data, cursor));
                let w = 1.0 + f64::from(next_byte(data, cursor) % 96);
                let h = 1.0 + f64::from(next_byte(data, cursor) % 96);
                out.push_str(&format!("{x:.2} {y:.2} {w:.2} {h:.2} re S\n"));
            }
            4 => {
                let x = coord_value(next_byte(data, cursor));
                let y = coord_value(next_byte(data, cursor));
                let x2 = coord_value(next_byte(data, cursor));
                let y2 = coord_value(next_byte(data, cursor));
                out.push_str(&format!("{x:.2} {y:.2} m {x2:.2} {y2:.2} l S\n"));
            }
            5 => {
                let text = adversarial_ascii(data, cursor);
                let x = coord_value(next_byte(data, cursor));
                let y = coord_value(next_byte(data, cursor));
                let size = 1.0 + f64::from(next_byte(data, cursor) % 72);
                out.push_str(&format!(
                    "BT /F1 {size:.2} Tf {x:.2} {y:.2} Td ({}) Tj ET\n",
                    escape_pdf_literal(&text)
                ));
            }
            6 => {
                let r = color_value(next_byte(data, cursor));
                let g = color_value(next_byte(data, cursor));
                let b = color_value(next_byte(data, cursor));
                out.push_str(&format!(
                    "{r:.4} {g:.4} {b:.4} rg {r:.4} {g:.4} {b:.4} RG\n"
                ));
            }
            7 => {
                let w = f64::from(next_byte(data, cursor) % 24) / 4.0;
                let dash = 1 + usize::from(next_byte(data, cursor) % 16);
                out.push_str(&format!("{w:.2} w [{dash}] 0 d\n"));
            }
            8 => {
                let x = coord_value(next_byte(data, cursor));
                let y = coord_value(next_byte(data, cursor));
                let x1 = coord_value(next_byte(data, cursor));
                let y1 = coord_value(next_byte(data, cursor));
                let x2 = coord_value(next_byte(data, cursor));
                let y2 = coord_value(next_byte(data, cursor));
                out.push_str(&format!(
                    "{x:.2} {y:.2} m {x1:.2} {y1:.2} {x2:.2} {y2:.2} {x:.2} {y:.2} c f\n"
                ));
            }
            _ => {
                let x = coord_value(next_byte(data, cursor));
                let y = coord_value(next_byte(data, cursor));
                let w = 1.0 + f64::from(next_byte(data, cursor) % 80);
                let h = 1.0 + f64::from(next_byte(data, cursor) % 80);
                out.push_str(&format!("{x:.2} {y:.2} {w:.2} {h:.2} re W n\n"));
            }
        }
        if index % 13 == 0 {
            out.push_str("% structured-fuzz\n");
        }
    }
    while q_depth > 0 {
        out.push_str("Q\n");
        q_depth -= 1;
    }
    if stream_index % 2 == 0 {
        out.push_str("BT /F1 9 Tf 12 12 Td (structured fuzz) Tj ET\n");
    }
    out.into_bytes()
}

fn ref_obj(number: u32) -> PdfObject {
    PdfObject::Reference {
        number,
        generation: 0,
    }
}

fn next_byte(data: &[u8], cursor: &mut usize) -> u8 {
    if data.is_empty() {
        return 0;
    }
    let byte = data[*cursor % data.len()];
    *cursor = (*cursor).wrapping_add(1);
    byte
}

fn adversarial_ascii(data: &[u8], cursor: &mut usize) -> String {
    let len = 1 + usize::from(next_byte(data, cursor) % 24);
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        let byte = next_byte(data, cursor);
        let ch = match byte % 8 {
            0 => ' ',
            1 => '-',
            2 => '.',
            3 => '(',
            4 => ')',
            _ => char::from(0x41 + (byte % 26)),
        };
        out.push(ch);
    }
    out
}

fn escape_pdf_literal(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn bounded_coord(byte: u8, max: f64) -> f64 {
    f64::from(byte) / 255.0 * max
}

fn coord_value(byte: u8) -> f64 {
    f64::from(byte) - 128.0
}

fn matrix_value(byte: u8) -> f64 {
    match byte % 8 {
        0 => 0.001,
        1 => 0.01,
        2 => 0.1,
        3 => 1.0,
        4 => 10.0,
        5 => -1.0,
        6 => -0.1,
        _ => 2.0,
    }
}

fn color_value(byte: u8) -> f64 {
    f64::from(byte) / 255.0
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
