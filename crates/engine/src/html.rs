//! HTML / XML output (`pdftohtml`-equivalent).
//!
//! Pure **assembly** of capabilities prior rounds already built — the text
//! pipeline (positioned, BiDi-correct fragments), the raster renderer, and the
//! image encoder — into HTML. No new parsing or rendering.
//!
//! # Modes
//!
//! - **Complex** (default, the high-value mode): a per-page container sized to
//!   the page, with absolutely-positioned text laid out from the reading-order
//!   pipeline's [`TextLine`]s (so reading order and BiDi are already correct).
//!   With `background = true` the page is also rendered to a PNG and placed
//!   behind the text — the "raster background + selectable text overlay"
//!   approach. This is the **highest-fidelity** option: the background reproduces
//!   *every* graphic, image, shading, and vector mark in its exact position
//!   (it IS the raster render), while the overlaid text stays selectable and
//!   correctly positioned. It also sidesteps the fact that per-image device
//!   placement isn't separately exposed by the engine — the rendered page
//!   already contains the images where they belong.
//! - **Simple** (flowing): the existing reading-order text wrapped in minimal
//!   paragraphs — readable, low-fidelity, no positioning.
//! - **XML**: each text fragment with its position/size, à la `pdftohtml -xml`.
//!
//! Text is HTML-escaped and the output declares UTF-8. RTL lines carry
//! `dir="rtl"` so they render in the right visual direction.

use serde::Serialize;

use crate::engine::ContentEngine;
use crate::error::Result;
use crate::text::{TextExtractOptions, TextExtractor, TextLine};

/// Which flavour of HTML/XML to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HtmlMode {
    /// Absolutely-positioned text (the default `pdftohtml -c` analogue).
    Complex,
    /// Flowing paragraphs (readable, no positioning).
    Simple,
    /// `<page>`/`<text>` fragments with positions (`pdftohtml -xml`).
    Xml,
}

/// Options for HTML/XML export.
#[derive(Debug, Clone)]
pub struct HtmlOptions {
    pub mode: HtmlMode,
    /// Complex mode only: render the page to a PNG and place it behind the text
    /// for full graphics fidelity (the raster-background + text-overlay mode).
    pub background: bool,
    /// DPI used for the raster background (complex + background only).
    pub background_dpi: u32,
    /// Points→pixels scale for positioning (CSS px per PDF point). 96/72 ≈ 1.333
    /// matches a CSS reference pixel; default keeps text at natural size.
    pub scale: f64,
    /// When `background` is on, make the overlaid text invisible (transparent
    /// fill) so only the raster shows but the text stays selectable/searchable —
    /// like a scanned-PDF OCR layer. Off by default (text is visible).
    pub invisible_text_over_background: bool,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            mode: HtmlMode::Complex,
            background: false,
            background_dpi: 150,
            scale: 96.0 / 72.0,
            invisible_text_over_background: false,
        }
    }
}

/// Assembles HTML/XML from the engine's text + raster capabilities.
pub struct HtmlExporter;

impl HtmlExporter {
    /// Produce a complete, self-contained HTML (or XML) document for the given
    /// 1-based pages.
    pub fn export(
        engine: &ContentEngine,
        pages: &[usize],
        options: &HtmlOptions,
    ) -> Result<String> {
        match options.mode {
            HtmlMode::Xml => Self::export_xml(engine, pages),
            HtmlMode::Simple => Self::export_simple(engine, pages),
            HtmlMode::Complex => Self::export_complex(engine, pages, options),
        }
    }

    fn page_lines(engine: &ContentEngine, page: usize) -> Result<Vec<TextLine>> {
        let extractor = TextExtractor::new();
        let opts = TextExtractOptions::default();
        let (_n, lines) = extractor.extract_page(engine, page, &opts)?;
        Ok(lines)
    }

    fn export_complex(
        engine: &ContentEngine,
        pages: &[usize],
        options: &HtmlOptions,
    ) -> Result<String> {
        let mut out = String::new();
        out.push_str(HTML_HEADER);
        out.push_str("<style>\n");
        out.push_str(COMPLEX_CSS);
        out.push_str("</style>\n</head>\n<body>\n");

        for &page in pages {
            let (w_pt, h_pt, y0) = Self::page_geom(engine, page)?;
            let scale = options.scale;
            let w_px = w_pt * scale;
            let h_px = h_pt * scale;

            out.push_str(&format!(
                "<div class=\"page\" id=\"page{page}\" style=\"width:{w_px:.0}px;height:{h_px:.0}px;\">\n"
            ));

            // Optional raster background.
            if options.background {
                if let Ok(png) = engine.render_page_png_fast(page, options.background_dpi) {
                    let b64 = base64_encode(&png);
                    out.push_str(&format!(
                        "<img class=\"bg\" src=\"data:image/png;base64,{b64}\" \
                         style=\"width:{w_px:.0}px;height:{h_px:.0}px;\" alt=\"\"/>\n"
                    ));
                }
            }

            // Positioned text lines (reading-order + BiDi already applied).
            let lines = Self::page_lines(engine, page)?;
            for line in &lines {
                if line.is_blank() {
                    continue;
                }
                // PDF y is bottom-left origin and is the baseline; flip to a
                // top-left CSS top, lifting by ~the cap height so the glyph box
                // aligns. Use the line's font size as the baseline-to-top offset.
                let top_pt = h_pt - (line.y - y0) - line.font_size;
                let left_px = (line.x_min - 0.0) * scale;
                let top_px = top_pt * scale;
                let size_px = line.font_size * scale;
                let dir = if is_rtl_line(&line.text) {
                    " dir=\"rtl\""
                } else {
                    ""
                };
                let color = if options.background && options.invisible_text_over_background {
                    "transparent"
                } else {
                    "#000"
                };
                out.push_str(&format!(
                    "<div class=\"t\"{dir} style=\"left:{left_px:.2}px;top:{top_px:.2}px;\
                     font-size:{size_px:.2}px;color:{color};\">{}</div>\n",
                    escape_html(&line.text)
                ));
            }

            out.push_str("</div>\n");
        }

        out.push_str("</body>\n</html>\n");
        Ok(out)
    }

    fn export_simple(engine: &ContentEngine, pages: &[usize]) -> Result<String> {
        let mut out = String::new();
        out.push_str(HTML_HEADER);
        out.push_str("</head>\n<body>\n");
        for &page in pages {
            out.push_str(&format!("<div class=\"page\" id=\"page{page}\">\n"));
            let text = engine.get_page_text(page).unwrap_or_default();
            for para in text.split('\n') {
                let para = para.trim_end();
                if para.is_empty() {
                    out.push_str("<br/>\n");
                } else {
                    out.push_str(&format!("<p>{}</p>\n", escape_html(para)));
                }
            }
            out.push_str("</div>\n");
        }
        out.push_str("</body>\n</html>\n");
        Ok(out)
    }

    fn export_xml(engine: &ContentEngine, pages: &[usize]) -> Result<String> {
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str("<pdf2xml>\n");
        for &page in pages {
            let (w_pt, h_pt, y0) = Self::page_geom(engine, page)?;
            out.push_str(&format!(
                "<page number=\"{page}\" width=\"{w_pt:.2}\" height=\"{h_pt:.2}\">\n"
            ));
            let lines = Self::page_lines(engine, page)?;
            for line in &lines {
                if line.is_blank() {
                    continue;
                }
                // Top-left-origin coordinates, matching pdftohtml -xml.
                let top = h_pt - (line.y - y0) - line.font_size;
                let width = (line.x_max - line.x_min).max(0.0);
                out.push_str(&format!(
                    "<text top=\"{top:.2}\" left=\"{:.2}\" width=\"{width:.2}\" \
                     height=\"{:.2}\" font-size=\"{:.2}\">{}</text>\n",
                    line.x_min,
                    line.font_size,
                    line.font_size,
                    escape_xml(&line.text)
                ));
            }
            out.push_str("</page>\n");
        }
        out.push_str("</pdf2xml>\n");
        Ok(out)
    }

    /// (width_pt, height_pt, y0) for a page — y0 is the MediaBox lower y, used
    /// to normalize the bottom-left origin.
    fn page_geom(engine: &ContentEngine, page: usize) -> Result<(f64, f64, f64)> {
        let p = engine.get_page(page)?;
        let w = (p.media_box[2] - p.media_box[0]).abs();
        let h = (p.media_box[3] - p.media_box[1]).abs();
        let y0 = p.media_box[1];
        Ok((w, h, y0))
    }
}

const HTML_HEADER: &str = "<!DOCTYPE html>\n<html>\n<head>\n\
    <meta charset=\"UTF-8\"/>\n\
    <meta name=\"generator\" content=\"oxide\"/>\n";

const COMPLEX_CSS: &str = "body{margin:0;background:#888;}\n\
    .page{position:relative;background:#fff;margin:8px auto;overflow:hidden;}\n\
    .page .bg{position:absolute;left:0;top:0;}\n\
    .t{position:absolute;white-space:pre;font-family:sans-serif;line-height:1;}\n";

/// True when a line's text is dominantly right-to-left (Arabic/Hebrew/…).
fn is_rtl_line(text: &str) -> bool {
    let mut rtl = 0usize;
    let mut total = 0usize;
    for c in text.chars() {
        if c.is_alphabetic() {
            total += 1;
            let cp = c as u32;
            if matches!(cp,
                0x0590..=0x05FF | 0x0600..=0x06FF | 0x0700..=0x074F | 0x0750..=0x077F
                | 0x08A0..=0x08FF | 0xFB1D..=0xFB4F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF)
            {
                rtl += 1;
            }
        }
    }
    total > 0 && rtl * 2 > total
}

/// Escape the five HTML-significant characters.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape XML text content (no need to escape quotes/apostrophes in element text).
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Standard base64 (shared shape with the SVG sink's encoder).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_specials() {
        assert_eq!(
            escape_html("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &#39; f"
        );
    }

    #[test]
    fn escapes_xml_text() {
        assert_eq!(escape_xml("x<y>&z"), "x&lt;y&gt;&amp;z");
    }

    #[test]
    fn unicode_passes_through_escaping() {
        assert_eq!(
            escape_html("café — 中文 — العربية"),
            "café — 中文 — العربية"
        );
    }

    #[test]
    fn detects_rtl_lines() {
        assert!(is_rtl_line("مرحبا بالعالم"));
        assert!(is_rtl_line("שלום עולם"));
        assert!(!is_rtl_line("Hello world"));
        assert!(!is_rtl_line("123 456"));
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }
}
