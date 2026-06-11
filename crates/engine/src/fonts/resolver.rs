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
    let default_base = match font_type {
        FontType::TrueType => "MacRomanEncoding",
        _ => "StandardEncoding",
    };

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
mod mega23_tests {
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
