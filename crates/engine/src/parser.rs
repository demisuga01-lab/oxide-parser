use std::collections::BTreeMap;

use crate::error::{OxideError, Result};
use crate::object::{PdfDictionary, PdfObject};

pub trait ParserResolver {
    fn resolve_for_parser(&self, object: &PdfObject) -> Result<PdfObject>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndirectObject {
    pub number: u32,
    pub generation: u16,
    pub object: PdfObject,
}

pub struct PdfParser<'a> {
    data: &'a [u8],
    pos: usize,
    resolver: Option<&'a dyn ParserResolver>,
}

impl<'a> PdfParser<'a> {
    pub fn new(data: &'a [u8], offset: usize) -> Result<Self> {
        Self::with_resolver(data, offset, None)
    }

    pub fn with_resolver(
        data: &'a [u8],
        offset: usize,
        resolver: Option<&'a dyn ParserResolver>,
    ) -> Result<Self> {
        if offset > data.len() {
            return Err(OxideError::ParseError(format!(
                "offset {offset} is beyond input length {}",
                data.len()
            )));
        }
        Ok(Self {
            data,
            pos: offset,
            resolver,
        })
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn parse_object(&mut self) -> Result<PdfObject> {
        self.skip_ws_and_comments();
        let byte = self.peek_byte().ok_or_else(|| {
            OxideError::ParseError("unexpected end of input while parsing object".to_string())
        })?;

        match byte {
            b'<' if self.starts_with(b"<<") => self.parse_dictionary_or_stream(),
            b'<' => self.parse_hex_string(),
            b'(' => self.parse_literal_string(),
            b'/' => self.parse_name().map(PdfObject::Name),
            b'[' => self.parse_array(),
            b't' if self.consume_keyword(b"true") => Ok(PdfObject::Boolean(true)),
            b'f' if self.consume_keyword(b"false") => Ok(PdfObject::Boolean(false)),
            b'n' if self.consume_keyword(b"null") => Ok(PdfObject::Null),
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.parse_number_or_reference(),
            other => Err(OxideError::ParseError(format!(
                "unexpected byte 0x{other:02X} while parsing object"
            ))),
        }
    }

    pub fn parse_indirect_object(&mut self) -> Result<IndirectObject> {
        self.skip_ws_and_comments();
        let number = self.parse_unsigned_integer_token()?;
        self.skip_ws_and_comments();
        let generation = self.parse_unsigned_integer_token()?;
        let number = u32::try_from(number).map_err(|_| {
            OxideError::ParseError(format!("object number {number} does not fit in u32"))
        })?;
        let generation = u16::try_from(generation).map_err(|_| {
            OxideError::ParseError(format!("generation {generation} does not fit in u16"))
        })?;
        self.skip_ws_and_comments();
        if !self.consume_keyword(b"obj") {
            return Err(OxideError::ParseError(
                "indirect object header is missing obj keyword".to_string(),
            ));
        }
        let object = self.parse_object()?;
        self.skip_ws_and_comments();
        if !self.consume_keyword(b"endobj") {
            return Err(OxideError::ParseError(format!(
                "object {number} {generation} is missing endobj"
            )));
        }
        Ok(IndirectObject {
            number,
            generation,
            object,
        })
    }

    fn parse_dictionary_or_stream(&mut self) -> Result<PdfObject> {
        let dict = self.parse_dictionary()?;
        let after_dict = self.pos;
        self.skip_ws_and_comments();
        if self.consume_keyword(b"stream") {
            let raw = self.parse_stream_bytes(&dict)?;
            Ok(PdfObject::Stream { dict, raw })
        } else {
            self.pos = after_dict;
            Ok(PdfObject::Dictionary(dict))
        }
    }

    fn parse_dictionary(&mut self) -> Result<PdfDictionary> {
        self.expect_bytes(b"<<")?;
        let mut entries = BTreeMap::new();
        loop {
            self.skip_ws_and_comments();
            if self.starts_with(b">>") {
                self.pos += 2;
                break;
            }
            if self.peek_byte().is_none() {
                return Err(OxideError::ParseError(
                    "unterminated dictionary".to_string(),
                ));
            }
            if self.peek_byte() != Some(b'/') {
                return Err(OxideError::ParseError(
                    "dictionary key must be a name".to_string(),
                ));
            }
            let key = self.parse_name()?;
            let value = self.parse_object()?;
            entries.insert(key, value);
        }
        Ok(PdfDictionary::new(entries))
    }

    fn parse_array(&mut self) -> Result<PdfObject> {
        self.expect_byte(b'[')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.peek_byte() {
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => items.push(self.parse_object()?),
                None => return Err(OxideError::ParseError("unterminated array".to_string())),
            }
        }
        Ok(PdfObject::Array(items))
    }

    fn parse_literal_string(&mut self) -> Result<PdfObject> {
        self.expect_byte(b'(')?;
        let mut out = Vec::new();
        let mut depth = 1usize;

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
                        return Ok(PdfObject::String(out));
                    }
                    out.push(byte);
                }
                b'\\' => self.parse_literal_escape(&mut out)?,
                _ => out.push(byte),
            }
        }

        Err(OxideError::ParseError(
            "unterminated literal string".to_string(),
        ))
    }

    fn parse_literal_escape(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let Some(byte) = self.next_byte() else {
            return Ok(());
        };
        match byte {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'(' | b')' | b'\\' => out.push(byte),
            b'\r' => {
                if self.peek_byte() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    match self.peek_byte() {
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
        Ok(())
    }

    fn parse_hex_string(&mut self) -> Result<PdfObject> {
        self.expect_byte(b'<')?;
        let mut out = Vec::new();
        let mut high: Option<u8> = None;

        loop {
            let byte = self
                .next_byte()
                .ok_or_else(|| OxideError::ParseError("unterminated hex string".to_string()))?;
            if byte == b'>' {
                break;
            }
            if is_pdf_whitespace(byte) {
                continue;
            }
            let value = hex_value(byte).ok_or_else(|| {
                OxideError::ParseError(format!("invalid hex string digit 0x{byte:02X}"))
            })?;
            match high.take() {
                Some(high_nibble) => out.push((high_nibble << 4) | value),
                None => high = Some(value),
            }
        }

        if let Some(high_nibble) = high {
            out.push(high_nibble << 4);
        }

        Ok(PdfObject::String(out))
    }

    fn parse_name(&mut self) -> Result<String> {
        self.expect_byte(b'/')?;
        let mut out = Vec::new();

        while let Some(byte) = self.peek_byte() {
            if is_pdf_whitespace(byte) || is_delimiter(byte) {
                break;
            }
            self.pos += 1;
            if byte == b'#' {
                let maybe_high = self.peek_byte();
                let maybe_low = self.data.get(self.pos + 1).copied();
                match (
                    maybe_high.and_then(hex_value),
                    maybe_low.and_then(hex_value),
                ) {
                    (Some(high), Some(low)) => {
                        self.pos += 2;
                        out.push((high << 4) | low);
                    }
                    _ => out.push(byte),
                }
            } else {
                out.push(byte);
            }
        }

        String::from_utf8(out)
            .map_err(|err| OxideError::ParseError(format!("name is not UTF-8: {err}")))
    }

    fn parse_number_or_reference(&mut self) -> Result<PdfObject> {
        let token = self.parse_number_token()?;
        let is_real = token.contains(&b'.');
        if !is_real {
            let integer = parse_i64_token(&token)?;
            if let Some(reference) = self.try_reference(integer)? {
                return Ok(reference);
            }
            return Ok(PdfObject::Integer(integer));
        }

        let text = std::str::from_utf8(&token)
            .map_err(|err| OxideError::ParseError(format!("invalid real token: {err}")))?;
        let value = text
            .parse::<f64>()
            .map_err(|err| OxideError::ParseError(format!("invalid real number: {err}")))?;
        Ok(PdfObject::Real(value))
    }

    fn try_reference(&mut self, first: i64) -> Result<Option<PdfObject>> {
        let saved = self.pos;
        if first < 0 || first > i64::from(u32::MAX) {
            return Ok(None);
        }
        self.skip_ws_and_comments();
        let second_start = self.pos;
        let Ok(second_token) = self.parse_number_token() else {
            self.pos = saved;
            return Ok(None);
        };
        if second_token.contains(&b'.') {
            self.pos = saved;
            return Ok(None);
        }
        let generation = parse_i64_token(&second_token)?;
        if generation < 0 || generation > i64::from(u16::MAX) {
            self.pos = saved;
            return Ok(None);
        }
        self.skip_ws_and_comments();
        if self.consume_keyword(b"R") {
            return Ok(Some(PdfObject::Reference {
                number: first as u32,
                generation: generation as u16,
            }));
        }
        self.pos = saved;
        if self.pos < second_start {
            self.pos = saved;
        }
        Ok(None)
    }

    fn parse_stream_bytes(&mut self, dict: &PdfDictionary) -> Result<Vec<u8>> {
        match self.peek_byte() {
            Some(b'\r') => {
                self.pos += 1;
                if self.peek_byte() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            Some(b'\n') => self.pos += 1,
            _ => {}
        }

        let stream_start = self.pos;
        if let Some(length) = self.resolve_stream_length(dict)? {
            let length = usize::try_from(length)
                .map_err(|_| OxideError::MalformedPdf("stream Length is too large".to_string()))?;
            let stream_end = stream_start
                .checked_add(length)
                .ok_or_else(|| OxideError::MalformedPdf("stream Length overflows".to_string()))?;
            if stream_end <= self.data.len() {
                let after_raw = skip_eol(self.data, stream_end);
                if bytes_at(self.data, after_raw, b"endstream") {
                    let raw = self.data[stream_start..stream_end].to_vec();
                    self.pos = after_raw + b"endstream".len();
                    return Ok(raw);
                }
            }
        }

        self.scan_stream_until_endstream(stream_start)
    }

    fn resolve_stream_length(&self, dict: &PdfDictionary) -> Result<Option<i64>> {
        let Some(length_obj) = dict.get("Length") else {
            return Ok(None);
        };
        match length_obj {
            PdfObject::Integer(value) => Ok(Some(*value)),
            PdfObject::Reference { .. } => {
                let Some(resolver) = self.resolver else {
                    return Ok(None);
                };
                match resolver.resolve_for_parser(length_obj)? {
                    PdfObject::Integer(value) => Ok(Some(value)),
                    other => Err(OxideError::MalformedPdf(format!(
                        "stream Length reference resolved to {}",
                        other.variant_name()
                    ))),
                }
            }
            other => Err(OxideError::MalformedPdf(format!(
                "stream Length must be integer or reference, got {}",
                other.variant_name()
            ))),
        }
    }

    fn scan_stream_until_endstream(&mut self, stream_start: usize) -> Result<Vec<u8>> {
        let mut cursor = stream_start;
        while cursor + b"endstream".len() <= self.data.len() {
            if bytes_at(self.data, cursor, b"endstream") {
                let raw_end = trim_single_eol_before(self.data, stream_start, cursor);
                let raw = self.data[stream_start..raw_end].to_vec();
                self.pos = cursor + b"endstream".len();
                return Ok(raw);
            }
            cursor += 1;
        }
        Err(OxideError::ParseError(
            "stream is missing endstream".to_string(),
        ))
    }

    fn parse_unsigned_integer_token(&mut self) -> Result<u64> {
        let token = self.parse_number_token()?;
        if token.contains(&b'.') || token.starts_with(b"-") {
            return Err(OxideError::ParseError(
                "expected unsigned integer token".to_string(),
            ));
        }
        let text = std::str::from_utf8(&token)
            .map_err(|err| OxideError::ParseError(format!("invalid integer token: {err}")))?;
        text.parse::<u64>()
            .map_err(|err| OxideError::ParseError(format!("invalid unsigned integer: {err}")))
    }

    fn parse_number_token(&mut self) -> Result<Vec<u8>> {
        self.skip_ws_and_comments();
        let start = self.pos;
        if matches!(self.peek_byte(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut saw_digit = false;
        let mut saw_dot = false;
        while let Some(byte) = self.peek_byte() {
            match byte {
                b'0'..=b'9' => {
                    saw_digit = true;
                    self.pos += 1;
                }
                b'.' if !saw_dot => {
                    saw_dot = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if !saw_digit {
            self.pos = start;
            return Err(OxideError::ParseError("expected numeric token".to_string()));
        }
        Ok(self.data[start..self.pos].to_vec())
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<()> {
        if self.starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(OxideError::ParseError(format!(
                "expected {}",
                String::from_utf8_lossy(expected)
            )))
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<()> {
        match self.next_byte() {
            Some(byte) if byte == expected => Ok(()),
            Some(byte) => Err(OxideError::ParseError(format!(
                "expected byte 0x{expected:02X}, got 0x{byte:02X}"
            ))),
            None => Err(OxideError::ParseError(format!(
                "expected byte 0x{expected:02X}, got EOF"
            ))),
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while matches!(self.peek_byte(), Some(byte) if is_pdf_whitespace(byte)) {
                self.pos += 1;
            }
            if self.peek_byte() == Some(b'%') {
                while let Some(byte) = self.next_byte() {
                    if byte == b'\r' || byte == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn consume_keyword(&mut self, keyword: &[u8]) -> bool {
        if !self.starts_with(keyword) {
            return false;
        }
        let after = self.pos + keyword.len();
        if self
            .data
            .get(after)
            .copied()
            .is_some_and(|byte| !is_pdf_whitespace(byte) && !is_delimiter(byte))
        {
            return false;
        }
        self.pos = after;
        true
    }

    fn starts_with(&self, bytes: &[u8]) -> bool {
        bytes_at(self.data, self.pos, bytes)
    }

    fn peek_byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek_byte()?;
        self.pos += 1;
        Some(byte)
    }
}

fn parse_i64_token(token: &[u8]) -> Result<i64> {
    let text = std::str::from_utf8(token)
        .map_err(|err| OxideError::ParseError(format!("invalid integer token: {err}")))?;
    text.parse::<i64>()
        .map_err(|err| OxideError::ParseError(format!("invalid integer: {err}")))
}

fn skip_eol(data: &[u8], pos: usize) -> usize {
    match data.get(pos).copied() {
        Some(b'\r') => {
            if data.get(pos + 1).copied() == Some(b'\n') {
                pos + 2
            } else {
                pos + 1
            }
        }
        Some(b'\n') => pos + 1,
        _ => pos,
    }
}

fn trim_single_eol_before(data: &[u8], start: usize, end: usize) -> usize {
    if end > start && data.get(end - 1).copied() == Some(b'\n') {
        if end >= start + 2 && data.get(end - 2).copied() == Some(b'\r') {
            end - 2
        } else {
            end - 1
        }
    } else if end > start && data.get(end - 1).copied() == Some(b'\r') {
        end - 1
    } else {
        end
    }
}

fn bytes_at(data: &[u8], pos: usize, bytes: &[u8]) -> bool {
    data.get(pos..pos + bytes.len())
        .is_some_and(|slice| slice == bytes)
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
    fn parses_literal_string_escapes_and_nested_parentheses() {
        let mut input = br"(a \(b\) (c) \053".to_vec();
        input.extend_from_slice(br"\\");
        input.push(b'\\');
        input.push(b'\n');
        input.extend_from_slice(b"continued)");
        let mut parser = PdfParser::new(&input, 0).unwrap();
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::String(b"a (b) (c) +\\continued".to_vec())
        );
    }

    #[test]
    fn parses_hex_string_with_odd_nibble() {
        let mut parser = PdfParser::new(br"<61 62 3>", 0).unwrap();
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::String(b"ab0".to_vec())
        );
    }

    #[test]
    fn parses_name_hex_escapes() {
        let mut parser = PdfParser::new(br"/A#20Name", 0).unwrap();
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Name("A Name".to_string())
        );
    }

    #[test]
    fn parses_references() {
        let mut parser = PdfParser::new(br"12 0 R", 0).unwrap();
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Reference {
                number: 12,
                generation: 0
            }
        );
    }
}
