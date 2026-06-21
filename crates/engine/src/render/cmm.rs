//! Small, deterministic color-management helpers for PDF render output.
//!
//! Embedded ICC profiles are handled with `qcms` when available. Device spaces
//! still need local fallbacks because PDF DeviceCMYK/Cal/Lab often appear
//! without an ICC profile.

use crate::filters::{decode_stream_lossless, StreamDecodeStatus};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

const D50: [f32; 3] = [0.96422, 1.0, 0.82521];

#[derive(Debug, Clone, Copy)]
pub(crate) struct LabParams {
    pub white_point: [f32; 3],
    pub range: [f32; 4],
}

impl Default for LabParams {
    fn default() -> Self {
        Self {
            white_point: D50,
            range: [-100.0, 100.0, -100.0, 100.0],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CalGrayParams {
    pub white_point: [f32; 3],
    pub gamma: f32,
}

impl Default for CalGrayParams {
    fn default() -> Self {
        Self {
            white_point: D50,
            gamma: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CalRgbParams {
    pub white_point: [f32; 3],
    pub gamma: [f32; 3],
    /// PDF matrix order: [XA YA ZA XB YB ZB XC YC ZC].
    pub matrix: [f32; 9],
}

impl Default for CalRgbParams {
    fn default() -> Self {
        Self {
            white_point: D50,
            gamma: [1.0, 1.0, 1.0],
            matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// Convert unprofiled DeviceCMYK through a Poppler/Splash-like deterministic
/// process-color fallback.
///
/// DeviceCMYK is device dependent. Without an OutputIntent or ICCBased stream
/// there is no single correct transform, but Poppler's Windows build renders
/// default process inks close to these sRGB anchors:
/// C=(0,173,239), M=(236,0,140), Y=(255,242,0), R=(237,28,36),
/// G=(0,166,80), B=(46,49,146), CMY=(54,54,57), K=(35,31,32).
/// This fallback uses trilinear interpolation over that CMY process-ink cube,
/// then blends toward the measured process black. It is much closer to Poppler
/// than the old `(1-c)*(1-k)` algebraic conversion while remaining deterministic
/// and profile-license-free.
pub(crate) fn device_cmyk_to_srgb(c: f32, m: f32, y: f32, k: f32) -> [f32; 3] {
    let c = c.clamp(0.0, 1.0);
    let m = m.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    let k = k.clamp(0.0, 1.0);

    const W: [f32; 3] = [255.0, 255.0, 255.0];
    const C: [f32; 3] = [0.0, 173.0, 239.0];
    const M: [f32; 3] = [236.0, 0.0, 140.0];
    const Y: [f32; 3] = [255.0, 242.0, 0.0];
    const CM: [f32; 3] = [46.0, 49.0, 146.0];
    const CY: [f32; 3] = [0.0, 166.0, 80.0];
    const MY: [f32; 3] = [237.0, 28.0, 36.0];
    const CMY: [f32; 3] = [54.0, 54.0, 57.0];
    const BLACK: [f32; 3] = [35.0, 31.0, 32.0];

    let mut out = [0.0; 3];
    for i in 0..3 {
        let c00 = lerp(W[i], C[i], c);
        let c10 = lerp(M[i], CM[i], c);
        let c01 = lerp(Y[i], CY[i], c);
        let c11 = lerp(MY[i], CMY[i], c);
        let c0 = lerp(c00, c10, m);
        let c1 = lerp(c01, c11, m);
        out[i] = lerp(lerp(c0, c1, y), BLACK[i], k).clamp(0.0, 255.0) / 255.0;
    }
    out
}

pub(crate) fn device_cmyk_bytes_to_rgb(pixels: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(pixels.len() / 4 * 3);
    for chunk in pixels.chunks_exact(4) {
        let out = device_cmyk_to_srgb(
            chunk[0] as f32 / 255.0,
            chunk[1] as f32 / 255.0,
            chunk[2] as f32 / 255.0,
            chunk[3] as f32 / 255.0,
        );
        rgb.extend(out.map(unit_to_u8));
    }
    rgb
}

pub(crate) fn lab_to_srgb(l: f32, a: f32, b: f32, params: LabParams) -> [f32; 3] {
    let l = l.clamp(0.0, 100.0);
    let a = a.clamp(params.range[0], params.range[1]);
    let b = b.clamp(params.range[2], params.range[3]);
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let xyz = [
        params.white_point[0] * lab_f_inv(fx),
        params.white_point[1] * lab_f_inv(fy),
        params.white_point[2] * lab_f_inv(fz),
    ];
    xyz_d50_to_srgb(adapt_xyz_to_d50(xyz, params.white_point))
}

pub(crate) fn lab_bytes_to_rgb(pixels: &[u8], params: LabParams) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(pixels.len());
    for chunk in pixels.chunks_exact(3) {
        let l = decode_range(chunk[0], 0.0, 100.0);
        let a = decode_range(chunk[1], params.range[0], params.range[1]);
        let b = decode_range(chunk[2], params.range[2], params.range[3]);
        rgb.extend(lab_to_srgb(l, a, b, params).map(unit_to_u8));
    }
    rgb
}

pub(crate) fn cal_gray_to_srgb(gray: f32, params: CalGrayParams) -> [f32; 3] {
    let g = gray.clamp(0.0, 1.0).powf(params.gamma.max(0.01));
    xyz_d50_to_srgb(adapt_xyz_to_d50(
        [
            params.white_point[0] * g,
            params.white_point[1] * g,
            params.white_point[2] * g,
        ],
        params.white_point,
    ))
}

pub(crate) fn cal_gray_bytes_to_rgb(pixels: &[u8], params: CalGrayParams) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for &gray in pixels {
        rgb.extend(cal_gray_to_srgb(gray as f32 / 255.0, params).map(unit_to_u8));
    }
    rgb
}

pub(crate) fn cal_rgb_to_srgb(components: [f32; 3], params: CalRgbParams) -> [f32; 3] {
    let a = components[0]
        .clamp(0.0, 1.0)
        .powf(params.gamma[0].max(0.01));
    let b = components[1]
        .clamp(0.0, 1.0)
        .powf(params.gamma[1].max(0.01));
    let c = components[2]
        .clamp(0.0, 1.0)
        .powf(params.gamma[2].max(0.01));
    let m = params.matrix;
    let xyz = [
        m[0] * a + m[3] * b + m[6] * c,
        m[1] * a + m[4] * b + m[7] * c,
        m[2] * a + m[5] * b + m[8] * c,
    ];
    xyz_d50_to_srgb(adapt_xyz_to_d50(xyz, params.white_point))
}

pub(crate) fn cal_rgb_bytes_to_rgb(pixels: &[u8], params: CalRgbParams) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(pixels.len());
    for chunk in pixels.chunks_exact(3) {
        let comps = [
            chunk[0] as f32 / 255.0,
            chunk[1] as f32 / 255.0,
            chunk[2] as f32 / 255.0,
        ];
        rgb.extend(cal_rgb_to_srgb(comps, params).map(unit_to_u8));
    }
    rgb
}

pub(crate) fn icc_bytes_to_rgb(
    pixels: &[u8],
    dict: &PdfDictionary,
    reader: &PdfReader,
) -> Option<(Vec<u8>, u8)> {
    let (profile_dict, profile_bytes) = icc_profile_stream(dict, reader)?;
    let n = profile_dict.get_integer("N").unwrap_or(3).clamp(1, 4) as u8;
    let src_type = match n {
        1 => qcms::DataType::Gray8,
        3 => qcms::DataType::RGB8,
        4 => qcms::DataType::CMYK,
        _ => return None,
    };
    let input = qcms::Profile::new_from_slice(&profile_bytes, false)?;
    let mut output = qcms::Profile::new_sRGB();
    output.precache_output_transform();
    let transform = qcms::Transform::new_to(
        &input,
        &output,
        src_type,
        qcms::DataType::RGB8,
        qcms::Intent::default(),
    )?;
    let pixel_count = pixels.len() / usize::from(n);
    let mut rgb = vec![0u8; pixel_count * 3];
    transform.convert(&pixels[..pixel_count * usize::from(n)], &mut rgb);
    Some((rgb, 3))
}

pub(crate) fn icc_components_to_srgb(
    space_obj: &PdfObject,
    components: &[f64],
    reader: &PdfReader,
) -> Option<[f32; 3]> {
    let (profile_dict, profile_bytes) = icc_profile_stream_from_space(space_obj, reader)?;
    let n = profile_dict.get_integer("N").unwrap_or(3).clamp(1, 4) as u8;
    let src_type = match n {
        1 => qcms::DataType::Gray8,
        3 => qcms::DataType::RGB8,
        4 => qcms::DataType::CMYK,
        _ => return None,
    };
    let mut src = vec![0u8; usize::from(n)];
    for (i, byte) in src.iter_mut().enumerate() {
        *byte = unit_to_u8(components.get(i).copied().unwrap_or(0.0) as f32);
    }
    let input = qcms::Profile::new_from_slice(&profile_bytes, false)?;
    let mut output = qcms::Profile::new_sRGB();
    output.precache_output_transform();
    let transform = qcms::Transform::new_to(
        &input,
        &output,
        src_type,
        qcms::DataType::RGB8,
        qcms::Intent::default(),
    )?;
    let mut dst = [0u8; 3];
    transform.convert(&src, &mut dst);
    Some([
        dst[0] as f32 / 255.0,
        dst[1] as f32 / 255.0,
        dst[2] as f32 / 255.0,
    ])
}

pub(crate) fn icc_channel_count(dict: &PdfDictionary, reader: &PdfReader) -> Option<u8> {
    let (profile_dict, _) = icc_profile_object(dict, reader)?;
    profile_dict.get_integer("N").map(|n| n.clamp(1, 4) as u8)
}

pub(crate) fn lab_params_from_image_dict(
    dict: &PdfDictionary,
    reader: Option<&PdfReader>,
) -> LabParams {
    dict.get("ColorSpace")
        .and_then(|obj| lab_params_from_space(obj, reader))
        .unwrap_or_default()
}

pub(crate) fn cal_gray_params_from_image_dict(
    dict: &PdfDictionary,
    reader: Option<&PdfReader>,
) -> CalGrayParams {
    dict.get("ColorSpace")
        .and_then(|obj| cal_gray_params_from_space(obj, reader))
        .unwrap_or_default()
}

pub(crate) fn cal_rgb_params_from_image_dict(
    dict: &PdfDictionary,
    reader: Option<&PdfReader>,
) -> CalRgbParams {
    dict.get("ColorSpace")
        .and_then(|obj| cal_rgb_params_from_space(obj, reader))
        .unwrap_or_default()
}

fn icc_profile_stream(
    dict: &PdfDictionary,
    reader: &PdfReader,
) -> Option<(PdfDictionary, Vec<u8>)> {
    let (profile_dict, stream_obj) = icc_profile_object(dict, reader)?;
    let decoded = decode_stream_lossless(&stream_obj, reader).ok()?;
    match decoded.status {
        StreamDecodeStatus::Complete => Some((profile_dict, decoded.data)),
        StreamDecodeStatus::StoppedAtImageFilter(_) => None,
    }
}

fn icc_profile_stream_from_space(
    space_obj: &PdfObject,
    reader: &PdfReader,
) -> Option<(PdfDictionary, Vec<u8>)> {
    let (profile_dict, stream_obj) = icc_profile_object_from_space(space_obj, reader)?;
    let decoded = decode_stream_lossless(&stream_obj, reader).ok()?;
    match decoded.status {
        StreamDecodeStatus::Complete => Some((profile_dict, decoded.data)),
        StreamDecodeStatus::StoppedAtImageFilter(_) => None,
    }
}

fn icc_profile_object(
    dict: &PdfDictionary,
    reader: &PdfReader,
) -> Option<(PdfDictionary, PdfObject)> {
    let arr = dict.get("ColorSpace")?.as_array()?;
    if arr.first().and_then(PdfObject::as_name) != Some("ICCBased") {
        return None;
    }
    let obj = reader.resolve(arr.get(1)?.clone()).ok()?;
    match obj {
        PdfObject::Stream { dict, raw } => {
            let profile_dict = dict.clone();
            Some((profile_dict, PdfObject::Stream { dict, raw }))
        }
        _ => None,
    }
}

fn icc_profile_object_from_space(
    space_obj: &PdfObject,
    reader: &PdfReader,
) -> Option<(PdfDictionary, PdfObject)> {
    let resolved = match space_obj {
        PdfObject::Reference { .. } => reader.resolve(space_obj.clone()).ok()?,
        other => other.clone(),
    };
    let arr = resolved.as_array()?;
    if arr.first().and_then(PdfObject::as_name) != Some("ICCBased") {
        return None;
    }
    let obj = reader.resolve(arr.get(1)?.clone()).ok()?;
    match obj {
        PdfObject::Stream { dict, raw } => {
            let profile_dict = dict.clone();
            Some((profile_dict, PdfObject::Stream { dict, raw }))
        }
        _ => None,
    }
}

pub(crate) fn lab_params_from_space(
    space: &PdfObject,
    reader: Option<&PdfReader>,
) -> Option<LabParams> {
    let arr = resolve_space_array(space, reader)?;
    if arr.first().and_then(PdfObject::as_name) != Some("Lab") {
        return None;
    }
    let dict = arr.get(1).and_then(|obj| resolve_to_dict(obj, reader))?;
    Some(LabParams {
        white_point: read_xyz(&dict, "WhitePoint").unwrap_or(D50),
        range: read_range4(&dict, "Range").unwrap_or([-100.0, 100.0, -100.0, 100.0]),
    })
}

pub(crate) fn cal_gray_params_from_space(
    space: &PdfObject,
    reader: Option<&PdfReader>,
) -> Option<CalGrayParams> {
    let arr = resolve_space_array(space, reader)?;
    if arr.first().and_then(PdfObject::as_name) != Some("CalGray") {
        return None;
    }
    let dict = arr.get(1).and_then(|obj| resolve_to_dict(obj, reader))?;
    Some(CalGrayParams {
        white_point: read_xyz(&dict, "WhitePoint").unwrap_or(D50),
        gamma: dict
            .get("Gamma")
            .and_then(PdfObject::as_number)
            .unwrap_or(1.0) as f32,
    })
}

pub(crate) fn cal_rgb_params_from_space(
    space: &PdfObject,
    reader: Option<&PdfReader>,
) -> Option<CalRgbParams> {
    let arr = resolve_space_array(space, reader)?;
    if arr.first().and_then(PdfObject::as_name) != Some("CalRGB") {
        return None;
    }
    let dict = arr.get(1).and_then(|obj| resolve_to_dict(obj, reader))?;
    Some(CalRgbParams {
        white_point: read_xyz(&dict, "WhitePoint").unwrap_or(D50),
        gamma: read_array3(&dict, "Gamma").unwrap_or([1.0, 1.0, 1.0]),
        matrix: read_array9(&dict, "Matrix")
            .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
    })
}

fn resolve_space_array(space: &PdfObject, reader: Option<&PdfReader>) -> Option<Vec<PdfObject>> {
    let resolved = match (space, reader) {
        (PdfObject::Reference { .. }, Some(reader)) => reader.resolve(space.clone()).ok()?,
        _ => space.clone(),
    };
    resolved.as_array().map(|arr| arr.to_vec())
}

fn resolve_to_dict(obj: &PdfObject, reader: Option<&PdfReader>) -> Option<PdfDictionary> {
    let resolved = match (obj, reader) {
        (PdfObject::Reference { .. }, Some(reader)) => reader.resolve(obj.clone()).ok()?,
        _ => obj.clone(),
    };
    resolved.as_dict().cloned()
}

fn read_xyz(dict: &PdfDictionary, key: &str) -> Option<[f32; 3]> {
    let arr = dict.get_array(key)?;
    if arr.len() < 3 {
        return None;
    }
    Some([
        arr[0].as_number()? as f32,
        arr[1].as_number()? as f32,
        arr[2].as_number()? as f32,
    ])
}

fn read_range4(dict: &PdfDictionary, key: &str) -> Option<[f32; 4]> {
    let arr = dict.get_array(key)?;
    if arr.len() < 4 {
        return None;
    }
    Some([
        arr[0].as_number()? as f32,
        arr[1].as_number()? as f32,
        arr[2].as_number()? as f32,
        arr[3].as_number()? as f32,
    ])
}

fn read_array3(dict: &PdfDictionary, key: &str) -> Option<[f32; 3]> {
    let arr = dict.get_array(key)?;
    if arr.len() < 3 {
        return None;
    }
    Some([
        arr[0].as_number()? as f32,
        arr[1].as_number()? as f32,
        arr[2].as_number()? as f32,
    ])
}

fn read_array9(dict: &PdfDictionary, key: &str) -> Option<[f32; 9]> {
    let arr = dict.get_array(key)?;
    if arr.len() < 9 {
        return None;
    }
    let mut out = [0.0; 9];
    for (i, dst) in out.iter_mut().enumerate() {
        *dst = arr[i].as_number()? as f32;
    }
    Some(out)
}

fn adapt_xyz_to_d50(xyz: [f32; 3], source_white: [f32; 3]) -> [f32; 3] {
    if close3(source_white, D50) {
        return xyz;
    }
    const BRADFORD: [[f32; 3]; 3] = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    const BRADFORD_INV: [[f32; 3]; 3] = [
        [0.9869929, -0.1470543, 0.1599627],
        [0.4323053, 0.5183603, 0.0492912],
        [-0.0085287, 0.0400428, 0.9684867],
    ];
    let src_lms = mat3_mul_vec(BRADFORD, source_white);
    let dst_lms = mat3_mul_vec(BRADFORD, D50);
    let xyz_lms = mat3_mul_vec(BRADFORD, xyz);
    let adapted_lms = [
        xyz_lms[0] * safe_ratio(dst_lms[0], src_lms[0]),
        xyz_lms[1] * safe_ratio(dst_lms[1], src_lms[1]),
        xyz_lms[2] * safe_ratio(dst_lms[2], src_lms[2]),
    ];
    mat3_mul_vec(BRADFORD_INV, adapted_lms)
}

fn xyz_d50_to_srgb(xyz: [f32; 3]) -> [f32; 3] {
    // D50-adapted sRGB matrix.
    let r = 3.133_856 * xyz[0] - 1.616_866_7 * xyz[1] - 0.490_614_6 * xyz[2];
    let g = -0.978_768_4 * xyz[0] + 1.916_141_5 * xyz[1] + 0.033_454 * xyz[2];
    let b = 0.071_945_3 * xyz[0] - 0.228_991_4 * xyz[1] + 1.405_242_7 * xyz[2];
    [srgb_encode(r), srgb_encode(g), srgb_encode(b)]
}

fn lab_f_inv(t: f32) -> f32 {
    const DELTA: f32 = 6.0 / 29.0;
    if t > DELTA {
        t * t * t
    } else {
        3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
    }
}

fn srgb_encode(linear: f32) -> f32 {
    let linear = linear.clamp(0.0, 1.0);
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn decode_range(byte: u8, lo: f32, hi: f32) -> f32 {
    lo + (byte as f32 / 255.0) * (hi - lo)
}

fn unit_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

fn mat3_mul_vec(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn safe_ratio(num: f32, den: f32) -> f32 {
    if den.abs() < 1e-6 {
        1.0
    } else {
        num / den
    }
}

fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
    (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4 && (a[2] - b[2]).abs() < 1e-4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(rgb: [f32; 3]) -> [u8; 3] {
        rgb.map(unit_to_u8)
    }

    #[test]
    fn device_cmyk_matches_poppler_process_anchors() {
        assert_eq!(
            bytes(device_cmyk_to_srgb(0.0, 0.0, 0.0, 0.0)),
            [255, 255, 255]
        );
        assert_eq!(
            bytes(device_cmyk_to_srgb(1.0, 0.0, 0.0, 0.0)),
            [0, 173, 239]
        );
        assert_eq!(
            bytes(device_cmyk_to_srgb(0.0, 1.0, 0.0, 0.0)),
            [236, 0, 140]
        );
        assert_eq!(
            bytes(device_cmyk_to_srgb(0.0, 0.0, 1.0, 0.0)),
            [255, 242, 0]
        );
        assert_eq!(
            bytes(device_cmyk_to_srgb(0.0, 1.0, 1.0, 0.0)),
            [237, 28, 36]
        );
        assert_eq!(bytes(device_cmyk_to_srgb(1.0, 0.0, 1.0, 0.0)), [0, 166, 80]);
        assert_eq!(
            bytes(device_cmyk_to_srgb(1.0, 1.0, 0.0, 0.0)),
            [46, 49, 146]
        );
        assert_eq!(bytes(device_cmyk_to_srgb(1.0, 1.0, 1.0, 0.0)), [54, 54, 57]);
        assert_eq!(bytes(device_cmyk_to_srgb(0.0, 0.0, 0.0, 1.0)), [35, 31, 32]);
        assert_eq!(
            bytes(device_cmyk_to_srgb(0.0, 0.0, 0.0, 0.5)),
            [145, 143, 144]
        );
    }

    #[test]
    fn device_cmyk_mixed_color_is_near_poppler_probe() {
        let out = bytes(device_cmyk_to_srgb(0.5, 0.25, 0.0, 0.2));
        assert!(
            (out[0] as i16 - 108).abs() <= 8,
            "R should be near Poppler probe, got {out:?}"
        );
        assert!(
            (out[1] as i16 - 137).abs() <= 12,
            "G should be near Poppler probe, got {out:?}"
        );
        assert!(
            (out[2] as i16 - 182).abs() <= 8,
            "B should be near Poppler probe, got {out:?}"
        );
    }

    #[test]
    fn lab_white_and_black_are_correct() {
        assert_eq!(
            bytes(lab_to_srgb(100.0, 0.0, 0.0, LabParams::default())),
            [255, 255, 255]
        );
        assert_eq!(
            bytes(lab_to_srgb(0.0, 0.0, 0.0, LabParams::default())),
            [0, 0, 0]
        );
    }

    #[test]
    fn lab_mid_gray_is_neutral() {
        let out = bytes(lab_to_srgb(50.0, 0.0, 0.0, LabParams::default()));
        assert!((out[0] as i16 - out[1] as i16).abs() <= 2, "{out:?}");
        assert!((out[1] as i16 - out[2] as i16).abs() <= 2, "{out:?}");
        assert!((115..=125).contains(&out[0]), "{out:?}");
    }

    #[test]
    fn cal_gray_gamma_is_applied() {
        let params = CalGrayParams {
            gamma: 2.0,
            ..CalGrayParams::default()
        };
        let out = bytes(cal_gray_to_srgb(0.5, params));
        assert!(out[0] < 140, "{out:?}");
        assert!((out[0] as i16 - out[1] as i16).abs() <= 2, "{out:?}");
    }
}
