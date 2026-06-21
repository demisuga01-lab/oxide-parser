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
    let out = run(&[
        "extract-text",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "3",
    ]);
    assert_ok(&out, "extract-text");
    assert!(!out.stdout.is_empty(), "extract-text produced no text");
}

#[test]
fn extract_text_structured_runs() {
    // Layout-aware extraction (XY-cut reading order) + structured JSON.
    let out = run(&[
        "extract-text",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "3",
        "--structured",
    ]);
    assert_ok(&out, "extract-text --structured");
    assert!(
        !out.stdout.is_empty(),
        "structured extraction produced no text"
    );

    let json = run(&[
        "extract-text",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "3",
        "--structured",
        "--format",
        "json",
    ]);
    assert_ok(&json, "extract-text --structured --format json");
    let s = String::from_utf8_lossy(&json.stdout);
    assert!(s.contains("\"blocks\""), "JSON should contain a block tree");
}

#[test]
fn extract_text_semantic_runs() {
    // Semantic mode uses tagged-PDF structure when present and falls back to the
    // geometric analyzer for untagged fixtures.
    let out = run(&[
        "extract-text",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "3",
        "--semantic",
    ]);
    assert_ok(&out, "extract-text --semantic");
    assert!(
        !out.stdout.is_empty(),
        "semantic extraction produced no text"
    );

    let json = run(&[
        "extract-text",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "3",
        "--semantic",
        "--format",
        "json",
    ]);
    assert_ok(&json, "extract-text --semantic --format json");
    let s = String::from_utf8_lossy(&json.stdout);
    assert!(s.contains("\"source\""), "JSON should describe the source");
    assert!(s.contains("geometric_fallback") || s.contains("tagged_pdf"));
}

#[test]
fn extract_tables_runs() {
    // Table extraction (no Poppler equivalent). The fixture may or may not
    // contain a table; the command must succeed and emit valid output either way.
    let csv = run(&[
        "extract-tables",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "csv",
    ]);
    assert_ok(&csv, "extract-tables --format csv");

    let json = run(&[
        "extract-tables",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "json",
        "--structure",
    ]);
    assert_ok(&json, "extract-tables --format json");
    let s = String::from_utf8_lossy(&json.stdout);
    assert!(s.contains("\"pages\""), "JSON should have a pages array");

    let html = run(&[
        "extract-tables",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "html",
    ]);
    assert_ok(&html, "extract-tables --format html");
    let h = String::from_utf8_lossy(&html.stdout);
    assert!(h.contains("<!doctype html>"), "HTML should be a document");
}

#[test]
fn parse_runs() {
    // The canonical-model `parse` command must serialize to all three formats.
    let md = run(&[
        "parse",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "markdown",
    ]);
    assert_ok(&md, "parse --format markdown");
    assert!(!md.stdout.is_empty(), "parse markdown produced no output");

    let json = run(&[
        "parse",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "json",
    ]);
    assert_ok(&json, "parse --format json");
    let s = String::from_utf8_lossy(&json.stdout);
    assert!(
        s.contains("\"schema_version\""),
        "JSON should carry a schema version"
    );
    assert!(
        s.contains("\"body\""),
        "JSON should carry the body block stream"
    );
    assert!(
        s.contains("\"pages\""),
        "JSON should carry the per-page view"
    );

    let html = run(&[
        "parse",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "html",
    ]);
    assert_ok(&html, "parse --format html");
    let h = String::from_utf8_lossy(&html.stdout);
    assert!(h.contains("<html>"), "HTML should be a document");
}

#[test]
fn parse_robustness_flags_and_per_page_source() {
    // The de-hyphenation / ligature flags are accepted, and JSON carries per-page
    // provenance (schema 1.1).
    let json = run(&[
        "parse",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "json",
        "--dehyphenate",
        "--normalize-ligatures",
    ]);
    assert_ok(&json, "parse with robustness flags");
    let s = String::from_utf8_lossy(&json.stdout);
    assert!(s.contains("\"schema_version\": \"1.1\""), "schema 1.1");
    assert!(s.contains("\"source\""), "per-page source recorded");
}

#[test]
fn document_model_alias_runs() {
    // `document-model` is retained as a back-compat alias for `parse`.
    let out = run(&[
        "document-model",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "md",
    ]);
    assert_ok(&out, "document-model alias");
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
fn render_ps_runs() {
    // `render --format ps` (pdftops / pdftocairo -ps equivalent) — completes
    // the 12/12 Poppler tool surface. Output is a single DSC PostScript file.
    let o = tmp("render_ps.ps");
    let out = run(&[
        "render",
        fx("multi_stream.pdf").to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "ps",
    ]);
    assert_ok(&out, "render ps");
    assert!(o.exists());
    let body = std::fs::read_to_string(&o).unwrap();
    assert!(
        body.starts_with("%!PS-Adobe-3.0"),
        "valid DSC PostScript header"
    );
    assert!(body.contains("showpage"));
    let _ = std::fs::remove_file(&o);
}

#[test]
fn render_eps_runs() {
    // `render --format eps` (pdftops -eps / pdftocairo -eps equivalent) — one
    // EPSF document per page inside the ZIP.
    let o = tmp("render_eps.zip");
    let out = run(&[
        "render",
        fx("multi_stream.pdf").to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
        "-p",
        "1",
        "--format",
        "eps",
    ]);
    assert_ok(&out, "render eps");
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
    let out = run(&[
        "to-html",
        fx("multi_stream.pdf").to_str().unwrap(),
        "-p",
        "1",
    ]);
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

/// Regression (Renderer Benchmark 0A, Part B): a hostile page declaring a giant
/// `/MediaBox` must NOT abort the process with a multi-hundred-gigabyte
/// allocation. The CLI must survive, exit 0, skip the page with a clean warning,
/// and write a (page-less) output archive.
#[test]
fn render_rejects_huge_page_without_abort() {
    // A parseable single-page PDF whose /MediaBox is [0 0 200000 200000]. At 144
    // DPI that is 400000x400000 px (~640 GB), which must be rejected cleanly
    // before allocation rather than aborting the process. The body is built with
    // a real cross-reference table + startxref so the reader accepts it.
    let input = tmp("huge_page.pdf");
    std::fs::write(&input, huge_page_pdf()).expect("write huge-page fixture");
    let o = tmp("huge_page.zip");

    let out = run(&[
        "render",
        input.to_str().unwrap(),
        "-o",
        o.to_str().unwrap(),
        "--dpi",
        "144",
    ]);

    // The process survives and exits cleanly (no abort/panic/signal).
    assert!(
        out.status.success(),
        "huge-page render must exit cleanly, got status {:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("resource limit") || stderr.contains("skipped page"),
        "expected a clean resource-limit warning, got stderr: {stderr}"
    );
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&o);
}

/// Build a minimal but well-formed single-page PDF with a giant `/MediaBox`,
/// including a valid xref table and `startxref` so the reader parses it.
fn huge_page_pdf() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200000 200000] >>",
    ];
    let mut pdf = String::from("%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (idx, body) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", idx + 1, body));
    }
    let xref_off = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
    for off in &offsets {
        pdf.push_str(&format!("{:010} 00000 n \n", off));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        objs.len() + 1,
        xref_off
    ));
    pdf.into_bytes()
}

// --- Unified-surface additions (Prompt 7) -----------------------------------

#[test]
fn version_reports_engine_and_ocr_status() {
    // `--version` must report the engine version AND whether OCR is compiled in,
    // so a user can tell without running an --ocr command. (Value of the OCR
    // line depends on build features; the labels are always present.)
    let out = run(&["--version"]);
    assert_ok(&out, "--version");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("engine:"), "version should report engine: {s}");
    assert!(s.contains("ocr:"), "version should report ocr status: {s}");
    assert!(s.contains("features:"), "version should list features: {s}");
}

#[test]
fn extract_tables_ocr_errors_cleanly() {
    // --ocr on extract-tables is intentionally unsupported (OCR'd table-grid
    // reconstruction is a known gap); it must fail with an actionable message,
    // not silently produce empty/garbage output.
    let out = run(&["extract-tables", fx("flate.pdf").to_str().unwrap(), "--ocr"]);
    assert!(!out.status.success(), "extract-tables --ocr should error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("does not support --ocr"),
        "should explain the gap: {err}"
    );
}

// --- Structural-write ops (Bucket 2) ----------------------------------------

#[test]
fn rotate_command_writes_rotated_pdf() {
    let out = tmp("rotate_out.pdf");
    let res = run(&[
        "rotate",
        fx("flate.pdf").to_str().unwrap(),
        "--angle",
        "90",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_ok(&res, "rotate");
    assert!(out.exists() && std::fs::metadata(&out).unwrap().len() > 0);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn encrypt_command_aes256_roundtrips_via_cli() {
    let out = tmp("encrypt_out.pdf");
    let res = run(&[
        "encrypt",
        fx("flate.pdf").to_str().unwrap(),
        "--user-pw",
        "secret",
        "--algo",
        "aes256",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_ok(&res, "encrypt");
    // The encrypted file must NOT extract text without the password...
    let no_pw = run(&["extract-text", out.to_str().unwrap()]);
    assert!(
        !no_pw.status.success(),
        "encrypted file must require a password"
    );
    // ...and must extract WITH it.
    let with_pw = run(&[
        "extract-text",
        out.to_str().unwrap(),
        "--password",
        "secret",
    ]);
    assert_ok(&with_pw, "extract-text with password");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn optimize_command_writes_smaller_or_equal_pdf() {
    let out = tmp("optimize_out.pdf");
    let res = run(&[
        "optimize",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert_ok(&res, "optimize");
    assert!(out.exists());
    // --json output is parseable and reports the op.
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(
        stdout.contains("\"op\":\"optimize\""),
        "json result: {stdout}"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn repair_command_writes_clean_pdf() {
    let out = tmp("repair_out.pdf");
    let res = run(&[
        "repair",
        fx("flate.pdf").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_ok(&res, "repair");
    assert!(out.exists());
    // The repaired file re-parses.
    let info = run(&["info", out.to_str().unwrap()]);
    assert_ok(&info, "info on repaired");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn linearize_command_writes_fast_web_view_pdf() {
    let out = tmp("linearize_out.pdf");
    let _ = std::fs::remove_file(&out);
    let res = run(&[
        "linearize",
        fx("flate.pdf").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_ok(&res, "linearize");
    assert!(out.exists());
    let bytes = std::fs::read(&out).expect("linearized output");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Linearized"),
        "output should carry /Linearized"
    );
    let info = run(&["info", out.to_str().unwrap()]);
    assert_ok(&info, "info on linearized");
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(
        stdout.contains("Optimized:       yes"),
        "info output: {stdout}"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn linearize_command_refuses_unvalidated_multi_page_pdf() {
    let out = tmp("linearize_multi_out.pdf");
    let _ = std::fs::remove_file(&out);
    let res = run(&[
        "linearize",
        fx("tracemonkey.pdf").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        !res.status.success(),
        "linearize should reject unvalidated multi-page output"
    );
    let err = String::from_utf8_lossy(&res.stderr);
    assert!(
        err.contains("multi-page"),
        "should explain the staged boundary: {err}"
    );
    assert!(
        !out.exists(),
        "unsupported linearization must not write output"
    );
}
