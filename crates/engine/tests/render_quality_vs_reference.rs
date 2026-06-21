//! Optional render-quality measurements.
//!
//! Default rendering stays in `RenderMode::Compat`, which composites in sRGB
//! byte space to track Poppler/Splash. `RenderMode::HighQuality` is opt-in and
//! keeps the same geometry and analytic AA coverage while compositing RGB in
//! linear light. The asserted test uses the same high-resolution HighQuality
//! linear-light downsample reference as the Prompt I measurement script.
//! Ghostscript is used only for an informational external-renderer report.
//!
//! The full Prompt I three-way Compat/High/Poppler measurement lives in
//! `scripts/render_quality_prompt_i.py` and writes:
//! - `docs/render_quality_prompt_i_results.json`
//! - `docs/render_quality_prompt_i_summary.md`

use std::path::{Path, PathBuf};
use std::process::Command;

use oxide_engine::images::decoder::RawImage;
use oxide_engine::render::quality::RenderQuality;
use oxide_engine::{ContentEngine, RenderMode};

const DPI: u32 = 72;
const SS: u32 = 4;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn find_ghostscript() -> Option<PathBuf> {
    for name in ["gswin64c", "gswin32c", "gs"] {
        if Command::new(name).arg("--version").output().is_ok() {
            return Some(PathBuf::from(name));
        }
    }
    if cfg!(windows) {
        if let Ok(entries) = std::fs::read_dir("C:/Program Files/gs") {
            for e in entries.flatten() {
                let c = e.path().join("bin").join("gswin64c.exe");
                if c.exists() {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn gs_render(gs: &Path, pdf: &Path, dpi: u32) -> Option<RawImage> {
    let out = std::env::temp_dir().join(format!(
        "oxide_ref_{}_{}_{}.png",
        std::process::id(),
        dpi,
        pdf.file_stem().and_then(|s| s.to_str()).unwrap_or("x")
    ));
    let status = Command::new(gs)
        .arg("-dSAFER")
        .arg("-dBATCH")
        .arg("-dNOPAUSE")
        .arg("-dFirstPage=1")
        .arg("-dLastPage=1")
        .arg("-sDEVICE=png16m")
        .arg("-dTextAlphaBits=4")
        .arg("-dGraphicsAlphaBits=4")
        .arg(format!("-r{dpi}"))
        .arg(format!("-o{}", out.display()))
        .arg(pdf)
        .output()
        .ok()?;
    if !status.status.success() {
        return None;
    }
    let img = RenderQuality::read_golden(&out).ok();
    let _ = std::fs::remove_file(&out);
    img
}

fn srgb_to_linear(v: u8) -> f64 {
    let c = f64::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

fn downsample_linear_light(img: &RawImage, factor: u32) -> RawImage {
    let factor = factor.max(1);
    let nw = img.width / factor;
    let nh = img.height / factor;
    let channels = usize::from(img.channels);
    let mut out = vec![0u8; (nw * nh) as usize * 3];
    let samples = f64::from(factor * factor);
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0.0f64; 3];
            for dy in 0..factor {
                for dx in 0..factor {
                    let sx = x * factor + dx;
                    let sy = y * factor + dy;
                    let idx = ((sy * img.width + sx) as usize) * channels;
                    for (c, slot) in acc.iter_mut().enumerate() {
                        *slot += srgb_to_linear(img.pixels[idx + c]);
                    }
                }
            }
            let o = ((y * nw + x) as usize) * 3;
            for (c, &a) in acc.iter().enumerate() {
                out[o + c] = linear_to_srgb(a / samples);
            }
        }
    }
    RawImage {
        width: nw,
        height: nh,
        channels: 3,
        bits_per_sample: 8,
        pixels: out,
    }
}

fn crop(img: &RawImage, width: u32, height: u32) -> RawImage {
    let width = width.min(img.width);
    let height = height.min(img.height);
    let channels = usize::from(img.channels);
    let mut out = vec![0u8; (width * height) as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let si = ((y * img.width + x) as usize) * channels;
            let di = ((y * width + x) as usize) * 3;
            out[di..di + 3].copy_from_slice(&img.pixels[si..si + 3]);
        }
    }
    RawImage {
        width,
        height,
        channels: 3,
        bits_per_sample: 8,
        pixels: out,
    }
}

fn psnr(a: &RawImage, b: &RawImage) -> f64 {
    let width = a.width.min(b.width);
    let height = a.height.min(b.height);
    let ca = crop(a, width, height);
    let cb = crop(b, width, height);
    RenderQuality::psnr(&ca, &cb).unwrap_or(0.0)
}

fn aa_pixel_count(img: &RawImage) -> usize {
    img.pixels
        .chunks_exact(img.channels as usize)
        .filter(|p| {
            let v = p[0];
            v > 24 && v < 231
        })
        .count()
}

#[test]
fn high_quality_mode_is_closer_to_linear_light_reference_than_compat() {
    let cases = ["basicapi.pdf", "flate.pdf"];
    let mut improved = 0;
    let mut measured = 0;

    for pdf_name in cases {
        let pdf = fixture(pdf_name);
        if !pdf.exists() {
            eprintln!("SKIP {pdf_name}: missing fixture");
            continue;
        }
        let engine = ContentEngine::open_path(&pdf).unwrap();

        let hi = engine
            .render_page_with_mode(1, DPI * SS, RenderMode::HighQuality)
            .unwrap()
            .to_raw_image();
        let reference = downsample_linear_light(&hi, SS);

        let compat = engine
            .render_page_with_mode(1, DPI, RenderMode::Compat)
            .unwrap()
            .to_raw_image();
        let high = engine
            .render_page_with_mode(1, DPI, RenderMode::HighQuality)
            .unwrap()
            .to_raw_image();

        let high_psnr = psnr(&high, &reference);
        let compat_psnr = psnr(&compat, &reference);
        let high_aa = aa_pixel_count(&high);
        let compat_aa = aa_pixel_count(&compat);

        measured += 1;
        if high_psnr >= compat_psnr {
            improved += 1;
        }
        eprintln!(
            "{pdf_name}: High vs ref = {high_psnr:.2} dB | Compat vs ref = {compat_psnr:.2} dB | delta = {:+.2} dB | AA px high/compat = {high_aa}/{compat_aa}",
            high_psnr - compat_psnr
        );
    }

    if measured == 0 {
        eprintln!("NOTE: no cases measured");
        return;
    }
    assert_eq!(
        improved, measured,
        "HighQuality should be closer to the linear-light reference on every measured case ({improved}/{measured})"
    );
}

#[test]
fn compat_and_high_vs_ghostscript_reference_psnr_is_reported() {
    let Some(gs) = find_ghostscript() else {
        eprintln!("NOTE: Ghostscript not found; skipping cross-renderer report");
        return;
    };

    for pdf_name in ["basicapi.pdf", "tracemonkey.pdf"] {
        let pdf = fixture(pdf_name);
        if !pdf.exists() {
            continue;
        }
        let engine = ContentEngine::open_path(&pdf).unwrap();
        let compat = engine
            .render_page_with_mode(1, DPI, RenderMode::Compat)
            .unwrap()
            .to_raw_image();
        let high = engine
            .render_page_with_mode(1, DPI, RenderMode::HighQuality)
            .unwrap()
            .to_raw_image();
        if let Some(gs_img) = gs_render(&gs, &pdf, DPI) {
            let compat_psnr = psnr(&compat, &gs_img);
            let high_psnr = psnr(&high, &gs_img);
            eprintln!(
                "{pdf_name}: Compat vs Ghostscript@{DPI}dpi = {compat_psnr:.2} dB | High = {high_psnr:.2} dB"
            );
        }
    }
}
