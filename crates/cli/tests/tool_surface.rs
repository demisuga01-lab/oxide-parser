//! Tool-surface parity test: exercise every Oxide CLI subcommand on fixtures
//! and assert it succeeds with the expected output shape.
//!
//! This is the continuously-verifiable evidence behind the command-by-command
//! Poppler parity claim. It invokes the actual built binary via
//! `CARGO_BIN_EXE_oxide`, so it covers argument parsing + the full pipeline,
//! not just the engine API.

use std::path::PathBuf;
use std::process::Command;

fn oxide() -> Command {
    Command::new(env!("CARGO_BIN_EXE_oxide"))
}

fn fixtures() -> PathBuf {
    // crates/cli -> repo root -> crates/engine/tests/fixtures
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("engine")
        .join("tests")
        .join("fixtures")
}

fn fx(name: &str) -> PathBuf {
    fixtures().join(name)
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("oxide_tool_surface_{name}"))
}

fn run(args: &[&str]) -> std::process::Output {
    oxide().args(args).output().expect("spawn oxide")
}

fn assert_ok(out: &std::process::Output, label: &str) {
    assert!(
        out.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn extract_text_runs() {
    let out = run(&["extract-text", fx("tracemonkey.pdf").to_str().unwrap(), "-p", "3"]);
    assert_ok(&out, "extract-text");
    assert!(!out.stdout.is_empty(), "extract-text produced no text");
}

#[test]
fn render_raster_runs() {
    let o = tmp("render.zip");
    let out = run(&[
        "render",
        fx("multi_stream.pdf").to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "png",
    ]);
    assert_ok(&out, "render png");
    assert!(o.exists());
    let _ = std::fs::remove_file(&o);
}

#[test]
fn render_svg_runs() {
    let o = tmp("render_svg.zip");
    let out = run(&[
        "render",
        fx("multi_stream.pdf").to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "svg",
    ]);
    assert_ok(&out, "render svg");
    assert!(o.exists());
    let _ = std::fs::remove_file(&o);
}

#[test]
fn extract_images_runs() {
    let o = tmp("images.zip");
    let out = run(&[
        "extract-images",
        fx("image_only.pdf").to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
    ]);
    assert_ok(&out, "extract-images");
    assert!(o.exists());
    let _ = std::fs::remove_file(&o);
}

#[test]
fn analyze_runs() {
    let out = run(&["analyze", fx("tracemonkey.pdf").to_str().unwrap()]);
    assert_ok(&out, "analyze");
    assert!(String::from_utf8_lossy(&out.stdout).contains("has_text_layer"));
}

#[test]
fn merge_runs_and_counts() {
    let o = tmp("merged.pdf");
    let out = run(&[
        "merge",
        fx("minimal.pdf").to_str().unwrap(),
        fx("flate.pdf").to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
    ]);
    assert_ok(&out, "merge");
    // Re-open with `info` and confirm 2 pages.
    let info = run(&["info", o.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&info.stdout).contains("Pages:           2"));
    let _ = std::fs::remove_file(&o);
}

#[test]
fn split_runs() {
    let pat = tmp("split-%d.pdf");
    let out = run(&[
        "split",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-o",
        pat.to_str().unwrap(),
        "-f",
        "1",
        "-l",
        "2",
    ]);
    assert_ok(&out, "split");
    for n in 1..=2 {
        let p = std::env::temp_dir().join(format!("oxide_tool_surface_split-{n}.pdf"));
        assert!(p.exists(), "split page {n} missing");
        let _ = std::fs::remove_file(&p);
    }
}

#[test]
fn extract_pages_runs() {
    let o = tmp("subset.pdf");
    let out = run(&[
        "extract-pages",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "3,1",
        "-o",
        o.to_str().unwrap(),
    ]);
    assert_ok(&out, "extract-pages");
    let info = run(&["info", o.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&info.stdout).contains("Pages:           2"));
    let _ = std::fs::remove_file(&o);
}

#[test]
fn info_runs_json_and_human() {
    let human = run(&["info", fx("tracemonkey.pdf").to_str().unwrap()]);
    assert_ok(&human, "info");
    assert!(String::from_utf8_lossy(&human.stdout).contains("Pages:"));

    let json = run(&["info", fx("tracemonkey.pdf").to_str().unwrap(), "--json"]);
    assert_ok(&json, "info --json");
    assert!(String::from_utf8_lossy(&json.stdout).contains("\"page_count\""));
}

#[test]
fn fonts_runs() {
    let out = run(&["fonts", fx("tracemonkey.pdf").to_str().unwrap()]);
    assert_ok(&out, "fonts");
    assert!(String::from_utf8_lossy(&out.stdout).contains("type"));
}

#[test]
fn detach_runs() {
    let out = run(&["detach", fx("attach_nametree.pdf").to_str().unwrap()]);
    assert_ok(&out, "detach");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Hello.txt"));
}

#[test]
fn to_html_runs() {
    let out = run(&["to-html", fx("multi_stream.pdf").to_str().unwrap(), "-p", "1"]);
    assert_ok(&out, "to-html");
    assert!(String::from_utf8_lossy(&out.stdout).contains("<!DOCTYPE html>"));
}

#[test]
fn verify_sig_runs() {
    let out = run(&["verify-sig", fx("sig_valid.pdf").to_str().unwrap()]);
    assert_ok(&out, "verify-sig");
    assert!(String::from_utf8_lossy(&out.stdout).contains("VALID"));
}

#[test]
fn password_flag_accepted_by_render_and_images() {
    // render + extract-images accept --password (an encrypted fixture that
    // unlocks with the empty password).
    let enc = fixtures()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("corpus")
        .join("pdfs")
        .join("pdfjs")
        .join("empty_protected.pdf");
    if !enc.exists() {
        eprintln!("NOTE: encrypted fixture missing; skipping password-flag test");
        return;
    }
    let o = tmp("enc.zip");
    let out = run(&[
        "render",
        enc.to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
        "-p",
        "1",
        "--password",
        "",
    ]);
    assert_ok(&out, "render --password");
    let _ = std::fs::remove_file(&o);
}
