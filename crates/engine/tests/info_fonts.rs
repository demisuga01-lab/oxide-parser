//! Integration tests for the `info` (pdfinfo-equivalent) and `fonts`
//! (pdffonts-equivalent) reporting tools.
//!
//! Where a Poppler install is available (bundled under `target/tools/poppler`
//! or on PATH), the tests cross-check Oxide's report against `pdfinfo` /
//! `pdffonts` — the strongest correctness signal. Without Poppler they fall
//! back to asserting against values read directly from the fixtures.

use std::path::PathBuf;
use std::process::Command;

use oxide_engine::{ContentEngine, PdfDocument};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn corpus_path(rel: &str) -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent()?.parent()?;
    let p = root.join("tests").join("corpus").join("pdfs").join(rel);
    p.exists().then_some(p)
}

fn poppler_tool(tool: &str) -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(|p| p.parent()) {
        let base = root.join("target").join("tools").join("poppler");
        if base.is_dir() {
            if let Some(found) = find_under(&base, tool) {
                return Some(found);
            }
        }
    }
    let candidate = if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    };
    Command::new(&candidate)
        .arg("-v")
        .output()
        .ok()
        .map(|_| PathBuf::from(candidate))
}

fn find_under(dir: &std::path::Path, tool: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    };
    let mut subdirs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(exe.as_str()) {
            return Some(path);
        }
    }
    for sub in subdirs {
        if let Some(found) = find_under(&sub, tool) {
            return Some(found);
        }
    }
    None
}

/// Parse `pdfinfo` stdout into key→value lines.
fn run_pdfinfo(path: &std::path::Path) -> Option<Vec<(String, String)>> {
    let tool = poppler_tool("pdfinfo")?;
    let out = Command::new(&tool).arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut fields = Vec::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once(':') {
            fields.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some(fields)
}

fn pdfinfo_field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Parse `pdffonts` stdout into one record per font (the columns are
/// fixed-width). Returns Vec of (name, type, encoding, emb, sub, uni, obj).
fn run_pdffonts(path: &std::path::Path) -> Option<Vec<PdffontsRow>> {
    let tool = poppler_tool("pdffonts")?;
    let out = Command::new(&tool).arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in stdout.lines().skip(2) {
        // Columns are whitespace-separated; the last two are object id + gen.
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 7 {
            continue;
        }
        // object id and gen are the last two; everything walking back:
        // name type encoding emb sub uni objid gen
        let n = cols.len();
        let obj = cols[n - 2].parse::<u32>().ok();
        let uni = cols[n - 3] == "yes";
        let sub = cols[n - 4] == "yes";
        let emb = cols[n - 5] == "yes";
        let name = cols[0].to_string();
        rows.push(PdffontsRow {
            name,
            emb,
            sub,
            uni,
            object_number: obj,
        });
    }
    Some(rows)
}

struct PdffontsRow {
    name: String,
    emb: bool,
    sub: bool,
    uni: bool,
    object_number: Option<u32>,
}

// ---------------------------------------------------------------------------
// info tests
// ---------------------------------------------------------------------------

#[test]
fn info_basic_fields_match_fixture() {
    let doc =
        PdfDocument::open_bytes(std::fs::read(fixture_path("tracemonkey.pdf")).unwrap()).unwrap();
    let engine =
        ContentEngine::open_bytes(std::fs::read(fixture_path("tracemonkey.pdf")).unwrap()).unwrap();
    let info = engine.document_info().unwrap();

    assert_eq!(info.page_count, doc.get_pages().unwrap().len());
    assert_eq!(info.page_count, 14);
    assert_eq!(info.pdf_version, "1.4");
    assert!(!info.encrypted);
    assert_eq!(info.creator.as_deref(), Some("TeX"));
    assert_eq!(info.producer.as_deref(), Some("pdfeTeX-1.21a"));
    // First page is US Letter: 612 x 792 pts.
    let first = &info.page_sizes[0];
    assert_eq!(first.width_pts, 612.0);
    assert_eq!(first.height_pts, 792.0);
    assert!(!info.page_size_varies);
}

#[test]
fn info_cross_checks_pdfinfo() {
    let mut ran = false;
    for name in ["tracemonkey.pdf", "basicapi.pdf", "form_160f.pdf"] {
        let path = fixture_path(name);
        let Some(fields) = run_pdfinfo(&path) else {
            continue;
        };
        ran = true;
        let engine = ContentEngine::open_bytes(std::fs::read(&path).unwrap()).unwrap();
        let info = engine.document_info().unwrap();

        // Page count.
        if let Some(pages) = pdfinfo_field(&fields, "Pages") {
            assert_eq!(
                info.page_count.to_string(),
                pages,
                "{name}: page count disagrees with pdfinfo"
            );
        }
        // PDF version.
        if let Some(ver) = pdfinfo_field(&fields, "PDF version") {
            assert_eq!(info.pdf_version, ver, "{name}: PDF version disagrees");
        }
        // Encryption status.
        if let Some(enc) = pdfinfo_field(&fields, "Encrypted") {
            let poppler_encrypted = enc.starts_with("yes");
            assert_eq!(
                info.encrypted, poppler_encrypted,
                "{name}: encryption status disagrees with pdfinfo"
            );
        }
        // Page size: compare the integer point dimensions.
        if let Some(size) = pdfinfo_field(&fields, "Page size") {
            // e.g. "612 x 792 pts (letter)"
            let nums: Vec<f64> = size
                .split_whitespace()
                .filter_map(|t| t.parse::<f64>().ok())
                .collect();
            if nums.len() >= 2 {
                let first = &info.page_sizes[0];
                assert!(
                    (first.width_pts - nums[0]).abs() < 1.0
                        && (first.height_pts - nums[1]).abs() < 1.0,
                    "{name}: page size {}x{} disagrees with pdfinfo {}x{}",
                    first.width_pts,
                    first.height_pts,
                    nums[0],
                    nums[1]
                );
            }
        }
    }
    if !ran {
        eprintln!("NOTE: pdfinfo not found; skipped info cross-check");
    }
}

#[test]
fn info_reports_encryption_on_encrypted_fixture() {
    // Find an encrypted corpus fixture that unlocks with the empty password.
    let candidates = ["pdfjs/empty_protected.pdf", "pdfjs/secHandler.pdf"];
    for rel in candidates {
        let Some(path) = corpus_path(rel) else {
            continue;
        };
        let Ok(engine) = ContentEngine::open_bytes(std::fs::read(&path).unwrap()) else {
            continue;
        };
        if !engine.is_encrypted() {
            continue;
        }
        let info = engine.document_info().unwrap();
        assert!(info.encrypted);
        let enc = info
            .encryption
            .expect("encrypted doc must produce an encryption report");
        assert!(
            enc.algorithm.contains("AES") || enc.algorithm.contains("RC4"),
            "unexpected algorithm label: {}",
            enc.algorithm
        );
        assert!(enc.version >= 1 && enc.revision >= 2);
        return;
    }
    eprintln!("NOTE: no openable encrypted fixture; skipped encryption-report test");
}

// ---------------------------------------------------------------------------
// fonts tests
// ---------------------------------------------------------------------------

#[test]
fn fonts_cross_checks_pdffonts_set_and_flags() {
    let mut ran = false;
    for name in ["tracemonkey.pdf", "basicapi.pdf", "form_160f.pdf"] {
        let path = fixture_path(name);
        let Some(poppler_rows) = run_pdffonts(&path) else {
            continue;
        };
        ran = true;
        let engine = ContentEngine::open_bytes(std::fs::read(&path).unwrap()).unwrap();
        let fonts = engine.list_fonts().unwrap();

        // Same SET of object ids (the critical missing-font check).
        let mut ours: Vec<u32> = fonts.iter().map(|f| f.object_number).collect();
        let mut theirs: Vec<u32> = poppler_rows
            .iter()
            .filter_map(|r| r.object_number)
            .collect();
        ours.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(
            ours, theirs,
            "{name}: font object-id set disagrees with pdffonts (missing or extra font)"
        );

        // For each font Poppler reports, our emb/sub/uni flags must agree.
        for pr in &poppler_rows {
            let Some(obj) = pr.object_number else {
                continue;
            };
            let ours = fonts
                .iter()
                .find(|f| f.object_number == obj)
                .unwrap_or_else(|| panic!("{name}: missing font object {obj}"));
            assert_eq!(
                ours.embedded, pr.emb,
                "{name}: font {obj} emb flag disagrees"
            );
            assert_eq!(ours.subset, pr.sub, "{name}: font {obj} sub flag disagrees");
            assert_eq!(
                ours.to_unicode, pr.uni,
                "{name}: font {obj} uni flag disagrees"
            );
            // Name should match (allowing Poppler's '+' subset prefix form).
            assert_eq!(ours.name, pr.name, "{name}: font {obj} name disagrees");
        }
    }
    if !ran {
        eprintln!("NOTE: pdffonts not found; skipped fonts cross-check");
    }
}

#[test]
fn fonts_detects_subset_and_embedded() {
    let engine =
        ContentEngine::open_bytes(std::fs::read(fixture_path("tracemonkey.pdf")).unwrap()).unwrap();
    let fonts = engine.list_fonts().unwrap();
    assert!(!fonts.is_empty());
    // tracemonkey's fonts are all embedded subsets (XXXXXX+ prefix).
    let subset_embedded = fonts.iter().filter(|f| f.subset && f.embedded).count();
    assert!(
        subset_embedded >= 20,
        "expected many embedded subset fonts, found {subset_embedded}"
    );
    // Every subset font's name carries a 6-letter '+' prefix.
    for f in &fonts {
        if f.subset {
            let prefix = f.name.split('+').next().unwrap_or("");
            assert_eq!(prefix.len(), 6, "subset prefix wrong for {}", f.name);
        }
    }
}

#[test]
fn fonts_reports_non_embedded_standard_14() {
    // basicapi.pdf uses the standard-14 Helvetica/Times/Courier (not embedded).
    let engine =
        ContentEngine::open_bytes(std::fs::read(fixture_path("basicapi.pdf")).unwrap()).unwrap();
    let fonts = engine.list_fonts().unwrap();
    let helv = fonts
        .iter()
        .find(|f| f.name == "Helvetica")
        .expect("Helvetica present");
    assert!(!helv.embedded, "standard-14 Helvetica must be non-embedded");
    assert!(!helv.subset);
    assert_eq!(helv.font_type, "Type 1");
}

#[test]
fn fonts_reports_type0_cid_identity_h() {
    // basicapi.pdf embeds DejaVuSans as a CID TrueType with Identity-H.
    let engine =
        ContentEngine::open_bytes(std::fs::read(fixture_path("basicapi.pdf")).unwrap()).unwrap();
    let fonts = engine.list_fonts().unwrap();
    let cid = fonts
        .iter()
        .find(|f| f.name.contains("DejaVuSans") && !f.name.contains("Bold"))
        .expect("DejaVuSans CID font present");
    assert_eq!(cid.font_type, "CID TrueType");
    assert_eq!(cid.encoding, "Identity-H");
    assert!(cid.embedded);
}
