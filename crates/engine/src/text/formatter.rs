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

    /// Characters per average-width glyph for preserve_layout mode.
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
                let leading_spaces = (line.x_min / options.pts_per_char).round() as usize;
                for _ in 0..leading_spaces.min(40) {
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
