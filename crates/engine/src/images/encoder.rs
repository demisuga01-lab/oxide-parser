use crate::error::{OxideError, Result};
use crate::images::decoder::RawImage;
use crate::images::locator::{ImageLocator, ImageReference};
use crate::reader::PdfReader;

#[derive(Debug, Clone, PartialEq)]
pub enum ImageOutputFormat {
    Png,
    Jpeg,
    /// Keep original compressed bytes when possible.
    Original,
}

impl ImageOutputFormat {
    pub fn file_extension(&self) -> &'static str {
        match self {
            ImageOutputFormat::Png => "png",
            ImageOutputFormat::Jpeg => "jpg",
            ImageOutputFormat::Original => "bin",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            ImageOutputFormat::Png => "image/png",
            ImageOutputFormat::Jpeg => "image/jpeg",
            ImageOutputFormat::Original => "application/octet-stream",
        }
    }
}

pub struct ImageEncoder;

impl ImageEncoder {
    /// Encode a RawImage as PNG bytes.
    pub fn encode_png(image: &RawImage) -> Result<Vec<u8>> {
        use png::{BitDepth, ColorType, Encoder};

        let color_type = match image.channels {
            1 => ColorType::Grayscale,
            3 => ColorType::Rgb,
            4 => ColorType::Rgba,
            n => {
                return Err(OxideError::UnsupportedFeature(format!(
                    "PNG encoding: unsupported channel count {n}"
                )))
            }
        };

        let mut out = Vec::new();
        {
            let mut encoder = Encoder::new(&mut out, image.width, image.height);
            encoder.set_color(color_type);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_compression(png::Compression::Default);
            let mut writer = encoder
                .write_header()
                .map_err(|e| OxideError::MalformedPdf(format!("PNG header error: {e}")))?;
            writer
                .write_image_data(&image.pixels)
                .map_err(|e| OxideError::MalformedPdf(format!("PNG encode error: {e}")))?;
        }
        Ok(out)
    }

    /// Encode a RawImage as PNG with fast compression.
    pub fn encode_png_fast(image: &RawImage) -> Result<Vec<u8>> {
        use png::{BitDepth, ColorType, Compression, Encoder};

        let color_type = match image.channels {
            1 => ColorType::Grayscale,
            3 => ColorType::Rgb,
            4 => ColorType::Rgba,
            n => {
                return Err(OxideError::UnsupportedFeature(format!(
                    "PNG fast encode: unsupported channel count {n}"
                )))
            }
        };

        let mut out = Vec::new();
        {
            let mut encoder = Encoder::new(&mut out, image.width, image.height);
            encoder.set_color(color_type);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_compression(Compression::Fast);
            let mut writer = encoder
                .write_header()
                .map_err(|e| OxideError::MalformedPdf(format!("PNG fast encode header: {e}")))?;
            writer
                .write_image_data(&image.pixels)
                .map_err(|e| OxideError::MalformedPdf(format!("PNG fast encode data: {e}")))?;
        }
        Ok(out)
    }

    /// Encode a RawImage as JPEG bytes.
    pub fn encode_jpeg(image: &RawImage, quality: u8) -> Result<Vec<u8>> {
        use jpeg_encoder::{ColorType, Encoder};

        let quality = quality.clamp(1, 100);
        let color_type = match image.channels {
            1 => ColorType::Luma,
            3 => ColorType::Rgb,
            n => {
                return Err(OxideError::UnsupportedFeature(format!(
                    "JPEG encoding: unsupported channel count {n} (convert to RGB first)"
                )))
            }
        };
        let width = u16::try_from(image.width).map_err(|_| {
            OxideError::UnsupportedFeature(format!(
                "JPEG encoding: image width {} exceeds u16::MAX",
                image.width
            ))
        })?;
        let height = u16::try_from(image.height).map_err(|_| {
            OxideError::UnsupportedFeature(format!(
                "JPEG encoding: image height {} exceeds u16::MAX",
                image.height
            ))
        })?;

        let mut output = Vec::new();
        let encoder = Encoder::new(&mut output, quality);
        encoder
            .encode(&image.pixels, width, height, color_type)
            .map_err(|e| OxideError::MalformedPdf(format!("JPEG encode error: {e}")))?;
        Ok(output)
    }

    /// TODO(webp): implement using the `webp` crate once cmake/C toolchain is available.
    pub fn encode_webp(_image: &RawImage, _quality: u8) -> Result<Vec<u8>> {
        Err(OxideError::UnsupportedFeature(
            "WebP encoding is not yet available. Use PNG or JPEG output instead.".to_string(),
        ))
    }

    /// Keep original stream bytes when the source encoding matches the target.
    pub fn keep_original(
        image: &ImageReference,
        reader: &PdfReader,
        format: &ImageOutputFormat,
    ) -> Result<Option<(Vec<u8>, &'static str)>> {
        let raw = match ImageLocator::get_stream_bytes(image, reader)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };

        match format {
            ImageOutputFormat::Original => {
                let ext = if is_dct_only(&image.filter) {
                    "jpg"
                } else if image.filter.is_empty() {
                    "png"
                } else {
                    "bin"
                };
                Ok(Some((raw, ext)))
            }
            ImageOutputFormat::Jpeg => {
                if is_dct_only(&image.filter) {
                    Ok(Some((raw, "jpg")))
                } else {
                    Ok(None)
                }
            }
            ImageOutputFormat::Png => Ok(None),
        }
    }

    /// Encode a decoded RawImage to the requested format.
    pub fn encode(
        image: &RawImage,
        format: &ImageOutputFormat,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        let quality = quality.unwrap_or(85);
        match format {
            ImageOutputFormat::Png => Self::encode_png(image),
            ImageOutputFormat::Jpeg => Self::encode_jpeg(image, quality),
            ImageOutputFormat::Original => Self::encode_png(image),
        }
    }
}

fn is_dct_only(filters: &[String]) -> bool {
    matches!(filters, [only] if only == "DCTDecode" || only == "DCT")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_png_one_pixel_gray() {
        let img = RawImage {
            width: 1,
            height: 1,
            channels: 1,
            bits_per_sample: 8,
            pixels: vec![128u8],
        };
        let png_bytes = ImageEncoder::encode_png(&img).unwrap();
        assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
        assert_eq!(&png_bytes[12..16], b"IHDR");
        assert!(png_bytes.len() >= 67);

        let decoder = png::Decoder::new(std::io::Cursor::new(&png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut decoded = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!(info.width, 1);
        assert_eq!(info.height, 1);
        assert_eq!(&decoded[..info.buffer_size()], &[128u8]);
    }

    #[test]
    fn encode_png_rgb_image() {
        let img = RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        };
        let png_bytes = ImageEncoder::encode_png(&img).unwrap();
        assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn encode_png_fast_rgb_image() {
        let img = RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        };
        let png_bytes = ImageEncoder::encode_png_fast(&img).unwrap();
        assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn encode_jpeg_rgb_image() {
        let img = RawImage {
            width: 4,
            height: 4,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![128u8; 48],
        };
        let jpeg_bytes = ImageEncoder::encode_jpeg(&img, 85).unwrap();
        assert_eq!(&jpeg_bytes[..2], &[0xFF, 0xD8]);
        assert_eq!(&jpeg_bytes[jpeg_bytes.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn encode_jpeg_rejects_four_channel_input() {
        let img = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![0u8; 4],
        };
        assert!(ImageEncoder::encode_jpeg(&img, 85).is_err());
    }

    #[test]
    fn encode_webp_returns_unsupported_feature() {
        let img = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0u8; 3],
        };
        assert!(matches!(
            ImageEncoder::encode_webp(&img, 80),
            Err(OxideError::UnsupportedFeature(_))
        ));
    }

    #[test]
    fn jpeg_quality_affects_output_size() {
        let pixels: Vec<u8> = (0..(16 * 16 * 3))
            .map(|i| (i as u8).wrapping_mul(3))
            .collect();
        let img = RawImage {
            width: 16,
            height: 16,
            channels: 3,
            bits_per_sample: 8,
            pixels,
        };
        let low_q = ImageEncoder::encode_jpeg(&img, 10).unwrap();
        let high_q = ImageEncoder::encode_jpeg(&img, 95).unwrap();
        assert!(high_q.len() > low_q.len() || high_q.len() >= 100);
    }

    #[test]
    fn image_output_format_extensions_and_mime_types() {
        assert_eq!(ImageOutputFormat::Png.file_extension(), "png");
        assert_eq!(ImageOutputFormat::Jpeg.file_extension(), "jpg");
        assert_eq!(ImageOutputFormat::Png.mime_type(), "image/png");
        assert_eq!(ImageOutputFormat::Jpeg.mime_type(), "image/jpeg");
    }

    #[test]
    fn keep_original_logic_does_not_match_flate_as_jpeg() {
        let img_ref = ImageReference {
            page_number: 1,
            xobject_name: "Im1".to_string(),
            object_number: 0,
            generation_number: 0,
            width: 1,
            height: 1,
            bits_per_component: 8,
            color_space: "DeviceRGB".to_string(),
            filter: vec!["FlateDecode".to_string()],
            is_inline: false,
            is_mask: false,
            is_smask: false,
        };
        assert!(!is_dct_only(&img_ref.filter));
    }
}
