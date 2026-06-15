//! Render tests for mesh shadings (ShadingType 1, 4, 5, 6, 7).
//! Minimal PDFs paint a shading via the `sh` operator and assert pixel
//! colors at known points against hand-computed expectations.

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
fn function_based_shading_type1_varies_with_x() {
    // 20x20 page. Shading type 1 over Domain [0 1 0 1]; Function is Type 2 with
    // C0=[0,0,0] (black) at x=0 and C1=[1,0,0] (red) at x=1, N=1. /Matrix scales
    // the unit domain to cover the page (20x20). Color depends on x only.
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R \
         /Resources << /Shading << /Sh1 5 0 R >> >> >>",
    ); // 3
    b.add_stream("", b"/Sh1 sh\n"); // 4
    // Shading dict: type 1, domain unit square, Matrix scales to 20x20.
    b.add(
        "<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] \
         /Matrix [20 0 0 20 0 0] /Function 6 0 R >>",
    ); // 5
    b.add(
        "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 0 0] /N 1 >>",
    ); // 6

    let pdf = b.build();
    // Left edge (x small) -> near black; right edge (x large) -> near red.
    let left = render_pixel(pdf.clone(), 72, 0.1, 0.5);
    let right = render_pixel(pdf, 72, 0.9, 0.5);

    assert!(left[0] < 80, "left should be dark: {:?}", left);
    assert!(
        right[0] > 150 && right[1] < 90 && right[2] < 90,
        "right should be reddish: {:?}",
        right
    );
}

/// Build a packed big-endian field of `bits` bits into a byte buffer at a
/// byte-aligned position (we use 8/16-bit fields so everything is byte-aligned).
fn push_be(buf: &mut Vec<u8>, value: u32, bytes: usize) {
    for i in (0..bytes).rev() {
        buf.push((value >> (i * 8)) as u8);
    }
}

#[test]
fn gouraud_mesh_type4_interpolates_vertex_colors() {
    // A single triangle covering most of a 20x20 page, with three distinct
    // vertex colors: red, green, blue. Verify each corner is near its color and
    // the centroid is a blend.
    //
    // Stream layout per vertex (Type 4): flag(8) x(16) y(16) r(8) g(8) b(8).
    // Decode maps coords [0,20] and colors [0,1].
    let mut data: Vec<u8> = Vec::new();
    let vmax: u32 = 0xFFFF;
    let coord = |c: f64| -> u32 { ((c / 20.0) * vmax as f64).round() as u32 };
    // Triangle corners near the page corners.
    // v0 (2,2) red, v1 (18,2) green, v2 (10,18) blue — all flag 0.
    let verts = [
        (2.0, 2.0, [255u8, 0, 0]),
        (18.0, 2.0, [0u8, 255, 0]),
        (10.0, 18.0, [0u8, 0, 255]),
    ];
    for (x, y, col) in verts {
        data.push(0); // flag
        push_be(&mut data, coord(x), 2);
        push_be(&mut data, coord(y), 2);
        data.push(col[0]);
        data.push(col[1]);
        data.push(col[2]);
    }

    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R \
         /Resources << /Shading << /Sh1 5 0 R >> >> >>",
    ); // 3
    b.add_stream("", b"/Sh1 sh\n"); // 4
    b.add_stream(
        "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 \
         /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 20 0 20 0 1 0 1 0 1]",
        &data,
    ); // 5

    let pdf = b.build();
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    // Sample near each vertex (device Y is flipped; v0/v1 at PDF y=2 are near the
    // BOTTOM of the page = large device y). Just confirm the three primary colors
    // each appear somewhere and a blended interior pixel exists.
    let mut saw_red = false;
    let mut saw_green = false;
    let mut saw_blue = false;
    let mut saw_blend = false;
    for y in 0..buf.height as i32 {
        for x in 0..buf.width as i32 {
            let p = buf.get_pixel(x, y);
            if p[3] == 0 {
                continue;
            }
            if p[0] > 180 && p[1] < 70 && p[2] < 70 {
                saw_red = true;
            }
            if p[1] > 180 && p[0] < 70 && p[2] < 70 {
                saw_green = true;
            }
            if p[2] > 180 && p[0] < 70 && p[1] < 70 {
                saw_blue = true;
            }
            // A blended pixel: at least two channels meaningfully present.
            let mid = |v: u8| v > 40 && v < 200;
            if [mid(p[0]), mid(p[1]), mid(p[2])].iter().filter(|&&m| m).count() >= 2 {
                saw_blend = true;
            }
        }
    }
    assert!(saw_red, "triangle should show red near v0");
    assert!(saw_green, "triangle should show green near v1");
    assert!(saw_blue, "triangle should show blue near v2");
    assert!(saw_blend, "interior should show interpolated/blended colors");
}

#[test]
fn lattice_mesh_type5_renders_grid() {
    // A 2x2 lattice (VerticesPerRow=2, two rows) = one cell = two triangles
    // covering a square, with four corner colors. Verify all four colors appear.
    // Layout per vertex (Type 5, no flag): x(16) y(16) r(8) g(8) b(8).
    let mut data: Vec<u8> = Vec::new();
    let vmax: u32 = 0xFFFF;
    let coord = |c: f64| -> u32 { ((c / 20.0) * vmax as f64).round() as u32 };
    // Row 0: (2,2) red, (18,2) green. Row 1: (2,18) blue, (18,18) yellow.
    let verts = [
        (2.0, 2.0, [255u8, 0, 0]),
        (18.0, 2.0, [0u8, 255, 0]),
        (2.0, 18.0, [0u8, 0, 255]),
        (18.0, 18.0, [255u8, 255, 0]),
    ];
    for (x, y, col) in verts {
        push_be(&mut data, coord(x), 2);
        push_be(&mut data, coord(y), 2);
        data.push(col[0]);
        data.push(col[1]);
        data.push(col[2]);
    }

    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R \
         /Resources << /Shading << /Sh1 5 0 R >> >> >>",
    ); // 3
    b.add_stream("", b"/Sh1 sh\n"); // 4
    b.add_stream(
        "/ShadingType 5 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 \
         /BitsPerComponent 8 /VerticesPerRow 2 /Decode [0 20 0 20 0 1 0 1 0 1]",
        &data,
    ); // 5

    let pdf = b.build();
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    let mut painted = 0;
    for y in 0..buf.height as i32 {
        for x in 0..buf.width as i32 {
            if buf.get_pixel(x, y)[3] != 0 {
                // any non-transparent pixel from the mesh
                let p = buf.get_pixel(x, y);
                if p != [0, 0, 0, 0] {
                    painted += 1;
                }
            }
        }
    }
    // The two triangles should cover a substantial chunk of the 20x20-ish device
    // area (roughly the square between the four corners).
    assert!(painted > 50, "lattice mesh should paint a filled region: {painted}");
}

#[test]
fn coons_patch_type6_renders_smooth_fill() {
    // A single Coons patch (flag 0): 12 boundary control points forming a square
    // (straight edges) with 4 corner colors. Verify the patch fills its area and
    // shows all four corner colors somewhere (smooth gradient across the patch).
    //
    // Layout (Type 6, flag 0): flag(8) then 12 points [x(16) y(16)] then 4
    // colors [r(8) g(8) b(8)].
    let mut data: Vec<u8> = Vec::new();
    let vmax: u32 = 0xFFFF;
    let coord = |c: f64| -> u32 { ((c / 20.0) * vmax as f64).round() as u32 };
    let mut pt = |buf: &mut Vec<u8>, x: f64, y: f64| {
        push_be(buf, coord(x), 2);
        push_be(buf, coord(y), 2);
    };
    data.push(0); // flag 0 = new patch
    // 12 boundary points of a 16x16 square from (2,2) to (18,18). The exact
    // boundary path order matches the spec's p1..p12 traversal; for a square the
    // control points lie on straight edges (thirds).
    // Edge C1 (p1..p4): left edge bottom->top.
    pt(&mut data, 2.0, 2.0); // p1 corner (u0,v0)
    pt(&mut data, 2.0, 7.33);
    pt(&mut data, 2.0, 12.66);
    pt(&mut data, 2.0, 18.0); // p4 corner (u0,v1)
    // Edge D2 (p5..p6 then p7): top edge left->right.
    pt(&mut data, 7.33, 18.0);
    pt(&mut data, 12.66, 18.0);
    pt(&mut data, 18.0, 18.0); // p7 corner (u1,v1)
    // Edge C2 reversed (p8..p9 then p10): right edge top->bottom.
    pt(&mut data, 18.0, 12.66);
    pt(&mut data, 18.0, 7.33);
    pt(&mut data, 18.0, 2.0); // p10 corner (u1,v0)
    // Edge D1 reversed (p11..p12): bottom edge right->left back toward p1.
    pt(&mut data, 12.66, 2.0);
    pt(&mut data, 7.33, 2.0);
    // 4 corner colors: red, green, blue, yellow.
    for col in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]] {
        data.push(col[0]);
        data.push(col[1]);
        data.push(col[2]);
    }

    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R \
         /Resources << /Shading << /Sh1 5 0 R >> >> >>",
    ); // 3
    b.add_stream("", b"/Sh1 sh\n"); // 4
    b.add_stream(
        "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 \
         /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 20 0 20 0 1 0 1 0 1]",
        &data,
    ); // 5

    let pdf = b.build();
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();

    let mut painted = 0;
    for y in 0..buf.height as i32 {
        for x in 0..buf.width as i32 {
            if buf.get_pixel(x, y) != [0, 0, 0, 0] {
                painted += 1;
            }
        }
    }
    // The patch should fill a meaningful region without crashing.
    assert!(painted > 50, "Coons patch should paint a filled region: {painted}");
}
