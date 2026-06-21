use std::io::Read;

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::error::{OxideError, Result};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

/// Absolute backstop on how large a single FlateDecode stream may expand to.
///
/// This guards against decompression bombs: a tiny compressed stream that
/// inflates to gigabytes and OOMs the process. 512 MiB comfortably exceeds any
/// legitimate single PDF stream (the largest real streams are uncompressed
/// images, themselves bounded by page/image limits) while stopping absurd
/// expansion ratios. The server layers tighter, configurable per-request caps
/// on top of this; this is the engine's own hard floor so any caller — CLI,
/// tests, embedders — is protected even without the server.
pub const MAX_FLATE_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamDecodeStatus {
    Complete,
    StoppedAtImageFilter(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStream {
    pub data: Vec<u8>,
    pub status: StreamDecodeStatus,
}

/// Fully decodes a stream through all implemented filters.
///
/// Image-only filters (`DCTDecode`, `JPXDecode`, `CCITTFaxDecode`, and
/// `JBIG2Decode`) are intentionally not decoded in this layer. Use
/// [`decode_stream_lossless`] when callers want bytes decoded through preceding
/// lossless filters and an explicit status naming the remaining image filter.
pub fn decode_stream(stream: &PdfObject, reader: &PdfReader) -> Result<Vec<u8>> {
    let decoded = decode_stream_lossless(stream, reader)?;
    match decoded.status {
        StreamDecodeStatus::Complete => Ok(decoded.data),
        StreamDecodeStatus::StoppedAtImageFilter(filter) => Err(OxideError::UnsupportedFeature(
            format!("image filter remains: {filter}"),
        )),
    }
}

/// Decodes implemented lossless filters in order and stops before an image
/// codec filter, returning the current bytes plus a status.
pub fn decode_stream_lossless(stream: &PdfObject, reader: &PdfReader) -> Result<DecodedStream> {
    let (dict, raw) = stream.as_stream().ok_or_else(|| {
        OxideError::MalformedPdf("decode_stream requires a stream object".to_string())
    })?;
    decode_stream_parts(dict, raw, Some(reader))
}

pub(crate) fn decode_stream_from_dict(dict: &PdfDictionary, raw: &[u8]) -> Result<Vec<u8>> {
    let decoded = decode_stream_parts(dict, raw, None)?;
    match decoded.status {
        StreamDecodeStatus::Complete => Ok(decoded.data),
        StreamDecodeStatus::StoppedAtImageFilter(filter) => Err(OxideError::UnsupportedFeature(
            format!("image filter remains: {filter}"),
        )),
    }
}

pub(crate) fn apply_filter_bytes(
    filter_name: &str,
    input: &[u8],
    decode_parms: Option<&PdfDictionary>,
) -> Result<Vec<u8>> {
    match filter_name {
        "FlateDecode" | "Fl" => {
            let data = flate_decode(input)?;
            apply_predictor(data, decode_parms)
        }
        "LZWDecode" | "LZW" => {
            let early_change = int_param(decode_parms, "EarlyChange", 1)?;
            if !(0..=1).contains(&early_change) {
                return Err(OxideError::MalformedPdf(format!(
                    "invalid LZW EarlyChange value {early_change}"
                )));
            }
            let data = lzw_decode(input, early_change as u8)?;
            apply_predictor(data, decode_parms)
        }
        "ASCIIHexDecode" | "AHx" => ascii_hex_decode(input),
        "ASCII85Decode" | "A85" => ascii85_decode(input),
        "RunLengthDecode" | "RL" => run_length_decode(input),
        other => Err(OxideError::UnsupportedFeature(format!(
            "unsupported inline image filter {other}"
        ))),
    }
}

/// Fuzz-only entry point: drive a single stream decoder by a leading selector
/// byte, with the remaining input fed to that decoder as raw filter bytes.
///
/// Exposed only under the `fuzzing` feature so libFuzzer (which can reach
/// `pub` items only) can exercise the otherwise-private decoders directly,
/// without constructing a `PdfReader`. Not part of the normal public API.
#[cfg(feature = "fuzzing")]
pub fn fuzz_decode_filter(input: &[u8]) -> Result<Vec<u8>> {
    let Some((selector, rest)) = input.split_first() else {
        return Ok(Vec::new());
    };
    let filter = match selector % 6 {
        0 => "FlateDecode",
        1 => "LZWDecode",
        2 => "ASCIIHexDecode",
        3 => "ASCII85Decode",
        4 => "RunLengthDecode",
        // Exercise the predictor path on top of Flate with a small fixed
        // DecodeParms so the predictor code is reachable from the fuzzer.
        _ => "FlateDecode",
    };
    apply_filter_bytes(filter, rest, None)
}

/// Fuzz-only entry point for the PNG/TIFF predictor stage in isolation: the
/// first three bytes select Predictor, Colors, and Columns, the rest is the
/// data buffer.
#[cfg(feature = "fuzzing")]
pub fn fuzz_apply_predictor(input: &[u8]) -> Result<Vec<u8>> {
    let predictor = i64::from(input.first().copied().unwrap_or(0));
    let colors = i64::from(input.get(1).copied().unwrap_or(1)).max(1);
    let columns = i64::from(input.get(2).copied().unwrap_or(1)).max(1);
    let body = input.get(3..).unwrap_or(&[]).to_vec();

    let mut params = PdfDictionary::empty();
    params.insert("Predictor", PdfObject::Integer(predictor));
    params.insert("Colors", PdfObject::Integer(colors));
    params.insert("Columns", PdfObject::Integer(columns));
    params.insert("BitsPerComponent", PdfObject::Integer(8));

    apply_predictor(body, Some(&params))
}

fn decode_stream_parts(
    dict: &PdfDictionary,
    raw: &[u8],
    reader: Option<&PdfReader>,
) -> Result<DecodedStream> {
    let filters = filter_names(dict, reader)?;
    let params = decode_params(dict, reader, filters.len())?;
    let mut data = raw.to_vec();

    for (idx, filter) in filters.iter().enumerate() {
        let param = params.get(idx).and_then(Option::as_ref);
        match filter.as_str() {
            "FlateDecode" | "Fl" => {
                data = flate_decode(&data)?;
                data = apply_predictor(data, param)?;
            }
            "LZWDecode" | "LZW" => {
                let early_change = int_param(param, "EarlyChange", 1)?;
                if !(0..=1).contains(&early_change) {
                    return Err(OxideError::MalformedPdf(format!(
                        "invalid LZW EarlyChange value {early_change}"
                    )));
                }
                data = lzw_decode(&data, early_change as u8)?;
                data = apply_predictor(data, param)?;
            }
            "ASCIIHexDecode" | "AHx" => data = ascii_hex_decode(&data)?,
            "ASCII85Decode" | "A85" => data = ascii85_decode(&data)?,
            "RunLengthDecode" | "RL" => data = run_length_decode(&data)?,
            // The reader applies PDF crypt filters while fetching stream
            // objects, because decryption needs object/generation numbers and
            // the document encryption dictionary. At the decode-filter layer,
            // `/Crypt` is therefore just the marker for that already-applied
            // step. If there is no active encryption context, only the
            // explicit `/Identity` crypt filter is a no-op; any other crypt
            // filter means the caller needs to reopen with the right password.
            "Crypt" => {
                if reader.and_then(PdfReader::encryption).is_none()
                    && !crypt_filter_is_identity(param)
                {
                    return Err(OxideError::EncryptedPdf(
                        "stream uses /Crypt filter; provide the correct password".to_string(),
                    ));
                }
            }
            "DCTDecode" | "DCT" | "JPXDecode" | "CCITTFaxDecode" | "CCF" | "JBIG2Decode" => {
                return Ok(DecodedStream {
                    data,
                    status: StreamDecodeStatus::StoppedAtImageFilter(filter.clone()),
                });
            }
            other => {
                return Err(OxideError::UnsupportedFeature(format!(
                    "unsupported stream filter {other}"
                )));
            }
        }
    }

    Ok(DecodedStream {
        data,
        status: StreamDecodeStatus::Complete,
    })
}

fn resolved_object(obj: &PdfObject, reader: Option<&PdfReader>) -> Result<PdfObject> {
    match reader {
        Some(reader) => reader.resolve(obj.clone()),
        None => Ok(obj.clone()),
    }
}

fn crypt_filter_is_identity(param: Option<&PdfDictionary>) -> bool {
    matches!(
        param.and_then(|dict| dict.get_name("Name")),
        Some("Identity")
    )
}

fn filter_names(dict: &PdfDictionary, reader: Option<&PdfReader>) -> Result<Vec<String>> {
    let Some(filter_obj) = dict.get("Filter").or_else(|| dict.get("F")) else {
        return Ok(Vec::new());
    };
    let filter_obj = resolved_object(filter_obj, reader)?;
    match filter_obj {
        PdfObject::Name(name) => Ok(vec![name]),
        PdfObject::Array(items) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match resolved_object(&item, reader)? {
                    PdfObject::Name(name) => names.push(name),
                    other => {
                        return Err(OxideError::MalformedPdf(format!(
                            "filter array contains {}",
                            other.variant_name()
                        )));
                    }
                }
            }
            Ok(names)
        }
        PdfObject::Null => Ok(Vec::new()),
        other => Err(OxideError::MalformedPdf(format!(
            "Filter must be a name or array, got {}",
            other.variant_name()
        ))),
    }
}

fn decode_params(
    dict: &PdfDictionary,
    reader: Option<&PdfReader>,
    filter_count: usize,
) -> Result<Vec<Option<PdfDictionary>>> {
    let Some(params_obj) = dict.get("DecodeParms").or_else(|| dict.get("DP")) else {
        return Ok(vec![None; filter_count]);
    };
    let params_obj = resolved_object(params_obj, reader)?;
    match params_obj {
        PdfObject::Null => Ok(vec![None; filter_count]),
        PdfObject::Dictionary(params) => {
            let mut out = vec![None; filter_count];
            if !out.is_empty() {
                out[0] = Some(params);
            }
            Ok(out)
        }
        PdfObject::Array(items) => {
            let mut out = Vec::with_capacity(filter_count);
            for item in items.into_iter().take(filter_count) {
                match resolved_object(&item, reader)? {
                    PdfObject::Null => out.push(None),
                    PdfObject::Dictionary(params) => out.push(Some(params)),
                    other => {
                        return Err(OxideError::MalformedPdf(format!(
                            "DecodeParms array contains {}",
                            other.variant_name()
                        )));
                    }
                }
            }
            while out.len() < filter_count {
                out.push(None);
            }
            Ok(out)
        }
        other => Err(OxideError::MalformedPdf(format!(
            "DecodeParms must be a dictionary or array, got {}",
            other.variant_name()
        ))),
    }
}

fn flate_decode(data: &[u8]) -> Result<Vec<u8>> {
    flate_decode_capped(data, MAX_FLATE_DECOMPRESSED_BYTES)
}

/// FlateDecode with an explicit decompressed-size cap (parameterized so tests
/// can exercise the bomb guard without allocating the full production cap).
fn flate_decode_capped(data: &[u8], cap: u64) -> Result<Vec<u8>> {
    // Cap reads at one byte over the limit so we can distinguish "exactly at the
    // limit" (fine) from "exceeded it" (bomb). `take` makes the decoder stop
    // reading past the cap instead of inflating unbounded into memory.
    let read_cap = cap + 1;
    let mut out = Vec::new();
    let mut zlib = ZlibDecoder::new(data).take(read_cap);
    match zlib.read_to_end(&mut out) {
        Ok(_) => check_decompressed_size(out, cap),
        Err(zlib_error) => {
            let mut raw_out = Vec::new();
            let mut deflate = DeflateDecoder::new(data).take(read_cap);
            match deflate.read_to_end(&mut raw_out) {
                Ok(_) => check_decompressed_size(raw_out, cap),
                Err(_) => Err(OxideError::ParseError(format!(
                    "FlateDecode failed: {zlib_error}"
                ))),
            }
        }
    }
}

/// Reject output that hit the decompression-bomb backstop.
fn check_decompressed_size(out: Vec<u8>, cap: u64) -> Result<Vec<u8>> {
    if out.len() as u64 > cap {
        return Err(OxideError::MalformedPdf(format!(
            "FlateDecode output exceeds {} byte limit (possible decompression bomb)",
            cap
        )));
    }
    Ok(out)
}

pub(crate) fn apply_predictor(data: Vec<u8>, params: Option<&PdfDictionary>) -> Result<Vec<u8>> {
    let predictor = int_param(params, "Predictor", 1)?;
    if predictor == 1 {
        return Ok(data);
    }

    let columns = positive_usize_param(params, "Columns", 1)?;
    let colors = positive_usize_param(params, "Colors", 1)?;
    let bits_per_component = positive_usize_param(params, "BitsPerComponent", 8)?;
    // Columns/Colors/BitsPerComponent are attacker-controlled. Computing the
    // row length by plain multiplication can overflow `usize` on crafted input
    // (e.g. a huge /Columns), which panics under overflow checks and silently
    // wraps to a bogus length otherwise. Use checked arithmetic and reject
    // overflow as malformed rather than crashing or misparsing.
    let row_bits = columns
        .checked_mul(colors)
        .and_then(|v| v.checked_mul(bits_per_component))
        .ok_or_else(|| {
            OxideError::MalformedPdf(
                "predictor row dimensions overflow (Columns × Colors × BitsPerComponent)"
                    .to_string(),
            )
        })?;
    let row_len = ceil_div(row_bits, 8);
    if row_len == 0 {
        return Ok(data);
    }

    match predictor {
        2 => tiff_predictor(data, row_len, colors, bits_per_component),
        10..=15 => png_predictor(data, row_len, colors, bits_per_component),
        other => Err(OxideError::UnsupportedFeature(format!(
            "unsupported predictor {other}"
        ))),
    }
}

fn int_param(params: Option<&PdfDictionary>, key: &str, default: i64) -> Result<i64> {
    match params.and_then(|dict| dict.get(key)) {
        Some(PdfObject::Integer(value)) => Ok(*value),
        Some(other) => Err(OxideError::MalformedPdf(format!(
            "DecodeParms /{key} must be an integer, got {}",
            other.variant_name()
        ))),
        None => Ok(default),
    }
}

fn positive_usize_param(
    params: Option<&PdfDictionary>,
    key: &str,
    default: usize,
) -> Result<usize> {
    let value = int_param(params, key, default as i64)?;
    if value <= 0 {
        return Err(OxideError::MalformedPdf(format!(
            "DecodeParms /{key} must be positive"
        )));
    }
    usize::try_from(value).map_err(|_| {
        OxideError::MalformedPdf(format!("DecodeParms /{key} is too large for this platform"))
    })
}

fn tiff_predictor(
    mut data: Vec<u8>,
    row_len: usize,
    colors: usize,
    bits_per_component: usize,
) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(row_len) {
        return Err(OxideError::MalformedPdf(
            "TIFF predictor data length is not row-aligned".to_string(),
        ));
    }

    match bits_per_component {
        8 => {
            for row in data.chunks_mut(row_len) {
                for idx in colors..row.len() {
                    row[idx] = row[idx].wrapping_add(row[idx - colors]);
                }
            }
        }
        16 => {
            let stride = colors * 2;
            if !row_len.is_multiple_of(2) {
                return Err(OxideError::MalformedPdf(
                    "16-bit TIFF predictor row has odd byte length".to_string(),
                ));
            }
            for row in data.chunks_mut(row_len) {
                let mut idx = stride;
                while idx + 1 < row.len() {
                    let current = u16::from_be_bytes([row[idx], row[idx + 1]]);
                    let prior = u16::from_be_bytes([row[idx - stride], row[idx + 1 - stride]]);
                    let decoded = current.wrapping_add(prior).to_be_bytes();
                    row[idx] = decoded[0];
                    row[idx + 1] = decoded[1];
                    idx += 2;
                }
            }
        }
        other => {
            return Err(OxideError::UnsupportedFeature(format!(
                "TIFF predictor with {other} bits per component"
            )));
        }
    }

    Ok(data)
}

fn png_predictor(
    data: Vec<u8>,
    row_len: usize,
    colors: usize,
    bits_per_component: usize,
) -> Result<Vec<u8>> {
    let row_with_filter = row_len + 1;
    if !data.len().is_multiple_of(row_with_filter) {
        return Err(OxideError::MalformedPdf(
            "PNG predictor data length is not row-aligned".to_string(),
        ));
    }

    let bytes_per_pixel = ceil_div(colors * bits_per_component, 8).max(1);
    let mut out = Vec::with_capacity((data.len() / row_with_filter) * row_len);
    let mut prev_row = vec![0u8; row_len];

    for encoded_row in data.chunks(row_with_filter) {
        let filter = encoded_row[0];
        let encoded = &encoded_row[1..];
        let mut row = encoded.to_vec();
        match filter {
            0 => {}
            1 => {
                for idx in 0..row_len {
                    let left = idx
                        .checked_sub(bytes_per_pixel)
                        .and_then(|left_idx| row.get(left_idx).copied())
                        .unwrap_or(0);
                    row[idx] = row[idx].wrapping_add(left);
                }
            }
            2 => {
                for idx in 0..row_len {
                    row[idx] = row[idx].wrapping_add(prev_row[idx]);
                }
            }
            3 => {
                for idx in 0..row_len {
                    let left = idx
                        .checked_sub(bytes_per_pixel)
                        .and_then(|left_idx| row.get(left_idx).copied())
                        .unwrap_or(0);
                    let up = prev_row[idx];
                    row[idx] = row[idx].wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8);
                }
            }
            4 => {
                for idx in 0..row_len {
                    let left = idx
                        .checked_sub(bytes_per_pixel)
                        .and_then(|left_idx| row.get(left_idx).copied())
                        .unwrap_or(0);
                    let up = prev_row[idx];
                    let up_left = idx
                        .checked_sub(bytes_per_pixel)
                        .and_then(|left_idx| prev_row.get(left_idx).copied())
                        .unwrap_or(0);
                    row[idx] = row[idx].wrapping_add(paeth(left, up, up_left));
                }
            }
            other => {
                return Err(OxideError::MalformedPdf(format!(
                    "invalid PNG predictor row filter {other}"
                )));
            }
        }
        out.extend_from_slice(&row);
        prev_row = row;
    }

    Ok(out)
}

fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let left_i = i32::from(left);
    let up_i = i32::from(up);
    let up_left_i = i32::from(up_left);
    let estimate = left_i + up_i - up_left_i;
    let pa = (estimate - left_i).abs();
    let pb = (estimate - up_i).abs();
    let pc = (estimate - up_left_i).abs();
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        up
    } else {
        up_left
    }
}

fn ceil_div(value: usize, divisor: usize) -> usize {
    if value == 0 {
        0
    } else {
        1 + ((value - 1) / divisor)
    }
}

fn ascii_hex_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;

    for &byte in data {
        if byte == b'>' {
            break;
        }
        if is_pdf_whitespace(byte) {
            continue;
        }
        let value = hex_value(byte).ok_or_else(|| {
            OxideError::ParseError(format!("invalid ASCIIHex digit 0x{byte:02X}"))
        })?;
        match high.take() {
            Some(high_nibble) => out.push((high_nibble << 4) | value),
            None => high = Some(value),
        }
    }

    if let Some(high_nibble) = high {
        out.push(high_nibble << 4);
    }

    Ok(out)
}

fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group: Vec<u8> = Vec::with_capacity(5);
    let mut idx = 0;

    while idx < data.len() {
        let byte = data[idx];
        idx += 1;

        if is_pdf_whitespace(byte) {
            continue;
        }

        if byte == b'~' {
            let mut saw_end = false;
            while idx < data.len() {
                let next = data[idx];
                idx += 1;
                if is_pdf_whitespace(next) {
                    continue;
                }
                if next == b'>' {
                    saw_end = true;
                    break;
                }
                return Err(OxideError::ParseError(
                    "ASCII85 '~' must be followed by '>'".to_string(),
                ));
            }
            if !saw_end {
                return Err(OxideError::ParseError(
                    "unterminated ASCII85 EOD marker".to_string(),
                ));
            }
            break;
        }

        if byte == b'z' {
            if !group.is_empty() {
                return Err(OxideError::ParseError(
                    "ASCII85 'z' cannot appear inside a group".to_string(),
                ));
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }

        if !(b'!'..=b'u').contains(&byte) {
            return Err(OxideError::ParseError(format!(
                "invalid ASCII85 byte 0x{byte:02X}"
            )));
        }

        group.push(byte - b'!');
        if group.len() == 5 {
            push_ascii85_group(&group, 4, &mut out)?;
            group.clear();
        }
    }

    if !group.is_empty() {
        if group.len() == 1 {
            return Err(OxideError::ParseError(
                "ASCII85 final group cannot contain one digit".to_string(),
            ));
        }
        let output_len = group.len() - 1;
        while group.len() < 5 {
            group.push(84);
        }
        push_ascii85_group(&group, output_len, &mut out)?;
    }

    Ok(out)
}

fn push_ascii85_group(group: &[u8], output_len: usize, out: &mut Vec<u8>) -> Result<()> {
    if group.len() != 5 || output_len > 4 {
        return Err(OxideError::ParseError(
            "invalid ASCII85 group length".to_string(),
        ));
    }
    let mut value = 0u32;
    for &digit in group {
        value = value
            .checked_mul(85)
            .and_then(|v| v.checked_add(u32::from(digit)))
            .ok_or_else(|| OxideError::ParseError("ASCII85 group overflows".to_string()))?;
    }
    let bytes = value.to_be_bytes();
    out.extend_from_slice(&bytes[..output_len]);
    Ok(())
}

fn run_length_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut idx = 0;

    while idx < data.len() {
        let len = data[idx];
        idx += 1;
        match len {
            0..=127 => {
                let count = usize::from(len) + 1;
                let end = idx.checked_add(count).ok_or_else(|| {
                    OxideError::ParseError("RunLength literal count overflow".to_string())
                })?;
                if end > data.len() {
                    return Err(OxideError::ParseError(
                        "truncated RunLength literal run".to_string(),
                    ));
                }
                out.extend_from_slice(&data[idx..end]);
                idx = end;
            }
            128 => break,
            129..=255 => {
                if idx >= data.len() {
                    return Err(OxideError::ParseError(
                        "truncated RunLength repeat run".to_string(),
                    ));
                }
                let count = usize::from(257u16 - u16::from(len));
                out.extend(std::iter::repeat_n(data[idx], count));
                idx += 1;
            }
        }
    }

    Ok(out)
}

fn lzw_decode(data: &[u8], early_change: u8) -> Result<Vec<u8>> {
    let mut reader = MsbBitReader::new(data);
    let mut table = initial_lzw_table();
    let mut code_width = 9usize;
    let mut next_code = 258usize;
    let mut out = Vec::new();
    let mut previous: Option<Vec<u8>> = None;

    while let Some(code) = reader.read_bits(code_width) {
        match code {
            256 => {
                table = initial_lzw_table();
                code_width = 9;
                next_code = 258;
                previous = None;
            }
            257 => break,
            code => {
                let code_usize = usize::from(code);
                let entry = if code_usize < table.len() {
                    table[code_usize].clone().ok_or_else(|| {
                        OxideError::ParseError(format!("invalid empty LZW code {code_usize}"))
                    })?
                } else if code_usize == next_code {
                    let prev = previous.as_ref().ok_or_else(|| {
                        OxideError::ParseError("LZW KwKwK code without previous entry".to_string())
                    })?;
                    let mut synthesized = prev.clone();
                    let first = *prev.first().ok_or_else(|| {
                        OxideError::ParseError("empty LZW previous entry".to_string())
                    })?;
                    synthesized.push(first);
                    synthesized
                } else {
                    return Err(OxideError::ParseError(format!(
                        "invalid LZW code {code_usize}"
                    )));
                };

                out.extend_from_slice(&entry);

                if let Some(prev) = previous.as_ref() {
                    if next_code < 4096 {
                        let mut new_entry = prev.clone();
                        let first = *entry
                            .first()
                            .ok_or_else(|| OxideError::ParseError("empty LZW entry".to_string()))?;
                        new_entry.push(first);
                        if table.len() <= next_code {
                            table.resize(next_code + 1, None);
                        }
                        table[next_code] = Some(new_entry);
                        next_code += 1;
                        if code_width < 12
                            && next_code + usize::from(early_change) >= (1usize << code_width)
                        {
                            code_width += 1;
                        }
                    }
                }

                previous = Some(entry);
            }
        }
    }

    Ok(out)
}

fn initial_lzw_table() -> Vec<Option<Vec<u8>>> {
    let mut table = Vec::with_capacity(4096);
    for byte in 0u16..=255 {
        table.push(Some(vec![byte as u8]));
    }
    table.push(None);
    table.push(None);
    table
}

struct MsbBitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> MsbBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bits(&mut self, count: usize) -> Option<u16> {
        if count == 0 || self.bit_pos + count > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u16;
        for _ in 0..count {
            let byte = self.data[self.bit_pos / 8];
            let bit_offset = 7 - (self.bit_pos % 8);
            let bit = (byte >> bit_offset) & 1;
            value = (value << 1) | u16::from(bit);
            self.bit_pos += 1;
        }
        Some(value)
    }
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn ascii_hex_decodes_odd_nibble() {
        assert_eq!(ascii_hex_decode(b"61 62 3>").unwrap(), b"ab0");
    }

    #[test]
    fn ascii85_decodes_known_vector() {
        assert_eq!(
            ascii85_decode(b"87cURD]i,\"Ebo7~>").unwrap(),
            b"Hello World"
        );
        assert_eq!(ascii85_decode(b"z~>").unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn run_length_decodes_packbits() {
        let encoded = [2, b'a', b'b', b'c', 253, b'x', 128];
        assert_eq!(run_length_decode(&encoded).unwrap(), b"abcxxxx");
    }

    #[test]
    fn lzw_decodes_literal_codes() {
        let encoded = pack_lzw_codes(&[65, 66, 67, 257], 9);
        assert_eq!(lzw_decode(&encoded, 1).unwrap(), b"ABC");
    }

    #[test]
    fn flate_decode_accepts_normal_stream() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write as _;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"hello flate").unwrap();
        let compressed = enc.finish().unwrap();
        assert_eq!(flate_decode(&compressed).unwrap(), b"hello flate");
    }

    #[test]
    fn flate_decode_rejects_decompression_bomb() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write as _;
        // A 4 MiB run of zeros compresses to a tiny input but inflates well past
        // a small test cap, simulating the bomb without allocating the 512 MiB
        // production limit.
        let raw = vec![0u8; 4 * 1024 * 1024];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&raw).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(
            compressed.len() < 64 * 1024,
            "bomb input should be tiny, was {}",
            compressed.len()
        );
        // Cap of 1 MiB < 4 MiB decompressed => rejected.
        let err = flate_decode_capped(&compressed, 1024 * 1024).unwrap_err();
        assert!(
            matches!(err, OxideError::MalformedPdf(ref m) if m.contains("decompression bomb")),
            "expected decompression-bomb rejection, got {:?}",
            err
        );
        // The same data under a generous cap decodes fine.
        let ok = flate_decode_capped(&compressed, 8 * 1024 * 1024).unwrap();
        assert_eq!(ok.len(), raw.len());
    }

    #[test]
    fn png_predictor_up_decodes_rows() {
        let params = dict(&[
            ("Predictor", PdfObject::Integer(12)),
            ("Columns", PdfObject::Integer(3)),
        ]);
        let encoded = vec![0, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(
            apply_predictor(encoded, Some(&params)).unwrap(),
            vec![1, 2, 3, 2, 3, 4]
        );
    }

    #[test]
    fn predictor_row_dimensions_overflow_returns_err_not_panic() {
        // Attacker-controlled Columns/Colors/BitsPerComponent whose product
        // overflows usize must yield a clean MalformedPdf error rather than
        // panicking under overflow checks (or wrapping to a bogus row length).
        // Regression for the unchecked `columns * colors * bits_per_component`
        // multiplication (fuzz finding: predictor size-field overflow).
        let huge = i64::MAX;
        let params = dict(&[
            ("Predictor", PdfObject::Integer(12)),
            ("Columns", PdfObject::Integer(huge)),
            ("Colors", PdfObject::Integer(huge)),
            ("BitsPerComponent", PdfObject::Integer(16)),
        ]);
        let err = apply_predictor(vec![0u8; 32], Some(&params)).unwrap_err();
        assert!(
            matches!(err, OxideError::MalformedPdf(_)),
            "expected MalformedPdf on dimension overflow, got {err:?}"
        );
    }

    fn pack_lzw_codes(codes: &[u16], width: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut current = 0u8;
        let mut used = 0usize;
        for &code in codes {
            for bit_idx in (0..width).rev() {
                let bit = ((code >> bit_idx) & 1) as u8;
                current = (current << 1) | bit;
                used += 1;
                if used == 8 {
                    out.push(current);
                    current = 0;
                    used = 0;
                }
            }
        }
        if used > 0 {
            current <<= 8 - used;
            out.push(current);
        }
        out
    }
}
