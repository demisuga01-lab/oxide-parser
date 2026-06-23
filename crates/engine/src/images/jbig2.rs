use crate::error::{OxideError, Result};
use crate::images::decoder::RawImage;

/// Decode an embedded PDF JBIG2 stream into Oxide's grayscale RawImage.
///
/// The underlying decoder supports PDF embedded organization, optional global
/// segments, generic regions, symbol dictionaries, text regions, halftone
/// regions, and generic refinement regions. Unsupported or malformed JBIG2
/// constructs are surfaced as decode errors rather than panics.
pub fn decode(data: &[u8], globals: Option<&[u8]>) -> Result<RawImage> {
    let image = hayro_jbig2::Image::new_embedded(data, globals)
        .map_err(|err| OxideError::MalformedPdf(format!("JBIG2Decode parse failed: {err}")))?;

    // H-6: bound the codestream-declared region dimensions before allocating the
    // grayscale sink, so a crafted multi-gigapixel JBIG2 page cannot force a huge
    // reservation in our sink.
    crate::images::decoder::ensure_decode_budget(image.width(), image.height(), 1)?;
    let mut sink = GrayscaleSink::new(image.width(), image.height());
    image
        .decode(&mut sink)
        .map_err(|err| OxideError::MalformedPdf(format!("JBIG2Decode failed: {err}")))?;
    Ok(sink.finish())
}

struct GrayscaleSink {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GrayscaleSink {
    fn new(width: u32, height: u32) -> Self {
        let expected = width as usize * height as usize;
        Self {
            width,
            height,
            pixels: Vec::with_capacity(expected),
        }
    }

    fn finish(mut self) -> RawImage {
        let expected = self.width as usize * self.height as usize;
        if self.pixels.len() != expected {
            log::warn!(
                "JBIG2Decode {}x{}: decoded {} pixels, expected {}; truncating/padding",
                self.width,
                self.height,
                self.pixels.len(),
                expected
            );
            self.pixels.resize(expected, 255);
        }
        RawImage {
            width: self.width,
            height: self.height,
            channels: 1,
            bits_per_sample: 8,
            pixels: self.pixels,
        }
    }

    fn push_gray(&mut self, black: bool, count: usize) {
        let value = if black { 0 } else { 255 };
        let expected = self.width as usize * self.height as usize;
        let remaining = expected.saturating_sub(self.pixels.len());
        self.pixels
            .extend(std::iter::repeat_n(value, count.min(remaining)));
    }
}

impl hayro_jbig2::Decoder for GrayscaleSink {
    fn push_pixel(&mut self, black: bool) {
        self.push_gray(black, 1);
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        self.push_gray(black, chunk_count as usize * 8);
    }

    fn next_line(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_embedded_stream_returns_error() {
        let result = decode(b"not a jbig2 stream", None);
        assert!(result.is_err());
    }
}
