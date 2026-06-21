//! OpenType **variable font** (OTVar) instance selection.
//!
//! A variable font stores a continuum of styles in one program; a concrete
//! *instance* is chosen by coordinates along design axes (`wght`, `wdth`,
//! `slnt`, `ital`, `opsz`, or custom axes), and glyph outlines are interpolated
//! from per-master deltas. This module decides **which instance** to render and
//! applies it to a [`ttf_parser::Face`]; the crate then produces the
//! interpolated outline (gvar / CFF2), avar-normalized coordinates, and
//! variation-adjusted metrics (HVAR / MVAR) automatically — we only select
//! coordinates and route the result to the existing rasterizer.
//!
//! # How the instance is selected
//!
//! PDF has **no** standard channel to pass arbitrary axis coordinates to an
//! embedded font program, so in practice the instance comes from one of:
//!
//! 1. **Pre-instanced** — the producer flattened the variable font to a static
//!    instance before embedding (no `fvar` table). This is overwhelmingly the
//!    common case (the entire local corpus is pre-instanced). Such fonts are not
//!    variable and render unchanged through the static path.
//! 2. **Default instance** — a true variable font embedded as-is. `ttf-parser`
//!    initializes coordinates to the font's default (all normalized to 0), so
//!    the default instance already renders correctly with no action.
//! 3. **PDF-descriptor-selected** — the `FontDescriptor` carries `/FontWeight`
//!    (100–900) and/or `/FontStretch` (a name like `/Condensed`). When the
//!    embedded font is variable and exposes a matching `wght`/`wdth` axis, those
//!    descriptor values select the instance — the one case where Oxide can
//!    honor a *non-default* instance from PDF metadata. This is the genuine
//!    correctness win this module adds.
//!
//! Determinism: identical (font bytes, request) → identical coordinates →
//! identical outline. No allocation beyond the request's small axis list; all
//! coordinate values are clamped to each axis's `[min, max]` by `ttf-parser`.

use ttf_parser::{Face, Tag};

/// `wght` axis (CSS weight 1–1000; PDF `/FontWeight` 100–900).
pub const AXIS_WGHT: Tag = Tag::from_bytes(b"wght");
/// `wdth` axis (width percentage; 100 = normal).
pub const AXIS_WDTH: Tag = Tag::from_bytes(b"wdth");

/// One axis coordinate to pin (user-space value, not normalized).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisValue {
    pub tag: Tag,
    pub value: f32,
}

/// The intended variable-font instance: a small set of axis coordinates to pin.
/// An empty request means "render the font's default instance" (the no-op case).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VariationRequest {
    axes: Vec<AxisValue>,
}

impl VariationRequest {
    /// A request that pins nothing — the default instance.
    pub fn none() -> Self {
        Self { axes: Vec::new() }
    }

    /// True if this request pins no axes (the default instance / no-op).
    pub fn is_empty(&self) -> bool {
        self.axes.is_empty()
    }

    /// Pin an axis to a user-space value (overwrites a prior value for the tag).
    pub fn with_axis(mut self, tag: Tag, value: f32) -> Self {
        if let Some(existing) = self.axes.iter_mut().find(|a| a.tag == tag) {
            existing.value = value;
        } else {
            self.axes.push(AxisValue { tag, value });
        }
        self
    }

    /// The pinned axes.
    pub fn axes(&self) -> &[AxisValue] {
        &self.axes
    }

    /// A stable, order-independent hash of the pinned coordinates, for use as
    /// part of a glyph-cache key (so two instances of the same font program do
    /// not collide). `0` for the empty request (the default instance), keeping
    /// the cache key identical to the pre-variation behaviour for static fonts.
    pub fn cache_hash(&self) -> u64 {
        if self.axes.is_empty() {
            return 0;
        }
        // Sort by tag so the hash is independent of insertion order.
        let mut sorted = self.axes.clone();
        sorted.sort_by_key(|a| a.tag.0);
        // FNV-1a over (tag, value-bits) pairs.
        let mut h: u64 = 0xcbf29ce484222325;
        let mut mix = |x: u64| {
            for byte in x.to_le_bytes() {
                h ^= u64::from(byte);
                h = h.wrapping_mul(0x100000001b3);
            }
        };
        for av in &sorted {
            mix(u64::from(av.tag.0));
            mix(u64::from(av.value.to_bits()));
        }
        h
    }

    /// Build a request from PDF `FontDescriptor` values: `/FontWeight` → `wght`,
    /// `/FontStretch` → `wdth`. Returns an empty request when neither is present
    /// (or both are at their normal/default values, so nothing needs pinning).
    pub fn from_descriptor(font_weight: Option<f64>, font_stretch: Option<&str>) -> Self {
        let mut req = Self::none();
        // `/FontWeight`: 100..900 (PDF). Only pin when it is a sane non-normal
        // value — 400 is "normal" and equals most fonts' default, so pinning it
        // is a harmless no-op we skip to keep the default path byte-identical.
        if let Some(w) = font_weight {
            if w.is_finite() && (1.0..=1000.0).contains(&w) && (w - 400.0).abs() > f64::EPSILON {
                req = req.with_axis(AXIS_WGHT, w as f32);
            }
        }
        if let Some(stretch) = font_stretch {
            if let Some(pct) = font_stretch_percent(stretch) {
                if (pct - 100.0).abs() > f32::EPSILON {
                    req = req.with_axis(AXIS_WDTH, pct);
                }
            }
        }
        req
    }
}

/// Map a PDF `/FontStretch` name to a `wdth`-axis percentage (per the OpenType
/// `wdth` axis registration / CSS `font-stretch` keyword table).
pub fn font_stretch_percent(name: &str) -> Option<f32> {
    let n = name.trim_start_matches('/');
    Some(match n {
        "UltraCondensed" => 50.0,
        "ExtraCondensed" => 62.5,
        "Condensed" => 75.0,
        "SemiCondensed" => 87.5,
        "Normal" => 100.0,
        "SemiExpanded" => 112.5,
        "Expanded" => 125.0,
        "ExtraExpanded" => 150.0,
        "UltraExpanded" => 200.0,
        _ => return None,
    })
}

/// Whether the font program is an OpenType variable font (has an `fvar` table
/// with at least one axis). Bare-CFF / Type1 programs and pre-instanced static
/// fonts return `false`.
pub fn is_variable(font_bytes: &[u8]) -> bool {
    Face::parse(font_bytes, 0)
        .map(|f| f.is_variable())
        .unwrap_or(false)
}

/// The font's variation axes as `(tag, min, default, max)`, empty when not
/// variable. Useful for diagnostics and for clamping a request to real axes.
pub fn axes(font_bytes: &[u8]) -> Vec<(Tag, f32, f32, f32)> {
    let Ok(face) = Face::parse(font_bytes, 0) else {
        return Vec::new();
    };
    face.variation_axes()
        .into_iter()
        .map(|a| (a.tag, a.min_value, a.def_value, a.max_value))
        .collect()
}

/// Apply a [`VariationRequest`] to a mutable face. Only axes the font actually
/// exposes are set (others are ignored). Returns `true` if at least one axis was
/// applied (i.e. the face now renders a non-default instance).
///
/// `set_variation` clamps each value to the axis `[min, max]` and applies `avar`
/// normalization internally, so callers may pass raw user-space values.
pub fn apply_request(face: &mut Face, request: &VariationRequest) -> bool {
    if request.is_empty() || !face.is_variable() {
        return false;
    }
    // Which axes exist on this face.
    let available: Vec<Tag> = face.variation_axes().into_iter().map(|a| a.tag).collect();
    let mut applied = false;
    for av in request.axes() {
        if available.contains(&av.tag) && face.set_variation(av.tag, av.value).is_some() {
            applied = true;
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_normal_weight_is_noop() {
        let req = VariationRequest::from_descriptor(Some(400.0), Some("Normal"));
        assert!(req.is_empty(), "weight 400 + Normal stretch => no pinning");
    }

    #[test]
    fn descriptor_bold_pins_wght() {
        let req = VariationRequest::from_descriptor(Some(700.0), None);
        assert_eq!(req.axes(), &[AxisValue { tag: AXIS_WGHT, value: 700.0 }]);
    }

    #[test]
    fn descriptor_condensed_pins_wdth() {
        let req = VariationRequest::from_descriptor(None, Some("Condensed"));
        assert_eq!(req.axes(), &[AxisValue { tag: AXIS_WDTH, value: 75.0 }]);
    }

    #[test]
    fn descriptor_bold_condensed_pins_both() {
        let req = VariationRequest::from_descriptor(Some(800.0), Some("/SemiCondensed"));
        assert_eq!(
            req.axes(),
            &[
                AxisValue { tag: AXIS_WGHT, value: 800.0 },
                AxisValue { tag: AXIS_WDTH, value: 87.5 },
            ]
        );
    }

    #[test]
    fn out_of_range_weight_ignored() {
        assert!(VariationRequest::from_descriptor(Some(0.0), None).is_empty());
        assert!(VariationRequest::from_descriptor(Some(5000.0), None).is_empty());
        assert!(VariationRequest::from_descriptor(Some(f64::NAN), None).is_empty());
    }

    #[test]
    fn unknown_stretch_ignored() {
        assert!(VariationRequest::from_descriptor(None, Some("Wonky")).is_empty());
    }

    #[test]
    fn with_axis_overwrites() {
        let req = VariationRequest::none()
            .with_axis(AXIS_WGHT, 300.0)
            .with_axis(AXIS_WGHT, 700.0);
        assert_eq!(req.axes(), &[AxisValue { tag: AXIS_WGHT, value: 700.0 }]);
    }

    #[test]
    fn non_variable_bytes_report_false() {
        // A non-font byte blob is not variable.
        assert!(!is_variable(b"not a font"));
        assert!(axes(b"not a font").is_empty());
    }
}
