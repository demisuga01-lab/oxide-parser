use crate::content::state::Matrix;
use crate::filters::decode_stream_lossless;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::render::buffer::{PixelBuffer, PixelColor, BLACK};
use crate::render::line::DashState;
use crate::render::path::{FillRule, Path, PathPainter};
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

    if name.contains("symbol") || name.contains("dingbat") || name.contains("wingding") {
        return None;
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

        match render_mode {
            0 | 4 => PathPainter::fill(
                buf,
                &glyph_path,
                &glyph_ctm,
                viewport,
                color,
                FillRule::NonZero,
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
                PathPainter::fill(
                    buf,
                    &glyph_path,
                    &glyph_ctm,
                    viewport,
                    color,
                    FillRule::NonZero,
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
    fn fallback_font_skips_symbolic_fonts() {
        assert!(get_fallback_font("Symbol").is_none());
        assert!(get_fallback_font("ZapfDingbats").is_none());
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
}
