//! End-to-end table detection & extraction validation.
//!
//! There is no Poppler CLI that extracts tables to CSV, so the bar (per the
//! round spec) is HAND-AUTHORED ground truth: build PDFs with a known table and
//! assert the extracted rows × cells match exactly. We cover both detection
//! strategies — ruled (drawn grid lines) and borderless (alignment-only) — and
//! confirm prose is not falsely detected as a table.

use oxide_engine::analysis::tables::{Table, TableSource};
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

fn page(content: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream(content);
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.build()
}

fn cell_text(x: f64, y: f64, t: &str) -> String {
    format!("BT /F1 10 Tf 1 0 0 1 {x:.1} {y:.1} Tm ({t}) Tj ET\n")
}

fn stroke_grid(content: &mut String, xs: &[f64], ys: &[f64]) {
    content.push_str("1 w 0 0 0 RG\n");
    for &y in ys {
        content.push_str(&format!("{} {y} m {} {y} l S\n", xs[0], xs[xs.len() - 1]));
    }
    for &x in xs {
        content.push_str(&format!("{x} {} m {x} {} l S\n", ys[0], ys[ys.len() - 1]));
    }
}

fn stroke_line(content: &mut String, x0: f64, y0: f64, x1: f64, y1: f64) {
    content.push_str(&format!("{x0} {y0} m {x1} {y1} l S\n"));
}

const LABELS: [[&str; 3]; 3] = [
    ["Name", "Age", "City"],
    ["Alice", "30", "NYC"],
    ["Bob", "25", "LA"],
];

#[test]
fn ruled_table_extracts_exact_cells() {
    // 3x3 grid with drawn rules.
    let xs = [50.0, 180.0, 300.0, 430.0];
    let ys = [600.0, 625.0, 650.0, 675.0];
    let mut content = String::from("1 w 0 0 0 RG\n");
    for &y in &ys {
        content.push_str(&format!("{} {y} m {} {y} l S\n", xs[0], xs[3]));
    }
    for &x in &xs {
        content.push_str(&format!("{x} {} m {x} {} l S\n", ys[0], ys[3]));
    }
    // Text centred in each cell (rows top-to-bottom: y≈658,633,608).
    for (r, &yc) in [658.0, 633.0, 608.0].iter().enumerate() {
        for (c, &xc) in [60.0, 190.0, 310.0].iter().enumerate() {
            content.push_str(&cell_text(xc, yc, LABELS[r][c]));
        }
    }

    let engine = ContentEngine::open_bytes(page(content.as_bytes())).unwrap();
    let tables = engine.extract_tables(1).unwrap();
    assert_eq!(tables.len(), 1, "one ruled table");
    let t = &tables[0];
    assert_eq!(t.source, TableSource::Ruled);
    assert!(
        (t.confidence - 1.0).abs() < 1e-9,
        "ruled => full confidence"
    );
    assert_eq!(t.num_rows(), 3);
    assert_eq!(t.num_cols(), 3);
    assert_cells_match(&t.rows);
    assert_eq!(t.to_csv(), "Name,Age,City\nAlice,30,NYC\nBob,25,LA\n");
}

/// Assert the extracted rows exactly match the hand-authored ground truth.
fn assert_cells_match(rows: &[Vec<String>]) {
    for (row, expected) in rows.iter().zip(LABELS.iter()) {
        for (got, want) in row.iter().zip(expected.iter()) {
            assert_eq!(got, want, "cell mismatch");
        }
    }
}

#[test]
fn borderless_table_extracts_exact_cells() {
    // Same data, NO grid lines — pure alignment. Wide column gutters (~140pt).
    let mut content = String::new();
    for (r, &yc) in [658.0, 633.0, 608.0].iter().enumerate() {
        for (c, &xc) in [60.0, 200.0, 340.0].iter().enumerate() {
            content.push_str(&cell_text(xc, yc, LABELS[r][c]));
        }
    }
    let engine = ContentEngine::open_bytes(page(content.as_bytes())).unwrap();
    let tables = engine.extract_tables(1).unwrap();
    assert_eq!(tables.len(), 1, "one borderless table");
    let t = &tables[0];
    assert_eq!(t.source, TableSource::Borderless);
    assert_eq!(t.num_rows(), 3);
    assert_eq!(t.num_cols(), 3);
    assert_cells_match(&t.rows);
    // Heuristic, but a clean aligned grid should be high-confidence.
    assert!(
        t.confidence > 0.9,
        "clean borderless grid -> high confidence, got {:.2}",
        t.confidence
    );
}

#[test]
fn prose_page_yields_no_table() {
    // A normal paragraph must NOT be detected as a table (no false positives).
    let mut content = String::new();
    for i in 0..6 {
        content.push_str(&cell_text(
            72.0,
            700.0 - i as f64 * 14.0,
            "This is an ordinary sentence of running prose text.",
        ));
    }
    let engine = ContentEngine::open_bytes(page(content.as_bytes())).unwrap();
    let tables = engine.extract_tables(1).unwrap();
    assert!(tables.is_empty(), "prose must not be a table: {tables:?}");
}

#[derive(Clone)]
struct ExpectedSpan {
    text: &'static str,
    row: usize,
    col: usize,
    rowspan: usize,
    colspan: usize,
}

#[derive(Clone)]
struct ExpectedTable {
    rows: usize,
    cols: usize,
    source: TableSource,
    spans: Vec<ExpectedSpan>,
    headers: Vec<&'static str>,
    hierarchy_parent: Option<&'static str>,
    nested_tables: usize,
    html_contains: Option<&'static str>,
}

struct SmokeCase {
    name: &'static str,
    pdf: Vec<u8>,
    expected: ExpectedTable,
}

#[test]
fn table_structure_smoke_10_pdf_ground_truth() {
    let cases = table_smoke_cases();
    assert_eq!(cases.len(), 10, "the smoke set must stay at 10 PDFs");

    let mut perfect = 0usize;
    for case in cases {
        let engine = ContentEngine::open_bytes(case.pdf).unwrap();
        let first = engine.extract_tables(1).unwrap().into_iter().next();
        let Some(table) = first else {
            panic!("{}: expected one table", case.name);
        };

        let (passed, total) = score_table(&table, &case.expected);
        let accuracy = passed as f64 / total as f64;
        eprintln!(
            "table smoke {}: {}/{} checks ({:.0}%)",
            case.name,
            passed,
            total,
            accuracy * 100.0
        );
        assert_eq!(
            passed, total,
            "{}: structure mismatch\n{:#?}",
            case.name, table
        );
        perfect += 1;
    }
    assert_eq!(perfect, 10);
}

fn score_table(table: &Table, expected: &ExpectedTable) -> (usize, usize) {
    let mut passed = 0usize;
    let mut total = 0usize;

    total += 1;
    passed += usize::from(table.num_rows() == expected.rows);
    total += 1;
    passed += usize::from(table.num_cols() == expected.cols);
    total += 1;
    passed += usize::from(table.source == expected.source);

    for span in &expected.spans {
        total += 1;
        let found = table.cells.iter().any(|cell| {
            cell.text == span.text
                && cell.row == span.row
                && cell.col == span.col
                && cell.rowspan == span.rowspan
                && cell.colspan == span.colspan
        });
        passed += usize::from(found);
    }

    for header in &expected.headers {
        total += 1;
        let found = table
            .cells
            .iter()
            .any(|cell| cell.text == *header && cell.is_header);
        passed += usize::from(found);
    }

    if let Some(parent) = expected.hierarchy_parent {
        total += 1;
        passed += usize::from(
            table
                .header_hierarchy
                .iter()
                .any(|rel| rel.parent.text == parent && !rel.children.is_empty()),
        );
    }

    if expected.nested_tables > 0 {
        total += 1;
        let nested_count: usize = table
            .cells
            .iter()
            .map(|cell| cell.nested_tables.len())
            .sum();
        passed += usize::from(nested_count >= expected.nested_tables);
    }

    if let Some(fragment) = expected.html_contains {
        total += 1;
        passed += usize::from(table.to_html().contains(fragment));
    }

    (passed, total)
}

fn table_smoke_cases() -> Vec<SmokeCase> {
    vec![
        SmokeCase {
            name: "ruled-simple",
            pdf: ruled_simple_pdf(),
            expected: ExpectedTable {
                rows: 3,
                cols: 3,
                source: TableSource::Ruled,
                spans: vec![],
                headers: vec!["Name", "Age", "City"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("<thead>"),
            },
        },
        SmokeCase {
            name: "borderless-simple",
            pdf: borderless_simple_pdf(),
            expected: ExpectedTable {
                rows: 3,
                cols: 3,
                source: TableSource::Borderless,
                spans: vec![],
                headers: vec!["Name", "Age", "City"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("<th scope=\"col\">Name</th>"),
            },
        },
        SmokeCase {
            name: "ruled-colspan",
            pdf: ruled_colspan_pdf(),
            expected: ExpectedTable {
                rows: 2,
                cols: 3,
                source: TableSource::Ruled,
                spans: vec![ExpectedSpan {
                    text: "Group",
                    row: 0,
                    col: 0,
                    rowspan: 1,
                    colspan: 2,
                }],
                headers: vec!["Group", "Solo"],
                hierarchy_parent: Some("Group"),
                nested_tables: 0,
                html_contains: Some("colspan=\"2\""),
            },
        },
        SmokeCase {
            name: "ruled-rowspan",
            pdf: ruled_rowspan_pdf(),
            expected: ExpectedTable {
                rows: 2,
                cols: 3,
                source: TableSource::Ruled,
                spans: vec![ExpectedSpan {
                    text: "Span",
                    row: 0,
                    col: 0,
                    rowspan: 2,
                    colspan: 1,
                }],
                headers: vec!["Span", "B", "C"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("rowspan=\"2\""),
            },
        },
        SmokeCase {
            name: "two-level-header",
            pdf: two_level_header_pdf(),
            expected: ExpectedTable {
                rows: 3,
                cols: 3,
                source: TableSource::Ruled,
                spans: vec![ExpectedSpan {
                    text: "Group",
                    row: 0,
                    col: 0,
                    rowspan: 1,
                    colspan: 2,
                }],
                headers: vec!["Group", "Other", "Q1", "Q2"],
                hierarchy_parent: Some("Group"),
                nested_tables: 0,
                html_contains: Some("<thead>"),
            },
        },
        SmokeCase {
            name: "row-header-column",
            pdf: row_header_pdf(),
            expected: ExpectedTable {
                rows: 3,
                cols: 3,
                source: TableSource::Ruled,
                spans: vec![],
                headers: vec!["Region", "Sales", "Cost", "North", "South"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("scope=\"row\""),
            },
        },
        SmokeCase {
            name: "tagged-semantic",
            pdf: tagged_table_pdf(),
            expected: ExpectedTable {
                rows: 2,
                cols: 2,
                source: TableSource::Semantic,
                spans: vec![],
                headers: vec!["Name", "Age"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("<th scope=\"col\">Name</th>"),
            },
        },
        SmokeCase {
            name: "nested-ruled",
            pdf: nested_table_pdf(),
            expected: ExpectedTable {
                rows: 2,
                cols: 2,
                source: TableSource::Ruled,
                spans: vec![],
                headers: vec!["Outer A", "Outer B"],
                hierarchy_parent: None,
                nested_tables: 1,
                html_contains: Some("<table>"),
            },
        },
        SmokeCase {
            name: "thin-rect-rules",
            pdf: thin_rect_rules_pdf(),
            expected: ExpectedTable {
                rows: 2,
                cols: 2,
                source: TableSource::Ruled,
                spans: vec![],
                headers: vec!["H1", "H2"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("<th scope=\"col\">H1</th>"),
            },
        },
        SmokeCase {
            name: "html-escaping",
            pdf: html_escape_table_pdf(),
            expected: ExpectedTable {
                rows: 2,
                cols: 2,
                source: TableSource::Ruled,
                spans: vec![],
                headers: vec!["A&B", "C"],
                hierarchy_parent: None,
                nested_tables: 0,
                html_contains: Some("A&amp;B"),
            },
        },
    ]
}

fn ruled_simple_pdf() -> Vec<u8> {
    let xs = [50.0, 180.0, 300.0, 430.0];
    let ys = [600.0, 625.0, 650.0, 675.0];
    let mut content = String::new();
    stroke_grid(&mut content, &xs, &ys);
    add_label_grid(
        &mut content,
        &[
            ["Name", "Age", "City"],
            ["Alice", "30", "NYC"],
            ["Bob", "25", "LA"],
        ],
        &[60.0, 190.0, 310.0],
        &[658.0, 633.0, 608.0],
    );
    page(content.as_bytes())
}

fn borderless_simple_pdf() -> Vec<u8> {
    let mut content = String::new();
    add_label_grid(
        &mut content,
        &[
            ["Name", "Age", "City"],
            ["Alice", "30", "NYC"],
            ["Bob", "25", "LA"],
        ],
        &[60.0, 200.0, 340.0],
        &[658.0, 633.0, 608.0],
    );
    page(content.as_bytes())
}

fn ruled_colspan_pdf() -> Vec<u8> {
    let xs = [50.0, 180.0, 300.0, 430.0];
    let ys = [600.0, 625.0, 650.0];
    let mut content = String::from("1 w 0 0 0 RG\n");
    for &y in &ys {
        stroke_line(&mut content, xs[0], y, xs[3], y);
    }
    for &x in &[50.0, 300.0, 430.0] {
        stroke_line(&mut content, x, ys[0], x, ys[2]);
    }
    stroke_line(&mut content, 180.0, ys[0], 180.0, ys[1]);
    content.push_str(&cell_text(60.0, 633.0, "Group"));
    content.push_str(&cell_text(310.0, 633.0, "Solo"));
    for (x, t) in [(60.0, "A"), (190.0, "B"), (310.0, "C")] {
        content.push_str(&cell_text(x, 608.0, t));
    }
    page(content.as_bytes())
}

fn ruled_rowspan_pdf() -> Vec<u8> {
    let xs = [50.0, 180.0, 300.0, 430.0];
    let ys = [600.0, 625.0, 650.0];
    let mut content = String::from("1 w 0 0 0 RG\n");
    stroke_line(&mut content, xs[0], ys[0], xs[3], ys[0]);
    stroke_line(&mut content, xs[1], ys[1], xs[3], ys[1]);
    stroke_line(&mut content, xs[0], ys[2], xs[3], ys[2]);
    for &x in &xs {
        stroke_line(&mut content, x, ys[0], x, ys[2]);
    }
    content.push_str(&cell_text(60.0, 633.0, "Span"));
    content.push_str(&cell_text(190.0, 633.0, "B"));
    content.push_str(&cell_text(310.0, 633.0, "C"));
    content.push_str(&cell_text(190.0, 608.0, "E"));
    content.push_str(&cell_text(310.0, 608.0, "F"));
    page(content.as_bytes())
}

fn two_level_header_pdf() -> Vec<u8> {
    let xs = [50.0, 150.0, 250.0, 350.0];
    let ys = [600.0, 625.0, 650.0, 675.0];
    let mut content = String::from("1 w 0 0 0 RG\n");
    for &y in &ys {
        stroke_line(&mut content, xs[0], y, xs[3], y);
    }
    for &x in &[50.0, 250.0, 350.0] {
        stroke_line(&mut content, x, ys[0], x, ys[3]);
    }
    stroke_line(&mut content, 150.0, ys[0], 150.0, ys[2]);
    content.push_str(&cell_text(60.0, 658.0, "Group"));
    content.push_str(&cell_text(260.0, 658.0, "Other"));
    for (x, t) in [(60.0, "Q1"), (160.0, "Q2"), (260.0, "Q3")] {
        content.push_str(&cell_text(x, 633.0, t));
    }
    for (x, t) in [(60.0, "10"), (160.0, "12"), (260.0, "22")] {
        content.push_str(&cell_text(x, 608.0, t));
    }
    page(content.as_bytes())
}

fn row_header_pdf() -> Vec<u8> {
    let xs = [50.0, 180.0, 300.0, 430.0];
    let ys = [600.0, 625.0, 650.0, 675.0];
    let mut content = String::new();
    stroke_grid(&mut content, &xs, &ys);
    add_label_grid(
        &mut content,
        &[
            ["Region", "Sales", "Cost"],
            ["North", "100", "50"],
            ["South", "120", "70"],
        ],
        &[60.0, 190.0, 310.0],
        &[658.0, 633.0, 608.0],
    );
    page(content.as_bytes())
}

fn nested_table_pdf() -> Vec<u8> {
    let xs = [50.0, 200.0, 350.0];
    let ys = [600.0, 650.0, 700.0];
    let mut content = String::new();
    stroke_grid(&mut content, &xs, &ys);
    content.push_str(&cell_text(60.0, 680.0, "Outer A"));
    content.push_str(&cell_text(210.0, 680.0, "Outer B"));
    content.push_str(&cell_text(210.0, 625.0, "Body"));
    let nested_xs = [75.0, 115.0, 155.0];
    let nested_ys = [612.0, 630.0, 645.0];
    stroke_grid(&mut content, &nested_xs, &nested_ys);
    content.push_str(&cell_text(80.0, 635.0, "n1"));
    content.push_str(&cell_text(120.0, 635.0, "n2"));
    content.push_str(&cell_text(80.0, 618.0, "n3"));
    content.push_str(&cell_text(120.0, 618.0, "n4"));
    page(content.as_bytes())
}

fn thin_rect_rules_pdf() -> Vec<u8> {
    let xs = [50.0, 160.0, 270.0];
    let ys = [600.0, 625.0, 650.0];
    let mut content = String::from("0 0 0 rg\n");
    for &y in &ys {
        content.push_str(&format!("50 {y} 220 1 re f\n"));
    }
    for &x in &xs {
        content.push_str(&format!("{x} 600 1 50 re f\n"));
    }
    content.push_str(&cell_text(60.0, 633.0, "H1"));
    content.push_str(&cell_text(170.0, 633.0, "H2"));
    content.push_str(&cell_text(60.0, 608.0, "V1"));
    content.push_str(&cell_text(170.0, 608.0, "V2"));
    page(content.as_bytes())
}

fn html_escape_table_pdf() -> Vec<u8> {
    let xs = [50.0, 160.0, 270.0];
    let ys = [600.0, 625.0, 650.0];
    let mut content = String::new();
    stroke_grid(&mut content, &xs, &ys);
    content.push_str(&cell_text(60.0, 633.0, "A&B"));
    content.push_str(&cell_text(170.0, 633.0, "C"));
    content.push_str(&cell_text(60.0, 608.0, "D"));
    content.push_str(&cell_text(170.0, 608.0, "E"));
    page(content.as_bytes())
}

fn add_label_grid<const R: usize, const C: usize>(
    content: &mut String,
    labels: &[[&str; C]; R],
    xs: &[f64],
    ys: &[f64],
) {
    for (r, row) in labels.iter().enumerate() {
        for (c, label) in row.iter().enumerate() {
            content.push_str(&cell_text(xs[c], ys[r], label));
        }
    }
}

fn tagged_table_pdf() -> Vec<u8> {
    let mut content = String::new();
    for (mcid, text, x, y, tag) in [
        (0, "Name", 72.0, 620.0, "TH"),
        (1, "Age", 180.0, 620.0, "TH"),
        (2, "Alice", 72.0, 605.0, "TD"),
        (3, "30", 180.0, 605.0, "TD"),
    ] {
        content.push_str(&format!(
            "/{tag} <</MCID {mcid}>> BDC\nBT /F1 10 Tf 1 0 0 1 {x:.1} {y:.1} Tm ({text}) Tj ET\nEMC\n"
        ));
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
    b.add("<< /Type /StructTreeRoot /K [7 0 R] >>");
    b.add("<< /Type /StructElem /S /Table /P 6 0 R /K [8 0 R 11 0 R] >>");
    b.add("<< /Type /StructElem /S /TR /P 7 0 R /K [9 0 R 10 0 R] >>");
    b.add("<< /Type /StructElem /S /TH /P 8 0 R /Pg 3 0 R /K 0 >>");
    b.add("<< /Type /StructElem /S /TH /P 8 0 R /Pg 3 0 R /K 1 >>");
    b.add("<< /Type /StructElem /S /TR /P 7 0 R /K [12 0 R 13 0 R] >>");
    b.add("<< /Type /StructElem /S /TD /P 11 0 R /Pg 3 0 R /K 2 >>");
    b.add("<< /Type /StructElem /S /TD /P 11 0 R /Pg 3 0 R /K 3 >>");
    b.build()
}
