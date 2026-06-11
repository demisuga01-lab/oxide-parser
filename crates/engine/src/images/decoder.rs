use std::io::Cursor;

use crate::error::{OxideError, Result};
use crate::filters::{apply_filter_bytes, decode_stream_lossless, StreamDecodeStatus};
use crate::images::locator::{ImageLocator, ImageReference};
use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;

/// Decoded image data: always 8 bits per channel, channels interleaved.
#[derive(Debug, Clone)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// Number of channels per pixel.
    pub channels: u8,
    /// Always 8 after decoding.
    pub bits_per_sample: u8,
    /// Raw pixel bytes.
    pub pixels: Vec<u8>,
}

impl RawImage {
    /// Total number of pixels.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Total bytes in the pixel buffer.
    pub fn byte_count(&self) -> usize {
        self.pixel_count() * self.channels as usize
    }

    /// Row stride in bytes.
    pub fn row_stride(&self) -> usize {
        self.width as usize * self.channels as usize
    }

    /// Get a single pixel as a slice of channel values.
    pub fn pixel(&self, x: usize, y: usize) -> &[u8] {
        let channels = self.channels as usize;
        let start = y
            .saturating_mul(self.row_stride())
            .saturating_add(x.saturating_mul(channels));
        let end = start.saturating_add(channels);
        if channels == 0 || end > self.pixels.len() {
            &[]
        } else {
            &self.pixels[start..end]
        }
    }

    /// True if the image is grayscale.
    pub fn is_grayscale(&self) -> bool {
        self.channels == 1
    }

    /// True if the image is RGB.
    pub fn is_rgb(&self) -> bool {
        self.channels == 3
    }

    /// Verify pixel buffer length matches dimensions x channels.
    pub fn is_valid(&self) -> bool {
        self.pixels.len() == self.byte_count()
            && self.width > 0
            && self.height > 0
            && self.channels > 0
    }
}

pub struct ImageDecoder;

impl ImageDecoder {
    /// Decode an image from its PDF ImageReference.
    pub fn decode(image: &ImageReference, reader: &PdfReader) -> Result<RawImage> {
        if image.object_number == 0 {
            return Err(OxideError::UnsupportedFeature(
                "inline image decoding via decode() is not supported; use decode_inline() with the raw pixel bytes"
                    .to_string(),
            ));
        }

        let obj = reader.get_object(image.object_number, image.generation_number)?;
        let (dict, raw) = match obj {
            PdfObject::Stream { dict, raw } => (dict, raw),
            _ => {
                return Err(OxideError::MalformedPdf(format!(
                    "image object {} is not a stream",
                    image.object_number
                )))
            }
        };

        let stream_obj = PdfObject::Stream {
            dict: dict.clone(),
            raw,
        };
        let decoded = decode_stream_lossless(&stream_obj, reader)?;

        match decoded.status {
            StreamDecodeStatus::Complete => Self::build_raw_image(
                decoded.data,
                image.width,
                image.height,
                image.bits_per_component,
                &image.color_space,
                &dict,
                Some(reader),
            ),
            StreamDecodeStatus::StoppedAtImageFilter(filter) => {
                Self::decode_remaining_image_filter(&decoded.data, &filter, image, reader, &dict)
            }
        }
    }

    /// Decode an inline image from its raw pixel bytes and parameters.
    pub fn decode_inline(
        pixel_data: &[u8],
        width: u32,
        height: u32,
        bpc: u8,
        color_space: &str,
        filter: &[&str],
        decode_parms: Option<&PdfDictionary>,
    ) -> Result<RawImage> {
        let decompressed = Self::apply_filters_direct(pixel_data, filter, decode_parms)?;
        if let Some(last_filter) = filter.last() {
            if matches!(*last_filter, "DCTDecode" | "DCT") {
                let (mut pixels, w, h, channels) = Self::decode_jpeg_with_info(&decompressed)?;
                let final_channels = if channels == 4 {
                    pixels = ColorSpaceConverter::cmyk_to_rgb(&pixels);
                    3
                } else {
                    channels
                };
                return Ok(RawImage {
                    width: w,
                    height: h,
                    channels: final_channels,
                    bits_per_sample: 8,
                    pixels,
                });
            }
        }

        let empty_dict = PdfDictionary::empty();
        Self::build_raw_image(
            decompressed,
            width,
            height,
            bpc,
            color_space,
            &empty_dict,
            None,
        )
    }

    /// Decode a JPEG image reference directly from its original stream bytes.
    pub fn decode_jpeg_image(image: &ImageReference, reader: &PdfReader) -> Result<RawImage> {
        let raw = ImageLocator::get_stream_bytes(image, reader)?.ok_or_else(|| {
            OxideError::UnsupportedFeature(
                "inline JPEG images not supported via this path".to_string(),
            )
        })?;
        let (mut pixels, width, height, channels) = Self::decode_jpeg_with_info(&raw)?;
        let final_channels = if channels == 4 {
            pixels = ColorSpaceConverter::cmyk_to_rgb(&pixels);
            3
        } else {
            channels
        };
        Ok(RawImage {
            width,
            height,
            channels: final_channels,
            bits_per_sample: 8,
            pixels,
        })
    }

    /// Decode JPEG bytes and return pixels plus width, height, channel count.
    pub fn decode_jpeg_with_info(jpeg_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32, u8)> {
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(jpeg_bytes));
        let pixels = decoder
            .decode()
            .map_err(|e| OxideError::MalformedPdf(format!("JPEG decode failed: {e}")))?;
        let info = decoder.info().ok_or_else(|| {
            OxideError::MalformedPdf("JPEG decode: no metadata after decode".to_string())
        })?;
        let channels = match info.pixel_format {
            jpeg_decoder::PixelFormat::L8 => 1,
            jpeg_decoder::PixelFormat::RGB24 => 3,
            jpeg_decoder::PixelFormat::CMYK32 => 4,
            other => {
                return Err(OxideError::UnsupportedFeature(format!(
                    "unsupported JPEG pixel format: {other:?}"
                )))
            }
        };
        Ok((
            pixels,
            u32::from(info.width),
            u32::from(info.height),
            channels,
        ))
    }

    pub(crate) fn normalise_bit_depth(
        raw: Vec<u8>,
        width: u32,
        height: u32,
        channels: u8,
        bpc: u8,
    ) -> Result<Vec<u8>> {
        match bpc {
            8 => Ok(raw),
            16 => Ok(raw.chunks(2).map(|chunk| chunk[0]).collect()),
            4 => {
                let total = expected_len(width, height, channels);
                let mut out = Vec::with_capacity(total);
                for byte in &raw {
                    out.push(((byte >> 4) & 0x0F) * 17);
                    out.push((byte & 0x0F) * 17);
                }
                out.truncate(total);
                Ok(out)
            }
            2 => {
                let total = expected_len(width, height, channels);
                let mut out = Vec::with_capacity(total);
                for byte in &raw {
                    for shift in [6u8, 4, 2, 0] {
                        out.push(((byte >> shift) & 0x03) * 85);
                    }
                }
                out.truncate(total);
                Ok(out)
            }
            1 => {
                let total = expected_len(width, height, channels);
                let mut out = Vec::with_capacity(total);
                for byte in &raw {
                    for shift in [7u8, 6, 5, 4, 3, 2, 1, 0] {
                        let v = (byte >> shift) & 0x01;
                        out.push(if v == 0 { 0 } else { 255 });
                    }
                }
                out.truncate(total);
                Ok(out)
            }
            other => Err(OxideError::UnsupportedFeature(format!(
                "unsupported bits_per_component: {other}"
            ))),
        }
    }

    #[cfg(test)]
    pub(crate) fn build_raw_image_pub(
        decompressed: Vec<u8>,
        width: u32,
        height: u32,
        bpc: u8,
        color_space: &str,
        dict: &PdfDictionary,
    ) -> Result<RawImage> {
        Self::build_raw_image(decompressed, width, height, bpc, color_space, dict, None)
    }

    fn decode_remaining_image_filter(
        data: &[u8],
        filter: &str,
        image: &ImageReference,
        reader: &PdfReader,
        dict: &PdfDictionary,
    ) -> Result<RawImage> {
        match filter {
            "DCTDecode" | "DCT" => {
                let (mut pixels, width, height, channels) = Self::decode_jpeg_with_info(data)?;
                if width != image.width || height != image.height {
                    log::warn!(
                        "JPEG image {}: dict dimensions {}x{} differ from JPEG header {}x{}; using JPEG header",
                        image.xobject_name,
                        image.width,
                        image.height,
                        width,
                        height
                    );
                }
                let final_channels = if channels == 4 {
                    pixels = ColorSpaceConverter::cmyk_to_rgb(&pixels);
                    3
                } else {
                    channels
                };
                Ok(RawImage {
                    width,
                    height,
                    channels: final_channels,
                    bits_per_sample: 8,
                    pixels,
                })
            }
            "JPXDecode" => {
                log::warn!("JPXDecode (JPEG2000) is not yet implemented");
                Err(OxideError::UnsupportedFeature(
                    "JPXDecode (JPEG2000) is not yet supported; planned for a future release"
                        .to_string(),
                ))
            }
            "CCITTFaxDecode" | "CCF" => {
                log::warn!("CCITTFaxDecode is not yet implemented");
                Err(OxideError::UnsupportedFeature(
                    "CCITTFaxDecode is not yet supported; planned for a future release".to_string(),
                ))
            }
            "JBIG2Decode" => Err(OxideError::UnsupportedFeature(
                "JBIG2Decode is not supported".to_string(),
            )),
            other => {
                let _ = reader;
                let _ = dict;
                Err(OxideError::UnsupportedFeature(format!(
                    "unknown image filter '{other}'"
                )))
            }
        }
    }

    fn apply_filters_direct(
        raw: &[u8],
        filters: &[&str],
        decode_parms: Option<&PdfDictionary>,
    ) -> Result<Vec<u8>> {
        let mut data = raw.to_vec();
        for &filter in filters {
            if matches!(
                filter,
                "DCTDecode" | "DCT" | "JPXDecode" | "CCITTFaxDecode" | "CCF" | "JBIG2Decode"
            ) {
                return Ok(data);
            }
            data = apply_filter_bytes(filter, &data, decode_parms)?;
        }
        Ok(data)
    }

    fn build_raw_image(
        decompressed: Vec<u8>,
        width: u32,
        height: u32,
        bpc: u8,
        color_space: &str,
        dict: &PdfDictionary,
        reader: Option<&PdfReader>,
    ) -> Result<RawImage> {
        let raw_channels = Self::raw_channel_count(color_space, dict, reader);
        if width == 0 || height == 0 {
            return Ok(RawImage {
                width,
                height,
                channels: raw_channels.max(1),
                bits_per_sample: 8,
                pixels: Vec::new(),
            });
        }
        if decompressed.is_empty() {
            return Err(OxideError::MalformedPdf(format!(
                "image {width}x{height} decoded to empty pixel data"
            )));
        }

        let normalised = Self::normalise_bit_depth(decompressed, width, height, raw_channels, bpc)?;
        let is_mask = match dict.get("ImageMask").or_else(|| dict.get("IM")) {
            Some(PdfObject::Boolean(value)) => *value,
            Some(PdfObject::Name(name)) => name.eq_ignore_ascii_case("true"),
            _ => false,
        };
        if is_mask {
            let mut pixels = normalised;
            let expected_size = expected_len(width, height, 1);
            resize_mismatch(&mut pixels, width, height, expected_size);
            return Ok(RawImage {
                width,
                height,
                channels: 1,
                bits_per_sample: 8,
                pixels,
            });
        }

        let (mut pixels, channels) = match color_space {
            "DeviceGray" | "G" | "CalGray" => (normalised, 1u8),
            "DeviceRGB" | "RGB" | "CalRGB" | "sRGB" => (normalised, 3u8),
            "DeviceCMYK" | "CMYK" => (ColorSpaceConverter::cmyk_to_rgb(&normalised), 3u8),
            "Lab" => (ColorSpaceConverter::lab_to_rgb(&normalised), 3u8),
            "Indexed" => {
                if let Some(reader) = reader {
                    ColorSpaceConverter::decode_indexed(&normalised, dict, reader, width, height)?
                } else {
                    log::warn!("Indexed color space without reader; using raw index bytes");
                    (normalised, 1u8)
                }
            }
            "ICCBased" => {
                if let Some(reader) = reader {
                    let n = ColorSpaceConverter::icc_channel_count(dict, reader).unwrap_or(3);
                    match n {
                        1 => (normalised, 1u8),
                        3 => (normalised, 3u8),
                        4 => (ColorSpaceConverter::cmyk_to_rgb(&normalised), 3u8),
                        _ => {
                            return Err(OxideError::UnsupportedFeature(format!(
                                "ICCBased with {n} components not supported"
                            )))
                        }
                    }
                } else {
                    (normalised, 3u8)
                }
            }
            other => {
                log::warn!("build_raw_image: unhandled color space '{other}', using raw data");
                let ch = if normalised.len() == (width * height) as usize {
                    1u8
                } else {
                    3u8
                };
                (normalised, ch)
            }
        };

        let expected_size = expected_len(width, height, channels);
        resize_mismatch(&mut pixels, width, height, expected_size);

        Ok(RawImage {
            width,
            height,
            channels,
            bits_per_sample: 8,
            pixels,
        })
    }

    fn raw_channel_count(
        color_space: &str,
        dict: &PdfDictionary,
        reader: Option<&PdfReader>,
    ) -> u8 {
        match color_space {
            "DeviceGray" | "G" | "CalGray" | "Indexed" => 1,
            "DeviceRGB" | "RGB" | "CalRGB" | "sRGB" | "Lab" => 3,
            "DeviceCMYK" | "CMYK" => 4,
            "ICCBased" => reader
                .and_then(|reader| ColorSpaceConverter::icc_channel_count(dict, reader))
                .unwrap_or(3),
            _ => 3,
        }
    }
}

pub struct ColorSpaceConverter;

impl ColorSpaceConverter {
    /// Convert source color space pixels to normalized output.
    pub fn convert(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        source_cs: &str,
        dict: &PdfDictionary,
        reader: &PdfReader,
    ) -> Result<(Vec<u8>, u8)> {
        match source_cs {
            "DeviceGray" | "G" | "CalGray" => Ok((pixels, 1)),
            "DeviceRGB" | "RGB" | "CalRGB" | "sRGB" => Ok((pixels, 3)),
            "DeviceCMYK" | "CMYK" => Ok((Self::cmyk_to_rgb(&pixels), 3)),
            "ICCBased" => {
                let n = Self::icc_channel_count(dict, reader).unwrap_or(3);
                match n {
                    1 => Ok((pixels, 1)),
                    3 => Ok((pixels, 3)),
                    4 => Ok((Self::cmyk_to_rgb(&pixels), 3)),
                    _ => Err(OxideError::UnsupportedFeature(format!(
                        "ICCBased with {n} components not supported"
                    ))),
                }
            }
            "Indexed" => Self::decode_indexed(&pixels, dict, reader, width, height),
            "Lab" => Ok((Self::lab_to_rgb(&pixels), 3)),
            other => {
                log::warn!(
                    "unknown color space '{}', treating as RGB/Gray by channel count",
                    other
                );
                let channels = if pixels.len() == (width * height) as usize {
                    1
                } else {
                    3
                };
                Ok((pixels, channels))
            }
        }
    }

    /// Convert interleaved CMYK pixels to RGB.
    pub fn cmyk_to_rgb(pixels: &[u8]) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(pixels.len() / 4 * 3);
        for chunk in pixels.chunks_exact(4) {
            let c = chunk[0] as f32 / 255.0;
            let m = chunk[1] as f32 / 255.0;
            let y = chunk[2] as f32 / 255.0;
            let k = chunk[3] as f32 / 255.0;
            let r = ((1.0 - c) * (1.0 - k) * 255.0).round().clamp(0.0, 255.0) as u8;
            let g = ((1.0 - m) * (1.0 - k) * 255.0).round().clamp(0.0, 255.0) as u8;
            let b = ((1.0 - y) * (1.0 - k) * 255.0).round().clamp(0.0, 255.0) as u8;
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
        rgb
    }

    fn decode_indexed(
        pixels: &[u8],
        dict: &PdfDictionary,
        reader: &PdfReader,
        _width: u32,
        _height: u32,
    ) -> Result<(Vec<u8>, u8)> {
        let cs_array = dict
            .get("ColorSpace")
            .and_then(PdfObject::as_array)
            .unwrap_or(&[]);

        if cs_array.len() < 4 {
            log::warn!("Indexed color space: missing required parameters, treating as gray");
            return Ok((pixels.to_vec(), 1));
        }

        let base_cs = cs_array
            .get(1)
            .and_then(PdfObject::as_name)
            .unwrap_or("DeviceRGB");
        let hival = cs_array
            .get(2)
            .and_then(PdfObject::as_integer)
            .unwrap_or(255)
            .max(0) as usize;

        let lookup = match cs_array.get(3) {
            Some(PdfObject::String(bytes)) => bytes.clone(),
            Some(PdfObject::Reference { number, generation }) => {
                match reader.get_object(*number, *generation) {
                    Ok(PdfObject::String(bytes)) => bytes,
                    Ok(PdfObject::Stream { raw, .. }) => raw,
                    _ => {
                        log::warn!("Indexed color space: lookup table reference failed");
                        return Ok((pixels.to_vec(), 1));
                    }
                }
            }
            _ => {
                log::warn!("Indexed color space: unrecognised lookup type");
                return Ok((pixels.to_vec(), 1));
            }
        };

        let base_channels = match base_cs {
            "DeviceGray" | "G" => 1usize,
            "DeviceRGB" | "RGB" => 3usize,
            "DeviceCMYK" | "CMYK" => 4usize,
            _ => 3usize,
        };
        let expected_lookup_len = (hival + 1) * base_channels;
        if lookup.len() < expected_lookup_len {
            log::warn!(
                "Indexed color space: lookup table too short ({} < {})",
                lookup.len(),
                expected_lookup_len
            );
        }

        let mut output = Vec::with_capacity(pixels.len() * base_channels);
        for &idx in pixels {
            let idx = (idx as usize).min(hival);
            let start = idx * base_channels;
            let end = (start + base_channels).min(lookup.len());
            if end <= start {
                output.extend(std::iter::repeat_n(0u8, base_channels));
            } else {
                output.extend_from_slice(&lookup[start..end]);
                output.extend(std::iter::repeat_n(0u8, start + base_channels - end));
            }
        }

        if base_channels == 4 {
            Ok((Self::cmyk_to_rgb(&output), 3))
        } else {
            Ok((output, base_channels as u8))
        }
    }

    fn icc_channel_count(dict: &PdfDictionary, reader: &PdfReader) -> Option<u8> {
        let arr = dict.get("ColorSpace")?.as_array()?;
        let reference = arr.get(1)?;
        let (obj_num, gen_num) = match reference {
            PdfObject::Reference { number, generation } => (*number, *generation),
            _ => return None,
        };
        match reader.get_object(obj_num, gen_num).ok()? {
            PdfObject::Stream { dict: icc_dict, .. } => {
                icc_dict.get_integer("N").map(|n| n.clamp(1, 4) as u8)
            }
            _ => None,
        }
    }

    fn lab_to_rgb(pixels: &[u8]) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks_exact(3) {
            let l = chunk[0] as f32 / 2.55;
            let a = chunk[1] as f32 - 128.0;
            let b = chunk[2] as f32 - 128.0;
            let fy = (l + 16.0) / 116.0;
            let fx = a / 500.0 + fy;
            let fz = fy - b / 200.0;
            let x = 0.96422 * Self::lab_f_inv(fx);
            let y = Self::lab_f_inv(fy);
            let z = 0.82521 * Self::lab_f_inv(fz);
            let r_lin = 3.133_856 * x - 1.6168667 * y - 0.4906146 * z;
            let g_lin = -0.9787684 * x + 1.9161415 * y + 0.0334540 * z;
            let b_lin = 0.0719453 * x - 0.2289914 * y + 1.4052427 * z;
            let r = (Self::srgb_gamma(r_lin) * 255.0).clamp(0.0, 255.0) as u8;
            let g = (Self::srgb_gamma(g_lin) * 255.0).clamp(0.0, 255.0) as u8;
            let b = (Self::srgb_gamma(b_lin) * 255.0).clamp(0.0, 255.0) as u8;
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
        rgb
    }

    fn lab_f_inv(t: f32) -> f32 {
        const DELTA: f32 = 6.0 / 29.0;
        if t > DELTA {
            t * t * t
        } else {
            3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
        }
    }

    fn srgb_gamma(linear: f32) -> f32 {
        if linear <= 0.0031308 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    }
}

fn expected_len(width: u32, height: u32, channels: u8) -> usize {
    width as usize * height as usize * channels as usize
}

fn resize_mismatch(pixels: &mut Vec<u8>, width: u32, height: u32, expected_size: usize) {
    if pixels.len() != expected_size {
        log::warn!(
            "image {}x{}: decoded {} bytes, expected {}; truncating/padding",
            width,
            height,
            pixels.len(),
            expected_size
        );
        pixels.resize(expected_size, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::encoder::ImageEncoder;

    #[test]
    fn normalise_bit_depth_8_bit_passthrough() {
        let pixels = vec![100u8, 150, 200];
        let out = ImageDecoder::normalise_bit_depth(pixels.clone(), 3, 1, 1, 8).unwrap();
        assert_eq!(out, pixels);
    }

    #[test]
    fn normalise_bit_depth_16_bit_to_8_bit() {
        let pixels = vec![0xAB_u8, 0xCD, 0x00, 0xFF];
        let out = ImageDecoder::normalise_bit_depth(pixels, 2, 1, 1, 16).unwrap();
        assert_eq!(out, vec![0xAB, 0x00]);
    }

    #[test]
    fn normalise_bit_depth_4_bit() {
        let pixels = vec![0xF0_u8];
        let out = ImageDecoder::normalise_bit_depth(pixels, 2, 1, 1, 4).unwrap();
        assert_eq!(out, vec![255, 0]);
    }

    #[test]
    fn normalise_bit_depth_2_bit() {
        let pixels = vec![0b11_10_01_00_u8];
        let out = ImageDecoder::normalise_bit_depth(pixels, 4, 1, 1, 2).unwrap();
        assert_eq!(out, vec![255, 170, 85, 0]);
    }

    #[test]
    fn normalise_bit_depth_1_bit() {
        let pixels = vec![0b1010_0000_u8];
        let out = ImageDecoder::normalise_bit_depth(pixels, 8, 1, 1, 1).unwrap();
        assert_eq!(out[0], 255);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 255);
    }

    #[test]
    fn normalise_bit_depth_unsupported_bpc_returns_error() {
        let result = ImageDecoder::normalise_bit_depth(vec![0], 1, 1, 1, 3);
        assert!(result.is_err());
    }

    #[test]
    fn cmyk_to_rgb_pure_black() {
        let cmyk = vec![0u8, 0, 0, 255];
        assert_eq!(ColorSpaceConverter::cmyk_to_rgb(&cmyk), vec![0, 0, 0]);
    }

    #[test]
    fn cmyk_to_rgb_no_ink_is_white() {
        let cmyk = vec![0u8, 0, 0, 0];
        assert_eq!(ColorSpaceConverter::cmyk_to_rgb(&cmyk), vec![255, 255, 255]);
    }

    #[test]
    fn cmyk_to_rgb_pure_cyan() {
        let cmyk = vec![255u8, 0, 0, 0];
        let rgb = ColorSpaceConverter::cmyk_to_rgb(&cmyk);
        assert_eq!(rgb, vec![0, 255, 255]);
    }

    #[test]
    fn cmyk_to_rgb_pure_magenta() {
        let cmyk = vec![0u8, 255, 0, 0];
        let rgb = ColorSpaceConverter::cmyk_to_rgb(&cmyk);
        assert_eq!(rgb, vec![255, 0, 255]);
    }

    #[test]
    fn cmyk_to_rgb_processes_multiple_pixels() {
        let cmyk = vec![0u8, 0, 0, 0, 0, 0, 0, 255];
        let rgb = ColorSpaceConverter::cmyk_to_rgb(&cmyk);
        assert_eq!(rgb, vec![255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn raw_image_is_valid_rejects_wrong_buffer_size() {
        let img = RawImage {
            width: 10,
            height: 10,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0u8; 100],
        };
        assert!(!img.is_valid());
    }

    #[test]
    fn raw_image_is_valid_accepts_correct_buffer() {
        let img = RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0u8; 12],
        };
        assert!(img.is_valid());
        assert_eq!(img.byte_count(), 12);
        assert_eq!(img.pixel_count(), 4);
        assert_eq!(img.row_stride(), 6);
        assert!(img.is_rgb());
    }

    #[test]
    fn raw_image_pixel_accessor() {
        let img = RawImage {
            width: 2,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![10, 20, 30, 40, 50, 60],
        };
        assert_eq!(img.pixel(0, 0), &[10, 20, 30]);
        assert_eq!(img.pixel(1, 0), &[40, 50, 60]);
        assert_eq!(img.pixel(3, 0), &[]);
    }

    #[test]
    fn build_raw_image_handles_device_gray() {
        let pixels = vec![0u8, 128, 255];
        let dict = PdfDictionary::empty();
        let img = ImageDecoder::build_raw_image_pub(pixels.clone(), 3, 1, 8, "DeviceGray", &dict)
            .unwrap();
        assert_eq!(img.channels, 1);
        assert_eq!(img.pixels, pixels);
        assert!(img.is_grayscale());
    }

    #[test]
    fn build_raw_image_handles_device_cmyk_to_rgb() {
        let pixels = vec![0u8, 0, 0, 0];
        let dict = PdfDictionary::empty();
        let img = ImageDecoder::build_raw_image_pub(pixels, 1, 1, 8, "DeviceCMYK", &dict).unwrap();
        assert_eq!(img.channels, 3);
        assert_eq!(img.pixels, vec![255, 255, 255]);
    }

    #[test]
    fn build_raw_image_pads_and_truncates_mismatched_buffers() {
        let dict = PdfDictionary::empty();
        let img =
            ImageDecoder::build_raw_image_pub(vec![255], 2, 1, 8, "DeviceRGB", &dict).unwrap();
        assert_eq!(img.pixels, vec![255, 0, 0, 0, 0, 0]);

        let img = ImageDecoder::build_raw_image_pub(vec![1, 2, 3, 4], 1, 1, 8, "DeviceGray", &dict)
            .unwrap();
        assert_eq!(img.pixels, vec![1]);
    }

    #[test]
    fn jpeg_decode_round_trip() {
        let mut pixels = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                pixels.push(x * 64);
                pixels.push(y * 64);
                pixels.push(128u8);
            }
        }
        let original = RawImage {
            width: 4,
            height: 4,
            channels: 3,
            bits_per_sample: 8,
            pixels,
        };
        let jpeg = ImageEncoder::encode_jpeg(&original, 95).unwrap();
        let (decoded_pixels, width, height, channels) =
            ImageDecoder::decode_jpeg_with_info(&jpeg).unwrap();
        assert_eq!(width, 4);
        assert_eq!(height, 4);
        assert_eq!(channels, 3);
        assert_eq!(decoded_pixels.len(), 4 * 4 * 3);
    }
}
