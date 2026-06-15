//! Integration tests for the PDF writer/serializer and the merge/split/extract
//! builders layered on top of it.
//!
//! These are the *foundation* tests for the PDF writer: they prove it
//! emits valid, faithful PDFs by
//!   1. round-tripping (parse → write → re-parse) and comparing page count,
//!      page sizes, and extracted text, and
//!   2. opening the written output with an external Poppler install
//!      (`pdfinfo` / `pdftotext`) when one is available, which is the strongest
//!      signal the output is a genuinely valid PDF and not just self-consistent.
//!
//! Poppler validation is best-effort: if no Poppler binary is found the test
//! still asserts the in-engine round-trip invariants and prints a notice. The
//! bundled Poppler used by the parity harness lives under
//! `target/tools/poppler/...`, so when the repo has it the external checks run.

use std::path::PathBuf;
use std::process::Command;

use oxide_engine::{
    build_merged, build_subset, write_document_roundtrip, ContentEngine, PdfDocument,
};

const FIXTURES: &[&str] = &["minimal.pdf", "flate.pdf", "multi_stream.pdf"];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).expect("fixture readable")
}

/// Path to a file in the shared `tests/corpus` tree at the repo root.
fn corpus_path(rel: &str) -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent()?.parent()?;
    let p = root.join("tests").join("corpus").join("pdfs").join(rel);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Locate a Poppler tool (`pdfinfo`, `pdftotext`) for external validation.
/// Checks the bundled copy under `target/tools/poppler/...` first, then PATH.
/// Returns `None` if not found (external checks are then skipped).
fn poppler_tool(tool: &str) -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/engine -> repo root is two levels up.
    let repo_root = manifest.parent().and_then(|p| p.parent());
    if let Some(root) = repo_root {
        let base = root.join("target").join("tools").join("poppler");
        if base.is_dir() {
            // Search for `<tool>.exe` (or `<tool>`) anywhere under the bundle.
            if let Some(found) = find_under(&base, tool) {
                return Some(found);
            }
        }
    }
    // Fall back to PATH: rely on the OS to resolve a bare command name.
    let candidate = if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    };
    // Probe via `--help`/`-v`; if it runs, assume it's on PATH.
    if Command::new(&candidate).arg("-v").output().is_ok() {
        return Some(PathBuf::from(candidate));
    }
    None
}

fn find_under(dir: &std::path::Path, tool: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    };
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
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

/// Run Poppler `pdfinfo` on `bytes` written to a temp file; return reported
/// page count, or `None` if Poppler is unavailable. Panics if Poppler runs but
/// rejects the file (that's a writer bug we want to fail on).
fn poppler_page_count(bytes: &[u8], label: &str) -> Option<usize> {
    let tool = poppler_tool("pdfinfo")?;
    let tmp = std::env::temp_dir().join(format!("oxide_writer_test_{label}.pdf"));
    std::fs::write(&tmp, bytes).expect("write temp pdf");
    let output = Command::new(&tool)
        .arg(&tmp)
        .output()
        .expect("run pdfinfo");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_file(&tmp);
    assert!(
        output.status.success(),
        "Poppler pdfinfo REJECTED writer output for {label}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Pages:") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

/// Run Poppler `pdftotext` and return the extracted text, or `None` if Poppler
/// is unavailable. Panics if Poppler rejects the file.
fn poppler_text(bytes: &[u8], label: &str) -> Option<String> {
    let tool = poppler_tool("pdftotext")?;
    let tmp = std::env::temp_dir().join(format!("oxide_writer_text_{label}.pdf"));
    std::fs::write(&tmp, bytes).expect("write temp pdf");
    let output = Command::new(&tool)
        .arg(&tmp)
        .arg("-")
        .output()
        .expect("run pdftotext");
    let _ = std::fs::remove_file(&tmp);
    assert!(
        output.status.success(),
        "Poppler pdftotext REJECTED writer output for {label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Normalize text for comparison: collapse all whitespace runs to single
/// spaces and trim, so formatting differences don't cause spurious mismatches.
fn norm(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn roundtrip_preserves_page_count_sizes_and_text() {
    for name in FIXTURES {
        let original = ContentEngine::open_bytes(fixture_bytes(name)).unwrap();
        let orig_pages: Vec<_> = original.document().get_pages().unwrap();
        let orig_count = orig_pages.len();

        let doc = PdfDocument::open_bytes(fixture_bytes(name)).unwrap();
        let written = write_document_roundtrip(doc.reader()).unwrap();

        // Re-parse the written bytes.
        let rewritten = ContentEngine::open_bytes(written.clone()).unwrap();
        let new_pages: Vec<_> = rewritten.document().get_pages().unwrap();

        assert_eq!(
            new_pages.len(),
            orig_count,
            "{name}: page count changed across round-trip"
        );
        for (i, (o, n)) in orig_pages.iter().zip(new_pages.iter()).enumerate() {
            assert_eq!(
                o.media_box,
                n.media_box,
                "{name}: page {i} media box changed"
            );
        }

        // Text equality across the round-trip.
        for page in 1..=orig_count {
            let before = norm(&original.get_page_text(page).unwrap_or_default());
            let after = norm(&rewritten.get_page_text(page).unwrap_or_default());
            assert_eq!(before, after, "{name}: page {page} text changed in round-trip");
        }
    }
}

#[test]
fn roundtrip_output_opens_in_poppler() {
    let mut ran_external = false;
    for name in FIXTURES {
        let doc = PdfDocument::open_bytes(fixture_bytes(name)).unwrap();
        let orig_count = doc.get_pages().unwrap().len();
        let written = write_document_roundtrip(doc.reader()).unwrap();

        if let Some(pages) = poppler_page_count(&written, &format!("rt_{}", name.replace('.', "_")))
        {
            ran_external = true;
            assert_eq!(
                pages, orig_count,
                "{name}: Poppler reports {pages} pages, expected {orig_count}"
            );
        }
    }
    if !ran_external {
        eprintln!("NOTE: Poppler not found; skipped external validation of round-trip output");
    }
}

#[test]
fn extract_single_page_matches_source_text() {
    // multi_stream.pdf is a one-page doc; minimal/flate likewise. Use the
    // multi-page tracemonkey if present, else fall back to single-page fixture.
    let name = "minimal.pdf";
    let doc = PdfDocument::open_bytes(fixture_bytes(name)).unwrap();
    let source = ContentEngine::open_bytes(fixture_bytes(name)).unwrap();

    let written = build_subset(&doc, &[1]).unwrap();
    let extracted = ContentEngine::open_bytes(written.clone()).unwrap();

    assert_eq!(extracted.document().get_pages().unwrap().len(), 1);
    assert_eq!(
        norm(&extracted.get_page_text(1).unwrap()),
        norm(&source.get_page_text(1).unwrap()),
        "extracted page text differs from source"
    );

    // External: Poppler must accept it and report 1 page.
    if let Some(pages) = poppler_page_count(&written, "extract_one") {
        assert_eq!(pages, 1);
    }
}

#[test]
fn merge_two_documents_concatenates_pages() {
    let doc_a = PdfDocument::open_bytes(fixture_bytes("minimal.pdf")).unwrap();
    let doc_b = PdfDocument::open_bytes(fixture_bytes("flate.pdf")).unwrap();
    let count_a = doc_a.get_pages().unwrap().len();
    let count_b = doc_b.get_pages().unwrap().len();

    let all_a: Vec<usize> = (1..=count_a).collect();
    let all_b: Vec<usize> = (1..=count_b).collect();
    let merged = build_merged(&[(&doc_a, all_a), (&doc_b, all_b)]).unwrap();

    let result = ContentEngine::open_bytes(merged.clone()).unwrap();
    assert_eq!(
        result.document().get_pages().unwrap().len(),
        count_a + count_b,
        "merged page count must equal sum of inputs"
    );

    // Page 1 text should match minimal's page 1; the last page should match
    // flate's last page.
    let src_a = ContentEngine::open_bytes(fixture_bytes("minimal.pdf")).unwrap();
    let src_b = ContentEngine::open_bytes(fixture_bytes("flate.pdf")).unwrap();
    assert_eq!(
        norm(&result.get_page_text(1).unwrap()),
        norm(&src_a.get_page_text(1).unwrap()),
        "first merged page text must match first input"
    );
    assert_eq!(
        norm(&result.get_page_text(count_a + count_b).unwrap()),
        norm(&src_b.get_page_text(count_b).unwrap()),
        "last merged page text must match last input"
    );

    if let Some(pages) = poppler_page_count(&merged, "merge_two") {
        assert_eq!(pages, count_a + count_b);
    }
    // Faithful-preservation check: Poppler's text from the merged file must
    // match its text from the two originals concatenated. (These particular
    // fixtures reference an undefined font /F1, so Poppler emits empty text for
    // each — the point is that merge changes nothing Poppler can see, not that
    // the fixtures happen to carry extractable text.)
    let merged_text = poppler_text(&merged, "merge_two_text");
    let orig_a_text = poppler_text(&fixture_bytes("minimal.pdf"), "merge_orig_a");
    let orig_b_text = poppler_text(&fixture_bytes("flate.pdf"), "merge_orig_b");
    if let (Some(m), Some(a), Some(b)) = (merged_text, orig_a_text, orig_b_text) {
        assert_eq!(
            norm(&m),
            norm(&format!("{a} {b}")),
            "Poppler text of merged file must equal concatenation of inputs' text"
        );
    }
}

#[test]
fn roundtrip_real_document_preserves_text_and_poppler_agrees() {
    // tracemonkey.pdf is a realistic multi-page paper with embedded fonts and
    // (typically) object streams — the strongest faithfulness probe. We require
    // it to round-trip with identical Oxide text, identical page count, and —
    // when Poppler is available — identical Poppler text and page count.
    let name = "tracemonkey.pdf";
    let path = fixture_path(name);
    if !path.exists() {
        eprintln!("NOTE: {name} not present; skipping real-document round-trip");
        return;
    }
    let original = ContentEngine::open_bytes(fixture_bytes(name)).unwrap();
    let orig_count = original.document().get_pages().unwrap().len();
    assert!(orig_count > 1, "expected a multi-page document");

    let doc = PdfDocument::open_bytes(fixture_bytes(name)).unwrap();
    let written = write_document_roundtrip(doc.reader()).unwrap();
    let rewritten = ContentEngine::open_bytes(written.clone()).unwrap();

    assert_eq!(
        rewritten.document().get_pages().unwrap().len(),
        orig_count,
        "real-document round-trip changed page count"
    );

    // Compare Oxide-extracted text across all pages.
    for page in 1..=orig_count {
        let before = norm(&original.get_page_text(page).unwrap_or_default());
        let after = norm(&rewritten.get_page_text(page).unwrap_or_default());
        assert_eq!(before, after, "page {page} text changed in real round-trip");
    }

    // External: Poppler page count and whole-document text must match.
    if let Some(pages) = poppler_page_count(&written, "rt_tracemonkey") {
        assert_eq!(pages, orig_count, "Poppler page count changed in round-trip");
    }
    let rt_text = poppler_text(&written, "rt_tm_text");
    let orig_text = poppler_text(&fixture_bytes(name), "orig_tm_text");
    if let (Some(rt), Some(og)) = (rt_text, orig_text) {
        assert_eq!(
            norm(&rt),
            norm(&og),
            "Poppler text differs between original and round-tripped real document"
        );
    }
}

#[test]
fn encrypted_input_produces_unencrypted_openable_output() {
    // Permission-only-encrypted input that unlocks with the empty user
    // password. Round-tripping it must (a) succeed, (b) produce output with NO
    // /Encrypt entry (the bytes were decrypted on read), and (c) open in
    // Poppler with the same page count.
    // Try several known encrypted corpus fixtures and use the first that both
    // opens (unlocks with the empty user password) AND reports encrypted.
    let candidates = [
        "pdfjs/empty_protected.pdf",
        "pdfjs/secHandler.pdf",
        "pdfjs/issue14297.pdf",
    ];
    let mut chosen: Option<(Vec<u8>, ContentEngine)> = None;
    for rel in candidates {
        let Some(path) = corpus_path(rel) else { continue };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Ok(engine) = ContentEngine::open_bytes(bytes.clone()) {
            if engine.is_encrypted() && engine.document().get_pages().is_ok() {
                chosen = Some((bytes, engine));
                break;
            }
        }
    }
    let Some((bytes, source)) = chosen else {
        eprintln!(
            "NOTE: no encrypted corpus fixture unlocked with the empty password; \
             skipping encrypted round-trip (decrypt-on-read is covered elsewhere)"
        );
        return;
    };
    let orig_count = source.document().get_pages().unwrap().len();

    let doc = PdfDocument::open_bytes(bytes).unwrap();
    let written = write_document_roundtrip(doc.reader()).unwrap();

    // Output must reopen, must NOT be encrypted, and must keep the page count.
    let reopened = ContentEngine::open_bytes(written.clone()).unwrap();
    assert!(
        !reopened.is_encrypted(),
        "manipulation output must be unencrypted"
    );
    assert_eq!(
        reopened.document().get_pages().unwrap().len(),
        orig_count,
        "encrypted round-trip changed page count"
    );

    if let Some(pages) = poppler_page_count(&written, "rt_encrypted") {
        assert_eq!(pages, orig_count);
    }
}

#[test]
fn merge_preserves_differing_page_sizes() {
    // minimal.pdf is 200x200; flate.pdf uses its own media box. Merge and
    // confirm each page keeps its own size.
    let doc_a = PdfDocument::open_bytes(fixture_bytes("minimal.pdf")).unwrap();
    let doc_b = PdfDocument::open_bytes(fixture_bytes("flate.pdf")).unwrap();
    let size_a = doc_a.get_pages().unwrap()[0].media_box;
    let size_b = doc_b.get_pages().unwrap()[0].media_box;

    let merged = build_merged(&[(&doc_a, vec![1]), (&doc_b, vec![1])]).unwrap();
    let result = ContentEngine::open_bytes(merged).unwrap();
    let pages = result.document().get_pages().unwrap();
    assert_eq!(pages[0].media_box, size_a, "page 1 must keep doc A size");
    assert_eq!(pages[1].media_box, size_b, "page 2 must keep doc B size");
}
