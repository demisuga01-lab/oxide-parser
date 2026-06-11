use std::collections::HashMap;

use crate::content::{ContentOperation, GraphicsState, Operand};
use crate::engine::PageResources;
use crate::fonts::FontResolver;
use crate::reader::PdfReader;

#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub font_size: f64,
    pub font_name: String,
    pub width: f64,
    pub is_rtl: bool,
    pub is_vertical: bool,
    pub is_invisible: bool,
}

impl TextChunk {
    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn is_whitespace(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// True if this chunk is part of a hidden OCR layer (rendering mode 3).
    pub fn is_ocr_layer(&self) -> bool {
        self.is_invisible
    }
}

/// Returns true if the character belongs to a right-to-left script.
pub(crate) fn is_rtl_char(c: char) -> bool {
    let cp = c as u32;
    matches!(
        cp,
        0x0590..=0x05FF
            | 0x0600..=0x06FF
            | 0x0700..=0x074F
            | 0x0750..=0x077F
            | 0x07C0..=0x07FF
            | 0x0800..=0x083F
            | 0x0840..=0x085F
            | 0xFB1D..=0xFB4F
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
    )
}

/// Returns true if more than half the alphabetic characters are RTL.
pub(crate) fn is_rtl_dominant(s: &str) -> bool {
    let mut rtl_count = 0usize;
    let mut total_count = 0usize;

    for c in s.chars() {
        if c.is_alphabetic() {
            total_count += 1;
            if is_rtl_char(c) {
                rtl_count += 1;
            }
        }
    }

    total_count > 0 && rtl_count * 2 > total_count
}

pub struct TextCollector<'a> {
    gs: GraphicsState,
    font_resolvers: HashMap<String, FontResolver>,
    resources: PageResources,
    reader: Option<&'a PdfReader>,
}

impl<'a> TextCollector<'a> {
    pub fn new(resources: PageResources, reader: &'a PdfReader) -> Self {
        Self {
            gs: GraphicsState::new(),
            font_resolvers: HashMap::new(),
            resources,
            reader: Some(reader),
        }
    }

    pub fn new_with_resolvers(
        resources: PageResources,
        resolvers: HashMap<String, FontResolver>,
    ) -> TextCollector<'static> {
        TextCollector {
            gs: GraphicsState::new(),
            font_resolvers: resolvers,
            resources,
            reader: None,
        }
    }

    pub fn collect(&mut self, operations: &[ContentOperation]) -> Vec<TextChunk> {
        self.gs = GraphicsState::new();
        if self.reader.is_some() {
            self.font_resolvers.clear();
        }

        let mut chunks = Vec::new();
        for operation in operations {
            self.process_op(operation, &mut chunks);
        }
        chunks
    }

    fn process_op(&mut self, op: &ContentOperation, chunks: &mut Vec<TextChunk>) {
        match op.operator.as_str() {
            "Tj" => self.show_bytes(op.string_bytes(0).unwrap_or(&[]).to_vec(), chunks),
            "'" => {
                self.gs.process(&ContentOperation::new("T*", vec![]));
                self.show_bytes(op.string_bytes(0).unwrap_or(&[]).to_vec(), chunks);
            }
            "\"" => {
                if let Some(aw) = op.number(0) {
                    self.gs.text.word_spacing = aw;
                }
                if let Some(ac) = op.number(1) {
                    self.gs.text.char_spacing = ac;
                }
                self.gs.process(&ContentOperation::new("T*", vec![]));
                self.show_bytes(op.string_bytes(2).unwrap_or(&[]).to_vec(), chunks);
            }
            "TJ" => self.show_tj(op, chunks),
            "Tf" => {
                self.gs.process(op);
                if let Some(name) = op.name(0) {
                    self.ensure_font_loaded(name);
                }
            }
            "gs" => {
                if let Some(name) = op.name(0) {
                    if let Some(ext_dict) = self.resources.ext_g_states.get(name).cloned() {
                        self.gs.apply_ext_g_state(&ext_dict);
                    } else {
                        log::warn!("TextCollector: ExtGState '{}' not found in resources", name);
                    }
                }
            }
            _ => self.gs.process(op),
        }
    }

    fn ensure_font_loaded(&mut self, font_name: &str) {
        if self.font_resolvers.contains_key(font_name) {
            return;
        }
        let Some(font_dict) = self.resources.fonts.get(font_name).cloned() else {
            log::warn!(
                "TextCollector: font '{}' not found in page resources",
                font_name
            );
            return;
        };
        let Some(reader) = self.reader else {
            log::warn!(
                "TextCollector: no PdfReader available to load font '{}'",
                font_name
            );
            return;
        };
        self.font_resolvers
            .insert(font_name.to_string(), FontResolver::new(&font_dict, reader));
    }

    fn show_bytes(&mut self, bytes: Vec<u8>, chunks: &mut Vec<TextChunk>) {
        if bytes.is_empty() {
            return;
        }

        let font_name = self.gs.text.font_name.clone();
        let font_size = self.gs.text.font_size;
        let char_spacing = self.gs.text.char_spacing;
        let word_spacing = self.gs.text.word_spacing;
        let h_scale = self.gs.text.horizontal_scaling / 100.0;
        let rise = self.gs.text.rise;
        let x_start = self.gs.text.tm[4];
        let y_start = self.gs.text.tm[5] + rise;
        let font_size_eff = font_size * self.gs.effective_font_size();

        let resolver = self.font_resolvers.get(&font_name);
        let code_size = resolver.map(FontResolver::code_size).unwrap_or(1);
        let codes = extract_char_codes(&bytes, code_size);
        let mut decoded_text = String::new();
        let mut total_advance = 0.0_f64;

        for code in &codes {
            let ch_text = match resolver {
                Some(resolver) => resolver.decode_char(*code),
                None if (0x20..=0x7E).contains(code) => char::from(*code as u8).to_string(),
                None => "\u{FFFD}".to_string(),
            };
            decoded_text.push_str(&ch_text);

            let glyph_units = resolver
                .map(|resolver| resolver.glyph_width(*code))
                .unwrap_or(500.0);
            let is_space = resolver
                .map(|resolver| resolver.is_space_code(*code))
                .unwrap_or(*code == 0x20);
            let tx = (glyph_units / 1000.0 * font_size
                + char_spacing
                + if is_space { word_spacing } else { 0.0 })
                * h_scale;

            self.gs.text.tm[4] += self.gs.text.tm[0] * tx;
            self.gs.text.tm[5] += self.gs.text.tm[1] * tx;
            total_advance += tx;
        }

        if !decoded_text.is_empty() {
            let width_x = self.gs.text.tm[0] * total_advance;
            let width_y = self.gs.text.tm[1] * total_advance;
            let width = (width_x.powi(2) + width_y.powi(2)).sqrt();
            let is_rtl = is_rtl_dominant(&decoded_text);
            let tm = &self.gs.text.tm;
            let is_vertical = tm[1].abs() > tm[0].abs() + 0.1;
            // NOTE: we extract text regardless of rendering_mode.
            // rendering_mode=3 (invisible) is used for OCR layers in scanned PDFs;
            // extracting it is correct and deliberate. If callers want to filter
            // invisible text, they can inspect TextChunk::is_ocr_layer() after
            // extraction. We never skip based on rendering mode here.
            let is_invisible = self.gs.text.rendering_mode == 3;
            chunks.push(TextChunk {
                text: decoded_text,
                x: x_start,
                y: y_start,
                font_size: font_size_eff,
                font_name,
                width,
                is_rtl,
                is_vertical,
                is_invisible,
            });
        }
    }

    fn show_tj(&mut self, op: &ContentOperation, chunks: &mut Vec<TextChunk>) {
        let Some(array) = op
            .operand(0)
            .and_then(Operand::as_array)
            .map(<[Operand]>::to_vec)
        else {
            log::warn!("TextCollector: TJ operand is not an array");
            return;
        };
        let font_size = self.gs.text.font_size;
        let h_scale = self.gs.text.horizontal_scaling / 100.0;

        for elem in array {
            match elem {
                Operand::String(bytes) => self.show_bytes(bytes, chunks),
                Operand::Integer(value) => {
                    let tx = -(value as f64) / 1000.0 * font_size * h_scale;
                    self.gs.text.tm[4] += self.gs.text.tm[0] * tx;
                    self.gs.text.tm[5] += self.gs.text.tm[1] * tx;
                }
                Operand::Real(value) => {
                    let tx = -value / 1000.0 * font_size * h_scale;
                    self.gs.text.tm[4] += self.gs.text.tm[0] * tx;
                    self.gs.text.tm[5] += self.gs.text.tm[1] * tx;
                }
                _ => {}
            }
        }
    }
}

fn extract_char_codes(bytes: &[u8], code_size: u8) -> Vec<u16> {
    if code_size == 2 {
        bytes
            .chunks(2)
            .map(|chunk| {
                let high = u16::from(chunk[0]);
                let low = chunk.get(1).copied().map(u16::from).unwrap_or(0);
                (high << 8) | low
            })
            .collect()
    } else {
        bytes.iter().map(|byte| u16::from(*byte)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentParser;
    use crate::object::{PdfDictionary, PdfObject};

    fn make_test_resources() -> PageResources {
        let mut resources = PageResources::default();
        let mut font_dict = PdfDictionary::empty();
        font_dict.insert("Type", PdfObject::Name("Font".to_string()));
        font_dict.insert("Subtype", PdfObject::Name("Type1".to_string()));
        font_dict.insert("Encoding", PdfObject::Name("StandardEncoding".to_string()));
        font_dict.insert("FirstChar", PdfObject::Integer(32));
        font_dict.insert("LastChar", PdfObject::Integer(126));
        let widths = (32..=126)
            .map(|_| PdfObject::Integer(600))
            .collect::<Vec<_>>();
        font_dict.insert("Widths", PdfObject::Array(widths));
        resources.fonts.insert("F1".to_string(), font_dict);
        resources
    }

    fn collect_text(stream: &[u8]) -> Vec<TextChunk> {
        let resources = make_test_resources();
        let mut resolvers = HashMap::new();
        if let Some(font_dict) = resources.fonts.get("F1") {
            resolvers.insert(
                "F1".to_string(),
                FontResolver::new_from_dict_only(font_dict),
            );
        }
        let mut collector = TextCollector::new_with_resolvers(resources, resolvers);
        let operations = ContentParser::parse(stream).unwrap_or_default();
        collector.collect(&operations)
    }

    fn make_std_encoding_font() -> FontResolver {
        let mut d = PdfDictionary::empty();
        d.insert("Type", PdfObject::Name("Font".to_string()));
        d.insert("Subtype", PdfObject::Name("Type1".to_string()));
        d.insert("Encoding", PdfObject::Name("StandardEncoding".to_string()));
        d.insert("FirstChar", PdfObject::Integer(0xAE));
        d.insert("LastChar", PdfObject::Integer(0xAF));
        d.insert(
            "Widths",
            PdfObject::Array(vec![PdfObject::Integer(600), PdfObject::Integer(600)]),
        );
        FontResolver::new_from_dict_only(&d)
    }

    fn make_win_ansi_font() -> FontResolver {
        let mut d = PdfDictionary::empty();
        d.insert("Type", PdfObject::Name("Font".to_string()));
        d.insert("Subtype", PdfObject::Name("Type1".to_string()));
        d.insert("Encoding", PdfObject::Name("WinAnsiEncoding".to_string()));
        d.insert("FirstChar", PdfObject::Integer(0x80));
        d.insert("LastChar", PdfObject::Integer(0x9F));
        d.insert(
            "Widths",
            PdfObject::Array((0x80u8..=0x9F).map(|_| PdfObject::Integer(600)).collect()),
        );
        FontResolver::new_from_dict_only(&d)
    }

    #[test]
    fn basic_tj_positioning() {
        let chunks = collect_text(b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello");
        assert!((chunks[0].x - 100.0).abs() < 1.0);
        assert!((chunks[0].y - 700.0).abs() < 1.0);
        assert!((chunks[0].font_size - 12.0).abs() < 1.0);
        assert_eq!(chunks[0].font_name, "F1");
    }

    #[test]
    fn position_advances_after_tj() {
        let chunks = collect_text(b"BT /F1 12 Tf 100 700 Td (Hi) Tj (World) Tj ET");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Hi");
        assert_eq!(chunks[1].text, "World");
        let expected_x2 = 100.0 + 2.0 * 600.0 / 1000.0 * 12.0;
        assert!((chunks[1].x - expected_x2).abs() < 0.5);
    }

    #[test]
    fn tj_negative_number_adjusts_right() {
        let chunks = collect_text(b"BT /F1 12 Tf 100 700 Td [(Hi) -100 (World)] TJ ET");
        assert_eq!(chunks.len(), 2);
        let expected = 100.0 + 2.0 * 600.0 / 1000.0 * 12.0 + 100.0 / 1000.0 * 12.0;
        assert!((chunks[1].x - expected).abs() < 0.5);
    }

    #[test]
    fn tj_positive_number_moves_left() {
        let chunks = collect_text(b"BT /F1 12 Tf 100 700 Td [(Hi) 1000 (World)] TJ ET");
        let expected = 100.0 + 14.4 - 12.0;
        assert!((chunks[1].x - expected).abs() < 0.5);
    }

    #[test]
    fn tstar_advances_by_leading() {
        let chunks = collect_text(b"BT /F1 12 Tf 14 TL 100 700 Td (Line1) Tj T* (Line2) Tj ET");
        assert_eq!(chunks.len(), 2);
        assert!((chunks[0].y - 700.0).abs() < 1.0);
        assert!((chunks[1].y - 686.0).abs() < 1.0);
    }

    #[test]
    fn td_upper_sets_leading_and_moves() {
        let chunks = collect_text(b"BT /F1 12 Tf 100 700 Td (Line1) Tj 0 -16 TD (Line2) Tj ET");
        assert_eq!(chunks.len(), 2);
        assert!((chunks[1].y - 684.0).abs() < 1.0);
    }

    #[test]
    fn apostrophe_operator_advances_line_then_shows() {
        let chunks = collect_text(b"BT /F1 12 Tf 14 TL 100 700 Td (Line1) Tj (Line2) ' ET");
        assert_eq!(chunks.len(), 2);
        assert!((chunks[1].y - 686.0).abs() < 1.0);
    }

    #[test]
    fn multiple_text_blocks_reset_matrices() {
        let chunks = collect_text(
            b"BT /F1 12 Tf 100 700 Td (Block1) Tj ET BT /F1 12 Tf 100 650 Td (Block2) Tj ET",
        );
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Block1");
        assert_eq!(chunks[1].text, "Block2");
        assert!((chunks[0].y - 700.0).abs() < 1.0);
        assert!((chunks[1].y - 650.0).abs() < 1.0);
    }

    #[test]
    fn graphics_state_save_restore_does_not_corrupt_text_state() {
        let chunks = collect_text(b"BT /F1 12 Tf 100 700 Td q 0 0.5 0 rg Q (Hello) Tj ET");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello");
        assert!((chunks[0].x - 100.0).abs() < 1.0);
    }

    #[test]
    fn cm_before_text_does_not_panic() {
        let chunks = collect_text(b"1 0 0 1 50 0 cm BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn char_code_extraction_pads_odd_two_byte_input() {
        assert_eq!(
            extract_char_codes(&[0x12, 0x34, 0x56], 2),
            vec![0x1234, 0x5600]
        );
        assert_eq!(extract_char_codes(&[0x41, 0x42], 1), vec![0x41, 0x42]);
    }

    #[test]
    fn fi_ligature_expands_to_two_chars() {
        let resolver = make_std_encoding_font();
        let decoded = resolver.decode_string(&[0xAE]);
        assert_eq!(
            decoded, "fi",
            "fi ligature should expand to 'fi', got {:?}",
            decoded
        );
    }

    #[test]
    fn fl_ligature_expands_to_two_chars() {
        let resolver = make_std_encoding_font();
        let decoded = resolver.decode_string(&[0xAF]);
        assert_eq!(
            decoded, "fl",
            "fl ligature should expand to 'fl', got {:?}",
            decoded
        );
    }

    #[test]
    fn win_ansi_euro_decodes_correctly() {
        let resolver = make_win_ansi_font();
        let decoded = resolver.decode_string(&[0x80]);
        assert_eq!(decoded, "\u{20AC}", "0x80 in WinAnsi should be Euro sign");
    }

    #[test]
    fn win_ansi_emdash_decodes_correctly() {
        let resolver = make_win_ansi_font();
        let decoded = resolver.decode_string(&[0x97]);
        assert_eq!(decoded, "\u{2014}", "0x97 in WinAnsi should be em dash");
    }

    #[test]
    fn win_ansi_endash_decodes_correctly() {
        let resolver = make_win_ansi_font();
        let decoded = resolver.decode_string(&[0x96]);
        assert_eq!(decoded, "\u{2013}", "0x96 in WinAnsi should be en dash");
    }

    #[test]
    fn is_rtl_dominant_detects_arabic_and_hebrew() {
        assert!(is_rtl_dominant("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"));
        assert!(is_rtl_dominant("\u{05E9}\u{05DC}\u{05D5}\u{05DD}"));
    }

    #[test]
    fn is_rtl_dominant_false_for_latin() {
        assert!(!is_rtl_dominant("Hello World"));
        assert!(!is_rtl_dominant(""));
        assert!(!is_rtl_dominant("123"));
    }

    #[test]
    fn is_rtl_dominant_handles_mixed_text() {
        assert!(!is_rtl_dominant(
            "Hello \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"
        ));
        assert!(!is_rtl_dominant(
            "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} Hello"
        ));
        assert!(is_rtl_dominant(
            "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0628}\u{0627}\u{0644}\u{0639}\u{0627}\u{0644}\u{0645}"
        ));
    }

    #[test]
    fn invisible_text_chunk_is_still_collected() {
        let stream = b"BT /F1 12 Tf 100 700 Td 3 Tr (Hidden) Tj ET";
        let resources = make_test_resources();
        let mut resolvers = HashMap::new();
        if let Some(font_dict) = resources.fonts.get("F1") {
            resolvers.insert(
                "F1".to_string(),
                FontResolver::new_from_dict_only(font_dict),
            );
        }
        let mut collector = TextCollector::new_with_resolvers(resources, resolvers);
        let ops = ContentParser::parse(stream).unwrap();
        let chunks = collector.collect(&ops);
        assert!(
            !chunks.is_empty(),
            "invisible text should still be extracted"
        );
        let hidden = chunks.iter().find(|c| c.text.contains("Hidden"));
        assert!(hidden.is_some(), "should find Hidden text in output");
        assert!(
            hidden.unwrap().is_invisible,
            "chunk with rendering_mode=3 should be marked invisible"
        );
        assert!(hidden.unwrap().is_ocr_layer());
    }

    #[test]
    fn text_chunk_helpers_work() {
        let chunk = TextChunk {
            text: " ".to_string(),
            x: 10.0,
            y: 20.0,
            font_size: 12.0,
            font_name: "F1".to_string(),
            width: 5.0,
            is_rtl: false,
            is_vertical: false,
            is_invisible: true,
        };
        assert_eq!(chunk.right(), 15.0);
        assert!(chunk.is_whitespace());
        assert!(chunk.is_ocr_layer());
    }
}
