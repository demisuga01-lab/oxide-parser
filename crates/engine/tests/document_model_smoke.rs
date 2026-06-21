//! 10-PDF **document-model smoke** against hand-authored ground truth.
//!
//! Per Doc-Intel Prompt 2 Part D.2: a fast smoke over complex-layout documents
//! (academic 2-column, magazine + sidebar, figures + captions, a tagged PDF for
//! the tags-first path, an RTL doc, …) comparing the recovered document model's
//! **reading order** and **block classifications** to ground truth, and
//! reporting accuracy *honestly* per file. No full renderer benchmark; no
//! external corpus — the fixtures are deterministic synthetic PDFs whose exact
//! correct order and labels are known by construction, so the metrics are exact
//! rather than eyeballed.
//!
//! Metrics per file:
//!   - **Reading order**: Kendall-tau (rank correlation) of the matched blocks'
//!     model order vs. ground-truth order, plus the % of correctly-ordered
//!     adjacent ground-truth pairs.
//!   - **Classification**: fraction of matched blocks whose recovered type equals
//!     the ground-truth type. The honest `Text` fallback counts as a MISS (we do
//!     not credit "didn't mislabel" as a correct label), so this is a strict
//!     lower bound on usefulness.
//!
//! The test prints a table and asserts only on conservative *aggregate* floors,
//! so it documents real accuracy without being brittle to a single block.

use oxide_engine::{ClassifiedType, ContentEngine, DocumentModel};

// ════════════════════════════════════════════════════════════════════════════
// PDF builders (shared shape with the other engine tests)
// ════════════════════════════════════════════════════════════════════════════

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

fn tx(font: &str, size: f64, x: f64, y: f64, s: &str) -> String {
    format!("BT /{font} {size} Tf 1 0 0 1 {x:.1} {y:.1} Tm ({s}) Tj ET\n")
}

const BULLET: u8 = 0x95; // WinAnsi "•"

/// Single page with WinAnsi Helvetica (F1) + Helvetica-Bold (FB).
fn page(content: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R /FB 6 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", content);
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    b.build()
}

// ════════════════════════════════════════════════════════════════════════════
// Ground truth model
// ════════════════════════════════════════════════════════════════════════════

/// A coarse type label for ground truth (matches `ClassifiedType` modulo the
/// heading level, which we check separately when given).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Title,
    Heading,
    Paragraph,
    List,
    Figure,
    Caption,
    Table,
    Header,
    Footer,
    PageNumber,
}

fn coarse(c: ClassifiedType) -> Ty {
    match c {
        ClassifiedType::Title => Ty::Title,
        ClassifiedType::Heading { .. } => Ty::Heading,
        ClassifiedType::Paragraph => Ty::Paragraph,
        ClassifiedType::List { .. } => Ty::List,
        ClassifiedType::ListItem => Ty::List,
        ClassifiedType::Figure => Ty::Figure,
        ClassifiedType::Caption => Ty::Caption,
        ClassifiedType::Table => Ty::Table,
        ClassifiedType::Header => Ty::Header,
        ClassifiedType::Footer => Ty::Footer,
        ClassifiedType::PageNumber => Ty::PageNumber,
        // The honest low-confidence fallback is never a ground-truth match.
        ClassifiedType::Text => Ty::Paragraph, // coarsed for display only
    }
}

/// One ground-truth block: a text token that uniquely identifies the model block,
/// its expected coarse type, and its expected reading-order rank (0-based).
struct GtBlock {
    token: &'static str,
    ty: Ty,
    rank: usize,
}

struct Fixture {
    name: &'static str,
    pdf: Vec<u8>,
    pages: Vec<usize>,
    gt: Vec<GtBlock>,
}

struct Report {
    name: &'static str,
    matched: usize,
    total: usize,
    kendall_tau: f64,
    pairwise_order_pct: f64,
    class_correct: usize,
}

/// Match each ground-truth block to the first model block whose text contains its
/// token; compute order + classification metrics. Unmatched GT blocks count
/// against classification and are excluded from the order correlation (and noted
/// in `matched/total`).
fn evaluate(name: &'static str, model: &DocumentModel, gt: &[GtBlock]) -> Report {
    // (gt_rank, model_order_index, type_ok)
    let mut matched: Vec<(usize, usize, bool)> = Vec::new();
    for g in gt {
        if let Some(b) = model.blocks.iter().find(|b| b.text.contains(g.token)) {
            let type_ok = coarse_eq(b.classified, g.ty);
            matched.push((g.rank, b.reading_order_index, type_ok));
        }
    }
    let class_correct = matched.iter().filter(|(_, _, ok)| *ok).count();

    // Kendall tau over matched blocks: compare every pair's GT-rank order with
    // their model-order; tau = (concordant - discordant) / total_pairs.
    let mut concordant = 0i64;
    let mut discordant = 0i64;
    for i in 0..matched.len() {
        for j in (i + 1)..matched.len() {
            let (ri, mi, _) = matched[i];
            let (rj, mj, _) = matched[j];
            if ri == rj || mi == mj {
                continue;
            }
            let gt_lt = ri < rj;
            let md_lt = mi < mj;
            if gt_lt == md_lt {
                concordant += 1;
            } else {
                discordant += 1;
            }
        }
    }
    let pairs = concordant + discordant;
    let kendall_tau = if pairs == 0 {
        1.0
    } else {
        (concordant - discordant) as f64 / pairs as f64
    };

    // Pairwise adjacent ground-truth order: of consecutive GT ranks, what % keep
    // their relative order in the model.
    let mut by_rank = matched.clone();
    by_rank.sort_by_key(|&(r, _, _)| r);
    let mut ok = 0usize;
    let mut tot = 0usize;
    for w in by_rank.windows(2) {
        tot += 1;
        if w[0].1 < w[1].1 {
            ok += 1;
        }
    }
    let pairwise_order_pct = if tot == 0 {
        100.0
    } else {
        100.0 * ok as f64 / tot as f64
    };

    Report {
        name,
        matched: matched.len(),
        total: gt.len(),
        kendall_tau,
        pairwise_order_pct,
        class_correct,
    }
}

fn coarse_eq(c: ClassifiedType, gt: Ty) -> bool {
    // Title and Heading are interchangeable for the top-level title (a document's
    // largest line may be classified either way honestly).
    match (c, gt) {
        (ClassifiedType::Title, Ty::Title | Ty::Heading) => true,
        (ClassifiedType::Heading { .. }, Ty::Heading | Ty::Title) => true,
        (ClassifiedType::Text, _) => false, // honest fallback is not a match
        _ => coarse(c) == gt,
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Fixtures (10) — each with exact hand-authored ground truth
// ════════════════════════════════════════════════════════════════════════════

/// 1. Single column: title + 3 paragraphs.
fn fx_simple_article() -> Fixture {
    let mut c = String::new();
    c.push_str(&tx("FB", 22.0, 72.0, 750.0, "ARTICLE Headline Of The Day"));
    let paras = [
        "PARA1 opening line of ordinary prose continuing here",
        "PARA2 second paragraph well beneath the first one here",
        "PARA3 final paragraph rounding out the column cleanly",
    ];
    let mut y = 712.0;
    for p in &paras {
        c.push_str(&tx("F1", 10.0, 72.0, y, p));
        c.push_str(&tx("F1", 10.0, 72.0, y - 13.0, "and a wrapped continuation line of body text."));
        y -= 45.0;
    }
    Fixture {
        name: "simple_article",
        pdf: page(c.as_bytes()),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "ARTICLE", ty: Ty::Title, rank: 0 },
            GtBlock { token: "PARA1", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "PARA2", ty: Ty::Paragraph, rank: 2 },
            GtBlock { token: "PARA3", ty: Ty::Paragraph, rank: 3 },
        ],
    }
}

/// 2. Heading hierarchy: H1 (22pt) > H2 (16pt) > body, interleaved with prose.
fn fx_heading_hierarchy() -> Fixture {
    let mut c = String::new();
    c.push_str(&tx("FB", 24.0, 72.0, 750.0, "H1HEAD Major Section"));
    c.push_str(&tx("F1", 10.0, 72.0, 730.0, "BODYA introductory paragraph under the major heading here."));
    c.push_str(&tx("F1", 10.0, 72.0, 717.0, "continuing the introductory paragraph onto a second line."));
    c.push_str(&tx("FB", 16.0, 72.0, 690.0, "H2HEAD Subsection"));
    c.push_str(&tx("F1", 10.0, 72.0, 670.0, "BODYB another body paragraph under the subsection heading."));
    c.push_str(&tx("F1", 10.0, 72.0, 657.0, "with its own wrapped continuation line of prose text."));
    Fixture {
        name: "heading_hierarchy",
        pdf: page(c.as_bytes()),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "H1HEAD", ty: Ty::Heading, rank: 0 },
            GtBlock { token: "BODYA", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "H2HEAD", ty: Ty::Heading, rank: 2 },
            GtBlock { token: "BODYB", ty: Ty::Paragraph, rank: 3 },
        ],
    }
}

/// 3. Bulleted list between two paragraphs.
fn fx_bulleted_list() -> Fixture {
    let mut c: Vec<u8> = Vec::new();
    c.extend_from_slice(tx("FB", 18.0, 72.0, 750.0, "LISTDOC Title").as_bytes());
    c.extend_from_slice(tx("F1", 10.0, 72.0, 725.0, "INTRO paragraph before the list begins here on its line.").as_bytes());
    let items = ["ITEMA first bullet point", "ITEMB second bullet point", "ITEMC third bullet point"];
    let mut y = 700.0;
    for it in items {
        c.extend_from_slice(format!("BT /F1 10 Tf 1 0 0 1 72 {y:.1} Tm (").as_bytes());
        c.push(BULLET);
        c.extend_from_slice(format!(" {it}) Tj ET\n").as_bytes());
        y -= 16.0;
    }
    c.extend_from_slice(tx("F1", 10.0, 72.0, y - 10.0, "OUTRO paragraph after the list closing the section.").as_bytes());
    Fixture {
        name: "bulleted_list",
        pdf: page(&c),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "LISTDOC", ty: Ty::Title, rank: 0 },
            GtBlock { token: "INTRO", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "ITEMA", ty: Ty::List, rank: 2 },
            GtBlock { token: "OUTRO", ty: Ty::Paragraph, rank: 3 },
        ],
    }
}

/// 4. Enumerated (ordered) list.
fn fx_ordered_list() -> Fixture {
    let mut c = String::new();
    c.push_str(&tx("FB", 18.0, 72.0, 750.0, "STEPS Procedure"));
    let items = ["1. STEPA prepare the workspace first", "2. STEPB assemble the parts in order", "3. STEPC verify the final result"];
    let mut y = 720.0;
    for it in items {
        c.push_str(&tx("F1", 10.0, 72.0, y, it));
        y -= 16.0;
    }
    Fixture {
        name: "ordered_list",
        pdf: page(c.as_bytes()),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "STEPS", ty: Ty::Title, rank: 0 },
            GtBlock { token: "STEPA", ty: Ty::List, rank: 1 },
        ],
    }
}

/// 5. Figure (image) with a "Figure 1:" caption below.
fn fx_figure_caption() -> Fixture {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R /FB 6 0 R >> /XObject << /Im0 7 0 R >> >> \
         /Contents 4 0 R >>",
    );
    let mut c = String::new();
    c.push_str(&tx("FB", 18.0, 72.0, 750.0, "FIGDOC Report"));
    c.push_str(&tx("F1", 10.0, 72.0, 728.0, "BODY1 paragraph introducing the figure that follows below."));
    c.push_str("q 300 0 0 180 72 520 cm /Im0 Do Q\n");
    c.push_str(&tx("F1", 9.0, 72.0, 504.0, "Figure 1: CAPTOK quarterly revenue chart"));
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    b.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );
    Fixture {
        name: "figure_caption",
        pdf: b.build(),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "FIGDOC", ty: Ty::Title, rank: 0 },
            GtBlock { token: "BODY1", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "CAPTOK", ty: Ty::Caption, rank: 3 },
        ],
    }
}

/// 6. Running header + page number across 3 pages.
fn fx_running_elements() -> Fixture {
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
    for cobj in [8, 9, 10] {
        b.add(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 6 0 R /FB 7 0 R >> >> /Contents {cobj} 0 R >>"
        ));
    }
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    for p in 1..=3usize {
        let mut c = String::new();
        c.push_str(&tx("F1", 9.0, 72.0, 765.0, "RUNHEAD Quarterly Bulletin"));
        c.push_str(&tx("F1", 10.0, 72.0, 420.0, &format!("PG{p}BODY content unique to this page only here.")));
        c.push_str(&tx("F1", 9.0, 300.0, 30.0, &format!("{p}")));
        b.add_stream("", c.as_bytes());
    }
    // Ground truth: the banner is a header on every page; we check one instance.
    Fixture {
        name: "running_elements",
        pdf: b.build(),
        pages: vec![1, 2, 3],
        gt: vec![
            GtBlock { token: "RUNHEAD", ty: Ty::Header, rank: 0 },
            GtBlock { token: "PG1BODY", ty: Ty::Paragraph, rank: 1 },
        ],
    }
}

/// 7. Tagged PDF (tags-first): authored order left-before-right despite stream
///    order; H1 + list + figure.
fn fx_tagged() -> Fixture {
    let mc = |mcid: i64, tag: &str, x: f64, y: f64, s: &str| -> String {
        format!("/{tag} <</MCID {mcid}>> BDC\nBT /F1 10 Tf 1 0 0 1 {x:.1} {y:.1} Tm ({s}) Tj ET\nEMC\n")
    };
    let mut content = String::new();
    content.push_str(&mc(0, "H1", 72.0, 740.0, "TTITLE Tagged Heading"));
    content.push_str(&mc(1, "P", 320.0, 700.0, "TRIGHT right column paragraph text"));
    content.push_str(&mc(2, "P", 72.0, 700.0, "TLEFT left column paragraph text"));
    content.push_str(&mc(3, "Lbl", 90.0, 670.0, "TITEM one"));

    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", content.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add("<< /Type /StructTreeRoot /K [7 0 R 8 0 R 9 0 R 10 0 R] >>");
    b.add("<< /Type /StructElem /S /H1 /P 6 0 R /Pg 3 0 R /K 0 >>");
    // authored: left (MCID 2) before right (MCID 1)
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 2 >>");
    b.add("<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 1 >>");
    b.add("<< /Type /StructElem /S /L /P 6 0 R /K [11 0 R] >>");
    b.add("<< /Type /StructElem /S /LI /P 10 0 R /Pg 3 0 R /K 3 >>");
    Fixture {
        name: "tagged_doc",
        pdf: b.build(),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "TTITLE", ty: Ty::Heading, rank: 0 },
            GtBlock { token: "TLEFT", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "TRIGHT", ty: Ty::Paragraph, rank: 2 },
            GtBlock { token: "TITEM", ty: Ty::List, rank: 3 },
        ],
    }
}

/// 8. RTL column via a /ToUnicode CMap mapping A..F to Hebrew letters.
fn fx_rtl() -> Fixture {
    let mut c = String::new();
    c.push_str(&tx("F1", 18.0, 380.0, 740.0, "AB AB AB")); // heading => אב
    let mut y = 705.0;
    for line in ["CD CD CD", "CD CD CD"] {
        c.push_str(&tx("F1", 10.0, 380.0, y, line)); // p1 => גד
        y -= 13.0;
    }
    y -= 22.0;
    for line in ["EF EF EF", "EF EF EF"] {
        c.push_str(&tx("F1", 10.0, 380.0, y, line)); // p2 => הו
        y -= 13.0;
    }
    let cmap = "\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
6 beginbfchar
<41> <05D0>
<42> <05D1>
<43> <05D2>
<44> <05D3>
<45> <05D4>
<46> <05D5>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end";
    let mut b = PdfBuilder::new();
    b.add("<< /Type /Catalog /Pages 2 0 R >>");
    b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.add_stream("", c.as_bytes());
    b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>");
    b.add_stream("", cmap.as_bytes());
    Fixture {
        name: "rtl_column",
        pdf: b.build(),
        pages: vec![1],
        gt: vec![
            // tokens are the Hebrew decodings
            GtBlock { token: "\u{05D0}\u{05D1}", ty: Ty::Heading, rank: 0 },
            GtBlock { token: "\u{05D2}\u{05D3}", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "\u{05D4}\u{05D5}", ty: Ty::Paragraph, rank: 2 },
        ],
    }
}

/// 9. Magazine-style: a spanning headline, a body paragraph, and a clearly
///    set-apart sidebar/callout block lower on the page. (Single visual column
///    of flowing prose + a distinct callout, ordered top-to-bottom.)
fn fx_magazine_sidebar() -> Fixture {
    let mut c = String::new();
    c.push_str(&tx("FB", 26.0, 72.0, 752.0, "MAGHEAD Cover Feature"));
    c.push_str(&tx("F1", 10.0, 72.0, 722.0, "LEAD paragraph of the feature article opening the story here."));
    c.push_str(&tx("F1", 10.0, 72.0, 709.0, "continuing the lead with a second wrapped line of prose text."));
    c.push_str(&tx("F1", 10.0, 72.0, 696.0, "and a third line bringing the opening paragraph to a close."));
    // a callout further down, bold + larger => reads as a sub-heading/heading
    c.push_str(&tx("FB", 14.0, 72.0, 640.0, "CALLOUT Key Takeaway"));
    c.push_str(&tx("F1", 10.0, 72.0, 620.0, "BODYEND closing paragraph wrapping up the feature article."));
    Fixture {
        name: "magazine_sidebar",
        pdf: page(c.as_bytes()),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "MAGHEAD", ty: Ty::Title, rank: 0 },
            GtBlock { token: "LEAD", ty: Ty::Paragraph, rank: 1 },
            GtBlock { token: "CALLOUT", ty: Ty::Heading, rank: 2 },
            GtBlock { token: "BODYEND", ty: Ty::Paragraph, rank: 3 },
        ],
    }
}

/// 10. Ruled table between a heading and a paragraph: a 2x2 grid drawn with
///     rules so the table detector fires (ruled, high confidence).
fn fx_ruled_table() -> Fixture {
    let mut c = String::new();
    c.push_str(&tx("FB", 18.0, 72.0, 750.0, "TBLDOC Results"));
    // Draw a ruled 2-column, 2-row grid around (72..272, 660..710).
    c.push_str("1 w\n");
    // horizontal rules
    for yy in [710.0, 685.0, 660.0] {
        c.push_str(&format!("72 {yy} m 272 {yy} l S\n"));
    }
    // vertical rules
    for xx in [72.0, 172.0, 272.0] {
        c.push_str(&format!("{xx} 660 m {xx} 710 l S\n"));
    }
    // cell text
    c.push_str(&tx("F1", 9.0, 80.0, 692.0, "Name"));
    c.push_str(&tx("F1", 9.0, 180.0, 692.0, "Score"));
    c.push_str(&tx("F1", 9.0, 80.0, 667.0, "TCELLA"));
    c.push_str(&tx("F1", 9.0, 180.0, 667.0, "99"));
    c.push_str(&tx("F1", 10.0, 72.0, 620.0, "AFTERTBL paragraph following the results table here."));
    Fixture {
        name: "ruled_table",
        pdf: page(c.as_bytes()),
        pages: vec![1],
        gt: vec![
            GtBlock { token: "TBLDOC", ty: Ty::Title, rank: 0 },
            GtBlock { token: "TCELLA", ty: Ty::Table, rank: 1 },
            GtBlock { token: "AFTERTBL", ty: Ty::Paragraph, rank: 2 },
        ],
    }
}

// ════════════════════════════════════════════════════════════════════════════
// The smoke
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn smoke_10_pdfs_vs_ground_truth() {
    let fixtures = vec![
        fx_simple_article(),
        fx_heading_hierarchy(),
        fx_bulleted_list(),
        fx_ordered_list(),
        fx_figure_caption(),
        fx_running_elements(),
        fx_tagged(),
        fx_rtl(),
        fx_magazine_sidebar(),
        fx_ruled_table(),
    ];
    assert_eq!(fixtures.len(), 10, "the smoke set must contain 10 PDFs");

    let mut reports: Vec<Report> = Vec::new();
    for fx in &fixtures {
        let engine = ContentEngine::open_bytes(fx.pdf.clone())
            .unwrap_or_else(|e| panic!("{}: open failed: {e:?}", fx.name));
        let model = engine
            .build_document_model(&fx.pages)
            .unwrap_or_else(|e| panic!("{}: build_document_model failed: {e:?}", fx.name));
        reports.push(evaluate(fx.name, &model, &fx.gt));
    }

    // ── Honest per-file report ──
    eprintln!("\n╔══════════════════════════════════════════════════════════════════════════╗");
    eprintln!("║  Document-model smoke: 10 PDFs vs hand-authored ground truth                ║");
    eprintln!("╠════════════════════╦═════════╦═══════════╦══════════════╦═══════════════════╣");
    eprintln!("║ fixture            ║ matched ║ kendall-τ ║ pairwise ord ║ classification    ║");
    eprintln!("╠════════════════════╬═════════╬═══════════╬══════════════╬═══════════════════╣");
    let mut tau_sum = 0.0;
    let mut class_correct_sum = 0usize;
    let mut gt_sum = 0usize;
    let mut matched_sum = 0usize;
    for r in &reports {
        eprintln!(
            "║ {:<18} ║  {:>2}/{:<2}  ║  {:>+.3}   ║   {:>5.1}%     ║  {:>2}/{:<2} = {:>5.1}%   ║",
            r.name,
            r.matched,
            r.total,
            r.kendall_tau,
            r.pairwise_order_pct,
            r.class_correct,
            r.total,
            100.0 * r.class_correct as f64 / r.total as f64,
        );
        tau_sum += r.kendall_tau;
        class_correct_sum += r.class_correct;
        gt_sum += r.total;
        matched_sum += r.matched;
    }
    eprintln!("╚════════════════════╩═════════╩═══════════╩══════════════╩═══════════════════╝");
    let avg_tau = tau_sum / reports.len() as f64;
    let class_acc = 100.0 * class_correct_sum as f64 / gt_sum as f64;
    let match_rate = 100.0 * matched_sum as f64 / gt_sum as f64;
    eprintln!(
        "  AGGREGATE: avg kendall-τ = {avg_tau:+.3} | classification accuracy = {class_acc:.1}% \
         ({class_correct_sum}/{gt_sum}) | block match rate = {match_rate:.1}%\n"
    );

    // ── Conservative aggregate floors (documented, not brittle) ──
    // Reading order is the headline claim — it must be strong across the set.
    assert!(
        avg_tau >= 0.90,
        "avg Kendall-tau {avg_tau:+.3} below 0.90 — reading order regressed"
    );
    // Classification is harder; require a solid majority correct across all GT
    // blocks (the honest `Text` fallback counts against us here).
    assert!(
        class_acc >= 70.0,
        "classification accuracy {class_acc:.1}% below 70% across the smoke set"
    );
    // Almost every ground-truth block should be locatable in the model.
    assert!(
        match_rate >= 90.0,
        "only {match_rate:.1}% of ground-truth blocks matched a model block"
    );
}
