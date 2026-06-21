//! OpenType variable-font (OTVar) support: instance selection + interpolated
//! outlines and metrics, exercised through the **production** code path
//! (`oxide_engine::fonts::variations` + `render::glyph_outline::*_var`).
//!
//! Because the local PDF corpus contains **no** embedded variable fonts (every
//! variable font that reaches these PDFs was pre-instanced by the producer), the
//! interpolation machinery is proven here against a self-contained synthetic
//! variable TrueType font built in-memory: one `wght` axis (100..400..900), a
//! square glyph whose `gvar` deltas expand it at max weight, and an `HVAR` table
//! that grows the advance with weight. This is deterministic and pure-Rust — no
//! external font, no network.
//!
//! Coverage:
//!   - the font is detected as variable and its axis is read (`variations::axes`);
//!   - two different `wght` coordinates produce DIFFERENT outlines (interpolation
//!     actually happens) and DIFFERENT advances (HVAR metrics variation);
//!   - the default instance (empty request) is byte-identical to no variation;
//!   - determinism: same coordinates → same outline + advance;
//!   - `VariationRequest::from_descriptor` maps PDF `/FontWeight` + `/FontStretch`.

use oxide_engine::fonts::variations::{self, VariationRequest, AXIS_WGHT};
use oxide_engine::render::glyph_outline::extract_glyph_path_for_simple_var;
use oxide_engine::render::path::PathSegment;

mod synthfont;
use synthfont::build_weight_variable_font;

/// Axis-aligned bounds of a glyph's path segments (font units).
fn path_bounds(segments: &[PathSegment]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut acc = |x: f64, y: f64| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for seg in segments {
        match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => acc(*x, *y),
            PathSegment::CubicTo {
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x,
                y,
            } => {
                acc(*cp1x, *cp1y);
                acc(*cp2x, *cp2y);
                acc(*x, *y);
            }
            PathSegment::ClosePath => {}
        }
    }
    (min_x, min_y, max_x, max_y)
}

fn area(b: (f64, f64, f64, f64)) -> f64 {
    (b.2 - b.0).max(0.0) * (b.3 - b.1).max(0.0)
}

/// Extract the synthetic glyph ('A' -> gid 1) at a given weight via the
/// production variable-font outline path.
fn glyph_at_weight(font: &[u8], wght: f32) -> (Vec<PathSegment>, f64) {
    let req = if (wght - 400.0).abs() < f32::EPSILON {
        VariationRequest::none()
    } else {
        VariationRequest::none().with_axis(AXIS_WGHT, wght)
    };
    let (path, advance) = extract_glyph_path_for_simple_var(font, 0x41, 'A', None, &req);
    (path.expect("glyph A should have an outline").segments, advance)
}

#[test]
fn synthetic_font_is_detected_as_variable_with_wght_axis() {
    let font = build_weight_variable_font();
    assert!(variations::is_variable(&font), "synthetic font must be variable");
    let axes = variations::axes(&font);
    assert_eq!(axes.len(), 1, "one axis");
    let (tag, min, def, max) = axes[0];
    assert_eq!(tag, AXIS_WGHT);
    assert_eq!((min, def, max), (100.0, 400.0, 900.0));
}

#[test]
fn different_weights_interpolate_different_outlines() {
    let font = build_weight_variable_font();

    let (light_segs, _) = glyph_at_weight(&font, 400.0); // default
    let (heavy_segs, _) = glyph_at_weight(&font, 900.0); // max weight

    let light = path_bounds(&light_segs);
    let heavy = path_bounds(&heavy_segs);

    // The default instance is the un-expanded square (~100..400 on each axis).
    assert!((light.2 - light.0 - 300.0).abs() < 2.0, "default width ~300: {light:?}");

    // At max weight the gvar deltas expand the glyph: strictly larger area, and a
    // genuinely different point set (interpolation actually ran).
    assert!(
        area(heavy) > area(light) * 1.5,
        "heavy glyph must cover noticeably more area (light={:.0}, heavy={:.0})",
        area(light),
        area(heavy)
    );
    assert_ne!(light_segs, heavy_segs, "outline points must differ between weights");
}

#[test]
fn advance_reflects_hvar_metrics_variation() {
    let font = build_weight_variable_font();
    let (_, adv_default) = glyph_at_weight(&font, 400.0);
    let (_, adv_heavy) = glyph_at_weight(&font, 900.0);

    // Synthetic hmtx advance is 600 for gid1 (units/em 1000 => 600 per-mille).
    assert!((adv_default - 600.0).abs() < 1.0, "default advance ~600: {adv_default}");
    // HVAR adds +300 at peak weight => ~900 per-mille.
    assert!(
        adv_heavy > adv_default + 100.0,
        "advance must grow with weight via HVAR (default={adv_default}, heavy={adv_heavy})"
    );
}

#[test]
fn default_instance_equals_no_variation() {
    let font = build_weight_variable_font();
    // Empty request vs an explicit default-weight request: both render the
    // default instance, so the outline + advance must be identical.
    let (none_path, none_adv) =
        extract_glyph_path_for_simple_var(&font, 0x41, 'A', None, &VariationRequest::none());
    let (def_path, def_adv) = extract_glyph_path_for_simple_var(
        &font,
        0x41,
        'A',
        None,
        &VariationRequest::none().with_axis(AXIS_WGHT, 400.0),
    );
    assert_eq!(none_path.unwrap().segments, def_path.unwrap().segments);
    assert_eq!(none_adv, def_adv);
}

#[test]
fn interpolation_is_deterministic() {
    let font = build_weight_variable_font();
    let (a_segs, a_adv) = glyph_at_weight(&font, 700.0);
    let (b_segs, b_adv) = glyph_at_weight(&font, 700.0);
    assert_eq!(a_segs, b_segs, "same coords => same outline");
    assert_eq!(a_adv, b_adv, "same coords => same advance");
}

#[test]
fn intermediate_weight_is_between_extremes() {
    let font = build_weight_variable_font();
    let light = area(path_bounds(&glyph_at_weight(&font, 400.0).0));
    let mid = area(path_bounds(&glyph_at_weight(&font, 650.0).0));
    let heavy = area(path_bounds(&glyph_at_weight(&font, 900.0).0));
    assert!(
        light < mid && mid < heavy,
        "interpolation is monotonic in weight (light={light:.0}, mid={mid:.0}, heavy={heavy:.0})"
    );
}

#[test]
fn descriptor_maps_weight_and_stretch_to_axes() {
    // PDF FontDescriptor /FontWeight 700 -> wght 700; /FontStretch /Condensed -> wdth 75.
    let req = VariationRequest::from_descriptor(Some(700.0), Some("Condensed"));
    let tags: Vec<_> = req.axes().iter().map(|a| (a.tag, a.value)).collect();
    assert!(tags.contains(&(AXIS_WGHT, 700.0)));
    assert!(tags.contains(&(variations::AXIS_WDTH, 75.0)));

    // The bold descriptor, applied to the synthetic wght font, yields the heavy
    // instance (wdth is ignored since the font has no wdth axis).
    let font = build_weight_variable_font();
    let (_, adv_default) = glyph_at_weight(&font, 400.0);
    let bold = VariationRequest::from_descriptor(Some(900.0), None);
    let (_, adv_bold) = extract_glyph_path_for_simple_var(&font, 0x41, 'A', None, &bold);
    assert!(
        adv_bold > adv_default + 100.0,
        "descriptor-selected bold instance must use the heavier advance"
    );
}
