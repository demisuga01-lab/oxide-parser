use std::collections::HashMap;

use crate::filters::decode_stream_lossless;
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

pub struct ToUnicodeCMap {
    map: HashMap<u16, String>,
    code_size: u8,
}

impl ToUnicodeCMap {
    pub fn parse(cmap_bytes: &[u8]) -> Self {
        let mut parser = CMapParser {
            bytes: cmap_bytes,
            pos: 0,
            map: HashMap::new(),
            saw_two_byte_source: false,
        };
        parser.parse_all();
        let code_size = if parser.saw_two_byte_source || parser.map.keys().any(|code| *code > 0xFF)
        {
            2
        } else {
            1
        };
        Self {
            map: parser.map,
            code_size,
        }
    }

    pub fn lookup(&self, code: u16) -> Option<&str> {
        self.map.get(&code).map(String::as_str)
    }

    pub fn code_size(&self) -> u8 {
        self.code_size
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

struct CMapParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    map: HashMap<u16, String>,
    saw_two_byte_source: bool,
}

impl CMapParser<'_> {
    fn parse_all(&mut self) {
        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments();
            if self.starts_with(b"beginbfchar") {
                self.pos += b"beginbfchar".len();
                self.parse_bfchar_block();
            } else if self.starts_with(b"beginbfrange") {
                self.pos += b"beginbfrange".len();
                self.parse_bfrange_block();
            } else {
                self.pos += 1;
            }
        }
    }

    fn parse_bfchar_block(&mut self) {
        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments();
            if self.starts_with(b"endbfchar") {
                self.pos += b"endbfchar".len();
                return;
            }
            let Some(src) = parse_hex_string(self.bytes, &mut self.pos) else {
                self.pos += 1;
                continue;
            };
            let Some(dst) = parse_hex_string(self.bytes, &mut self.pos) else {
                continue;
            };
            self.insert_mapping(&src, &dst);
        }
    }

    fn parse_bfrange_block(&mut self) {
        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments();
            if self.starts_with(b"endbfrange") {
                self.pos += b"endbfrange".len();
                return;
            }
            let Some(start_bytes) = parse_hex_string(self.bytes, &mut self.pos) else {
                self.pos += 1;
                continue;
            };
            let Some(end_bytes) = parse_hex_string(self.bytes, &mut self.pos) else {
                continue;
            };
            let start_code = source_code(&start_bytes);
            let end_code = source_code(&end_bytes);
            self.record_source_len(&start_bytes);
            self.record_source_len(&end_bytes);
            if end_code < start_code {
                continue;
            }

            self.skip_ws_and_comments();
            if self.peek() == Some(b'[') {
                let destinations = parse_bfrange_array(self.bytes, &mut self.pos);
                for (offset, dst) in destinations.into_iter().enumerate() {
                    let code = start_code.saturating_add(offset as u16);
                    if code > end_code {
                        break;
                    }
                    self.map.insert(code, utf16be_to_string(&dst));
                }
            } else if let Some(dst) = parse_hex_string(self.bytes, &mut self.pos) {
                for code in start_code..=end_code {
                    let offset = code.saturating_sub(start_code);
                    let mapped = increment_utf16be(&dst, offset);
                    self.map.insert(code, utf16be_to_string(&mapped));
                }
            }
        }
    }

    fn insert_mapping(&mut self, src: &[u8], dst: &[u8]) {
        self.record_source_len(src);
        self.map.insert(source_code(src), utf16be_to_string(dst));
    }

    fn record_source_len(&mut self, src: &[u8]) {
        if src.len() > 1 {
            self.saw_two_byte_source = true;
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self.peek().is_some_and(is_ps_whitespace) {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                while let Some(byte) = self.peek() {
                    self.pos += 1;
                    if byte == b'\r' || byte == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.bytes
            .get(self.pos..self.pos + needle.len())
            .is_some_and(|slice| slice.eq_ignore_ascii_case(needle))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
}

fn parse_hex_string(bytes: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    skip_ws_and_comments(bytes, pos);
    if bytes.get(*pos).copied() != Some(b'<') || bytes.get(*pos + 1).copied() == Some(b'<') {
        return None;
    }
    *pos += 1;
    let mut out = Vec::new();
    let mut high = None;
    while let Some(byte) = bytes.get(*pos).copied() {
        *pos += 1;
        if byte == b'>' {
            if let Some(high_nibble) = high {
                out.push(high_nibble << 4);
            }
            return Some(out);
        }
        if is_ps_whitespace(byte) {
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
    None
}

fn parse_bfrange_array(bytes: &[u8], pos: &mut usize) -> Vec<Vec<u8>> {
    skip_ws_and_comments(bytes, pos);
    if bytes.get(*pos).copied() != Some(b'[') {
        return Vec::new();
    }
    *pos += 1;
    let mut values = Vec::new();
    while *pos < bytes.len() {
        skip_ws_and_comments(bytes, pos);
        if bytes.get(*pos).copied() == Some(b']') {
            *pos += 1;
            break;
        }
        if let Some(hex) = parse_hex_string(bytes, pos) {
            values.push(hex);
        } else {
            *pos += 1;
        }
    }
    values
}

fn skip_ws_and_comments(bytes: &[u8], pos: &mut usize) {
    loop {
        while bytes.get(*pos).copied().is_some_and(is_ps_whitespace) {
            *pos += 1;
        }
        if bytes.get(*pos).copied() == Some(b'%') {
            while let Some(byte) = bytes.get(*pos).copied() {
                *pos += 1;
                if byte == b'\r' || byte == b'\n' {
                    break;
                }
            }
        } else {
            break;
        }
    }
}

fn source_code(bytes: &[u8]) -> u16 {
    match bytes {
        [] => 0,
        [one] => u16::from(*one),
        [high, low, ..] => (u16::from(*high) << 8) | u16::from(*low),
    }
}

fn increment_utf16be(bytes: &[u8], offset: u16) -> Vec<u8> {
    if bytes.len() < 2 {
        return bytes.to_vec();
    }
    let mut out = bytes.to_vec();
    let last = out.len() - 2;
    let value = u16::from_be_bytes([out[last], out[last + 1]]).wrapping_add(offset);
    let encoded = value.to_be_bytes();
    out[last] = encoded[0];
    out[last + 1] = encoded[1];
    out
}

fn utf16be_to_string(bytes: &[u8]) -> String {
    if bytes.len() == 1 {
        return char::from_u32(u32::from(bytes[0]))
            .unwrap_or('\u{FFFD}')
            .to_string();
    }
    let mut units = Vec::new();
    let mut idx = 0;
    while idx < bytes.len() {
        let high = bytes[idx];
        let low = bytes.get(idx + 1).copied().unwrap_or(0);
        units.push(u16::from_be_bytes([high, low]));
        idx += 2;
    }
    char::decode_utf16(units)
        .map(|item| item.unwrap_or('\u{FFFD}'))
        .collect()
}

fn is_ps_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse a ToUnicode CMap stream into a CID-to-Unicode map.
pub fn parse_to_unicode_cmap(cmap_bytes: &[u8]) -> HashMap<u32, char> {
    let mut map = HashMap::new();
    let text = std::str::from_utf8(cmap_bytes).unwrap_or("");

    for block in cmap_blocks(text, "beginbfchar", "endbfchar") {
        for line in block.lines() {
            parse_bf_char_line(line.trim(), &mut map);
        }
    }

    for block in cmap_blocks(text, "beginbfrange", "endbfrange") {
        for line in block.lines() {
            parse_bf_range_line(line.trim(), &mut map);
        }
    }

    map
}

pub fn extract_to_unicode_map(
    font_dict: &PdfDictionary,
    reader: &PdfReader,
) -> Option<HashMap<u32, char>> {
    let object = font_dict.get("ToUnicode")?;
    let resolved = reader.resolve(object.clone()).ok()?;
    let PdfObject::Stream { dict, raw } = resolved else {
        return None;
    };
    let raw_fallback = raw.clone();
    let stream = PdfObject::Stream { dict, raw };
    let decoded = decode_stream_lossless(&stream, reader)
        .map(|decoded| decoded.data)
        .unwrap_or(raw_fallback);
    Some(parse_to_unicode_cmap(&decoded))
}

fn cmap_blocks<'a>(text: &'a str, begin: &str, end: &str) -> Vec<&'a str> {
    let mut blocks = Vec::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let Some(start_rel) = find_ascii_case_insensitive(&text[pos..], begin) else {
            break;
        };
        let block_start = pos + start_rel + begin.len();
        let end_rel = find_ascii_case_insensitive(&text[block_start..], end)
            .unwrap_or(text.len() - block_start);
        let block_end = block_start + end_rel;
        blocks.push(&text[block_start..block_end]);
        pos = block_end.saturating_add(end.len());
    }
    blocks
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

fn parse_bf_char_line(line: &str, map: &mut HashMap<u32, char>) {
    let hexes = extract_hex_values(line);
    if hexes.len() < 2 {
        return;
    }
    let Some(cid) = parse_hex(hexes[0]) else {
        return;
    };
    if let Some(unicode) = parse_unicode_hex(hexes[1]) {
        map.insert(cid, unicode);
    }
}

fn parse_bf_range_line(line: &str, map: &mut HashMap<u32, char>) {
    let parts = extract_hex_values(line);
    if parts.len() < 3 {
        return;
    }
    let (Some(cid_start), Some(cid_end)) = (parse_hex(parts[0]), parse_hex(parts[1])) else {
        return;
    };
    if cid_end < cid_start || cid_end.saturating_sub(cid_start) > 65_535 {
        return;
    }

    if line.contains('[') {
        let arr_start = line.find('[').unwrap_or(line.len());
        let arr_end = line[arr_start..]
            .find(']')
            .map(|offset| arr_start + offset)
            .unwrap_or(line.len());
        if arr_start >= arr_end {
            return;
        }
        let arr_str = &line[arr_start + 1..arr_end];
        for (offset, unicode_hex) in extract_hex_values(arr_str).iter().enumerate() {
            let cid = cid_start.saturating_add(offset as u32);
            if cid > cid_end {
                break;
            }
            if let Some(ch) = parse_unicode_hex(unicode_hex) {
                map.insert(cid, ch);
            }
        }
    } else {
        let Some(unicode_start) = parse_hex(parts[2]) else {
            return;
        };
        for cid in cid_start..=cid_end {
            let unicode_val = unicode_start.saturating_add(cid - cid_start);
            if let Some(ch) = char::from_u32(unicode_val) {
                map.insert(cid, ch);
            }
        }
    }
}

fn extract_hex_values(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        if rest.starts_with('<') {
            rest = &rest[1..];
            continue;
        }
        let Some(end) = rest.find('>') else {
            break;
        };
        result.push(&rest[..end]);
        rest = &rest[end + 1..];
    }
    result
}

fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim(), 16).ok()
}

fn parse_unicode_hex(s: &str) -> Option<char> {
    let bytes = hex_string_to_bytes(s)?;
    if bytes.is_empty() {
        return None;
    }
    if bytes.len() == 1 {
        return char::from_u32(u32::from(bytes[0]));
    }
    utf16be_to_string(&bytes).chars().next()
}

fn hex_string_to_bytes(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut high = None;
    for byte in s.bytes() {
        if is_ps_whitespace(byte) {
            continue;
        }
        let value = hex_value(byte)?;
        match high.take() {
            Some(high_nibble) => out.push((high_nibble << 4) | value),
            None => high = Some(value),
        }
    }
    if let Some(high_nibble) = high {
        out.push(high_nibble << 4);
    }
    Some(out)
}

#[cfg(test)]
mod cid_cmap_tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn parse_bf_char_line_maps_single_character() {
        let mut map = HashMap::new();
        parse_bf_char_line("<0041> <0041>", &mut map);
        assert_eq!(map.get(&0x41), Some(&'A'));
    }

    #[test]
    fn parse_bf_range_line_contiguous_maps_sequence() {
        let mut map = HashMap::new();
        parse_bf_range_line("<0041> <0043> <0041>", &mut map);
        assert_eq!(map.get(&0x41), Some(&'A'));
        assert_eq!(map.get(&0x42), Some(&'B'));
        assert_eq!(map.get(&0x43), Some(&'C'));
    }

    #[test]
    fn parse_bf_range_line_array_maps_explicit_values() {
        let mut map = HashMap::new();
        parse_bf_range_line("<0041> <0043> [<0058> <0059> <005A>]", &mut map);
        assert_eq!(map.get(&0x41), Some(&'X'));
        assert_eq!(map.get(&0x42), Some(&'Y'));
        assert_eq!(map.get(&0x43), Some(&'Z'));
    }

    #[test]
    fn parse_to_unicode_cmap_handles_bfchar_and_bfrange() {
        let cmap_text = b"beginbfchar\n<0020> <0020>\n<002E> <002E>\nendbfchar\n\
                          beginbfrange\n<0041> <005A> <0041>\nendbfrange";
        let map = parse_to_unicode_cmap(cmap_text);
        assert_eq!(map.get(&0x20), Some(&' '));
        assert_eq!(map.get(&0x41), Some(&'A'));
        assert_eq!(map.get(&0x5A), Some(&'Z'));
        assert_eq!(map.get(&0x5B), None);
    }

    #[test]
    fn parse_to_unicode_cmap_handles_multi_byte_cids() {
        let cmap_text = b"beginbfchar\n<3042> <3042>\nendbfchar";
        let map = parse_to_unicode_cmap(cmap_text);
        assert_eq!(map.get(&0x3042), Some(&'\u{3042}'));
    }

    #[test]
    fn parse_to_unicode_cmap_empty_input_returns_empty_map() {
        let map = parse_to_unicode_cmap(b"");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_to_unicode_cmap_handles_multiple_blocks() {
        let cmap = b"\
            beginbfchar\n<0041> <0041>\nendbfchar\n\
            beginbfchar\n<0042> <0042>\nendbfchar\n\
            beginbfrange\n<0043> <0044> <0043>\nendbfrange";
        let map = parse_to_unicode_cmap(cmap);
        assert_eq!(map.len(), 4);
        assert_eq!(map.get(&0x41), Some(&'A'));
        assert_eq!(map.get(&0x44), Some(&'D'));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_beginbfchar() {
        let cmap = b"
        /CIDInit /ProcSet findresource begin
        begincmap
        2 beginbfchar
        <41> <0041>
        <42> <0042>
        endbfchar
        endcmap end
        ";
        let parsed = ToUnicodeCMap::parse(cmap);
        assert_eq!(parsed.lookup(0x41), Some("A"));
        assert_eq!(parsed.lookup(0x42), Some("B"));
        assert_eq!(parsed.code_size(), 1);
    }

    #[test]
    fn parses_beginbfrange_scalar() {
        let cmap = b"
        begincmap
        1 beginbfrange
        <0041> <0046> <0041>
        endbfrange
        endcmap
        ";
        let parsed = ToUnicodeCMap::parse(cmap);
        assert_eq!(parsed.lookup(0x0041), Some("A"));
        assert_eq!(parsed.lookup(0x0044), Some("D"));
        assert_eq!(parsed.lookup(0x0046), Some("F"));
        assert_eq!(parsed.lookup(0x0047), None);
    }

    #[test]
    fn parses_beginbfrange_array() {
        let cmap = b"
        begincmap
        1 beginbfrange
        <20> <21> [<0048> <0049>]
        endbfrange
        endcmap
        ";
        let parsed = ToUnicodeCMap::parse(cmap);
        assert_eq!(parsed.lookup(0x20), Some("H"));
        assert_eq!(parsed.lookup(0x21), Some("I"));
        assert_eq!(parsed.code_size(), 1);
    }

    #[test]
    fn detects_two_byte_codes() {
        let cmap = b"
        begincmap
        1 beginbfchar
        <0041> <0041>
        endbfchar
        endcmap
        ";
        let parsed = ToUnicodeCMap::parse(cmap);
        assert_eq!(parsed.code_size(), 2);
        assert_eq!(parsed.lookup(0x0041), Some("A"));
    }
}
