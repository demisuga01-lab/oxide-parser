#![cfg(feature = "fuzzing")]

use std::fs;
use std::process::Command;

use oxide_engine::{fuzz::structured_pdf_samples_for_seed, ContentEngine};

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn structured_fuzz_samples_parse_and_qpdf_accepts_when_available() {
    let seed =
        b"structured-pdf-seed-v1 q q cm re W BT Tf Tj annots pdfa linearize render edit signature";
    let samples = structured_pdf_samples_for_seed(seed);
    assert!(samples.len() >= 2, "expected authored and raw samples");

    let check_qpdf = qpdf_available();
    for (index, bytes) in samples.iter().enumerate() {
        let engine = ContentEngine::open_bytes(bytes.clone()).expect("generated PDF parses");
        assert!(
            engine.page_count().unwrap() >= 1,
            "sample {index} has pages"
        );

        if check_qpdf {
            let path = std::env::temp_dir().join(format!(
                "oxide-structured-fuzz-sample-{}-{index}.pdf",
                std::process::id()
            ));
            fs::write(&path, bytes).expect("write generated sample");
            let output = Command::new("qpdf")
                .arg("--check")
                .arg(&path)
                .output()
                .expect("run qpdf");
            let _ = fs::remove_file(&path);
            assert!(
                output.status.success(),
                "qpdf rejected sample {index}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
