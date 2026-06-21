//! Regression tests for Renderer Benchmark 0A fixes.
//!
//! Part B: a hostile page declaring a giant `/MediaBox` must be rejected with a
//! clean [`OxideError::ResourceLimit`] BEFORE any pixel buffer is allocated —
//! never a multi-hundred-gigabyte allocation that aborts the process.
//!
//! Part C: the effective page box used for sizing is the CropBox (MediaBox ∩
//! CropBox), matching `pdftoppm -cropbox`, pdfinfo's reported page size, and the
//! common viewer default. These tests lock in the dimensions so a future change
//! to box selection cannot silently regress the benchmark's apples-to-apples
//! comparison.

use oxide_engine::{ContentEngine, OxideError};

/// Build a single-page PDF with the given page-dictionary box/rotate entries and
/// a trivial content stream. `extra` is spliced into the `/Page` dictionary
/// (e.g. `/MediaBox [...] /CropBox [...] /Rotate 270`).
fn one_page_pdf(extra: &str) -> Vec<u8> {
    let content = b"BT /F1 20 Tf 72 700 Td (hi) Tj ET\n";
    let mut objs: Vec<String> = Vec::new();
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
    objs.push(format!(
        "<< /Length {} >>\nstream\n{}endstream",
        content.len(),
        std::str::from_utf8(content).unwrap()
    ));
    objs.push(format!(
        "<< /Type /Page /Parent 4 0 R {} /Resources << /Font << /F1 1 0 R >> >> /Contents 2 0 R >>",
        extra
    ));
    objs.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string());
    objs.push("<< /Type /Catalog /Pages 4 0 R >>".to_string());

    let mut pdf = String::from("%PDF-1.7\n");
    let mut offsets = vec![0usize];
    for (i, body) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
    }
    let xref_off = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
    for off in &offsets[1..] {
        pdf.push_str(&format!("{:010} 00000 n \n", off));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 5 0 R >>\nstartxref\n{}\n%%EOF\n",
        objs.len() + 1,
        xref_off
    ));
    pdf.into_bytes()
}

#[test]
fn huge_mediabox_is_rejected_cleanly_not_aborted() {
    // /MediaBox [0 0 200000 200000] at 144 DPI = 400000x400000 px = 1.6e11
    // pixels, whose 4-byte buffer would be ~640 GB. This must surface a clean
    // error, not attempt the allocation (which aborts the process).
    let pdf = one_page_pdf("/MediaBox [0 0 200000 200000]");
    let engine = ContentEngine::open_bytes(pdf).expect("open");

    let err = engine
        .render_page(1, 144)
        .expect_err("a 640 GB page must be rejected, not rendered");
    match err {
        OxideError::ResourceLimit(msg) => {
            assert!(
                msg.contains("pixels") && msg.contains("exceeding the limit"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected ResourceLimit, got {other:?}"),
    }

    // The engine must remain usable for the next page/file after the rejection.
    let ok_pdf = one_page_pdf("/MediaBox [0 0 612 792]");
    let ok_engine = ContentEngine::open_bytes(ok_pdf).expect("open second");
    let buf = ok_engine.render_page(1, 144).expect("normal page renders");
    assert_eq!(buf.width, 1224);
    assert_eq!(buf.height, 1584);
}

#[test]
fn huge_mediabox_rejected_at_viewport_before_allocation() {
    // page_viewport is the single chokepoint that every render/svg/ps path uses,
    // so it must reject without constructing the buffer.
    let pdf = one_page_pdf("/MediaBox [0 0 200000 200000]");
    let engine = ContentEngine::open_bytes(pdf).expect("open");
    assert!(matches!(
        engine.page_viewport(1, 144),
        Err(OxideError::ResourceLimit(_))
    ));
}

#[test]
fn extreme_dpi_on_normal_page_is_capped() {
    // A normal Letter page at an absurd DPI also crosses the cap and must be
    // rejected cleanly rather than allocating. 612x792 pt at 50000 DPI =
    // 425000 x 550000 px ~= 2.3e11 pixels.
    let pdf = one_page_pdf("/MediaBox [0 0 612 792]");
    let engine = ContentEngine::open_bytes(pdf).expect("open");
    assert!(matches!(
        engine.page_viewport(1, 50_000),
        Err(OxideError::ResourceLimit(_))
    ));
}

#[test]
fn oversized_real_world_page_is_capped_at_default_limit() {
    // pdf.js issue19517-class: a one-page image PDF with a 12608x16806 pt page
    // would allocate ~212 MP even at 72 DPI. That is too large for the default
    // CLI/harness memory envelope and must fail before PixelBuffer allocation.
    let pdf = one_page_pdf("/MediaBox [0 0 12608 16806]");
    let engine = ContentEngine::open_bytes(pdf).expect("open");
    assert!(matches!(
        engine.page_viewport(1, 72),
        Err(OxideError::ResourceLimit(_))
    ));
}

#[test]
fn cropbox_drives_page_dimensions_matching_poppler_cropbox() {
    // bug1802506-class file: MediaBox 612x792, CropBox 267.75x145.5. Oxide
    // renders the CropBox (= pdftoppm -cropbox), which at 144 DPI is
    // 267.75*2 = 535.5 -> 536 wide, 145.5*2 = 291 tall.
    let pdf = one_page_pdf("/MediaBox [0 0 612 792] /CropBox [110.25 472.5 378 618]");
    let engine = ContentEngine::open_bytes(pdf).expect("open");
    let vp = engine.page_viewport(1, 144).expect("viewport");
    assert_eq!((vp.width_px, vp.height_px), (536, 291));
}

#[test]
fn rotated_cropbox_swaps_dimensions() {
    // synthetic_geometry_rotate_*: MediaBox 612x792, CropBox 572x752 (=
    // [20 20 592 772]), /Rotate 270. CropBox at 144 DPI = 1144x1504; the 270°
    // rotation swaps to 1504x1144 (matches pdftoppm -cropbox exactly).
    let pdf = one_page_pdf("/MediaBox [0 0 612 792] /CropBox [20 20 592 772] /Rotate 270");
    let engine = ContentEngine::open_bytes(pdf).expect("open");
    let vp = engine.page_viewport(1, 144).expect("viewport");
    assert_eq!((vp.width_px, vp.height_px), (1504, 1144));
}
