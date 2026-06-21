//! Resolution of special PDF colour spaces — `/Separation` and `/DeviceN`
//! (spec §8.6.6.4) — into device RGB for the fill/stroke paint path.
//!
//! A `/Separation` colour space has one tint component; `/DeviceN` has N. Both
//! carry an *alternate* colour space (a normal space such as DeviceCMYK /
//! DeviceRGB / DeviceGray / ICCBased) and a *tint-transform function* that maps
//! the N tint values into the alternate space. To paint, we evaluate the tint
//! transform (Function Types 0/2/3/4, all supported by
//! [`crate::render::function`]) and run the resulting components through the
//! existing alternate-space → RGB conversion.
//!
//! The tint transform is evaluated with the PDF function machinery (Function
//! Types 0 and 4, in `render/function.rs`); this module wires the fill/stroke
//! colour path through it.

use crate::object::PdfObject;
use crate::reader::PdfReader;
use crate::render::cmm;
use crate::render::color::{ColorSpaceHandler, RenderColor};

/// Outcome of resolving a named colour space to a paint colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NamedColor {
    /// A concrete colour to paint with.
    Color(RenderColor),
    /// The colour produces no marks at all (e.g. `/Separation /None`); the
    /// caller must skip the paint operation entirely.
    NoPaint,
    /// The named space is not a Separation/DeviceN we can resolve (caller falls
    /// back to its existing behaviour).
    Unhandled,
}

/// Resolve a named colour-space *resource object* with the given tint
/// `components` into a paint colour.
///
/// `space_obj` is the already-resolved `/ColorSpace` resource entry (an array
/// like `[/Separation /Name altSpace tintFn]` or
/// `[/DeviceN [names] altSpace tintFn ...]`, or a bare family name). `reader`
/// resolves the alternate-space and tint-function indirect references.
pub fn resolve_named_color(
    space_obj: &PdfObject,
    components: &[f64],
    alpha: f32,
    reader: &PdfReader,
) -> NamedColor {
    let arr = match space_obj {
        PdfObject::Array(arr) => arr.as_slice(),
        // A bare name that isn't a device space is not something we resolve here.
        _ => return NamedColor::Unhandled,
    };
    let family = match arr.first().and_then(PdfObject::as_name) {
        Some(name) => name,
        None => return NamedColor::Unhandled,
    };

    match family {
        "Separation" => resolve_separation(arr, components, alpha, reader),
        "DeviceN" => resolve_device_n(arr, components, alpha, reader),
        "ICCBased" => cmm::icc_components_to_srgb(space_obj, components, reader)
            .map(|[r, g, b]| NamedColor::Color(RenderColor::new(r, g, b, alpha)))
            .unwrap_or(NamedColor::Unhandled),
        "Lab" => {
            let params = cmm::lab_params_from_space(space_obj, Some(reader)).unwrap_or_default();
            let l = components.first().copied().unwrap_or(0.0) as f32;
            let a = components.get(1).copied().unwrap_or(0.0) as f32;
            let b = components.get(2).copied().unwrap_or(0.0) as f32;
            let [r, g, b] = cmm::lab_to_srgb(l, a, b, params);
            NamedColor::Color(RenderColor::new(r, g, b, alpha))
        }
        "CalGray" => {
            let params =
                cmm::cal_gray_params_from_space(space_obj, Some(reader)).unwrap_or_default();
            let gray = components.first().copied().unwrap_or(0.0) as f32;
            let [r, g, b] = cmm::cal_gray_to_srgb(gray, params);
            NamedColor::Color(RenderColor::new(r, g, b, alpha))
        }
        "CalRGB" => {
            let params =
                cmm::cal_rgb_params_from_space(space_obj, Some(reader)).unwrap_or_default();
            let comps = [
                components.first().copied().unwrap_or(0.0) as f32,
                components.get(1).copied().unwrap_or(0.0) as f32,
                components.get(2).copied().unwrap_or(0.0) as f32,
            ];
            let [r, g, b] = cmm::cal_rgb_to_srgb(comps, params);
            NamedColor::Color(RenderColor::new(r, g, b, alpha))
        }
        // Indexed and other color spaces fall through to the component-count
        // heuristic elsewhere.
        _ => NamedColor::Unhandled,
    }
}

/// `[/Separation /Name altSpace tintTransform]`
fn resolve_separation(
    arr: &[PdfObject],
    components: &[f64],
    alpha: f32,
    reader: &PdfReader,
) -> NamedColor {
    // Colorant name: /None paints nothing; /All approximates as full ink.
    let colorant = arr.get(1).and_then(PdfObject::as_name);
    if colorant == Some("None") {
        return NamedColor::NoPaint;
    }
    let alt = match arr.get(2) {
        Some(obj) => obj,
        None => return NamedColor::Unhandled,
    };
    let tint_fn = match arr.get(3) {
        Some(obj) => obj,
        None => return NamedColor::Unhandled,
    };

    // /All: a single colorant covering every separation. Approximate as the
    // alternate-space result of full ink (tint = 1.0), which is the conventional
    // near-black/maximum-density rendering.
    let tint = if colorant == Some("All") {
        1.0
    } else {
        components.first().copied().unwrap_or(1.0)
    };

    let alt_components = crate::render::function::eval_function_n(tint_fn, &[tint], reader);
    if alt_components.is_empty() {
        return NamedColor::Unhandled;
    }
    let alt_name = alternate_space_name(alt, reader);
    NamedColor::Color(ColorSpaceHandler::from_components(
        &alt_name,
        &alt_components,
        alpha,
    ))
}

/// `[/DeviceN [/Name1 /Name2 ...] altSpace tintTransform attributes?]`
fn resolve_device_n(
    arr: &[PdfObject],
    components: &[f64],
    alpha: f32,
    reader: &PdfReader,
) -> NamedColor {
    let names = match arr.get(1).and_then(PdfObject::as_array) {
        Some(n) => n,
        None => return NamedColor::Unhandled,
    };
    // If every colorant is /None, the space produces no marks.
    if !names.is_empty() && names.iter().all(|n| n.as_name() == Some("None")) {
        return NamedColor::NoPaint;
    }
    let alt = match arr.get(2) {
        Some(obj) => obj,
        None => return NamedColor::Unhandled,
    };
    let tint_fn = match arr.get(3) {
        Some(obj) => obj,
        None => return NamedColor::Unhandled,
    };

    // Feed all N tint components through the multi-input tint transform.
    let n = names.len().max(1);
    let mut tints: Vec<f64> = components.iter().copied().take(n).collect();
    while tints.len() < n {
        tints.push(1.0);
    }
    let alt_components = crate::render::function::eval_function_n(tint_fn, &tints, reader);
    if alt_components.is_empty() {
        return NamedColor::Unhandled;
    }
    let alt_name = alternate_space_name(alt, reader);
    NamedColor::Color(ColorSpaceHandler::from_components(
        &alt_name,
        &alt_components,
        alpha,
    ))
}

/// Map an alternate colour-space object to the family name understood by
/// [`ColorSpaceHandler::from_components`]. ICCBased is reduced to a device space
/// by its component count (`/N`) when the full ICC stream is unavailable in this
/// spot-color shortcut.
fn alternate_space_name(alt: &PdfObject, reader: &PdfReader) -> String {
    let resolved = match alt {
        PdfObject::Reference { .. } => reader.resolve(alt.clone()).unwrap_or_else(|_| alt.clone()),
        other => other.clone(),
    };
    match &resolved {
        PdfObject::Name(name) => name.clone(),
        PdfObject::Array(arr) => {
            let head = arr
                .first()
                .and_then(PdfObject::as_name)
                .unwrap_or("DeviceRGB");
            if head == "ICCBased" {
                // Resolve the stream's /N to pick the device space.
                let n = arr
                    .get(1)
                    .and_then(|s| reader.resolve(s.clone()).ok())
                    .and_then(|obj| match obj {
                        PdfObject::Stream { dict, .. } => dict.get_integer("N"),
                        _ => None,
                    })
                    .unwrap_or(3);
                match n {
                    1 => "DeviceGray".to_string(),
                    4 => "DeviceCMYK".to_string(),
                    _ => "DeviceRGB".to_string(),
                }
            } else {
                head.to_string()
            }
        }
        _ => "DeviceRGB".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{PdfDictionary, PdfObject};
    use std::collections::BTreeMap;

    fn reader() -> PdfReader {
        PdfReader::from_bytes(crate::render::shading::tests_minimal_pdf()).unwrap()
    }

    fn name(s: &str) -> PdfObject {
        PdfObject::Name(s.to_string())
    }

    fn real_arr(vals: &[f64]) -> PdfObject {
        PdfObject::Array(vals.iter().map(|&v| PdfObject::Real(v)).collect())
    }

    /// Type 2 tint transform: C0 -> C1 in the alternate space.
    fn type2_fn(c0: &[f64], c1: &[f64]) -> PdfObject {
        let mut m: BTreeMap<String, PdfObject> = BTreeMap::new();
        m.insert("FunctionType".into(), PdfObject::Integer(2));
        m.insert("Domain".into(), real_arr(&[0.0, 1.0]));
        m.insert("C0".into(), real_arr(c0));
        m.insert("C1".into(), real_arr(c1));
        m.insert("N".into(), PdfObject::Real(1.0));
        PdfObject::Dictionary(PdfDictionary::new(m))
    }

    #[test]
    fn separation_tint0_is_white_cmyk() {
        // /Separation spot -> DeviceCMYK, tint 0 = all-zero CMYK = white.
        let space = PdfObject::Array(vec![
            name("Separation"),
            name("PANTONE 286 C"),
            name("DeviceCMYK"),
            type2_fn(&[0.0, 0.0, 0.0, 0.0], &[1.0, 0.5, 0.0, 0.2]),
        ]);
        let r = resolve_named_color(&space, &[0.0], 1.0, &reader());
        match r {
            NamedColor::Color(c) => {
                assert!((c.r - 1.0).abs() < 0.01, "white R: {}", c.r);
                assert!((c.g - 1.0).abs() < 0.01, "white G: {}", c.g);
                assert!((c.b - 1.0).abs() < 0.01, "white B: {}", c.b);
            }
            other => panic!("expected Color, got {other:?}"),
        }
    }

    #[test]
    fn separation_tint1_is_alt_cmyk_full() {
        // tint 1 -> C1 = CMYK(1, 0.5, 0, 0.2), resolved through the shared
        // Poppler-like DeviceCMYK fallback.
        let space = PdfObject::Array(vec![
            name("Separation"),
            name("PANTONE 286 C"),
            name("DeviceCMYK"),
            type2_fn(&[0.0, 0.0, 0.0, 0.0], &[1.0, 0.5, 0.0, 0.2]),
        ]);
        let r = resolve_named_color(&space, &[1.0], 1.0, &reader());
        match r {
            NamedColor::Color(c) => {
                assert!(
                    (c.r - 0.10).abs() < 0.03,
                    "R near process fallback: {}",
                    c.r
                );
                assert!(
                    (c.g - 0.37).abs() < 0.03,
                    "G near process fallback: {}",
                    c.g
                );
                assert!(
                    (c.b - 0.63).abs() < 0.03,
                    "B near process fallback: {}",
                    c.b
                );
            }
            other => panic!("expected Color, got {other:?}"),
        }
    }

    #[test]
    fn separation_none_produces_no_paint() {
        let space = PdfObject::Array(vec![
            name("Separation"),
            name("None"),
            name("DeviceCMYK"),
            type2_fn(&[0.0, 0.0, 0.0, 0.0], &[0.0, 0.0, 0.0, 1.0]),
        ]);
        assert_eq!(
            resolve_named_color(&space, &[1.0], 1.0, &reader()),
            NamedColor::NoPaint
        );
    }

    #[test]
    fn separation_all_uses_full_ink() {
        // /All approximates tint=1 regardless of the supplied component.
        let space = PdfObject::Array(vec![
            name("Separation"),
            name("All"),
            name("DeviceGray"),
            // gray 1.0 at tint 0 -> 0.0 at tint 1 (so full ink = black)
            type2_fn(&[1.0], &[0.0]),
        ]);
        // Even with component 0.0 supplied, /All forces tint 1 -> gray 0 = black.
        match resolve_named_color(&space, &[0.0], 1.0, &reader()) {
            NamedColor::Color(c) => assert!(c.r < 0.01, "All -> full ink (black): {}", c.r),
            other => panic!("expected Color, got {other:?}"),
        }
    }

    #[test]
    fn device_n_two_inputs_feed_tint_transform() {
        // DeviceN with 2 colorants -> DeviceRGB via a Type 4 transform that maps
        // [a b] -> [a, b, 0]. With inputs [0.25, 0.75] -> RGB(0.25, 0.75, 0).
        let mut m: BTreeMap<String, PdfObject> = BTreeMap::new();
        m.insert("FunctionType".into(), PdfObject::Integer(4));
        m.insert("Domain".into(), real_arr(&[0.0, 1.0, 0.0, 1.0]));
        m.insert("Range".into(), real_arr(&[0.0, 1.0, 0.0, 1.0, 0.0, 1.0]));
        let program = b"{ 0 }".to_vec(); // a b -> a b 0  (push a 0)
        let tint_fn = PdfObject::Stream {
            dict: PdfDictionary::new({
                let mut mm = m.clone();
                mm.insert("Length".into(), PdfObject::Integer(program.len() as i64));
                mm
            }),
            raw: program,
        };
        let space = PdfObject::Array(vec![
            name("DeviceN"),
            PdfObject::Array(vec![name("Spot1"), name("Spot2")]),
            name("DeviceRGB"),
            tint_fn,
        ]);
        match resolve_named_color(&space, &[0.25, 0.75], 1.0, &reader()) {
            NamedColor::Color(c) => {
                assert!((c.r - 0.25).abs() < 0.02, "R~0.25: {}", c.r);
                assert!((c.g - 0.75).abs() < 0.02, "G~0.75: {}", c.g);
                assert!(c.b < 0.02, "B~0: {}", c.b);
            }
            other => panic!("expected Color, got {other:?}"),
        }
    }

    #[test]
    fn non_special_space_is_unhandled() {
        let space = PdfObject::Array(vec![name("ICCBased")]);
        assert_eq!(
            resolve_named_color(&space, &[0.5], 1.0, &reader()),
            NamedColor::Unhandled
        );
    }
}
