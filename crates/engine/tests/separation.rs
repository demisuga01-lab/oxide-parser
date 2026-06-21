//! Render tests for Separation / DeviceN colour spaces.
//!
//! Each test builds a minimal single-page PDF that sets a Separation/DeviceN
//! fill colour space, fills a rectangle, renders, and asserts the painted pixel
//! colour matches the tint-transform → alternate-space → RGB result.

use oxide_engine::ContentEngine;

struct PdfBuilder {
    objects: Vec<Vec<u8>>,
}

impl PdfBuilder {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
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

fn render_center_pixel(pdf: Vec<u8>) -> [u8; 4] {
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, 72).unwrap();
    buf.get_pixel(buf.width as i32 / 2, buf.height as i32 / 2)
}

#[test]
fn separation_fill_resolves_through_cmyk_tint_transform() {
    // /Separation "Spot" -> DeviceCMYK with a Type 2 tint transform: tint 0 ->
    // CMYK(0,0,0,0)=white, tint 1 -> CMYK(0,1,1,0). Fill the whole page with
    // tint 1: expected red via the shared Poppler-like DeviceCMYK fallback.
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 40] /Contents 4 0 R \
         /Resources << /ColorSpace << /CS0 5 0 R >> >> >>",
    ); // 3
       // Set Separation fill space, tint 1, fill the page.
    b.add_stream("", b"/CS0 cs 1 scn 0 0 40 40 re f\n"); // 4
                                                         // /Separation /Spot /DeviceCMYK <fn 6 0 R>
    b.add("[/Separation /Spot /DeviceCMYK 6 0 R]"); // 5
    b.add("<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 1 0] /N 1 >>"); // 6

    let px = render_center_pixel(b.build());
    assert!(px[0] > 200, "Separation tint 1 -> red R: {:?}", px);
    assert!(px[1] < 60, "G low: {:?}", px);
    assert!(px[2] < 60, "B low: {:?}", px);
}

#[test]
fn separation_none_paints_nothing() {
    // /Separation /None must produce no marks: the page stays white background.
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 40] /Contents 4 0 R \
         /Resources << /ColorSpace << /CS0 5 0 R >> >> >>",
    ); // 3
    b.add_stream("", b"/CS0 cs 1 scn 0 0 40 40 re f\n"); // 4
    b.add("[/Separation /None /DeviceCMYK 6 0 R]"); // 5
    b.add("<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>"); // 6

    let px = render_center_pixel(b.build());
    // White background preserved (no black/colored fill).
    assert_eq!(
        px,
        [255, 255, 255, 255],
        "None must leave background: {:?}",
        px
    );
}

#[test]
fn device_n_two_components_resolve_to_rgb() {
    // /DeviceN [/A /B] -> DeviceRGB with a Type 4 transform mapping [a b] ->
    // [a, b, 0]. Fill with components (1, 0): expected RGB ~ (255, 0, 0).
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>"); // 1
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"); // 2
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 40] /Contents 4 0 R \
         /Resources << /ColorSpace << /CS0 5 0 R >> >> >>",
    ); // 3
    b.add_stream("", b"/CS0 cs 1 0 scn 0 0 40 40 re f\n"); // 4
    b.add("[/DeviceN [/A /B] /DeviceRGB 6 0 R]"); // 5
                                                  // PostScript: stack starts [a b]; "0" pushes -> [a b 0]; outputs are top 3.
    b.add_stream(
        "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
        b"{ 0 }",
    ); // 6

    let px = render_center_pixel(b.build());
    assert!(px[0] > 200, "DeviceN A=1 -> R high: {:?}", px);
    assert!(px[1] < 60, "B=0 -> G low: {:?}", px);
    assert!(px[2] < 60, "third comp 0 -> B low: {:?}", px);
}
