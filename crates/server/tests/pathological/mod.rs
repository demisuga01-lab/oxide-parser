//! Generators for pathological-but-WELL-FORMED PDFs.
//!
//! These are valid PDFs that are merely abusive in *scale* or *structure* —
//! distinct from the malformed fuzz inputs of the parser-hardening round. Each
//! is built programmatically so the abusive property is self-documenting and
//! the fixture is reproducible. They exercise the server's resource-safety
//! guarantees: per-request timeout (cooperative cancellation) and resource
//! limits (pixel/output/decompression caps).
//!
//! Included via `#[path = ...] mod pathological;` from the integration test.

#![allow(dead_code)]

use std::io::Write;

/// Assemble a single-page PDF given object bodies, a page MediaBox, and a
/// content stream. Writes a correct classic xref table so the document parses
/// cleanly — the whole point is that these are VALID PDFs.
fn assemble(
    media_box: &str,
    content: &[u8],
    extra_page_dict: &str,
    extra_objects: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    // Object numbering:
    //   1 = Catalog, 2 = Pages, 3 = Page, 4 = Contents stream, 5.. = extras
    let mut objects: Vec<(u32, Vec<u8>)> = Vec::new();

    objects.push((1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()));
    objects.push((2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()));

    let page_dict = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [{}] /Contents 4 0 R {} >>",
        media_box, extra_page_dict
    );
    objects.push((3, page_dict.into_bytes()));

    let mut contents = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    contents.extend_from_slice(content);
    contents.extend_from_slice(b"\nendstream");
    objects.push((4, contents));

    for (num, body) in extra_objects {
        objects.push((*num, body.clone()));
    }

    objects.sort_by_key(|(num, _)| *num);
    let max_num = objects.last().map(|(n, _)| *n).unwrap_or(0);

    let mut pdf: Vec<u8> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n");
    let mut offsets = vec![0usize; (max_num + 1) as usize];
    for (num, body) in &objects {
        offsets[*num as usize] = pdf.len();
        pdf.extend_from_slice(format!("{} 0 obj\n", num).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = pdf.len();
    let size = max_num + 1;
    pdf.extend_from_slice(format!("xref\n0 {}\n", size).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for num in 1..size {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offsets[num as usize]).as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            size, xref_offset
        )
        .as_bytes(),
    );
    pdf
}

/// A page whose content stream repeatedly fills the full page rectangle. Each
/// fill rasterizes the whole (DPI-scaled) page, so a large operator count
/// produces a very expensive render from a small file. The page is kept
/// moderate (400pt) so each individual fill is cheap enough that the engine's
/// per-64-operator cancellation check fires sub-second, while the sheer count
/// makes the total work vastly exceed any short timeout. Parses fine and isn't
/// malformed — it just takes a long time, exercising cooperative cancellation.
pub fn huge_operator_count_pdf(op_count: usize) -> Vec<u8> {
    let mut content: Vec<u8> = Vec::new();
    for i in 0..op_count {
        let g = (i % 100) as f64 / 100.0;
        content.extend_from_slice(format!("{:.2} g\n", g).as_bytes());
        content.extend_from_slice(b"0 0 400 400 re\nf\n");
    }
    assemble(
        "0 0 400 400",
        &content,
        "/Resources << /ProcSet [/PDF] >>",
        &[],
    )
}

/// A page with a tiling pattern whose XStep/YStep are tiny relative to a large
/// fill area, so the naive tile count is enormous. Exercises the 20k-tile cap
/// and the per-tile cancellation check.
pub fn pathological_tiling_pattern_pdf() -> Vec<u8> {
    // Pattern content: fill the 1x1 cell.
    let pat_content: &[u8] = b"0 0 1 1 re\nf\n";
    let pattern = format!(
        "<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
         /BBox [0 0 1 1] /XStep 1 /YStep 1 \
         /Resources << /ProcSet [/PDF] >> /Length {} >>\nstream\n",
        pat_content.len()
    );
    let mut pattern_obj = pattern.into_bytes();
    pattern_obj.extend_from_slice(pat_content);
    pattern_obj.extend_from_slice(b"\nendstream");

    // Page: select the pattern as fill color space, then fill a huge area so the
    // (area / step) tile count is astronomically larger than the 20k cap.
    let content: &[u8] =
        b"/Pattern cs\n/P0 scn\n0 0 100000 100000 re\nf\n";

    assemble(
        "0 0 5000 5000",
        content,
        "/Resources << /Pattern << /P0 5 0 R >> /ProcSet [/PDF] >>",
        &[(5, pattern_obj)],
    )
}

/// A page with an enormous MediaBox. At a normal DPI the rendered pixel count
/// (width_px * height_px) explodes well past the 100 MP cap, exercising the
/// pre-allocation pixel guard. Sized so that at 150 DPI it is ~156 MP (over the
/// cap, rejected) while at the 24 DPI floor it is only ~4 MP (under the cap,
/// and small enough to render quickly even in a debug build).
pub fn giant_mediabox_pdf() -> Vec<u8> {
    // 6000pt square. 150 DPI: (6000/72*150)^2 = 12500^2 = 156 MP (> 100 MP cap).
    // 24 DPI: (6000/72*24)^2 = 2000^2 = 4 MP (< cap, fast to render).
    let content: &[u8] = b"0 0 100 100 re\nf\n";
    assemble(
        "0 0 6000 6000",
        content,
        "/Resources << /ProcSet [/PDF] >>",
        &[],
    )
}

/// Deeply nested Form XObjects: Fm0 -> Fm1 -> ... -> FmN, exceeding the
/// engine's depth-8 recursion guard. Confirms the guard holds and the page
/// still renders (clean degradation, not a stack overflow).
pub fn deeply_nested_forms_pdf(depth: u32) -> Vec<u8> {
    let mut extras: Vec<(u32, Vec<u8>)> = Vec::new();

    // Form object numbers start at 5. Form k (0-indexed) is object 5+k and
    // invokes Form k+1 via /Fm(k+1), except the last which just paints.
    for k in 0..depth {
        let obj_num = 5 + k;
        let content: Vec<u8> = if k + 1 < depth {
            let child = 5 + k + 1;
            format!("q /Fm{} Do Q\n", child).into_bytes()
        } else {
            b"0 0 10 10 re f\n".to_vec()
        };
        let resources = if k + 1 < depth {
            let child = 5 + k + 1;
            format!(
                "/Resources << /XObject << /Fm{} {} 0 R >> /ProcSet [/PDF] >>",
                k + 1,
                child
            )
        } else {
            "/Resources << /ProcSet [/PDF] >>".to_string()
        };
        let mut body = format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] {} /Length {} >>\nstream\n",
            resources,
            content.len()
        )
        .into_bytes();
        body.extend_from_slice(&content);
        body.extend_from_slice(b"\nendstream");
        extras.push((obj_num, body));
    }

    let content: &[u8] = b"q /Fm0 Do Q\n";
    assemble(
        "0 0 100 100",
        content,
        "/Resources << /XObject << /Fm0 5 0 R >> /ProcSet [/PDF] >>",
        &extras,
    )
}

/// A Form XObject that references ITSELF (a direct cycle). Confirms cycle/depth
/// handling prevents infinite recursion and the page still completes.
pub fn self_referential_form_pdf() -> Vec<u8> {
    // Object 5 is a Form whose content invokes /Fm0, and whose own Resources
    // map /Fm0 back to object 5 — an A->A cycle.
    let content: &[u8] = b"q /Fm0 Do Q\n0 0 10 10 re f\n";
    let mut body = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
         /Resources << /XObject << /Fm0 5 0 R >> /ProcSet [/PDF] >> /Length {} >>\nstream\n",
        content.len()
    )
    .into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(b"\nendstream");

    let page_content: &[u8] = b"q /Fm0 Do Q\n";
    assemble(
        "0 0 100 100",
        page_content,
        "/Resources << /XObject << /Fm0 5 0 R >> /ProcSet [/PDF] >>",
        &[(5, body)],
    )
}

/// A FlateDecode stream that decompresses to `decompressed_len` bytes from a
/// tiny compressed input — a decompression bomb. Returns the raw deflate-zlib
/// compressed bytes (not a full PDF); used to unit-test the filter cap directly.
pub fn flate_bomb_compressed(decompressed_len: usize) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    // A long run of identical bytes compresses to almost nothing but inflates
    // to `decompressed_len`.
    let raw = vec![0u8; decompressed_len];
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap()
}

/// A many-page PDF: `page_count` near-identical trivial pages. Exercises the
/// page-cap enforcement and per-page timeout behavior across many pages.
pub fn many_pages_pdf(page_count: usize) -> Vec<u8> {
    let content: &[u8] = b"0 0 10 10 re\nf\n";

    // Object layout: 1=Catalog, 2=Pages, then for each page two objects
    // (the Page dict and its Contents stream).
    let mut objects: Vec<(u32, Vec<u8>)> = Vec::new();
    objects.push((1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()));

    let mut kids = String::new();
    let mut next = 3u32;
    let mut page_objs: Vec<(u32, Vec<u8>)> = Vec::new();
    for _ in 0..page_count {
        let page_num = next;
        let contents_num = next + 1;
        next += 2;
        kids.push_str(&format!("{} 0 R ", page_num));
        let page_dict = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Contents {} 0 R /Resources << /ProcSet [/PDF] >> >>",
            contents_num
        );
        page_objs.push((page_num, page_dict.into_bytes()));
        let mut c = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        c.extend_from_slice(content);
        c.extend_from_slice(b"\nendstream");
        page_objs.push((contents_num, c));
    }

    objects.push((
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.trim_end(),
            page_count
        )
        .into_bytes(),
    ));
    objects.extend(page_objs);
    objects.sort_by_key(|(num, _)| *num);
    let max_num = objects.last().map(|(n, _)| *n).unwrap_or(0);

    let mut pdf: Vec<u8> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n");
    let mut offsets = vec![0usize; (max_num + 1) as usize];
    for (num, body) in &objects {
        offsets[*num as usize] = pdf.len();
        pdf.extend_from_slice(format!("{} 0 obj\n", num).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    let size = max_num + 1;
    pdf.extend_from_slice(format!("xref\n0 {}\n", size).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for num in 1..size {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offsets[num as usize]).as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            size, xref_offset
        )
        .as_bytes(),
    );
    pdf
}
