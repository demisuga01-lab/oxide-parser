use std::io::Cursor;
use std::path::Path;

use crate::error::{OxideError, Result};
use crate::images::decoder::RawImage;

pub struct RenderQuality;

impl RenderQuality {
    /// Mean Squared Error per channel per pixel.
    pub fn mse(a: &RawImage, b: &RawImage) -> Result<f64> {
        Self::validate_same_layout("PSNR", a, b)?;
        let n = a.pixels.len() as f64;
        if n == 0.0 {
            return Ok(0.0);
        }

        let sum_sq: f64 = a
            .pixels
            .iter()
            .zip(b.pixels.iter())
            .map(|(&pa, &pb)| {
                let d = f64::from(pa) - f64::from(pb);
                d * d
            })
            .sum();
        Ok(sum_sq / n)
    }

    /// PSNR (Peak Signal-to-Noise Ratio).
    ///
    /// Reference for rendered PDF pages:
    /// - INFINITY: identical images
    /// - > 50 dB: visually identical
    /// - 40-50 dB: excellent, usually sub-pixel differences
    /// - 30-40 dB: good, visible on close inspection
    /// - 20-30 dB: acceptable
    /// - < 20 dB: significant differences
    pub fn psnr(reference: &RawImage, rendered: &RawImage) -> Result<f64> {
        let mse = Self::mse(reference, rendered)?;
        if mse < 1e-10 {
            return Ok(f64::INFINITY);
        }
        Ok(20.0 * 255.0_f64.log10() - 10.0 * mse.log10())
    }

    /// Per-pixel max-channel diff map; output is single-channel grayscale.
    pub fn diff_map(reference: &RawImage, rendered: &RawImage) -> Result<RawImage> {
        Self::validate_same_layout("diff_map", reference, rendered)?;

        let channels = usize::from(reference.channels);
        let pixel_count = reference.pixel_count();
        let mut diff = vec![0u8; pixel_count];

        for (pixel_idx, out) in diff.iter_mut().enumerate() {
            let start = pixel_idx * channels;
            let mut max_delta = 0u8;
            for channel_idx in 0..channels {
                let a = reference.pixels[start + channel_idx];
                let b = rendered.pixels[start + channel_idx];
                max_delta = max_delta.max(a.abs_diff(b));
            }
            *out = max_delta;
        }

        Ok(RawImage {
            width: reference.width,
            height: reference.height,
            channels: 1,
            bits_per_sample: 8,
            pixels: diff,
        })
    }

    /// Count pixels whose max-channel diff exceeds threshold.
    pub fn diff_pixel_count(a: &RawImage, b: &RawImage, threshold: u8) -> Result<usize> {
        let diff = Self::diff_map(a, b)?;
        Ok(diff.pixels.iter().filter(|&&v| v > threshold).count())
    }

    /// Write a RawImage as a PNG to disk, creating parent directories.
    pub fn write_golden(path: &Path, image: &RawImage) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| OxideError::MalformedPdf(format!("write_golden mkdir: {}", err)))?;
        }

        let png = crate::images::encoder::ImageEncoder::encode_png(image)?;
        std::fs::write(path, &png)
            .map_err(|err| OxideError::MalformedPdf(format!("write_golden write: {}", err)))?;
        Ok(())
    }

    /// Read a PNG from disk as a RawImage.
    pub fn read_golden(path: &Path) -> Result<RawImage> {
        let bytes = std::fs::read(path).map_err(|err| {
            OxideError::MalformedPdf(format!("read_golden '{}': {}", path.display(), err))
        })?;

        let decoder = png::Decoder::new(Cursor::new(&bytes));
        let mut reader = decoder
            .read_info()
            .map_err(|err| OxideError::MalformedPdf(format!("read_golden decode: {}", err)))?;
        let mut pixels = vec![0u8; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut pixels)
            .map_err(|err| OxideError::MalformedPdf(format!("read_golden frame: {}", err)))?;
        pixels.truncate(info.buffer_size());

        let channels = match info.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            other => {
                return Err(OxideError::UnsupportedFeature(format!(
                    "read_golden: unsupported PNG color type {:?}",
                    other
                )))
            }
        };

        Ok(RawImage {
            width: info.width,
            height: info.height,
            channels,
            bits_per_sample: 8,
            pixels,
        })
    }

    /// Compare against golden reference, or create it if absent.
    ///
    /// Set UPDATE_GOLDEN=1 to force regeneration.
    /// Returns PSNR value, or INFINITY when the golden was just created.
    pub fn compare_or_create_golden(golden_path: &Path, rendered: &RawImage) -> Result<f64> {
        let force = std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1");
        if force || !golden_path.exists() {
            log::info!("golden: creating reference at {}", golden_path.display());
            Self::write_golden(golden_path, rendered)?;
            return Ok(f64::INFINITY);
        }

        let reference = Self::read_golden(golden_path)?;
        let psnr = Self::psnr(&reference, rendered)?;
        log::debug!("golden PSNR={:.1} dB: {}", psnr, golden_path.display());
        Ok(psnr)
    }

    fn validate_same_layout(label: &str, a: &RawImage, b: &RawImage) -> Result<()> {
        if a.width != b.width || a.height != b.height || a.channels != b.channels {
            return Err(OxideError::MalformedPdf(format!(
                "{}: dimension mismatch: {}x{}x{} vs {}x{}x{}",
                label, a.width, a.height, a.channels, b.width, b.height, b.channels
            )));
        }

        let expected = a.byte_count();
        if a.pixels.len() != expected || b.pixels.len() != expected {
            return Err(OxideError::MalformedPdf(format!(
                "{}: pixel buffer length mismatch: expected {}, got {} and {}",
                label,
                expected,
                a.pixels.len(),
                b.pixels.len()
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_image(width: u32, height: u32, channels: u8, pixels: Vec<u8>) -> RawImage {
        RawImage {
            width,
            height,
            channels,
            bits_per_sample: 8,
            pixels,
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("oxide_{}_{}.png", name, std::process::id()))
    }

    #[test]
    fn psnr_of_identical_images_is_infinity() {
        let img = raw_image(
            2,
            2,
            3,
            vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128],
        );
        let psnr = RenderQuality::psnr(&img, &img).unwrap();
        assert!(psnr.is_infinite(), "identical images: PSNR=inf");
    }

    #[test]
    fn psnr_of_maximally_different_single_channel_images_is_zero_db() {
        let a = raw_image(1, 1, 1, vec![0]);
        let b = raw_image(1, 1, 1, vec![255]);
        let psnr = RenderQuality::psnr(&a, &b).unwrap();
        assert!(
            (psnr - 0.0).abs() < 0.01,
            "max diff = 0 dB PSNR, got {}",
            psnr
        );
    }

    #[test]
    fn psnr_dimension_mismatch_returns_err() {
        let a = raw_image(2, 2, 3, vec![0; 12]);
        let b = raw_image(3, 2, 3, vec![0; 18]);
        assert!(RenderQuality::psnr(&a, &b).is_err());
    }

    #[test]
    fn mse_of_identical_images_is_zero() {
        let img = raw_image(1, 1, 3, vec![100, 150, 200]);
        assert_eq!(RenderQuality::mse(&img, &img).unwrap(), 0.0);
    }

    #[test]
    fn mse_of_one_unit_diff_is_one() {
        let a = raw_image(1, 1, 1, vec![100]);
        let b = raw_image(1, 1, 1, vec![101]);
        let mse = RenderQuality::mse(&a, &b).unwrap();
        assert!((mse - 1.0).abs() < 1e-10, "got {}", mse);
    }

    #[test]
    fn psnr_for_tiny_error_in_4x4_image_is_high() {
        let a = raw_image(4, 4, 3, vec![128; 48]);
        let mut b = a.clone();
        b.pixels[0] = 129;
        let psnr = RenderQuality::psnr(&a, &b).unwrap();
        assert!(psnr > 50.0, "tiny error should give PSNR > 50 dB: {}", psnr);
    }

    #[test]
    fn diff_pixel_count_with_threshold_zero() {
        let a = raw_image(3, 1, 1, vec![0, 100, 200]);
        let b = raw_image(3, 1, 1, vec![0, 101, 200]);
        assert_eq!(RenderQuality::diff_pixel_count(&a, &b, 0).unwrap(), 1);
    }

    #[test]
    fn diff_pixel_count_with_threshold_ignores_small_diffs() {
        let a = raw_image(3, 1, 1, vec![0, 100, 200]);
        let b = raw_image(3, 1, 1, vec![3, 101, 195]);
        assert_eq!(
            RenderQuality::diff_pixel_count(&a, &b, 5).unwrap(),
            0,
            "all diffs <= 5, none exceed threshold"
        );
    }

    #[test]
    fn diff_map_uses_channel_maximum() {
        let a = raw_image(1, 1, 3, vec![200, 0, 0]);
        let b = raw_image(1, 1, 3, vec![100, 50, 0]);
        let diff = RenderQuality::diff_map(&a, &b).unwrap();
        assert_eq!(diff.channels, 1);
        assert_eq!(diff.pixels[0], 100);
    }

    #[test]
    fn write_golden_and_read_golden_round_trip() {
        let original = raw_image(
            2,
            2,
            3,
            vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128],
        );
        let tmpfile = temp_path("round_trip");
        let _ = std::fs::remove_file(&tmpfile);

        RenderQuality::write_golden(&tmpfile, &original).unwrap();
        assert!(tmpfile.exists(), "golden file should be created");

        let recovered = RenderQuality::read_golden(&tmpfile).unwrap();
        assert_eq!(recovered.width, 2);
        assert_eq!(recovered.height, 2);
        assert_eq!(recovered.channels, 3);
        assert_eq!(recovered.pixels, original.pixels);

        let _ = std::fs::remove_file(&tmpfile);
    }

    #[test]
    fn read_golden_returns_err_for_nonexistent_file() {
        let nonexistent = temp_path("definitely_missing");
        let _ = std::fs::remove_file(&nonexistent);
        assert!(RenderQuality::read_golden(&nonexistent).is_err());
    }

    #[test]
    fn psnr_range_for_small_differences() {
        let a = raw_image(4, 4, 3, vec![100; 48]);
        let mut b = a.clone();
        for v in &mut b.pixels {
            *v = v.saturating_add(3);
        }
        let psnr = RenderQuality::psnr(&a, &b).unwrap();
        assert!(
            psnr > 35.0 && psnr < 42.0,
            "uniform 3-unit noise should give ~38-39 dB PSNR: {}",
            psnr
        );
    }

    #[test]
    fn diff_map_supports_four_channel_image() {
        let a = raw_image(1, 1, 4, vec![200, 100, 50, 255]);
        let b = raw_image(1, 1, 4, vec![100, 100, 50, 200]);
        let diff = RenderQuality::diff_map(&a, &b).unwrap();
        assert_eq!(diff.pixels[0], 100);
    }

    #[test]
    fn mse_accumulates_correctly_across_multiple_pixels() {
        let a = raw_image(2, 2, 1, vec![100, 100, 100, 100]);
        let b = raw_image(2, 2, 1, vec![102, 102, 102, 102]);
        let mse = RenderQuality::mse(&a, &b).unwrap();
        assert!((mse - 4.0).abs() < 0.01, "got {}", mse);
    }

    #[test]
    fn diff_map_produces_grayscale_output_for_color_input() {
        let a = raw_image(2, 1, 3, vec![200, 100, 50, 0, 0, 0]);
        let b = raw_image(2, 1, 3, vec![200, 100, 50, 0, 0, 0]);
        let diff = RenderQuality::diff_map(&a, &b).unwrap();
        assert_eq!(diff.channels, 1);
        assert_eq!(diff.width, 2);
        assert_eq!(diff.height, 1);
        assert_eq!(diff.pixels.len(), 2);
        assert_eq!(diff.pixels[0], 0);
        assert_eq!(diff.pixels[1], 0);
    }

    #[test]
    fn compare_or_create_golden_creates_then_compares() {
        let path = temp_path("compare_or_create");
        let _ = std::fs::remove_file(&path);
        let img = raw_image(1, 1, 3, vec![100, 100, 100]);

        let created = RenderQuality::compare_or_create_golden(&path, &img).unwrap();
        assert!(created.is_infinite());
        assert!(path.exists());

        let compared = RenderQuality::compare_or_create_golden(&path, &img).unwrap();
        assert!(compared.is_infinite());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn compare_or_create_golden_does_not_overwrite_existing_without_update_flag() {
        let path = temp_path("no_overwrite");
        let _ = std::fs::remove_file(&path);
        let img = raw_image(1, 1, 3, vec![100, 100, 100]);
        RenderQuality::compare_or_create_golden(&path, &img).unwrap();
        let size_before = std::fs::metadata(&path).unwrap().len();

        let img2 = raw_image(1, 1, 3, vec![200, 200, 200]);
        let psnr = RenderQuality::compare_or_create_golden(&path, &img2).unwrap();
        assert!(psnr.is_finite());
        let size_after = std::fs::metadata(&path).unwrap().len();
        assert_eq!(size_before, size_after);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn psnr_formula_numerical_verification() {
        let white = raw_image(1, 1, 1, vec![255]);
        let black = raw_image(1, 1, 1, vec![0]);
        let mse = RenderQuality::mse(&white, &black).unwrap();
        let psnr = RenderQuality::psnr(&white, &black).unwrap();
        assert!((mse - 65025.0).abs() < 0.01, "got {}", mse);
        assert!(psnr.abs() < 0.1, "got {}", psnr);

        let a = raw_image(1, 1, 1, vec![0]);
        let b = raw_image(1, 1, 1, vec![1]);
        let psnr_1 = RenderQuality::psnr(&a, &b).unwrap();
        assert!((psnr_1 - 48.13).abs() < 0.1, "got {}", psnr_1);
    }
}
