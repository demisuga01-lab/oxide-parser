use crate::content::operation::{ContentOperation, Operand};
use crate::content::tokenizer::{ContentToken, ContentTokenizer};
use crate::error::Result;
use crate::object::{PdfDictionary, PdfObject};

pub struct ContentParser;

#[derive(Debug)]
enum StackItem {
    Operand(Operand),
    ArrayStart,
}

impl ContentParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse all tokens from the byte slice into operations.
    /// Token errors are logged as warnings; parsing continues.
    pub fn parse(data: &[u8]) -> Result<Vec<ContentOperation>> {
        Ok(Self::parse_tokens(ContentTokenizer::new(data)))
    }

    /// Same as [`ContentParser::parse`] but accepts a pre-built token iterator.
    pub fn parse_tokens(
        tokens: impl IntoIterator<Item = Result<ContentToken>>,
    ) -> Vec<ContentOperation> {
        let mut stack = Vec::new();
        let mut array_depth = 0u32;
        let mut operations = Vec::new();

        for token_result in tokens {
            let token = match token_result {
                Ok(token) => token,
                Err(err) => {
                    log::warn!("content token error: {err}");
                    continue;
                }
            };

            match token {
                ContentToken::ArrayStart | ContentToken::DictStart => {
                    stack.push(StackItem::ArrayStart);
                    array_depth = array_depth.saturating_add(1);
                }
                ContentToken::ArrayEnd | ContentToken::DictEnd => {
                    match collect_array(&mut stack) {
                        Some(array) => stack.push(StackItem::Operand(Operand::Array(array))),
                        None => log::warn!("content parser saw array/dict end without start"),
                    }
                    array_depth = array_depth.saturating_sub(1);
                }
                ContentToken::Operator(op) => {
                    let mut operands = drain_operands(&mut stack);
                    if op == "ID" {
                        operands = normalize_inline_image_operands(operands);
                    }
                    if array_depth > 0 {
                        log::warn!("operator '{op}' encountered before closing array");
                        array_depth = 0;
                    }
                    operations.push(ContentOperation::new(op, operands));
                }
                ContentToken::InlineImageData(bytes) => {
                    if !stack.is_empty() {
                        log::warn!("flushing operands before inline image data");
                        stack.clear();
                    }
                    operations.push(ContentOperation::new(
                        "inline_image_data",
                        vec![Operand::String(bytes)],
                    ));
                }
                other => match Option::<Operand>::from(other) {
                    Some(operand) => stack.push(StackItem::Operand(operand)),
                    None => log::warn!("content parser skipped non-operand token"),
                },
            }
        }

        if !stack.is_empty() {
            log::warn!(
                "trailing operands without operator: {:?}",
                stack
                    .into_iter()
                    .filter_map(|item| match item {
                        StackItem::Operand(operand) => Some(operand),
                        StackItem::ArrayStart => None,
                    })
                    .collect::<Vec<_>>()
            );
        }

        operations
    }
}

impl Default for ContentParser {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_array(stack: &mut Vec<StackItem>) -> Option<Vec<Operand>> {
    let mut values = Vec::new();
    while let Some(item) = stack.pop() {
        match item {
            StackItem::Operand(operand) => values.push(operand),
            StackItem::ArrayStart => {
                values.reverse();
                return Some(values);
            }
        }
    }
    None
}

fn drain_operands(stack: &mut Vec<StackItem>) -> Vec<Operand> {
    let mut operands = Vec::new();
    for item in stack.drain(..) {
        match item {
            StackItem::Operand(operand) => operands.push(operand),
            StackItem::ArrayStart => log::warn!("discarding unmatched array start before operator"),
        }
    }
    operands
}

pub fn expand_inline_image_keys(dict: &mut PdfDictionary) {
    let mut expanded = PdfDictionary::empty();
    for (key, value) in dict.entries() {
        let full_key = inline_image_full_key(key);
        expanded.insert(full_key, expand_inline_image_value(full_key, value.clone()));
    }
    *dict = expanded;
}

fn normalize_inline_image_operands(operands: Vec<Operand>) -> Vec<Operand> {
    let mut dict = PdfDictionary::empty();
    let mut iter = operands.into_iter().peekable();
    while let Some(operand) = iter.next() {
        let Operand::Name(key) = operand else {
            continue;
        };
        let Some(value) = iter.next() else {
            break;
        };
        if let Some(object) = operand_to_pdf_object(value) {
            dict.insert(key, object);
        }
    }

    expand_inline_image_keys(&mut dict);
    dict.entries()
        .flat_map(|(key, value)| {
            let mut out = vec![Operand::Name(key.clone())];
            if let Some(value) = pdf_object_to_operand(value) {
                out.push(value);
            }
            out
        })
        .collect()
}

fn inline_image_full_key(key: &str) -> &str {
    match key {
        "BPC" => "BitsPerComponent",
        "CS" => "ColorSpace",
        "D" => "Decode",
        "DP" => "DecodeParms",
        "F" => "Filter",
        "H" => "Height",
        "IM" => "ImageMask",
        "I" => "Interpolate",
        "W" => "Width",
        other => other,
    }
}

fn expand_inline_image_value(key: &str, value: PdfObject) -> PdfObject {
    match key {
        "ColorSpace" => expand_name_value(value, inline_image_color_space_name),
        "Filter" => expand_filter_value(value),
        _ => value,
    }
}

fn expand_filter_value(value: PdfObject) -> PdfObject {
    match value {
        PdfObject::Array(items) => {
            PdfObject::Array(items.into_iter().map(expand_filter_value).collect())
        }
        other => expand_name_value(other, inline_image_filter_name),
    }
}

fn expand_name_value(value: PdfObject, mapper: fn(&str) -> &str) -> PdfObject {
    match value {
        PdfObject::Name(name) => PdfObject::Name(mapper(&name).to_string()),
        other => other,
    }
}

fn inline_image_color_space_name(name: &str) -> &str {
    match name {
        "G" => "DeviceGray",
        "RGB" => "DeviceRGB",
        "CMYK" => "DeviceCMYK",
        "I" => "Indexed",
        other => other,
    }
}

fn inline_image_filter_name(name: &str) -> &str {
    match name {
        "AHx" => "ASCIIHexDecode",
        "A85" => "ASCII85Decode",
        "LZW" => "LZWDecode",
        "Fl" => "FlateDecode",
        "RL" => "RunLengthDecode",
        "CCF" => "CCITTFaxDecode",
        "DCT" => "DCTDecode",
        other => other,
    }
}

fn operand_to_pdf_object(operand: Operand) -> Option<PdfObject> {
    match operand {
        Operand::Integer(value) => Some(PdfObject::Integer(value)),
        Operand::Real(value) => Some(PdfObject::Real(value)),
        Operand::Boolean(value) => Some(PdfObject::Boolean(value)),
        Operand::Name(value) => Some(PdfObject::Name(value)),
        Operand::String(value) => Some(PdfObject::String(value)),
        Operand::Array(items) => Some(PdfObject::Array(
            items
                .into_iter()
                .filter_map(operand_to_pdf_object)
                .collect(),
        )),
    }
}

fn pdf_object_to_operand(object: &PdfObject) -> Option<Operand> {
    match object {
        PdfObject::Integer(value) => Some(Operand::Integer(*value)),
        PdfObject::Real(value) => Some(Operand::Real(*value)),
        PdfObject::Boolean(value) => Some(Operand::Boolean(*value)),
        PdfObject::Name(value) => Some(Operand::Name(value.clone())),
        PdfObject::String(value) => Some(Operand::String(value.clone())),
        PdfObject::Array(items) => Some(Operand::Array(
            items.iter().filter_map(pdf_object_to_operand).collect(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_text_operations() {
        let operations = ContentParser::parse(b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET").unwrap();
        assert_eq!(operations.len(), 5);
        assert_eq!(operations[0].operator, "BT");
        assert_eq!(operations[0].operands, vec![]);
        assert_eq!(operations[1].operator, "Tf");
        assert_eq!(
            operations[1].operands,
            vec![Operand::Name("F1".to_string()), Operand::Integer(12)]
        );
        assert_eq!(operations[2].operator, "Td");
        assert_eq!(
            operations[2].operands,
            vec![Operand::Integer(100), Operand::Integer(700)]
        );
        assert_eq!(operations[3].operator, "Tj");
        assert_eq!(
            operations[3].operands,
            vec![Operand::String(b"Hello".to_vec())]
        );
        assert_eq!(operations[4].operator, "ET");
        assert_eq!(operations[4].operands, vec![]);
    }

    #[test]
    fn parses_tj_array() {
        let operations = ContentParser::parse(b"[(Hello) 50 (World)] TJ").unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operator, "TJ");
        assert_eq!(
            operations[0].operands,
            vec![Operand::Array(vec![
                Operand::String(b"Hello".to_vec()),
                Operand::Integer(50),
                Operand::String(b"World".to_vec())
            ])]
        );
    }

    #[test]
    fn parses_setdash_operator() {
        let operations = ContentParser::parse(b"[3 5] 6 d").unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operator, "d");
        assert_eq!(
            operations[0].operands,
            vec![
                Operand::Array(vec![Operand::Integer(3), Operand::Integer(5)]),
                Operand::Integer(6)
            ]
        );
    }

    #[test]
    fn parses_color_operator() {
        let operations = ContentParser::parse(b"0.5 0.0 1.0 rg").unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operator, "rg");
        assert_eq!(
            operations[0].operands,
            vec![Operand::Real(0.5), Operand::Real(0.0), Operand::Real(1.0)]
        );
    }

    #[test]
    fn parses_cm_operator() {
        let operations = ContentParser::parse(b"1 0 0 1 100 200 cm").unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operator, "cm");
        assert_eq!(
            operations[0].operands,
            vec![
                Operand::Integer(1),
                Operand::Integer(0),
                Operand::Integer(0),
                Operand::Integer(1),
                Operand::Integer(100),
                Operand::Integer(200)
            ]
        );
    }

    #[test]
    fn parses_unknown_operators_without_error() {
        let operations = ContentParser::parse(b"42 XX 17 yy").unwrap();
        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0].operator, "XX");
        assert_eq!(operations[0].operands, vec![Operand::Integer(42)]);
        assert_eq!(operations[1].operator, "yy");
        assert_eq!(operations[1].operands, vec![Operand::Integer(17)]);
    }

    #[test]
    fn parses_inline_image_as_separate_operations() {
        let operations =
            ContentParser::parse(b"BI /W 4 /H 4 /CS /G /BPC 8 ID \x00\x11\x22\x33 EI").unwrap();
        assert_eq!(operations.len(), 4);
        assert_eq!(operations[0].operator, "BI");
        assert!(operations[0].operands.is_empty());
        assert_eq!(operations[1].operator, "ID");
        assert_eq!(
            operations[1].operands,
            vec![
                Operand::Name("BitsPerComponent".to_string()),
                Operand::Integer(8),
                Operand::Name("ColorSpace".to_string()),
                Operand::Name("DeviceGray".to_string()),
                Operand::Name("Height".to_string()),
                Operand::Integer(4),
                Operand::Name("Width".to_string()),
                Operand::Integer(4)
            ]
        );
        assert_eq!(operations[2].operator, "inline_image_data");
        assert_eq!(
            operations[2].operands,
            vec![Operand::String(vec![0x00, 0x11, 0x22, 0x33])]
        );
        assert_eq!(operations[3].operator, "EI");
        assert!(operations[3].operands.is_empty());
    }

    #[test]
    fn expand_inline_image_keys_expands_dimensions_and_gray_color_space() {
        let mut dict = PdfDictionary::empty();
        dict.insert("W", PdfObject::Integer(100));
        dict.insert("H", PdfObject::Integer(50));
        dict.insert("CS", PdfObject::Name("G".to_string()));

        expand_inline_image_keys(&mut dict);

        assert!(dict.get("Width").is_some());
        assert!(dict.get("Height").is_some());
        assert!(dict.get("W").is_none());
        assert_eq!(dict.get_name("ColorSpace"), Some("DeviceGray"));
    }

    #[test]
    fn expand_inline_image_keys_expands_flate_filter() {
        let mut dict = PdfDictionary::empty();
        dict.insert("F", PdfObject::Name("Fl".to_string()));

        expand_inline_image_keys(&mut dict);

        assert_eq!(dict.get_name("Filter"), Some("FlateDecode"));
    }

    #[test]
    fn expand_inline_image_keys_expands_bits_per_component() {
        let mut dict = PdfDictionary::empty();
        dict.insert("BPC", PdfObject::Integer(8));

        expand_inline_image_keys(&mut dict);

        assert!(dict.get("BitsPerComponent").is_some());
        assert!(dict.get("BPC").is_none());
    }
}
