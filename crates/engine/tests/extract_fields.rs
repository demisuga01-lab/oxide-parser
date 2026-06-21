//! Key-value / form-field extraction tests (Parser-Pivot prompt 4).
//!
//! Covers all three strategies on synthetic-but-realistic PDFs:
//! - AcroForm direct field→value (with `/TU` labels);
//! - spatial label→value pairing (inline `Total: $42.00`, label-above-value);
//! - invoice profile (canonical field re-keying + line-item table → rows);
//! - document-type detection;
//! - determinism.
//!
//! Source-agnostic KV (the *same* engine on an OCR'd scanned invoice) is proven
//! in `oxide-ocr-tesseract/tests/smoke.rs` (gated on a local tesseract).

use oxide_engine::{ContentEngine, DocType, ExtractOptions, FieldSource, FieldValue};

// ── a tiny PDF builder ───────────────────────────────────────────────────────

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
    fn add_raw(&mut self, body: Vec<u8>) -> usize {
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
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                offsets.len() + 1,
                xref
            )
            .as_bytes(),
        );
        pdf
    }
}

/// One text-show op at a baseline position.
fn text_at(c: &mut String, x: f64, y: f64, size: f64, s: &str) {
    // Escape parens for the literal string.
    let esc = s.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)");
    c.push_str(&format!("BT /F1 {size} Tf 1 0 0 1 {x:.1} {y:.1} Tm ({esc}) Tj ET\n"));
}

// ── digital invoice fixture ──────────────────────────────────────────────────

/// A born-digital invoice page with labeled fields and a line-item table laid
/// out as aligned text columns (a borderless table the detector recovers).
fn invoice_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    // Title / vendor at the top.
    text_at(&mut c, 72.0, 740.0, 20.0, "Acme Supplies Inc");
    text_at(&mut c, 72.0, 715.0, 18.0, "INVOICE");
    // Labeled header fields (label and value on the same baseline, value to the
    // right — the right-of pairing case).
    text_at(&mut c, 72.0, 680.0, 11.0, "Invoice Number:");
    text_at(&mut c, 200.0, 680.0, 11.0, "INV-2024-0042");
    text_at(&mut c, 72.0, 664.0, 11.0, "Invoice Date:");
    text_at(&mut c, 200.0, 664.0, 11.0, "Jan 15, 2024");
    text_at(&mut c, 72.0, 648.0, 11.0, "Due Date:");
    text_at(&mut c, 200.0, 648.0, 11.0, "2024-02-15");
    text_at(&mut c, 72.0, 632.0, 11.0, "Bill To:");
    text_at(&mut c, 200.0, 632.0, 11.0, "Globex Corporation");

    // Line-item table (header + 2 rows) as aligned columns.
    let cols = [80.0f64, 320.0, 400.0, 490.0];
    let rows = [
        ["Description", "Qty", "Unit Price", "Amount"],
        ["Widget assembly", "10", "$25.00", "$250.00"],
        ["Premium gizmo", "2", "$100.00", "$200.00"],
    ];
    let mut y = 580.0;
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            text_at(&mut c, cols[i], y, 11.0, cell);
        }
        y -= 18.0;
    }

    // Totals block (label left, amount right).
    text_at(&mut c, 360.0, 500.0, 11.0, "Subtotal:");
    text_at(&mut c, 490.0, 500.0, 11.0, "$450.00");
    text_at(&mut c, 360.0, 484.0, 11.0, "Tax:");
    text_at(&mut c, 490.0, 484.0, 11.0, "$36.00");
    text_at(&mut c, 360.0, 468.0, 13.0, "Total:");
    text_at(&mut c, 490.0, 468.0, 13.0, "$486.00");

    let content = format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len());
    b.add_raw(content.into_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.build()
}

// ── AcroForm fixture ─────────────────────────────────────────────────────────

/// A page with a real `/AcroForm`: two text fields (one with a `/TU` label) and
/// a checkbox, each with a widget rect on the page.
fn acroform_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >> >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    // Page references the widgets as annotations.
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Annots [5 0 R 6 0 R 7 0 R] /Contents 4 0 R >>",
    );
    b.add("<< /Length 0 >>\nstream\n\nendstream");
    // Field 1: text, /T "applicant_name", /TU "Applicant Name", /V "Jane Doe".
    b.add(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (applicant_name) \
         /TU (Applicant Name) /V (Jane Doe) /Rect [72 700 300 720] >>",
    );
    // Field 2: text, /T "amount", /V "1500.00", no /TU.
    b.add(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (amount) /V (1500.00) \
         /Rect [72 660 300 680] >>",
    );
    // Field 3: checkbox, /T "agree", /V /Yes.
    b.add(
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /V /Yes \
         /Rect [72 620 92 640] >>",
    );
    b.build()
}

// ── receipt fixture ──────────────────────────────────────────────────────────

fn receipt_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 500] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    let mut c = String::new();
    text_at(&mut c, 40.0, 460.0, 14.0, "Joe's Coffee Shop");
    text_at(&mut c, 40.0, 440.0, 10.0, "RECEIPT");
    text_at(&mut c, 40.0, 420.0, 10.0, "Date: 03/22/2024");
    text_at(&mut c, 40.0, 380.0, 10.0, "Subtotal:");
    text_at(&mut c, 200.0, 380.0, 10.0, "$8.50");
    text_at(&mut c, 40.0, 366.0, 10.0, "Tax:");
    text_at(&mut c, 200.0, 366.0, 10.0, "$0.68");
    text_at(&mut c, 40.0, 352.0, 10.0, "Total:");
    text_at(&mut c, 200.0, 352.0, 10.0, "$9.18");
    text_at(&mut c, 40.0, 330.0, 10.0, "Payment: VISA ****1234");
    let content = format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len());
    b.add_raw(content.into_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.build()
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
fn acroform_fields_extracted_exactly_with_tu_labels() {
    let engine = ContentEngine::open_bytes(acroform_pdf()).unwrap();
    let result = engine.extract_fields(&ExtractOptions::default()).unwrap();

    assert_eq!(result.doc_type, DocType::Form, "AcroForm doc detected as Form");

    // The /TU label is preferred over /T as the key.
    let name = result.get("Applicant Name").expect("applicant name field");
    assert_eq!(name.source, FieldSource::AcroForm);
    assert_eq!(name.confidence, 1.0, "AcroForm values are exact");
    assert_eq!(name.raw, "Jane Doe");

    // The amount field (no /TU) keys on /T and normalizes the value.
    let amount = result.get("amount").expect("amount field");
    assert_eq!(amount.source, FieldSource::AcroForm);
    assert_eq!(amount.raw, "1500.00");

    // The checkbox is a Bool(true) for /V /Yes.
    let agree = result.get("agree").expect("checkbox field");
    assert!(matches!(agree.value, FieldValue::Bool { value: true }));

    // Each field carries a page + a non-zero widget bbox.
    assert_eq!(name.page, 1);
    assert!(name.bbox[2] > name.bbox[0]);
}

#[test]
fn invoice_profile_extracts_canonical_fields_and_normalizes() {
    let engine = ContentEngine::open_bytes(invoice_pdf()).unwrap();
    let result = engine
        .extract_fields(&ExtractOptions {
            doc_type: Some(DocType::Invoice),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.doc_type, DocType::Invoice);

    let inv_no = result.get("invoice_number").expect("invoice_number");
    assert_eq!(inv_no.raw, "INV-2024-0042");
    assert_eq!(inv_no.source, FieldSource::Template);

    // Dates normalize to ISO regardless of input format.
    let date = result.get("invoice_date").expect("invoice_date");
    assert!(
        matches!(&date.value, FieldValue::Date { iso } if iso == "2024-01-15"),
        "invoice_date should be ISO 2024-01-15, got {:?}",
        date.value
    );
    let due = result.get("due_date").expect("due_date");
    assert!(matches!(&due.value, FieldValue::Date { iso } if iso == "2024-02-15"));

    // Amounts normalize to decimal + currency.
    let total = result.get("total").expect("total");
    assert!(
        matches!(&total.value, FieldValue::Amount { value, currency } if *value == 486.0 && currency.as_deref() == Some("USD")),
        "total should be 486.00 USD, got {:?}",
        total.value
    );
}

#[test]
fn invoice_line_items_mapped_from_table_columns() {
    let engine = ContentEngine::open_bytes(invoice_pdf()).unwrap();
    let result = engine
        .extract_fields(&ExtractOptions {
            doc_type: Some(DocType::Invoice),
            ..Default::default()
        })
        .unwrap();

    assert!(
        result.line_items.len() >= 2,
        "expected >=2 line items, got {}: {:?}",
        result.line_items.len(),
        result.line_items
    );
    let first = &result.line_items[0];
    assert!(
        first.description.as_deref().unwrap_or("").contains("Widget"),
        "first item description: {:?}",
        first.description
    );
    assert_eq!(first.quantity, Some(10.0));
    assert!(
        matches!(&first.amount, Some(FieldValue::Amount { value, .. }) if *value == 250.0),
        "first item amount: {:?}",
        first.amount
    );
}

#[test]
fn auto_detects_invoice_and_receipt() {
    let inv = ContentEngine::open_bytes(invoice_pdf()).unwrap();
    let r = inv.extract_fields(&ExtractOptions::default()).unwrap();
    assert_eq!(r.doc_type, DocType::Invoice, "auto-detect invoice");
    assert!(!r.doc_type_forced);

    let rec = ContentEngine::open_bytes(receipt_pdf()).unwrap();
    let r = rec.extract_fields(&ExtractOptions::default()).unwrap();
    assert_eq!(r.doc_type, DocType::Receipt, "auto-detect receipt");
    let total = r.get("total").expect("receipt total");
    assert!(matches!(&total.value, FieldValue::Amount { value, .. } if *value == 9.18));
}

#[test]
fn spatial_pairing_does_not_mispair_label_as_value() {
    // The invoice has consecutive labels ("Subtotal:", "Tax:", "Total:"). None
    // of these labels must be paired AS the value of another label.
    let engine = ContentEngine::open_bytes(invoice_pdf()).unwrap();
    let r = engine.extract_fields(&ExtractOptions::default()).unwrap();
    for f in &r.fields {
        let v = f.value.as_text();
        assert!(
            !v.trim_end().ends_with(':'),
            "field {:?} took a label as its value: {v:?}",
            f.key
        );
    }
}

#[test]
fn extraction_is_deterministic() {
    let engine = ContentEngine::open_bytes(invoice_pdf()).unwrap();
    let a = engine.extract_fields(&ExtractOptions::default()).unwrap();
    let b = engine.extract_fields(&ExtractOptions::default()).unwrap();
    assert_eq!(a.to_json(), b.to_json(), "same document → identical fields");
}

#[test]
fn json_output_is_machine_readable() {
    let engine = ContentEngine::open_bytes(invoice_pdf()).unwrap();
    let r = engine
        .extract_fields(&ExtractOptions {
            doc_type: Some(DocType::Invoice),
            ..Default::default()
        })
        .unwrap();
    let json = r.to_json();
    // Typed values serialize with their tag.
    assert!(json.contains("\"type\": \"amount\""), "amount type tag present:\n{json}");
    assert!(json.contains("\"type\": \"date\""), "date type tag present");
    assert!(json.contains("\"doc_type\": \"invoice\""));
    assert!(json.contains("\"line_items\""));
}
