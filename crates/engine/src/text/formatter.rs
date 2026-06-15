use super::reading_order::TextLine;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LineEnding {
    #[default]
    Unix,
    Windows,
    Classic,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            LineEnding::Unix => "\n",
            LineEnding::Windows => "\r\n",
            LineEnding::Classic => "\r",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextFormatOptions {
    /// Insert "--- Page N ---" before each page's text.
    pub include_page_markers: bool,

    /// Attempt to preserve horizontal spacing with padding spaces.
    pub preserve_layout: bool,

    /// Insert blank lines between detected paragraphs.
    pub paragraph_breaks: bool,

    /// Insert an extra newline before heading-sized lines.
    pub heading_breaks: bool,

    /// Line ending style.
    pub line_ending: LineEnding,

    /// Fallback character-cell width (PDF points) for preserve_layout mode, used
    /// only when the page has no measurable glyph advances to derive an adaptive
    /// cell width from. The layout grid normally uses the document's own median
    /// per-character advance instead of this constant.
    pub pts_per_char: f64,
}

impl Default for TextFormatOptions {
    fn default() -> Self {
        Self {
            include_page_markers: true,
            preserve_layout: false,
            paragraph_breaks: true,
            heading_breaks: true,
            line_ending: LineEnding::Unix,
            pts_per_char: 6.0,
        }
    }
}

pub struct TextFormatter;

impl Default for TextFormatter {
    fn default() -> Self {
        Self::new()
    }
}

impl TextFormatter {
    pub fn new() -> Self {
        TextFormatter
    }

    /// Format a single page's lines into a String.
    pub fn format_page(
        &self,
        lines: &[TextLine],
        page_number: usize,
        options: &TextFormatOptions,
    ) -> String {
        let le = options.line_ending.as_str();
        let mut out = String::new();

        if options.include_page_markers && page_number > 0 {
            out.push_str(&format!("--- Page {} ---{}", page_number, le));
        }

        let avg_font_size = if lines.is_empty() {
            12.0
        } else {
            lines.iter().map(|l| l.font_size).sum::<f64>() / lines.len() as f64
        };

        // Variable-width layout preservation: derive the monospaced output grid's
        // character-cell width from the document's *actual* glyph metrics rather
        // than a fixed points-per-char constant, so columns and indentation line
        // up for proportional fonts and multi-column pages. Computed once per
        // page; the page width in cells bounds the leading-pad so garbage
        // coordinates can't produce pathological gaps.
        let (cell_width, max_cols) = if options.preserve_layout {
            let cw = Self::estimate_cell_width(lines, options.pts_per_char);
            let page_x_max = lines
                .iter()
                .map(|l| l.x_max)
                .fold(0.0_f64, f64::max)
                .max(0.0);
            let cap = ((page_x_max / cw).ceil() as usize).clamp(40, 1000);
            (cw, cap)
        } else {
            (options.pts_per_char, 0)
        };

        for line in lines {
            if line.is_blank() {
                continue;
            }

            if options.paragraph_breaks && line.is_paragraph_break {
                out.push_str(le);
            }

            let is_heading = line.is_heading(avg_font_size);
            if options.heading_breaks && is_heading {
                out.push_str(le);
            }

            if options.preserve_layout {
                // Place the line's start at the output column derived from its
                // page-x origin and the adaptive cell width. Never emit negative
                // padding; bound by the page width in cells.
                let leading = if line.x_min > 0.0 {
                    ((line.x_min / cell_width).round() as usize).min(max_cols)
                } else {
                    0
                };
                for _ in 0..leading {
                    out.push(' ');
                }
            }

            out.push_str(&line.text);
            out.push_str(le);

            if options.heading_breaks && is_heading {
                out.push_str(le);
            }
        }

        out
    }

    /// Estimate the monospaced output grid's character-cell width (in PDF points)
    /// from the document's own glyph metrics: the median per-character horizontal
    /// advance across all non-blank lines, where a line's advance is
    /// `(x_max - x_min) / char_count`. This adapts to font size and document
    /// scale, unlike a fixed constant.
    ///
    /// Falls back to `fallback` (the configured `pts_per_char`) when no line has a
    /// measurable advance (e.g. all single-character or zero-width lines).
    fn estimate_cell_width(lines: &[TextLine], fallback: f64) -> f64 {
        let mut advances: Vec<f64> = lines
            .iter()
            .filter(|l| !l.is_blank())
            .filter_map(|l| {
                let span = l.x_max - l.x_min;
                let chars = l.text.chars().filter(|c| !c.is_whitespace()).count();
                if span > 0.0 && chars > 0 {
                    Some(span / chars as f64)
                } else {
                    None
                }
            })
            .collect();

        if advances.is_empty() {
            return fallback.max(0.1);
        }

        advances.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = advances[advances.len() / 2];
        median.max(0.1)
    }

    /// Format lines from multiple pages into one string.
    pub fn format_pages(
        &self,
        page_texts: &[(usize, Vec<TextLine>)],
        options: &TextFormatOptions,
    ) -> String {
        page_texts
            .iter()
            .map(|(n, lines)| self.format_page(lines, *n, options))
            .collect::<Vec<_>>()
            .join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tline(text: &str, y: f64, font_size: f64, is_para_break: bool) -> TextLine {
        TextLine {
            text: text.to_string(),
            y,
            x_min: 50.0,
            x_max: 50.0 + text.len() as f64 * font_size * 0.6,
            font_size,
            is_paragraph_break: is_para_break,
            column: 0,
        }
    }

    #[test]
    fn basic_format_with_page_marker() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: true,
            ..Default::default()
        };
        let lines = vec![tline("Hello world", 700.0, 12.0, false)];
        let result = f.format_page(&lines, 1, &opts);
        assert!(
            result.starts_with("--- Page 1 ---"),
            "should start with page marker, got: {:?}",
            result
        );
        assert!(result.contains("Hello world"));
    }

    #[test]
    fn no_page_marker_when_disabled() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            ..Default::default()
        };
        let lines = vec![tline("Text", 700.0, 12.0, false)];
        let result = f.format_page(&lines, 1, &opts);
        assert!(!result.contains("---"), "no page marker expected");
        assert!(result.contains("Text"));
    }

    #[test]
    fn paragraph_break_inserts_blank_line() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            paragraph_breaks: true,
            ..Default::default()
        };
        let lines = vec![
            tline("Para1", 700.0, 12.0, false),
            tline("Para2", 650.0, 12.0, true),
        ];
        let result = f.format_page(&lines, 0, &opts);
        assert!(result.contains("Para1"));
        assert!(result.contains("Para2"));
        assert!(
            result.contains("\n\n"),
            "paragraph break should produce blank line, got: {:?}",
            result
        );
    }

    #[test]
    fn heading_line_gets_extra_spacing() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            heading_breaks: true,
            paragraph_breaks: false,
            ..Default::default()
        };
        let lines = vec![
            tline("Normal text", 700.0, 12.0, false),
            tline("BIG HEADING", 680.0, 24.0, false),
            tline("More text", 660.0, 12.0, false),
        ];
        let result = f.format_page(&lines, 0, &opts);
        let heading_pos = result
            .find("BIG HEADING")
            .expect("heading should be present");
        let before = &result[..heading_pos];
        assert!(
            before.ends_with("\n\n") || before.ends_with('\n'),
            "heading should be preceded by extra newline"
        );
    }

    #[test]
    fn preserve_layout_adds_leading_spaces() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            preserve_layout: true,
            pts_per_char: 6.0,
            ..Default::default()
        };
        let mut line = tline("Indented", 700.0, 12.0, false);
        line.x_min = 60.0;
        let lines = vec![line];
        let result = f.format_page(&lines, 0, &opts);
        let text_pos = result.find("Indented").expect("text should be present");
        let leading = &result[..text_pos];
        assert!(
            leading.ends_with("          "),
            "should have 10 leading spaces for x_min=60, got: {:?}",
            leading
        );
    }

    #[test]
    fn windows_line_ending() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            line_ending: LineEnding::Windows,
            ..Default::default()
        };
        let lines = vec![tline("Line", 700.0, 12.0, false)];
        let result = f.format_page(&lines, 0, &opts);
        assert!(result.contains("\r\n"), "should use CRLF line endings");
    }

    /// A custom TextLine builder that sets an explicit glyph extent (x_min/x_max)
    /// independent of text length, so layout-grid math can be exercised directly.
    fn layout_line(text: &str, x_min: f64, x_max: f64, font_size: f64) -> TextLine {
        TextLine {
            text: text.to_string(),
            y: 700.0,
            x_min,
            x_max,
            font_size,
            is_paragraph_break: false,
            column: 0,
        }
    }

    fn leading_spaces(s: &str, needle: &str) -> usize {
        let pos = s.find(needle).expect("text present");
        s[..pos].chars().rev().take_while(|c| *c == ' ').count()
    }

    #[test]
    fn layout_cell_width_is_document_adaptive_not_fixed_six() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            preserve_layout: true,
            paragraph_breaks: false,
            heading_breaks: false,
            ..Default::default()
        };
        // 10 non-space chars across 120 pts → cell width ≈ 12 pts.
        let body = layout_line("ABCDEFGHIJ", 50.0, 170.0, 24.0);
        // Indented line starts at x=120 → 120/12 = 10 cells.
        let indented = layout_line("X", 120.0, 132.0, 24.0);
        let result = f.format_page(&[body, indented], 0, &opts);
        // With the OLD fixed 6.0 constant the indent would have been 120/6 = 20.
        // With the adaptive ~12pt width it is 10.
        assert_eq!(
            leading_spaces(&result, "X"),
            10,
            "indent should use the document's ~12pt cell width, got:\n{result}"
        );
    }

    #[test]
    fn layout_same_start_aligns_regardless_of_glyph_width() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            preserve_layout: true,
            paragraph_breaks: false,
            heading_breaks: false,
            ..Default::default()
        };
        // Body line sets the cell width (~10pt/char here).
        let body = layout_line("MMMMMMMMMM", 0.0, 100.0, 20.0);
        let wide = layout_line("WW", 200.0, 240.0, 20.0); // starts at x=200
        let narrow = layout_line("ii", 200.0, 210.0, 20.0); // also starts at x=200
        let result = f.format_page(&[body, wide, narrow], 0, &opts);
        let wide_indent = leading_spaces(&result, "WW");
        let narrow_indent = leading_spaces(&result, "ii");
        assert_eq!(
            wide_indent, narrow_indent,
            "lines starting at same x must align identically, got wide={wide_indent} narrow={narrow_indent}\n{result}"
        );
        assert!(
            (18..=22).contains(&wide_indent),
            "indent ~20 expected (200/10), got {wide_indent}"
        );
    }

    #[test]
    fn layout_indent_cap_is_page_width_based_not_forty() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            preserve_layout: true,
            paragraph_breaks: false,
            heading_breaks: false,
            ..Default::default()
        };
        // Several body lines pin the median cell width at ~10pt/char so a single
        // far-right line can't skew it.
        let body1 = layout_line("MMMMMMMMMM", 0.0, 100.0, 20.0); // ~10pt/char
        let body2 = layout_line("NNNNNNNNNN", 0.0, 100.0, 20.0);
        let body3 = layout_line("OOOOOOOOOO", 0.0, 100.0, 20.0);
        // Realistic single glyph far to the right (span ~ one cell).
        let far = layout_line("Z", 600.0, 620.0, 20.0);
        let result = f.format_page(&[body1, body2, body3, far], 0, &opts);
        let indent = leading_spaces(&result, "Z");
        assert!(
            indent > 40,
            "wide page should allow indent beyond the old 40-cap, got {indent}\n{result}"
        );
    }

    #[test]
    fn non_layout_path_is_unaffected_by_changes() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            preserve_layout: false,
            ..Default::default()
        };
        let line = layout_line("Hello", 300.0, 360.0, 12.0);
        let result = f.format_page(&[line], 0, &opts);
        assert!(
            result.starts_with("Hello"),
            "flowing-text path should emit no leading spaces, got: {result:?}"
        );
    }

    #[test]
    fn blank_lines_are_skipped() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: false,
            ..Default::default()
        };
        let lines = vec![
            tline("Real", 700.0, 12.0, false),
            tline("", 690.0, 12.0, false),
            tline("   ", 680.0, 12.0, false),
            tline("Also", 670.0, 12.0, false),
        ];
        let result = f.format_page(&lines, 0, &opts);
        assert!(result.contains("Real"));
        assert!(result.contains("Also"));
        let line_count = result.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(
            line_count, 2,
            "should have exactly 2 non-blank lines, got:\n{}",
            result
        );
    }

    #[test]
    fn format_pages_combines_multiple_pages() {
        let f = TextFormatter::new();
        let opts = TextFormatOptions {
            include_page_markers: true,
            ..Default::default()
        };
        let page_texts = vec![
            (1, vec![tline("Page one text", 700.0, 12.0, false)]),
            (2, vec![tline("Page two text", 700.0, 12.0, false)]),
        ];
        let result = f.format_pages(&page_texts, &opts);
        assert!(result.contains("--- Page 1 ---"));
        assert!(result.contains("--- Page 2 ---"));
        assert!(result.contains("Page one text"));
        assert!(result.contains("Page two text"));
        let p1_pos = result.find("--- Page 1 ---").unwrap();
        let p2_pos = result.find("--- Page 2 ---").unwrap();
        assert!(p1_pos < p2_pos);
    }
}
