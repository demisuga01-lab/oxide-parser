//! Render-fidelity tests for transparency groups, ExtGState soft masks, and
//! inline image painting.
//!
//! Each test builds a minimal single-page PDF in memory (computing xref offsets
//! so the reader parses it), renders it at a known DPI, and asserts the pixel
//! color at a known point matches a hand-computed expected blend.

use oxide_engine::ContentEngine;

/// A tiny PDF builder that appends numbered objects and writes a valid xref.
struct PdfBuilder {
    objects: Vec<Vec<u8>>,
}

impl PdfBuilder {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    /// Add an object body (without the `N 0 obj`/`endobj` wrapper) and return
    /// its object number (1-based).
    fn add(&mut self, body: &str) -> usize {
        self.objects.push(body.as_bytes().to_vec());
        self.objects.len()
    }

    /// Add a stream object (dict + stream bytes), return its object number.
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

/// Render a single-page PDF at a low DPI and return (buffer width/height,
/// sampled pixel at the buffer center).
fn render_center(pdf: Vec<u8>, dpi: u32) -> [u8; 4] {
    let engine = ContentEngine::open_bytes(pdf).unwrap();
    let buf = engine.render_page(1, dpi).unwrap();
    buf.get_pixel(buf.width as i32 / 2, buf.height as i32 / 2)
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

/// Standard 3-object prologue (catalog, pages, page) referencing the given page
/// dictionary entries and content stream object number. Returns a builder
/// pre-loaded so the caller can add the content + extra objects.
fn page_pdf(content: &[u8], extra_objects: &[(&str,)], page_extra: &str) -> Vec<u8> {
    // We need the content stream object number to be stable. Layout:
    //   1 catalog, 2 pages, 3 page, 4 content, 5.. extra
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(&format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R {} >>",
        page_extra
    ));
    b.add_stream("", content);
    for (body,) in extra_objects {
        b.add(body);
    }
    b.build()
}

#[test]
fn semi_transparent_rect_over_red_is_blended() {
    // Page: fill whole page red, then fill a 50%-alpha blue rect over the
    // center. Expected center color: blue at 0.5 over red = (127, 0, 127)-ish.
    let content = b"1 0 0 rg 0 0 100 100 re f\n\
                    /GS1 gs 0 0 1 rg 20 20 60 60 re f\n";
    let page_extra = "/Resources << /ExtGState << /GS1 5 0 R >> >>";
    let extra = [("<< /Type /ExtGState /ca 0.5 >>",)];
    let pdf = page_pdf(content, &extra, page_extra);

    let p = render_center(pdf, 72);
    // 50% blue over red, composited in sRGB space (matches Poppler/Splash): the
    // R and B channels mix 50% of 0 with 50% of 255 directly -> ~127 (the sRGB
    // midpoint). G stays ~0. The result is a purple blend of blue over red.
    assert!((p[0] as i32 - 127).abs() < 30, "R={} expected ~127", p[0]);
    assert!(p[1] < 40, "G={} expected ~0", p[1]);
    assert!((p[2] as i32 - 127).abs() < 30, "B={} expected ~127", p[2]);
}

#[test]
fn non_isolated_group_blends_with_page_backdrop() {
    // A non-isolated transparency group (/I false) containing a 50%-alpha blue
    // rect, drawn over a red page. Because it is non-isolated and uses Normal
    // blending at ca=1 for the group itself, the result equals the same blue
    // rect blended directly: ~ (127, 0, 127).
    let content = b"1 0 0 rg 0 0 100 100 re f\n/Fm1 Do\n";
    let page_extra = "/Resources << /XObject << /Fm1 5 0 R >> >>";
    let form = "<< /Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 100 100] \
        /Group << /Type /Group /S /Transparency /I false >> \
        /Resources << /ExtGState << /GS1 6 0 R >> >> /Length 44 >>\n\
        stream\n/GS1 gs 0 0 1 rg 20 20 60 60 re f\nendstream";
    let gs = "<< /Type /ExtGState /ca 0.5 >>";
    let extra = [(form,), (gs,)];
    let pdf = page_pdf(content, &extra, page_extra);

    let p = render_center(pdf, 72);
    // Same sRGB-space blend as above: R ~127 (sRGB midpoint), substantial blue.
    assert!((p[0] as i32 - 127).abs() < 40, "R={} expected ~127", p[0]);
    assert!(p[2] as i32 > 80, "B={} should be substantial", p[2]);
}

#[test]
fn isolated_group_differs_from_non_isolated_with_multiply() {
    // An isolated group with Multiply blend mode, containing a blue rect, over a
    // red page. Isolated means the group's interior composites against a
    // transparent backdrop, so the group result is opaque blue, then the whole
    // group multiplies onto red: red*blue per channel = (1*0, 0*0, 0*1) = black.
    // (Multiply of pure blue over pure red is black.) This visibly differs from
    // a non-isolated multiply.
    let content = b"1 0 0 rg 0 0 100 100 re f\n/GS1 gs /Fm1 Do\n";
    let page_extra = "/Resources << /XObject << /Fm1 5 0 R >> /ExtGState << /GS1 6 0 R >> >>";
    let form = "<< /Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 100 100] \
        /Group << /Type /Group /S /Transparency /I true >> \
        /Length 28 >>\n\
        stream\n0 0 1 rg 0 0 100 100 re f\nendstream";
    let gs = "<< /Type /ExtGState /BM /Multiply >>";
    let extra = [(form,), (gs,)];
    let pdf = page_pdf(content, &extra, page_extra);

    let p = render_center(pdf, 72);
    // Multiply(red, blue) = black-ish.
    assert!(p[0] < 60, "R={} expected dark (multiply)", p[0]);
    assert!(p[1] < 60, "G={} expected dark", p[1]);
    assert!(p[2] < 60, "B={} expected dark (multiply)", p[2]);
}

fn group_with_interior_screen_pdf(isolated: bool) -> Vec<u8> {
    let content = b"1 0 0 rg 0 0 100 100 re f\n/Fm1 Do\n";
    let page_extra = "/Resources << /XObject << /Fm1 5 0 R >> >>";
    let group_isolated = if isolated { "true" } else { "false" };
    let form_stream = "/GS1 gs 0 0 1 rg 0 0 100 100 re f\n";
    let form = format!(
        "<< /Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 100 100] \
         /Group << /Type /Group /S /Transparency /I {} >> \
         /Resources << /ExtGState << /GS1 6 0 R >> >> /Length {} >>\n\
         stream\n{}endstream",
        group_isolated,
        form_stream.len(),
        form_stream
    );
    let gs = "<< /Type /ExtGState /BM /Screen >>";
    let extra = [(form.as_str(),), (gs,)];
    page_pdf(content, &extra, page_extra)
}

#[test]
fn interior_blend_uses_group_backdrop_for_isolated_vs_non_isolated() {
    let isolated = render_center(group_with_interior_screen_pdf(true), 72);
    assert!(
        isolated[2] > 230 && isolated[0] < 40 && isolated[1] < 40,
        "isolated group Screen object should blend against transparent group backdrop, got {:?}",
        isolated
    );

    let non_isolated = render_center(group_with_interior_screen_pdf(false), 72);
    assert!(
        non_isolated[0] > 230 && non_isolated[1] < 40 && non_isolated[2] > 230,
        "non-isolated group Screen object should blend against red page backdrop, got {:?}",
        non_isolated
    );
}

#[test]
fn luminosity_soft_mask_reveals_and_hides() {
    // A luminosity soft mask whose group paints a white rect on the LEFT half
    // (mask=1, content visible) and leaves the RIGHT half black (mask=0, content
    // hidden). Under the mask we fill the whole page black over a white page.
    // Left half should become black; right half should stay white.
    let content = b"1 1 1 rg 0 0 100 100 re f\n\
                    /GS1 gs 0 0 0 rg 0 0 100 100 re f\n";
    let page_extra = "/Resources << /ExtGState << /GS1 5 0 R >> >>";
    let smask_gs = "<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 6 0 R >> >>";
    // Mask group: paints white only on the left half [0,0,50,100].
    let mask_form = "<< /Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 100 100] \
        /Group << /Type /Group /S /Transparency /CS /DeviceGray >> /Length 27 >>\n\
        stream\n1 g 0 0 50 100 re f\nendstream";
    let extra = [(smask_gs,), (mask_form,)];
    let pdf = page_pdf(content, &extra, page_extra);

    // Left quarter: masked-in -> black fill applied over white -> dark.
    let left = render_pixel(pdf.clone(), 72, 0.25, 0.5);
    // Right quarter: masked-out -> black fill suppressed -> stays white.
    let right = render_pixel(pdf, 72, 0.75, 0.5);

    assert!(
        left[0] < 80,
        "left should be dark (mask reveals): {:?}",
        left
    );
    assert!(
        right[0] > 180,
        "right should stay white (mask hides): {:?}",
        right
    );
}

#[test]
fn alpha_soft_mask_uses_alpha_channel() {
    // An alpha soft mask whose group paints an opaque rect on the LEFT half
    // (alpha=1 -> mask=1) and leaves the RIGHT transparent (alpha=0 -> mask=0).
    // Same black-over-white content as the luminosity test; left should darken,
    // right should stay white. Crucially the mask group paints BLACK (luminosity
    // would give 0 everywhere it paints), so only the /Alpha interpretation
    // produces the left-visible result.
    let content = b"1 1 1 rg 0 0 100 100 re f\n\
                    /GS1 gs 0 0 0 rg 0 0 100 100 re f\n";
    let page_extra = "/Resources << /ExtGState << /GS1 5 0 R >> >>";
    let smask_gs = "<< /Type /ExtGState /SMask << /Type /Mask /S /Alpha /G 6 0 R >> >>";
    // Mask group paints an opaque BLACK rect on the left half.
    let mask_form = "<< /Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 100 100] \
        /Group << /Type /Group /S /Transparency >> /Length 27 >>\n\
        stream\n0 g 0 0 50 100 re f\nendstream";
    let extra = [(smask_gs,), (mask_form,)];
    let pdf = page_pdf(content, &extra, page_extra);

    let left = render_pixel(pdf.clone(), 72, 0.25, 0.5);
    let right = render_pixel(pdf, 72, 0.75, 0.5);

    assert!(
        left[0] < 80,
        "left should be dark (alpha mask reveals): {:?}",
        left
    );
    assert!(
        right[0] > 180,
        "right should stay white (alpha mask hides): {:?}",
        right
    );
}

#[test]
fn image_xobject_smask_makes_pixels_transparent() {
    // A black 1x1 image with a fully transparent image /SMask should leave the
    // white page background unchanged. Without applying image SMask, this paints
    // an opaque black page.
    let content = b"q 100 0 0 100 0 0 cm /Im1 Do Q\n";
    let page_extra = "/Resources << /XObject << /Im1 5 0 R >> >>";
    let image = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
        /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R /Length 3 >>\n\
        stream\n\0\0\0\nendstream";
    let mask = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
        /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\n\
        stream\n\0\nendstream";
    let extra = [(image,), (mask,)];
    let pdf = page_pdf(content, &extra, page_extra);

    let center = render_center(pdf, 72);
    assert!(
        center[0] > 240 && center[1] > 240 && center[2] > 240,
        "fully transparent image mask should preserve white background, got {:?}",
        center
    );
}

#[test]
fn image_mask_xobject_paints_current_fill_color() {
    // ImageMask true is a stencil. A set bit should paint the current
    // nonstroking color, not the mask's grayscale sample.
    let content = b"1 0 0 rg q 100 0 0 100 0 0 cm /Im1 Do Q\n";
    let page_extra = "/Resources << /XObject << /Im1 5 0 R >> >>";
    let image = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
        /ImageMask true /Filter /ASCIIHexDecode /Length 3 >>\n\
        stream\n80>\nendstream";
    let extra = [(image,)];
    let pdf = page_pdf(content, &extra, page_extra);

    let center = render_center(pdf, 72);
    assert!(
        center[0] > 200 && center[1] < 40 && center[2] < 40,
        "image mask should paint current red fill color, got {:?}",
        center
    );
}

#[test]
fn inline_image_is_painted() {
    // A 2x2 RGB inline image scaled to fill the page: top-left red, top-right
    // green, bottom-left blue, bottom-right white (in PDF/image row order).
    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"q 100 0 0 100 0 0 cm\nBI /W 2 /H 2 /CS /RGB /BPC 8 ID\n");
    // Image rows top-to-bottom: row0 = red, green; row1 = blue, white.
    content.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    content.extend_from_slice(b"\nEI\nQ\n");

    let pdf = page_pdf(&content, &[], "/Resources << >>");

    // Device space flips Y: image row 0 (red/green) lands at the TOP of the page
    // buffer. Sample near the extreme corners (a 2x2 image stretched to 100x100
    // bilinear-blends near cell boundaries, so sample where each cell dominates).
    let tl = render_pixel(pdf.clone(), 72, 0.04, 0.04);
    let tr = render_pixel(pdf.clone(), 72, 0.96, 0.04);
    let bl = render_pixel(pdf.clone(), 72, 0.04, 0.96);
    let br = render_pixel(pdf, 72, 0.96, 0.96);

    // Top-left should be reddish (R dominant).
    assert!(
        tl[0] > 150 && tl[1] < 120 && tl[2] < 120,
        "TL not red: {:?}",
        tl
    );
    // Top-right should be greenish.
    assert!(tr[1] > 150 && tr[0] < 120, "TR not green: {:?}", tr);
    // Bottom-left should be bluish.
    assert!(bl[2] > 150 && bl[0] < 120, "BL not blue: {:?}", bl);
    // Bottom-right should be whitish.
    assert!(
        br[0] > 180 && br[1] > 180 && br[2] > 180,
        "BR not white: {:?}",
        br
    );
}
