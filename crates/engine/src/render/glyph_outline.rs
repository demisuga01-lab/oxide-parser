//! Shared glyph-outline extraction used by both the raster renderer and the
//! vector (SVG) renderer.
//!
//! Both backends must turn a glyph (by Unicode char for simple fonts, or by
//! glyph id for CID fonts) into the SAME outline [`Path`] in font units, so
//! that vector output is visually identical to raster output. These free
//! functions mirror the extraction the raster path uses (`ttf-parser` outlines,
//! with a bare-CFF / Type1C fallback) and are the single source of truth the
//! SVG renderer relies on.

use crate::render::font_rasterizer::GlyphToPath;
use crate::render::path::Path;

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
    None
}

/// Extract the outline [`Path`] (in font units) and advance width (in 1/1000
/// text units) for a Unicode character from a simple font. Falls back to the
/// standalone CFF parser for bare-CFF (FontFile3 /Type1C) programs.
pub fn extract_glyph_path(font_bytes: &[u8], ch: char) -> (Option<Path>, f64) {
    let face = match ttf_parser::Face::parse(font_bytes, 0) {
        Ok(face) => face,
        Err(_) => {
            if let Some(result) =
                crate::render::font_rasterizer::cff_support::outline_by_char(font_bytes, ch)
            {
                return result;
            }
            return (None, 500.0);
        }
    };

    let upem = f64::from(face.units_per_em());
    let glyph_id = face
        .glyph_index(ch)
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
    let face = match ttf_parser::Face::parse(font_bytes, 0) {
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
