use crate::error::{OxideError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum ContentToken {
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Name(String),
    LiteralString(Vec<u8>),
    HexString(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    Operator(String),
    InlineImageData(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineImageState {
    Normal,
    Params,
    Data,
    PendingEnd,
}

pub struct ContentTokenizer<'a> {
    data: &'a [u8],
    pos: usize,
    inline_image_state: InlineImageState,
}

impl<'a> ContentTokenizer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            inline_image_state: InlineImageState::Normal,
        }
    }

    fn next_token(&mut self) -> Result<Option<ContentToken>> {
        if self.inline_image_state == InlineImageState::PendingEnd {
            self.inline_image_state = InlineImageState::Normal;
            return Ok(Some(ContentToken::Operator("EI".to_string())));
        }
        if self.inline_image_state == InlineImageState::Data {
            return self.read_inline_image_data().map(Some);
        }

        self.skip_ws_and_comments();
        if self.pos >= self.data.len() {
            return Ok(None);
        }

        let byte = self.data[self.pos];
        let token = match byte {
            b'[' => {
                self.pos += 1;
                ContentToken::ArrayStart
            }
            b']' => {
                self.pos += 1;
                ContentToken::ArrayEnd
            }
            b'<' if self.starts_with(b"<<") => {
                self.pos += 2;
                ContentToken::DictStart
            }
            b'<' => self.read_hex_string()?,
            b'>' if self.starts_with(b">>") => {
                self.pos += 2;
                ContentToken::DictEnd
            }
            b'>' => {
                self.pos += 1;
                ContentToken::Operator("?".to_string())
            }
            b'/' => self.read_name(),
            b'(' => self.read_literal_string()?,
            byte if is_number_start(self.data, self.pos) => self.read_number(byte),
            _ => self.read_operator(),
        };

        match &token {
            ContentToken::Operator(op) if op == "BI" => {
                self.inline_image_state = InlineImageState::Params;
            }
            ContentToken::Operator(op)
                if op == "ID" && self.inline_image_state == InlineImageState::Params =>
            {
                self.consume_inline_image_data_separator();
                self.inline_image_state = InlineImageState::Data;
            }
            _ => {}
        }

        Ok(Some(token))
    }

    fn read_number(&mut self, _first: u8) -> ContentToken {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut saw_dot = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => self.pos += 1,
                b'.' if !saw_dot => {
                    saw_dot = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.data[start..self.pos]).unwrap_or("");
        if saw_dot {
            text.parse::<f64>()
                .map(ContentToken::Real)
                .unwrap_or_else(|_| ContentToken::Operator("?".to_string()))
        } else {
            text.parse::<i64>()
                .map(ContentToken::Integer)
                .unwrap_or_else(|_| ContentToken::Operator("?".to_string()))
        }
    }

    fn read_name(&mut self) -> ContentToken {
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(byte) = self.peek() {
            if is_pdf_whitespace(byte) || is_delimiter(byte) {
                break;
            }
            self.pos += 1;
            if byte == b'#' {
                let high = self.peek().and_then(hex_value);
                let low = self.data.get(self.pos + 1).copied().and_then(hex_value);
                if let (Some(high), Some(low)) = (high, low) {
                    self.pos += 2;
                    out.push((high << 4) | low);
                } else {
                    out.push(byte);
                }
            } else {
                out.push(byte);
            }
        }
        ContentToken::Name(String::from_utf8_lossy(&out).into_owned())
    }

    fn read_literal_string(&mut self) -> Result<ContentToken> {
        self.pos += 1;
        let mut depth = 1usize;
        let mut out = Vec::new();

        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            self.pos += 1;
            match byte {
                b'(' => {
                    depth += 1;
                    out.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(ContentToken::LiteralString(out));
                    }
                    out.push(byte);
                }
                b'\\' => self.read_literal_escape(&mut out),
                _ => out.push(byte),
            }
        }

        Err(OxideError::ParseError(
            "unterminated content literal string".to_string(),
        ))
    }

    fn read_literal_escape(&mut self, out: &mut Vec<u8>) {
        let Some(byte) = self.next_byte() else {
            return;
        };
        match byte {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'(' | b')' | b'\\' => out.push(byte),
            b'\r' => {
                if self.peek() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    match self.peek() {
                        Some(next @ b'0'..=b'7') => {
                            self.pos += 1;
                            value = (value << 3) + u16::from(next - b'0');
                        }
                        _ => break,
                    }
                }
                out.push((value & 0xFF) as u8);
            }
            other => out.push(other),
        }
    }

    fn read_hex_string(&mut self) -> Result<ContentToken> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut high: Option<u8> = None;

        while let Some(byte) = self.next_byte() {
            if byte == b'>' {
                if let Some(high_nibble) = high {
                    out.push(high_nibble << 4);
                }
                return Ok(ContentToken::HexString(out));
            }
            if is_pdf_whitespace(byte) {
                continue;
            }
            let Some(value) = hex_value(byte) else {
                continue;
            };
            match high.take() {
                Some(high_nibble) => out.push((high_nibble << 4) | value),
                None => high = Some(value),
            }
        }

        Err(OxideError::ParseError(
            "unterminated content hex string".to_string(),
        ))
    }

    fn read_operator(&mut self) -> ContentToken {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if is_pdf_whitespace(byte) || is_delimiter(byte) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            self.pos += 1;
            return ContentToken::Operator("?".to_string());
        }
        let op = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
        match op.as_str() {
            "true" => ContentToken::Boolean(true),
            "false" => ContentToken::Boolean(false),
            _ => ContentToken::Operator(op),
        }
    }

    fn read_inline_image_data(&mut self) -> Result<ContentToken> {
        let start = self.pos;
        let mut cursor = self.pos;
        while cursor + 2 < self.data.len() {
            if is_pdf_whitespace(self.data[cursor])
                && self.data[cursor + 1] == b'E'
                && self.data[cursor + 2] == b'I'
                && self
                    .data
                    .get(cursor + 3)
                    .copied()
                    .is_none_or(is_inline_image_end_follow)
            {
                let data = self.data[start..cursor].to_vec();
                self.pos = cursor + 3;
                self.inline_image_state = InlineImageState::PendingEnd;
                return Ok(ContentToken::InlineImageData(data));
            }
            cursor += 1;
        }

        // No `EI` terminator before end of data. Treat the remaining bytes as
        // the (unterminated) inline-image payload, consume to EOF, and LEAVE the
        // inline-image state so iteration terminates at EOF. Returning an error
        // here while staying in `Data` at the same position would loop forever
        // in a caller that recovers from token errors (regression: tokenizer
        // inline-image hang).
        let data = self.data[start..].to_vec();
        self.pos = self.data.len();
        self.inline_image_state = InlineImageState::Normal;
        Ok(ContentToken::InlineImageData(data))
    }

    fn consume_inline_image_data_separator(&mut self) {
        match self.peek() {
            Some(b'\r') => {
                self.pos += 1;
                if self.peek() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            Some(byte) if is_pdf_whitespace(byte) => self.pos += 1,
            _ => {}
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(byte) if is_pdf_whitespace(byte)) {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                while let Some(byte) = self.next_byte() {
                    if byte == b'\r' || byte == b'\n' {
                        if byte == b'\r' && self.peek() == Some(b'\n') {
                            self.pos += 1;
                        }
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn starts_with(&self, bytes: &[u8]) -> bool {
        self.data
            .get(self.pos..self.pos + bytes.len())
            .is_some_and(|slice| slice == bytes)
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }
}

impl Iterator for ContentTokenizer<'_> {
    type Item = Result<ContentToken>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_token().transpose()
    }
}

pub fn tokenize_all(data: &[u8]) -> Result<Vec<ContentToken>> {
    ContentTokenizer::new(data).collect()
}

fn is_number_start(data: &[u8], pos: usize) -> bool {
    match data.get(pos).copied() {
        Some(b'0'..=b'9') => true,
        Some(b'.') => matches!(data.get(pos + 1), Some(b'0'..=b'9')),
        Some(b'+' | b'-') => matches!(data.get(pos + 1), Some(b'0'..=b'9' | b'.')),
        _ => false,
    }
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_inline_image_end_follow(byte: u8) -> bool {
    is_pdf_whitespace(byte) || is_delimiter(byte)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_basic_text_stream() {
        let tokens = tokenize_all(b"BT /F1 12 Tf 100 700 Td (Hello World) Tj ET").unwrap();
        assert_eq!(
            tokens,
            vec![
                ContentToken::Operator("BT".to_string()),
                ContentToken::Name("F1".to_string()),
                ContentToken::Integer(12),
                ContentToken::Operator("Tf".to_string()),
                ContentToken::Integer(100),
                ContentToken::Integer(700),
                ContentToken::Operator("Td".to_string()),
                ContentToken::LiteralString(b"Hello World".to_vec()),
                ContentToken::Operator("Tj".to_string()),
                ContentToken::Operator("ET".to_string()),
            ]
        );
    }

    #[test]
    fn tokenizes_numbers_and_reals() {
        let tokens = tokenize_all(b"1 2.5 -3 -.75 +0").unwrap();
        assert_eq!(
            tokens,
            vec![
                ContentToken::Integer(1),
                ContentToken::Real(2.5),
                ContentToken::Integer(-3),
                ContentToken::Real(-0.75),
                ContentToken::Integer(0),
            ]
        );
    }

    #[test]
    fn tokenizes_hex_string_and_dict_markers() {
        let tokens = tokenize_all(b"<</Key <48656C6C6F>>>").unwrap();
        assert_eq!(
            tokens,
            vec![
                ContentToken::DictStart,
                ContentToken::Name("Key".to_string()),
                ContentToken::HexString(b"Hello".to_vec()),
                ContentToken::DictEnd,
            ]
        );
    }

    #[test]
    fn tokenizes_literal_string_escapes() {
        assert_eq!(
            tokenize_all(b"(Hello\\nWorld)").unwrap(),
            vec![ContentToken::LiteralString(b"Hello\nWorld".to_vec())]
        );
    }

    #[test]
    fn tokenizes_balanced_nested_parentheses() {
        assert_eq!(
            tokenize_all(b"(outer (inner) text)").unwrap(),
            vec![ContentToken::LiteralString(b"outer (inner) text".to_vec())]
        );
    }

    #[test]
    fn skips_comments() {
        assert_eq!(
            tokenize_all(b"1 % this is a comment\n2").unwrap(),
            vec![ContentToken::Integer(1), ContentToken::Integer(2)]
        );
    }

    #[test]
    fn tokenizes_name_with_hex_escape() {
        assert_eq!(
            tokenize_all(b"/F#231").unwrap(),
            vec![ContentToken::Name("F#1".to_string())]
        );
    }

    #[test]
    fn tokenizes_inline_image_data_without_parsing_binary_bytes() {
        let tokens = tokenize_all(b"BI /W 2 /H 2 /CS /G /BPC 8 ID \x00\xFF\xFF\x00 EI").unwrap();
        assert_eq!(
            tokens,
            vec![
                ContentToken::Operator("BI".to_string()),
                ContentToken::Name("W".to_string()),
                ContentToken::Integer(2),
                ContentToken::Name("H".to_string()),
                ContentToken::Integer(2),
                ContentToken::Name("CS".to_string()),
                ContentToken::Name("G".to_string()),
                ContentToken::Name("BPC".to_string()),
                ContentToken::Integer(8),
                ContentToken::Operator("ID".to_string()),
                ContentToken::InlineImageData(vec![0x00, 0xFF, 0xFF, 0x00]),
                ContentToken::Operator("EI".to_string()),
            ]
        );
    }

    #[test]
    fn unterminated_inline_image_terminates_and_does_not_hang() {
        // Regression: an inline image (`BI`/`ID`) with NO `EI` terminator used
        // to return a token error while leaving the tokenizer in the Data state
        // at the same position — a caller that recovers from token errors
        // (`ContentParser::parse`) then looped forever. The tokenizer must now
        // consume the rest as inline-image data and terminate at EOF.
        //
        // This is the libFuzzer-minimized class of input from the
        // `content_tokenizer` target.
        let data = b"BI /W 2 /H 2 /CS /G /BPC 8 ID \x00\xFF\xFF\x00 no terminator here";
        let tokens = tokenize_all(data).expect("tokenizes without error");
        // Must include the BI, ID, and an InlineImageData token, then stop.
        assert!(tokens
            .iter()
            .any(|t| matches!(t, ContentToken::InlineImageData(_))));
        assert!(tokens
            .iter()
            .any(|t| matches!(t, ContentToken::Operator(op) if op == "ID")));

        // And the higher-level parser must also terminate (this is what hung).
        let ops = crate::content::ContentParser::parse(data).unwrap();
        assert!(ops.iter().any(|o| o.operator == "ID"));
    }
}
