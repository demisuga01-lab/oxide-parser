//! Tagged-PDF semantic extraction validation.

use oxide_engine::{ContentEngine, SemanticSource};

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

    fn add_stream(&mut self, stream: &[u8]) -> usize {
        let mut body = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
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
        for off in offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                self.objects.len() + 1,
                xref_start
            )
            .as_bytes(),
        );
        pdf
    }
}

fn marked_text(mcid: i64, tag: &str, x: f64, y: f64, text: &str) -> String {
    format!(
        "/{tag} <</MCID {mcid}>> BDC\nBT /F1 10 Tf 1 0 0 1 {x:.1} {y:.1} Tm ({text}) Tj ET\nEMC\n"
    )
}

fn tagged_pdf() -> Vec<u8> {
    let mut content = String::new();
    // Physical stream order is deliberately row-major/interleaved for the
    // column tokens; the structure tree below authors the correct order.
    content.push_str(&marked_text(0, "H1", 72.0, 740.0, "Semantic Title"));
    content.push_str(&marked_text(1, "P", 72.0, 720.0, "Intro paragraph"));
    content.push_str(&marked_text(2, "P", 320.0, 700.0, "Right column first"));
    content.push_str(&marked_text(3, "P", 72.0, 700.0, "Left column first"));
    content.push_str(&marked_text(4, "Lbl", 90.0, 670.0, "One"));
    content.push_str(&marked_text(5, "Lbl", 90.0, 655.0, "Two"));
    for (mcid, text, x, y, tag) in [
        (6, "Name", 72.0, 620.0, "TH"),
        (7, "Age", 180.0, 620.0, "TH"),
        (8, "Alice", 72.0, 605.0, "TD"),
        (9, "30", 180.0, 605.0, "TD"),
    ] {
        content.push_str(&marked_text(mcid, tag, x, y, text));
    }

    let mut b = PdfBuilder::new();
    b.add(
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> \
         /StructTreeRoot 6 0 R >>",
    );
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream(content.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add("<< /Type /StructTreeRoot /K [7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 21 0 R] >>");
    b.add("<< /Type /StructElem /S /H1 /P 6 0 R /Pg 3 0 R /K 0 >>");
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 1 >>");
    // Authored reading order: left column before right column, despite stream order.
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 3 >>");
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 2 >>");
    b.add("<< /Type /StructElem /S /L /P 6 0 R /K [13 0 R 14 0 R] >>");
    b.add("<< /Type /StructElem /S /Table /P 6 0 R /K [15 0 R 18 0 R] >>");
    b.add("<< /Type /StructElem /S /LI /P 11 0 R /Pg 3 0 R /K 4 >>");
    b.add("<< /Type /StructElem /S /LI /P 11 0 R /Pg 3 0 R /K 5 >>");
    b.add("<< /Type /StructElem /S /TR /P 12 0 R /K [16 0 R 17 0 R] >>");
    b.add("<< /Type /StructElem /S /TH /P 15 0 R /Pg 3 0 R /K 6 >>");
    b.add("<< /Type /StructElem /S /TH /P 15 0 R /Pg 3 0 R /K 7 >>");
    b.add("<< /Type /StructElem /S /TR /P 12 0 R /K [19 0 R 20 0 R] >>");
    b.add("<< /Type /StructElem /S /TD /P 18 0 R /Pg 3 0 R /K 8 >>");
    b.add("<< /Type /StructElem /S /TD /P 18 0 R /Pg 3 0 R /K 9 >>");
    b.add("<< /Type /StructElem /S /Figure /P 6 0 R /Pg 3 0 R /Alt (Revenue chart) >>");
    b.build()
}

fn untagged_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream(b"BT /F1 10 Tf 1 0 0 1 72 720 Tm (Fallback paragraph) Tj ET\n");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.build()
}

#[test]
fn tagged_structure_extracts_types_text_table_and_alt_text() {
    let engine = ContentEngine::open_bytes(tagged_pdf()).unwrap();
    let doc = engine.extract_semantic_document(&[1]).unwrap();

    assert!(doc.tagged);
    assert_eq!(doc.source, SemanticSource::TaggedPdf);
    assert_eq!(doc.elements[0].element_type, "H1");
    assert_eq!(doc.elements[0].text, "Semantic Title");

    let text = doc.to_text();
    let left = text.find("Left column first").unwrap();
    let right = text.find("Right column first").unwrap();
    assert!(left < right, "tag tree reading order should win:\n{text}");
    assert!(text.contains("Figure: Revenue chart"));

    assert_eq!(doc.tables.len(), 1);
    let table = &doc.tables[0];
    assert_eq!(table.rows[0], vec!["Name".to_string(), "Age".to_string()]);
    assert_eq!(table.rows[1], vec!["Alice".to_string(), "30".to_string()]);
    assert_eq!(table.to_csv(), "Name,Age\nAlice,30\n");
}

#[test]
fn untagged_semantic_mode_falls_back_to_geometry() {
    let engine = ContentEngine::open_bytes(untagged_pdf()).unwrap();
    let doc = engine.extract_semantic_document(&[1]).unwrap();

    assert!(!doc.tagged);
    assert_eq!(doc.source, SemanticSource::GeometricFallback);
    assert!(doc.to_text().contains("Fallback paragraph"));
}
