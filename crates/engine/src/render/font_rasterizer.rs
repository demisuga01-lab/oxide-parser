use crate::content::state::Matrix;
use crate::filters::decode_stream_lossless;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::render::buffer::{PixelBuffer, PixelColor, BLACK};
use crate::render::line::DashState;
use crate::render::path::{FillRule, GlyphHinting, Path, PathPainter};
use crate::render::transform::{Transform2D, Viewport};
use ttf_parser::OutlineBuilder;

mod fallback_fonts {
    pub static LIBERATION_SANS_REGULAR: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSans-Regular.ttf"
    ));
    pub static LIBERATION_SANS_BOLD: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSans-Bold.ttf"
    ));
    pub static LIBERATION_SANS_ITALIC: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSans-Italic.ttf"
    ));
    pub static LIBERATION_SANS_BOLD_ITALIC: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSans-BoldItalic.ttf"
    ));

    pub static LIBERATION_SERIF_REGULAR: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSerif-Regular.ttf"
    ));
    pub static LIBERATION_SERIF_BOLD: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSerif-Bold.ttf"
    ));
    pub static LIBERATION_SERIF_ITALIC: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSerif-Italic.ttf"
    ));
    pub static LIBERATION_SERIF_BOLD_ITALIC: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationSerif-BoldItalic.ttf"
    ));

    pub static LIBERATION_MONO_REGULAR: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationMono-Regular.ttf"
    ));
    pub static LIBERATION_MONO_BOLD: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationMono-Bold.ttf"
    ));
    pub static LIBERATION_MONO_ITALIC: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationMono-Italic.ttf"
    ));
    pub static LIBERATION_MONO_BOLD_ITALIC: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/LiberationMono-BoldItalic.ttf"
    ));

    /// DejaVu Sans — symbolic-font fallback for Symbol / ZapfDingbats /
    /// Wingdings. Chosen because it has broad Unicode coverage:
    /// the Greek block and Mathematical Operators (for Symbol), and the Dingbats
    /// / Miscellaneous Symbols blocks (for ZapfDingbats and Wingdings, which are
    /// mapped through Unicode). Licence: Bitstream Vera / DejaVu (Bitstream
    /// portions © Bitstream Inc.; DejaVu changes are public domain) — a
    /// permissive free licence, at least as permissive as the OFL used by the
    /// bundled Liberation fonts. Source: http://dejavu.sourceforge.net/.
    pub static DEJAVU_SANS: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/DejaVuSans.ttf"));
}

/// Map a PDF font name to a bundled fallback font byte slice.
pub fn get_fallback_font(font_name: &str) -> Option<&'static [u8]> {
    let raw = font_name.trim_start_matches('/');
    let raw = raw.find('+').map_or(raw, |idx| &raw[idx + 1..]);
    let name = raw.to_lowercase();

    let is_bold = name.contains("bold")
        || name.contains("-b")
        || name.ends_with('b')
        || name.contains("heavy")
        || name.contains("black");
    let is_italic = name.contains("italic")
        || name.contains("oblique")
        || name.contains("slant")
        || name.ends_with("-i")
        || name.ends_with("-o");

    // Symbolic fonts (Symbol, ZapfDingbats, Wingdings, Webdings) have glyph sets
    // that the Latin Liberation fonts can't represent. Route them to DejaVu Sans,
    // which covers the Greek/math (Symbol) and Dingbats/Misc-Symbols (ZapfDingbats
    // / Wingdings via Unicode) ranges. The char-code → glyph mapping uses the
    // built-in Symbol/ZapfDingbats encodings (Appendix D) → Unicode → DejaVu cmap.
    if name.contains("symbol")
        || name.contains("dingbat")
        || name.contains("wingding")
        || name.contains("webding")
    {
        return Some(fallback_fonts::DEJAVU_SANS);
    }

    if name.contains("courier")
        || name.contains("mono")
        || name.contains("typewriter")
        || name.contains("consolas")
        || name.contains("inconsolata")
        || name.contains("sourcecodemono")
        || name.contains("lucidaconsole")
    {
        return Some(match (is_bold, is_italic) {
            (true, true) => fallback_fonts::LIBERATION_MONO_BOLD_ITALIC,
            (true, false) => fallback_fonts::LIBERATION_MONO_BOLD,
            (false, true) => fallback_fonts::LIBERATION_MONO_ITALIC,
            (false, false) => fallback_fonts::LIBERATION_MONO_REGULAR,
        });
    }

    if name.contains("times")
        || name.contains("serif")
        || name.contains("georgia")
        || name.contains("palatino")
        || name.contains("bookman")
        || name.contains("garamond")
        || name.contains("cambria")
        || name.contains("constantia")
        || name == "trmn"
    {
        return Some(match (is_bold, is_italic) {
            (true, true) => fallback_fonts::LIBERATION_SERIF_BOLD_ITALIC,
            (true, false) => fallback_fonts::LIBERATION_SERIF_BOLD,
            (false, true) => fallback_fonts::LIBERATION_SERIF_ITALIC,
            (false, false) => fallback_fonts::LIBERATION_SERIF_REGULAR,
        });
    }

    Some(match (is_bold, is_italic) {
        (true, true) => fallback_fonts::LIBERATION_SANS_BOLD_ITALIC,
        (true, false) => fallback_fonts::LIBERATION_SANS_BOLD,
        (false, true) => fallback_fonts::LIBERATION_SANS_ITALIC,
        (false, false) => fallback_fonts::LIBERATION_SANS_REGULAR,
    })
}

pub(crate) struct GlyphToPath {
    path: Path,
    current_x: f32,
    current_y: f32,
}

impl GlyphToPath {
    pub(crate) fn new() -> Self {
        Self {
            path: Path::new(),
            current_x: 0.0,
            current_y: 0.0,
        }
    }

    pub(crate) fn into_path(self) -> Path {
        self.path
    }
}

impl OutlineBuilder for GlyphToPath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(x as f64, y as f64);
        self.current_x = x;
        self.current_y = y;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(x as f64, y as f64);
        self.current_x = x;
        self.current_y = y;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0x = self.current_x as f64;
        let p0y = self.current_y as f64;
        let p1x = x1 as f64;
        let p1y = y1 as f64;
        let p2x = x as f64;
        let p2y = y as f64;

        let cp1x = p0x + 2.0 / 3.0 * (p1x - p0x);
        let cp1y = p0y + 2.0 / 3.0 * (p1y - p0y);
        let cp2x = p2x + 2.0 / 3.0 * (p1x - p2x);
        let cp2y = p2y + 2.0 / 3.0 * (p1y - p2y);

        self.path.curve_to(cp1x, cp1y, cp2x, cp2y, p2x, p2y);
        self.current_x = x;
        self.current_y = y;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(
            x1 as f64, y1 as f64, x2 as f64, y2 as f64, x as f64, y as f64,
        );
        self.current_x = x;
        self.current_y = y;
    }

    fn close(&mut self) {
        self.path.close();
    }
}

/// Bare-CFF (Compact Font Format) glyph support.
///
/// PDF embeds CFF/Type2 fonts via `/FontFile3` with `/Subtype /Type1C` (simple
/// fonts) or `/CIDFontType0C` (CID-keyed, used by Type0 composite fonts, common
/// in CJK). These are a *raw* `CFF ` table, NOT wrapped in an sfnt/OpenType
/// container, so [`ttf_parser::Face::parse`] rejects them (it requires `head`,
/// `hhea`, and `maxp` tables and an sfnt magic). `ttf_parser` does, however,
/// expose a standalone [`ttf_parser::cff::Table`] parser for exactly this case.
///
/// These helpers are a *fallback*: callers try `Face::parse` first (handling
/// TrueType and CFF-flavoured OpenType, which are sfnt-wrapped) and only reach
/// for the bare-CFF path when that fails. This keeps the existing, working font
/// paths completely untouched (minimal blast radius).
///
/// CFF charstring coordinates are already in a 1000-unit em by convention (the
/// FontMatrix default is `0.001`), so we report a units-per-em of `1000` and
/// scale advances accordingly, matching the glyf path's `/1000` convention.
pub(crate) mod cff_support {
    use super::{GlyphToPath, Path};
    use ttf_parser::GlyphId;

    /// Parse a bare CFF table, returning `None` if the bytes are not a usable
    /// standalone CFF font (e.g. they are actually an sfnt — handled elsewhere).
    fn parse(font_bytes: &[u8]) -> Option<ttf_parser::cff::Table<'_>> {
        ttf_parser::cff::Table::parse(font_bytes)
    }

    /// True if the bytes parse as a bare CFF table.
    pub(crate) fn is_bare_cff(font_bytes: &[u8]) -> bool {
        parse(font_bytes).is_some()
    }

    /// The effective units-per-em for a bare CFF font. CFF charstrings use the
    /// FontMatrix (default 0.001 → a 1000-unit em); we normalise everything to
    /// 1000 so the renderer's existing `/1000` advance math applies unchanged.
    pub(crate) fn units_per_em() -> f64 {
        1000.0
    }

    /// Extract a glyph outline and advance width (in 1000-unit em) for a glyph
    /// index. Returns `(None, advance)` when the glyph has no outline (e.g.
    /// whitespace) and `None` entirely when the font is not bare CFF.
    pub(crate) fn outline_by_gid(font_bytes: &[u8], gid: u16) -> Option<(Option<Path>, f64)> {
        let table = parse(font_bytes)?;
        let glyph_id = GlyphId(gid);
        let advance = table
            .glyph_width(glyph_id)
            .map(f64::from)
            // CID-keyed CFF returns None for glyph_width; the descendant font's
            // /W array (handled by the caller) supplies the real advance, so a
            // neutral 1000 here is only a fallback.
            .unwrap_or(1000.0);
        let mut builder = GlyphToPath::new();
        match table.outline(glyph_id, &mut builder) {
            Ok(_) => Some((Some(builder.into_path()), advance)),
            Err(_) => Some((None, advance)),
        }
    }

    /// Extract a glyph outline and advance for an original 8-bit PDF character
    /// code in a *simple* (SID-keyed) CFF font, mapping the code through the
    /// CFF encoding + charset. Returns `None` when the font is not bare CFF.
    pub(crate) fn outline_by_code(font_bytes: &[u8], code: u8) -> Option<(Option<Path>, f64)> {
        let table = parse(font_bytes)?;
        let glyph_id = table.glyph_index(code).unwrap_or(GlyphId(0));
        let advance = table.glyph_width(glyph_id).map(f64::from).unwrap_or(1000.0);
        let mut builder = GlyphToPath::new();
        match table.outline(glyph_id, &mut builder) {
            Ok(_) => Some((Some(builder.into_path()), advance)),
            Err(_) => Some((None, advance)),
        }
    }

    /// Extract a glyph outline and advance by Adobe glyph name from a
    /// SID-keyed CFF font. PDF `/Encoding /Differences` entries are glyph-name
    /// based and override the CFF program's own 8-bit encoding, so this is the
    /// preferred simple-font lookup whenever the PDF resolved a glyph name.
    pub(crate) fn outline_by_name(
        font_bytes: &[u8],
        glyph_name: &str,
    ) -> Option<(Option<Path>, f64)> {
        let table = parse(font_bytes)?;
        let glyph_id = table.glyph_index_by_name(glyph_name)?;
        let advance = table.glyph_width(glyph_id).map(f64::from).unwrap_or(1000.0);
        let mut builder = GlyphToPath::new();
        match table.outline(glyph_id, &mut builder) {
            Ok(_) => Some((Some(builder.into_path()), advance)),
            Err(_) => Some((None, advance)),
        }
    }

    /// Extract a glyph outline and advance for a Unicode scalar in a *simple*
    /// (SID-keyed) CFF font. This is a fallback for callers that no longer have
    /// the original PDF character code; prefer [`outline_by_code`] for PDF
    /// simple fonts so high-byte WinAnsi punctuation does not collapse to
    /// `.notdef`.
    pub(crate) fn outline_by_char(font_bytes: &[u8], ch: char) -> Option<(Option<Path>, f64)> {
        let code = u32::from(ch);
        if code <= 0xFF {
            outline_by_code(font_bytes, code as u8)
        } else {
            let table = parse(font_bytes)?;
            let glyph_id = GlyphId(0);
            let advance = table.glyph_width(glyph_id).map(f64::from).unwrap_or(1000.0);
            let mut builder = GlyphToPath::new();
            match table.outline(glyph_id, &mut builder) {
                Ok(_) => Some((Some(builder.into_path()), advance)),
                Err(_) => Some((None, advance)),
            }
        }
    }
}

pub struct FontRasterizer;

impl FontRasterizer {
    /// Rasterize a single glyph onto the pixel buffer.
    #[allow(clippy::too_many_arguments)]
    pub fn rasterize_glyph(
        buf: &mut PixelBuffer,
        char_code: u16,
        font_bytes: &[u8],
        font_size: f64,
        tm: &Matrix,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        render_mode: i32,
        stroke_color: PixelColor,
        stroke_width: f64,
    ) -> bool {
        if render_mode == 3 {
            return true;
        }

        let face = match ttf_parser::Face::parse(font_bytes, 0) {
            Ok(face) => face,
            Err(err) => {
                log::warn!("FontRasterizer: failed to parse font: {:?}", err);
                return false;
            }
        };

        let ch = char::from_u32(char_code as u32).unwrap_or('\u{FFFD}');
        let glyph_id = face
            .glyph_index(ch)
            .unwrap_or_else(|| ttf_parser::GlyphId(char_code.saturating_sub(1)));

        let mut builder = GlyphToPath::new();
        if face.outline_glyph(glyph_id, &mut builder).is_none() {
            return true;
        }

        let glyph_path = builder.into_path();
        let upem = face.units_per_em() as f64;
        if upem <= 0.0 || font_size <= 0.0 {
            return true;
        }

        let scale_t = Transform2D::scale(font_size / upem, font_size / upem);
        let tm_t = Transform2D::from(*tm);
        let glyph_ctm = scale_t.concat(&tm_t).concat(ctm);

        // Keep production glyph rendering on the non-distorting coverage path.
        // Light grid-fitting is available to test but is deferred for default
        // rendering until it improves Poppler comparisons instead of regressing.
        let glyph_hinting = GlyphHinting::disabled();

        match render_mode {
            0 | 4 => PathPainter::fill_glyph(
                buf,
                &glyph_path,
                &glyph_ctm,
                viewport,
                color,
                FillRule::NonZero,
                glyph_hinting,
            ),
            1 | 5 => PathPainter::stroke(
                buf,
                &glyph_path,
                &glyph_ctm,
                viewport,
                stroke_color,
                stroke_width,
                &DashState::solid(),
            ),
            2 | 6 => {
                PathPainter::fill_glyph(
                    buf,
                    &glyph_path,
                    &glyph_ctm,
                    viewport,
                    color,
                    FillRule::NonZero,
                    glyph_hinting,
                );
                PathPainter::stroke(
                    buf,
                    &glyph_path,
                    &glyph_ctm,
                    viewport,
                    stroke_color,
                    stroke_width,
                    &DashState::solid(),
                );
            }
            3 | 7 => {}
            _ => log::warn!("FontRasterizer: unknown render_mode {}", render_mode),
        }

        true
    }

    /// Rasterize a run of decoded Unicode text.
    #[allow(clippy::too_many_arguments)]
    pub fn rasterize_text(
        buf: &mut PixelBuffer,
        text: &str,
        font_bytes: &[u8],
        font_size: f64,
        tm: &Matrix,
        ctm: &Transform2D,
        viewport: &Viewport,
        color: PixelColor,
        render_mode: i32,
    ) {
        if ttf_parser::Face::parse(font_bytes, 0).is_err() {
            log::warn!("FontRasterizer::rasterize_text: font parse failed");
            return;
        }

        for c in text.chars() {
            let char_code = if (c as u32) <= u16::MAX as u32 {
                c as u16
            } else {
                0xFFFD
            };
            let _ = Self::rasterize_glyph(
                buf,
                char_code,
                font_bytes,
                font_size,
                tm,
                ctm,
                viewport,
                color,
                render_mode,
                BLACK,
                1.0,
            );
        }
    }

    /// Extract embedded raw font bytes from a PDF font dictionary.
    pub fn extract_font_bytes(font_dict: &PdfDictionary, reader: &PdfReader) -> Option<Vec<u8>> {
        let descriptor = match font_dict.get("FontDescriptor") {
            Some(value) => match reader.resolve(value.clone()).ok()? {
                PdfObject::Dictionary(dict) => dict,
                _ => return None,
            },
            None => return None,
        };

        for key in ["FontFile3", "FontFile2", "FontFile"] {
            if let Some(font_file) = descriptor.get(key) {
                if let PdfObject::Stream { dict, raw } = reader.resolve(font_file.clone()).ok()? {
                    if !raw.is_empty() {
                        let stream = PdfObject::Stream {
                            dict: dict.clone(),
                            raw: raw.clone(),
                        };
                        if let Ok(decoded) = decode_stream_lossless(&stream, reader) {
                            if !decoded.data.is_empty() {
                                return Some(decoded.data);
                            }
                        }
                        return Some(raw);
                    }
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::{BLACK, WHITE};
    use crate::render::path::PathSegment;

    #[test]
    fn glyph_to_path_converts_line_to_correctly() {
        let mut builder = GlyphToPath::new();
        builder.move_to(0.0, 0.0);
        builder.line_to(100.0, 0.0);
        let path = builder.into_path();
        assert_eq!(path.segments.len(), 2);
        assert!(matches!(path.segments[0], PathSegment::MoveTo(0.0, 0.0)));
        assert!(matches!(path.segments[1], PathSegment::LineTo(100.0, 0.0)));
    }

    #[test]
    fn glyph_to_path_quad_to_produces_cubic() {
        let mut builder = GlyphToPath::new();
        builder.move_to(0.0, 0.0);
        builder.quad_to(50.0, 100.0, 100.0, 0.0);
        let path = builder.into_path();
        assert_eq!(path.segments.len(), 2);
        assert!(matches!(path.segments[1], PathSegment::CubicTo { .. }));
    }

    #[test]
    fn glyph_to_path_quad_to_cubic_endpoint_is_correct() {
        let mut builder = GlyphToPath::new();
        builder.move_to(0.0, 0.0);
        builder.quad_to(50.0, 100.0, 100.0, 0.0);
        let path = builder.into_path();
        match path.segments.get(1) {
            Some(PathSegment::CubicTo { x, y, .. }) => {
                assert!((*x - 100.0).abs() < 0.001);
                assert!((*y - 0.0).abs() < 0.001);
            }
            other => panic!("expected CubicTo, got {other:?}"),
        }
    }

    #[test]
    fn get_fallback_font_returns_some_or_none_for_known_names() {
        assert!(get_fallback_font("Helvetica")
            .map(|bytes| !bytes.is_empty())
            .unwrap_or(true));
        let _ = get_fallback_font("UnknownFont12345");
    }

    #[test]
    fn fallback_font_returns_real_bytes_for_standard_fonts() {
        for name in ["Helvetica", "Times-Roman", "Courier"] {
            let font = get_fallback_font(name).expect("standard font should have fallback");
            assert!(
                font.len() > 10_000,
                "{name} should return a real TTF, got {} bytes",
                font.len()
            );
            assert!(
                font.starts_with(b"\x00\x01\x00\x00")
                    || font.starts_with(b"true")
                    || font.starts_with(b"OTTO"),
                "{name} should start with a valid TTF/OTF header"
            );
        }
    }

    #[test]
    fn fallback_font_selects_weight_and_style_variants() {
        let regular = get_fallback_font("Helvetica").expect("regular fallback");
        let bold = get_fallback_font("Helvetica-Bold").expect("bold fallback");
        let italic = get_fallback_font("Helvetica-Oblique").expect("italic fallback");
        let bold_italic = get_fallback_font("Helvetica-BoldOblique").expect("bold italic fallback");

        assert_ne!(regular.as_ptr(), bold.as_ptr());
        assert_ne!(regular.as_ptr(), italic.as_ptr());
        assert_ne!(regular.as_ptr(), bold_italic.as_ptr());
        assert!(!bold_italic.is_empty());
    }

    #[test]
    fn fallback_font_handles_subset_prefix_and_aliases() {
        assert!(get_fallback_font("ABCDEF+Helvetica").is_some());

        let arial = get_fallback_font("Arial").expect("Arial fallback");
        let helvetica = get_fallback_font("Helvetica").expect("Helvetica fallback");
        assert_eq!(arial.as_ptr(), helvetica.as_ptr());

        let courier_new = get_fallback_font("CourierNew").expect("CourierNew fallback");
        assert!(courier_new.len() > 10_000);
    }

    #[test]
    fn fallback_font_routes_symbolic_fonts_to_dejavu() {
        // Symbolic fonts now get the DejaVu Sans fallback rather
        // than rendering as nothing.
        for name in ["Symbol", "ZapfDingbats", "Wingdings", "ABCDEF+Symbol"] {
            let font = get_fallback_font(name)
                .unwrap_or_else(|| panic!("{name} should get a symbolic fallback"));
            assert!(font.len() > 100_000, "{name} -> DejaVu (large TTF)");
        }
        // Symbolic fallback is a different font than the Latin Liberation Sans.
        let symbol = get_fallback_font("Symbol").unwrap();
        let helv = get_fallback_font("Helvetica").unwrap();
        assert_ne!(symbol.as_ptr(), helv.as_ptr());
    }

    #[test]
    fn symbolic_fallback_font_has_greek_math_and_dingbat_glyphs() {
        let font = get_fallback_font("Symbol").expect("symbol fallback");
        let face = ttf_parser::Face::parse(font, 0).expect("DejaVu should parse");
        // Greek alpha (Symbol), summation/integral (math), check mark + black
        // circle (ZapfDingbats), right arrow (Wingdings-ish).
        for ch in [
            '\u{03B1}', '\u{2211}', '\u{222B}', '\u{2713}', '\u{25CF}', '\u{2192}',
        ] {
            assert!(
                face.glyph_index(ch).is_some(),
                "DejaVu should cover U+{:04X}",
                ch as u32
            );
        }
    }

    #[test]
    fn fallback_font_is_parseable_and_contains_common_glyphs() {
        let font = get_fallback_font("Helvetica").expect("Helvetica fallback");
        let parsed = ttf_parser::Face::parse(font, 0);
        assert!(parsed.is_ok(), "Liberation Sans should parse: {parsed:?}");
        let Ok(face) = parsed else {
            return;
        };
        assert!(face.units_per_em() > 0);
        assert!(face.glyph_index('H').is_some(), "should have glyph for H");
    }

    #[test]
    fn fallback_font_can_extract_glyph_outline() {
        let font = get_fallback_font("Helvetica").expect("Helvetica fallback");
        let parsed = ttf_parser::Face::parse(font, 0);
        assert!(parsed.is_ok(), "Liberation Sans should parse: {parsed:?}");
        let Ok(face) = parsed else {
            return;
        };
        let glyph_id = face.glyph_index('A');
        assert!(
            glyph_id.is_some(),
            "Liberation Sans should have glyph for A"
        );
        let Some(glyph_id) = glyph_id else {
            return;
        };
        let mut builder = GlyphToPath::new();
        assert!(face.outline_glyph(glyph_id, &mut builder).is_some());
        let path = builder.into_path();
        assert!(
            !path.segments.is_empty(),
            "glyph A should have path segments"
        );
    }

    #[test]
    fn rasterize_glyph_with_fallback_font_renders_visible_pixels() {
        let font_bytes = match get_fallback_font("Helvetica") {
            Some(bytes) if !bytes.is_empty() => bytes,
            _ => {
                println!("SKIP: fallback fonts not bundled, skipping glyph render test");
                return;
            }
        };
        let vp = Viewport::new([0.0, 0.0, 200.0, 200.0], 72);
        let ctm = Transform2D::identity();
        let tm: Matrix = [1.0, 0.0, 0.0, 1.0, 50.0, 100.0];
        let mut buf = PixelBuffer::new_filled(200, 200, WHITE);

        let success = FontRasterizer::rasterize_glyph(
            &mut buf, 'A' as u16, font_bytes, 24.0, &tm, &ctm, &vp, BLACK, 0, BLACK, 1.0,
        );
        assert!(success);

        let darkened_count = (0..200i32)
            .flat_map(|y| (0..200i32).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get_pixel(x, y)[0] < 200)
            .count();
        println!("glyph A darkened pixels: {darkened_count}");
        assert!(darkened_count > 0);
    }

    #[test]
    fn rasterize_glyph_invisible_mode_does_not_paint() {
        let font_bytes = match get_fallback_font("Helvetica") {
            Some(bytes) if !bytes.is_empty() => bytes,
            _ => return,
        };
        let vp = Viewport::new([0.0, 0.0, 200.0, 200.0], 72);
        let ctm = Transform2D::identity();
        let tm: Matrix = [1.0, 0.0, 0.0, 1.0, 50.0, 100.0];
        let mut buf = PixelBuffer::new_filled(200, 200, WHITE);

        FontRasterizer::rasterize_glyph(
            &mut buf, 'A' as u16, font_bytes, 24.0, &tm, &ctm, &vp, BLACK, 3, BLACK, 1.0,
        );

        let changed = (0..200i32)
            .flat_map(|y| (0..200i32).map(move |x| (x, y)))
            .any(|(x, y)| buf.get_pixel(x, y) != WHITE);
        assert!(!changed);
    }

    // ── Bare CFF (OpenType-CFF / Type1C) support ────────────────────────────
    //
    // `sample_type1c.cff` is a real `/FontFile3 /Subtype /Type1C` program
    // extracted from the `freeculture.pdf` corpus fixture (a bare CFF table,
    // header 01 00 04 02). It exercises the standalone-CFF fallback that the
    // sfnt-based `Face::parse` cannot handle.
    const SAMPLE_CFF: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_type1c.cff"
    ));

    #[test]
    fn bare_cff_is_not_accepted_by_sfnt_parser() {
        // Confirms the gap this code closes: ttf_parser::Face::parse (sfnt-only)
        // rejects a bare CFF table, so the CFF fallback is genuinely needed.
        assert!(
            ttf_parser::Face::parse(SAMPLE_CFF, 0).is_err(),
            "bare CFF must NOT parse as an sfnt face"
        );
    }

    #[test]
    fn bare_cff_is_detected() {
        assert!(cff_support::is_bare_cff(SAMPLE_CFF));
        // A non-CFF blob is not misdetected.
        assert!(!cff_support::is_bare_cff(b"not a font at all"));
    }

    #[test]
    fn bare_cff_extracts_a_glyph_outline_by_gid() {
        // Glyph 0 is .notdef; scan for the first glyph index that yields a
        // non-empty outline, proving the CFF charstring interpreter runs.
        let mut found_outline = false;
        for gid in 0..64u16 {
            if let Some((Some(path), advance)) = cff_support::outline_by_gid(SAMPLE_CFF, gid) {
                if !path.segments.is_empty() {
                    assert!(advance >= 0.0, "advance should be non-negative");
                    found_outline = true;
                    break;
                }
            }
        }
        assert!(
            found_outline,
            "at least one CFF glyph should produce outline segments"
        );
    }

    #[test]
    fn bare_cff_reports_1000_unit_em() {
        assert_eq!(cff_support::units_per_em(), 1000.0);
    }

    #[test]
    fn bare_cff_helpers_return_none_for_non_cff() {
        assert!(cff_support::outline_by_gid(b"garbage", 1).is_none());
        assert!(cff_support::outline_by_char(b"garbage", 'A').is_none());
    }
}
