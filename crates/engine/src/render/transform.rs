use crate::content::state::Matrix;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform2D {
    /// The identity transform: [1 0 0 1 0 0].
    pub fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Construct from the 6 PDF matrix components [a b c d e f].
    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Construct from a flat array [a, b, c, d, e, f].
    pub fn from_array(m: [f64; 6]) -> Self {
        Self {
            a: m[0],
            b: m[1],
            c: m[2],
            d: m[3],
            e: m[4],
            f: m[5],
        }
    }

    /// Export as a flat array [a, b, c, d, e, f].
    pub fn to_array(&self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    /// Translation matrix: [1 0 0 1 tx ty].
    pub fn translation(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// Uniform scale matrix: [s 0 0 s 0 0].
    pub fn uniform_scale(s: f64) -> Self {
        Self {
            a: s,
            b: 0.0,
            c: 0.0,
            d: s,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Non-uniform scale matrix: [sx 0 0 sy 0 0].
    pub fn scale(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Rotation matrix for angle theta (radians, counter-clockwise).
    pub fn rotation(angle_radians: f64) -> Self {
        let cos = angle_radians.cos();
        let sin = angle_radians.sin();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Shear matrix.
    pub fn shear(shear_x: f64, shear_y: f64) -> Self {
        Self {
            a: 1.0,
            b: shear_y,
            c: shear_x,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Concatenate: self applied first, other applied second.
    pub fn concat(&self, other: &Transform2D) -> Transform2D {
        Transform2D {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Apply this transform to the point (x, y).
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Apply this transform to a vector (dx, dy). Translation is ignored.
    pub fn transform_vector(&self, dx: f64, dy: f64) -> (f64, f64) {
        (self.a * dx + self.c * dy, self.b * dx + self.d * dy)
    }

    /// Determinant: a*d - b*c.
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// Compute the inverse transform. Returns None for singular matrices.
    pub fn inverse(&self) -> Option<Transform2D> {
        let det = self.determinant();
        if det.abs() < 1e-10 {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Transform2D {
            a: self.d * inv_det,
            b: -self.b * inv_det,
            c: -self.c * inv_det,
            d: self.a * inv_det,
            e: (self.c * self.f - self.d * self.e) * inv_det,
            f: (self.b * self.e - self.a * self.f) * inv_det,
        })
    }

    /// The scaling factor in the X direction.
    pub fn scale_x(&self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    /// The scaling factor in the Y direction.
    pub fn scale_y(&self) -> f64 {
        (self.c * self.c + self.d * self.d).sqrt()
    }

    /// Uniform scale factor as the geometric mean.
    pub fn scale_factor(&self) -> f64 {
        (self.scale_x() * self.scale_y()).sqrt()
    }

    /// True if this transform has no rotation or shear.
    pub fn is_axis_aligned(&self) -> bool {
        self.b.abs() < 1e-10 && self.c.abs() < 1e-10
    }

    /// True if this transform is approximately identity.
    pub fn is_identity(&self) -> bool {
        (self.a - 1.0).abs() < 1e-10
            && self.b.abs() < 1e-10
            && self.c.abs() < 1e-10
            && (self.d - 1.0).abs() < 1e-10
            && self.e.abs() < 1e-10
            && self.f.abs() < 1e-10
    }
}

impl From<Matrix> for Transform2D {
    fn from(m: Matrix) -> Self {
        Self::from_array(m)
    }
}

impl From<Transform2D> for Matrix {
    fn from(t: Transform2D) -> Matrix {
        [t.a, t.b, t.c, t.d, t.e, t.f]
    }
}

impl From<&Transform2D> for Matrix {
    fn from(t: &Transform2D) -> Matrix {
        [t.a, t.b, t.c, t.d, t.e, t.f]
    }
}

#[derive(Debug, Clone)]
pub struct Viewport {
    /// MediaBox: [x_min, y_min, x_max, y_max] in PDF points.
    pub media_box: [f64; 4],
    /// Rendering resolution in dots per inch.
    pub dpi: u32,
    /// Pixels per point.
    pub scale: f64,
    /// Rendered image width in pixels.
    pub width_px: u32,
    /// Rendered image height in pixels.
    pub height_px: u32,
    /// Page display rotation in clockwise degrees.
    pub rotation: u32,
}

impl Viewport {
    /// Create a Viewport for a MediaBox at a given DPI.
    pub fn new(media_box: [f64; 4], dpi: u32) -> Self {
        let dpi = dpi.max(1);
        let scale = dpi as f64 / 72.0;
        let page_w = (media_box[2] - media_box[0]).abs();
        let page_h = (media_box[3] - media_box[1]).abs();
        let width_px = safe_ceil_to_u32(page_w * scale);
        let height_px = safe_ceil_to_u32(page_h * scale);
        Self {
            media_box,
            dpi,
            scale,
            width_px,
            height_px,
            rotation: 0,
        }
    }

    /// Create a Viewport that accounts for page display rotation.
    pub fn new_rotated(media_box: [f64; 4], dpi: u32, rotation: u32) -> Self {
        let base = Self::new(media_box, dpi);
        match rotation % 360 {
            90 | 270 => Self {
                media_box: base.media_box,
                dpi: base.dpi,
                scale: base.scale,
                width_px: base.height_px,
                height_px: base.width_px,
                rotation,
            },
            0 | 180 => Self { rotation, ..base },
            _ => Self { rotation, ..base },
        }
    }

    /// Width of the page in PDF points.
    pub fn page_width_pts(&self) -> f64 {
        (self.media_box[2] - self.media_box[0]).abs()
    }

    /// Height of the page in PDF points.
    pub fn page_height_pts(&self) -> f64 {
        (self.media_box[3] - self.media_box[1]).abs()
    }

    /// Convert a PDF user-space point to integer pixel coordinates.
    pub fn page_to_pixel(&self, x: f64, y: f64) -> (i32, i32) {
        let (px, py) = self.page_to_pixel_f64(x, y);
        (safe_trunc_to_i32(px), safe_trunc_to_i32(py))
    }

    /// Convert a PDF user-space point to sub-pixel pixel coordinates.
    pub fn page_to_pixel_f64(&self, x: f64, y: f64) -> (f64, f64) {
        self.to_transform().transform_point(x, y)
    }

    /// Convert pixel coordinates back to PDF user-space.
    pub fn pixel_to_page(&self, px: i32, py: i32) -> (f64, f64) {
        self.to_transform()
            .inverse()
            .map(|inverse| inverse.transform_point(px as f64, py as f64))
            .unwrap_or((0.0, 0.0))
    }

    /// The full page-to-pixel transform.
    pub fn to_transform(&self) -> Transform2D {
        let x1 = self.media_box[0];
        let y1 = self.media_box[1];
        let x2 = self.media_box[2];
        let y2 = self.media_box[3];
        let s = self.scale;

        match self.rotation % 360 {
            0 => Transform2D {
                a: s,
                b: 0.0,
                c: 0.0,
                d: -s,
                e: -x1 * s,
                f: y2 * s,
            },
            // /Rotate is a CLOCKWISE display rotation. The device transform is the
            // page→device map (with the PDF y-up → device y-down flip) composed
            // with that rotation. The flip is a reflection (det < 0), so a correct
            // rotated transform must KEEP det < 0 — the previous 90/270 matrices
            // had det > 0, i.e. they rotated WITHOUT the flip and rendered content
            // mirror-imaged. Derivation: base device (X,Y) = (s(x-x1), s(y2-y)),
            // image WxH = (s(x2-x1), s(y2-y1)); a clockwise 90° maps (X,Y)->(H-Y,X),
            // 270° maps (X,Y)->(Y,W-X). Substituting gives:
            90 => Transform2D {
                // px = s(y - y1), py = s(x - x1)
                a: 0.0,
                b: s,
                c: s,
                d: 0.0,
                e: -y1 * s,
                f: -x1 * s,
            },
            180 => Transform2D {
                a: -s,
                b: 0.0,
                c: 0.0,
                d: s,
                e: x2 * s,
                f: -y1 * s,
            },
            270 => Transform2D {
                // px = s(y2 - y), py = s(x2 - x)
                a: 0.0,
                b: -s,
                c: -s,
                d: 0.0,
                e: y2 * s,
                f: x2 * s,
            },
            _ => Transform2D {
                a: s,
                b: 0.0,
                c: 0.0,
                d: -s,
                e: -x1 * s,
                f: y2 * s,
            },
        }
    }
}

fn safe_ceil_to_u32(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= u32::MAX as f64 {
        u32::MAX
    } else {
        value.ceil() as u32
    }
}

fn safe_trunc_to_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_transform() {
        let id = Transform2D::identity();
        assert!(id.is_identity());
        assert_eq!(id.transform_point(3.0, 7.0), (3.0, 7.0));
    }

    #[test]
    fn translation() {
        let t = Transform2D::translation(10.0, 20.0);
        assert_eq!(t.transform_point(0.0, 0.0), (10.0, 20.0));
        assert_eq!(t.transform_point(5.0, -3.0), (15.0, 17.0));
    }

    #[test]
    fn translation_vector_ignores_translation_component() {
        let t = Transform2D::translation(10.0, 20.0);
        assert_eq!(t.transform_vector(3.0, 4.0), (3.0, 4.0));
    }

    #[test]
    fn scale() {
        let s = Transform2D::scale(2.0, 3.0);
        assert_eq!(s.transform_point(5.0, 7.0), (10.0, 21.0));
    }

    #[test]
    fn rotation_by_90_degrees() {
        let r = Transform2D::rotation(std::f64::consts::FRAC_PI_2);
        let (x, y) = r.transform_point(1.0, 0.0);
        println!("rotation 90 result: ({x}, {y})");
        assert!((x - 0.0).abs() < 1e-10);
        assert!((y - 1.0).abs() < 1e-10);
    }

    #[test]
    fn concat_translate_then_scale() {
        let t = Transform2D::translation(10.0, 0.0);
        let s = Transform2D::scale(2.0, 2.0);
        let ts = t.concat(&s);
        assert_eq!(ts.transform_point(5.0, 3.0), (30.0, 6.0));
    }

    #[test]
    fn inverse_of_identity_is_identity() {
        let inv = Transform2D::identity().inverse().unwrap();
        assert!(inv.is_identity());
    }

    #[test]
    fn inverse_round_trip() {
        let t = Transform2D::translation(3.0, -7.0);
        let inv = t.inverse().unwrap();
        assert!(t.concat(&inv).is_identity());
    }

    #[test]
    fn rotation_inverse() {
        let r = Transform2D::rotation(1.234);
        let inv = r.inverse().unwrap();
        let composed = r.concat(&inv);
        assert!((composed.a - 1.0).abs() < 1e-9);
        assert!((composed.d - 1.0).abs() < 1e-9);
        assert!(composed.b.abs() < 1e-9);
        assert!(composed.c.abs() < 1e-9);
    }

    #[test]
    fn inverse_of_singular_matrix_returns_none() {
        let singular = Transform2D::new(1.0, 0.0, 1.0, 0.0, 0.0, 0.0);
        assert!(singular.inverse().is_none());
    }

    #[test]
    fn scale_x_and_scale_y() {
        let s = Transform2D::scale(3.0, 5.0);
        assert!((s.scale_x() - 3.0).abs() < 1e-10);
        assert!((s.scale_y() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn matrix_conversion_round_trip() {
        let arr: Matrix = [2.0, 0.0, 0.0, 3.0, 10.0, -5.0];
        let t = Transform2D::from(arr);
        assert_eq!(t.a, 2.0);
        assert_eq!(t.d, 3.0);
        assert_eq!(t.e, 10.0);
        assert_eq!(t.f, -5.0);
        let back: Matrix = t.into();
        assert_eq!(back, arr);
    }

    #[test]
    fn a4_at_150_dpi() {
        let vp = Viewport::new([0.0, 0.0, 595.0, 842.0], 150);
        println!(
            "a4 150 dpi: scale={} width={} height={}",
            vp.scale, vp.width_px, vp.height_px
        );
        assert_eq!(vp.dpi, 150);
        assert_eq!(vp.width_px, 1240);
        assert_eq!(vp.height_px, 1755);
    }

    #[test]
    fn bottom_left_maps_to_pixel_bottom() {
        let vp = Viewport::new([0.0, 0.0, 595.0, 842.0], 150);
        let (px, py) = vp.page_to_pixel(0.0, 0.0);
        println!("a4 bottom-left pixel: ({px}, {py})");
        assert_eq!(px, 0);
        assert!(py as u32 >= vp.height_px - 1);
        let (px, py) = vp.page_to_pixel(0.0, 842.0);
        assert_eq!(px, 0);
        assert_eq!(py, 0);
    }

    #[test]
    fn page_center_maps_to_pixel_center() {
        let vp = Viewport::new([0.0, 0.0, 595.0, 842.0], 150);
        let (px, py) = vp.page_to_pixel(297.5, 421.0);
        assert_eq!(px, (297.5 * vp.scale) as i32);
        assert_eq!(py, ((842.0 - 421.0) * vp.scale) as i32);
    }

    #[test]
    fn pixel_to_page_is_approximately_inverse() {
        let vp = Viewport::new([0.0, 0.0, 595.0, 842.0], 150);
        let orig_x = 150.0;
        let orig_y = 600.0;
        let (px, py) = vp.page_to_pixel(orig_x, orig_y);
        let (rx, ry) = vp.pixel_to_page(px, py);
        assert!((rx - orig_x).abs() < 1.0);
        assert!((ry - orig_y).abs() < 1.0);
    }

    #[test]
    fn to_transform_produces_consistent_results() {
        let vp = Viewport::new([0.0, 0.0, 200.0, 200.0], 72);
        let t = vp.to_transform();
        let (px, py) = t.transform_point(100.0, 100.0);
        assert!((px - 100.0).abs() < 0.001);
        assert!((py - 100.0).abs() < 0.001);
    }

    #[test]
    fn uniform_scale() {
        let s = Transform2D::uniform_scale(4.0);
        assert_eq!(s.transform_point(2.0, 3.0), (8.0, 12.0));
        assert_eq!(s.scale_x(), 4.0);
        assert_eq!(s.scale_y(), 4.0);
    }

    #[test]
    fn concat_identity_leaves_transform_unchanged() {
        let t = Transform2D::translation(5.0, 7.0);
        let t_id = t.concat(&Transform2D::identity());
        assert!((t_id.a - t.a).abs() < 1e-12);
        assert!((t_id.e - t.e).abs() < 1e-12);
        assert!((t_id.f - t.f).abs() < 1e-12);
    }

    #[test]
    fn scale_then_translate() {
        let scale_half = Transform2D::scale(0.5, 0.5);
        let translate = Transform2D::translation(100.0, 200.0);
        let combined = scale_half.concat(&translate);
        let (x, y) = combined.transform_point(10.0, 20.0);
        assert!((x - 105.0).abs() < 1e-10);
        assert!((y - 210.0).abs() < 1e-10);
    }

    #[test]
    fn is_axis_aligned_detects_rotation() {
        assert!(Transform2D::scale(2.0, 3.0).is_axis_aligned());
        assert!(!Transform2D::rotation(0.1).is_axis_aligned());
        assert!(Transform2D::translation(10.0, 5.0).is_axis_aligned());
    }

    #[test]
    fn shear_transform() {
        let sh = Transform2D::shear(0.5, 0.0);
        let (x, y) = sh.transform_point(0.0, 2.0);
        assert!((x - 1.0).abs() < 1e-10);
        assert!((y - 2.0).abs() < 1e-10);
    }

    #[test]
    fn scale_factor_for_rotation_is_one() {
        let r = Transform2D::rotation(0.7);
        assert!((r.scale_factor() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn from_array_and_to_array_are_inverse() {
        let arr = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0_f64];
        assert_eq!(Transform2D::from_array(arr).to_array(), arr);
    }

    #[test]
    fn dpi_72_one_point_is_one_pixel() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        assert!((vp.scale - 1.0).abs() < 1e-10);
        assert_eq!(vp.width_px, 100);
        assert_eq!(vp.height_px, 100);
        assert_eq!(vp.page_to_pixel(50.0, 50.0), (50, 50));
    }

    #[test]
    fn page_to_pixel_f64_sub_pixel_accuracy() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 150);
        let (px, _) = vp.page_to_pixel_f64(10.333, 50.0);
        let expected_x = 10.333 * (150.0 / 72.0);
        assert!((px - expected_x).abs() < 1e-10);
    }

    #[test]
    fn to_transform_is_consistent_with_page_to_pixel_f64() {
        let vp = Viewport::new([0.0, 0.0, 200.0, 300.0], 96);
        let t = vp.to_transform();
        let (tx, ty) = t.transform_point(100.0, 150.0);
        let (vx, vy) = vp.page_to_pixel_f64(100.0, 150.0);
        assert!((tx - vx).abs() < 0.001);
        assert!((ty - vy).abs() < 0.001);
    }

    #[test]
    fn viewport_new_has_rotation_zero() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        assert_eq!(vp.rotation, 0);
    }

    #[test]
    fn viewport_new_rotated_90_swaps_dimensions() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 90);
        assert_eq!(vp.width_px, 200);
        assert_eq!(vp.height_px, 100);
        assert_eq!(vp.rotation, 90);
    }

    #[test]
    fn viewport_new_rotated_180_preserves_dimensions() {
        let vp_0 = Viewport::new([0.0, 0.0, 100.0, 200.0], 72);
        let vp_180 = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 180);
        assert_eq!(vp_0.width_px, vp_180.width_px);
        assert_eq!(vp_0.height_px, vp_180.height_px);
    }

    #[test]
    fn viewport_new_rotated_270_swaps_dimensions() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 270);
        assert_eq!(vp.width_px, 200);
        assert_eq!(vp.height_px, 100);
    }

    #[test]
    fn to_transform_for_rotation_zero_matches_original() {
        let vp_0 = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let vp_0r = Viewport::new_rotated([0.0, 0.0, 100.0, 100.0], 72, 0);
        let t_orig = vp_0.to_transform();
        let t_rotated = vp_0r.to_transform();
        assert!((t_orig.a - t_rotated.a).abs() < 1e-10);
        assert!((t_orig.d - t_rotated.d).abs() < 1e-10);
        assert!((t_orig.e - t_rotated.e).abs() < 1e-10);
        assert!((t_orig.f - t_rotated.f).abs() < 1e-10);
    }

    #[test]
    fn page_to_pixel_rotation_zero_maps_top_left_to_origin() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 0);
        assert_eq!(vp.page_to_pixel(0.0, 200.0), (0, 0));
    }

    #[test]
    fn viewport_rotation_invalid_values_keep_portrait_dimensions() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 45);
        assert_eq!(vp.rotation, 45);
        assert_eq!(vp.width_px, 100);
        assert_eq!(vp.height_px, 200);
    }

    #[test]
    fn rotation_90_maps_page_center_to_pixel_center() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 90);
        let (px, py) = vp.page_to_pixel(50.0, 100.0);
        assert!((i64::from(px) - 100).abs() <= 2, "90deg x: {px}");
        assert!((i64::from(py) - 50).abs() <= 2, "90deg y: {py}");
    }

    #[test]
    fn rotation_180_maps_page_center_to_pixel_center() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 180);
        let (px, py) = vp.page_to_pixel(50.0, 100.0);
        assert!((i64::from(px) - 50).abs() <= 2, "180deg x: {px}");
        assert!((i64::from(py) - 100).abs() <= 2, "180deg y: {py}");
    }

    #[test]
    fn rotation_270_maps_page_center_to_pixel_center() {
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 270);
        let (px, py) = vp.page_to_pixel(50.0, 100.0);
        assert!((i64::from(px) - 100).abs() <= 2, "270deg x: {px}");
        assert!((i64::from(py) - 50).abs() <= 2, "270deg y: {py}");
    }

    #[test]
    fn render_page_dimensions_respect_rotation() {
        let vp_0 = Viewport::new_rotated([0.0, 0.0, 595.0, 842.0], 72, 0);
        let vp_90 = Viewport::new_rotated([0.0, 0.0, 595.0, 842.0], 72, 90);
        assert!(vp_0.width_px < vp_0.height_px);
        assert!(vp_90.width_px > vp_90.height_px);
        assert_eq!(vp_90.width_px, vp_0.height_px);
        assert_eq!(vp_90.height_px, vp_0.width_px);
    }

    // Regression (Benchmark Fix B): /Rotate 90 and 270 must rotate WITHOUT
    // mirroring. The page→device transform includes the PDF y-up → device
    // y-down flip (a reflection, det < 0); a correct rotated transform keeps
    // det < 0. The previous 90/270 matrices had det > 0 (rotation without the
    // flip) and rendered content mirror-imaged. We assert the determinant sign
    // for every rotation, and that an asymmetric corner maps where a clockwise
    // display rotation (not a mirror) places it.
    #[test]
    fn rotation_transforms_are_proper_not_mirrored() {
        for rot in [0u32, 90, 180, 270] {
            let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, rot);
            let t = vp.to_transform();
            let det = t.a * t.d - t.b * t.c;
            assert!(
                det < 0.0,
                "rotation {rot} must keep the y-flip reflection (det<0), got det={det}"
            );
        }
    }

    #[test]
    fn rotation_270_corner_orientation_is_clockwise() {
        // Page box 100x200. The PDF top-left corner (0, 200) under a clockwise
        // 270° display rotation lands at device (top-left of the rotated 200x100
        // buffer is the page's top-right). Concretely the page's bottom-left
        // (0,0) maps to device top-left-ish; verify (0,0) and (100,0) are NOT
        // mirror images of each other across the buffer.
        let vp = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 270);
        // bottom-left and bottom-right of the page (differ in x only)
        let bl = vp.page_to_pixel(0.0, 0.0);
        let br = vp.page_to_pixel(100.0, 0.0);
        // Under 270°, the x-axis maps to the device y-axis; bl and br must differ
        // in device-y (a real rotation), and the mapping must be orientation-
        // preserving relative to the 90° case (opposite y-direction).
        let vp90 = Viewport::new_rotated([0.0, 0.0, 100.0, 200.0], 72, 90);
        let bl90 = vp90.page_to_pixel(0.0, 0.0);
        let br90 = vp90.page_to_pixel(100.0, 0.0);
        // 270° increasing page-x => increasing device-y; 90° increasing page-x
        // => decreasing device-y. They must move in OPPOSITE directions.
        let d270 = br.1 - bl.1;
        let d90 = br90.1 - bl90.1;
        assert!(
            d270.signum() != d90.signum(),
            "90° and 270° must rotate in opposite senses (no mirror): d90={d90}, d270={d270}"
        );
    }
}
