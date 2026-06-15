use crate::content::operation::Operand;
use crate::error::Result;
use crate::filters::{decode_stream_from_dict, decode_stream_lossless};
use crate::fonts::cmap::ToUnicodeCMap;
use crate::fonts::encoding::Encoding;
use crate::fonts::glyph_list::glyph_name_to_unicode;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

#[derive(Debug, Clone, PartialEq)]
pub enum FontSubtype {
    Type0,
    Type1,
    TrueType,
    Type3,
    CIDFontType0,
    CIDFontType2,
    Unknown,
}

pub fn detect_font_subtype(font_dict: &PdfDictionary) -> FontSubtype {
    match font_dict.get_name("Subtype") {
        Some("Type0") => FontSubtype::Type0,
        Some("Type1") => FontSubtype::Type1,
        Some("TrueType") => FontSubtype::TrueType,
        Some("Type3") => FontSubtype::Type3,
        Some("CIDFontType0") => FontSubtype::CIDFontType0,
        Some("CIDFontType2") => FontSubtype::CIDFontType2,
        _ => FontSubtype::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FontType {
    Type1,
    MMType1,
    TrueType,
    Type3,
    Type0,
    CIDFontType0,
    CIDFontType2,
    Unknown(String),
}

impl FontType {
    pub fn from_name(s: &str) -> Self {
        match s {
            "Type1" => FontType::Type1,
            "MMType1" => FontType::MMType1,
            "TrueType" => FontType::TrueType,
            "Type3" => FontType::Type3,
            "Type0" => FontType::Type0,
            "CIDFontType0" => FontType::CIDFontType0,
            "CIDFontType2" => FontType::CIDFontType2,
            other => FontType::Unknown(other.to_string()),
        }
    }

    pub fn is_cid(&self) -> bool {
        matches!(
            self,
            FontType::Type0 | FontType::CIDFontType0 | FontType::CIDFontType2
        )
    }
}

pub struct FontResolver {
    font_type: FontType,
    to_unicode: Option<ToUnicodeCMap>,
    encoding_table: Option<Vec<String>>,
    widths: Vec<f64>,
    first_char: u32,
    last_char: u32,
    descendant_font: Option<PdfDictionary>,
    default_width: f64,
    code_size: u8,
    /// Writing mode of the font's encoding CMap: 0 = horizontal (glyphs advance
    /// left-to-right), 1 = vertical (glyphs advance top-to-bottom, columns
    /// arranged right-to-left). Only Type0 (composite) fonts can be vertical;
    /// every simple font is horizontal. Derived from the `/Encoding` CMap's
    /// `/WMode` entry, or from a predefined CMap name's `-V`/`-H` suffix
    /// (`Identity-V` ⇒ vertical). See PDF 32000-1 §9.7.4.3.
    wmode: u8,
}

impl FontResolver {
    pub fn new(font_dict: &PdfDictionary, reader: &PdfReader) -> Self {
        Self::build(font_dict, Some(reader))
    }

    pub fn new_from_dict_only(font_dict: &PdfDictionary) -> Self {
        Self::build(font_dict, None)
    }

    pub fn decode_string(&self, bytes: &[u8]) -> String {
        let mut result = String::new();
        let mut idx = 0usize;
        let code_size = self.code_size.max(1);
        while idx < bytes.len() {
            let code = if code_size == 2 {
                let high = bytes[idx];
                let low = bytes.get(idx + 1).copied().unwrap_or(0);
                idx += 2;
                (u16::from(high) << 8) | u16::from(low)
            } else {
                let code = u16::from(bytes[idx]);
                idx += 1;
                code
            };

            if let Some(text) = self.to_unicode.as_ref().and_then(|cmap| cmap.lookup(code)) {
                result.push_str(text);
                continue;
            }

            let glyph_name = self
                .encoding_table
                .as_ref()
                .and_then(|table| table.get(code as usize))
                .map(String::as_str)
                .unwrap_or(".notdef");

            if glyph_name != ".notdef" {
                if let Some(ch) = glyph_name_to_unicode(glyph_name) {
                    result.push_str(&expand_ligature(ch));
                    continue;
                }
            }

            if let Some(ch) = char::from_u32(u32::from(code)) {
                if !ch.is_control() || ch.is_whitespace() {
                    result.push(ch);
                    continue;
                }
            }
            log::warn!("font decode produced replacement character for code {code:#06X}");
            result.push('\u{FFFD}');
        }
        result
    }

    pub fn decode_char(&self, code: u16) -> String {
        let bytes = if self.code_size == 2 {
            vec![(code >> 8) as u8, (code & 0xFF) as u8]
        } else {
            vec![code as u8]
        };
        self.decode_string(&bytes)
    }

    pub fn code_size(&self) -> u8 {
        self.code_size
    }

    pub fn is_space_code(&self, code: u16) -> bool {
        if self.code_size == 1 && code == 0x0020 {
            return true;
        }
        self.decode_char(code) == " "
    }

    pub fn glyph_width(&self, char_code: u16) -> f64 {
        if let Some(descendant_font) = &self.descendant_font {
            return lookup_cid_width(u32::from(char_code), descendant_font);
        }

        let index = u32::from(char_code);
        if index >= self.first_char && index <= self.last_char {
            let i = (index - self.first_char) as usize;
            self.widths.get(i).copied().unwrap_or(self.default_width)
        } else {
            self.default_width
        }
    }

    pub fn font_type(&self) -> &FontType {
        &self.font_type
    }

    /// Writing mode of the font: `false` = horizontal, `true` = vertical.
    /// Vertical text advances glyphs top-to-bottom and arranges columns
    /// right-to-left. Driven by the encoding CMap's WMode (PDF 32000-1 §9.7.4.3),
    /// never by the text matrix. Only Type0 fonts are ever vertical.
    pub fn is_vertical(&self) -> bool {
        self.wmode == 1
    }

    /// Vertical glyph metrics (W2) for the given CID, as `(w1y, v_x, v_y)` in
    /// glyph space (1000-unit em), per PDF 32000-1 §9.7.4.3:
    /// - `w1y` is the vertical displacement (the glyph's advance height, normally
    ///   negative since vertical writing proceeds downward),
    /// - `(v_x, v_y)` is the position vector from the glyph's horizontal origin
    ///   to its vertical origin.
    ///
    /// Falls back to the descendant font's `/DW2` (default `[880 -1000]`) when the
    /// CID has no explicit `/W2` entry. Returns the spec defaults for a font with
    /// no descendant (`v_y = 880`, `w1y = -1000`, `v_x = w0/2`).
    pub fn vertical_metrics(&self, char_code: u16) -> (f64, f64, f64) {
        let cid = u32::from(char_code);
        let w0 = self.glyph_width(char_code);
        match &self.descendant_font {
            Some(desc) => lookup_cid_vertical(cid, w0, desc),
            None => (-1000.0, w0 / 2.0, 880.0),
        }
    }

    fn build(font_dict: &PdfDictionary, reader: Option<&PdfReader>) -> Self {
        let font_type = font_dict
            .get_name("Subtype")
            .map(FontType::from_name)
            .unwrap_or_else(|| FontType::Unknown("Unknown".to_string()));
        let to_unicode = parse_to_unicode(font_dict, reader);
        let encoding_table = if font_type.is_cid() {
            None
        } else {
            Some(build_encoding_table(font_dict, reader, &font_type))
        };
        let descendant_font = if matches!(font_type, FontType::Type0) {
            get_descendant_font_optional(font_dict, reader)
        } else {
            None
        };
        let first_char = font_dict
            .get_integer("FirstChar")
            .filter(|value| *value >= 0)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let last_char = font_dict
            .get_integer("LastChar")
            .filter(|value| *value >= 0)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(255);
        let widths = parse_widths(font_dict, first_char, last_char);
        let default_width = if font_type.is_cid() {
            descendant_font
                .as_ref()
                .and_then(|dict| dict.get("DW"))
                .and_then(PdfObject::as_number)
                .unwrap_or(1000.0)
        } else if widths.is_empty() {
            500.0
        } else {
            widths.iter().sum::<f64>() / widths.len() as f64
        };
        let code_size = if font_type.is_cid() {
            to_unicode
                .as_ref()
                .map(ToUnicodeCMap::code_size)
                .unwrap_or(2)
        } else {
            1
        };

        // Writing mode: only composite (Type0) fonts can be vertical, and only
        // via their /Encoding CMap. Simple fonts are always horizontal.
        let wmode = if matches!(font_type, FontType::Type0) {
            detect_wmode(font_dict, reader)
        } else {
            0
        };

        Self {
            font_type,
            to_unicode,
            encoding_table,
            widths,
            first_char,
            last_char,
            descendant_font,
            default_width,
            code_size,
            wmode,
        }
    }
}

pub fn get_descendant_font(
    type0_dict: &PdfDictionary,
    reader: &PdfReader,
) -> Option<PdfDictionary> {
    get_descendant_font_optional(type0_dict, Some(reader))
}

pub fn lookup_cid_width(cid: u32, desc_dict: &PdfDictionary) -> f64 {
    let dw = desc_dict
        .get("DW")
        .and_then(PdfObject::as_number)
        .unwrap_or(1000.0);

    let Some(w_arr) = desc_dict.get("W").and_then(PdfObject::as_array) else {
        return dw;
    };

    let mut idx = 0usize;
    while idx < w_arr.len() {
        let Some(c1) = w_arr[idx]
            .as_number()
            .filter(|value| *value >= 0.0)
            .map(|value| value as u32)
        else {
            break;
        };
        idx += 1;
        if idx >= w_arr.len() {
            break;
        }

        match &w_arr[idx] {
            PdfObject::Array(widths) => {
                for (offset, width_obj) in widths.iter().enumerate() {
                    if c1.saturating_add(offset as u32) == cid {
                        if let Some(width) = width_obj.as_number() {
                            return width;
                        }
                    }
                }
                idx += 1;
            }
            _ => {
                let Some(c2) = w_arr[idx]
                    .as_number()
                    .filter(|value| *value >= 0.0)
                    .map(|value| value as u32)
                else {
                    break;
                };
                idx += 1;
                if idx >= w_arr.len() {
                    break;
                }
                let Some(width) = w_arr[idx].as_number() else {
                    break;
                };
                idx += 1;

                if cid >= c1 && cid <= c2 {
                    return width;
                }
            }
        }
    }

    dw
}

/// Determine the writing mode (0 = horizontal, 1 = vertical) of a Type0 font
/// from its `/Encoding`. The encoding is either a predefined CMap name (whose
/// `-V`/`-H` suffix, or `Identity-V`/`Identity-H`, declares the mode) or an
/// embedded CMap stream carrying a `/WMode` entry. Defaults to horizontal.
fn detect_wmode(font_dict: &PdfDictionary, reader: Option<&PdfReader>) -> u8 {
    let Some(encoding) = font_dict.get("Encoding") else {
        return 0;
    };
    let resolved = resolve_optional(encoding, reader).unwrap_or_else(|_| encoding.clone());
    match resolved {
        // Predefined CMap referenced by name: the name's suffix is authoritative.
        PdfObject::Name(name) => wmode_from_cmap_name(&name),
        // Embedded CMap stream: read its /WMode key, falling back to the
        // CMapName / name suffix inside the decoded program.
        PdfObject::Stream { dict, raw } => {
            if let Some(w) = dict.get_integer("WMode") {
                return u8::from(w == 1);
            }
            let decoded = match reader {
                Some(reader) => {
                    let stream = PdfObject::Stream { dict, raw };
                    decode_stream_lossless(&stream, reader)
                        .map(|d| d.data)
                        .unwrap_or_default()
                }
                None => decode_stream_from_dict(&dict, &raw).unwrap_or_default(),
            };
            wmode_from_cmap_bytes(&decoded)
        }
        _ => 0,
    }
}

/// Vertical iff a predefined CMap name ends in `-V` (e.g. `Identity-V`,
/// `UniGB-UCS2-V`, `UniJIS-UCS2-V`). All `-H` names and anything else are
/// horizontal.
fn wmode_from_cmap_name(name: &str) -> u8 {
    u8::from(name.ends_with("-V"))
}

/// Scan a decoded CMap program for an explicit `/WMode 1` declaration or a
/// `/CMapName` ending in `-V`. Conservative: defaults to horizontal.
fn wmode_from_cmap_bytes(bytes: &[u8]) -> u8 {
    let text = String::from_utf8_lossy(bytes);
    if let Some(idx) = text.find("/WMode") {
        let rest = text[idx + "/WMode".len()..].trim_start();
        if rest.starts_with('1') {
            return 1;
        }
        if rest.starts_with('0') {
            return 0;
        }
    }
    if let Some(idx) = text.find("/CMapName") {
        let rest = &text[idx..];
        // Match a token like `/CMapName /Something-V def`.
        if let Some(slash) = rest[1..].find('/') {
            let after = &rest[1 + slash + 1..];
            let token: String = after
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '/')
                .collect();
            return wmode_from_cmap_name(token.trim());
        }
    }
    0
}

/// Look up vertical metrics `(w1y, v_x, v_y)` for a CID from a CIDFont's `/W2`
/// array, with `/DW2` as the per-font default. See PDF 32000-1 §9.7.4.3.
///
/// `/DW2` is `[v_y w1y]` (default `[880 -1000]`): `v_y` is the y of the position
/// vector and `w1y` the default vertical displacement; the default `v_x` is
/// `w0/2` (half the glyph's horizontal width).
///
/// `/W2` entries come in two forms:
/// - `c [w1y_1 v1x_1 v1y_1  w1y_2 v1x_2 v1y_2  …]` — consecutive CIDs from `c`,
///   three numbers each.
/// - `c_first c_last w1y v1x v1y` — a CID range sharing one triple.
pub fn lookup_cid_vertical(cid: u32, w0: f64, desc_dict: &PdfDictionary) -> (f64, f64, f64) {
    let (def_vy, def_w1y) = desc_dict
        .get("DW2")
        .and_then(PdfObject::as_array)
        .and_then(|a| {
            let vy = a.first().and_then(PdfObject::as_number)?;
            let w1y = a.get(1).and_then(PdfObject::as_number)?;
            Some((vy, w1y))
        })
        .unwrap_or((880.0, -1000.0));
    let default = (def_w1y, w0 / 2.0, def_vy);

    let Some(w2) = desc_dict.get("W2").and_then(PdfObject::as_array) else {
        return default;
    };

    let mut idx = 0usize;
    while idx < w2.len() {
        let Some(c1) = w2[idx]
            .as_number()
            .filter(|v| *v >= 0.0)
            .map(|v| v as u32)
        else {
            break;
        };
        idx += 1;
        if idx >= w2.len() {
            break;
        }

        match &w2[idx] {
            PdfObject::Array(triples) => {
                // c [w1y vx vy  w1y vx vy …]
                let n = triples.len() / 3;
                for k in 0..n {
                    if c1.saturating_add(k as u32) == cid {
                        let w1y = triples[k * 3].as_number().unwrap_or(def_w1y);
                        let vx = triples[k * 3 + 1].as_number().unwrap_or(w0 / 2.0);
                        let vy = triples[k * 3 + 2].as_number().unwrap_or(def_vy);
                        return (w1y, vx, vy);
                    }
                }
                idx += 1;
            }
            _ => {
                // c_first c_last w1y vx vy
                let Some(c2) = w2[idx]
                    .as_number()
                    .filter(|v| *v >= 0.0)
                    .map(|v| v as u32)
                else {
                    break;
                };
                idx += 1;
                if idx + 2 > w2.len() {
                    break;
                }
                let w1y = w2[idx].as_number().unwrap_or(def_w1y);
                let vx = w2[idx + 1].as_number().unwrap_or(w0 / 2.0);
                let vy = w2[idx + 2].as_number().unwrap_or(def_vy);
                idx += 3;
                if cid >= c1 && cid <= c2 {
                    return (w1y, vx, vy);
                }
            }
        }
    }

    default
}

pub(crate) fn expand_ligature(ch: char) -> String {
    match ch {
        '\u{FB00}' => "ff".to_string(),
        '\u{FB01}' => "fi".to_string(),
        '\u{FB02}' => "fl".to_string(),
        '\u{FB03}' => "ffi".to_string(),
        '\u{FB04}' => "ffl".to_string(),
        '\u{FB05}' | '\u{FB06}' => "st".to_string(),
        other => other.to_string(),
    }
}

fn parse_to_unicode(
    font_dict: &PdfDictionary,
    reader: Option<&PdfReader>,
) -> Option<ToUnicodeCMap> {
    let object = font_dict.get("ToUnicode")?;
    let resolved = resolve_optional(object, reader).ok()?;
    let PdfObject::Stream { dict, raw } = resolved else {
        return None;
    };
    let decoded = match reader {
        Some(reader) => {
            let stream = PdfObject::Stream { dict, raw };
            decode_stream_lossless(&stream, reader).ok()?.data
        }
        None => decode_stream_from_dict(&dict, &raw).ok()?,
    };
    Some(ToUnicodeCMap::parse(&decoded))
}

fn build_encoding_table(
    font_dict: &PdfDictionary,
    reader: Option<&PdfReader>,
    font_type: &FontType,
) -> Vec<String> {
    // Symbol and ZapfDingbats are standard-14 fonts with their own built-in
    // encodings (spec Appendix D). When the BaseFont names one of them, that
    // encoding — not StandardEncoding/MacRoman — is the implicit default, used
    // both when /Encoding is absent and as the base for any /Differences.
    let symbolic_base = symbolic_builtin_encoding(font_dict);
    let default_base = symbolic_base.unwrap_or(match font_type {
        FontType::TrueType => "MacRomanEncoding",
        _ => "StandardEncoding",
    });

    let Some(encoding_obj) = font_dict.get("Encoding") else {
        return table_for(default_base);
    };

    let resolved = resolve_optional(encoding_obj, reader).unwrap_or_else(|_| encoding_obj.clone());
    match resolved {
        PdfObject::Name(name) => table_for(&name),
        PdfObject::Dictionary(dict) => {
            let base = dict.get_name("BaseEncoding").unwrap_or(default_base);
            let diffs = dict
                .get_array("Differences")
                .map(pdf_objects_to_operands)
                .unwrap_or_default();
            if diffs.is_empty() {
                table_for(base)
            } else {
                Encoding::apply_differences(base, &diffs)
            }
        }
        _ => table_for(default_base),
    }
}

/// If the font's `/BaseFont` is the Symbol or ZapfDingbats standard-14 font,
/// return the name of its built-in encoding (so [`Encoding::lookup`] uses the
/// Appendix D tables). A subset prefix like `ABCDEF+Symbol` is handled.
fn symbolic_builtin_encoding(font_dict: &PdfDictionary) -> Option<&'static str> {
    let base = font_dict.get_name("BaseFont")?;
    let base = base.rsplit('+').next().unwrap_or(base);
    let lower = base.to_ascii_lowercase();
    if lower.contains("zapfdingbats") || lower.contains("dingbats") {
        Some("ZapfDingbatsEncoding")
    } else if lower == "symbol" || lower.starts_with("symbol") || lower.contains("-symbol") {
        Some("SymbolEncoding")
    } else {
        None
    }
}

fn table_for(name: &str) -> Vec<String> {
    (0u8..=255)
        .map(|byte| Encoding::lookup(name, byte).to_string())
        .collect()
}

fn pdf_objects_to_operands(objects: &[PdfObject]) -> Vec<Operand> {
    objects
        .iter()
        .filter_map(|object| match object {
            PdfObject::Integer(value) => Some(Operand::Integer(*value)),
            PdfObject::Real(value) => Some(Operand::Real(*value)),
            PdfObject::Name(value) => Some(Operand::Name(value.clone())),
            PdfObject::String(value) => Some(Operand::String(value.clone())),
            PdfObject::Array(items) => Some(Operand::Array(pdf_objects_to_operands(items))),
            PdfObject::Boolean(value) => Some(Operand::Boolean(*value)),
            _ => None,
        })
        .collect()
}

fn parse_widths(font_dict: &PdfDictionary, first_char: u32, last_char: u32) -> Vec<f64> {
    let Some(widths) = font_dict.get_array("Widths") else {
        return Vec::new();
    };
    let wanted_len = last_char
        .checked_sub(first_char)
        .and_then(|value| value.checked_add(1))
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let mut values: Vec<f64> = widths.iter().filter_map(PdfObject::as_number).collect();
    if wanted_len > 0 {
        values.truncate(wanted_len);
    }
    values
}

fn get_descendant_font_optional(
    type0_dict: &PdfDictionary,
    reader: Option<&PdfReader>,
) -> Option<PdfDictionary> {
    let descendants = match type0_dict.get("DescendantFonts")? {
        PdfObject::Array(items) => items.clone(),
        PdfObject::Reference { number, generation } => {
            let reader = reader?;
            match reader.get_and_resolve(*number, *generation).ok()? {
                PdfObject::Array(items) => items,
                _ => return None,
            }
        }
        _ => return None,
    };

    match descendants.first()?.clone() {
        PdfObject::Dictionary(dict) => Some(dict),
        PdfObject::Reference { number, generation } => {
            let reader = reader?;
            match reader.get_and_resolve(number, generation).ok()? {
                PdfObject::Dictionary(dict) => Some(dict),
                _ => None,
            }
        }
        _ => None,
    }
}

fn resolve_optional(object: &PdfObject, reader: Option<&PdfReader>) -> Result<PdfObject> {
    match reader {
        Some(reader) => reader.resolve(object.clone()),
        None => Ok(object.clone()),
    }
}

#[cfg(test)]
mod cid_font_tests {
    use super::*;

    #[test]
    fn lookup_cid_width_returns_dw_when_w_absent() {
        let mut dict = PdfDictionary::empty();
        dict.insert("DW", PdfObject::Integer(1000));
        assert_eq!(lookup_cid_width(65, &dict), 1000.0);
    }

    #[test]
    fn lookup_cid_width_defaults_to_1000_when_absent() {
        assert_eq!(lookup_cid_width(65, &PdfDictionary::empty()), 1000.0);
    }

    #[test]
    fn lookup_cid_width_format_array() {
        let mut dict = PdfDictionary::empty();
        dict.insert("DW", PdfObject::Integer(1000));
        dict.insert(
            "W",
            PdfObject::Array(vec![
                PdfObject::Integer(65),
                PdfObject::Array(vec![
                    PdfObject::Integer(722),
                    PdfObject::Integer(667),
                    PdfObject::Integer(611),
                ]),
            ]),
        );
        assert_eq!(lookup_cid_width(65, &dict), 722.0);
        assert_eq!(lookup_cid_width(66, &dict), 667.0);
        assert_eq!(lookup_cid_width(68, &dict), 1000.0);
    }

    #[test]
    fn lookup_cid_width_format_range() {
        let mut dict = PdfDictionary::empty();
        dict.insert("DW", PdfObject::Integer(1000));
        dict.insert(
            "W",
            PdfObject::Array(vec![
                PdfObject::Integer(100),
                PdfObject::Integer(200),
                PdfObject::Integer(400),
            ]),
        );
        assert_eq!(lookup_cid_width(150, &dict), 400.0);
        assert_eq!(lookup_cid_width(50, &dict), 1000.0);
    }

    #[test]
    fn lookup_cid_width_mixed_formats() {
        let mut dict = PdfDictionary::empty();
        dict.insert("DW", PdfObject::Integer(1000));
        dict.insert(
            "W",
            PdfObject::Array(vec![
                PdfObject::Integer(32),
                PdfObject::Array(vec![PdfObject::Integer(277), PdfObject::Integer(333)]),
                PdfObject::Integer(65),
                PdfObject::Integer(90),
                PdfObject::Integer(722),
            ]),
        );
        assert_eq!(lookup_cid_width(32, &dict), 277.0);
        assert_eq!(lookup_cid_width(33, &dict), 333.0);
        assert_eq!(lookup_cid_width(70, &dict), 722.0);
        assert_eq!(lookup_cid_width(10, &dict), 1000.0);
    }

    #[test]
    fn lookup_cid_width_empty_w_uses_dw() {
        let mut dict = PdfDictionary::empty();
        dict.insert("DW", PdfObject::Integer(500));
        dict.insert("W", PdfObject::Array(vec![]));
        assert_eq!(lookup_cid_width(100, &dict), 500.0);
    }

    #[test]
    fn detect_font_subtype_identifies_type0() {
        let mut dict = PdfDictionary::empty();
        dict.insert("Subtype", PdfObject::Name("Type0".to_string()));
        assert_eq!(detect_font_subtype(&dict), FontSubtype::Type0);
    }

    #[test]
    fn wmode_name_suffix_detection() {
        assert_eq!(wmode_from_cmap_name("Identity-V"), 1);
        assert_eq!(wmode_from_cmap_name("Identity-H"), 0);
        assert_eq!(wmode_from_cmap_name("UniJIS-UCS2-V"), 1);
        assert_eq!(wmode_from_cmap_name("UniGB-UCS2-H"), 0);
        assert_eq!(wmode_from_cmap_name("90ms-RKSJ-V"), 1);
        assert_eq!(wmode_from_cmap_name("WeirdName"), 0);
    }

    #[test]
    fn wmode_from_embedded_cmap_bytes() {
        assert_eq!(wmode_from_cmap_bytes(b"/WMode 1 def"), 1);
        assert_eq!(wmode_from_cmap_bytes(b"/WMode 0 def"), 0);
        assert_eq!(wmode_from_cmap_bytes(b"/CMapName /Adobe-Japan1-V def"), 1);
        assert_eq!(wmode_from_cmap_bytes(b"no wmode here"), 0);
    }

    #[test]
    fn type0_identity_v_font_is_vertical() {
        let mut desc = PdfDictionary::empty();
        desc.insert("Subtype", PdfObject::Name("CIDFontType2".to_string()));
        let mut dict = PdfDictionary::empty();
        dict.insert("Subtype", PdfObject::Name("Type0".to_string()));
        dict.insert("Encoding", PdfObject::Name("Identity-V".to_string()));
        dict.insert(
            "DescendantFonts",
            PdfObject::Array(vec![PdfObject::Dictionary(desc)]),
        );
        let resolver = FontResolver::new_from_dict_only(&dict);
        assert!(resolver.is_vertical(), "Identity-V should be vertical");
    }

    #[test]
    fn type0_identity_h_font_is_horizontal() {
        let mut dict = PdfDictionary::empty();
        dict.insert("Subtype", PdfObject::Name("Type0".to_string()));
        dict.insert("Encoding", PdfObject::Name("Identity-H".to_string()));
        let resolver = FontResolver::new_from_dict_only(&dict);
        assert!(!resolver.is_vertical(), "Identity-H should be horizontal");
    }

    #[test]
    fn simple_font_is_never_vertical() {
        let mut dict = PdfDictionary::empty();
        dict.insert("Subtype", PdfObject::Name("Type1".to_string()));
        dict.insert("Encoding", PdfObject::Name("WinAnsiEncoding".to_string()));
        let resolver = FontResolver::new_from_dict_only(&dict);
        assert!(!resolver.is_vertical());
    }

    #[test]
    fn lookup_cid_vertical_uses_dw2_default() {
        let dict = PdfDictionary::empty();
        let (w1y, vx, vy) = lookup_cid_vertical(5, 1000.0, &dict);
        assert_eq!(w1y, -1000.0);
        assert_eq!(vx, 500.0);
        assert_eq!(vy, 880.0);
    }

    #[test]
    fn lookup_cid_vertical_honors_explicit_dw2() {
        let mut dict = PdfDictionary::empty();
        dict.insert(
            "DW2",
            PdfObject::Array(vec![PdfObject::Integer(900), PdfObject::Integer(-1100)]),
        );
        let (w1y, vx, vy) = lookup_cid_vertical(5, 1000.0, &dict);
        assert_eq!(w1y, -1100.0);
        assert_eq!(vx, 500.0);
        assert_eq!(vy, 900.0);
    }

    #[test]
    fn lookup_cid_vertical_w2_array_form() {
        let mut dict = PdfDictionary::empty();
        dict.insert(
            "W2",
            PdfObject::Array(vec![
                PdfObject::Integer(10),
                PdfObject::Array(vec![
                    PdfObject::Integer(-900),
                    PdfObject::Integer(450),
                    PdfObject::Integer(800),
                    PdfObject::Integer(-950),
                    PdfObject::Integer(460),
                    PdfObject::Integer(810),
                ]),
            ]),
        );
        assert_eq!(lookup_cid_vertical(10, 1000.0, &dict), (-900.0, 450.0, 800.0));
        assert_eq!(lookup_cid_vertical(11, 1000.0, &dict), (-950.0, 460.0, 810.0));
        assert_eq!(lookup_cid_vertical(12, 1000.0, &dict), (-1000.0, 500.0, 880.0));
    }

    #[test]
    fn lookup_cid_vertical_w2_range_form() {
        let mut dict = PdfDictionary::empty();
        dict.insert(
            "W2",
            PdfObject::Array(vec![
                PdfObject::Integer(100),
                PdfObject::Integer(200),
                PdfObject::Integer(-880),
                PdfObject::Integer(500),
                PdfObject::Integer(880),
            ]),
        );
        assert_eq!(lookup_cid_vertical(150, 1000.0, &dict), (-880.0, 500.0, 880.0));
        assert_eq!(lookup_cid_vertical(50, 1000.0, &dict), (-1000.0, 500.0, 880.0));
    }

    #[test]
    fn detect_font_subtype_identifies_truetype() {
        let mut dict = PdfDictionary::empty();
        dict.insert("Subtype", PdfObject::Name("TrueType".to_string()));
        assert_eq!(detect_font_subtype(&dict), FontSubtype::TrueType);
    }

    #[test]
    fn font_subtype_enum_covers_common_pdf_subtypes() {
        let mut type1 = PdfDictionary::empty();
        type1.insert("Subtype", PdfObject::Name("Type1".to_string()));
        assert_eq!(detect_font_subtype(&type1), FontSubtype::Type1);

        let mut cid2 = PdfDictionary::empty();
        cid2.insert("Subtype", PdfObject::Name("CIDFontType2".to_string()));
        assert_eq!(detect_font_subtype(&cid2), FontSubtype::CIDFontType2);

        assert_eq!(
            detect_font_subtype(&PdfDictionary::empty()),
            FontSubtype::Unknown
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn dict(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn type1_standard_encoding_decodes_strings() {
        let font = dict(&[
            ("Type", PdfObject::Name("Font".to_string())),
            ("Subtype", PdfObject::Name("Type1".to_string())),
            ("Encoding", PdfObject::Name("StandardEncoding".to_string())),
            ("FirstChar", PdfObject::Integer(65)),
            ("LastChar", PdfObject::Integer(67)),
            (
                "Widths",
                PdfObject::Array(vec![
                    PdfObject::Integer(600),
                    PdfObject::Integer(600),
                    PdfObject::Integer(600),
                ]),
            ),
        ]);
        let resolver = FontResolver::new_from_dict_only(&font);
        assert_eq!(resolver.decode_string(b"ABC"), "ABC");
        assert_eq!(resolver.decode_string(b"\xAE"), "fi");
    }

    #[test]
    fn win_ansi_encoding_decodes_strings() {
        let font = dict(&[
            ("Subtype", PdfObject::Name("Type1".to_string())),
            ("Encoding", PdfObject::Name("WinAnsiEncoding".to_string())),
        ]);
        let resolver = FontResolver::new_from_dict_only(&font);
        assert_eq!(resolver.decode_string(&[0x80]), "€");
        assert_eq!(resolver.decode_string(&[0x96]), "–");
    }

    #[test]
    fn to_unicode_overrides_encoding() {
        let cmap = b"
        begincmap
        1 beginbfchar
        <41> <4E2D>
        endbfchar
        endcmap
        ";
        let font = dict(&[
            ("Subtype", PdfObject::Name("Type1".to_string())),
            ("Encoding", PdfObject::Name("StandardEncoding".to_string())),
            (
                "ToUnicode",
                PdfObject::Stream {
                    dict: PdfDictionary::empty(),
                    raw: cmap.to_vec(),
                },
            ),
        ]);
        let resolver = FontResolver::new_from_dict_only(&font);
        assert_eq!(resolver.decode_string(b"A"), "中");
    }
}
