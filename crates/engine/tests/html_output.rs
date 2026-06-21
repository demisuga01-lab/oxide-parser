//! Integration tests for HTML/XML output (`pdftohtml`-equivalent).
//!
//! Validation strategy (no headless browser available in this environment):
//! - TEXT CONTENT: the produced HTML/XML must contain the page's text, escaped,
//!   in reading order — verified incl. an RTL fixture and a multi-column one.
//! - STRUCTURE/POSITION: complex-mode fragments carry plausible left/top/
//!   font-size; the raster-background mode embeds the exact raster render.
//! - CROSS-CHECK: text content agrees with Poppler `pdftohtml` (markup differs;
//!   we compare the set of words, not bytes).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use oxide_engine::{ContentEngine, HtmlMode, HtmlOptions};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn corpus(rel: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join("tests")
        .join("corpus")
        .join("pdfs")
        .join(rel);
    p.exists().then_some(p)
}

fn engine_path(p: &std::path::Path) -> ContentEngine {
    ContentEngine::open_bytes(std::fs::read(p).unwrap()).unwrap()
}

fn engine(name: &str) -> ContentEngine {
    engine_path(&fixture(name))
}

/// Words (length>=2, alphanumeric/unicode) from a blob of text/HTML, after
/// stripping tags. For loose content comparison against Poppler.
fn words(s: &str) -> HashSet<String> {
    // Strip anything between < and > (tags), then split on non-word chars.
    let mut text = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_lowercase())
        .collect()
}

fn poppler_pdftohtml() -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join("target")
        .join("tools")
        .join("poppler");
    base.is_dir()
        .then(|| find_under(&base, "pdftohtml"))
        .flatten()
}

fn find_under(dir: &std::path::Path, tool: &str) -> Option<PathBuf> {
    let exe = format!("{tool}.exe");
    let mut subdirs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().and_then(|n| n.to_str()) == Some(exe.as_str()) {
            return Some(p);
        }
    }
    for s in subdirs {
        if let Some(f) = find_under(&s, tool) {
            return Some(f);
        }
    }
    None
}

#[test]
fn complex_html_is_well_formed_and_positioned() {
    let e = engine("multi_stream.pdf");
    let html = e.export_html(&[1], &HtmlOptions::default()).unwrap();

    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("charset=\"UTF-8\""));
    assert!(html.contains("class=\"page\""));
    assert!(html.trim_end().ends_with("</html>"));

    // Positioned text fragments with left/top/font-size.
    assert!(html.contains("class=\"t\""));
    assert!(html.contains("left:") && html.contains("top:") && html.contains("font-size:"));

    // The fixture's text appears.
    assert!(html.contains("Hello") && html.contains("World"));
}

#[test]
fn complex_positions_are_plausible() {
    // multi_stream p1 draws "Hello" near (50,80)pt and "World" near (50,100)pt
    // (PDF bottom-left origin). At scale 96/72, left = 50*1.333 = 66.7px, and
    // top = (792 - y - fontsize)*1.333. Assert the emitted values are sane.
    let e = engine("multi_stream.pdf");
    let html = e.export_html(&[1], &HtmlOptions::default()).unwrap();
    // Find the "Hello" fragment line and parse its left/top.
    let line = html
        .lines()
        .find(|l| l.contains(">Hello<"))
        .expect("Hello fragment present");
    let left = parse_px(line, "left:");
    let top = parse_px(line, "top:");
    let size = parse_px(line, "font-size:");
    assert!(
        (60.0..75.0).contains(&left),
        "left {left} out of expected range"
    );
    assert!(top > 0.0 && top < 1056.0, "top {top} off page");
    assert!((10.0..25.0).contains(&size), "font-size {size} unexpected");
}

fn parse_px(line: &str, key: &str) -> f64 {
    let start = line.find(key).unwrap() + key.len();
    let rest = &line[start..];
    let end = rest.find("px").unwrap();
    rest[..end].trim().parse().unwrap()
}

#[test]
fn background_mode_embeds_raster_and_keeps_text() {
    let e = engine("multi_stream.pdf");
    let opts = HtmlOptions {
        background: true,
        ..Default::default()
    };
    let html = e.export_html(&[1], &opts).unwrap();
    assert!(
        html.contains("class=\"bg\"") && html.contains("data:image/png;base64,"),
        "background mode must embed a raster PNG data URI"
    );
    // Text is still present (overlaid, selectable).
    assert!(html.contains("Hello"));
}

#[test]
fn xml_mode_emits_positioned_fragments() {
    let e = engine("multi_stream.pdf");
    let opts = HtmlOptions {
        mode: HtmlMode::Xml,
        ..Default::default()
    };
    let xml = e.export_html(&[1], &opts).unwrap();
    assert!(xml.starts_with("<?xml"));
    assert!(xml.contains("<pdf2xml>"));
    assert!(xml.contains("<page number=\"1\""));
    assert!(xml.contains("<text") && xml.contains("font-size="));
    assert!(xml.contains(">Hello<") && xml.contains(">World<"));
}

#[test]
fn simple_mode_emits_paragraphs() {
    let e = engine("multi_stream.pdf");
    let opts = HtmlOptions {
        mode: HtmlMode::Simple,
        ..Default::default()
    };
    let html = e.export_html(&[1], &opts).unwrap();
    assert!(html.contains("<p>") || html.contains("<br/>"));
    assert!(html.contains("Hello"));
}

#[test]
fn html_escapes_special_characters() {
    // tracemonkey p3 has real text; ensure no raw unescaped angle brackets leak
    // into text content. (We can't easily craft a fixture with literal '<' in
    // text, so assert the invariant that any '<' is part of a tag: every '<' is
    // followed by a letter, '/', '!', or '?'.)
    let e = engine("tracemonkey.pdf");
    let html = e.export_html(&[3], &HtmlOptions::default()).unwrap();
    for (i, _) in html.match_indices('<') {
        let next = html[i + 1..].chars().next().unwrap_or(' ');
        assert!(
            next.is_ascii_alphabetic() || matches!(next, '/' | '!' | '?'),
            "stray '<' not starting a tag at byte {i}"
        );
    }
}

#[test]
fn rtl_text_is_present_and_marked() {
    let Some(path) = corpus("pdfjs/ArabicCIDTrueType.pdf") else {
        eprintln!("NOTE: Arabic fixture missing; skipping RTL test");
        return;
    };
    let e = engine_path(&path);
    let html = e.export_html(&[1], &HtmlOptions::default()).unwrap();
    // The Oxide text extraction yields Arabic; that text must appear in the HTML
    // and at least one fragment must be flagged dir="rtl".
    let plain = e.get_page_text(1).unwrap_or_default();
    let arabic: String = plain
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            (0x0600..=0x06FF).contains(&cp)
                || (0xFB50..=0xFDFF).contains(&cp)
                || (0xFE70..=0xFEFF).contains(&cp)
        })
        .take(3)
        .collect();
    if !arabic.is_empty() {
        assert!(
            html.contains(&arabic),
            "Arabic text must appear in the HTML"
        );
        assert!(
            html.contains("dir=\"rtl\""),
            "an RTL line must be marked dir=rtl"
        );
    }
}

#[test]
fn multi_column_text_present() {
    let Some(path) = corpus("generated/generated_two_columns.pdf") else {
        eprintln!("NOTE: two-column fixture missing; skipping");
        return;
    };
    let e = engine_path(&path);
    let html = e.export_html(&[1], &HtmlOptions::default()).unwrap();
    // Both columns' text should be present.
    assert!(html.contains("Left column"), "left column text missing");
    assert!(
        html.contains("Right column") || html.to_lowercase().contains("right"),
        "right column text missing"
    );
}

#[test]
fn cross_check_text_against_pdftohtml() {
    let Some(tool) = poppler_pdftohtml() else {
        eprintln!("NOTE: pdftohtml not found; skipping text cross-check");
        return;
    };
    let name = "tracemonkey.pdf";
    let page = 3usize;
    let e = engine(name);
    let oxide_html = e.export_html(&[page], &HtmlOptions::default()).unwrap();
    let oxide_words = words(&oxide_html);

    // pdftohtml -stdout -f P -l P -i (ignore images) -c (complex) the page.
    let out = Command::new(&tool)
        .arg("-stdout")
        .arg("-f")
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg("-i")
        .arg("-c")
        .arg(fixture(name))
        .output()
        .expect("run pdftohtml");
    if !out.status.success() {
        eprintln!("pdftohtml failed; skipping cross-check");
        return;
    }
    let poppler_words = words(&String::from_utf8_lossy(&out.stdout));
    if poppler_words.is_empty() {
        eprintln!("pdftohtml produced no words; skipping");
        return;
    }

    // Loose agreement: most of Poppler's words appear in Oxide's output.
    let common = poppler_words.intersection(&oxide_words).count();
    let ratio = common as f64 / poppler_words.len() as f64;
    assert!(
        ratio >= 0.6,
        "Oxide HTML shares only {:.0}% of pdftohtml's words ({common}/{})",
        ratio * 100.0,
        poppler_words.len()
    );
    eprintln!(
        "pdftohtml text cross-check ({name} p{page}): {:.0}% word overlap",
        ratio * 100.0
    );
}
