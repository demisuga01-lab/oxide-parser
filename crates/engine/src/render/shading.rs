//! PDF shading and function evaluation.
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
use crate::render::color::{ColorSpaceHandler, RenderColor};
use crate::render::transform::{Transform2D, Viewport};

/// A minimal valid PDF used by render tests that need a `PdfReader` but never
/// resolve indirect objects. Crate-visible so sibling render modules can reuse
/// it for function/shading tests.
#[cfg(test)]
pub(crate) fn tests_minimal_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut off = [0usize; 4];
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
    for offset in off.iter().take(4).skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

// ---------------------------------------------------------------------------
// PDF function evaluation
// ---------------------------------------------------------------------------

/// Evaluate a single-input PDF function object at input `t`, returning its
/// output components. Delegates to the multi-input dispatcher in
/// [`crate::render::function`], which supports Function Types 0, 2, 3, and 4.
/// Returns an empty `Vec` for unsupported types or malformed input.
pub(crate) fn eval_function(func_obj: &PdfObject, t: f64, reader: &PdfReader) -> Vec<f64> {
    crate::render::function::eval_function_n(func_obj, &[t], reader)
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
#[cfg(test)]
pub(crate) fn components_to_pixel(components: &[f64], color_space: &str) -> PixelColor {
    components_to_render_color(components, color_space).to_pixel_color()
}

/// Convert shading function output components to a float render colour.
pub(crate) fn components_to_render_color(components: &[f64], color_space: &str) -> RenderColor {
    ColorSpaceHandler::from_components(color_space, components, 1.0)
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

const SHADING_LUT_STEPS: usize = 4096;

const BAYER_8X8: [u8; 64] = [
    0, 48, 12, 60, 3, 51, 15, 63, 32, 16, 44, 28, 35, 19, 47, 31, 8, 56, 4, 52, 11, 59, 7, 55, 40,
    24, 36, 20, 43, 27, 39, 23, 2, 50, 14, 62, 1, 49, 13, 61, 34, 18, 46, 30, 33, 17, 45, 29, 10,
    58, 6, 54, 9, 57, 5, 53, 42, 26, 38, 22, 41, 25, 37, 21,
];

#[derive(Debug, Clone)]
struct ShadingColorCache {
    entries: Vec<Option<RenderColor>>,
}

impl ShadingColorCache {
    fn new() -> Self {
        Self {
            entries: vec![None; SHADING_LUT_STEPS + 1],
        }
    }

    #[inline]
    fn bucket(s: f64) -> usize {
        (s.clamp(0.0, 1.0) * SHADING_LUT_STEPS as f64).round() as usize
    }

    #[inline]
    fn get(&self, s: f64) -> Option<RenderColor> {
        self.entries.get(Self::bucket(s)).and_then(|entry| *entry)
    }

    #[inline]
    fn set(&mut self, s: f64, color: RenderColor) {
        if let Some(slot) = self.entries.get_mut(Self::bucket(s)) {
            *slot = Some(color);
        }
    }
}

#[inline]
fn ordered_dither_offset(x: i32, y: i32) -> f32 {
    let xi = x.rem_euclid(8) as usize;
    let yi = y.rem_euclid(8) as usize;
    (BAYER_8X8[yi * 8 + xi] as f32 + 0.5) / 64.0 - 0.5
}

#[inline]
fn quantize_shading_channel(value: f32, dither_offset: f32, dither: bool) -> u8 {
    let scaled = value.clamp(0.0, 1.0) * 255.0;
    let adjusted = if dither {
        scaled + dither_offset
    } else {
        scaled
    };
    adjusted.round().clamp(0.0, 255.0) as u8
}

#[inline]
fn quantize_shading_color(color: RenderColor, x: i32, y: i32, dither: bool) -> PixelColor {
    let offset = if dither {
        ordered_dither_offset(x, y)
    } else {
        0.0
    };
    [
        quantize_shading_channel(color.r, offset, dither),
        quantize_shading_channel(color.g, offset, dither),
        quantize_shading_channel(color.b, offset, dither),
        (color.a.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

impl ShadingRenderer {
    /// Paint a shading dictionary into `buf`, bounded by the buffer's current
    /// clip mask. `ctm` is the current user-space → media-box transform.
    /// `mesh_data` is the shading's decoded stream body, required for mesh
    /// shadings (Types 4–7 store vertex/patch data in the stream); it is `None`
    /// for dictionary-only shadings (Types 1–3).
    pub fn paint(
        shading_dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
        mesh_data: Option<&[u8]>,
    ) {
        match shading_dict.get_integer("ShadingType").unwrap_or(0) {
            1 => Self::paint_function_based(shading_dict, ctm, viewport, buf, reader),
            2 => Self::paint_axial(shading_dict, ctm, viewport, buf, reader),
            3 => Self::paint_radial(shading_dict, ctm, viewport, buf, reader),
            4 | 5 => Self::paint_gouraud_mesh(shading_dict, ctm, viewport, buf, reader, mesh_data),
            6 | 7 => Self::paint_patch_mesh(shading_dict, ctm, viewport, buf, reader, mesh_data),
            other => log::debug!("ShadingRenderer: ShadingType {other} not supported"),
        }
    }

    /// ShadingType 1 (function-based): color at each point (x, y) within /Domain
    /// is the result of a 2-input function, optionally pre-transformed by the
    /// shading's /Matrix. We iterate device pixels, map back to domain space, and
    /// evaluate.
    fn paint_function_based(
        dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
    ) {
        let func_obj = match dict.get("Function") {
            Some(f) => f.clone(),
            None => {
                log::warn!("function-based shading: missing /Function");
                return;
            }
        };
        // Domain [x0 x1 y0 y1] (defaults to the unit square).
        let domain = get_float_array(dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0, 0.0, 1.0]);
        let (dx0, dx1) = (
            domain.first().copied().unwrap_or(0.0),
            domain.get(1).copied().unwrap_or(1.0),
        );
        let (dy0, dy1) = (
            domain.get(2).copied().unwrap_or(0.0),
            domain.get(3).copied().unwrap_or(1.0),
        );
        // /Matrix maps domain space → the shading's target user space.
        let shading_matrix = match get_float_array(dict, "Matrix") {
            Some(m) if m.len() >= 6 => Transform2D::from([m[0], m[1], m[2], m[3], m[4], m[5]]),
            _ => Transform2D::identity(),
        };
        let color_space = shading_color_space_name(dict);
        let dither = buf.render_mode().is_high_quality();

        // device pixel → user space → domain space.
        let pixel_to_user = Self::pixel_to_user(ctm, viewport);
        let user_to_domain = shading_matrix
            .inverse()
            .unwrap_or_else(Transform2D::identity);

        let w = buf.width as i32;
        let h = buf.height as i32;
        for py in 0..h {
            for px in 0..w {
                if !buf.clip_allows(px, py) {
                    continue;
                }
                let (ux, uy) = pixel_to_user.transform_point(px as f64 + 0.5, py as f64 + 0.5);
                let (mx, my) = user_to_domain.transform_point(ux, uy);
                if mx < dx0.min(dx1) || mx > dx0.max(dx1) || my < dy0.min(dy1) || my > dy0.max(dy1)
                {
                    continue;
                }
                let comps = crate::render::function::eval_function_n(&func_obj, &[mx, my], reader);
                if comps.is_empty() {
                    continue;
                }
                let color = components_to_render_color(&comps, &color_space);
                let pixel = quantize_shading_color(color, px, py, dither);
                buf.blend_pixel(px, py, pixel, 1.0);
            }
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

        // Cache high-resolution float colours to avoid re-evaluating the
        // function per pixel without prematurely stepping the gradient at 8-bit
        // output precision.
        let mut cache = ShadingColorCache::new();
        let dither = buf.render_mode().is_high_quality();

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
                    let pixel = quantize_shading_color(color, px, py, dither);
                    buf.blend_pixel(px, py, pixel, 1.0);
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
        let mut cache = ShadingColorCache::new();
        let dither = buf.render_mode().is_high_quality();

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
                    let pixel = quantize_shading_color(color, px, py, dither);
                    buf.blend_pixel(px, py, pixel, 1.0);
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
        cache: &mut ShadingColorCache,
    ) -> Option<RenderColor> {
        if let Some(cached) = cache.get(s) {
            return Some(cached);
        }
        let t = t0 + s * (t1 - t0);
        let components = eval_function(func_obj, t, reader);
        if components.is_empty() {
            return None;
        }
        let color = components_to_render_color(&components, color_space);
        cache.set(s, color);
        Some(color)
    }
}

// ---------------------------------------------------------------------------
// Mesh shadings (Types 4-7): shared vertex model + Gouraud triangle rasterizer
// ---------------------------------------------------------------------------

/// A mesh vertex in device space with an associated RGBA color.
#[derive(Debug, Clone, Copy)]
struct MeshVertex {
    /// Device-space x, y (already mapped through CTM + viewport).
    dx: f64,
    dy: f64,
    color: RenderColor,
}

/// Decode parameters shared by the mesh vertex stream readers.
struct MeshDecode {
    bits_per_coord: usize,
    bits_per_comp: usize,
    bits_per_flag: usize,
    /// Decode array: [xmin xmax ymin ymax c1min c1max ...].
    decode: Vec<f64>,
    /// Number of color components per vertex when colors are given directly
    /// (no /Function); 1 when a /Function maps a single parametric value.
    n_color: usize,
    has_function: bool,
}

impl MeshDecode {
    fn from_dict(dict: &PdfDictionary, color_space: &str) -> Option<Self> {
        let bits_per_coord = dict.get_integer("BitsPerCoordinate")? as usize;
        let bits_per_comp = dict.get_integer("BitsPerComponent")? as usize;
        let bits_per_flag = dict.get_integer("BitsPerFlag").unwrap_or(8) as usize;
        let decode = get_float_array(dict, "Decode")?;
        let has_function = dict.get("Function").is_some();
        let n_color = if has_function {
            1
        } else {
            color_space_component_count(color_space)
        };
        Some(Self {
            bits_per_coord,
            bits_per_comp,
            bits_per_flag,
            decode,
            n_color,
            has_function,
        })
    }

    /// Read one coordinate pair, mapping through Decode + CTM/viewport to device
    /// space, plus the raw color components (parametric or direct).
    fn read_vertex(
        &self,
        br: &mut crate::render::function::BitReader,
        to_device: &Transform2D,
        func_obj: Option<&PdfObject>,
        color_space: &str,
        reader: &PdfReader,
    ) -> Option<MeshVertex> {
        let xr = br.read(self.bits_per_coord)? as f64;
        let yr = br.read(self.bits_per_coord)? as f64;
        let xmax_raw = crate::render::function::max_value(self.bits_per_coord);
        let x = decode_value(
            xr,
            xmax_raw,
            self.decode.first().copied().unwrap_or(0.0),
            self.decode.get(1).copied().unwrap_or(1.0),
        );
        let y = decode_value(
            yr,
            xmax_raw,
            self.decode.get(2).copied().unwrap_or(0.0),
            self.decode.get(3).copied().unwrap_or(1.0),
        );
        let (dx, dy) = to_device.transform_point(x, y);

        let cmax_raw = crate::render::function::max_value(self.bits_per_comp);
        let mut comps = Vec::with_capacity(self.n_color);
        for k in 0..self.n_color {
            let raw = br.read(self.bits_per_comp)? as f64;
            let dlo = self.decode.get(4 + 2 * k).copied().unwrap_or(0.0);
            let dhi = self.decode.get(5 + 2 * k).copied().unwrap_or(1.0);
            comps.push(decode_value(raw, cmax_raw, dlo, dhi));
        }
        let color = resolve_vertex_color(&comps, self.has_function, func_obj, color_space, reader);
        Some(MeshVertex { dx, dy, color })
    }
}

/// Map a raw integer sample in [0, max] onto [lo, hi].
fn decode_value(raw: f64, max: f64, lo: f64, hi: f64) -> f64 {
    if max <= 0.0 {
        lo
    } else {
        lo + (raw / max) * (hi - lo)
    }
}

fn color_space_component_count(name: &str) -> usize {
    match name {
        "DeviceGray" | "CalGray" | "G" => 1,
        "DeviceCMYK" | "CMYK" => 4,
        _ => 3,
    }
}

/// Turn a vertex's raw color components into an RGBA color. With a /Function the
/// single parametric value is mapped through it first.
fn resolve_vertex_color(
    comps: &[f64],
    has_function: bool,
    func_obj: Option<&PdfObject>,
    color_space: &str,
    reader: &PdfReader,
) -> RenderColor {
    let resolved = if has_function {
        match func_obj {
            Some(f) => {
                let t = comps.first().copied().unwrap_or(0.0);
                crate::render::function::eval_function_n(f, &[t], reader)
            }
            None => comps.to_vec(),
        }
    } else {
        comps.to_vec()
    };
    components_to_render_color(&resolved, color_space)
}

impl ShadingRenderer {
    /// ShadingType 4 (free-form) and 5 (lattice-form) Gouraud triangle meshes.
    fn paint_gouraud_mesh(
        dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
        mesh_data: Option<&[u8]>,
    ) {
        let shading_type = dict.get_integer("ShadingType").unwrap_or(4);
        let color_space = shading_color_space_name(dict);
        let Some(dec) = MeshDecode::from_dict(dict, &color_space) else {
            log::warn!("mesh shading: missing BitsPerCoordinate/BitsPerComponent/Decode");
            return;
        };
        let data = match mesh_data {
            Some(d) => d.to_vec(),
            None => {
                log::warn!("mesh shading: vertex stream not available");
                return;
            }
        };
        let func_obj = dict.get("Function").cloned();
        let to_device = ctm.concat(&viewport.to_transform());
        let mut br = crate::render::function::BitReader::new(&data);

        if shading_type == 5 {
            // Lattice-form: a grid of /VerticesPerRow columns; each 2x2 cell of
            // adjacent rows makes two triangles. No flags, no colors-as-flags.
            let per_row = dict.get_integer("VerticesPerRow").unwrap_or(0).max(0) as usize;
            if per_row < 2 {
                log::warn!("lattice mesh: VerticesPerRow < 2");
                return;
            }
            let mut prev_row: Vec<MeshVertex> = Vec::new();
            loop {
                // Read one row.
                let mut row = Vec::with_capacity(per_row);
                let mut complete = true;
                for _ in 0..per_row {
                    match dec.read_vertex(
                        &mut br,
                        &to_device,
                        func_obj.as_ref(),
                        &color_space,
                        reader,
                    ) {
                        Some(v) => row.push(v),
                        None => {
                            complete = false;
                            break;
                        }
                    }
                }
                if !complete || row.len() < per_row {
                    break;
                }
                if !prev_row.is_empty() {
                    for c in 0..per_row - 1 {
                        // Two triangles per cell.
                        fill_gouraud_triangle(buf, prev_row[c], prev_row[c + 1], row[c]);
                        fill_gouraud_triangle(buf, prev_row[c + 1], row[c + 1], row[c]);
                    }
                }
                prev_row = row;
            }
            return;
        }

        // Free-form (Type 4): each vertex prefixed by a flag byte.
        // flag 0 starts a new triangle (needs 3 consecutive flag-0 vertices);
        // flag 1 reuses (vb, vc) -> (vc, new); flag 2 reuses (va, vc) -> (vc,new).
        let mut va: Option<MeshVertex> = None;
        let mut vb: Option<MeshVertex> = None;
        let mut vc: Option<MeshVertex> = None;
        while br.bits_remaining() >= dec.bits_per_flag + 2 * dec.bits_per_coord {
            let flag = match br.read(dec.bits_per_flag) {
                Some(f) => f,
                None => break,
            };
            let v =
                match dec.read_vertex(&mut br, &to_device, func_obj.as_ref(), &color_space, reader)
                {
                    Some(v) => v,
                    None => break,
                };
            br.align_to_byte();
            match flag {
                0 => {
                    // Shift in as the next of a fresh triangle.
                    if va.is_none() {
                        va = Some(v);
                    } else if vb.is_none() {
                        vb = Some(v);
                    } else {
                        vc = Some(v);
                        if let (Some(a), Some(b), Some(c)) = (va, vb, vc) {
                            fill_gouraud_triangle(buf, a, b, c);
                        }
                    }
                }
                1 => {
                    // (vb, vc, v): reuse the last edge (b, c).
                    if let (Some(b), Some(c)) = (vb, vc) {
                        va = Some(b);
                        vb = Some(c);
                        vc = Some(v);
                        fill_gouraud_triangle(buf, b, c, v);
                    }
                }
                2 => {
                    // (va, vc, v): reuse edge (a, c).
                    if let (Some(a), Some(c)) = (va, vc) {
                        vb = Some(c);
                        vc = Some(v);
                        fill_gouraud_triangle(buf, a, c, v);
                    }
                }
                _ => break,
            }
        }
    }

    /// ShadingType 6 (Coons) and 7 (tensor-product) patch meshes. Each patch is
    /// subdivided into a fixed grid of Gouraud quads using bilinear corner-color
    /// interpolation and bicubic Bezier surface positions.
    fn paint_patch_mesh(
        dict: &PdfDictionary,
        ctm: &Transform2D,
        viewport: &Viewport,
        buf: &mut PixelBuffer,
        reader: &PdfReader,
        mesh_data: Option<&[u8]>,
    ) {
        let shading_type = dict.get_integer("ShadingType").unwrap_or(6);
        let n_points_new = if shading_type == 7 { 16 } else { 12 };
        let color_space = shading_color_space_name(dict);
        let Some(dec) = MeshDecode::from_dict(dict, &color_space) else {
            log::warn!("patch mesh: missing BitsPerCoordinate/BitsPerComponent/Decode");
            return;
        };
        let data = match mesh_data {
            Some(d) => d.to_vec(),
            None => return,
        };
        let func_obj = dict.get("Function").cloned();
        let to_device = ctm.concat(&viewport.to_transform());
        let mut br = crate::render::function::BitReader::new(&data);

        let coord_max = crate::render::function::max_value(dec.bits_per_coord);
        let comp_max = crate::render::function::max_value(dec.bits_per_comp);

        // Previous patch's control points (in patch/user space, pre-device) and
        // corner colors, for edge sharing (flags 1/2/3).
        let mut prev_pts: Vec<(f64, f64)> = Vec::new();
        let mut prev_cols: Vec<RenderColor> = Vec::new();

        loop {
            if br.bits_remaining() < dec.bits_per_flag {
                break;
            }
            let flag = match br.read(dec.bits_per_flag) {
                Some(f) => f,
                None => break,
            };
            let new_pts_count = if flag == 0 {
                n_points_new
            } else {
                n_points_new - 4
            };
            let new_cols_count = if flag == 0 { 4 } else { 2 };

            // Read new control points (user space).
            let mut new_pts = Vec::with_capacity(new_pts_count);
            let mut ok = true;
            for _ in 0..new_pts_count {
                let (Some(xr), Some(yr)) =
                    (br.read(dec.bits_per_coord), br.read(dec.bits_per_coord))
                else {
                    ok = false;
                    break;
                };
                let x = decode_value(
                    xr as f64,
                    coord_max,
                    dec.decode.first().copied().unwrap_or(0.0),
                    dec.decode.get(1).copied().unwrap_or(1.0),
                );
                let y = decode_value(
                    yr as f64,
                    coord_max,
                    dec.decode.get(2).copied().unwrap_or(0.0),
                    dec.decode.get(3).copied().unwrap_or(1.0),
                );
                new_pts.push((x, y));
            }
            if !ok {
                break;
            }
            // Read new corner colors.
            let mut new_cols = Vec::with_capacity(new_cols_count);
            for _ in 0..new_cols_count {
                let mut comps = Vec::with_capacity(dec.n_color);
                for k in 0..dec.n_color {
                    let Some(raw) = br.read(dec.bits_per_comp) else {
                        ok = false;
                        break;
                    };
                    let dlo = dec.decode.get(4 + 2 * k).copied().unwrap_or(0.0);
                    let dhi = dec.decode.get(5 + 2 * k).copied().unwrap_or(1.0);
                    comps.push(decode_value(raw as f64, comp_max, dlo, dhi));
                }
                if !ok {
                    break;
                }
                new_cols.push(resolve_vertex_color(
                    &comps,
                    dec.has_function,
                    func_obj.as_ref(),
                    &color_space,
                    reader,
                ));
            }
            if !ok {
                break;
            }
            br.align_to_byte();

            // Assemble the full 12 (Coons) control points and 4 corner colors,
            // sharing an edge from the previous patch when flag != 0.
            let (pts12, cols4) = match assemble_patch(
                flag,
                &new_pts,
                &new_cols,
                &prev_pts,
                &prev_cols,
                shading_type,
            ) {
                Some(v) => v,
                None => break,
            };

            // Render the patch by fixed-grid subdivision.
            render_coons_patch(buf, &pts12, &cols4, &to_device);

            prev_pts = pts12;
            prev_cols = cols4;
        }
    }
}

/// A point in patch/user space (pre-device).
type PatchPoint = (f64, f64);
/// A patch's resolved control points and 4 corner colors.
type PatchData = (Vec<PatchPoint>, Vec<RenderColor>);

/// Assemble a patch's full 12 Coons boundary control points and 4 corner colors,
/// honoring edge-sharing flags 1/2/3 (the new patch shares one edge with the
/// previous one). For tensor patches (type 7) the 4 internal points (p13..p16,
/// stream indices 12..15) are dropped here — the Coons surface in
/// [`coons_point`] uses only the 12 boundary points, which is visually very
/// close for the smooth fills these files use.
///
/// **Spec mapping (ISO 32000-1 §8.7.4.5.7, Table 85; cross-checked against
/// Apache PDFBox `Patch`/`CoonsPatch`, GSoC 2014, Apache-2.0).** The boundary
/// point order p1..p12 (0-based 0..11) traces the four cubic Bézier edges:
/// `C1`: p1 p2 p3 p4 (v at u=0); `D2`: p4 p5 p6 p7 (u at v=1);
/// `C2` reversed: p7 p8 p9 p10 (v at u=1); `D1` reversed: p10 p11 p12 p1
/// (u at v=0). Corners and their colors: p1↔c1, p4↔c2, p7↔c3, p10↔c4
/// (0-based corner indices 0, 3, 6, 9 ↔ colors 0, 1, 2, 3).
///
/// For a flagged patch (flag f), the new patch's first 4 boundary points
/// (its `p1..p4`, i.e. the shared edge) and its first 2 corner colors (`c1, c2`)
/// are taken from the *previous* patch; the stream then supplies the remaining 8
/// points (`p5..p12`) and 2 colors (`c3, c4`). The exact previous-patch indices
/// reused for each flag are:
///
/// | flag | shared points (prev idx) | shared colors (prev idx) |
/// |------|--------------------------|--------------------------|
/// | 1    | p4 p5 p6 p7   = [3,4,5,6]   | c2 c3 = [1,2] |
/// | 2    | p7 p8 p9 p10  = [6,7,8,9]   | c3 c4 = [2,3] |
/// | 3    | p10 p11 p12 p1 = [9,10,11,0] | c4 c1 = [3,0] |
fn assemble_patch(
    flag: u32,
    new_pts: &[(f64, f64)],
    new_cols: &[RenderColor],
    prev_pts: &[(f64, f64)],
    prev_cols: &[RenderColor],
    shading_type: i64,
) -> Option<PatchData> {
    // Keep only the 12 boundary points; tensor patches carry 4 extra interior
    // points after them (stream indices 12..15) that the Coons surface ignores.
    let take_coons = |pts: &[(f64, f64)]| -> Vec<(f64, f64)> {
        if shading_type == 7 {
            pts.iter().take(12).copied().collect()
        } else {
            pts.to_vec()
        }
    };

    if flag == 0 {
        let pts = take_coons(new_pts);
        if pts.len() < 12 || new_cols.len() < 4 {
            return None;
        }
        return Some((pts, new_cols.to_vec()));
    }

    // Shared-edge patches reuse one edge (4 points + 2 colors) of the previous
    // patch; the previous patch must therefore be fully formed.
    if prev_pts.len() < 12 || prev_cols.len() < 4 {
        return None;
    }
    let shared_edge: [usize; 4] = shared_edge_indices(flag);
    let shared_cols: [usize; 2] = shared_color_indices(flag);

    // new boundary = [shared edge p1..p4] ++ [8 new points p5..p12].
    let mut pts = Vec::with_capacity(12);
    for &i in &shared_edge {
        pts.push(prev_pts[i]);
    }
    let new_coons = take_coons(new_pts);
    for &p in new_coons.iter().take(8) {
        pts.push(p);
    }
    if pts.len() < 12 {
        return None;
    }
    pts.truncate(12);

    // new corner colors = [shared c1, c2] ++ [2 new colors c3, c4].
    let mut cols = Vec::with_capacity(4);
    cols.push(prev_cols[shared_cols[0]]);
    cols.push(prev_cols[shared_cols[1]]);
    for &c in new_cols.iter().take(2) {
        cols.push(c);
    }
    if cols.len() < 4 {
        return None;
    }
    Some((pts, cols))
}

/// Previous-patch boundary-point indices reused as the new patch's shared edge
/// (p1..p4) for edge flags 1/2/3. See [`assemble_patch`] for the spec table.
fn shared_edge_indices(flag: u32) -> [usize; 4] {
    match flag {
        1 => [3, 4, 5, 6],
        2 => [6, 7, 8, 9],
        _ => [9, 10, 11, 0],
    }
}

/// Previous-patch corner-color indices reused as the new patch's shared colors
/// (c1, c2) for edge flags 1/2/3. See [`assemble_patch`] for the spec table.
fn shared_color_indices(flag: u32) -> [usize; 2] {
    match flag {
        1 => [1, 2],
        2 => [2, 3],
        _ => [3, 0],
    }
}

/// Render a Coons patch by subdividing its boundary into an N×N grid of quads,
/// each split into two Gouraud triangles. Positions come from the bicubic Coons
/// surface; colors come from bilinear interpolation of the 4 corner colors.
fn render_coons_patch(
    buf: &mut PixelBuffer,
    pts12: &[(f64, f64)],
    cols4: &[RenderColor],
    to_device: &Transform2D,
) {
    if pts12.len() < 12 || cols4.len() < 4 {
        return;
    }
    const N: usize = 10;
    // Build the grid of device-space vertices + colors.
    let mut grid: Vec<Vec<MeshVertex>> = Vec::with_capacity(N + 1);
    for iu in 0..=N {
        let u = iu as f64 / N as f64;
        let mut row = Vec::with_capacity(N + 1);
        for iv in 0..=N {
            let v = iv as f64 / N as f64;
            let (ux, uy) = coons_point(pts12, u, v);
            let (dx, dy) = to_device.transform_point(ux, uy);
            let color = bilerp_color(cols4, u, v);
            row.push(MeshVertex { dx, dy, color });
        }
        grid.push(row);
    }
    for iu in 0..N {
        for iv in 0..N {
            let a = grid[iu][iv];
            let b = grid[iu + 1][iv];
            let c = grid[iu][iv + 1];
            let d = grid[iu + 1][iv + 1];
            fill_gouraud_triangle(buf, a, b, c);
            fill_gouraud_triangle(buf, b, d, c);
        }
    }
}

/// Evaluate the Coons surface position at (u, v) from the 12 boundary control
/// points. Point order follows the PDF spec boundary: p1..p12 trace the four
/// cubic Bezier edges; corners are p1 (u0,v0), p4 (u0,v1), p7 (u1,v1),
/// p10 (u1,v0).
fn coons_point(p: &[(f64, f64)], u: f64, v: f64) -> (f64, f64) {
    // Boundary curves (each a cubic Bezier):
    //   C1 (v at u=0): p1 p2  p3  p4
    //   C2 (v at u=1): p10 p9 p8 p7   (reversed indices for direction)
    //   D1 (u at v=0): p1 p12 p11 p10
    //   D2 (u at v=1): p4 p5  p6  p7
    let c1 = bezier(p[0], p[1], p[2], p[3], v);
    let c2 = bezier(p[9], p[8], p[7], p[6], v);
    let d1 = bezier(p[0], p[11], p[10], p[9], u);
    let d2 = bezier(p[3], p[4], p[5], p[6], u);

    // Corners.
    let p00 = p[0];
    let p01 = p[3];
    let p11 = p[6];
    let p10 = p[9];

    // Coons surface = ruled(u) + ruled(v) - bilinear(corners).
    let sx = (1.0 - u) * c1.0 + u * c2.0 + (1.0 - v) * d1.0 + v * d2.0
        - ((1.0 - u) * (1.0 - v) * p00.0
            + (1.0 - u) * v * p01.0
            + u * (1.0 - v) * p10.0
            + u * v * p11.0);
    let sy = (1.0 - u) * c1.1 + u * c2.1 + (1.0 - v) * d1.1 + v * d2.1
        - ((1.0 - u) * (1.0 - v) * p00.1
            + (1.0 - u) * v * p01.1
            + u * (1.0 - v) * p10.1
            + u * v * p11.1);
    (sx, sy)
}

/// Cubic Bezier interpolation of four control points at parameter t.
fn bezier(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64), t: f64) -> (f64, f64) {
    let mt = 1.0 - t;
    let a = mt * mt * mt;
    let b = 3.0 * mt * mt * t;
    let c = 3.0 * mt * t * t;
    let d = t * t * t;
    (
        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
    )
}

/// Bilinearly interpolate the 4 patch corner colors. Corner order: c0 at
/// (u0,v0), c1 at (u0,v1), c2 at (u1,v1), c3 at (u1,v0).
fn bilerp_color(c: &[RenderColor], u: f64, v: f64) -> RenderColor {
    let channel = |select: fn(RenderColor) -> f32| -> f32 {
        let top = (1.0 - v) * select(c[0]) as f64 + v * select(c[1]) as f64;
        let bot = (1.0 - v) * select(c[3]) as f64 + v * select(c[2]) as f64;
        ((1.0 - u) * top + u * bot) as f32
    };
    RenderColor::new(
        channel(|color| color.r),
        channel(|color| color.g),
        channel(|color| color.b),
        channel(|color| color.a),
    )
}

/// Rasterize a Gouraud-shaded triangle into `buf` with barycentric color
/// interpolation. Honors the buffer's clip and blends with full coverage.
fn fill_gouraud_triangle(buf: &mut PixelBuffer, v0: MeshVertex, v1: MeshVertex, v2: MeshVertex) {
    let min_x = v0.dx.min(v1.dx).min(v2.dx).floor().max(0.0) as i32;
    let max_x = (v0.dx.max(v1.dx).max(v2.dx).ceil() as i32).min(buf.width as i32 - 1);
    let min_y = v0.dy.min(v1.dy).min(v2.dy).floor().max(0.0) as i32;
    let max_y = (v0.dy.max(v1.dy).max(v2.dy).ceil() as i32).min(buf.height as i32 - 1);
    if max_x < min_x || max_y < min_y {
        return;
    }

    let area = edge(v0.dx, v0.dy, v1.dx, v1.dy, v2.dx, v2.dy);
    if area.abs() < 1e-9 {
        return; // degenerate
    }
    let dither = buf.render_mode().is_high_quality();

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if !buf.clip_allows(px, py) {
                continue;
            }
            let fx = px as f64 + 0.5;
            let fy = py as f64 + 0.5;
            let w0 = edge(v1.dx, v1.dy, v2.dx, v2.dy, fx, fy) / area;
            let w1 = edge(v2.dx, v2.dy, v0.dx, v0.dy, fx, fy) / area;
            let w2 = edge(v0.dx, v0.dy, v1.dx, v1.dy, fx, fy) / area;
            // Inside test with a small epsilon to avoid seams between triangles.
            if w0 < -1e-6 || w1 < -1e-6 || w2 < -1e-6 {
                continue;
            }
            let r = w0 * v0.color.r as f64 + w1 * v1.color.r as f64 + w2 * v2.color.r as f64;
            let g = w0 * v0.color.g as f64 + w1 * v1.color.g as f64 + w2 * v2.color.g as f64;
            let b = w0 * v0.color.b as f64 + w1 * v1.color.b as f64 + w2 * v2.color.b as f64;
            let a = w0 * v0.color.a as f64 + w1 * v1.color.a as f64 + w2 * v2.color.a as f64;
            let color = quantize_shading_color(
                RenderColor::new(r as f32, g as f32, b as f32, a as f32),
                px,
                py,
                dither,
            );
            buf.blend_pixel(px, py, color, 1.0);
        }
    }
}

/// Signed area of the triangle (a, b, c) doubled (the edge function).
fn edge(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64) -> f64 {
    (cx - ax) * (by - ay) - (cy - ay) * (bx - ax)
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
        let reader = crate::reader::PdfReader::from_bytes(super::tests_minimal_pdf()).unwrap();
        let r = eval_type3(&d, 0.5, &reader);
        assert!((r[0] - 0.5).abs() < 0.01, "Type3->Type2 at 0.5: {:?}", r);
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
        assert_eq!(black, [35, 31, 32, 255]);
        let white = components_to_pixel(&[0.0, 0.0, 0.0, 0.0], "DeviceCMYK");
        assert_eq!(white, [255, 255, 255, 255]);
    }

    #[test]
    fn shading_color_cache_keeps_sub_byte_gradient_steps() {
        let func = PdfObject::Dictionary(make_type2_dict(&[0.0], &[1.0], 1.0));
        let reader = crate::reader::PdfReader::from_bytes(super::tests_minimal_pdf()).unwrap();
        let mut cache = ShadingColorCache::new();

        let c0 =
            ShadingRenderer::color_for(0.1000, 0.0, 1.0, &func, "DeviceGray", &reader, &mut cache)
                .expect("color at first sample");
        let c1 =
            ShadingRenderer::color_for(0.1010, 0.0, 1.0, &func, "DeviceGray", &reader, &mut cache)
                .expect("color at nearby sample");

        assert!(
            c1.r > c0.r && (c1.r - c0.r) > 0.0005,
            "cache should preserve sub-byte progression: {c0:?} -> {c1:?}"
        );
    }

    fn longest_run(values: &[u8]) -> usize {
        let mut longest = 0usize;
        let mut current = 0usize;
        let mut previous = None;
        for &value in values {
            if previous == Some(value) {
                current += 1;
            } else {
                current = 1;
                previous = Some(value);
            }
            longest = longest.max(current);
        }
        longest
    }

    #[test]
    fn ordered_dither_breaks_long_quantization_runs_and_is_deterministic() {
        const W: i32 = 1024;
        let plain: Vec<u8> = (0..W)
            .map(|x| {
                let t = 0.49 + 0.02 * x as f32 / (W - 1) as f32;
                quantize_shading_color(RenderColor::gray(t), x, 0, false)[0]
            })
            .collect();
        let dithered: Vec<u8> = (0..W)
            .map(|x| {
                let t = 0.49 + 0.02 * x as f32 / (W - 1) as f32;
                quantize_shading_color(RenderColor::gray(t), x, 0, true)[0]
            })
            .collect();
        let dithered_again: Vec<u8> = (0..W)
            .map(|x| {
                let t = 0.49 + 0.02 * x as f32 / (W - 1) as f32;
                quantize_shading_color(RenderColor::gray(t), x, 0, true)[0]
            })
            .collect();

        assert_eq!(dithered, dithered_again);
        assert!(
            longest_run(&plain) > 100,
            "undithered shallow gradient should visibly band"
        );
        assert!(
            longest_run(&dithered) < longest_run(&plain) / 4,
            "dithered gradient should break long runs: plain {}, dithered {}",
            longest_run(&plain),
            longest_run(&dithered)
        );
    }

    #[test]
    fn ordered_dither_does_not_texture_exact_byte_flat_colors() {
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    quantize_shading_color(RenderColor::gray(128.0 / 255.0), x, y, true),
                    [128, 128, 128, 255]
                );
            }
        }
    }

    // ---- Coons/tensor shared-edge patch reconstruction ---------------------

    fn rc(r: f32, g: f32, b: f32, a: f32) -> RenderColor {
        RenderColor::new(r, g, b, a)
    }

    /// Helper: a deterministic 12-point "previous" patch where point i is
    /// (i*10, i*10) and corner colors are distinguishable. Lets us assert exact
    /// index reuse for each flag.
    fn prev_patch() -> (Vec<(f64, f64)>, Vec<RenderColor>) {
        let pts: Vec<(f64, f64)> = (0..12)
            .map(|i| (i as f64 * 10.0, i as f64 * 10.0))
            .collect();
        // 4 corner colors, each tagged in its red channel by its index.
        let cols: Vec<RenderColor> = (0..4).map(|i| rc(i as f32 / 10.0, 0.0, 0.0, 1.0)).collect();
        (pts, cols)
    }

    #[test]
    fn assemble_patch_flag0_is_independent() {
        // Flag 0: all 12 points and 4 colors come straight from the stream.
        let new_pts: Vec<(f64, f64)> = (0..12).map(|i| (i as f64, 0.0)).collect();
        let new_cols: Vec<RenderColor> = (0..4).map(|_| rc(0.0, 0.0, 0.0, 1.0)).collect();
        let (pts, cols) =
            assemble_patch(0, &new_pts, &new_cols, &[], &[], 6).expect("flag 0 must assemble");
        assert_eq!(pts.len(), 12);
        assert_eq!(cols.len(), 4);
        assert_eq!(pts[0], (0.0, 0.0));
        assert_eq!(pts[11], (11.0, 0.0));
    }

    #[test]
    fn assemble_patch_flag1_reuses_edge_p4_p7_and_colors_c2_c3() {
        let (pp, pc) = prev_patch();
        // 8 new boundary points (p5..p12) + 2 new colors (c3, c4).
        let new_pts: Vec<(f64, f64)> = (0..8).map(|i| (100.0 + i as f64, -1.0)).collect();
        let new_cols = vec![rc(0.7, 0.0, 0.0, 1.0), rc(0.8, 0.0, 0.0, 1.0)];
        let (pts, cols) =
            assemble_patch(1, &new_pts, &new_cols, &pp, &pc, 6).expect("flag 1 must assemble");
        // Shared edge p1..p4 = prev indices [3,4,5,6].
        assert_eq!(pts[0], pp[3]);
        assert_eq!(pts[1], pp[4]);
        assert_eq!(pts[2], pp[5]);
        assert_eq!(pts[3], pp[6]);
        // Remaining 8 are the new points.
        assert_eq!(pts[4], new_pts[0]);
        assert_eq!(pts[11], new_pts[7]);
        // Shared colors c1,c2 = prev colors [1,2]; new colors fill c3,c4.
        assert_eq!(cols[0], pc[1]);
        assert_eq!(cols[1], pc[2]);
        assert_eq!(cols[2], new_cols[0]);
        assert_eq!(cols[3], new_cols[1]);
    }

    #[test]
    fn assemble_patch_flag2_reuses_edge_p7_p10_and_colors_c3_c4() {
        let (pp, pc) = prev_patch();
        let new_pts: Vec<(f64, f64)> = (0..8).map(|i| (200.0 + i as f64, -2.0)).collect();
        let new_cols = vec![rc(0.6, 0.0, 0.0, 1.0), rc(0.9, 0.0, 0.0, 1.0)];
        let (pts, cols) =
            assemble_patch(2, &new_pts, &new_cols, &pp, &pc, 6).expect("flag 2 must assemble");
        assert_eq!(pts[0], pp[6]);
        assert_eq!(pts[1], pp[7]);
        assert_eq!(pts[2], pp[8]);
        assert_eq!(pts[3], pp[9]);
        assert_eq!(cols[0], pc[2]);
        assert_eq!(cols[1], pc[3]);
    }

    #[test]
    fn assemble_patch_flag3_reuses_edge_p10_p1_and_colors_c4_c1() {
        let (pp, pc) = prev_patch();
        let new_pts: Vec<(f64, f64)> = (0..8).map(|i| (300.0 + i as f64, -3.0)).collect();
        let new_cols = vec![rc(0.5, 0.0, 0.0, 1.0), rc(0.4, 0.0, 0.0, 1.0)];
        let (pts, cols) =
            assemble_patch(3, &new_pts, &new_cols, &pp, &pc, 6).expect("flag 3 must assemble");
        // Shared edge p1..p4 = prev indices [9,10,11,0] (wraps to p1).
        assert_eq!(pts[0], pp[9]);
        assert_eq!(pts[1], pp[10]);
        assert_eq!(pts[2], pp[11]);
        assert_eq!(pts[3], pp[0]);
        assert_eq!(cols[0], pc[3]);
        assert_eq!(cols[1], pc[0]);
    }

    #[test]
    fn assemble_tensor_patch_drops_interior_points() {
        // Tensor flag 0: 16 stream points; only the first 12 boundary points are
        // kept (interior points 12..15 are dropped by the Coons surface).
        let new_pts: Vec<(f64, f64)> = (0..16).map(|i| (i as f64, 0.0)).collect();
        let new_cols: Vec<RenderColor> = (0..4).map(|_| rc(0.0, 0.0, 0.0, 1.0)).collect();
        let (pts, _cols) =
            assemble_patch(0, &new_pts, &new_cols, &[], &[], 7).expect("tensor flag 0");
        assert_eq!(pts.len(), 12);
        assert_eq!(pts[11], (11.0, 0.0)); // p12, not p16
    }

    #[test]
    fn assemble_tensor_flag1_shares_edge_uses_12_new_points() {
        // Tensor flagged patch reads 12 new points (8 boundary p5..p12 + 4
        // interior); only the 8 boundary ones are appended after the shared edge.
        let (pp, pc) = prev_patch();
        let new_pts: Vec<(f64, f64)> = (0..12).map(|i| (100.0 + i as f64, -1.0)).collect();
        let new_cols = vec![rc(0.7, 0.0, 0.0, 1.0), rc(0.8, 0.0, 0.0, 1.0)];
        let (pts, cols) =
            assemble_patch(1, &new_pts, &new_cols, &pp, &pc, 7).expect("tensor flag 1");
        assert_eq!(pts.len(), 12);
        assert_eq!(pts[0], pp[3]); // shared edge
        assert_eq!(pts[4], new_pts[0]); // first new boundary point
        assert_eq!(pts[11], new_pts[7]); // 8th new boundary point (interior dropped)
        assert_eq!(cols[0], pc[1]);
    }
}
