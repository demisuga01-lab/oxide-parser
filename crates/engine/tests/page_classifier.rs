//! Engine-level page-classifier tests: run the real classifier (collector +
//! graphics) over synthetic PDFs whose nature is known by construction.

use oxide_engine::{classify_page, ClassifyConfig, ContentEngine, PageSource};

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
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
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

fn text_page() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    for i in 0..10 {
        let y = 700.0 - i as f64 * 14.0;
        c.push_str(&format!(
            "BT /F1 11 Tf 1 0 0 1 72 {y:.1} Tm (A line of ordinary selectable body text number {i}.) Tj ET\n"
        ));
    }
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.build()
}

fn full_image_no_text() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", b"q 612 0 0 792 0 0 cm /Im0 Do Q\n");
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.build()
}

fn full_image_with_invisible_text() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 6 0 R >> /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    c.push_str("q 612 0 0 792 0 0 cm /Im0 Do Q\n");
    c.push_str("BT 3 Tr /F1 10 Tf 1 0 0 1 72 720 Tm (Invisible searchable layer over the scanned image here) Tj ET\n");
    b.add_stream("", c.as_bytes());
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.build()
}

fn small_logo_on_text_page() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 6 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    // small 40x40 logo in the corner
    c.push_str("q 40 0 0 40 72 740 cm /Im0 Do Q\n");
    for i in 0..12 {
        let y = 700.0 - i as f64 * 14.0;
        c.push_str(&format!(
            "BT /F1 11 Tf 1 0 0 1 72 {y:.1} Tm (Plenty of real body text on this page, line {i} of prose.) Tj ET\n"
        ));
    }
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    b.build()
}

#[test]
fn classifies_text_page_as_digital_born() {
    let engine = ContentEngine::open_bytes(text_page()).unwrap();
    let c = classify_page(&engine, 1, &ClassifyConfig::default());
    assert_eq!(c.source, PageSource::DigitalBorn, "{c:?}");
    assert!(c.char_count > 50);
    assert!(c.image_coverage < 0.01);
}

#[test]
fn classifies_full_image_no_text_as_scanned() {
    let engine = ContentEngine::open_bytes(full_image_no_text()).unwrap();
    let c = classify_page(&engine, 1, &ClassifyConfig::default());
    assert_eq!(c.source, PageSource::Scanned, "{c:?}");
    assert!(c.image_coverage > 0.9, "full-page image: {c:?}");
    assert_eq!(c.char_count, 0);
}

#[test]
fn classifies_searchable_scan_as_digital_born_over_image() {
    let engine = ContentEngine::open_bytes(full_image_with_invisible_text()).unwrap();
    let c = classify_page(&engine, 1, &ClassifyConfig::default());
    assert_eq!(c.source, PageSource::DigitalBornOverImage, "{c:?}");
    assert!(c.has_invisible_text, "invisible text detected: {c:?}");
    assert!(c.image_coverage > 0.9);
    assert!(c.char_count > 0, "uses the existing text layer");
}

#[test]
fn small_logo_does_not_flip_text_page_to_scanned() {
    let engine = ContentEngine::open_bytes(small_logo_on_text_page()).unwrap();
    let c = classify_page(&engine, 1, &ClassifyConfig::default());
    assert_eq!(c.source, PageSource::DigitalBorn, "{c:?}");
    assert!(c.image_coverage < 0.5, "logo is small: {c:?}");
}
