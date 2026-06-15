//! Render tests for symbolic-font fallback (Symbol / ZapfDingbats).
//!
//! These build a minimal PDF that uses a non-embedded `/Symbol` or
//! `/ZapfDingbats` Type 1 font and draw some text, then assert that the page
//! actually has painted (non-background) pixels where previously nothing
//! rendered. Exact glyph shapes need not match Poppler (a substitute font is
//! used); the win is *presence* of content.

use oxide_engine::ContentEngine;

struct PdfBuilder {
    objects: Vec<Vec<u8>>,
}

impl PdfBuilder {
    fn new() -> Self {
        Self { objects: Vec::new() }
    }
    fn add(&mut self, body: &str) -> usize {
        self.objects.push(body.as_bytes().to_vec());
        self.objects.len()
    }
    fn add_stream(&mut self, dict_extra: &str, stream: &[u8]) -> usize {
        let mut body =
            format!("<< /Length {} {} >>\nstream\n", stream.len(), dict_extra).into_bytes();
        body.extend_from_slice(stream);
        body.extend_from_slice(b"\nendstream");
        self.objects.push(body);
        self.objects.len()
    }
    fn build(&self) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            pdf.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                offsets.len() + 1,
                xref_start
            )
            .as_bytes(),
        );
        pdf
    }
}

/// Count non-white painted pixels in the rendered page.
fn painted_pixel_count(pdf: Vec<u8>) -> usize {
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 150).unwrap();
    let mut count = 0;
    for y in 0..buf.height as i32 {
        for x in 0..buf.width as i32 {
            let p = buf.get_pixel(x, y);
            // background is opaque white; anything darker is painted content.
            if p[3] != 0 && (p[0] < 250 || p[1] < 250 || p[2] < 250) {
                count += 1;
            }
        }
    }
    count
}

fn symbolic_font_pdf(base_font: &str, text_hex: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 80] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>",
    ); // 3
    // Draw the text at a large size near the top-left.
    let content = format!("BT /F1 48 Tf 10 20 Td <{text_hex}> Tj ET\n");
    b.add_stream("", content.as_bytes()); // 4
    // A non-embedded Type 1 symbolic font (no FontDescriptor / FontFile).
    b.add(&format!(
        "<< /Type /Font /Subtype /Type1 /BaseFont /{base_font} >>"
    )); // 5
    b.build()
}

#[test]
fn symbol_font_renders_non_blank_glyphs() {
    // Symbol codes: 0x61='a'->alpha, 0x62='b'->beta, 0x53='S'->Sigma.
    let painted = painted_pixel_count(symbolic_font_pdf("Symbol", "616253"));
    assert!(
        painted > 200,
        "Symbol text should render visible glyphs, painted={painted}"
    );
}

#[test]
fn zapf_dingbats_font_renders_non_blank_glyphs() {
    // ZapfDingbats codes: 0x21->a1, 0x22->a2, 0x6C->a71 (black circle).
    let painted = painted_pixel_count(symbolic_font_pdf("ZapfDingbats", "21226C"));
    assert!(
        painted > 200,
        "ZapfDingbats text should render visible glyphs, painted={painted}"
    );
}

#[test]
fn symbol_font_with_subset_prefix_still_renders() {
    let painted = painted_pixel_count(symbolic_font_pdf("ABCDEF+Symbol", "616263"));
    assert!(
        painted > 200,
        "subset-prefixed Symbol should still render, painted={painted}"
    );
}
