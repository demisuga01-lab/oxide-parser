//! Shared glyph-outline extraction used by both the raster renderer and the
//! vector (SVG) renderer.
//!
//! Both backends must turn a glyph (by Unicode char for simple fonts, or by
//! glyph id for CID fonts) into the SAME outline [`Path`] in font units, so
//! that vector output is visually identical to raster output. These free
//! functions mirror the extraction the raster path uses (`ttf-parser` outlines,
//! with a bare-CFF / Type1C fallback) and are the single source of truth the
//! SVG renderer relies on.

use crate::fonts::variations::{self, VariationRequest};
use crate::render::font_rasterizer::GlyphToPath;
use crate::render::path::Path;
use ttf_parser::GlyphId;

/// Convert a font size (text-space units) and the font's units-per-em into the
/// scale that maps glyph-outline font units to text space. Returns 0 for
/// degenerate inputs (caller skips the glyph).
pub fn font_size_scale(font_size: f64, upem: f64) -> f64 {
    if font_size <= 0.0 || upem <= 0.0 || !font_size.is_finite() || !upem.is_finite() {
        0.0
    } else {
        font_size / upem
    }
}

/// The font's units-per-em. Handles sfnt-wrapped fonts via `ttf-parser` and
/// bare CFF / Type1C programs (which use a 1000-unit em).
pub fn get_upem(font_bytes: &[u8]) -> Option<u16> {
    if let Ok(face) = ttf_parser::Face::parse(font_bytes, 0) {
        return Some(face.units_per_em());
    }
    if crate::render::font_rasterizer::cff_support::is_bare_cff(font_bytes) {
        return Some(crate::render::font_rasterizer::cff_support::units_per_em() as u16);
    }
    if crate::fonts::type1::Type1Font::is_type1(font_bytes) {
        return Some(crate::fonts::type1::units_per_em() as u16);
    }
    None
}

/// Extract the outline [`Path`] (in font units) and advance width (in 1/1000
/// text units) for a Unicode character from a simple font. Falls back to the
/// standalone CFF parser for bare-CFF (FontFile3 /Type1C) programs.
pub fn extract_glyph_path(font_bytes: &[u8], ch: char) -> (Option<Path>, f64) {
    extract_glyph_path_for_simple(font_bytes, fallback_gid(ch), ch, None)
}

/// Extract the outline [`Path`] and advance width for a simple-font glyph,
/// preserving the original PDF character code and glyph name. This is needed
/// for Type1 programs (glyph-name keyed CharStrings) and subset TrueType fonts
/// whose useful cmap is not Unicode-keyed.
pub fn extract_glyph_path_for_simple(
    font_bytes: &[u8],
    code: u16,
    ch: char,
    glyph_name: Option<&str>,
) -> (Option<Path>, f64) {
    extract_glyph_path_for_simple_var(font_bytes, code, ch, glyph_name, &VariationRequest::none())
}

/// Variable-font aware variant of [`extract_glyph_path_for_simple`]: applies the
/// requested instance coordinates (if the font is variable and exposes the
/// axes) before producing the interpolated outline and HVAR-adjusted advance.
/// With an empty request this is byte-identical to the non-variable path.
pub fn extract_glyph_path_for_simple_var(
    font_bytes: &[u8],
    code: u16,
    ch: char,
    glyph_name: Option<&str>,
    request: &VariationRequest,
) -> (Option<Path>, f64) {
    let mut face = match ttf_parser::Face::parse(font_bytes, 0) {
        Ok(face) => face,
        Err(_) => {
            if let Some(glyph_name) = glyph_name {
                if let Some(result) = crate::render::font_rasterizer::cff_support::outline_by_name(
                    font_bytes, glyph_name,
                ) {
                    return result;
                }
            }
            if let Ok(code) = u8::try_from(code) {
                if let Some(result) =
                    crate::render::font_rasterizer::cff_support::outline_by_code(font_bytes, code)
                {
                    return result;
                }
            }
            if let Some(glyph_name) = glyph_name {
                if let Some(result) = crate::fonts::type1::outline_by_name(font_bytes, glyph_name) {
                    return result;
                }
            }
            if let Some(result) =
                crate::render::font_rasterizer::cff_support::outline_by_char(font_bytes, ch)
            {
                return result;
            }
            return (None, 500.0);
        }
    };

    // Apply the variable-font instance (no-op for static fonts / empty request);
    // the crate then interpolates the outline (gvar/CFF2) and the advance (HVAR).
    variations::apply_request(&mut face, request);

    let upem = f64::from(face.units_per_em());
    let glyph_id = glyph_index_for_simple(&face, code, ch, glyph_name)
        .unwrap_or_else(|| ttf_parser::GlyphId(fallback_gid(ch)));
    let advance = face
        .glyph_hor_advance(glyph_id)
        .map(|width| f64::from(width) / upem * 1000.0)
        .unwrap_or(500.0);

    let mut builder = GlyphToPath::new();
    if face.outline_glyph(glyph_id, &mut builder).is_none() {
        return (None, advance);
    }
    (Some(builder.into_path()), advance)
}

/// Extract the outline [`Path`] and advance width for a glyph id (CID fonts).
/// Falls back to the standalone CFF parser for bare CFF (/CIDFontType0C).
pub fn extract_glyph_path_by_gid(font_bytes: &[u8], gid: u16) -> (Option<Path>, f64) {
    extract_glyph_path_by_gid_var(font_bytes, gid, &VariationRequest::none())
}

/// Variable-font aware variant of [`extract_glyph_path_by_gid`]: applies the
/// requested instance coordinates before producing the interpolated outline and
/// HVAR-adjusted advance. Byte-identical to the non-variable path for an empty
/// request.
pub fn extract_glyph_path_by_gid_var(
    font_bytes: &[u8],
    gid: u16,
    request: &VariationRequest,
) -> (Option<Path>, f64) {
    let mut face = match ttf_parser::Face::parse(font_bytes, 0) {
        Ok(face) => face,
        Err(_) => {
            if let Some(result) =
                crate::render::font_rasterizer::cff_support::outline_by_gid(font_bytes, gid)
            {
                return result;
            }
            return (None, 500.0);
        }
    };

    variations::apply_request(&mut face, request);

    let upem = f64::from(face.units_per_em());
    if upem <= 0.0 {
        return (None, 500.0);
    }
    let glyph_id = ttf_parser::GlyphId(gid);
    let advance = face
        .glyph_hor_advance(glyph_id)
        .map(|width| f64::from(width) / upem * 1000.0)
        .unwrap_or(1000.0);

    let mut builder = GlyphToPath::new();
    if face.outline_glyph(glyph_id, &mut builder).is_none() {
        return (None, advance);
    }
    (Some(builder.into_path()), advance)
}

/// Best-effort fallback glyph id for a char with no cmap mapping, mirroring the
/// raster path's heuristic (code-point-derived id, saturating).
fn fallback_gid(ch: char) -> u16 {
    let code = ch as u32;
    (code.min(u32::from(u16::MAX)) as u16).saturating_sub(1)
}

fn glyph_index_for_simple(
    face: &ttf_parser::Face<'_>,
    code: u16,
    ch: char,
    glyph_name: Option<&str>,
) -> Option<GlyphId> {
    if let Some(name) = glyph_name.filter(|name| *name != ".notdef") {
        if let Some(gid) = face.glyph_index_by_name(name) {
            return Some(gid);
        }
    }
    if let Some(gid) = face.glyph_index(ch) {
        return Some(gid);
    }
    glyph_index_from_non_unicode_cmap(face, u32::from(code))
}

fn glyph_index_from_non_unicode_cmap(face: &ttf_parser::Face<'_>, code: u32) -> Option<GlyphId> {
    let cmap = face.tables().cmap?;

    // Prefer the simple-font cmap subtables used by PDF producers for 8-bit
    // subset fonts: Macintosh Roman (1,0) and Windows Symbol (3,0). Only use
    // this path as a fallback after Unicode/name lookup to keep common Unicode
    // TrueType rendering unchanged.
    for wanted in [
        (ttf_parser::PlatformId::Macintosh, 0u16),
        (ttf_parser::PlatformId::Windows, 0u16),
    ] {
        for subtable in cmap.subtables {
            if subtable.platform_id == wanted.0 && subtable.encoding_id == wanted.1 {
                if let Some(gid) = subtable.glyph_index(code) {
                    return Some(gid);
                }
            }
        }
    }

    for subtable in cmap.subtables {
        if subtable.is_unicode() {
            continue;
        }
        if let Some(gid) = subtable.glyph_index(code) {
            return Some(gid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ContentEngine;
    use crate::fonts::resolver::FontResolver;
    use crate::render::font_rasterizer::FontRasterizer;

    fn repo_fixture(path: &str) -> String {
        format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn simple_truetype_subset_uses_non_unicode_cmap_when_unicode_absent() {
        let engine =
            ContentEngine::open_path(repo_fixture("tests/corpus/pdfs/pdfjs/openoffice.pdf"))
                .expect("open openoffice fixture");
        let resources = engine.get_page_resources(1).expect("page resources");
        let reader = engine.document().reader();
        let font_dict = resources
            .fonts
            .values()
            .find(|dict| {
                dict.get_name("BaseFont")
                    .map(|name| name.contains("Helvetica-Bold"))
                    .unwrap_or(false)
            })
            .expect("openoffice subset font");
        let font_bytes =
            FontRasterizer::extract_font_bytes(font_dict, reader).expect("embedded font bytes");
        let face = ttf_parser::Face::parse(&font_bytes, 0).expect("parse subset font");

        assert!(
            face.glyph_index('A').is_none(),
            "fixture must exercise the non-Unicode cmap fallback"
        );
        let expected_gid = face
            .tables()
            .cmap
            .and_then(|cmap| {
                cmap.subtables
                    .into_iter()
                    .find(|subtable| !subtable.is_unicode())
                    .and_then(|subtable| subtable.glyph_index(65))
            })
            .expect("MacRoman cmap maps code 65");
        let expected_advance = face
            .glyph_hor_advance(expected_gid)
            .map(|width| f64::from(width) / f64::from(face.units_per_em()) * 1000.0)
            .expect("expected glyph advance");

        let (path, advance) = extract_glyph_path_for_simple(&font_bytes, 65, 'A', Some("A"));

        assert!(path.is_some(), "mapped subset glyph should have an outline");
        assert!(
            (advance - expected_advance).abs() < 0.1,
            "advance {advance} should come from cmap-selected gid {expected_gid:?}, expected {expected_advance}"
        );
    }

    #[test]
    fn bare_cff_simple_font_uses_pdf_char_code_for_winansi_punctuation() {
        let engine = ContentEngine::open_path(repo_fixture(
            "renderer-benchmark/corpus/real-world/irs-public-domain/f1040.pdf",
        ))
        .expect("open IRS form fixture");
        let resources = engine.get_page_resources(1).expect("page resources");
        let reader = engine.document().reader();
        let font_dict = resources
            .fonts
            .values()
            .find(|dict| {
                dict.get_name("BaseFont")
                    .map(|name| name.contains("HelveticaNeueLTStd-Roman"))
                    .unwrap_or(false)
            })
            .expect("IRS form should use embedded HelveticaNeue Type1C");
        let font_bytes =
            FontRasterizer::extract_font_bytes(font_dict, reader).expect("embedded font bytes");
        assert!(
            crate::render::font_rasterizer::cff_support::is_bare_cff(&font_bytes),
            "fixture should exercise bare CFF / Type1C simple fonts"
        );

        let resolver = FontResolver::new(font_dict, reader);
        assert_eq!(resolver.decode_char(0x96), "\u{2013}");
        assert_eq!(resolver.glyph_name(0x96), Some("endash"));

        let (notdef_path, _) = extract_glyph_path_for_simple(&font_bytes, 0, '\0', Some(".notdef"));
        let (endash_path, _) =
            extract_glyph_path_for_simple(&font_bytes, 0x96, '\u{2013}', Some("endash"));

        let notdef = path_bounds(&notdef_path.expect(".notdef outline should exist"));
        let endash = path_bounds(&endash_path.expect("endash outline should exist"));

        assert!(
            endash.2 > endash.0 && endash.3 > endash.1,
            "endash outline should have positive bounds"
        );
        assert_ne!(
            quantized_bounds(notdef),
            quantized_bounds(endash),
            "WinAnsi 0x96 must render the endash glyph, not .notdef"
        );
    }

    fn path_bounds(path: &Path) -> (f64, f64, f64, f64) {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for segment in &path.segments {
            match segment {
                crate::render::path::PathSegment::MoveTo(x, y)
                | crate::render::path::PathSegment::LineTo(x, y) => {
                    min_x = min_x.min(*x);
                    min_y = min_y.min(*y);
                    max_x = max_x.max(*x);
                    max_y = max_y.max(*y);
                }
                crate::render::path::PathSegment::CubicTo {
                    cp1x,
                    cp1y,
                    cp2x,
                    cp2y,
                    x,
                    y,
                } => {
                    for (px, py) in [(*cp1x, *cp1y), (*cp2x, *cp2y), (*x, *y)] {
                        min_x = min_x.min(px);
                        min_y = min_y.min(py);
                        max_x = max_x.max(px);
                        max_y = max_y.max(py);
                    }
                }
                crate::render::path::PathSegment::ClosePath => {}
            }
        }
        (min_x, min_y, max_x, max_y)
    }

    fn quantized_bounds(bounds: (f64, f64, f64, f64)) -> (i32, i32, i32, i32) {
        (
            bounds.0.round() as i32,
            bounds.1.round() as i32,
            bounds.2.round() as i32,
            bounds.3.round() as i32,
        )
    }
}
