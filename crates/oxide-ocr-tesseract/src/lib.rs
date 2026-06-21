//! Tesseract OCR backend for `oxide-engine`.
//!
//! This crate implements [`oxide_engine::OcrEngine`] by driving the **external**
//! `tesseract` program as a child process — it links **no C library**. The core
//! engine stays pure Rust and depends only on the [`OcrEngine`] trait; this
//! optional crate is the concrete backend a binary opts into.
//!
//! # How it works
//!
//! For each preprocessed page image (a single-channel [`OcrImage`]) the backend:
//! 1. writes the image to a temporary **PGM** file (a trivial, dependency-free
//!    grayscale format `tesseract` reads natively),
//! 2. invokes `tesseract <in> stdout -l <langs> [--psm N] tsv`, asking for
//!    **TSV** word boxes + confidences (passed as an argument *vector* — never a
//!    shell string — so there is no shell-injection surface),
//! 3. parses the TSV into [`OcrWord`]s (text + pixel bbox + 0..1 confidence +
//!    line id),
//! 4. cleans up the temp file (even on error, via an RAII guard).
//!
//! # Robustness
//!
//! - A missing/undiscoverable `tesseract` binary yields a clear, actionable
//!   [`OxideError::UnsupportedFeature`] — never a panic.
//! - The subprocess is bounded by a configurable **timeout**; on expiry the
//!   child is killed and an error returned.
//! - A non-zero exit or unparseable output is a clean `Err`, so the caller can
//!   degrade the page gracefully.
//!
//! # Determinism
//!
//! Tesseract is deterministic for a fixed input + version; the engine version is
//! recorded via [`OcrEngine::version`] for reproducibility.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use oxide_engine::{OcrEngine, OcrImage, OcrOptions, OcrPage, OcrWord, OxideError, Result};

/// Default per-page OCR subprocess timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The Tesseract-backed [`OcrEngine`]. Construct with [`TesseractEngine::new`]
/// (auto-discovers `tesseract` on `PATH`) or [`TesseractEngine::with_path`].
pub struct TesseractEngine {
    /// Path to the `tesseract` executable.
    binary: PathBuf,
    /// Cached version string (from `tesseract --version`), if it could be read.
    version: Option<String>,
    /// Per-invocation timeout.
    timeout: Duration,
}

impl TesseractEngine {
    /// Discover `tesseract` on `PATH` and probe its version. Returns an
    /// actionable error if the binary is not found or not runnable.
    pub fn new() -> Result<Self> {
        Self::with_path("tesseract")
    }

    /// Use an explicit `tesseract` path (or a bare name resolved via `PATH`).
    /// Probes `--version` to confirm the binary is runnable.
    pub fn with_path(path: impl Into<PathBuf>) -> Result<Self> {
        let binary = path.into();
        let version = probe_version(&binary).map_err(|e| {
            OxideError::UnsupportedFeature(format!(
                "could not run tesseract at {:?}: {e}. Install Tesseract OCR and its language \
                 data (e.g. `tesseract-ocr` + `tesseract-ocr-eng`) and ensure the `tesseract` \
                 binary is on PATH, or pass an explicit path.",
                binary
            ))
        })?;
        Ok(TesseractEngine {
            binary,
            version: Some(version),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Override the per-page subprocess timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The discovered binary path.
    pub fn binary_path(&self) -> &Path {
        &self.binary
    }
}

impl OcrEngine for TesseractEngine {
    fn recognize(&self, image: &OcrImage, opts: &OcrOptions) -> Result<OcrPage> {
        if !image.is_valid() {
            return Err(OxideError::ParseError(
                "OCR image is empty or malformed".to_string(),
            ));
        }

        // Write the gray image to a temp PGM (RAII-cleaned).
        let tmp = TempPgm::write(image)?;

        // Build the argument vector (NO shell — no injection surface).
        let langs = if opts.languages.is_empty() {
            "eng".to_string()
        } else {
            opts.languages.join("+")
        };
        let mut args: Vec<String> = vec![
            tmp.path.to_string_lossy().into_owned(),
            "stdout".to_string(),
            "-l".to_string(),
            langs,
        ];
        if let Some(psm) = opts.psm {
            args.push("--psm".to_string());
            args.push(psm.to_string());
        }
        // DPI hint helps Tesseract's internal scaling decisions.
        if opts.dpi > 0 {
            args.push("--dpi".to_string());
            args.push(opts.dpi.to_string());
        }
        // The output "configfile": `tsv` emits the word-box TSV we parse.
        args.push("tsv".to_string());

        let stdout = run_with_timeout(&self.binary, &args, self.timeout)?;
        let words = parse_tsv(&stdout);
        Ok(OcrPage::new(words))
    }

    fn name(&self) -> &str {
        "tesseract"
    }

    fn version(&self) -> Option<String> {
        self.version.clone()
    }
}

/// Probe `tesseract --version`, returning the first line's version token.
fn probe_version(binary: &Path) -> std::io::Result<String> {
    let out = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    // tesseract prints "tesseract v5.5.0..." to stdout (some builds stderr).
    let text = if !out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stdout)
    } else {
        String::from_utf8_lossy(&out.stderr)
    };
    let first = text.lines().next().unwrap_or("").trim();
    let ver = first
        .split_whitespace()
        .nth(1)
        .unwrap_or(first)
        .trim_start_matches('v')
        .to_string();
    if ver.is_empty() {
        Ok("unknown".to_string())
    } else {
        Ok(ver)
    }
}

/// Run `binary args...`, capturing stdout/stderr, killing the child if it
/// exceeds `timeout`. Returns the captured stdout bytes on a zero exit.
///
/// stdout and stderr are drained on dedicated threads so a full pipe buffer can
/// never deadlock the child, while the main thread polls for completion against
/// the deadline.
fn run_with_timeout(binary: &Path, args: &[String], timeout: Duration) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            OxideError::UnsupportedFeature(format!(
                "failed to launch tesseract at {binary:?}: {e}"
            ))
        })?;

    // Move the pipe handles onto reader threads so neither can block the child.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(OxideError::Cancelled(format!(
                        "tesseract exceeded the {}s OCR timeout",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    if !status.success() {
        let err = String::from_utf8_lossy(&stderr);
        return Err(OxideError::ParseError(format!(
            "tesseract exited with {status}: {}",
            err.trim()
        )));
    }
    Ok(stdout)
}

/// Process-unique counter so concurrent page OCR (rayon) does not collide on
/// temp filenames without needing randomness (which the engine forbids in some
/// contexts and which would hurt reproducibility of the filename).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// An RAII temp PGM file: written on construction, deleted on drop.
struct TempPgm {
    path: PathBuf,
}

impl TempPgm {
    fn write(image: &OcrImage) -> Result<Self> {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut path = std::env::temp_dir();
        path.push(format!("oxide-ocr-{pid}-{seq}.pgm"));

        let mut f = fs::File::create(&path)?;
        // Binary PGM (P5): "P5\n<w> <h>\n255\n" + raw bytes.
        write!(f, "P5\n{} {}\n255\n", image.width, image.height)?;
        f.write_all(&image.gray)?;
        f.flush()?;
        Ok(TempPgm { path })
    }
}

impl Drop for TempPgm {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Parse Tesseract TSV output into [`OcrWord`]s.
///
/// TSV columns (header row present): `level page_num block_num par_num line_num
/// word_num left top width height conf text`. We keep only `level == 5` (word)
/// rows with non-empty text and confidence `>= 0`. `conf` is 0..100 → 0..1.
/// `line_id` is a stable per-page line index synthesized from
/// `(block, par, line)`.
fn parse_tsv(bytes: &[u8]) -> Vec<OcrWord> {
    let text = String::from_utf8_lossy(bytes);
    let mut words = Vec::new();
    let mut line_keys: Vec<(i64, i64, i64)> = Vec::new();

    for line in text.lines() {
        // Skip the header (starts with "level").
        if line.starts_with("level") {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let level: i64 = cols[0].parse().unwrap_or(-1);
        if level != 5 {
            continue; // not a word row
        }
        let block: i64 = cols[2].parse().unwrap_or(0);
        let par: i64 = cols[3].parse().unwrap_or(0);
        let line_num: i64 = cols[4].parse().unwrap_or(0);
        let left: f64 = cols[6].parse().unwrap_or(0.0);
        let top: f64 = cols[7].parse().unwrap_or(0.0);
        let width: f64 = cols[8].parse().unwrap_or(0.0);
        let height: f64 = cols[9].parse().unwrap_or(0.0);
        let conf: f32 = cols[10].parse().unwrap_or(-1.0);
        let word = cols[11..].join("\t"); // text may itself contain tabs? rare; rejoin defensively

        if conf < 0.0 || word.trim().is_empty() {
            continue;
        }

        // Synthesize a stable per-page line id.
        let key = (block, par, line_num);
        let line_id = match line_keys.iter().position(|k| *k == key) {
            Some(i) => i as u32,
            None => {
                line_keys.push(key);
                (line_keys.len() - 1) as u32
            }
        };

        words.push(OcrWord {
            text: word,
            bbox: [left, top, left + width, top + height],
            confidence: (conf / 100.0).clamp(0.0, 1.0),
            line_id: Some(line_id),
        });
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative Tesseract TSV fragment (header + a couple of word rows
    /// plus the non-word level rows it interleaves). Parsing must keep only the
    /// words, decode their boxes/confidence, and group lines.
    const SAMPLE_TSV: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t600\t800\t-1\t\n\
2\t1\t1\t0\t0\t0\t20\t30\t560\t40\t-1\t\n\
3\t1\t1\t1\t0\t0\t20\t30\t560\t40\t-1\t\n\
4\t1\t1\t1\t1\t0\t20\t30\t300\t40\t-1\t\n\
5\t1\t1\t1\t1\t1\t20\t30\t140\t40\t96\tHello\n\
5\t1\t1\t1\t1\t2\t170\t30\t150\t40\t91\tworld\n\
4\t1\t1\t1\t2\t0\t20\t90\t300\t40\t-1\t\n\
5\t1\t1\t1\t2\t1\t20\t90\t120\t40\t-1\t\n\
5\t1\t1\t1\t2\t2\t150\t90\t160\t40\t88\tSecond\n";

    #[test]
    fn tsv_parses_words_boxes_and_confidence() {
        let words = parse_tsv(SAMPLE_TSV.as_bytes());
        assert_eq!(words.len(), 3, "should keep 3 confident word rows");

        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[0].bbox, [20.0, 30.0, 160.0, 70.0]);
        assert!((words[0].confidence - 0.96).abs() < 1e-6);
        assert_eq!(words[0].line_id, Some(0));

        assert_eq!(words[1].text, "world");
        assert_eq!(words[1].line_id, Some(0), "same TSV line groups together");

        assert_eq!(words[2].text, "Second");
        assert_eq!(words[2].line_id, Some(1), "next TSV line is a new line id");
    }

    #[test]
    fn tsv_skips_negative_confidence_and_empty() {
        // A word row with conf -1 (no text recognized) is dropped.
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t-1\t\n\
5\t1\t1\t1\t1\t2\t0\t0\t10\t10\t50\t   \n";
        let words = parse_tsv(tsv.as_bytes());
        assert!(words.is_empty());
    }

    #[test]
    fn missing_binary_is_actionable_error() {
        let err = TesseractEngine::with_path("definitely-not-a-real-binary-xyz")
            .err()
            .expect("should fail to find the binary");
        let msg = err.to_string();
        assert!(
            msg.contains("Install Tesseract") || msg.contains("could not run tesseract"),
            "error should be actionable, got: {msg}"
        );
    }

    #[test]
    fn temp_pgm_is_cleaned_on_drop() {
        let img = OcrImage::white(4, 4);
        let path = {
            let tmp = TempPgm::write(&img).expect("write pgm");
            assert!(tmp.path.exists());
            tmp.path.clone()
        };
        assert!(!path.exists(), "temp PGM must be removed on drop");
    }
}
