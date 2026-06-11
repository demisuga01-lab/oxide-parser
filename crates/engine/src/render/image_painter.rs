use crate::images::decoder::RawImage;
use crate::render::buffer::PixelBuffer;
use crate::render::transform::{Transform2D, Viewport};

pub struct ImagePainter;

impl ImagePainter {
    /// Paint a decoded image onto the buffer.
    pub fn paint_image(
        buf: &mut PixelBuffer,
        image: &RawImage,
        ctm: &Transform2D,
        viewport: &Viewport,
    ) {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return;
        }
        if ctm.determinant().abs() < 1e-10 {
            log::warn!("ImagePainter: singular transform, skipping image");
            return;
        }

        let vp_transform = viewport.to_transform();
        let combined = ctm.concat(&vp_transform);

        if ctm.is_axis_aligned() {
            Self::paint_bilinear(buf, image, &combined);
        } else {
            Self::paint_affine(buf, image, &combined);
        }
    }

    fn paint_bilinear(buf: &mut PixelBuffer, image: &RawImage, combined: &Transform2D) {
        let corners = [
            combined.transform_point(0.0, 0.0),
            combined.transform_point(1.0, 0.0),
            combined.transform_point(0.0, 1.0),
            combined.transform_point(1.0, 1.0),
        ];
        let (px_min, px_max, py_min, py_max) = bounding_box(&corners);
        if !px_min.is_finite() || !px_max.is_finite() || !py_min.is_finite() || !py_max.is_finite()
        {
            return;
        }

        let dst_w = (px_max - px_min).max(1.0);
        let dst_h = (py_max - py_min).max(1.0);
        let (x0, x1, y0, y1) = clipped_bounds(buf, px_min, px_max, py_min, py_max);
        if x0 > x1 || y0 > y1 {
            return;
        }

        for py in y0..=y1 {
            for px in x0..=x1 {
                let u = (px as f64 + 0.5 - px_min) / dst_w;
                let v = (py as f64 + 0.5 - py_min) / dst_h;
                let sample = Self::bilinear_sample(image, u, v);
                let coverage = if image.channels == 4 {
                    sample[3] as f32 / 255.0
                } else {
                    1.0
                };
                buf.blend_pixel(px, py, [sample[0], sample[1], sample[2], 255], coverage);
            }
        }
    }

    fn paint_affine(buf: &mut PixelBuffer, image: &RawImage, combined: &Transform2D) {
        let inv = match combined.inverse() {
            Some(matrix) => matrix,
            None => {
                log::warn!("ImagePainter: singular transform, skipping image");
                return;
            }
        };

        let corners = [
            combined.transform_point(0.0, 0.0),
            combined.transform_point(1.0, 0.0),
            combined.transform_point(1.0, 1.0),
            combined.transform_point(0.0, 1.0),
        ];
        let (px_min, px_max, py_min, py_max) = bounding_box(&corners);
        if !px_min.is_finite() || !px_max.is_finite() || !py_min.is_finite() || !py_max.is_finite()
        {
            return;
        }

        let (x0, x1, y0, y1) = clipped_bounds(buf, px_min, px_max, py_min, py_max);
        if x0 > x1 || y0 > y1 {
            return;
        }

        for py in y0..=y1 {
            for px in x0..=x1 {
                let (u, v) = inv.transform_point(px as f64 + 0.5, py as f64 + 0.5);
                if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                    continue;
                }

                let sample = Self::bilinear_sample(image, u, v);
                let coverage = if image.channels == 4 {
                    sample[3] as f32 / 255.0
                } else {
                    1.0
                };
                buf.blend_pixel(px, py, [sample[0], sample[1], sample[2], 255], coverage);
            }
        }
    }

    /// Sample image at normalized coordinates using bilinear interpolation.
    pub fn bilinear_sample(image: &RawImage, u: f64, v: f64) -> [u8; 4] {
        if image.width == 0 || image.height == 0 || image.channels == 0 || image.pixels.is_empty() {
            return [0, 0, 0, 0];
        }

        let w = image.width as f64;
        let h = image.height as f64;
        let sx = (u * (w - 1.0)).clamp(0.0, (w - 1.0).max(0.0));
        let sy = (v * (h - 1.0)).clamp(0.0, (h - 1.0).max(0.0));

        let x0 = sx.floor() as usize;
        let y0 = sy.floor() as usize;
        let x1 = (x0 + 1).min(image.width.saturating_sub(1) as usize);
        let y1 = (y0 + 1).min(image.height.saturating_sub(1) as usize);
        let fx = (sx - x0 as f64) as f32;
        let fy = (sy - y0 as f64) as f32;

        let p00 = Self::get_pixel_channels(image, x0, y0);
        let p10 = Self::get_pixel_channels(image, x1, y0);
        let p01 = Self::get_pixel_channels(image, x0, y1);
        let p11 = Self::get_pixel_channels(image, x1, y1);

        let lerp2 = |v00: u8, v10: u8, v01: u8, v11: u8| -> u8 {
            let top = v00 as f32 * (1.0 - fx) + v10 as f32 * fx;
            let bottom = v01 as f32 * (1.0 - fx) + v11 as f32 * fx;
            (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
        };

        [
            lerp2(p00[0], p10[0], p01[0], p11[0]),
            lerp2(p00[1], p10[1], p01[1], p11[1]),
            lerp2(p00[2], p10[2], p01[2], p11[2]),
            lerp2(p00[3], p10[3], p01[3], p11[3]),
        ]
    }

    fn get_pixel_channels(image: &RawImage, x: usize, y: usize) -> [u8; 4] {
        let channels = image.channels as usize;
        let stride = match (image.width as usize).checked_mul(channels) {
            Some(stride) => stride,
            None => return [0, 0, 0, 255],
        };
        let base = match y
            .checked_mul(stride)
            .and_then(|row| row.checked_add(x * channels))
        {
            Some(base) => base,
            None => return [0, 0, 0, 255],
        };

        match image.channels {
            1 => {
                let g = image.pixels.get(base).copied().unwrap_or(0);
                [g, g, g, 255]
            }
            3 => [
                image.pixels.get(base).copied().unwrap_or(0),
                image.pixels.get(base + 1).copied().unwrap_or(0),
                image.pixels.get(base + 2).copied().unwrap_or(0),
                255,
            ],
            4 => [
                image.pixels.get(base).copied().unwrap_or(0),
                image.pixels.get(base + 1).copied().unwrap_or(0),
                image.pixels.get(base + 2).copied().unwrap_or(0),
                image.pixels.get(base + 3).copied().unwrap_or(255),
            ],
            _ => [0, 0, 0, 255],
        }
    }
}

fn bounding_box(corners: &[(f64, f64); 4]) -> (f64, f64, f64, f64) {
    let px_min = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    let px_max = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let py_min = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::INFINITY, f64::min);
    let py_max = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max);
    (px_min, px_max, py_min, py_max)
}

fn clipped_bounds(
    buf: &PixelBuffer,
    px_min: f64,
    px_max: f64,
    py_min: f64,
    py_max: f64,
) -> (i32, i32, i32, i32) {
    if buf.width == 0 || buf.height == 0 {
        return (1, 0, 1, 0);
    }
    let x0 = floor_i32(px_min).max(0);
    let x1 = ceil_i32(px_max).min(buf.width as i32 - 1);
    let y0 = floor_i32(py_min).max(0);
    let y1 = ceil_i32(py_max).min(buf.height as i32 - 1);
    (x0, x1, y0, y1)
}

fn floor_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value.floor() as i32
    }
}

fn ceil_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value.ceil() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::{BLACK, WHITE};

    fn rgb_2x2_image() -> RawImage {
        RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        }
    }

    #[test]
    fn bilinear_sample_on_2x2_image_corners() {
        let image = rgb_2x2_image();
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 0.0, 0.0)[..3],
            &[255, 0, 0]
        );
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 1.0, 0.0)[..3],
            &[0, 255, 0]
        );
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 0.0, 1.0)[..3],
            &[0, 0, 255]
        );
        assert_eq!(
            &ImagePainter::bilinear_sample(&image, 1.0, 1.0)[..3],
            &[255, 255, 0]
        );
    }

    #[test]
    fn bilinear_sample_at_center_blends_all_corners() {
        let image = RawImage {
            width: 2,
            height: 2,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![200, 0, 0, 0, 200, 0, 0, 0, 200, 200, 200, 0],
        };
        let center = ImagePainter::bilinear_sample(&image, 0.5, 0.5);
        assert!((center[0] as i32 - 100).abs() <= 3);
    }

    #[test]
    fn bilinear_sample_gray_image_replicates_channels() {
        let image = RawImage {
            width: 2,
            height: 1,
            channels: 1,
            bits_per_sample: 8,
            pixels: vec![0, 255],
        };
        let left = ImagePainter::bilinear_sample(&image, 0.0, 0.5);
        let right = ImagePainter::bilinear_sample(&image, 1.0, 0.5);
        assert_eq!(&left[..3], &[0, 0, 0]);
        assert_eq!(&right[..3], &[255, 255, 255]);
        assert_eq!(left[3], 255);
        assert_eq!(right[3], 255);
    }

    #[test]
    fn bilinear_sample_rgba_image_preserves_alpha() {
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 128],
        };
        let sample = ImagePainter::bilinear_sample(&image, 0.5, 0.5);
        assert_eq!(sample, [255, 0, 0, 128]);
    }

    #[test]
    fn paint_image_places_pixels_in_correct_region() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0],
        };
        let ctm = Transform2D::new(50.0, 0.0, 0.0, 50.0, 25.0, 25.0);
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        let center = buf.get_pixel(50, 50);
        println!("paint_image center pixel: {:?}", center);
        assert!(center[0] > 200);
        assert_eq!(buf.get_pixel(1, 1), WHITE);
    }

    #[test]
    fn paint_image_with_zero_size_image_does_not_panic() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let empty = RawImage {
            width: 0,
            height: 0,
            channels: 3,
            bits_per_sample: 8,
            pixels: Vec::new(),
        };
        ImagePainter::paint_image(&mut buf, &empty, &ctm, &vp);
    }

    #[test]
    fn paint_image_rgba_uses_alpha_channel() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf_opaque = PixelBuffer::new_filled(100, 100, WHITE);
        let mut buf_transp = PixelBuffer::new_filled(100, 100, WHITE);
        let ctm = Transform2D::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0);
        let opaque = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 255],
        };
        let transparent = RawImage {
            width: 1,
            height: 1,
            channels: 4,
            bits_per_sample: 8,
            pixels: vec![255, 0, 0, 0],
        };
        ImagePainter::paint_image(&mut buf_opaque, &opaque, &ctm, &vp);
        ImagePainter::paint_image(&mut buf_transp, &transparent, &ctm, &vp);
        assert!(buf_opaque.get_pixel(50, 50)[0] > 200);
        assert_eq!(buf_transp.get_pixel(50, 50), WHITE);
    }

    #[test]
    fn bilinear_sample_empty_image_is_transparent() {
        let image = RawImage {
            width: 0,
            height: 0,
            channels: 0,
            bits_per_sample: 8,
            pixels: Vec::new(),
        };
        assert_eq!(
            ImagePainter::bilinear_sample(&image, 0.5, 0.5),
            [0, 0, 0, 0]
        );
    }

    #[test]
    fn paint_image_singular_transform_skips_gracefully() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: vec![0, 0, 0],
        };
        let ctm = Transform2D::new(0.0, 0.0, 0.0, 0.0, 10.0, 10.0);
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        assert_eq!(buf.get_pixel(10, 10), WHITE);
        assert_eq!(buf.get_pixel(0, 0), WHITE);
    }

    #[test]
    fn paint_rotated_image_affine_path_draws_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let image = RawImage {
            width: 1,
            height: 1,
            channels: 3,
            bits_per_sample: 8,
            pixels: BLACK[..3].to_vec(),
        };
        let ctm = Transform2D::scale(30.0, 30.0)
            .concat(&Transform2D::rotation(std::f64::consts::FRAC_PI_4))
            .concat(&Transform2D::translation(50.0, 50.0));
        ImagePainter::paint_image(&mut buf, &image, &ctm, &vp);
        let dark = (0..100i32)
            .flat_map(|y| (0..100i32).map(move |x| (x, y)))
            .any(|(x, y)| buf.get_pixel(x, y)[0] < 200);
        assert!(dark);
    }
}
