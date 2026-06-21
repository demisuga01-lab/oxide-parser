use rustybuzz::{script, Direction, Script, UnicodeBuffer};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShapedGlyph {
    pub gid: u16,
    pub advance: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

pub(crate) fn shape_run(font_bytes: &[u8], text: &str, upem: f64) -> Option<Vec<ShapedGlyph>> {
    if text.is_empty() || upem <= 0.0 || !needs_shaping(text) {
        return None;
    }
    let script = dominant_shaping_script(text)?;
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_script(script);
    buffer.set_direction(direction_for_script(script));

    let face = rustybuzz::Face::from_slice(font_bytes, 0)?;
    let shaped = rustybuzz::shape(&face, &[], buffer);
    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();
    if infos.is_empty() || infos.len() != positions.len() {
        return None;
    }

    Some(
        infos
            .iter()
            .zip(positions.iter())
            .map(|(info, pos)| ShapedGlyph {
                gid: info.glyph_id.min(u32::from(u16::MAX)) as u16,
                advance: f64::from(pos.x_advance) / upem * 1000.0,
                offset_x: f64::from(pos.x_offset) / upem * 1000.0,
                offset_y: f64::from(pos.y_offset) / upem * 1000.0,
            })
            .collect(),
    )
}

pub(crate) fn needs_shaping(text: &str) -> bool {
    text.chars().any(script_for_char_requires_shaping)
}

fn dominant_shaping_script(text: &str) -> Option<Script> {
    text.chars().find_map(script_for_char)
}

fn script_for_char(ch: char) -> Option<Script> {
    let code = ch as u32;
    if is_arabic_codepoint(code) {
        return Some(script::ARABIC);
    }
    if is_devanagari_codepoint(code) {
        return Some(script::DEVANAGARI);
    }
    if is_bengali_codepoint(code) {
        return Some(script::BENGALI);
    }
    if is_gurmukhi_codepoint(code) {
        return Some(script::GURMUKHI);
    }
    if is_gujarati_codepoint(code) {
        return Some(script::GUJARATI);
    }
    if is_oriya_codepoint(code) {
        return Some(script::ORIYA);
    }
    if is_tamil_codepoint(code) {
        return Some(script::TAMIL);
    }
    if is_telugu_codepoint(code) {
        return Some(script::TELUGU);
    }
    if is_kannada_codepoint(code) {
        return Some(script::KANNADA);
    }
    if is_malayalam_codepoint(code) {
        return Some(script::MALAYALAM);
    }
    None
}

fn script_for_char_requires_shaping(ch: char) -> bool {
    script_for_char(ch).is_some()
}

fn direction_for_script(script: Script) -> Direction {
    if script == script::ARABIC {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    }
}

fn is_arabic_codepoint(code: u32) -> bool {
    matches!(
        code,
        0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
    )
}

fn is_devanagari_codepoint(code: u32) -> bool {
    matches!(code, 0x0900..=0x097F | 0xA8E0..=0xA8FF)
}

fn is_bengali_codepoint(code: u32) -> bool {
    matches!(code, 0x0980..=0x09FF)
}

fn is_gurmukhi_codepoint(code: u32) -> bool {
    matches!(code, 0x0A00..=0x0A7F)
}

fn is_gujarati_codepoint(code: u32) -> bool {
    matches!(code, 0x0A80..=0x0AFF)
}

fn is_oriya_codepoint(code: u32) -> bool {
    matches!(code, 0x0B00..=0x0B7F)
}

fn is_tamil_codepoint(code: u32) -> bool {
    matches!(code, 0x0B80..=0x0BFF)
}

fn is_telugu_codepoint(code: u32) -> bool {
    matches!(code, 0x0C00..=0x0C7F)
}

fn is_kannada_codepoint(code: u32) -> bool {
    matches!(code, 0x0C80..=0x0CFF)
}

fn is_malayalam_codepoint(code: u32) -> bool {
    matches!(code, 0x0D00..=0x0D7F)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::font_rasterizer::get_fallback_font;

    #[test]
    fn detects_arabic_and_indic_shaping_scripts() {
        assert!(needs_shaping("\u{0633}\u{0644}\u{0627}\u{0645}"));
        assert!(needs_shaping("\u{0915}\u{094D}\u{0937}"));
        assert!(!needs_shaping("Plain Latin"));
    }

    #[test]
    fn rustybuzz_shapes_arabic_to_glyphs() {
        let Some(font) = get_fallback_font("Symbol") else {
            return;
        };
        let shaped = shape_run(font, "\u{0633}\u{0644}\u{0627}\u{0645}", 2048.0)
            .expect("DejaVu fallback should shape Arabic");

        assert!(!shaped.is_empty());
        assert!(shaped.iter().all(|glyph| glyph.gid > 0));
    }
}
