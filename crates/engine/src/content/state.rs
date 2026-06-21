use crate::content::operation::{ContentOperation, Operand};
use crate::object::{PdfDictionary, PdfObject};

pub type Matrix = [f64; 6];

pub const IDENTITY_MATRIX: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

pub fn concat_matrix(m1: &Matrix, m2: &Matrix) -> Matrix {
    [
        m1[0] * m2[0] + m1[1] * m2[2],
        m1[0] * m2[1] + m1[1] * m2[3],
        m1[2] * m2[0] + m1[3] * m2[2],
        m1[2] * m2[1] + m1[3] * m2[3],
        m1[4] * m2[0] + m1[5] * m2[2] + m2[4],
        m1[4] * m2[1] + m1[5] * m2[3] + m2[5],
    ]
}

pub fn transform_point(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

pub fn transform_vector(m: &Matrix, dx: f64, dy: f64) -> (f64, f64) {
    (m[0] * dx + m[2] * dy, m[1] * dx + m[3] * dy)
}

pub fn translate_matrix(tx: f64, ty: f64) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    Named(String),
}

impl ColorSpace {
    pub fn component_count(&self) -> usize {
        match self {
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceCMYK => 4,
            ColorSpace::Named(_) => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Color {
    pub space: ColorSpace,
    pub components: Vec<f64>,
}

impl Color {
    pub fn device_gray(g: f64) -> Self {
        Self {
            space: ColorSpace::DeviceGray,
            components: vec![g],
        }
    }

    pub fn device_rgb(r: f64, g: f64, b: f64) -> Self {
        Self {
            space: ColorSpace::DeviceRGB,
            components: vec![r, g, b],
        }
    }

    pub fn device_cmyk(c: f64, m: f64, y: f64, k: f64) -> Self {
        Self {
            space: ColorSpace::DeviceCMYK,
            components: vec![c, m, y, k],
        }
    }

    pub fn black() -> Self {
        Self::device_gray(0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineDash {
    pub pattern: Vec<f64>,
    pub phase: f64,
}

impl Default for LineDash {
    fn default() -> Self {
        Self {
            pattern: vec![],
            phase: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LineCap {
    #[default]
    Butt = 0,
    Round = 1,
    ProjectingSquare = 2,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LineJoin {
    #[default]
    Miter = 0,
    Round = 1,
    Bevel = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub fn from_name(s: &str) -> Self {
        match s {
            "Normal" | "Compatible" => BlendMode::Normal,
            "Multiply" => BlendMode::Multiply,
            "Screen" => BlendMode::Screen,
            "Overlay" => BlendMode::Overlay,
            "Darken" => BlendMode::Darken,
            "Lighten" => BlendMode::Lighten,
            "ColorDodge" => BlendMode::ColorDodge,
            "ColorBurn" => BlendMode::ColorBurn,
            "HardLight" => BlendMode::HardLight,
            "SoftLight" => BlendMode::SoftLight,
            "Difference" => BlendMode::Difference,
            "Exclusion" => BlendMode::Exclusion,
            "Hue" => BlendMode::Hue,
            "Saturation" => BlendMode::Saturation,
            "Color" => BlendMode::Color,
            "Luminosity" => BlendMode::Luminosity,
            _ => BlendMode::Normal,
        }
    }

    /// True for *separable* blend modes (spec Table 136 / §11.3.5.2), whose
    /// result is computed channel-by-channel via [`Self::blend_channel`]. The
    /// four non-separable modes (Hue/Saturation/Color/Luminosity) instead operate
    /// on the whole RGB triple via [`Self::blend_rgb`].
    #[inline]
    pub fn is_separable(self) -> bool {
        !matches!(
            self,
            BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity
        )
    }

    /// Apply a *separable* blend mode to one normalized colour channel
    /// (spec Table 137). For non-separable modes this returns `src` unchanged —
    /// callers must use [`Self::blend_rgb`] for those (gated by
    /// [`Self::is_separable`]).
    #[inline]
    pub fn blend_channel(self, src: f32, dst: f32) -> f32 {
        match self {
            BlendMode::Normal => src,
            BlendMode::Multiply => src * dst,
            BlendMode::Screen => src + dst - src * dst,
            BlendMode::Overlay => Self::hard_light_channel(dst, src), // Overlay(s,d)=HardLight(d,s)
            BlendMode::Darken => src.min(dst),
            BlendMode::Lighten => src.max(dst),
            BlendMode::ColorDodge => {
                // Spec §11.3.5.2: backdrop 0 -> 0; src 1 -> 1; else min(1, d/(1-s)).
                if dst <= 0.0 {
                    0.0
                } else if src >= 1.0 {
                    1.0
                } else {
                    (dst / (1.0 - src)).min(1.0)
                }
            }
            BlendMode::ColorBurn => {
                // backdrop 1 -> 1; src 0 -> 0; else 1 - min(1, (1-d)/s).
                if dst >= 1.0 {
                    1.0
                } else if src <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - dst) / src).min(1.0)
                }
            }
            BlendMode::HardLight => Self::hard_light_channel(src, dst),
            BlendMode::SoftLight => Self::soft_light_channel(src, dst),
            BlendMode::Difference => (dst - src).abs(),
            BlendMode::Exclusion => src + dst - 2.0 * src * dst,
            // Non-separable: handled by blend_rgb; per-channel fallback = src.
            BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
                src
            }
        }
    }

    /// HardLight per-channel formula (spec Table 137). Overlay is the same with
    /// the operands swapped.
    #[inline]
    fn hard_light_channel(src: f32, dst: f32) -> f32 {
        if src <= 0.5 {
            2.0 * src * dst
        } else {
            1.0 - 2.0 * (1.0 - src) * (1.0 - dst)
        }
    }

    /// SoftLight per-channel formula (spec Table 137).
    #[inline]
    fn soft_light_channel(src: f32, dst: f32) -> f32 {
        if src <= 0.5 {
            dst - (1.0 - 2.0 * src) * dst * (1.0 - dst)
        } else {
            let d = if dst <= 0.25 {
                ((16.0 * dst - 12.0) * dst + 4.0) * dst
            } else {
                dst.sqrt()
            };
            dst + (2.0 * src - 1.0) * (d - dst)
        }
    }

    /// Apply a *non-separable* blend mode (Hue/Saturation/Color/Luminosity,
    /// spec Table 138 + §11.3.5.3) to the full RGB triples. For separable modes
    /// this applies [`Self::blend_channel`] per channel, so it is a correct
    /// general entry point for any mode.
    #[inline]
    pub fn blend_rgb(self, src: [f32; 3], dst: [f32; 3]) -> [f32; 3] {
        match self {
            BlendMode::Hue => set_lum(set_sat(src, sat(dst)), lum(dst)),
            BlendMode::Saturation => set_lum(set_sat(dst, sat(src)), lum(dst)),
            BlendMode::Color => set_lum(src, lum(dst)),
            BlendMode::Luminosity => set_lum(dst, lum(src)),
            _ => [
                self.blend_channel(src[0], dst[0]),
                self.blend_channel(src[1], dst[1]),
                self.blend_channel(src[2], dst[2]),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Non-separable blend-mode helpers (spec §11.3.5.3)
// ---------------------------------------------------------------------------

/// Luminosity of an RGB colour using the PDF-spec weights (Lum(C), §11.3.5.3).
/// These are the spec's exact coefficients (0.30 / 0.59 / 0.11), which differ
/// slightly from the Rec.601 weights (0.299 / 0.587 / 0.114) used elsewhere for
/// soft-mask luminosity — the spec mandates these specific values here.
#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// Saturation of an RGB colour (Sat(C), §11.3.5.3): max component − min.
#[inline]
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// Clip a colour into gamut while preserving its luminosity (ClipColor(C)).
#[inline]
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        let denom = l - n;
        if denom.abs() > f32::EPSILON {
            for v in &mut out {
                *v = l + (*v - l) * l / denom;
            }
        } else {
            out = [l, l, l];
        }
    }
    if x > 1.0 {
        let denom = x - l;
        if denom.abs() > f32::EPSILON {
            for v in &mut out {
                *v = l + (*v - l) * (1.0 - l) / denom;
            }
        } else {
            out = [l, l, l];
        }
    }
    out
}

/// Set the luminosity of `c` to `l` (SetLum(C, l), §11.3.5.3), clipping to gamut.
#[inline]
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

/// Set the saturation of `c` to `s` (SetSat(C, s), §11.3.5.3) by mapping the
/// mid/max components relative to the min, preserving relative ordering.
#[inline]
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Identify indices of min/mid/max by value (stable ordering by index on ties).
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let (i_min, i_mid, i_max) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[i_max] > c[i_min] {
        out[i_mid] = (c[i_mid] - c[i_min]) * s / (c[i_max] - c[i_min]);
        out[i_max] = s;
    } else {
        out[i_mid] = 0.0;
        out[i_max] = 0.0;
    }
    out[i_min] = 0.0;
    out
}

#[derive(Debug, Clone)]
pub struct TextState {
    pub font_name: String,
    pub font_size: f64,
    pub char_spacing: f64,
    pub word_spacing: f64,
    pub horizontal_scaling: f64,
    pub leading: f64,
    pub rendering_mode: i32,
    pub rise: f64,
    pub tm: Matrix,
    pub tlm: Matrix,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font_name: String::new(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 100.0,
            leading: 0.0,
            rendering_mode: 0,
            rise: 0.0,
            tm: IDENTITY_MATRIX,
            tlm: IDENTITY_MATRIX,
        }
    }
}

impl TextState {
    pub fn begin_text(&mut self) {
        self.tm = IDENTITY_MATRIX;
        self.tlm = IDENTITY_MATRIX;
    }
}

#[derive(Debug, Clone)]
pub struct GraphicsState {
    pub ctm: Matrix,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash: LineDash,
    pub rendering_intent: String,
    pub stroke_color_space: ColorSpace,
    pub fill_color_space: ColorSpace,
    pub stroke_color: Color,
    pub fill_color: Color,
    pub stroke_alpha: f64,
    pub fill_alpha: f64,
    pub blend_mode: BlendMode,
    pub text: TextState,
    pub clip_dirty: bool,
    /// Name of the current fill pattern resource, set by `scn /PatternName`.
    /// Meaningful only when `fill_color_space` is the `Pattern` color space.
    pub fill_pattern_name: Option<String>,
    /// Name of the current stroke pattern resource, set by `SCN /PatternName`.
    pub stroke_pattern_name: Option<String>,
    stack: Vec<GraphicsStateSnapshot>,
}

#[derive(Debug, Clone)]
struct GraphicsStateSnapshot {
    ctm: Matrix,
    line_width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash: LineDash,
    rendering_intent: String,
    stroke_color_space: ColorSpace,
    fill_color_space: ColorSpace,
    stroke_color: Color,
    fill_color: Color,
    stroke_alpha: f64,
    fill_alpha: f64,
    blend_mode: BlendMode,
    text: TextState,
    clip_dirty: bool,
    fill_pattern_name: Option<String>,
    stroke_pattern_name: Option<String>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: IDENTITY_MATRIX,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: LineDash::default(),
            rendering_intent: "RelativeColorimetric".to_string(),
            stroke_color_space: ColorSpace::DeviceGray,
            fill_color_space: ColorSpace::DeviceGray,
            stroke_color: Color::black(),
            fill_color: Color::black(),
            stroke_alpha: 1.0,
            fill_alpha: 1.0,
            blend_mode: BlendMode::Normal,
            text: TextState::default(),
            clip_dirty: false,
            fill_pattern_name: None,
            stroke_pattern_name: None,
            stack: Vec::new(),
        }
    }
}

impl GraphicsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self) {
        self.stack.push(GraphicsStateSnapshot {
            ctm: self.ctm,
            line_width: self.line_width,
            line_cap: self.line_cap.clone(),
            line_join: self.line_join.clone(),
            miter_limit: self.miter_limit,
            dash: self.dash.clone(),
            rendering_intent: self.rendering_intent.clone(),
            stroke_color_space: self.stroke_color_space.clone(),
            fill_color_space: self.fill_color_space.clone(),
            stroke_color: self.stroke_color.clone(),
            fill_color: self.fill_color.clone(),
            stroke_alpha: self.stroke_alpha,
            fill_alpha: self.fill_alpha,
            blend_mode: self.blend_mode,
            text: self.text.clone(),
            clip_dirty: self.clip_dirty,
            fill_pattern_name: self.fill_pattern_name.clone(),
            stroke_pattern_name: self.stroke_pattern_name.clone(),
        });
    }

    pub fn pop(&mut self) {
        match self.stack.pop() {
            Some(snap) => {
                self.ctm = snap.ctm;
                self.line_width = snap.line_width;
                self.line_cap = snap.line_cap;
                self.line_join = snap.line_join;
                self.miter_limit = snap.miter_limit;
                self.dash = snap.dash;
                self.rendering_intent = snap.rendering_intent;
                self.stroke_color_space = snap.stroke_color_space;
                self.fill_color_space = snap.fill_color_space;
                self.stroke_color = snap.stroke_color;
                self.fill_color = snap.fill_color;
                self.stroke_alpha = snap.stroke_alpha;
                self.fill_alpha = snap.fill_alpha;
                self.blend_mode = snap.blend_mode;
                self.text = snap.text;
                self.clip_dirty = snap.clip_dirty;
                self.fill_pattern_name = snap.fill_pattern_name;
                self.stroke_pattern_name = snap.stroke_pattern_name;
            }
            None => log::warn!("GraphicsState::pop called on empty stack"),
        }
    }

    pub fn stack_depth(&self) -> usize {
        self.stack.len()
    }

    pub fn process(&mut self, op: &ContentOperation) {
        match op.operator.as_str() {
            "q" => self.push(),
            "Q" => self.pop(),
            "cm" => self.op_cm(op),
            "w" => self.op_w(op),
            "J" => self.op_j_cap(op),
            "j" => self.op_j_join(op),
            "M" => self.op_miter_limit(op),
            "d" => self.op_d(op),
            "ri" => {
                if let Some(n) = op.name(0) {
                    self.rendering_intent = n.to_string();
                }
            }
            "i" => {}
            "G" => self.op_stroke_gray(op),
            "g" => self.op_fill_gray(op),
            "RG" => self.op_stroke_rgb(op),
            "rg" => self.op_fill_rgb(op),
            "K" => self.op_stroke_cmyk(op),
            "k" => self.op_fill_cmyk(op),
            "CS" => self.op_stroke_color_space(op),
            "cs" => self.op_fill_color_space(op),
            "SC" | "SCN" => self.op_stroke_color_components(op),
            "sc" | "scn" => self.op_fill_color_components(op),
            "gs" => self.op_gs(op),
            "Tf" => self.op_tf(op),
            "Tc" => self.text.char_spacing = op.number(0).unwrap_or(0.0),
            "Tw" => self.text.word_spacing = op.number(0).unwrap_or(0.0),
            "Tz" => self.text.horizontal_scaling = op.number(0).unwrap_or(100.0),
            "TL" => self.text.leading = op.number(0).unwrap_or(0.0),
            "Tr" => self.text.rendering_mode = op.number(0).unwrap_or(0.0) as i32,
            "Ts" => self.text.rise = op.number(0).unwrap_or(0.0),
            "BT" => self.text.begin_text(),
            "ET" => {}
            "Td" => self.op_td(op),
            "TD" => self.op_td_set_leading(op),
            "Tm" => self.op_tm(op),
            "T*" => self.op_tstar(),
            "Tj" => self.op_tj_advance(op),
            "'" => {
                self.op_tstar();
                self.op_tj_advance(op);
            }
            "\"" => {
                if let Some(aw) = op.number(0) {
                    self.text.word_spacing = aw;
                }
                if let Some(ac) = op.number(1) {
                    self.text.char_spacing = ac;
                }
                let shifted = ContentOperation::new(
                    "Tj",
                    vec![op
                        .operand(2)
                        .cloned()
                        .unwrap_or_else(|| Operand::String(vec![]))],
                );
                self.op_tstar();
                self.op_tj_advance(&shifted);
            }
            "TJ" => self.op_tj_array_advance(op),
            "m" | "l" | "c" | "v" | "y" | "h" | "re" => {}
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                self.clip_dirty = false;
            }
            "W" | "W*" => self.clip_dirty = true,
            "Do" => {}
            "BMC" | "BDC" | "EMC" | "MP" | "DP" => {}
            "BX" | "EX" => {}
            "BI" | "ID" | "EI" | "inline_image_data" => {}
            "sh" => {}
            _ => log::warn!("GraphicsState: unknown operator '{}'", op.operator),
        }
    }

    pub fn apply_ext_g_state(&mut self, dict: &PdfDictionary) {
        if let Some(ca) = dict_number(dict, "ca") {
            self.fill_alpha = ca.clamp(0.0, 1.0);
        }
        if let Some(stroke_alpha) = dict_number(dict, "CA") {
            self.stroke_alpha = stroke_alpha.clamp(0.0, 1.0);
        }
        if let Some(lw) = dict_number(dict, "LW") {
            self.line_width = lw.max(0.0);
        }
        if let Some(lc) = dict.get_integer("LC") {
            self.line_cap = match lc {
                1 => LineCap::Round,
                2 => LineCap::ProjectingSquare,
                _ => LineCap::Butt,
            };
        }
        if let Some(lj) = dict.get_integer("LJ") {
            self.line_join = match lj {
                1 => LineJoin::Round,
                2 => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        }
        if let Some(ml) = dict_number(dict, "ML") {
            self.miter_limit = ml.max(1.0);
        }
        if let Some(bm_name) = dict.get_name("BM") {
            self.blend_mode = BlendMode::from_name(bm_name);
        } else if let Some(first_name) = dict
            .get_array("BM")
            .and_then(|arr| arr.iter().find_map(PdfObject::as_name))
        {
            self.blend_mode = BlendMode::from_name(first_name);
        }
    }

    pub fn text_position(&self) -> (f64, f64) {
        (self.text.tm[4], self.text.tm[5])
    }

    pub fn effective_font_size(&self) -> f64 {
        (self.text.tm[0].powi(2) + self.text.tm[1].powi(2)).sqrt()
    }

    fn op_cm(&mut self, op: &ContentOperation) {
        let m = [
            op.number(0).unwrap_or(1.0),
            op.number(1).unwrap_or(0.0),
            op.number(2).unwrap_or(0.0),
            op.number(3).unwrap_or(1.0),
            op.number(4).unwrap_or(0.0),
            op.number(5).unwrap_or(0.0),
        ];
        self.ctm = concat_matrix(&m, &self.ctm);
    }

    fn op_w(&mut self, op: &ContentOperation) {
        self.line_width = op.number(0).unwrap_or(1.0).max(0.0);
    }

    fn op_j_cap(&mut self, op: &ContentOperation) {
        self.line_cap = match op.number(0).map(|n| n as i32).unwrap_or(0) {
            1 => LineCap::Round,
            2 => LineCap::ProjectingSquare,
            _ => LineCap::Butt,
        };
    }

    fn op_j_join(&mut self, op: &ContentOperation) {
        self.line_join = match op.number(0).map(|n| n as i32).unwrap_or(0) {
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        };
    }

    fn op_miter_limit(&mut self, op: &ContentOperation) {
        self.miter_limit = op.number(0).unwrap_or(10.0).max(1.0);
    }

    fn op_d(&mut self, op: &ContentOperation) {
        let pattern = op
            .operand(0)
            .and_then(Operand::as_array)
            .map(|arr| arr.iter().filter_map(Operand::as_number).collect())
            .unwrap_or_default();
        let phase = op.number(1).unwrap_or(0.0);
        self.dash = LineDash { pattern, phase };
    }

    fn op_stroke_gray(&mut self, op: &ContentOperation) {
        let g = op.number(0).unwrap_or(0.0).clamp(0.0, 1.0);
        self.stroke_color_space = ColorSpace::DeviceGray;
        self.stroke_color = Color::device_gray(g);
    }

    fn op_fill_gray(&mut self, op: &ContentOperation) {
        let g = op.number(0).unwrap_or(0.0).clamp(0.0, 1.0);
        self.fill_color_space = ColorSpace::DeviceGray;
        self.fill_color = Color::device_gray(g);
    }

    fn op_stroke_rgb(&mut self, op: &ContentOperation) {
        let r = op.number(0).unwrap_or(0.0).clamp(0.0, 1.0);
        let g = op.number(1).unwrap_or(0.0).clamp(0.0, 1.0);
        let b = op.number(2).unwrap_or(0.0).clamp(0.0, 1.0);
        self.stroke_color_space = ColorSpace::DeviceRGB;
        self.stroke_color = Color::device_rgb(r, g, b);
    }

    fn op_fill_rgb(&mut self, op: &ContentOperation) {
        let r = op.number(0).unwrap_or(0.0).clamp(0.0, 1.0);
        let g = op.number(1).unwrap_or(0.0).clamp(0.0, 1.0);
        let b = op.number(2).unwrap_or(0.0).clamp(0.0, 1.0);
        self.fill_color_space = ColorSpace::DeviceRGB;
        self.fill_color = Color::device_rgb(r, g, b);
    }

    fn op_stroke_cmyk(&mut self, op: &ContentOperation) {
        let c = op.number(0).unwrap_or(0.0).clamp(0.0, 1.0);
        let m = op.number(1).unwrap_or(0.0).clamp(0.0, 1.0);
        let y = op.number(2).unwrap_or(0.0).clamp(0.0, 1.0);
        let k = op.number(3).unwrap_or(0.0).clamp(0.0, 1.0);
        self.stroke_color_space = ColorSpace::DeviceCMYK;
        self.stroke_color = Color::device_cmyk(c, m, y, k);
    }

    fn op_fill_cmyk(&mut self, op: &ContentOperation) {
        let c = op.number(0).unwrap_or(0.0).clamp(0.0, 1.0);
        let m = op.number(1).unwrap_or(0.0).clamp(0.0, 1.0);
        let y = op.number(2).unwrap_or(0.0).clamp(0.0, 1.0);
        let k = op.number(3).unwrap_or(0.0).clamp(0.0, 1.0);
        self.fill_color_space = ColorSpace::DeviceCMYK;
        self.fill_color = Color::device_cmyk(c, m, y, k);
    }

    fn op_stroke_color_space(&mut self, op: &ContentOperation) {
        if let Some(name) = op.name(0) {
            self.stroke_color_space = color_space_from_name(name);
            self.stroke_color = default_color_for(&self.stroke_color_space);
        }
    }

    fn op_fill_color_space(&mut self, op: &ContentOperation) {
        if let Some(name) = op.name(0) {
            self.fill_color_space = color_space_from_name(name);
            self.fill_color = default_color_for(&self.fill_color_space);
        }
    }

    fn op_stroke_color_components(&mut self, op: &ContentOperation) {
        // `SCN` may carry a trailing pattern name (e.g. `SCN /P1`, or
        // `c1..cn /P1` for uncoloured tiling patterns). Record it; numeric
        // components, if any, still update the colour.
        if let Some(name) = trailing_pattern_name(op) {
            self.stroke_pattern_name = Some(name);
        }
        let comps = numeric_components(op);
        if !comps.is_empty() {
            self.stroke_color = Color {
                space: self.stroke_color_space.clone(),
                components: comps,
            };
        }
    }

    fn op_fill_color_components(&mut self, op: &ContentOperation) {
        if let Some(name) = trailing_pattern_name(op) {
            self.fill_pattern_name = Some(name);
        }
        let comps = numeric_components(op);
        if !comps.is_empty() {
            self.fill_color = Color {
                space: self.fill_color_space.clone(),
                components: comps,
            };
        }
    }

    fn op_gs(&mut self, _op: &ContentOperation) {}

    fn op_tf(&mut self, op: &ContentOperation) {
        if let Some(name) = op.name(0) {
            self.text.font_name = name.to_string();
        }
        if let Some(size) = op.number(1) {
            self.text.font_size = size;
        }
    }

    fn op_td(&mut self, op: &ContentOperation) {
        let tx = op.number(0).unwrap_or(0.0);
        let ty = op.number(1).unwrap_or(0.0);
        let t = &self.text;
        let mut new_tlm = t.tlm;
        new_tlm[4] = t.tlm[4] + t.tlm[0] * tx + t.tlm[2] * ty;
        new_tlm[5] = t.tlm[5] + t.tlm[1] * tx + t.tlm[3] * ty;
        self.text.tlm = new_tlm;
        self.text.tm = new_tlm;
    }

    fn op_td_set_leading(&mut self, op: &ContentOperation) {
        let ty = op.number(1).unwrap_or(0.0);
        self.text.leading = -ty;
        self.op_td(op);
    }

    fn op_tm(&mut self, op: &ContentOperation) {
        let m = [
            op.number(0).unwrap_or(1.0),
            op.number(1).unwrap_or(0.0),
            op.number(2).unwrap_or(0.0),
            op.number(3).unwrap_or(1.0),
            op.number(4).unwrap_or(0.0),
            op.number(5).unwrap_or(0.0),
        ];
        self.text.tm = m;
        self.text.tlm = m;
    }

    fn op_tstar(&mut self) {
        let tl = self.text.leading;
        let t = &self.text;
        let mut new_tlm = t.tlm;
        new_tlm[4] = t.tlm[4] + t.tlm[2] * (-tl);
        new_tlm[5] = t.tlm[5] + t.tlm[3] * (-tl);
        self.text.tlm = new_tlm;
        self.text.tm = new_tlm;
    }

    fn advance_text_pos(&mut self, width_text_units: f64) {
        let tx = (width_text_units / 1000.0)
            * self.text.font_size
            * (self.text.horizontal_scaling / 100.0);
        let t = &self.text;
        let adv_x = t.tm[0] * tx + t.tm[2] * 0.0;
        let adv_y = t.tm[1] * tx + t.tm[3] * 0.0;
        let mut new_tm = t.tm;
        new_tm[4] += adv_x;
        new_tm[5] += adv_y;
        self.text.tm = new_tm;
    }

    fn op_tj_advance(&mut self, op: &ContentOperation) {
        if let Some(bytes) = op.string_bytes(0) {
            let n = bytes.len() as f64;
            let advance = n * 500.0
                + n * self.text.char_spacing * 1000.0 / self.text.font_size.max(f64::EPSILON);
            self.advance_text_pos(advance);
        }
    }

    fn op_tj_array_advance(&mut self, op: &ContentOperation) {
        if let Some(arr) = op.operand(0).and_then(Operand::as_array) {
            for elem in arr {
                match elem {
                    Operand::String(bytes) => self.advance_text_pos(bytes.len() as f64 * 500.0),
                    Operand::Integer(n) => self.advance_text_pos(-(*n as f64)),
                    Operand::Real(r) => self.advance_text_pos(-r),
                    _ => {}
                }
            }
        }
    }
}

fn color_space_from_name(name: &str) -> ColorSpace {
    match name {
        "DeviceGray" => ColorSpace::DeviceGray,
        "DeviceRGB" => ColorSpace::DeviceRGB,
        "DeviceCMYK" => ColorSpace::DeviceCMYK,
        other => ColorSpace::Named(other.to_string()),
    }
}

fn default_color_for(cs: &ColorSpace) -> Color {
    Color {
        space: cs.clone(),
        components: vec![0.0; cs.component_count()],
    }
}

fn numeric_components(op: &ContentOperation) -> Vec<f64> {
    (0..op.operands.len())
        .filter_map(|i| op.number(i))
        .map(|value| value.clamp(0.0, 1.0))
        .collect()
}

/// Return the pattern name from a `scn`/`SCN` operator if its last operand is a
/// name (the pattern resource name).
fn trailing_pattern_name(op: &ContentOperation) -> Option<String> {
    op.operands
        .last()
        .and_then(Operand::as_name)
        .map(str::to_string)
}

fn dict_number(dict: &PdfDictionary, key: &str) -> Option<f64> {
    dict.get(key).and_then(PdfObject::as_number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(operator: &str, operands: impl IntoIterator<Item = Operand>) -> ContentOperation {
        ContentOperation::new(operator, operands.into_iter().collect())
    }

    #[test]
    fn push_pop_restores_state() {
        let mut gs = GraphicsState::new();
        gs.process(&op(
            "RG",
            [Operand::Real(0.5), Operand::Real(0.0), Operand::Real(0.0)],
        ));
        gs.process(&op("q", []));
        gs.process(&op(
            "RG",
            [Operand::Real(0.0), Operand::Real(1.0), Operand::Real(0.0)],
        ));
        assert_eq!(gs.stroke_color.components, [0.0, 1.0, 0.0]);
        gs.process(&op("Q", []));
        assert_eq!(gs.stroke_color.components, [0.5, 0.0, 0.0]);
        assert_eq!(gs.stack_depth(), 0);
    }

    #[test]
    fn cm_concatenates_to_ctm() {
        let mut gs = GraphicsState::new();
        gs.process(&op(
            "cm",
            [
                Operand::Real(1.0),
                Operand::Real(0.0),
                Operand::Real(0.0),
                Operand::Real(1.0),
                Operand::Real(10.0),
                Operand::Real(20.0),
            ],
        ));
        assert_eq!(gs.ctm[4], 10.0);
        assert_eq!(gs.ctm[5], 20.0);
        gs.process(&op(
            "cm",
            [
                Operand::Real(2.0),
                Operand::Real(0.0),
                Operand::Real(0.0),
                Operand::Real(2.0),
                Operand::Real(0.0),
                Operand::Real(0.0),
            ],
        ));
        assert_eq!(gs.ctm, [2.0, 0.0, 0.0, 2.0, 10.0, 20.0]);
    }

    #[test]
    fn bt_td_tm_text_matrix() {
        let mut gs = GraphicsState::new();
        gs.process(&op("BT", []));
        assert_eq!(gs.text.tm, IDENTITY_MATRIX);
        gs.process(&op(
            "Tf",
            [Operand::Name("F1".to_string()), Operand::Real(12.0)],
        ));
        gs.process(&op("Td", [Operand::Real(100.0), Operand::Real(700.0)]));
        assert_eq!(gs.text_position(), (100.0, 700.0));
    }

    #[test]
    fn tstar_advances_by_leading() {
        let mut gs = GraphicsState::new();
        gs.process(&op("BT", []));
        gs.process(&op("TL", [Operand::Real(14.0)]));
        gs.process(&op("Td", [Operand::Real(0.0), Operand::Real(700.0)]));
        gs.process(&op("T*", []));
        let (_, y) = gs.text_position();
        assert!(
            (y - 686.0).abs() < 0.001,
            "T* should advance y by -TL: got {y}"
        );
    }

    #[test]
    fn td_upper_sets_leading() {
        let mut gs = GraphicsState::new();
        gs.process(&op("BT", []));
        gs.process(&op("TD", [Operand::Real(10.0), Operand::Real(-14.0)]));
        assert!((gs.text.leading - 14.0).abs() < 0.001);
    }

    #[test]
    fn color_operators_update_color_state() {
        let mut gs = GraphicsState::new();
        gs.process(&op(
            "rg",
            [Operand::Real(1.0), Operand::Real(0.0), Operand::Real(0.5)],
        ));
        assert_eq!(gs.fill_color.components, [1.0, 0.0, 0.5]);
        assert_eq!(gs.fill_color.space, ColorSpace::DeviceRGB);
        gs.process(&op(
            "K",
            [
                Operand::Real(0.1),
                Operand::Real(0.2),
                Operand::Real(0.3),
                Operand::Real(0.4),
            ],
        ));
        assert_eq!(gs.stroke_color.space, ColorSpace::DeviceCMYK);
        assert_eq!(gs.stroke_color.components, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn empty_pop_does_not_panic() {
        let mut gs = GraphicsState::new();
        gs.process(&op("Q", []));
        assert_eq!(gs.ctm, IDENTITY_MATRIX);
        assert_eq!(gs.line_width, 1.0);
        assert_eq!(gs.stack_depth(), 0);
    }

    #[test]
    fn concat_matrix_handles_identity_and_translation() {
        let identity = IDENTITY_MATRIX;
        let translate = translate_matrix(10.0, 20.0);
        let result = concat_matrix(&translate, &identity);
        assert_eq!(result, [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]);

        let t2 = translate_matrix(5.0, 5.0);
        let r2 = concat_matrix(&translate, &t2);
        assert_eq!(r2[4], 15.0);
        assert_eq!(r2[5], 25.0);
    }

    // ---- Blend modes ---------------------------------------------------

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn approx_rgb(actual: [f32; 3], expected: [f32; 3]) -> bool {
        actual
            .iter()
            .zip(expected.iter())
            .all(|(actual, expected)| approx(*actual, *expected))
    }

    #[test]
    fn blend_from_name_parses_all_extended_modes() {
        assert_eq!(BlendMode::from_name("ColorDodge"), BlendMode::ColorDodge);
        assert_eq!(BlendMode::from_name("ColorBurn"), BlendMode::ColorBurn);
        assert_eq!(BlendMode::from_name("HardLight"), BlendMode::HardLight);
        assert_eq!(BlendMode::from_name("SoftLight"), BlendMode::SoftLight);
        assert_eq!(BlendMode::from_name("Difference"), BlendMode::Difference);
        assert_eq!(BlendMode::from_name("Exclusion"), BlendMode::Exclusion);
        assert_eq!(BlendMode::from_name("Hue"), BlendMode::Hue);
        assert_eq!(BlendMode::from_name("Saturation"), BlendMode::Saturation);
        assert_eq!(BlendMode::from_name("Color"), BlendMode::Color);
        assert_eq!(BlendMode::from_name("Luminosity"), BlendMode::Luminosity);
    }

    #[test]
    fn separable_classification() {
        for m in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::ColorDodge,
            BlendMode::HardLight,
            BlendMode::Difference,
        ] {
            assert!(m.is_separable(), "{m:?} should be separable");
        }
        for m in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            assert!(!m.is_separable(), "{m:?} should be non-separable");
        }
    }

    #[test]
    fn color_dodge_edge_and_midrange() {
        // backdrop 0 -> 0 regardless of source.
        assert!(approx(BlendMode::ColorDodge.blend_channel(0.5, 0.0), 0.0));
        // source 1 -> 1 (clamps) for any non-zero backdrop.
        assert!(approx(BlendMode::ColorDodge.blend_channel(1.0, 0.4), 1.0));
        // mid: d/(1-s) = 0.4/(1-0.5) = 0.8.
        assert!(approx(BlendMode::ColorDodge.blend_channel(0.5, 0.4), 0.8));
        // saturates to 1.
        assert!(approx(BlendMode::ColorDodge.blend_channel(0.6, 0.5), 1.0));
    }

    #[test]
    fn color_burn_edge_and_midrange() {
        // backdrop 1 -> 1.
        assert!(approx(BlendMode::ColorBurn.blend_channel(0.5, 1.0), 1.0));
        // source 0 -> 0.
        assert!(approx(BlendMode::ColorBurn.blend_channel(0.0, 0.5), 0.0));
        // mid: 1 - (1-d)/s = 1 - (1-0.6)/0.5 = 1 - 0.8 = 0.2.
        assert!(approx(BlendMode::ColorBurn.blend_channel(0.5, 0.6), 0.2));
    }

    #[test]
    fn hard_light_matches_overlay_swapped() {
        // HardLight(s,d) == Overlay(d,s).
        for &(s, d) in &[(0.3, 0.7), (0.8, 0.2), (0.5, 0.5), (0.1, 0.9)] {
            let hl = BlendMode::HardLight.blend_channel(s, d);
            let ov = BlendMode::Overlay.blend_channel(d, s);
            assert!(
                approx(hl, ov),
                "HardLight({s},{d})={hl} Overlay({d},{s})={ov}"
            );
        }
        // src <= 0.5: 2*s*d. src 0.25, d 0.6 -> 0.3.
        assert!(approx(BlendMode::HardLight.blend_channel(0.25, 0.6), 0.3));
        // src > 0.5: 1 - 2(1-s)(1-d). src 0.75, d 0.6 -> 1-2*0.25*0.4=0.8.
        assert!(approx(BlendMode::HardLight.blend_channel(0.75, 0.6), 0.8));
    }

    #[test]
    fn soft_light_neutral_at_half() {
        // src = 0.5 leaves the backdrop unchanged.
        for &d in &[0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            assert!(
                approx(BlendMode::SoftLight.blend_channel(0.5, d), d),
                "SoftLight(0.5,{d}) should equal {d}"
            );
        }
        // src < 0.5 darkens, src > 0.5 lightens a mid backdrop.
        assert!(BlendMode::SoftLight.blend_channel(0.2, 0.5) < 0.5);
        assert!(BlendMode::SoftLight.blend_channel(0.8, 0.5) > 0.5);
    }

    #[test]
    fn soft_light_both_branches_match_spec_formula() {
        // Spec Table 137, exact per-channel formula (not just directional).
        //
        // Branch src <= 0.5: dst - (1 - 2*src) * dst * (1 - dst).
        //   src=0.2, dst=0.8 -> 0.8 - 0.6 * 0.8 * 0.2 = 0.704.
        assert!(approx(BlendMode::SoftLight.blend_channel(0.2, 0.8), 0.704));
        //   src=0.4, dst=0.6 -> 0.6 - 0.2 * 0.6 * 0.4 = 0.552.
        assert!(approx(BlendMode::SoftLight.blend_channel(0.4, 0.6), 0.552));
        //
        // Branch src > 0.5, dst > 0.25: d = sqrt(dst); dst + (2*src-1)*(d - dst).
        //   src=0.8, dst=0.6 -> 0.6 + 0.6*(sqrt(0.6) - 0.6) = 0.704758...
        let expected_hi = 0.6 + 0.6 * (0.6f32.sqrt() - 0.6);
        assert!(approx(
            BlendMode::SoftLight.blend_channel(0.8, 0.6),
            expected_hi
        ));
        assert!((expected_hi - 0.70476).abs() < 1e-3);
        //
        // Branch src > 0.5, dst <= 0.25: d = ((16*dst-12)*dst + 4)*dst.
        //   src=0.8, dst=0.2 -> d = ((3.2-12)*0.2 + 4)*0.2 = 0.448;
        //                       0.2 + 0.6*(0.448 - 0.2) = 0.3488.
        assert!(approx(BlendMode::SoftLight.blend_channel(0.8, 0.2), 0.3488));
    }

    #[test]
    fn difference_and_exclusion_values() {
        assert!(approx(BlendMode::Difference.blend_channel(0.2, 0.7), 0.5));
        assert!(approx(BlendMode::Difference.blend_channel(0.7, 0.2), 0.5));
        // Exclusion: s + d - 2sd. 0.5,0.5 -> 0.5.
        assert!(approx(BlendMode::Exclusion.blend_channel(0.5, 0.5), 0.5));
        // black source is a no-op (s=0 -> d).
        assert!(approx(BlendMode::Exclusion.blend_channel(0.0, 0.42), 0.42));
    }

    #[test]
    fn nonsep_helpers_lum_sat() {
        // Spec weights: Lum(red)=0.30, Lum(green)=0.59, Lum(blue)=0.11.
        assert!(approx(lum([1.0, 0.0, 0.0]), 0.30));
        assert!(approx(lum([0.0, 1.0, 0.0]), 0.59));
        assert!(approx(lum([0.0, 0.0, 1.0]), 0.11));
        // Sat = max - min.
        assert!(approx(sat([0.2, 0.8, 0.5]), 0.6));
        assert!(approx(sat([0.4, 0.4, 0.4]), 0.0));
    }

    #[test]
    fn nonsep_set_lum_preserves_chroma_shifts_luma() {
        // SetLum increases each channel by (l - Lum(c)) then clips.
        let c = [0.2, 0.4, 0.6];
        let out = set_lum(c, 0.8);
        assert!(approx(lum(out), 0.8), "lum should be set to 0.8: {out:?}");
    }

    #[test]
    fn nonsep_set_sat_sets_target_saturation() {
        let out = set_sat([0.2, 0.5, 0.9], 0.5);
        assert!(approx(sat(out), 0.5), "sat should be 0.5: {out:?}");
        // Min channel goes to 0.
        let min = out[0].min(out[1]).min(out[2]);
        assert!(approx(min, 0.0));
    }

    #[test]
    fn nonsep_color_blend_takes_src_chroma_dst_luma() {
        // Color: result has source hue+sat but backdrop luminosity.
        let src = [1.0, 0.0, 0.0]; // red
        let dst = [0.5, 0.5, 0.5]; // gray, lum 0.5
        let out = BlendMode::Color.blend_rgb(src, dst);
        assert!(approx(lum(out), lum(dst)), "Color keeps dst luma: {out:?}");
    }

    #[test]
    fn nonsep_luminosity_blend_takes_src_luma_dst_chroma() {
        let src = [0.9, 0.9, 0.9]; // bright, lum 0.9
        let dst = [0.8, 0.1, 0.1]; // reddish
        let out = BlendMode::Luminosity.blend_rgb(src, dst);
        assert!(
            approx(lum(out), lum(src)),
            "Luminosity takes src luma: {out:?}"
        );
    }

    #[test]
    fn nonsep_hue_and_saturation_keep_dst_luma() {
        let src = [0.0, 0.0, 1.0];
        let dst = [0.6, 0.4, 0.2];
        let hue = BlendMode::Hue.blend_rgb(src, dst);
        let satm = BlendMode::Saturation.blend_rgb(src, dst);
        assert!(approx(lum(hue), lum(dst)), "Hue keeps dst luma: {hue:?}");
        assert!(
            approx(lum(satm), lum(dst)),
            "Saturation keeps dst luma: {satm:?}"
        );
    }

    /// Full-RGB-vector exact assertions for the four non-separable modes
    /// (spec Table 138 + §11.3.5.3). Each expected vector was computed by
    /// hand via the spec helpers (Lum / Sat / SetLum / SetSat / ClipColor)
    /// with the PDF Lum weights 0.30 / 0.59 / 0.11, and is asserted on all
    /// three channels — the earlier property-only `lum()` checks proved only
    /// the luma contract, not the channel-wise result.
    #[test]
    fn nonsep_color_blend_full_vector() {
        // Color(red=[1,0,0], gray=[0.5,0.5,0.5]):
        //   = set_lum([1,0,0], lum([0.5,0.5,0.5])=0.5)
        //   raw = [1.2, 0.2, 0.2]; clip at x=1.2 to gamut -> [1.0, 0.285714, 0.285714]
        let out = BlendMode::Color.blend_rgb([1.0, 0.0, 0.0], [0.5, 0.5, 0.5]);
        assert!((out[0] - 1.0).abs() < 1e-3, "R = 1.0, got {out:?}");
        assert!(
            (out[1] - 2.0 / 7.0).abs() < 1e-3 && (out[2] - 2.0 / 7.0).abs() < 1e-3,
            "G = B = 2/7, got {out:?}"
        );
        // And the luma contract (redundancy with the lum-only check).
        assert!(approx(lum(out), lum([0.5, 0.5, 0.5])));
    }

    #[test]
    fn nonsep_luminosity_blend_full_vector() {
        // Luminosity(gray=[0.9,0.9,0.9], red=[0.8,0.1,0.1]):
        //   = set_lum([0.8,0.1,0.1], lum([0.9,0.9,0.9])=0.9)
        //   raw = [1.39, 0.69, 0.69]; clip at x=1.39 -> [1.0, 6/7, 6/7]
        let out = BlendMode::Luminosity.blend_rgb([0.9, 0.9, 0.9], [0.8, 0.1, 0.1]);
        assert!((out[0] - 1.0).abs() < 1e-3, "R = 1.0, got {out:?}");
        assert!(
            (out[1] - 6.0 / 7.0).abs() < 1e-3 && (out[2] - 6.0 / 7.0).abs() < 1e-3,
            "G = B = 6/7, got {out:?}"
        );
        assert!(approx(lum(out), lum([0.9, 0.9, 0.9])));
    }

    #[test]
    fn nonsep_hue_blend_full_vector() {
        // Hue(blue=[0,0,1], dst=[0.6,0.4,0.2]):
        //   = set_lum(set_sat([0,0,1], sat(dst)=0.4), lum(dst)=0.438)
        //   set_sat([0,0,1], 0.4): max=1 -> out=[0, 0, 0.4]
        //   set_lum([0,0,0.4], 0.438): raw=[0.394, 0.394, 0.794]; in gamut.
        let out = BlendMode::Hue.blend_rgb([0.0, 0.0, 1.0], [0.6, 0.4, 0.2]);
        assert!(
            approx(out[0], 0.394) && approx(out[1], 0.394) && (out[2] - 0.794).abs() < 1e-3,
            "Hue vector = [0.394, 0.394, 0.794], got {out:?}"
        );
        assert!(approx(lum(out), lum([0.6, 0.4, 0.2])));
    }

    #[test]
    fn nonsep_saturation_blend_full_vector() {
        // Saturation(blue=[0,0,1], dst=[0.6,0.4,0.2]):
        //   = set_lum(set_sat([0.6,0.4,0.2], sat(blue)=1.0), lum(dst)=0.438)
        //   set_sat([0.6,0.4,0.2], 1.0): [1.0, 0.5, 0]
        //   set_lum([1.0,0.5,0], 0.438): raw=[0.843, 0.343, -0.157];
        //   clip at n=-0.157 -> [0.73613, 0.36807, 0.0]
        let out = BlendMode::Saturation.blend_rgb([0.0, 0.0, 1.0], [0.6, 0.4, 0.2]);
        let exp = [0.736_134_4_f32, 0.368_067_2_f32, 0.0];
        for i in 0..3 {
            assert!(
                (out[i] - exp[i]).abs() < 1e-3,
                "channel {i} = {} exp {}",
                out[i],
                exp[i]
            );
        }
        assert!(approx(lum(out), lum([0.6, 0.4, 0.2])));
    }

    #[test]
    fn all_blend_modes_match_spec_table() {
        let src = [0.2, 0.6, 0.9];
        let dst = [0.7, 0.3, 0.4];
        let cases: &[(BlendMode, [f32; 3])] = &[
            (BlendMode::Normal, [0.2, 0.6, 0.9]),
            (BlendMode::Multiply, [0.14, 0.18, 0.36]),
            (BlendMode::Screen, [0.76, 0.72, 0.94]),
            (BlendMode::Overlay, [0.52, 0.36, 0.72]),
            (BlendMode::Darken, [0.2, 0.3, 0.4]),
            (BlendMode::Lighten, [0.7, 0.6, 0.9]),
            (BlendMode::ColorDodge, [0.875, 0.75, 1.0]),
            (BlendMode::ColorBurn, [0.0, 0.0, 0.333_333_34]),
            (BlendMode::HardLight, [0.28, 0.44, 0.88]),
            (BlendMode::SoftLight, [0.574, 0.349_544_5, 0.585_964_44]),
            (BlendMode::Difference, [0.5, 0.3, 0.5]),
            (BlendMode::Exclusion, [0.62, 0.54, 0.58]),
            (BlendMode::Hue, [0.252_142_85, 0.480_714_3, 0.652_142_9]),
            (BlendMode::Saturation, [0.901_75, 0.201_75, 0.376_75]),
            (BlendMode::Color, [0.118, 0.518, 0.818]),
            (BlendMode::Luminosity, [0.782, 0.382, 0.482]),
        ];

        for &(mode, expected) in cases {
            let actual = mode.blend_rgb(src, dst);
            assert!(
                approx_rgb(actual, expected),
                "{mode:?} actual {actual:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn blend_rgb_separable_matches_per_channel() {
        // For a separable mode, blend_rgb must equal per-channel blend_channel.
        let src = [0.3, 0.6, 0.9];
        let dst = [0.5, 0.5, 0.5];
        let out = BlendMode::Multiply.blend_rgb(src, dst);
        for i in 0..3 {
            assert!(approx(
                out[i],
                BlendMode::Multiply.blend_channel(src[i], dst[i])
            ));
        }
    }
}
