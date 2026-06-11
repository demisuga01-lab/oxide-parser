//! PDF shading and function evaluation (Mega 20).
//!
//! Implements the subset of PDF shading needed for the common cases:
//!
//! - PDF Functions: Type 2 (exponential interpolation) and Type 3 (stitching).
//! - Axial shading (ShadingType 2): a linear gradient between two points.
//! - Radial shading (ShadingType 3): a gradient between two circles.
//!
//! Deliberately deferred (logged, no output):
//! - Function Type 0 (sampled) and Type 4 (PostScript calculator).
//! - ShadingType 1 (function-based) and 4–7 (mesh gradients).
//!
//! Rendering is pixel-by-pixel: for each device pixel we map back to user
//! space, project onto the gradient geometry to obtain the parametric value
//! `t`, evaluate the colour function, and blend. The pre-existing clip mask
//! bounds the painted region, so `sh` and shading-pattern fills only colour
//! the intended area.

use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::render::buffer::{PixelBuffer, PixelColor};
use crate::render::transform::{Transform2D, Viewport};

// ---------------------------------------------------------------------------
// PDF function evaluation
// ---------------------------------------------------------------------------

/// Evaluate a PDF function object at input `t`, returning its output
/// components. Returns an empty `Vec` for unsupported function types or
/// malformed input (never panics).
pub(crate) fn eval_function(func_obj: &PdfObject, t: f64, reader: &PdfReader) -> Vec<f64> {
    let dict = match resolve_to_dict(func_obj, reader) {
        Some(d) => d,
        None => return Vec::new(),
    };
    match dict.get_integer("FunctionType").unwrap_or(-1) {
        2 => eval_type2(&dict, t),
        3 => eval_type3(&dict, t, reader),
        other => {
            log::debug!("PDF Function Type {other} not supported");
            Vec::new()
        }
    }
}

/// Resolve a function reference / dict / stream to its dictionary.
fn resolve_to_dict(obj: &PdfObject, reader: &PdfReader) -> Option<PdfDictionary> {
    match obj {
        PdfObject::Dictionary(d) => Some(d.clone()),
        PdfObject::Stream { dict, .. } => Some(dict.clone()),
        PdfObject::Reference { number, generation } => {
            match reader.get_object(*number, *generation).ok()? {
                PdfObject::Dictionary(d) => Some(d),
                PdfObject::Stream { dict, .. } => Some(dict),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Type 2 (exponential interpolation): `f(t) = C0 + t^N * (C1 - C0)`.
pub(crate) fn eval_type2(dict: &PdfDictionary, t: f64) -> Vec<f64> {
    let n = dict.get("N").and_then(PdfObject::as_number).unwrap_or(1.0);

    let domain = get_float_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
    let d0 = domain.first().copied().unwrap_or(0.0);
    let d1 = domain.get(1).copied().unwrap_or(1.0);
    let t_clamped = t.clamp(d0.min(d1), d0.max(d1));
    let t_norm = if (d1 - d0).abs() < 1e-10 {
        0.0
    } else {
        (t_clamped - d0) / (d1 - d0)
    };

    let c0 = get_float_array(dict, "C0").unwrap_or_else(|| vec![0.0]);
    let c1 = get_float_array(dict, "C1").unwrap_or_else(|| vec![1.0]);
    let len = c0.len().max(c1.len());
    let factor = t_norm.powf(n);

    (0..len)
        .map(|i| {
            let v0 = c0.get(i).copied().unwrap_or(0.0);
            let v1 = c1.get(i).copied().unwrap_or(1.0);
            (v0 + factor * (v1 - v0)).clamp(0.0, 1.0)
        })
        .collect()
}

/// Type 3 (stitching): selects a sub-function by breakpoint and re-encodes `t`.
pub(crate) fn eval_type3(dict: &PdfDictionary, t: f64, reader: &PdfReader) -> Vec<f64> {
    let domain = get_float_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
    let bounds = get_float_array(dict, "Bounds").unwrap_or_default();
    let encode = get_float_array(dict, "Encode").unwrap_or_default();
    let funcs = match dict.get("Functions") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Vec::new(),
    };
    if funcs.is_empty() {
        return Vec::new();
    }

    let d0 = domain.first().copied().unwrap_or(0.0);
    let d1 = domain.get(1).copied().unwrap_or(1.0);
    let t = t.clamp(d0.min(d1), d0.max(d1));

    // Find the sub-function index: first bound the value falls below.
    let idx = {
        let mut found = funcs.len() - 1;
        for (i, &bound) in bounds.iter().enumerate() {
            if t < bound {
                found = i;
                break;
            }
        }
        found.min(funcs.len() - 1)
    };

    // The segment [seg_start, seg_end) this index maps to in the domain.
    let (seg_start, seg_end) = if bounds.is_empty() {
        (d0, d1)
    } else {
        let s = if idx == 0 { d0 } else { bounds[idx - 1] };
        let e = bounds.get(idx).copied().unwrap_or(d1);
        (s, e)
    };

    let e0 = encode.get(idx * 2).copied().unwrap_or(0.0);
    let e1 = encode.get(idx * 2 + 1).copied().unwrap_or(1.0);
    let t_enc = if (seg_end - seg_start).abs() < 1e-10 {
        e0
    } else {
        e0 + (t - seg_start) / (seg_end - seg_start) * (e1 - e0)
    };

    match funcs.get(idx) {
        Some(sub) => eval_function(sub, t_enc, reader),
        None => Vec::new(),
    }
}

/// Read a numeric array from `dict[key]`, returning `None` if absent or empty.
pub(crate) fn get_float_array(dict: &PdfDictionary, key: &str) -> Option<Vec<f64>> {
    let arr = dict.get(key)?.as_array()?;
    let vals: Vec<f64> = arr.iter().filter_map(PdfObject::as_number).collect();
    if vals.is_empty() {
        None
    } else {
        Some(vals)
    }
}

/// Read a 2-element boolean array (e.g. `/Extend [bool bool]`).
pub(crate) fn get_bool_pair(dict: &PdfDictionary, key: &str) -> Option<[bool; 2]> {
    let arr = dict.get(key)?.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    Some([
        arr[0].as_bool().unwrap_or(false),
        arr[1].as_bool().unwrap_or(false),
    ])
}

/// Convert shading function output components to an opaque pixel colour.
pub(crate) fn components_to_pixel(components: &[f64], color_space: &str) -> PixelColor {
    let to_u8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match color_space {
        "DeviceGray" | "CalGray" | "G" => {
            let g = to_u8(components.first().copied().unwrap_or(0.0));
            [g, g, g, 255]
        }
        "DeviceCMYK" | "CMYK" => {
            let c = components.first().copied().unwrap_or(0.0);
            let m = components.get(1).copied().unwrap_or(0.0);
            let y = components.get(2).copied().unwrap_or(0.0);
            let k = components.get(3).copied().unwrap_or(0.0);
            [
                to_u8((1.0 - c) * (1.0 - k)),
                to_u8((1.0 - m) * (1.0 - k)),
                to_u8((1.0 - y) * (1.0 - k)),
                255,
            ]
        }
        // DeviceRGB / CalRGB / ICCBased(3) / unknown: treat as RGB triple.
        _ => [
            to_u8(components.first().copied().unwrap_or(0.0)),
            to_u8(components.get(1).copied().unwrap_or(0.0)),
            to_u8(components.get(2).copied().unwrap_or(0.0)),
            255,
        ],
    }
}

/// Read the shading's colour-space name. Handles both a bare name and an array
/// whose first element names the family (e.g. `[/ICCBased N 0 R]`).
fn shading_color_space_name(dict: &PdfDictionary) -> String {
    match dict.get("ColorSpace") {
        Some(PdfObject::Name(name)) => name.clone(),
        Some(PdfObject::Array(arr)) => arr
            .first()
            .and_then(PdfObject::as_name)
            .unwrap_or("DeviceRGB")
            .to_string(),
        _ => "DeviceRGB".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Shading renderer
// ---------------------------------------------------------------------------

pub struct ShadingRenderer;

impl ShadingRenderer {
    /// Paint a shading dictionary into `buf`, bounded by the buffer's current
    /// clip mask. `ctm` is the current user-space → media-box transform.
    pub fn paint(
        shading_dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
    ) {
        match shading_dict.get_integer("ShadingType").unwrap_or(0) {
            2 => Self::paint_axial(shading_dict, ctm, viewport, buf, reader),
            3 => Self::paint_radial(shading_dict, ctm, viewport, buf, reader),
            other => log::debug!("ShadingRenderer: ShadingType {other} not supported"),
        }
    }

    /// Map a device pixel (px, py) back to user space (the space `Coords` live
    /// in). `pixel → media-box user space` is `inv_vp`; `media-box → current
    /// user space` is `inv_ctm`. Applying inv_vp first then inv_ctm gives the
    /// composite `inv_vp.concat(&inv_ctm)`.
    fn pixel_to_user(ctm: &Transform2D, viewport: &Viewport) -> Transform2D {
        let inv_ctm = ctm.inverse().unwrap_or_else(Transform2D::identity);
        let inv_vp = viewport
            .to_transform()
            .inverse()
            .unwrap_or_else(Transform2D::identity);
        inv_vp.concat(&inv_ctm)
    }

    fn paint_axial(
        dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
    ) {
        let coords = match get_float_array(dict, "Coords") {
            Some(c) if c.len() >= 4 => c,
            _ => {
                log::warn!("axial shading: missing or short /Coords");
                return;
            }
        };
        let (x0, y0, x1, y1) = (coords[0], coords[1], coords[2], coords[3]);

        let extend = get_bool_pair(dict, "Extend").unwrap_or([false, false]);
        let domain = get_float_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
        let t0 = domain.first().copied().unwrap_or(0.0);
        let t1 = domain.get(1).copied().unwrap_or(1.0);

        let func_obj = match dict.get("Function") {
            Some(f) => f.clone(),
            None => {
                log::warn!("axial shading: missing /Function");
                return;
            }
        };
        let color_space = shading_color_space_name(dict);

        let dx = x1 - x0;
        let dy = y1 - y0;
        let len_sq = dx * dx + dy * dy;
        if len_sq < 1e-10 {
            return;
        }

        let pixel_to_user = Self::pixel_to_user(ctm, viewport);
        let w = buf.width as i32;
        let h = buf.height as i32;

        // Cache colours by quantised t to avoid re-evaluating the function per
        // pixel (a 256-bucket lookup is visually indistinguishable here).
        let mut cache: Vec<Option<PixelColor>> = vec![None; 257];

        for py in 0..h {
            for px in 0..w {
                if !buf.clip_allows(px, py) {
                    continue;
                }
                let (ux, uy) = pixel_to_user.transform_point(px as f64 + 0.5, py as f64 + 0.5);
                let s = ((ux - x0) * dx + (uy - y0) * dy) / len_sq;

                let in_range =
                    (0.0..=1.0).contains(&s) || (s < 0.0 && extend[0]) || (s > 1.0 && extend[1]);
                if !in_range {
                    continue;
                }
                let s_clamped = s.clamp(0.0, 1.0);
                let color = Self::color_for(
                    s_clamped,
                    t0,
                    t1,
                    &func_obj,
                    &color_space,
                    reader,
                    &mut cache,
                );
                if let Some(color) = color {
                    buf.blend_pixel(px, py, color, 1.0);
                }
            }
        }
    }

    fn paint_radial(
        dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
    ) {
        let coords = match get_float_array(dict, "Coords") {
            Some(c) if c.len() >= 6 => c,
            _ => {
                log::warn!("radial shading: missing or short /Coords");
                return;
            }
        };
        let (x0, y0, r0) = (coords[0], coords[1], coords[2]);
        let (x1, y1, r1) = (coords[3], coords[4], coords[5]);

        let extend = get_bool_pair(dict, "Extend").unwrap_or([false, false]);
        let domain = get_float_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
        let t0 = domain.first().copied().unwrap_or(0.0);
        let t1 = domain.get(1).copied().unwrap_or(1.0);

        let func_obj = match dict.get("Function") {
            Some(f) => f.clone(),
            None => {
                log::warn!("radial shading: missing /Function");
                return;
            }
        };
        let color_space = shading_color_space_name(dict);

        let ax = x1 - x0;
        let ay = y1 - y0;
        let ar = r1 - r0;
        let aa = ax * ax + ay * ay - ar * ar;

        let pixel_to_user = Self::pixel_to_user(ctm, viewport);
        let w = buf.width as i32;
        let h = buf.height as i32;
        let mut cache: Vec<Option<PixelColor>> = vec![None; 257];

        for py in 0..h {
            for px in 0..w {
                if !buf.clip_allows(px, py) {
                    continue;
                }
                let (ux, uy) = pixel_to_user.transform_point(px as f64 + 0.5, py as f64 + 0.5);
                let dx = ux - x0;
                let dy = uy - y0;

                // Solve |P - C0 - s*(C1-C0)|^2 = (r0 + s*ar)^2 for the largest
                // s with a non-negative radius and within the (extended) range.
                let bb = 2.0 * (dx * ax + dy * ay + r0 * ar);
                let cc = dx * dx + dy * dy - r0 * r0;

                let s = if aa.abs() < 1e-10 {
                    if bb.abs() < 1e-10 {
                        None
                    } else {
                        accept_radial_s(cc / bb, ar, r0, extend)
                    }
                } else {
                    let disc = bb * bb - 4.0 * aa * cc;
                    if disc < 0.0 {
                        None
                    } else {
                        let sq = disc.sqrt();
                        let s_pos = (bb + sq) / (2.0 * aa);
                        let s_neg = (bb - sq) / (2.0 * aa);
                        accept_radial_s(s_pos, ar, r0, extend)
                            .into_iter()
                            .chain(accept_radial_s(s_neg, ar, r0, extend))
                            .reduce(f64::max)
                    }
                };

                let s = match s {
                    Some(s) => s.clamp(0.0, 1.0),
                    None => continue,
                };
                let color = Self::color_for(s, t0, t1, &func_obj, &color_space, reader, &mut cache);
                if let Some(color) = color {
                    buf.blend_pixel(px, py, color, 1.0);
                }
            }
        }
    }

    /// Map parametric `s ∈ [0,1]` to a pixel colour, caching by quantised `s`.
    #[allow(clippy::too_many_arguments)]
    fn color_for(
        s: f64,
        t0: f64,
        t1: f64,
        func_obj: &PdfObject,
        color_space: &str,
        reader: &PdfReader,
        cache: &mut [Option<PixelColor>],
    ) -> Option<PixelColor> {
        let bucket = (s.clamp(0.0, 1.0) * 256.0).round() as usize;
        if let Some(cached) = cache.get(bucket).and_then(|c| *c) {
            return Some(cached);
        }
        let t = t0 + s * (t1 - t0);
        let components = eval_function(func_obj, t, reader);
        if components.is_empty() {
            return None;
        }
        let color = components_to_pixel(&components, color_space);
        if let Some(slot) = cache.get_mut(bucket) {
            *slot = Some(color);
        }
        Some(color)
    }
}

/// Accept a candidate radial `s` if its circle radius is non-negative and it is
/// within the (possibly extended) parametric range.
fn accept_radial_s(s: f64, ar: f64, r0: f64, extend: [bool; 2]) -> Option<f64> {
    if r0 + s * ar < 0.0 {
        return None;
    }
    let in_range = (0.0..=1.0).contains(&s) || (s < 0.0 && extend[0]) || (s > 1.0 && extend[1]);
    if in_range {
        Some(s)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn make_type2_dict(c0: &[f64], c1: &[f64], n: f64) -> PdfDictionary {
        dict(&[
            ("FunctionType", PdfObject::Integer(2)),
            (
                "Domain",
                PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
            ),
            (
                "C0",
                PdfObject::Array(c0.iter().map(|&v| PdfObject::Real(v)).collect()),
            ),
            (
                "C1",
                PdfObject::Array(c1.iter().map(|&v| PdfObject::Real(v)).collect()),
            ),
            ("N", PdfObject::Real(n)),
        ])
    }

    #[test]
    fn type2_at_t0_returns_c0() {
        let d = make_type2_dict(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0], 1.0);
        let r = eval_type2(&d, 0.0);
        assert!((r[0] - 1.0).abs() < 0.01);
        assert!((r[2] - 0.0).abs() < 0.01);
    }

    #[test]
    fn type2_at_t1_returns_c1() {
        let d = make_type2_dict(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0], 1.0);
        let r = eval_type2(&d, 1.0);
        assert!((r[0] - 0.0).abs() < 0.01);
        assert!((r[2] - 1.0).abs() < 0.01);
    }

    #[test]
    fn type2_midpoint_linear() {
        let d = make_type2_dict(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0], 1.0);
        let r = eval_type2(&d, 0.5);
        assert!((r[0] - 0.5).abs() < 0.01, "R at 0.5 = {}", r[0]);
        assert!((r[2] - 0.5).abs() < 0.01, "B at 0.5 = {}", r[2]);
    }

    #[test]
    fn type2_quadratic_exponent() {
        let d = make_type2_dict(&[0.0], &[1.0], 2.0);
        let r = eval_type2(&d, 0.5);
        assert!((r[0] - 0.25).abs() < 0.01, "quadratic at 0.5 = {}", r[0]);
    }

    #[test]
    fn type2_output_clamped_to_unit_range() {
        let d = make_type2_dict(&[0.9], &[0.1], 1.0);
        let r = eval_type2(&d, 2.0);
        assert!((0.0..=1.0).contains(&r[0]), "out of range: {}", r[0]);
    }

    #[test]
    fn type3_single_subfunction_delegates() {
        // Build a reader-independent Type 3 with an inline Type 2 sub-function.
        let sub = PdfObject::Dictionary(make_type2_dict(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0], 1.0));
        let d = dict(&[
            ("FunctionType", PdfObject::Integer(3)),
            (
                "Domain",
                PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
            ),
            ("Functions", PdfObject::Array(vec![sub])),
            ("Bounds", PdfObject::Array(vec![])),
            (
                "Encode",
                PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
            ),
        ]);
        // eval_type3 only consults `reader` for indirect sub-functions; with an
        // inline dict it is never used, so a throwaway reader suffices. We build
        // one from a trivial PDF.
        let reader = crate::reader::PdfReader::from_bytes(super::tests::minimal_pdf()).unwrap();
        let r = eval_type3(&d, 0.5, &reader);
        assert!((r[0] - 0.5).abs() < 0.01, "Type3->Type2 at 0.5: {:?}", r);
    }

    /// A minimal valid PDF used only to obtain a `PdfReader` for tests that need
    /// one but never actually resolve indirect objects.
    pub(super) fn minimal_pdf() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut off = vec![0usize; 4];
        off[1] = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        off[2] = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        off[3] = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
        );
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for i in 1..=3 {
            pdf.extend_from_slice(format!("{:010} 00000 n \n", off[i]).as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    #[test]
    fn bool_pair_reads_extend() {
        let d = dict(&[(
            "Extend",
            PdfObject::Array(vec![PdfObject::Boolean(true), PdfObject::Boolean(false)]),
        )]);
        assert_eq!(get_bool_pair(&d, "Extend").unwrap(), [true, false]);
    }

    #[test]
    fn bool_pair_missing_is_none() {
        assert!(get_bool_pair(&PdfDictionary::empty(), "Extend").is_none());
    }

    #[test]
    fn components_to_pixel_rgb() {
        let c = components_to_pixel(&[1.0, 0.5, 0.0], "DeviceRGB");
        assert_eq!(c[0], 255);
        assert!((c[1] as i32 - 128).abs() <= 1, "G≈128: {}", c[1]);
        assert_eq!(c[2], 0);
        assert_eq!(c[3], 255);
    }

    #[test]
    fn components_to_pixel_gray() {
        let c = components_to_pixel(&[0.5], "DeviceGray");
        assert!((c[0] as i32 - 128).abs() <= 2, "gray≈128: {}", c[0]);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[0], c[2]);
    }

    #[test]
    fn components_to_pixel_cmyk_black_and_white() {
        let black = components_to_pixel(&[0.0, 0.0, 0.0, 1.0], "DeviceCMYK");
        assert_eq!(black, [0, 0, 0, 255]);
        let white = components_to_pixel(&[0.0, 0.0, 0.0, 0.0], "DeviceCMYK");
        assert_eq!(white, [255, 255, 255, 255]);
    }
}
