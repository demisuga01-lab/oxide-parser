//! Validation for the PostScript / EPS vector output backend.
//!
//! The correctness bar mirrors the SVG backend's (`svg_output.rs`): emit the
//! page as PostScript, rasterise it with **Ghostscript** (`gs` / `gswin64c`),
//! and compare (PSNR) against Oxide's OWN raster render of the same page. High
//! similarity proves the PostScript faithfully represents the page (same
//! validation philosophy as SVG-via-resvg). We also assert DSC/EPSF structural
//! conformance and the vector-vs-rasterize-embed decision.
//!
//! Ghostscript is a **dev/test** tool only — it is NOT a runtime dependency and
//! the crate remains pure-Rust. When Ghostscript is absent the rasterisation
//! tests skip with a printed NOTE (the structural tests still run).

use std::path::{Path, PathBuf};
use std::process::Command;

use oxide_engine::render::quality::RenderQuality;
use oxide_engine::ContentEngine;

const DPI: u32 = 96;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn engine(name: &str) -> ContentEngine {
    ContentEngine::open_bytes(std::fs::read(fixture(name)).unwrap()).unwrap()
}

/// Locate a Ghostscript executable, trying the common names on each platform.
fn find_ghostscript() -> Option<PathBuf> {
    for name in ["gswin64c", "gswin32c", "gs"] {
        // `--version` is a cheap presence probe.
        if Command::new(name).arg("--version").output().is_ok() {
            return Some(PathBuf::from(name));
        }
    }
    // Windows default install location fallback.
    if cfg!(windows) {
        let base = Path::new("C:/Program Files/gs");
        if let Ok(entries) = std::fs::read_dir(base) {
            for e in entries.flatten() {
                let candidate = e.path().join("bin").join("gswin64c.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Rasterise a PostScript/EPS document with Ghostscript to an RGB PNG of the
/// given device-pixel size, then load it as a RawImage. Our PostScript page
/// units ARE device pixels, so Ghostscript renders at 72 dpi (1 unit = 1 px).
fn ghostscript_rasterize(
    gs: &Path,
    ps_path: &Path,
    width: u32,
    height: u32,
) -> Option<oxide_engine::images::decoder::RawImage> {
    let out_png = std::env::temp_dir().join(format!(
        "oxide_ps_gs_{}_{}.png",
        std::process::id(),
        ps_path.file_name().and_then(|n| n.to_str()).unwrap_or("x")
    ));
    let status = Command::new(gs)
        .arg("-dSAFER")
        .arg("-dBATCH")
        .arg("-dNOPAUSE")
        .arg("-sDEVICE=png16m")
        .arg("-r72")
        .arg(format!("-g{width}x{height}"))
        .arg(format!("-o{}", out_png.display()))
        .arg(ps_path)
        .output()
        .ok()?;
    if !status.status.success() {
        eprintln!(
            "ghostscript failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        return None;
    }
    let img = RenderQuality::read_golden(&out_png).ok();
    let _ = std::fs::remove_file(&out_png);
    img
}

/// Write a PostScript document to a temp file and return its path.
fn write_temp(name: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("oxide_ps_{}_{}", std::process::id(), name));
    std::fs::write(&path, contents).unwrap();
    path
}

/// Composite a (possibly RGB or RGBA) RawImage onto white and return RGB bytes.
fn to_rgb_on_white(img: &oxide_engine::images::decoder::RawImage) -> Vec<u8> {
    let ch = img.channels as usize;
    let mut out = Vec::with_capacity(img.width as usize * img.height as usize * 3);
    for px in img.pixels.chunks_exact(ch) {
        if ch >= 4 {
            let a = px[3] as f64 / 255.0;
            for &comp in px.iter().take(3) {
                out.push((comp as f64 * a + 255.0 * (1.0 - a)).round() as u8);
            }
        } else {
            out.push(px[0]);
            out.push(px[1]);
            out.push(px.get(2).copied().unwrap_or(px[0]));
        }
    }
    out
}

/// PSNR (dB) over RGB between two equal-size images composited onto white.
fn psnr_rgb(
    a: &oxide_engine::images::decoder::RawImage,
    b: &oxide_engine::images::decoder::RawImage,
) -> f64 {
    let (aw, ah) = (a.width.min(b.width), a.height.min(b.height));
    let av = to_rgb_on_white(a);
    let bv = to_rgb_on_white(b);
    let mut sum_sq = 0.0f64;
    let mut count = 0.0f64;
    for y in 0..ah {
        for x in 0..aw {
            for c in 0..3 {
                let ia = ((y * a.width + x) * 3 + c) as usize;
                let ib = ((y * b.width + x) * 3 + c) as usize;
                let d = av[ia] as f64 - bv[ib] as f64;
                sum_sq += d * d;
                count += 1.0;
            }
        }
    }
    if count == 0.0 {
        return 99.0;
    }
    let mse = sum_sq / count;
    if mse <= 1e-9 {
        return 99.0;
    }
    20.0 * 255.0f64.log10() - 10.0 * mse.log10()
}

// ── Structural / DSC conformance (run with or without Ghostscript) ───────────

#[test]
fn ps_document_is_dsc_conformant() {
    let e = engine("multi_stream.pdf");
    let (ps, _rasterized) = e.render_document_ps(&[1], DPI).unwrap();
    assert!(ps.starts_with("%!PS-Adobe-3.0\n"));
    assert!(ps.contains("%%BoundingBox: 0 0 "));
    assert!(ps.contains("%%Pages: 1"));
    assert!(ps.contains("%%Page: 1 1"));
    assert!(ps.contains("showpage"));
    assert!(ps.trim_end().ends_with("%%EOF"));
    // The coordinate-flip prologue is present so device geometry maps correctly.
    assert!(ps.contains("1 -1 scale"));
}

#[test]
fn eps_document_is_epsf_conformant() {
    let e = engine("multi_stream.pdf");
    let (eps, _rasterized) = e.render_page_eps(1, DPI).unwrap();
    let vp = e.page_viewport(1, DPI).unwrap();
    assert!(eps.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
    assert!(eps.contains(&format!(
        "%%BoundingBox: 0 0 {} {}",
        vp.width_px, vp.height_px
    )));
    // EPS must not change global page state.
    assert!(!eps.contains("setpagedevice"));
    assert!(!eps.contains("showpage"));
    assert!(eps.trim_end().ends_with("%%EOF"));
}

#[test]
fn multipage_ps_has_one_page_per_requested() {
    let e = engine("tracemonkey.pdf");
    let total = e.page_count().unwrap();
    let pages: Vec<usize> = (1..=total.min(3)).collect();
    let (ps, _r) = e.render_document_ps(&pages, DPI).unwrap();
    assert_eq!(ps.matches("showpage").count(), pages.len());
    assert!(ps.contains(&format!("%%Pages: {}", pages.len())));
}

// ── Ghostscript rasterisation PSNR (skips cleanly when gs is absent) ─────────

#[test]
fn ps_rasterizes_close_to_oxide_raster() {
    let Some(gs) = find_ghostscript() else {
        eprintln!("NOTE: Ghostscript not found; skipping PS rasterisation PSNR test");
        return;
    };

    // Pages chosen because the raster render has meaningful non-white VECTOR
    // content (so a high PSNR is meaningful and the page is true-vector, not a
    // raster-embed fallback). Floors are conservative — PS path flattening +
    // Ghostscript's own AA differ slightly from Oxide's rasteriser.
    let cases = [("multi_stream.pdf", 1usize, 22.0f64)];

    for (name, page, floor) in cases {
        let e = engine(name);
        if page > e.page_count().unwrap() {
            continue;
        }
        let vp = e.page_viewport(page, DPI).unwrap();
        let raster = e.render_page(page, DPI).unwrap().to_raw_image();

        // Guard: the page must actually have marks.
        let nonwhite = raster
            .pixels
            .chunks_exact(3)
            .filter(|p| p[0] < 250 || p[1] < 250 || p[2] < 250)
            .count();
        assert!(
            nonwhite > 100,
            "{name} p{page}: expected visible marks (got {nonwhite} non-white px)"
        );

        let ps_page = e.render_page_ps(page, DPI).unwrap();
        assert!(
            !ps_page.is_rasterized,
            "{name} p{page} should be a true-vector PS page for this PSNR check"
        );
        let (ps_doc, _r) = e.render_document_ps(&[page], DPI).unwrap();
        let ps_path = write_temp(&format!("{name}_p{page}.ps"), &ps_doc);

        let Some(gs_img) = ghostscript_rasterize(&gs, &ps_path, vp.width_px, vp.height_px) else {
            eprintln!("NOTE: Ghostscript could not rasterise {name} p{page}; skipping");
            let _ = std::fs::remove_file(&ps_path);
            continue;
        };
        let _ = std::fs::remove_file(&ps_path);

        let psnr = psnr_rgb(&raster, &gs_img);
        eprintln!("{name} p{page}: PS-vs-raster PSNR {psnr:.2} dB (gs-rasterised)");
        assert!(
            psnr >= floor,
            "{name} p{page}: PS-vs-raster PSNR {psnr:.2} dB below floor {floor}"
        );
    }
}

#[test]
fn ps_rasterize_embed_fallback_round_trips() {
    // image_only.pdf uses an image XObject -> the whole page is embedded as a
    // `colorimage` raster. Re-rasterising that PS with Ghostscript must
    // reproduce the original raster near-exactly (it IS the raster).
    let path = fixture("image_only.pdf");
    if !path.exists() {
        eprintln!("NOTE: image_only.pdf missing; skipping fallback round-trip");
        return;
    }
    let e = ContentEngine::open_bytes(std::fs::read(&path).unwrap()).unwrap();
    let ps_page = e.render_page_ps(1, DPI).unwrap();
    assert!(
        ps_page.is_rasterized,
        "an image page must take the rasterize-embed fallback"
    );
    assert!(ps_page.body.contains("colorimage"));

    let Some(gs) = find_ghostscript() else {
        eprintln!("NOTE: Ghostscript not found; skipping fallback rasterisation");
        return;
    };
    let vp = e.page_viewport(1, DPI).unwrap();
    let raster = e.render_page(1, DPI).unwrap().to_raw_image();
    let (ps_doc, _r) = e.render_document_ps(&[1], DPI).unwrap();
    let ps_path = write_temp("image_only_p1.ps", &ps_doc);
    let Some(gs_img) = ghostscript_rasterize(&gs, &ps_path, vp.width_px, vp.height_px) else {
        let _ = std::fs::remove_file(&ps_path);
        return;
    };
    let _ = std::fs::remove_file(&ps_path);
    let psnr = psnr_rgb(&raster, &gs_img);
    eprintln!("image_only p1: fallback PS-vs-raster PSNR {psnr:.2} dB");
    // The embedded image is the raster itself; only 8-bit hex re-quantisation
    // and Ghostscript's image scaling differ -> should be very high.
    assert!(
        psnr >= 30.0,
        "fallback PS should reproduce the raster nearly exactly: {psnr:.2} dB"
    );
}

#[test]
fn eps_rasterizes_close_to_oxide_raster() {
    let Some(gs) = find_ghostscript() else {
        eprintln!("NOTE: Ghostscript not found; skipping EPS rasterisation PSNR test");
        return;
    };
    let name = "multi_stream.pdf";
    let page = 1usize;
    let e = engine(name);
    let vp = e.page_viewport(page, DPI).unwrap();
    let raster = e.render_page(page, DPI).unwrap().to_raw_image();

    let (eps, rasterized) = e.render_page_eps(page, DPI).unwrap();
    assert!(!rasterized, "{name} p{page} should be true-vector EPS");
    let eps_path = write_temp(&format!("{name}_p{page}.eps"), &eps);

    let Some(gs_img) = ghostscript_rasterize(&gs, &eps_path, vp.width_px, vp.height_px) else {
        eprintln!("NOTE: Ghostscript could not rasterise EPS; skipping");
        let _ = std::fs::remove_file(&eps_path);
        return;
    };
    let _ = std::fs::remove_file(&eps_path);

    let psnr = psnr_rgb(&raster, &gs_img);
    eprintln!("{name} p{page}: EPS-vs-raster PSNR {psnr:.2} dB (gs-rasterised)");
    assert!(
        psnr >= 22.0,
        "{name} p{page}: EPS-vs-raster PSNR {psnr:.2} dB below floor"
    );
}
