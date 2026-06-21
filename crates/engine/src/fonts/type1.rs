use std::collections::HashMap;

use crate::fonts::encoding::Encoding;
use crate::render::path::{Path, PathSegment};

const EEXEC_KEY: u16 = 55665;
const CHARSTRING_KEY: u16 = 4330;
const C1: u16 = 52845;
const C2: u16 = 22719;
const DEFAULT_LEN_IV: i32 = 4;
const MAX_SUBR_DEPTH: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct Type1Font {
    #[cfg(test)]
    len_iv: i32,
    subrs: HashMap<i32, Vec<u8>>,
    charstrings: HashMap<String, Vec<u8>>,
}

impl Type1Font {
    pub(crate) fn parse(font_bytes: &[u8]) -> Option<Self> {
        let private = decrypt_private_program(font_bytes)?;
        let len_iv = parse_len_iv(&private).unwrap_or(DEFAULT_LEN_IV);
        let subrs = parse_subrs(&private, len_iv);
        let charstrings = parse_charstrings(&private, len_iv);
        if charstrings.is_empty() {
            return None;
        }
        Some(Self {
            #[cfg(test)]
            len_iv,
            subrs,
            charstrings,
        })
    }

    pub(crate) fn is_type1(font_bytes: &[u8]) -> bool {
        find_token(font_bytes, b"eexec").is_some() || font_bytes.starts_with(b"%!PS-AdobeFont")
    }

    pub(crate) fn outline_by_name(&self, glyph_name: &str) -> (Option<Path>, f64) {
        let Some(charstring) = self.charstrings.get(glyph_name) else {
            return (None, 500.0);
        };
        let mut interpreter = Interpreter::new(self);
        match interpreter.execute(charstring, 0) {
            Ok(()) => (non_empty_path(interpreter.path), interpreter.width),
            Err(_) => (None, interpreter.width),
        }
    }

    #[cfg(test)]
    pub(crate) fn glyph_count(&self) -> usize {
        self.charstrings.len()
    }

    #[cfg(test)]
    pub(crate) fn subr_count(&self) -> usize {
        self.subrs.len()
    }

    #[cfg(test)]
    pub(crate) fn len_iv(&self) -> i32 {
        self.len_iv
    }
}

pub(crate) fn outline_by_name(font_bytes: &[u8], glyph_name: &str) -> Option<(Option<Path>, f64)> {
    let font = Type1Font::parse(font_bytes)?;
    Some(font.outline_by_name(glyph_name))
}

pub(crate) fn units_per_em() -> f64 {
    1000.0
}

fn decrypt_private_program(font_bytes: &[u8]) -> Option<Vec<u8>> {
    let eexec_pos = find_token(font_bytes, b"eexec")?;
    let mut payload = &font_bytes[eexec_pos + b"eexec".len()..];
    payload = trim_leading_space(payload);
    let encrypted = if looks_like_hex_eexec(payload) {
        decode_hex_stream(payload)
    } else {
        payload.to_vec()
    };
    if encrypted.len() <= 4 {
        return None;
    }
    let decrypted = decrypt_type1(&encrypted, EEXEC_KEY);
    Some(decrypted.get(4..)?.to_vec())
}

fn decrypt_charstring(data: &[u8], len_iv: i32) -> Vec<u8> {
    if len_iv < 0 {
        return data.to_vec();
    }
    let decrypted = decrypt_type1(data, CHARSTRING_KEY);
    let skip = len_iv as usize;
    if decrypted.len() <= skip {
        Vec::new()
    } else {
        decrypted[skip..].to_vec()
    }
}

fn decrypt_type1(data: &[u8], mut r: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &cipher in data {
        let plain = cipher ^ (r >> 8) as u8;
        r = ((u16::from(cipher)).wrapping_add(r))
            .wrapping_mul(C1)
            .wrapping_add(C2);
        out.push(plain);
    }
    out
}

fn parse_len_iv(private: &[u8]) -> Option<i32> {
    let pos = find_token(private, b"/lenIV")?;
    let mut p = pos + b"/lenIV".len();
    skip_space(private, &mut p);
    read_i32(private, &mut p)
}

fn parse_subrs(private: &[u8], len_iv: i32) -> HashMap<i32, Vec<u8>> {
    let Some(start) = find_token(private, b"/Subrs") else {
        return HashMap::new();
    };
    let end = find_token_from(private, b"/CharStrings", start).unwrap_or(private.len());
    let mut pos = start;
    let mut subrs = HashMap::new();
    while pos < end {
        if !starts_word(private, pos, b"dup") {
            pos += 1;
            continue;
        }
        let mut p = pos + 3;
        skip_space(private, &mut p);
        let Some(index) = read_i32(private, &mut p) else {
            pos += 1;
            continue;
        };
        skip_space(private, &mut p);
        let Some(length) = read_i32(private, &mut p).filter(|v| *v >= 0) else {
            pos += 1;
            continue;
        };
        skip_space(private, &mut p);
        if !read_binary_token(private, &mut p) {
            pos += 1;
            continue;
        }
        skip_one_binary_delimiter(private, &mut p);
        let length = length as usize;
        let Some(data_end) = p.checked_add(length).filter(|value| *value <= end) else {
            pos += 1;
            continue;
        };
        subrs.insert(index, decrypt_charstring(&private[p..data_end], len_iv));
        pos = data_end;
    }
    subrs
}

fn parse_charstrings(private: &[u8], len_iv: i32) -> HashMap<String, Vec<u8>> {
    let Some(start) = find_token(private, b"/CharStrings") else {
        return HashMap::new();
    };
    let mut pos = start + b"/CharStrings".len();
    let mut charstrings = HashMap::new();
    while pos < private.len() {
        if private[pos] != b'/' {
            pos += 1;
            continue;
        }
        let mut p = pos + 1;
        let Some(name) = read_name(private, &mut p) else {
            pos += 1;
            continue;
        };
        skip_space(private, &mut p);
        let Some(length) = read_i32(private, &mut p).filter(|v| *v >= 0) else {
            pos += 1;
            continue;
        };
        skip_space(private, &mut p);
        if !read_binary_token(private, &mut p) {
            pos += 1;
            continue;
        }
        skip_one_binary_delimiter(private, &mut p);
        let length = length as usize;
        let Some(data_end) = p
            .checked_add(length)
            .filter(|value| *value <= private.len())
        else {
            pos += 1;
            continue;
        };
        charstrings.insert(name, decrypt_charstring(&private[p..data_end], len_iv));
        pos = data_end;
    }
    charstrings
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecSignal {
    Continue,
    Return,
    End,
}

struct Interpreter<'a> {
    font: &'a Type1Font,
    path: Path,
    stack: Vec<f64>,
    othersubr_results: Vec<f64>,
    x: f64,
    y: f64,
    width: f64,
}

impl<'a> Interpreter<'a> {
    fn new(font: &'a Type1Font) -> Self {
        Self {
            font,
            path: Path::new(),
            stack: Vec::new(),
            othersubr_results: Vec::new(),
            x: 0.0,
            y: 0.0,
            width: 500.0,
        }
    }

    fn execute(&mut self, charstring: &[u8], depth: usize) -> Result<(), ()> {
        match self.execute_inner(charstring, depth)? {
            ExecSignal::Continue | ExecSignal::Return | ExecSignal::End => Ok(()),
        }
    }

    fn execute_inner(&mut self, charstring: &[u8], depth: usize) -> Result<ExecSignal, ()> {
        if depth > MAX_SUBR_DEPTH {
            return Err(());
        }
        let mut pos = 0usize;
        while pos < charstring.len() {
            let byte = charstring[pos];
            pos += 1;
            if byte >= 32 {
                let value = read_charstring_number(byte, charstring, &mut pos)?;
                self.stack.push(value);
                continue;
            }
            let signal = if byte == 12 {
                if pos >= charstring.len() {
                    return Err(());
                }
                let escaped = charstring[pos];
                pos += 1;
                self.exec_escaped(escaped, depth)?
            } else {
                self.exec_operator(byte, depth)?
            };
            match signal {
                ExecSignal::Continue => {}
                ExecSignal::Return | ExecSignal::End => return Ok(signal),
            }
        }
        Ok(ExecSignal::Continue)
    }

    fn exec_operator(&mut self, op: u8, depth: usize) -> Result<ExecSignal, ()> {
        match op {
            1 | 3 => self.clear(), // hstem, vstem
            4 => {
                let dy = self.pop1()?;
                self.move_to(self.x, self.y + dy);
                self.clear();
            }
            5 => {
                let values = self.take_stack();
                for pair in values.chunks_exact(2) {
                    self.line_to(self.x + pair[0], self.y + pair[1]);
                }
            }
            6 => {
                let values = self.take_stack();
                for dx in values {
                    self.line_to(self.x + dx, self.y);
                }
            }
            7 => {
                let values = self.take_stack();
                for dy in values {
                    self.line_to(self.x, self.y + dy);
                }
            }
            8 => {
                let values = self.take_stack();
                for c in values.chunks_exact(6) {
                    self.curve_to(
                        self.x + c[0],
                        self.y + c[1],
                        self.x + c[0] + c[2],
                        self.y + c[1] + c[3],
                        self.x + c[0] + c[2] + c[4],
                        self.y + c[1] + c[3] + c[5],
                    );
                }
            }
            9 => {
                self.path.close();
                self.clear();
            }
            10 => {
                let index = self.pop1()? as i32;
                if let Some(subr) = self.font.subrs.get(&index) {
                    match self.execute_inner(subr, depth + 1)? {
                        ExecSignal::Continue | ExecSignal::Return => {}
                        ExecSignal::End => return Ok(ExecSignal::End),
                    }
                }
            }
            11 => return Ok(ExecSignal::Return),
            13 => {
                let values = self.take_stack();
                if values.len() >= 2 {
                    self.x = values[0];
                    self.y = 0.0;
                    self.width = values[1];
                }
            }
            14 => {
                self.clear();
                return Ok(ExecSignal::End);
            }
            21 => {
                let values = self.take_stack();
                if values.len() >= 2 {
                    self.move_to(self.x + values[0], self.y + values[1]);
                }
            }
            22 => {
                let dx = self.pop1()?;
                self.move_to(self.x + dx, self.y);
                self.clear();
            }
            30 => {
                let values = self.take_stack();
                for c in values.chunks_exact(4) {
                    self.curve_to(
                        self.x,
                        self.y + c[0],
                        self.x + c[1],
                        self.y + c[0] + c[2],
                        self.x + c[1] + c[3],
                        self.y + c[0] + c[2],
                    );
                }
            }
            31 => {
                let values = self.take_stack();
                for c in values.chunks_exact(4) {
                    self.curve_to(
                        self.x + c[0],
                        self.y,
                        self.x + c[0] + c[1],
                        self.y + c[2],
                        self.x + c[0] + c[1],
                        self.y + c[2] + c[3],
                    );
                }
            }
            _ => self.clear(),
        }
        Ok(ExecSignal::Continue)
    }

    fn exec_escaped(&mut self, op: u8, _depth: usize) -> Result<ExecSignal, ()> {
        match op {
            0..=2 => self.clear(), // dotsection, vstem3, hstem3
            6 => {
                let values = self.take_stack();
                if values.len() >= 5 {
                    self.compose_seac(values[1], values[2], values[3] as i32, values[4] as i32);
                }
            }
            7 => {
                let values = self.take_stack();
                if values.len() >= 4 {
                    self.x = values[0];
                    self.y = values[1];
                    self.width = values[2];
                }
            }
            12 => {
                let b = self.pop1()?;
                let a = self.pop1()?;
                if b.abs() > f64::EPSILON {
                    self.stack.push(a / b);
                } else {
                    self.stack.push(0.0);
                }
            }
            16 => {
                let othersubr = self.pop1()? as i32;
                let nargs = self.pop1()?.max(0.0) as usize;
                let mut args = Vec::new();
                for _ in 0..nargs {
                    args.push(self.stack.pop().unwrap_or(0.0));
                }
                args.reverse();
                self.handle_callothersubr(othersubr, args);
                self.clear();
            }
            17 => {
                let value = self.othersubr_results.pop().unwrap_or(0.0);
                self.stack.push(value);
            }
            33 => {
                let values = self.take_stack();
                if values.len() >= 2 {
                    self.x = values[0];
                    self.y = values[1];
                }
            }
            _ => self.clear(),
        }
        Ok(ExecSignal::Continue)
    }

    fn handle_callothersubr(&mut self, othersubr: i32, args: Vec<f64>) {
        match othersubr {
            // Standard OtherSubr 3 is used for hint replacement. It returns its
            // single argument to the following `pop`, commonly a Subrs index.
            3 => {
                self.othersubr_results.extend(args.into_iter().rev());
            }
            // Standard flex OtherSubrs 0, 1 and 2 mostly communicate via the
            // Type1 interpreter state. These outlines still contain the normal
            // curve operators in the fonts we exercise; consume the operands so
            // the stack remains balanced.
            0..=2 => {}
            _ => {}
        }
    }

    fn compose_seac(&mut self, adx: f64, ady: f64, bchar: i32, achar: i32) {
        let base = standard_encoding_name(bchar);
        let accent = standard_encoding_name(achar);
        let old_x = self.x;
        let old_y = self.y;
        if let Some(base) = base.and_then(|name| self.font.charstrings.get(name)) {
            let mut sub = Interpreter::new(self.font);
            if sub.execute(base, 0).is_ok() {
                append_translated(&mut self.path, &sub.path, 0.0, 0.0);
            }
        }
        if let Some(accent) = accent.and_then(|name| self.font.charstrings.get(name)) {
            let mut sub = Interpreter::new(self.font);
            if sub.execute(accent, 0).is_ok() {
                append_translated(&mut self.path, &sub.path, adx, ady);
            }
        }
        self.x = old_x;
        self.y = old_y;
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.path.move_to(x, y);
        self.x = x;
        self.y = y;
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.path.line_to(x, y);
        self.x = x;
        self.y = y;
    }

    fn curve_to(&mut self, cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64) {
        self.path.curve_to(cp1x, cp1y, cp2x, cp2y, x, y);
        self.x = x;
        self.y = y;
    }

    fn pop1(&mut self) -> Result<f64, ()> {
        self.stack.pop().ok_or(())
    }

    fn take_stack(&mut self) -> Vec<f64> {
        std::mem::take(&mut self.stack)
    }

    fn clear(&mut self) {
        self.stack.clear();
    }
}

fn read_charstring_number(first: u8, data: &[u8], pos: &mut usize) -> Result<f64, ()> {
    match first {
        32..=246 => Ok(f64::from(i32::from(first) - 139)),
        247..=250 => {
            let b1 = *data.get(*pos).ok_or(())?;
            *pos += 1;
            Ok(f64::from(
                (i32::from(first) - 247) * 256 + i32::from(b1) + 108,
            ))
        }
        251..=254 => {
            let b1 = *data.get(*pos).ok_or(())?;
            *pos += 1;
            Ok(f64::from(
                -((i32::from(first) - 251) * 256) - i32::from(b1) - 108,
            ))
        }
        255 => {
            let bytes = data.get(*pos..(*pos).saturating_add(4)).ok_or(())?;
            *pos += 4;
            Ok(f64::from(i32::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3],
            ])))
        }
        _ => Err(()),
    }
}

fn append_translated(dst: &mut Path, src: &Path, dx: f64, dy: f64) {
    for segment in &src.segments {
        match *segment {
            PathSegment::MoveTo(x, y) => dst.move_to(x + dx, y + dy),
            PathSegment::LineTo(x, y) => dst.line_to(x + dx, y + dy),
            PathSegment::CubicTo {
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x,
                y,
            } => dst.curve_to(cp1x + dx, cp1y + dy, cp2x + dx, cp2y + dy, x + dx, y + dy),
            PathSegment::ClosePath => dst.close(),
        }
    }
}

fn non_empty_path(path: Path) -> Option<Path> {
    if path.segments.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn standard_encoding_name(code: i32) -> Option<&'static str> {
    u8::try_from(code)
        .ok()
        .map(|code| Encoding::lookup("StandardEncoding", code))
        .filter(|name| *name != ".notdef")
}

fn find_token(data: &[u8], token: &[u8]) -> Option<usize> {
    find_token_from(data, token, 0)
}

fn find_token_from(data: &[u8], token: &[u8], start: usize) -> Option<usize> {
    data.get(start..)?
        .windows(token.len())
        .position(|window| window == token)
        .map(|pos| start + pos)
}

fn starts_word(data: &[u8], pos: usize, word: &[u8]) -> bool {
    data.get(pos..pos.saturating_add(word.len())) == Some(word)
        && data
            .get(pos + word.len())
            .map(|b| is_space(*b))
            .unwrap_or(true)
        && (pos == 0 || data.get(pos - 1).map(|b| is_space(*b)).unwrap_or(true))
}

fn trim_leading_space(mut data: &[u8]) -> &[u8] {
    while data.first().copied().map(is_space).unwrap_or(false) {
        data = &data[1..];
    }
    data
}

fn looks_like_hex_eexec(data: &[u8]) -> bool {
    let mut seen = 0usize;
    for &b in data.iter().take(512) {
        if is_space(b) {
            continue;
        }
        if !b.is_ascii_hexdigit() {
            return false;
        }
        seen += 1;
    }
    seen >= 8
}

fn decode_hex_stream(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for &b in data {
        if is_space(b) {
            continue;
        }
        let Some(nibble) = hex_value(b) else {
            break;
        };
        if let Some(h) = high.take() {
            out.push((h << 4) | nibble);
        } else {
            high = Some(nibble);
        }
    }
    if let Some(h) = high {
        out.push(h << 4);
    }
    out
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn skip_space(data: &[u8], pos: &mut usize) {
    while *pos < data.len() && is_space(data[*pos]) {
        *pos += 1;
    }
}

fn skip_one_binary_delimiter(data: &[u8], pos: &mut usize) {
    if *pos < data.len() && is_space(data[*pos]) {
        *pos += 1;
    }
}

fn read_i32(data: &[u8], pos: &mut usize) -> Option<i32> {
    skip_space(data, pos);
    let start = *pos;
    if *pos < data.len() && (data[*pos] == b'+' || data[*pos] == b'-') {
        *pos += 1;
    }
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start || (*pos == start + 1 && matches!(data[start], b'+' | b'-')) {
        return None;
    }
    std::str::from_utf8(&data[start..*pos]).ok()?.parse().ok()
}

fn read_name(data: &[u8], pos: &mut usize) -> Option<String> {
    let start = *pos;
    while *pos < data.len() && !is_delimiter(data[*pos]) {
        *pos += 1;
    }
    if *pos == start {
        return None;
    }
    Some(String::from_utf8_lossy(&data[start..*pos]).into_owned())
}

fn read_binary_token(data: &[u8], pos: &mut usize) -> bool {
    let start = *pos;
    while *pos < data.len() && !is_space(data[*pos]) {
        *pos += 1;
    }
    matches!(&data[start..*pos], b"RD" | b"-|" | b"|-")
}

fn is_space(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(b: u8) -> bool {
    is_space(b) || matches!(b, b'/' | b'[' | b']' | b'<' | b'>' | b'(' | b')')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::PdfDocument;
    use crate::engine::PageResources;
    use crate::render::font_rasterizer::FontRasterizer;

    #[test]
    fn type1_decrypts_tracemonkey_font_program() {
        let bytes = tracemonkey_type1_font_bytes("F41");
        let font = Type1Font::parse(&bytes).expect("embedded Type1 should parse");
        assert_eq!(font.len_iv(), DEFAULT_LEN_IV);
        assert!(font.glyph_count() > 20);
        assert!(font.subr_count() > 20);
    }

    #[test]
    fn type1_interprets_tracemonkey_glyph_outline() {
        let bytes = tracemonkey_type1_font_bytes("F41");
        let font = Type1Font::parse(&bytes).expect("embedded Type1 should parse");
        let (outline, advance) = font.outline_by_name("A");
        let outline = outline.expect("glyph A should have an outline");
        assert!(outline.segments.len() > 5);
        assert!(advance > 500.0);
    }

    fn tracemonkey_type1_font_bytes(resource_name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("tracemonkey.pdf");
        let doc = PdfDocument::open_path(path).expect("fixture opens");
        let page = doc.get_pages().expect("pages")[0].clone();
        let resources = PageResources::from_dict(&page.resources, doc.reader());
        let font_dict = resources.fonts.get(resource_name).expect("font resource");
        FontRasterizer::extract_font_bytes(font_dict, doc.reader()).expect("font bytes")
    }
}
