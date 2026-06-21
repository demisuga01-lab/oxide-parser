//! Unit tests for the page-classifier decision logic. The `decide` seam is
//! tested on raw signals (no PDF needed); engine-level classification on real
//! synthetic PDFs lives in `crates/engine/tests/page_classifier.rs`.

use super::*;

fn cfg() -> ClassifyConfig {
    ClassifyConfig::default()
}

#[test]
fn digital_born_text_no_image() {
    // Plenty of text, no image.
    let (src, conf) = decide(500, 0.20, 0.0, false, &cfg());
    assert_eq!(src, PageSource::DigitalBorn);
    assert!(conf >= 0.9, "strong text coverage → high confidence");
}

#[test]
fn sparse_text_no_image_is_low_confidence_digital_born() {
    // Some text but below the strong-coverage bar, no image.
    let (src, conf) = decide(40, 0.01, 0.0, false, &cfg());
    assert_eq!(src, PageSource::DigitalBorn);
    assert!((0.7..0.9).contains(&conf));
}

#[test]
fn full_page_image_no_text_is_scanned() {
    let (src, conf) = decide(0, 0.0, 0.98, false, &cfg());
    assert_eq!(src, PageSource::Scanned);
    assert!(conf >= 0.9);
}

#[test]
fn full_page_image_with_invisible_text_is_searchable_scan() {
    // The common scanner-OCR case: full-page image + an invisible text layer.
    let (src, conf) = decide(800, 0.10, 0.99, true, &cfg());
    assert_eq!(src, PageSource::DigitalBornOverImage, "uses the existing text layer");
    assert!(conf >= 0.9, "invisible-text searchable scan is near-certain");
}

#[test]
fn full_page_image_with_visible_text_is_text_over_image() {
    // Visible text over a full-page image (rarer): still text-first, never OCR.
    let (src, conf) = decide(300, 0.08, 0.95, false, &cfg());
    assert_eq!(src, PageSource::DigitalBornOverImage);
    assert!((0.8..0.95).contains(&conf));
}

#[test]
fn empty_page_is_scanned_low_confidence() {
    let (src, conf) = decide(0, 0.0, 0.0, false, &cfg());
    assert_eq!(src, PageSource::Scanned);
    assert!(conf <= 0.6, "an empty page is an uncertain scan");
}

#[test]
fn tiny_image_does_not_trigger_scanned() {
    // A small embedded figure (logo) on a text page must not flip it to scanned.
    let (src, _conf) = decide(1200, 0.30, 0.05, false, &cfg());
    assert_eq!(src, PageSource::DigitalBorn);
}

#[test]
fn thresholds_are_configurable() {
    let strict = ClassifyConfig {
        full_page_image_frac: 0.95,
        ..ClassifyConfig::default()
    };
    // An image at 0.80 is "full-page" by default but not under the strict config.
    let (default_src, _) = decide(0, 0.0, 0.80, false, &ClassifyConfig::default());
    assert_eq!(default_src, PageSource::Scanned);
    let (strict_src, _) = decide(0, 0.0, 0.80, false, &strict);
    assert_eq!(strict_src, PageSource::Scanned, "still no text → scanned either way");
    // With text present, the strict config keeps it digital-born (image < 0.95).
    let (strict_text, _) = decide(400, 0.10, 0.80, false, &strict);
    assert_eq!(strict_text, PageSource::DigitalBorn);
}

#[test]
fn is_digital_born_helper() {
    assert!(PageSource::DigitalBorn.is_digital_born());
    assert!(PageSource::DigitalBornOverImage.is_digital_born());
    assert!(!PageSource::Scanned.is_digital_born());
}
