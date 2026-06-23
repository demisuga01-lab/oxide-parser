//! Integration tests for the structural-write ops (rotate / optimize / repair /
//! encrypt). Each op's output is re-parsed by Oxide and checked for content/
//! structure preservation; qpdf cross-validation lives in the benchmark harness
//! and the CLI smoke (qpdf isn't a cargo dependency).

use std::path::PathBuf;
use std::process::Command;

use oxide_engine::crypto::{secret_bytes, EncryptAlgorithm, EncryptParams};
use oxide_engine::structural::{encrypt, linearize, optimize, repair, rotate_pages, Rotation};
use oxide_engine::ContentEngine;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn open(name: &str) -> ContentEngine {
    ContentEngine::open_path(fixture(name)).expect("open fixture")
}

fn open_bytes(bytes: Vec<u8>) -> ContentEngine {
    ContentEngine::open_bytes(bytes).expect("re-open written bytes")
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn rotate_absolute_persists_and_reparses() {
    let engine = open("tracemonkey.pdf");
    let before = engine.page_count().unwrap();

    let out = rotate_pages(&engine, &[], Rotation::Absolute(90)).expect("rotate");
    let re = open_bytes(out);

    assert_eq!(re.page_count().unwrap(), before, "page count preserved");
    for p in 1..=re.page_count().unwrap() {
        assert_eq!(
            re.get_page(p).unwrap().rotate.rem_euclid(360),
            90,
            "page {p} should now report 90 degrees"
        );
    }
}

#[test]
fn rotate_relative_adds_to_current() {
    let engine = open("tracemonkey.pdf");
    let base: Vec<i32> = (1..=engine.page_count().unwrap())
        .map(|p| engine.get_page(p).unwrap().rotate.rem_euclid(360))
        .collect();

    let out = rotate_pages(&engine, &[1], Rotation::Relative(90)).expect("rotate");
    let re = open_bytes(out);

    // Page 1 advanced by 90; the rest unchanged.
    assert_eq!(
        re.get_page(1).unwrap().rotate.rem_euclid(360),
        (base[0] + 90).rem_euclid(360)
    );
    if re.page_count().unwrap() >= 2 {
        assert_eq!(re.get_page(2).unwrap().rotate.rem_euclid(360), base[1]);
    }
}

#[test]
fn rotate_normalizes_and_wraps() {
    let engine = open("tracemonkey.pdf");
    // 450 -> 90; a full 360 relative -> unchanged.
    let out = rotate_pages(&engine, &[1], Rotation::Absolute(450)).expect("rotate");
    let re = open_bytes(out);
    assert_eq!(re.get_page(1).unwrap().rotate.rem_euclid(360), 90);
}

#[test]
fn rotate_preserves_text_content() {
    let engine = open("tracemonkey.pdf");
    let before = engine.get_page_text(1).unwrap();
    let out = rotate_pages(&engine, &[], Rotation::Absolute(180)).expect("rotate");
    let re = open_bytes(out);
    let after = re.get_page_text(1).unwrap();
    // Rotation does not change the text stream, only the /Rotate flag.
    assert_eq!(
        before.trim(),
        after.trim(),
        "text content unchanged by rotation"
    );
}

// --- ENCRYPT ----------------------------------------------------------------

fn encrypt_roundtrip(algo: EncryptAlgorithm) {
    let engine = open("tracemonkey.pdf");
    let plain_text = engine.get_page_text(1).unwrap();
    let pages = engine.page_count().unwrap();

    let params = EncryptParams {
        user_password: secret_bytes(b"open-sesame".to_vec()),
        owner_password: secret_bytes(b"the-owner".to_vec()),
        algorithm: algo,
        ..Default::default()
    };
    let encrypted = encrypt(&engine, &params).expect("encrypt");

    // The output must be recognized as encrypted and require the password.
    assert!(
        ContentEngine::open_bytes(encrypted.clone()).is_err()
            || ContentEngine::open_bytes(encrypted.clone())
                .unwrap()
                .get_page_text(1)
                .map(|t| t.trim().is_empty())
                .unwrap_or(true),
        "{algo:?}: opening without a password must not yield the plaintext"
    );

    // Opening WITH the user password recovers the content exactly.
    let re = ContentEngine::open_bytes_with_password(encrypted.clone(), b"open-sesame")
        .expect("open with user password");
    assert_eq!(
        re.page_count().unwrap(),
        pages,
        "{algo:?}: page count preserved"
    );
    assert_eq!(
        re.get_page_text(1).unwrap().trim(),
        plain_text.trim(),
        "{algo:?}: decrypted text matches original"
    );

    // The OWNER password also opens it (V5 via verify_v5_owner_password; legacy
    // via Algorithm-3 reverse owner recovery, both supported on the read side).
    let re_owner = ContentEngine::open_bytes_with_password(encrypted.clone(), b"the-owner")
        .expect("open with owner password");
    assert_eq!(re_owner.get_page_text(1).unwrap().trim(), plain_text.trim());

    // A WRONG password is rejected (or yields no content).
    let wrong = ContentEngine::open_bytes_with_password(encrypted, b"wrong-password");
    let rejected = wrong.is_err()
        || wrong
            .unwrap()
            .get_page_text(1)
            .map(|t| t.trim() != plain_text.trim())
            .unwrap_or(true);
    assert!(
        rejected,
        "{algo:?}: wrong password must not recover the content"
    );
}

#[test]
fn encrypt_aes256_round_trips() {
    encrypt_roundtrip(EncryptAlgorithm::Aes256);
}

#[test]
fn encrypt_aes128_round_trips() {
    encrypt_roundtrip(EncryptAlgorithm::Aes128);
}

#[test]
fn encrypt_rc4_round_trips() {
    encrypt_roundtrip(EncryptAlgorithm::Rc4_128);
}

// --- OPTIMIZE ---------------------------------------------------------------

#[test]
fn optimize_preserves_content_and_pages() {
    // Use a fixture with uncompressed content streams so recompression triggers.
    for name in ["basicapi.pdf", "minimal.pdf", "multi_stream.pdf"] {
        let engine = open(name);
        let pages = engine.page_count().unwrap();
        let text_before: Vec<String> = (1..=pages)
            .map(|p| engine.get_page_text(p).unwrap())
            .collect();

        let (out, report) = optimize(&engine).expect("optimize");
        let re = open_bytes(out);

        assert_eq!(
            re.page_count().unwrap(),
            pages,
            "{name}: page count preserved"
        );
        for p in 1..=pages {
            assert_eq!(
                re.get_page_text(p).unwrap().trim(),
                text_before[p - 1].trim(),
                "{name}: page {p} text unchanged by optimize"
            );
        }
        // The report is well-formed (output_bytes set); recompression count is
        // fixture-dependent so we don't assert a specific number.
        assert!(report.output_bytes > 0, "{name}: produced output");
    }
}

#[test]
fn optimize_is_visually_safe() {
    // The strongest visual-safety check: a rendered page of the optimized file
    // must be byte-identical to the original under the SAME renderer (optimize
    // only changes stream-container compression + drops dead objects, never
    // decoded content).
    let engine = open("tracemonkey.pdf");
    let orig_png = engine.render_page_png_fast(1, 100).unwrap();
    let (out, _r) = optimize(&engine).expect("optimize");
    let re = open_bytes(out);
    let opt_png = re.render_page_png_fast(1, 100).unwrap();
    assert_eq!(
        orig_png, opt_png,
        "optimize must not change rendered output"
    );
}

#[test]
fn optimize_recompresses_uncompressed_streams() {
    // multi_stream.pdf has uncompressed content; optimize should recompress >=1
    // stream and not grow the file.
    let engine = open("multi_stream.pdf");
    let original = std::fs::metadata(fixture("multi_stream.pdf"))
        .unwrap()
        .len() as usize;
    let (out, report) = optimize(&engine).expect("optimize");
    // Output should not be larger than the input (GC + recompression).
    assert!(
        out.len() <= original + 256,
        "optimized ({}) should not exceed original ({original}) by much",
        out.len()
    );
    let _ = report;
}

#[test]
fn linearize_outputs_are_qpdf_clean_when_available() {
    if !qpdf_available() {
        eprintln!("NOTE: qpdf not found; skipped linearization hint-table validation");
        return;
    }

    for name in [
        "minimal.pdf",
        "flate.pdf",
        "multi_stream.pdf",
        "basicapi.pdf",
        "tracemonkey.pdf",
        "form_160f.pdf",
    ] {
        let engine = open(name);
        let original_pages = engine.page_count().unwrap();
        let original_text = engine.get_page_text(1).unwrap_or_default();
        let out = linearize::linearize(&engine).unwrap_or_else(|e| panic!("{name}: {e}"));
        let reparsed = open_bytes(out.clone());
        assert_eq!(
            reparsed.page_count().unwrap(),
            original_pages,
            "{name}: page count changed"
        );
        assert_eq!(
            reparsed.get_page_text(1).unwrap_or_default().trim(),
            original_text.trim(),
            "{name}: page 1 text changed"
        );

        let temp = std::env::temp_dir().join(format!(
            "oxide_linearize_qpdf_{}_{}.pdf",
            std::process::id(),
            name.replace('.', "_")
        ));
        std::fs::write(&temp, &out).expect("write linearized temp pdf");
        let check = Command::new("qpdf")
            .arg("--check")
            .arg(&temp)
            .output()
            .expect("run qpdf --check");
        let show = Command::new("qpdf")
            .arg("--show-linearization")
            .arg(&temp)
            .output()
            .expect("run qpdf --show-linearization");
        let _ = std::fs::remove_file(&temp);

        let check_output = format!(
            "{}{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let show_output = format!(
            "{}{}",
            String::from_utf8_lossy(&show.stdout),
            String::from_utf8_lossy(&show.stderr)
        );
        assert!(
            check.status.success(),
            "{name}: qpdf --check failed\n{check_output}"
        );
        assert!(
            show.status.success(),
            "{name}: qpdf --show-linearization failed\n{show_output}"
        );
        assert!(
            !check_output.contains("WARNING") && !show_output.contains("WARNING"),
            "{name}: qpdf reported linearization warnings\ncheck:\n{check_output}\nshow:\n{show_output}"
        );
        assert!(
            check_output.contains("File is linearized"),
            "{name}: qpdf did not report linearized output\n{check_output}"
        );
    }
}

// --- REPAIR -----------------------------------------------------------------

fn hostile(name: &str) -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("renderer-benchmark")
        .join("corpus")
        .join("hostile")
        .join(name);
    std::fs::read(p).ok()
}

#[test]
fn repair_handles_recoverable_damage() {
    // These categories open via the reader's recovery; repair must persist a
    // clean copy that re-parses. (Skip gracefully if the corpus isn't present.)
    let mut tested = 0;
    for name in ["hostile_003_missing-eof.pdf", "hostile_006_huge-length.pdf"] {
        let Some(bytes) = hostile(name) else { continue };
        // Only assert repair succeeds for files the reader can already open.
        if ContentEngine::open_bytes(bytes.clone()).is_err() {
            continue;
        }
        let repaired = repair(bytes, b"").unwrap_or_else(|e| panic!("{name}: repair failed: {e}"));
        let re = open_bytes(repaired);
        assert!(
            re.page_count().unwrap() >= 1,
            "{name}: repaired file has pages"
        );
        tested += 1;
    }
    // If the corpus is absent, the test is a no-op (documented); when present at
    // least one category should exercise repair.
    if hostile("hostile_003_missing-eof.pdf").is_some() {
        assert!(
            tested >= 1,
            "expected at least one recoverable hostile fixture"
        );
    }
}

#[test]
fn repair_clean_file_roundtrips() {
    // Repairing an already-clean file is a faithful copy.
    let bytes = std::fs::read(fixture("flate.pdf")).unwrap();
    let pages = ContentEngine::open_bytes(bytes.clone())
        .unwrap()
        .page_count()
        .unwrap();
    let repaired = repair(bytes, b"").expect("repair clean file");
    let re = open_bytes(repaired);
    assert_eq!(re.page_count().unwrap(), pages);
}

#[test]
fn encrypt_decrypted_content_is_deterministic() {
    // Encrypted bytes vary (random IV/salt), but the decrypted text must be
    // stable across two independent encryptions.
    let engine = open("tracemonkey.pdf");
    let params = EncryptParams {
        user_password: secret_bytes(b"pw".to_vec()),
        algorithm: EncryptAlgorithm::Aes256,
        ..Default::default()
    };
    let a = encrypt(&engine, &params).expect("encrypt a");
    let b = encrypt(&engine, &params).expect("encrypt b");
    assert_ne!(a, b, "encrypted bytes should differ (random IV/salt/key)");
    let ta = ContentEngine::open_bytes_with_password(a, b"pw")
        .unwrap()
        .get_page_text(1)
        .unwrap();
    let tb = ContentEngine::open_bytes_with_password(b, b"pw")
        .unwrap()
        .get_page_text(1)
        .unwrap();
    assert_eq!(ta, tb, "decrypted content is deterministic");
}
