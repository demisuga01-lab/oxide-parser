use crate::content::state::{Color as GsColor, ColorSpace as GsColorSpace};
use crate::render::buffer::PixelColor;
use crate::render::cmm::{self, CalGrayParams, CalRgbParams, LabParams};

/// Final device-space RGBA color used for pixel blending.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl RenderColor {
    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            r: r.clamp(0.0, 1.0),
            g: g.clamp(0.0, 1.0),
            b: b.clamp(0.0, 1.0),
            a: a.clamp(0.0, 1.0),
        }
    }

    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self::new(r, g, b, 1.0)
    }

    pub fn gray(g: f32) -> Self {
        Self::new(g, g, g, 1.0)
    }

    pub fn transparent() -> Self {
        Self::new(0.0, 0.0, 0.0, 0.0)
    }

    pub fn black() -> Self {
        Self::new(0.0, 0.0, 0.0, 1.0)
    }

    pub fn white() -> Self {
        Self::new(1.0, 1.0, 1.0, 1.0)
    }

    /// Convert to PixelColor ([u8; 4]) for PixelBuffer.
    pub fn to_pixel_color(self) -> PixelColor {
        [
            (self.r * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.g * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.b * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.a * 255.0).round().clamp(0.0, 255.0) as u8,
        ]
    }

    /// Replace the stored alpha.
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self::new(self.r, self.g, self.b, alpha)
    }

    /// Porter-Duff source-over compositing, performed in **sRGB space** to match
    /// the reference renderer (Poppler/Splash) and
    /// [`crate::render::buffer::PixelBuffer::blend_pixel`]. The colour components
    /// are mixed directly in their stored sRGB encoding.
    pub fn alpha_composite(dst: RenderColor, src: RenderColor) -> RenderColor {
        let out_a = src.a + dst.a * (1.0 - src.a);
        if out_a < 1e-6 {
            return RenderColor::transparent();
        }
        let inv_a = 1.0 / out_a;
        let mix = |s: f32, d: f32| -> f32 { (s * src.a + d * dst.a * (1.0 - src.a)) * inv_a };
        RenderColor {
            r: mix(src.r, dst.r),
            g: mix(src.g, dst.g),
            b: mix(src.b, dst.b),
            a: out_a,
        }
    }

    /// Blend source over destination with a coverage multiplier.
    pub fn blend_coverage(dst: RenderColor, src: RenderColor, coverage: f32) -> RenderColor {
        let src_scaled = src.with_alpha(src.a * coverage.clamp(0.0, 1.0));
        Self::alpha_composite(dst, src_scaled)
    }
}

pub struct ColorSpaceHandler;

impl ColorSpaceHandler {
    /// Convert a graphics-state color to final render color.
    pub fn to_render_color(color: &GsColor, alpha: f32) -> RenderColor {
        let comps = &color.components;
        match &color.space {
            GsColorSpace::DeviceGray => {
                let g = comps.first().copied().unwrap_or(0.0) as f32;
                RenderColor::new(g, g, g, alpha)
            }
            GsColorSpace::DeviceRGB => {
                let r = comps.first().copied().unwrap_or(0.0) as f32;
                let g = comps.get(1).copied().unwrap_or(0.0) as f32;
                let b = comps.get(2).copied().unwrap_or(0.0) as f32;
                RenderColor::new(r, g, b, alpha)
            }
            GsColorSpace::DeviceCMYK => {
                let c = comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let m = comps.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let y = comps.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let k = comps.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let [r, g, b] = cmm::device_cmyk_to_srgb(c as f32, m as f32, y as f32, k as f32);
                RenderColor::new(r, g, b, alpha)
            }
            GsColorSpace::Named(name) => {
                log::warn!(
                    "ColorSpaceHandler: unknown color space '{}', using black",
                    name
                );
                RenderColor::new(0.0, 0.0, 0.0, alpha)
            }
        }
    }

    /// Convert raw color-space components without requiring GraphicsState Color.
    pub fn from_components(space_name: &str, components: &[f64], alpha: f32) -> RenderColor {
        match space_name {
            "DeviceGray" | "G" => {
                let g = components.first().copied().unwrap_or(0.0) as f32;
                RenderColor::new(g, g, g, alpha)
            }
            "CalGray" => {
                let g = components.first().copied().unwrap_or(0.0) as f32;
                let [r, g, b] = cmm::cal_gray_to_srgb(g, CalGrayParams::default());
                RenderColor::new(r, g, b, alpha)
            }
            "DeviceRGB" | "RGB" | "sRGB" => {
                let r = components.first().copied().unwrap_or(0.0) as f32;
                let g = components.get(1).copied().unwrap_or(0.0) as f32;
                let b = components.get(2).copied().unwrap_or(0.0) as f32;
                RenderColor::new(r, g, b, alpha)
            }
            "CalRGB" => {
                let comps = [
                    components.first().copied().unwrap_or(0.0) as f32,
                    components.get(1).copied().unwrap_or(0.0) as f32,
                    components.get(2).copied().unwrap_or(0.0) as f32,
                ];
                let [r, g, b] = cmm::cal_rgb_to_srgb(comps, CalRgbParams::default());
                RenderColor::new(r, g, b, alpha)
            }
            "DeviceCMYK" | "CMYK" => {
                let c = components.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let m = components.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let y = components.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let k = components.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let [r, g, b] = cmm::device_cmyk_to_srgb(c as f32, m as f32, y as f32, k as f32);
                RenderColor::new(r, g, b, alpha)
            }
            "Lab" => {
                let l = components.first().copied().unwrap_or(0.0) as f32;
                let a = components.get(1).copied().unwrap_or(0.0) as f32;
                let b = components.get(2).copied().unwrap_or(0.0) as f32;
                let [r, g, b] = cmm::lab_to_srgb(l, a, b, LabParams::default());
                RenderColor::new(r, g, b, alpha)
            }
            other => {
                log::warn!(
                    "ColorSpaceHandler::from_components: unknown space '{}'",
                    other
                );
                RenderColor::black().with_alpha(alpha)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::state::{Color as GsColor, ColorSpace};

    #[test]
    fn to_pixel_color_round_trip() {
        let c = RenderColor::rgb(1.0, 0.5, 0.0);
        let p = c.to_pixel_color();
        assert_eq!(p[0], 255);
        assert!((p[1] as i32 - 128).abs() <= 1);
        assert_eq!(p[2], 0);
        assert_eq!(p[3], 255);
    }

    #[test]
    fn alpha_composite_opaque_source_replaces_destination() {
        let dst = RenderColor::gray(0.5);
        let src = RenderColor::rgb(1.0, 0.0, 0.0);
        let out = RenderColor::alpha_composite(dst, src);
        assert!((out.r - 1.0).abs() < 0.001);
        assert!(out.g.abs() < 0.001);
    }

    #[test]
    fn alpha_composite_transparent_source_leaves_destination() {
        let dst = RenderColor::gray(0.5);
        let src = RenderColor::rgb(1.0, 0.0, 0.0).with_alpha(0.0);
        let out = RenderColor::alpha_composite(dst, src);
        assert!((out.r - 0.5).abs() < 0.001);
    }

    #[test]
    fn alpha_composite_half_red_over_white_is_pink() {
        let dst = RenderColor::white();
        let src = RenderColor::rgb(1.0, 0.0, 0.0).with_alpha(0.5);
        let out = RenderColor::alpha_composite(dst, src);
        assert!(out.r > 0.5);
        assert!(out.g < 1.0 && out.g > 0.0);
    }

    #[test]
    fn color_space_handler_device_gray() {
        let c = GsColor {
            space: ColorSpace::DeviceGray,
            components: vec![0.75],
        };
        let rc = ColorSpaceHandler::to_render_color(&c, 1.0);
        assert!((rc.r - 0.75).abs() < 0.001);
        assert!((rc.g - 0.75).abs() < 0.001);
        assert!((rc.b - 0.75).abs() < 0.001);
        assert_eq!(rc.a, 1.0);
    }

    #[test]
    fn color_space_handler_device_rgb() {
        let c = GsColor {
            space: ColorSpace::DeviceRGB,
            components: vec![1.0, 0.5, 0.25],
        };
        let rc = ColorSpaceHandler::to_render_color(&c, 1.0);
        assert!((rc.r - 1.0).abs() < 0.001);
        assert!((rc.g - 0.5).abs() < 0.001);
        assert!((rc.b - 0.25).abs() < 0.001);
    }

    #[test]
    fn color_space_handler_device_cmyk_white() {
        let c = GsColor {
            space: ColorSpace::DeviceCMYK,
            components: vec![0.0, 0.0, 0.0, 0.0],
        };
        let rc = ColorSpaceHandler::to_render_color(&c, 1.0);
        assert!((rc.r - 1.0).abs() < 0.001);
        assert!((rc.g - 1.0).abs() < 0.001);
        assert!((rc.b - 1.0).abs() < 0.001);
    }

    #[test]
    fn color_space_handler_device_cmyk_black() {
        let c = GsColor {
            space: ColorSpace::DeviceCMYK,
            components: vec![0.0, 0.0, 0.0, 1.0],
        };
        let rc = ColorSpaceHandler::to_render_color(&c, 1.0);
        assert!((rc.r - 35.0 / 255.0).abs() < 0.01);
        assert!((rc.g - 31.0 / 255.0).abs() < 0.01);
        assert!((rc.b - 32.0 / 255.0).abs() < 0.01);
    }

    #[test]
    fn color_space_handler_alpha_scaling() {
        let c = GsColor {
            space: ColorSpace::DeviceGray,
            components: vec![0.0],
        };
        let rc = ColorSpaceHandler::to_render_color(&c, 0.5);
        assert!((rc.a - 0.5).abs() < 0.001);
    }

    #[test]
    fn from_components_device_rgb() {
        let rc = ColorSpaceHandler::from_components("DeviceRGB", &[0.2, 0.4, 0.6], 1.0);
        assert!((rc.r - 0.2).abs() < 0.001);
        assert!((rc.g - 0.4).abs() < 0.001);
        assert!((rc.b - 0.6).abs() < 0.001);
    }

    #[test]
    fn render_color_new_clamps_values() {
        let c = RenderColor::new(2.0, -0.5, 1.5, 0.5);
        assert_eq!(c.r, 1.0);
        assert_eq!(c.g, 0.0);
        assert_eq!(c.b, 1.0);
    }

    #[test]
    fn blend_coverage_with_half_coverage() {
        // 50% coverage of black over white, composited in sRGB space (matching
        // Poppler/Splash and the pixel compositor), lands at the sRGB midpoint
        // 0.5 — not the linear-light value ~0.737.
        let out = RenderColor::blend_coverage(RenderColor::white(), RenderColor::black(), 0.5);
        assert!(
            (out.r - 0.5).abs() < 0.01,
            "sRGB 50% black over white ~0.5, got {}",
            out.r
        );
        assert!(out.a > 0.9);
    }

    #[test]
    fn render_color_gray() {
        let c = RenderColor::gray(0.3);
        assert_eq!(c.r, c.g);
        assert_eq!(c.g, c.b);
        assert!((c.r - 0.3).abs() < 0.001);
    }

    #[test]
    fn alpha_compositing_two_step_blend_keeps_both_colors() {
        let white = RenderColor::white();
        let red = RenderColor::rgb(1.0, 0.0, 0.0).with_alpha(0.5);
        let blue = RenderColor::rgb(0.0, 0.0, 1.0).with_alpha(0.5);
        let after_red = RenderColor::alpha_composite(white, red);
        let after_blue = RenderColor::alpha_composite(after_red, blue);
        assert!(after_blue.b > 0.1);
        assert!(after_blue.r > 0.1);
    }
}
