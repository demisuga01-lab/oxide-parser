//! JPXDecode (JPEG 2000) image decoding.
//!
//! # Approach
//!
//! JPEG 2000 is a wavelet-based codec that is substantially more complex than
//! DCT (JPEG) or even JBIG2. Rather than implement a from-scratch decoder, we
//! integrate the pure-Rust [`hayro-jpeg2000`] crate. This is the same family of
//! crates already used for CCITT (`hayro-ccitt`) and JBIG2 (`hayro-jbig2`), so
//! it satisfies the hard "no C/C++ toolchain" constraint: the crate is
//! `#![forbid(unsafe_code)]` and `no_std`-compatible, and we pull it in with
//! `default-features = false` plus `std` (no `simd`/`image` features) so there
//! is no dependency on a C compiler, cmake, or `links =` native library.
//!
//! `hayro-jpeg2000` decodes both raw JPEG 2000 codestreams (the common case for
//! PDF embedding, magic `FF 4F FF 51`) and full JP2 container files (magic
//! `00 00 00 0C 6A 50 20 20`). It selects the right path internally based on the
//! leading bytes, so the adapter does not need to detect or strip the JP2 box
//! wrapper itself.
//!
//! # Supported subset
//!
//! The underlying crate supports the vast majority of the JPEG 2000 core coding
//! system (ISO/IEC 15444-1): both the 5/3 reversible and 9/7 irreversible
//! wavelet filters, all progression orders, multiple tiles and resolution
//! levels, and palette-indexed images. It also handles several ISO/IEC 15444-2
//! color-space extensions. Color spaces surfaced here are grayscale, RGB, CMYK,
//! and ICC-based / unknown (handled by channel count). Anything the crate cannot
//! decode (e.g. progression-order changes inside tile-parts) surfaces as an
//! [`OxideError`] rather than a panic, matching the CCITT/JBIG2 error contract.
//!
//! [`hayro-jpeg2000`]: https://crates.io/crates/hayro-jpeg2000

use hayro_jpeg2000::{ColorSpace, DecodeSettings, Image};

use crate::error::{OxideError, Result};
use crate::images::decoder::{ColorSpaceConverter, RawImage};

/// Decode a PDF-embedded JPEG 2000 (`JPXDecode`) stream into a `RawImage`.
///
/// `data` is the raw stream bytes after all preceding (non-image) filters have
/// been applied. It may be either a raw J2K codestream (the common PDF case) or
/// a JP2-wrapped file; both are handled. The output is always 8-bit, with
/// channels interleaved, in either grayscale (1) or RGB (3) form — CMYK is
/// converted to RGB and any alpha channel is dropped (soft masks are handled
/// separately by the SMask pipeline), mirroring how the DCTDecode path behaves.
pub fn decode(data: &[u8]) -> Result<RawImage> {
    let image = Image::new(data, &DecodeSettings::default())
        .map_err(|err| OxideError::MalformedPdf(format!("JPXDecode parse failed: {err}")))?;

    let width = image.width();
    let height = image.height();
    let has_alpha = image.has_alpha();
    let color_space = image.color_space().clone();

    let decoded = image
        .decode()
        .map_err(|err| OxideError::MalformedPdf(format!("JPXDecode failed: {err}")))?;

    let color_channels = color_space.num_channels() as usize;
    let stored_channels = color_channels + usize::from(has_alpha);
    if stored_channels == 0 {
        return Err(OxideError::MalformedPdf(
            "JPXDecode produced an image with zero channels".to_string(),
        ));
    }

    // Drop the trailing alpha channel (if any) down to the pure color channels.
    let color_only = if has_alpha {
        strip_alpha(&decoded, stored_channels, color_channels)
    } else {
        decoded
    };

    let (pixels, channels) = match color_space {
        ColorSpace::Gray => (color_only, 1u8),
        ColorSpace::RGB => (color_only, 3u8),
        ColorSpace::CMYK => (ColorSpaceConverter::cmyk_to_rgb(&color_only), 3u8),
        // ICC-based and unknown color spaces are surfaced by their raw channel
        // count. 1 -> gray, 3 -> RGB, 4 -> treat as CMYK; anything else is left
        // as-is and reported by its channel count so the caller can still emit
        // pixels rather than failing outright.
        ColorSpace::Icc { num_channels, .. } | ColorSpace::Unknown { num_channels } => {
            match num_channels {
                1 => (color_only, 1u8),
                3 => (color_only, 3u8),
                4 => (ColorSpaceConverter::cmyk_to_rgb(&color_only), 3u8),
                other => {
                    log::warn!(
                        "JPXDecode {width}x{height}: {other}-channel ICC/unknown color space \
                         passed through unconverted"
                    );
                    (color_only, other)
                }
            }
        }
    };

    let mut raw = RawImage {
        width,
        height,
        channels,
        bits_per_sample: 8,
        pixels,
    };

    // Guard against any decode-length surprises so downstream painters/encoders
    // never index out of bounds (same contract as the other image decoders).
    let expected = raw.byte_count();
    if raw.pixels.len() != expected {
        log::warn!(
            "JPXDecode {width}x{height}: decoded {} bytes, expected {expected}; truncating/padding",
            raw.pixels.len()
        );
        raw.pixels.resize(expected, 0);
    }

    Ok(raw)
}

/// Drop the trailing alpha channel from interleaved pixel data, keeping the
/// first `color_channels` of every `stored_channels`-wide pixel.
fn strip_alpha(data: &[u8], stored_channels: usize, color_channels: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / stored_channels * color_channels);
    for pixel in data.chunks_exact(stored_channels) {
        out.extend_from_slice(&pixel[..color_channels]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_codestream_returns_error() {
        let result = decode(b"not a jpeg2000 codestream");
        assert!(matches!(result, Err(OxideError::MalformedPdf(_))));
    }

    #[test]
    fn strip_alpha_drops_last_channel() {
        // Two RGBA pixels -> two RGB pixels.
        let rgba = vec![10, 20, 30, 255, 40, 50, 60, 128];
        let rgb = strip_alpha(&rgba, 4, 3);
        assert_eq!(rgb, vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn strip_alpha_gray_alpha_to_gray() {
        let gray_alpha = vec![100, 255, 200, 0];
        let gray = strip_alpha(&gray_alpha, 2, 1);
        assert_eq!(gray, vec![100, 200]);
    }
}
