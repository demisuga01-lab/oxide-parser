use crate::error::{OxideError, Result};
use crate::images::decoder::RawImage;

/// PDF /CCITTFaxDecode parameters relevant to bi-level image decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcittDecodeParams {
    pub k: i64,
    pub columns: u32,
    pub rows: u32,
    pub black_is_1: bool,
    pub encoded_byte_align: bool,
    pub end_of_line: bool,
    pub end_of_block: bool,
}

/// Decode a CCITT Group 3/Group 4 fax stream into Oxide's grayscale RawImage.
pub fn decode(data: &[u8], params: CcittDecodeParams) -> Result<RawImage> {
    if params.columns == 0 || params.rows == 0 {
        return Ok(RawImage {
            width: params.columns,
            height: params.rows,
            channels: 1,
            bits_per_sample: 8,
            pixels: Vec::new(),
        });
    }

    let encoding = if params.k < 0 {
        hayro_ccitt::EncodingMode::Group4
    } else if params.k == 0 {
        hayro_ccitt::EncodingMode::Group3_1D
    } else {
        hayro_ccitt::EncodingMode::Group3_2D {
            k: u32::try_from(params.k).map_err(|_| {
                OxideError::MalformedPdf("CCITTFaxDecode /K is too large".to_string())
            })?,
        }
    };

    let settings = hayro_ccitt::DecodeSettings {
        columns: params.columns,
        rows: params.rows,
        end_of_block: params.end_of_block,
        end_of_line: params.end_of_line,
        rows_are_byte_aligned: params.encoded_byte_align,
        encoding,
        invert_black: params.black_is_1,
    };
    let mut context = hayro_ccitt::DecoderContext::new(settings);
    let mut sink = GrayscaleSink::new(params.columns, params.rows);

    hayro_ccitt::decode(data, &mut sink, &mut context)
        .map_err(|err| OxideError::MalformedPdf(format!("CCITTFaxDecode failed: {err}")))?;

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
                "CCITTFaxDecode {}x{}: decoded {} pixels, expected {}; truncating/padding",
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

    fn push_gray(&mut self, white: bool, count: usize) {
        let value = if white { 255 } else { 0 };
        let expected = self.width as usize * self.height as usize;
        let remaining = expected.saturating_sub(self.pixels.len());
        self.pixels
            .extend(std::iter::repeat_n(value, count.min(remaining)));
    }
}

impl hayro_ccitt::Decoder for GrayscaleSink {
    fn push_pixel(&mut self, white: bool) {
        self.push_gray(white, 1);
    }

    fn push_pixel_chunk(&mut self, white: bool, chunk_count: u32) {
        self.push_gray(white, chunk_count as usize * 8);
    }

    fn next_line(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_bits(bits: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = 0u8;
        let mut bit_count = 0u8;

        for bit in bits.bytes().filter(|b| *b == b'0' || *b == b'1') {
            byte <<= 1;
            if bit == b'1' {
                byte |= 1;
            }
            bit_count += 1;
            if bit_count == 8 {
                out.push(byte);
                byte = 0;
                bit_count = 0;
            }
        }

        if bit_count > 0 {
            byte <<= 8 - bit_count;
            out.push(byte);
        }

        out
    }

    fn params(k: i64, columns: u32, rows: u32) -> CcittDecodeParams {
        CcittDecodeParams {
            k,
            columns,
            rows,
            black_is_1: false,
            encoded_byte_align: false,
            end_of_line: false,
            end_of_block: false,
        }
    }

    #[test]
    fn decodes_group3_1d_all_white_line() {
        let image = decode(&pack_bits("10011"), params(0, 8, 1)).unwrap();
        assert_eq!(image.width, 8);
        assert_eq!(image.height, 1);
        assert_eq!(image.pixels, vec![255; 8]);
    }

    #[test]
    fn decodes_group3_1d_known_run_lengths() {
        // White run 2 (0111), black run 3 (10), white run 3 (1000).
        let image = decode(&pack_bits("0111 10 1000"), params(0, 8, 1)).unwrap();
        assert_eq!(image.pixels, vec![255, 255, 0, 0, 0, 255, 255, 255]);
    }

    #[test]
    fn black_is_1_inverts_output_pixels() {
        let mut options = params(0, 8, 1);
        options.black_is_1 = true;

        let image = decode(&pack_bits("0111 10 1000"), options).unwrap();
        assert_eq!(image.pixels, vec![0, 0, 255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn decodes_group3_2d_mixed_lines() {
        // First line: tag 1 + 1D white run 8. Second line: tag 0 + 2D V0.
        let image = decode(&pack_bits("1 10011 0 1"), params(2, 8, 2)).unwrap();
        assert_eq!(image.pixels, vec![255; 16]);
    }

    #[test]
    fn decodes_group4_vertical_mode_lines() {
        // With an all-white reference line, V0 (1) emits an all-white line.
        let image = decode(&pack_bits("1 1"), params(-1, 8, 2)).unwrap();
        assert_eq!(image.pixels, vec![255; 16]);
    }

    #[test]
    fn decodes_group3_1d_byte_aligned_rows() {
        let mut options = params(0, 8, 2);
        options.encoded_byte_align = true;

        // Row 1: white run 8 (10011), padded to a byte boundary.
        // Row 2 starts at the next byte: white run 8 (10011).
        let image = decode(&pack_bits("10011 000 10011"), options).unwrap();
        assert_eq!(image.pixels, vec![255; 16]);
    }
}
