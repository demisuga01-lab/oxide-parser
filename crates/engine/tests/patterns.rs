//! Render tests for tiling patterns (PatternType 1).
//!
//! Each test builds a minimal single-page PDF with a tiling pattern resource,
//! fills a rectangle with it, renders, and asserts pixel colors at points that
//! fall in different tile phases.

use oxide_engine::ContentEngine;

/// Minimal numbered-object PDF builder with correct xref offsets.
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

fn render_pixel(pdf: Vec<u8>, dpi: u32, fx: f64, fy: f64) -> [u8; 4] {
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, dpi).unwrap();
    let x = ((buf.width as f64) * fx) as i32;
    let y = ((buf.height as f64) * fy) as i32;
    buf.get_pixel(
        x.clamp(0, buf.width as i32 - 1),
        y.clamp(0, buf.height as i32 - 1),
    )
}

#[test]
fn colored_tiling_pattern_fills_with_tile_content() {
    // A 50x50 page. Tile is 10x10: paints a 10x5 red bar in the bottom half of
    // each cell, leaving the top half white (page background). With XStep=YStep=
    // 10, the pattern repeats; rows of red bars alternate with white gaps.
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 50] /Contents 4 0 R \
         /Resources << /Pattern << /P1 5 0 R >> >> >>",
    ); // 3
    // Page content: white background, then set Pattern fill color space and fill
    // the whole page with the pattern.
    let content = b"1 1 1 rg 0 0 50 50 re f\n/Pattern cs /P1 scn 0 0 50 50 re f\n";
    b.add_stream("", content); // 4
    // Tiling pattern (colored, PaintType 1): a 10x10 tile with a red bar in
    // y=[0,5).
    let tile_content = b"1 0 0 rg 0 0 10 5 re f\n";
    b.add_stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
         /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
        tile_content,
    ); // 5

    let pdf = b.build();
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    // Count red vs white pixels to confirm the pattern actually tiled (roughly
    // half red bars / half white given the 10x5 bar in a 10x10 cell).
    let mut red = 0;
    let mut white = 0;
    for y in 0..buf.height as i32 {
        for x in 0..buf.width as i32 {
            let p = buf.get_pixel(x, y);
            if p[0] > 180 && p[1] < 80 && p[2] < 80 {
                red += 1;
            } else if p[0] > 200 && p[1] > 200 && p[2] > 200 {
                white += 1;
            }
        }
    }
    assert!(red > 0, "pattern should paint red bars; red={red}");
    assert!(white > 0, "gaps between bars should stay white; white={white}");
    // Neither should dominate completely (confirms genuine tiling, not a solid
    // fill of one color).
    let total = (buf.width * buf.height) as i32;
    assert!(red > total / 10, "too few red px: {red}/{total}");
    assert!(white > total / 10, "too few white px: {white}/{total}");
}

#[test]
fn uncolored_pattern_uses_fill_color_at_point_of_use() {
    // PaintType 2 (uncolored): the tile content omits color operators. Two
    // rectangles use the SAME pattern but with different scn colors; each must
    // render in its own color.
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 20] /Contents 4 0 R \
         /Resources << /Pattern << /P1 5 0 R >> >> >>",
    ); // 3
    // Left rect [0,0,20,20] in green pattern; right rect [20,0,20,20] in blue.
    // Pattern color space is [/Pattern /DeviceRGB] so scn takes color + name.
    let content = b"1 1 1 rg 0 0 40 20 re f\n\
                    /Pattern cs\n\
                    0 1 0 /P1 scn 0 0 20 20 re f\n\
                    0 0 1 /P1 scn 20 0 20 20 re f\n";
    b.add_stream("", content); // 4
    // Uncolored tile: fills its whole 10x10 BBox WITHOUT specifying a color.
    let tile_content = b"0 0 10 10 re f\n";
    b.add_stream(
        "/Type /Pattern /PatternType 1 /PaintType 2 /TilingType 1 \
         /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
        tile_content,
    ); // 5

    let pdf = b.build();
    // Left half should be green, right half blue.
    let left = render_pixel(pdf.clone(), 72, 0.25, 0.5);
    let right = render_pixel(pdf, 72, 0.75, 0.5);

    assert!(
        left[1] > 150 && left[0] < 100 && left[2] < 100,
        "left should be green: {:?}",
        left
    );
    assert!(
        right[2] > 150 && right[0] < 100 && right[1] < 100,
        "right should be blue: {:?}",
        right
    );
}

#[test]
fn pattern_step_larger_than_bbox_leaves_gaps() {
    // BBox 10x10 but XStep/YStep 20 -> tiles every 20 units, leaving 10-unit
    // gaps that stay the backdrop color (white).
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 40] /Contents 4 0 R \
         /Resources << /Pattern << /P1 5 0 R >> >> >>",
    ); // 3
    let content = b"1 1 1 rg 0 0 40 40 re f\n/Pattern cs /P1 scn 0 0 40 40 re f\n";
    b.add_stream("", content); // 4
    // Tile fully fills its 10x10 BBox red, but step is 20 -> gaps.
    let tile_content = b"1 0 0 rg 0 0 10 10 re f\n";
    b.add_stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
         /BBox [0 0 10 10] /XStep 20 /YStep 20 /Resources << >>",
        tile_content,
    ); // 5

    let pdf = b.build();
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    let mut red = 0;
    let mut white = 0;
    for y in 0..buf.height as i32 {
        for x in 0..buf.width as i32 {
            let p = buf.get_pixel(x, y);
            if p[0] > 180 && p[1] < 80 && p[2] < 80 {
                red += 1;
            } else if p[0] > 200 && p[1] > 200 && p[2] > 200 {
                white += 1;
            }
        }
    }
    // With 10x10 tiles on a 20-grid, ~1/4 of the area is red, ~3/4 white.
    assert!(red > 0, "tiles should paint red: {red}");
    assert!(white > red, "gaps (white) should exceed tile area (red): white={white} red={red}");
}
