use crate::cancel::CancelToken;
use crate::content::operation::{ContentOperation, Operand};
use crate::content::state::{BlendMode, ColorSpace, GraphicsState};
use crate::engine::{ContentEngine, PageResources};
use crate::error::Result;
use crate::fonts::cid::{cid_font_has_embedded_program, cid_to_gid};
use crate::fonts::resolver::{detect_font_subtype, get_descendant_font, FontSubtype};
use crate::fonts::variations::VariationRequest;
use crate::fonts::FontResolver;
use crate::images::decoder::ImageDecoder;
use crate::images::locator::ImageReference;
use crate::info::decode_pdf_text_string;
use crate::object::{PdfDictionary, PdfObject};
use crate::render::buffer::{AlphaMask, ClipMask, PixelBuffer, PixelColor, RenderMode, WHITE};
use crate::render::color::ColorSpaceHandler;
use crate::render::font_rasterizer::{get_fallback_font, FontRasterizer};
use crate::render::glyph_cache::{CachedGlyph, GlyphCache, GlyphCacheKey};
use crate::render::image_painter::ImagePainter;
use crate::render::line::DashState;
use crate::render::path::{flatten_path, FillRule, FlatPath, GlyphHinting, Path, PathPainter};
use crate::render::shading::ShadingRenderer;
use crate::render::transform::{Transform2D, Viewport};
use std::fmt::Write as _;

pub struct PageRenderer;

impl PageRenderer {
    /// Render a single PDF page to a PixelBuffer at the given DPI.
    pub fn render_page(
        engine: &ContentEngine,
        page_number: usize,
        dpi: u32,
    ) -> Result<PixelBuffer> {
        Self::render_page_cancellable(engine, page_number, dpi, &CancelToken::none())
    }

    /// Render a single PDF page to a PixelBuffer with an explicit render mode.
    pub fn render_page_with_mode(
        engine: &ContentEngine,
        page_number: usize,
        dpi: u32,
        render_mode: RenderMode,
    ) -> Result<PixelBuffer> {
        Self::render_page_cancellable_with_mode(
            engine,
            page_number,
            dpi,
            &CancelToken::none(),
            render_mode,
        )
    }

    /// Render a page, polling `cancel` periodically so a runaway content
    /// stream can be stopped from outside (e.g. a request-timeout timer).
    pub fn render_page_cancellable(
        engine: &ContentEngine,
        page_number: usize,
        dpi: u32,
        cancel: &CancelToken,
    ) -> Result<PixelBuffer> {
        Self::render_page_cancellable_with_mode(
            engine,
            page_number,
            dpi,
            cancel,
            RenderMode::Compat,
        )
    }

    /// Render a page with cancellation and an explicit render mode.
    pub fn render_page_cancellable_with_mode(
        engine: &ContentEngine,
        page_number: usize,
        dpi: u32,
        cancel: &CancelToken,
        render_mode: RenderMode,
    ) -> Result<PixelBuffer> {
        let ops = engine.get_page_content(page_number)?;
        let viewport = engine.page_viewport(page_number, dpi)?;
        let resources = engine.get_page_resources(page_number)?;
        let transparent_page_group = uses_top_level_transparency(&ops, &resources, engine);
        let buf = if transparent_page_group {
            PixelBuffer::new_transparent_with_mode(
                viewport.width_px,
                viewport.height_px,
                render_mode,
            )
        } else {
            PixelBuffer::new_filled_with_mode(
                viewport.width_px,
                viewport.height_px,
                WHITE,
                render_mode,
            )
        };

        let mut state = RenderState::new(buf, viewport, resources, engine, page_number);
        state.cancel = cancel.clone();
        state.dispatch_all(&ops);
        // dispatch_all bails out early (without error) when the token trips;
        // surface that as a distinct error so the caller returns a timeout
        // response rather than a half-rendered page presented as success.
        cancel.check("page render")?;
        state.render_page_annotations();
        cancel.check("page annotation render")?;
        let mut buf = state.into_buffer();
        if transparent_page_group {
            buf.flatten_onto_background(WHITE);
        }
        Ok(buf)
    }
}

struct RenderState<'a> {
    engine: &'a ContentEngine,
    page_number: usize,
    buf: PixelBuffer,
    viewport: Viewport,
    resources: PageResources,
    gs: GraphicsState,
    clip_stack: Vec<Option<ClipMask>>,
    smask_stack: Vec<Option<AlphaMask>>,
    path: Path,
    pending_clip: Option<FillRule>,
    glyph_cache: GlyphCache,
    /// Current Form XObject nesting depth, used to bound recursion.
    form_depth: usize,
    /// Parameters from the most recent `ID` operator, awaiting the following
    /// `inline_image_data` so the inline image can be painted.
    pending_inline: Option<Vec<Operand>>,
    /// The CTM in effect at the start of the current content stream (the page's
    /// or a Form's). Pattern `/Matrix` values are relative to *this* default
    /// coordinate system, not the CTM at the moment of the fill.
    base_ctm: Transform2D,
    /// Cooperative cancellation flag, polled by the operator dispatch loop and
    /// the tiling-pattern tile loop so a runaway page can be stopped from
    /// outside. Child states (Form groups, soft masks) share the same token.
    cancel: CancelToken,
}

impl<'a> RenderState<'a> {
    fn new(
        buf: PixelBuffer,
        viewport: Viewport,
        resources: PageResources,
        engine: &'a ContentEngine,
        page_number: usize,
    ) -> Self {
        Self {
            engine,
            page_number,
            buf,
            viewport,
            resources,
            gs: GraphicsState::default(),
            clip_stack: Vec::new(),
            smask_stack: Vec::new(),
            path: Path::new(),
            pending_clip: None,
            glyph_cache: GlyphCache::with_default_capacity(),
            form_depth: 0,
            pending_inline: None,
            base_ctm: Transform2D::identity(),
            cancel: CancelToken::none(),
        }
    }

    fn into_buffer(self) -> PixelBuffer {
        self.buf
    }

    fn dispatch_all(&mut self, ops: &[ContentOperation]) {
        // Poll the cancellation flag every CANCEL_CHECK_INTERVAL operators. An
        // atomic relaxed load is cheap, but doing it per-operator on a hot path
        // with millions of trivial ops is measurable, so we amortise it. The
        // interval is small enough that even when individual operators are
        // expensive (e.g. full-page fills) the wall-clock gap between checks
        // stays short, so cancellation is observed promptly. When the token
        // trips we stop executing immediately; the entry point converts the
        // early exit into an OxideError::Cancelled.
        const CANCEL_CHECK_INTERVAL: usize = 64;
        for (i, op) in ops.iter().enumerate() {
            if i % CANCEL_CHECK_INTERVAL == 0 && self.cancel.is_cancelled() {
                return;
            }
            self.dispatch(op);
        }
    }

    fn dispatch(&mut self, op: &ContentOperation) {
        match op.operator.as_str() {
            "m" => {
                if let (Some(x), Some(y)) = (op.number(0), op.number(1)) {
                    self.path.move_to(x, y);
                }
            }
            "l" => {
                if let (Some(x), Some(y)) = (op.number(0), op.number(1)) {
                    self.path.line_to(x, y);
                }
            }
            "c" => {
                if let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x3), Some(y3)) = (
                    op.number(0),
                    op.number(1),
                    op.number(2),
                    op.number(3),
                    op.number(4),
                    op.number(5),
                ) {
                    self.path.curve_to(x1, y1, x2, y2, x3, y3);
                }
            }
            "v" => {
                if let (Some(x2), Some(y2), Some(x3), Some(y3)) =
                    (op.number(0), op.number(1), op.number(2), op.number(3))
                {
                    let (cx, cy) = self.path.current_point.unwrap_or((0.0, 0.0));
                    self.path.curve_to(cx, cy, x2, y2, x3, y3);
                }
            }
            "y" => {
                if let (Some(x1), Some(y1), Some(x3), Some(y3)) =
                    (op.number(0), op.number(1), op.number(2), op.number(3))
                {
                    self.path.curve_to(x1, y1, x3, y3, x3, y3);
                }
            }
            "h" => self.path.close(),
            "re" => {
                if let (Some(x), Some(y), Some(w), Some(h)) =
                    (op.number(0), op.number(1), op.number(2), op.number(3))
                {
                    self.path.rect(x, y, w, h);
                }
            }
            "S" => self.stroke_and_clear(),
            "s" => {
                self.path.close();
                self.stroke_and_clear();
            }
            "f" | "F" => self.fill_and_clear(FillRule::NonZero),
            "f*" => self.fill_and_clear(FillRule::EvenOdd),
            "B" => self.fill_stroke_and_clear(FillRule::NonZero),
            "B*" => self.fill_stroke_and_clear(FillRule::EvenOdd),
            "b" => {
                self.path.close();
                self.fill_stroke_and_clear(FillRule::NonZero);
            }
            "b*" => {
                self.path.close();
                self.fill_stroke_and_clear(FillRule::EvenOdd);
            }
            "n" => {
                self.apply_pending_clip();
                self.path.clear();
            }
            "W" => self.pending_clip = Some(FillRule::NonZero),
            "W*" => self.pending_clip = Some(FillRule::EvenOdd),
            "q" => {
                self.clip_stack.push(self.buf.clip_mask().cloned());
                self.smask_stack.push(self.buf.smask_mask().cloned());
                self.gs.process(op);
            }
            "Q" => {
                self.gs.process(op);
                self.sync_blend_mode();
                match self.clip_stack.pop() {
                    Some(saved) => self.buf.restore_clip(saved),
                    None => log::warn!("PageRenderer: Q with empty clip stack"),
                }
                match self.smask_stack.pop() {
                    Some(saved) => self.buf.restore_smask(saved),
                    None => log::warn!("PageRenderer: Q with empty SMask stack"),
                }
            }
            "Do" => {
                if let Some(name) = op.name(0) {
                    self.handle_do(name);
                }
            }
            "gs" => {
                self.gs.process(op);
                self.apply_ext_g_state(op);
            }
            "Tj" => {
                if let Some(bytes) = op.string_bytes(0) {
                    self.render_text_string(bytes);
                }
            }
            "TJ" => self.render_text_array(op),
            "'" => {
                self.move_to_next_text_line();
                if let Some(bytes) = op.string_bytes(0) {
                    self.render_text_string(bytes);
                }
            }
            "\"" => {
                if let Some(word_spacing) = op.number(0) {
                    self.gs.text.word_spacing = word_spacing;
                }
                if let Some(char_spacing) = op.number(1) {
                    self.gs.text.char_spacing = char_spacing;
                }
                self.move_to_next_text_line();
                if let Some(bytes) = op.string_bytes(2) {
                    self.render_text_string(bytes);
                }
            }
            "BT" | "ET" | "Tf" | "Td" | "TD" | "Tm" | "T*" | "Tc" | "Tw" | "Tz" | "TL" | "Tr"
            | "Ts" | "cm" | "w" | "J" | "j" | "M" | "d" | "ri" | "i" | "G" | "g" | "RG" | "rg"
            | "K" | "k" | "CS" | "cs" | "SC" | "SCN" | "sc" | "scn" => {
                self.gs.process(op);
            }
            "sh" => {
                if let Some(name) = op.name(0) {
                    self.handle_sh(name.to_string());
                }
            }
            "ID" => {
                // Stash the inline image parameters; the pixel bytes arrive in
                // the following `inline_image_data` operation.
                self.pending_inline = Some(op.operands.clone());
            }
            "inline_image_data" => {
                if let (Some(params), Some(bytes)) =
                    (self.pending_inline.take(), op.string_bytes(0))
                {
                    self.paint_inline_image(&params, bytes);
                }
            }
            "BMC" | "BDC" | "EMC" | "MP" | "DP" | "BX" | "EX" | "BI" | "EI" => {}
            _ => self.gs.process(op),
        }
    }

    fn ctm(&self) -> Transform2D {
        Transform2D::from(self.gs.ctm)
    }

    /// Push the graphics-state blend mode onto the pixel buffer.
    fn sync_blend_mode(&mut self) {
        self.buf.blend_mode = self.gs.blend_mode;
    }

    fn fill_pixel_color(&self) -> PixelColor {
        self.resolve_paint_color(&self.gs.fill_color, self.gs.fill_alpha as f32)
    }

    fn stroke_pixel_color(&self) -> PixelColor {
        self.resolve_paint_color(&self.gs.stroke_color, self.gs.stroke_alpha as f32)
    }

    /// Resolve a graphics-state colour to a device pixel colour. Device spaces go
    /// straight through [`ColorSpaceHandler`]; a `Named` space is looked up in the
    /// page resources and, if it is a `/Separation` or `/DeviceN` space, its tint
    /// transform is evaluated and converted via the alternate space.
    /// `/Separation /None` (and all-`/None` DeviceN) resolve to a fully
    /// transparent colour so the paint produces no marks.
    fn resolve_paint_color(&self, color: &crate::content::state::Color, alpha: f32) -> PixelColor {
        if let ColorSpace::Named(name) = &color.space {
            if let Some(space_obj) = self.resources.color_spaces.get(name) {
                let reader = self.engine.document().reader();
                match crate::render::colorspace::resolve_named_color(
                    space_obj,
                    &color.components,
                    alpha,
                    reader,
                ) {
                    crate::render::colorspace::NamedColor::Color(rc) => return rc.to_pixel_color(),
                    crate::render::colorspace::NamedColor::NoPaint => {
                        return crate::render::color::RenderColor::transparent().to_pixel_color();
                    }
                    crate::render::colorspace::NamedColor::Unhandled => {}
                }
            }
        }
        ColorSpaceHandler::to_render_color(color, alpha).to_pixel_color()
    }

    fn dash_state(&self) -> DashState {
        if self.gs.dash.pattern.is_empty() {
            DashState::solid()
        } else {
            DashState::new(self.gs.dash.pattern.clone(), self.gs.dash.phase)
        }
    }

    fn apply_pending_clip(&mut self) {
        if let Some(rule) = self.pending_clip.take() {
            let ctm = self.ctm();
            let flat = flatten_path(&self.path, &ctm, &self.viewport, 0.5);
            let clip = ClipMask::from_path(&flat, self.buf.width, self.buf.height, rule);
            self.buf.set_clip(clip);
        }
    }

    fn stroke_and_clear(&mut self) {
        self.apply_pending_clip();
        let ctm = self.ctm();
        let color = self.stroke_pixel_color();
        let width = self.gs.line_width;
        let dash = self.dash_state();
        PathPainter::stroke_with_style(
            &mut self.buf,
            &self.path,
            &ctm,
            &self.viewport,
            color,
            width,
            &dash,
            &self.gs.line_cap,
            &self.gs.line_join,
            self.gs.miter_limit,
        );
        self.path.clear();
    }

    fn fill_and_clear(&mut self, rule: FillRule) {
        self.apply_pending_clip();
        if self.is_pattern_fill() {
            if let Some(pattern_name) = self.gs.fill_pattern_name.clone() {
                self.paint_pattern_fill(rule, &pattern_name);
            } else {
                log::debug!("PageRenderer: Pattern fill color space without a pattern name");
            }
            self.path.clear();
            return;
        }
        let ctm = self.ctm();
        let color = self.fill_pixel_color();
        PathPainter::fill(&mut self.buf, &self.path, &ctm, &self.viewport, color, rule);
        self.path.clear();
    }

    fn fill_stroke_and_clear(&mut self, rule: FillRule) {
        self.apply_pending_clip();
        let ctm = self.ctm();
        if self.is_pattern_fill() {
            if let Some(pattern_name) = self.gs.fill_pattern_name.clone() {
                self.paint_pattern_fill(rule, &pattern_name);
            }
        } else {
            let fill = self.fill_pixel_color();
            PathPainter::fill(&mut self.buf, &self.path, &ctm, &self.viewport, fill, rule);
        }
        let stroke = self.stroke_pixel_color();
        let width = self.gs.line_width;
        let dash = self.dash_state();
        PathPainter::stroke_with_style(
            &mut self.buf,
            &self.path,
            &ctm,
            &self.viewport,
            stroke,
            width,
            &dash,
            &self.gs.line_cap,
            &self.gs.line_join,
            self.gs.miter_limit,
        );
        self.path.clear();
    }

    /// True when the current fill color space is the Pattern space, either
    /// directly (`/Pattern cs`) or via a named resource that resolves to a
    /// `[/Pattern ...]` array (`/Cs cs` where `/Cs` is defined as a Pattern
    /// color space in the page resources).
    fn is_pattern_fill(&self) -> bool {
        match &self.gs.fill_color.space {
            ColorSpace::Named(name) if name == "Pattern" => true,
            ColorSpace::Named(name) => self.named_space_is_pattern(name),
            _ => false,
        }
    }

    /// Check whether a named color-space resource resolves to a Pattern space.
    fn named_space_is_pattern(&self, name: &str) -> bool {
        let Some(obj) = self.resources.color_spaces.get(name) else {
            return false;
        };
        match obj {
            PdfObject::Name(n) => n == "Pattern",
            PdfObject::Array(arr) => arr.first().and_then(PdfObject::as_name) == Some("Pattern"),
            _ => false,
        }
    }

    fn apply_ext_g_state(&mut self, op: &ContentOperation) {
        let Some(name) = op.name(0) else {
            return;
        };
        if let Some(dict) = self.resources.ext_g_states.get(name).cloned() {
            self.gs.apply_ext_g_state(&dict);
            self.sync_blend_mode();
            self.apply_ext_g_state_smask(&dict);
        } else {
            log::warn!("PageRenderer: ExtGState '{}' not found", name);
        }
    }

    fn apply_ext_g_state_smask(&mut self, dict: &PdfDictionary) {
        let Some(smask_val) = dict.get("SMask") else {
            return;
        };
        match smask_val {
            PdfObject::Name(name) if name == "None" => self.buf.clear_smask(),
            PdfObject::Dictionary(smask_dict) => self.apply_smask(smask_dict.clone()),
            PdfObject::Reference { number, generation } => {
                let reader = self.engine.document().reader();
                match reader.get_and_resolve(*number, *generation) {
                    Ok(PdfObject::Dictionary(smask_dict)) => self.apply_smask(smask_dict),
                    Ok(other) => log::debug!(
                        "PageRenderer: SMask reference resolved to {}, expected Dictionary",
                        other.variant_name()
                    ),
                    Err(err) => log::debug!("PageRenderer: failed to resolve SMask: {}", err),
                }
            }
            _ => log::debug!("PageRenderer: unsupported SMask value"),
        }
    }

    fn apply_smask(&mut self, smask_dict: PdfDictionary) {
        // Subtype: /Luminosity (default) converts the rendered mask group's RGB
        // to a luminance value; /Alpha uses the group's own alpha channel.
        let subtype = smask_dict.get_name("S").unwrap_or("Luminosity");
        let is_alpha = subtype == "Alpha";
        if subtype != "Luminosity" && subtype != "Alpha" {
            log::debug!(
                "PageRenderer: SMask /S '{}' is not supported; using luminosity",
                subtype
            );
        }

        let reader = self.engine.document().reader();
        let Some(g_obj) = smask_dict.get("G").cloned() else {
            log::debug!("PageRenderer: SMask is missing /G");
            return;
        };
        let (g_dict, g_raw) = match g_obj {
            PdfObject::Reference { number, generation } => {
                match reader.get_object(number, generation) {
                    Ok(PdfObject::Stream { dict, raw }) => (dict, raw),
                    Ok(other) => {
                        log::debug!(
                            "PageRenderer: SMask /G resolved to {}, expected Stream",
                            other.variant_name()
                        );
                        return;
                    }
                    Err(err) => {
                        log::debug!("PageRenderer: failed to resolve SMask /G: {}", err);
                        return;
                    }
                }
            }
            PdfObject::Stream { dict, raw } => (dict, raw),
            _ => {
                log::debug!("PageRenderer: SMask /G is not a Form stream");
                return;
            }
        };

        if g_dict.get_name("Subtype") != Some("Form") {
            log::debug!("PageRenderer: SMask /G is not /Subtype /Form");
            return;
        }

        let stream_obj = PdfObject::Stream {
            dict: g_dict.clone(),
            raw: g_raw,
        };
        let content_bytes = match crate::filters::decode_stream(&stream_obj, reader) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::warn!("PageRenderer: SMask /G stream decode failed: {}", err);
                return;
            }
        };

        let g_resources = if let Some(res_obj) = g_dict.get("Resources") {
            let form_res = crate::engine::parse_resources_from_obj(res_obj, reader);
            merge_resources(form_res, &self.resources)
        } else {
            self.resources.clone()
        };

        let form_matrix = extract_form_matrix(&g_dict);
        let current_ctm = Transform2D::from(self.gs.ctm);
        let form_t = Transform2D::from(form_matrix);
        let mut mask_gs = self.gs.clone();
        mask_gs.ctm = form_t.concat(&current_ctm).to_array();
        // The mask group renders from a clean compositing state.
        mask_gs.fill_alpha = 1.0;
        mask_gs.stroke_alpha = 1.0;
        mask_gs.blend_mode = BlendMode::Normal;
        let mask_base_ctm = Transform2D::from(mask_gs.ctm);

        // Backdrop initialization per subtype:
        //  - Luminosity: opaque black (mask 0 = fully masked) so areas the mask
        //    group does not paint stay masked out. /BC overrides the backdrop
        //    color (still opaque). Black-backdrop is the spec default.
        //  - Alpha: fully transparent, so the alpha channel reflects only what
        //    the mask group actually paints.
        let render_mode = self.buf.render_mode();
        let mut mask_buf = if is_alpha {
            PixelBuffer::new_transparent_with_mode(self.buf.width, self.buf.height, render_mode)
        } else {
            let bc = smask_backdrop_color(&smask_dict, &g_dict);
            PixelBuffer::new_filled_with_mode(self.buf.width, self.buf.height, bc, render_mode)
        };
        mask_buf.blend_mode = BlendMode::Normal;

        let mut mask_state = RenderState {
            engine: self.engine,
            page_number: self.page_number,
            buf: mask_buf,
            viewport: self.viewport.clone(),
            resources: g_resources,
            gs: mask_gs,
            clip_stack: Vec::new(),
            smask_stack: Vec::new(),
            path: Path::new(),
            pending_clip: None,
            glyph_cache: GlyphCache::with_default_capacity(),
            form_depth: self.form_depth + 1,
            pending_inline: None,
            base_ctm: mask_base_ctm,
            cancel: self.cancel.clone(),
        };

        if let Some(bbox) = extract_bbox(&g_dict) {
            mask_state.apply_form_bbox_clip(bbox);
        }

        let ops = match crate::content::ContentParser::parse(&content_bytes) {
            Ok(ops) => ops,
            Err(err) => {
                log::warn!("PageRenderer: SMask /G content parse failed: {}", err);
                return;
            }
        };
        mask_state.dispatch_all(&ops);
        let mask_buf = mask_state.into_buffer();

        let mut mask = if is_alpha {
            AlphaMask::from_alpha_channel(&mask_buf)
        } else {
            AlphaMask::from_luminosity(&mask_buf)
        };

        // Apply the /TR transfer function if present and not the /Identity
        // no-op. All function types (0/2/3/4) are supported via the shared
        // function evaluator.
        if let Some(lut) = self.build_transfer_lut(&smask_dict) {
            mask.apply_transfer_lut(&lut);
        }

        self.buf.set_smask(mask);
    }

    /// Build a 256-entry transfer LUT from an SMask `/TR` function, or `None`
    /// when /TR is absent, /Identity, or an unsupported function type. Supports
    /// Function Types 0, 2, 3, and 4 via the shared evaluator.
    fn build_transfer_lut(&self, smask_dict: &PdfDictionary) -> Option<[u8; 256]> {
        let tr = smask_dict.get("TR")?;
        // /Identity (a name) is the explicit no-op default.
        if let PdfObject::Name(name) = tr {
            if name == "Identity" {
                return None;
            }
        }
        let reader = self.engine.document().reader();
        // Probe the function once to confirm it evaluates; if not, skip.
        let probe = crate::render::shading::eval_function(tr, 0.5, reader);
        if probe.is_empty() {
            log::debug!("PageRenderer: SMask /TR is an unsupported function type; using identity");
            return None;
        }
        let mut lut = [0u8; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            let t = i as f64 / 255.0;
            let out = crate::render::shading::eval_function(tr, t, reader);
            let v = out.first().copied().unwrap_or(t).clamp(0.0, 1.0);
            *slot = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        Some(lut)
    }

    /// Decode and paint an inline image (BI/ID/EI). `params` are the `ID`
    /// operands (already normalized to full key names by the content parser);
    /// `data` is the raw bytes between `ID` and `EI`.
    fn paint_inline_image(&mut self, params: &[Operand], data: &[u8]) {
        let dict = inline_params_to_map(params);

        let width = dict_int(&dict, "Width").unwrap_or(0).max(0) as u32;
        let height = dict_int(&dict, "Height").unwrap_or(0).max(0) as u32;
        if width == 0 || height == 0 {
            return;
        }
        let is_mask = dict_bool(&dict, "ImageMask").unwrap_or(false);
        let bpc = if is_mask {
            1
        } else {
            dict_int(&dict, "BitsPerComponent")
                .unwrap_or(8)
                .clamp(1, 16) as u8
        };
        let color_space = dict_name(&dict, "ColorSpace").unwrap_or("DeviceGray");
        let filters: Vec<&str> = dict_filter_list(&dict);
        let interpolate = dict_bool(&dict, "Interpolate").unwrap_or(false);

        // Inline image masks are stencil masks: paint the current fill color
        // through the 1-bit mask. We currently decode them as a grayscale image
        // and paint that; full stencil-color application is a follow-up.
        let raw = match ImageDecoder::decode_inline(
            data,
            width,
            height,
            bpc,
            color_space,
            &filters,
            None,
        ) {
            Ok(raw) => raw,
            Err(err) => {
                log::warn!("PageRenderer: inline image decode failed: {}", err);
                return;
            }
        };

        let ctm = self.ctm();
        ImagePainter::paint_image_with_options(
            &mut self.buf,
            &raw,
            &ctm,
            &self.viewport,
            interpolate,
        );
    }

    fn handle_do(&mut self, name: &str) {
        let Some(&(obj_num, gen_num)) = self.resources.xobjects.get(name) else {
            log::warn!("PageRenderer: XObject '{}' not found in resources", name);
            return;
        };

        let reader = self.engine.document().reader();
        let obj = match reader.get_object(obj_num, gen_num) {
            Ok(obj) => obj,
            Err(err) => {
                log::warn!(
                    "PageRenderer: failed to resolve XObject '{}': {}",
                    name,
                    err
                );
                return;
            }
        };

        let PdfObject::Stream { dict, .. } = &obj else {
            log::warn!("PageRenderer: XObject '{}' is not a stream", name);
            return;
        };

        match dict.get_name("Subtype") {
            Some("Image") => self.handle_do_image(name, obj_num, gen_num, dict),
            Some("Form") => self.handle_do_form(name, obj_num, gen_num),
            Some(other) => log::debug!("PageRenderer: XObject subtype '{}' not handled", other),
            None => log::warn!("PageRenderer: XObject '{}' has no /Subtype", name),
        }
    }

    fn handle_do_image(&mut self, name: &str, obj_num: u32, gen_num: u16, dict: &PdfDictionary) {
        let image_ref = ImageReference {
            page_number: self.page_number,
            xobject_name: name.to_string(),
            object_number: obj_num,
            generation_number: gen_num,
            width: positive_u32(
                dict.get_integer("Width").or_else(|| dict.get_integer("W")),
                1,
            ),
            height: positive_u32(
                dict.get_integer("Height").or_else(|| dict.get_integer("H")),
                1,
            ),
            bits_per_component: dict
                .get_integer("BitsPerComponent")
                .or_else(|| dict.get_integer("BPC"))
                .unwrap_or(8)
                .clamp(0, 16) as u8,
            color_space: extract_color_space_name(dict),
            filter: extract_filter_names(dict),
            is_inline: false,
            is_mask: dict
                .get_bool("ImageMask")
                .or_else(|| dict.get_bool("IM"))
                .unwrap_or(false),
            is_smask: false,
            inline_data: None,
        };

        match ImageDecoder::decode(&image_ref, self.engine.document().reader()) {
            Ok(raw) => {
                let ctm = self.ctm();
                let smooth_jpx = image_ref.filter.iter().any(|filter| filter == "JPXDecode");
                if image_interpolate(dict) {
                    ImagePainter::paint_image_with_options(
                        &mut self.buf,
                        &raw,
                        &ctm,
                        &self.viewport,
                        true,
                    );
                } else if smooth_jpx {
                    ImagePainter::paint_image_with_jpx_compat(
                        &mut self.buf,
                        &raw,
                        &ctm,
                        &self.viewport,
                    );
                } else {
                    ImagePainter::paint_image(&mut self.buf, &raw, &ctm, &self.viewport);
                }
            }
            Err(err) => log::warn!("PageRenderer: image '{}' decode failed: {}", name, err),
        }
    }

    fn handle_do_form(&mut self, name: &str, obj_num: u32, gen_num: u16) {
        // Depth guard: prevent runaway recursion from malformed or cyclic PDFs.
        // TODO: track object numbers on the stack to catch a direct A->B->A
        // cycle immediately instead of after 8 levels.
        if self.form_depth >= 8 {
            log::warn!(
                "PageRenderer: Form XObject nesting depth limit (8) exceeded at '{}' (obj {})",
                name,
                obj_num
            );
            return;
        }

        let reader = self.engine.document().reader();

        // ── Step 1: Fetch the Form stream object ─────────────────────────────
        // get_object already decrypts; decode_stream then decompresses.
        let (form_dict, raw_bytes) = match reader.get_object(obj_num, gen_num) {
            Ok(PdfObject::Stream { dict, raw }) => (dict, raw),
            Ok(_) => {
                log::warn!("PageRenderer: Form XObject '{}' is not a stream", name);
                return;
            }
            Err(err) => {
                log::warn!(
                    "PageRenderer: failed to fetch Form XObject '{}': {}",
                    name,
                    err
                );
                return;
            }
        };

        if form_dict.get_name("Subtype") != Some("Form") {
            log::debug!(
                "PageRenderer: XObject '{}' is not /Subtype /Form, skipping",
                name
            );
            return;
        }

        if is_transparency_group(&form_dict) {
            let stream_obj = PdfObject::Stream {
                dict: form_dict.clone(),
                raw: raw_bytes.clone(),
            };
            let content_bytes = match crate::filters::decode_stream(&stream_obj, reader) {
                Ok(bytes) => bytes,
                Err(err) => {
                    log::warn!(
                        "PageRenderer: Form XObject '{}' stream decode failed: {}",
                        name,
                        err
                    );
                    return;
                }
            };
            self.handle_do_form_group(name, obj_num, gen_num, &form_dict, &content_bytes);
            return;
        }

        // A /Group that is NOT /S /Transparency (e.g. some other group subtype)
        // falls through to direct rendering, which is correct for the common
        // non-transparent case. /S /Transparency groups are handled above.
        if form_dict.get("Group").is_some() {
            log::debug!(
                "PageRenderer: Form XObject '{}' has a non-transparency /Group — rendering directly",
                name
            );
        }

        // ── Step 2: Extract Matrix and BBox ──────────────────────────────────
        let form_matrix = extract_form_matrix(&form_dict);
        let bbox = extract_bbox(&form_dict);

        // ── Step 3: Save graphics state, clip, and resources ─────────────────
        let saved_gs = self.gs.clone();
        self.clip_stack.push(self.buf.clip_mask().cloned());
        self.smask_stack.push(self.buf.smask_mask().cloned());
        let saved_resources = self.resources.clone();
        let saved_base_ctm = self.base_ctm;
        self.form_depth += 1;

        // ── Step 4: Apply the Form matrix to the CTM ─────────────────────────
        // The Form /Matrix maps Form space → the user space in effect at the Do.
        // concat(self, other) applies `self` first then `other`, so to apply the
        // form matrix before the current CTM we compute form_matrix.concat(ctm).
        let current_ctm = Transform2D::from(self.gs.ctm);
        let form_t = Transform2D::from(form_matrix);
        self.gs.ctm = form_t.concat(&current_ctm).to_array();
        // Patterns referenced inside this Form are relative to the Form's own
        // default coordinate system.
        self.base_ctm = Transform2D::from(self.gs.ctm);

        // ── Step 5: Clip to the BBox (intersected with any existing clip) ────
        if let Some(bb) = bbox {
            self.apply_form_bbox_clip(bb);
        }

        // ── Step 6: Merge the Form's own resources over the page resources ───
        if let Some(res_obj) = form_dict.get("Resources") {
            let form_res = crate::engine::parse_resources_from_obj(res_obj, reader);
            self.resources = merge_resources(form_res, &saved_resources);
        }
        // No /Resources: keep using the inherited (page) resources already set.

        // ── Step 7: Decode and parse the content stream ──────────────────────
        let stream_obj = PdfObject::Stream {
            dict: form_dict.clone(),
            raw: raw_bytes,
        };
        let content_bytes = match crate::filters::decode_stream(&stream_obj, reader) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::warn!(
                    "PageRenderer: Form XObject '{}' stream decode failed: {}",
                    name,
                    err
                );
                self.cleanup_after_form(saved_gs, saved_resources, saved_base_ctm);
                return;
            }
        };

        let ops = match crate::content::ContentParser::parse(&content_bytes) {
            Ok(ops) => ops,
            Err(err) => {
                log::warn!(
                    "PageRenderer: Form XObject '{}' content parse failed: {}",
                    name,
                    err
                );
                self.cleanup_after_form(saved_gs, saved_resources, saved_base_ctm);
                return;
            }
        };

        // ── Step 8: Render the Form's content stream ─────────────────────────
        self.dispatch_all(&ops);

        // ── Step 9: Restore the saved state ──────────────────────────────────
        self.cleanup_after_form(saved_gs, saved_resources, saved_base_ctm);
    }

    fn handle_do_form_group(
        &mut self,
        name: &str,
        _obj_num: u32,
        _gen_num: u16,
        form_dict: &PdfDictionary,
        content_bytes: &[u8],
    ) {
        let reader = self.engine.document().reader();

        // Group flags: /I (isolated) and /K (knockout), both default false.
        let (isolated, knockout) = match transparency_group_dict(form_dict) {
            Some(group) => (group_is_isolated(group), group_is_knockout(group)),
            None => (false, false),
        };
        if knockout {
            // Knockout (/K true): interior elements should knock out the group
            // backdrop rather than accumulate. We track the flag on the group
            // RenderState and apply knockout compositing at the group's
            // backdrop seam; per-element knockout among overlapping interior
            // elements is approximated as normal accumulation (rare in
            // practice — typically used for non-overlapping outline effects).
            log::debug!(
                "PageRenderer: knockout transparency group '{}' (interior overlap approximated)",
                name
            );
        }

        // An isolated group starts from a fully transparent backdrop. A
        // non-isolated group starts from a copy of the current backdrop (the
        // page/parent buffer so far), so blend modes inside the group can
        // interact with what is already painted. We remove that backdrop
        // contribution again before compositing the group result back, so the
        // backdrop is not counted twice (PDF 32000-1 §11.4.8).
        let render_mode = self.buf.render_mode();
        let mut group_buf = if isolated {
            PixelBuffer::new_transparent_with_mode(self.buf.width, self.buf.height, render_mode)
        } else {
            let mut copy = self.buf.clone();
            copy.clear_clip();
            copy.clear_smask();
            copy
        };
        group_buf.blend_mode = BlendMode::Normal;

        let form_matrix = extract_form_matrix(form_dict);
        let current_ctm = Transform2D::from(self.gs.ctm);
        let form_t = Transform2D::from(form_matrix);
        let mut group_gs = self.gs.clone();
        group_gs.ctm = form_t.concat(&current_ctm).to_array();
        // Inside the group, painting starts from a clean compositing state:
        // the group's own alpha/blend/soft-mask are applied when the *result*
        // is composited back, not to each interior element.
        group_gs.fill_alpha = 1.0;
        group_gs.stroke_alpha = 1.0;
        group_gs.blend_mode = BlendMode::Normal;
        let group_base_ctm = Transform2D::from(group_gs.ctm);

        let group_resources = if let Some(res_obj) = form_dict.get("Resources") {
            let form_res = crate::engine::parse_resources_from_obj(res_obj, reader);
            merge_resources(form_res, &self.resources)
        } else {
            self.resources.clone()
        };

        let mut group_state = RenderState {
            engine: self.engine,
            page_number: self.page_number,
            buf: group_buf,
            viewport: self.viewport.clone(),
            resources: group_resources,
            gs: group_gs,
            clip_stack: Vec::new(),
            smask_stack: Vec::new(),
            path: Path::new(),
            pending_clip: None,
            glyph_cache: GlyphCache::with_default_capacity(),
            form_depth: self.form_depth + 1,
            pending_inline: None,
            base_ctm: group_base_ctm,
            cancel: self.cancel.clone(),
        };

        // Carry the parent clip into the group so content is bounded the same
        // way direct rendering would be, then intersect the Form BBox.
        if let Some(clip) = self.buf.clip_mask().cloned() {
            group_state.buf.set_clip(clip);
        }
        if let Some(bbox) = extract_bbox(form_dict) {
            group_state.apply_form_bbox_clip(bbox);
        }

        let ops = match crate::content::ContentParser::parse(content_bytes) {
            Ok(ops) => ops,
            Err(err) => {
                log::warn!(
                    "PageRenderer: transparency Form XObject '{}' content parse failed: {}",
                    name,
                    err
                );
                return;
            }
        };
        group_state.dispatch_all(&ops);
        let mut group_buf = group_state.into_buffer();
        group_buf.clear_clip();

        // For a non-isolated group, subtract the backdrop we seeded it with so
        // it is not double-counted when we composite the group back.
        if !isolated {
            group_buf.remove_backdrop(&self.buf);
        }

        // Composite the finished group as a single unit, using the alpha /
        // blend mode / soft mask active at the point of the `Do` operator.
        let group_alpha = self.gs.fill_alpha as f32;
        let blend_mode = self.gs.blend_mode;
        let soft_mask = self.buf.smask_mask().cloned();
        self.buf
            .composite_from(&group_buf, group_alpha, blend_mode, soft_mask.as_ref());
    }

    /// Restore the graphics state, clip mask, and resources saved before a Form
    /// XObject was rendered, and decrement the depth counter.
    fn cleanup_after_form(
        &mut self,
        saved_gs: GraphicsState,
        saved_resources: PageResources,
        saved_base_ctm: Transform2D,
    ) {
        self.form_depth = self.form_depth.saturating_sub(1);
        self.resources = saved_resources;
        self.gs = saved_gs;
        self.base_ctm = saved_base_ctm;
        self.sync_blend_mode();
        match self.clip_stack.pop() {
            Some(saved) => self.buf.restore_clip(saved),
            None => log::warn!("PageRenderer: Form cleanup with empty clip stack"),
        }
        match self.smask_stack.pop() {
            Some(saved) => self.buf.restore_smask(saved),
            None => log::warn!("PageRenderer: Form cleanup with empty SMask stack"),
        }
    }

    fn render_page_annotations(&mut self) {
        let reader = self.engine.document().reader();
        let pages = match self.engine.document().get_pages() {
            Ok(pages) => pages,
            Err(err) => {
                log::debug!("PageRenderer: could not load pages for annotations: {err}");
                return;
            }
        };
        let Some(page) = pages.get(self.page_number.saturating_sub(1)) else {
            return;
        };
        let page_dict = match reader.get_and_resolve(page.object_number, page.generation_number) {
            Ok(PdfObject::Dictionary(dict)) => dict,
            Ok(_) => return,
            Err(err) => {
                log::debug!("PageRenderer: could not resolve page annotations: {err}");
                return;
            }
        };
        let Some(annots_obj) = page_dict.get("Annots").cloned() else {
            return;
        };
        let annots = match reader.resolve(annots_obj) {
            Ok(PdfObject::Array(items)) => items,
            _ => return,
        };

        for (index, annot_obj) in annots.into_iter().enumerate() {
            if self.cancel.is_cancelled() {
                return;
            }
            let annot = match reader.resolve(annot_obj) {
                Ok(PdfObject::Dictionary(dict)) => dict,
                _ => continue,
            };
            if annotation_is_hidden_or_no_view(&annot) {
                continue;
            }
            if annot.get_name("Subtype") != Some("Widget") {
                continue;
            }
            let Some(rect) = extract_rect(&annot) else {
                continue;
            };
            let Some((appearance_dict, appearance_raw)) =
                select_annotation_appearance(&annot, reader).or_else(|| {
                    synthesize_annotation_appearance(&annot, reader, self.engine, rect)
                })
            else {
                continue;
            };
            if appearance_dict.get_name("Subtype") != Some("Form") {
                continue;
            }
            self.render_annotation_appearance(
                &format!("Annot{}", index + 1),
                &appearance_dict,
                appearance_raw,
                rect,
            );
        }
    }

    fn render_annotation_appearance(
        &mut self,
        name: &str,
        form_dict: &PdfDictionary,
        raw_bytes: Vec<u8>,
        rect: [f64; 4],
    ) {
        if self.form_depth >= 8 {
            log::warn!(
                "PageRenderer: annotation appearance nesting depth limit (8) exceeded at '{}'",
                name
            );
            return;
        }

        let Some(bbox) = extract_bbox(form_dict) else {
            log::debug!(
                "PageRenderer: annotation appearance '{}' missing /BBox",
                name
            );
            return;
        };
        let Some(placement) = annotation_appearance_ctm(rect, bbox) else {
            return;
        };

        let saved_gs = self.gs.clone();
        let saved_clip = self.buf.clip_mask().cloned();
        let saved_smask = self.buf.smask_mask().cloned();
        self.buf.clear_clip();
        self.buf.clear_smask();
        self.gs = GraphicsState::default();
        self.gs.ctm = placement.to_array();
        self.sync_blend_mode();

        let stream_obj = PdfObject::Stream {
            dict: form_dict.clone(),
            raw: raw_bytes.clone(),
        };
        let reader = self.engine.document().reader();
        let content_bytes = match crate::filters::decode_stream(&stream_obj, reader) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::warn!(
                    "PageRenderer: annotation appearance '{}' stream decode failed: {}",
                    name,
                    err
                );
                self.gs = saved_gs;
                self.buf.restore_clip(saved_clip);
                self.buf.restore_smask(saved_smask);
                self.sync_blend_mode();
                return;
            }
        };

        if is_transparency_group(form_dict) {
            self.handle_do_form_group(name, 0, 0, form_dict, &content_bytes);
        } else {
            self.render_form_content_stream(name, form_dict, &content_bytes);
        }

        self.gs = saved_gs;
        self.buf.restore_clip(saved_clip);
        self.buf.restore_smask(saved_smask);
        self.sync_blend_mode();
    }

    fn render_form_content_stream(
        &mut self,
        name: &str,
        form_dict: &PdfDictionary,
        content_bytes: &[u8],
    ) {
        let reader = self.engine.document().reader();
        let form_matrix = extract_form_matrix(form_dict);
        let bbox = extract_bbox(form_dict);

        let saved_gs = self.gs.clone();
        self.clip_stack.push(self.buf.clip_mask().cloned());
        self.smask_stack.push(self.buf.smask_mask().cloned());
        let saved_resources = self.resources.clone();
        let saved_base_ctm = self.base_ctm;
        self.form_depth += 1;

        let current_ctm = Transform2D::from(self.gs.ctm);
        let form_t = Transform2D::from(form_matrix);
        self.gs.ctm = form_t.concat(&current_ctm).to_array();
        self.base_ctm = Transform2D::from(self.gs.ctm);

        if let Some(bb) = bbox {
            self.apply_form_bbox_clip(bb);
        }

        if let Some(res_obj) = form_dict.get("Resources") {
            let form_res = crate::engine::parse_resources_from_obj(res_obj, reader);
            self.resources = merge_resources(form_res, &saved_resources);
        }

        let ops = match crate::content::ContentParser::parse(content_bytes) {
            Ok(ops) => ops,
            Err(err) => {
                log::warn!(
                    "PageRenderer: Form XObject '{}' content parse failed: {}",
                    name,
                    err
                );
                self.cleanup_after_form(saved_gs, saved_resources, saved_base_ctm);
                return;
            }
        };

        self.dispatch_all(&ops);
        self.cleanup_after_form(saved_gs, saved_resources, saved_base_ctm);
    }

    /// Clip subsequent painting to the Form's BBox, transformed by the current
    /// CTM. `set_clip` intersects with any existing clip, so page-level clips
    /// are preserved.
    fn apply_form_bbox_clip(&mut self, bbox: [f64; 4]) {
        let x_min = bbox[0].min(bbox[2]);
        let y_min = bbox[1].min(bbox[3]);
        let width = (bbox[2] - bbox[0]).abs();
        let height = (bbox[3] - bbox[1]).abs();
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let mut bbox_path = Path::new();
        bbox_path.rect(x_min, y_min, width, height);
        let ctm = self.ctm();
        let flat = flatten_path(&bbox_path, &ctm, &self.viewport, 0.5);
        let clip = ClipMask::from_path(&flat, self.buf.width, self.buf.height, FillRule::NonZero);
        self.buf.set_clip(clip);
    }

    /// Handle the `sh` operator: paint the named shading over the current clip
    /// region (the entire page if no clip is set).
    fn handle_sh(&mut self, name: String) {
        let shading_obj = match self.resources.shadings.get(&name) {
            Some(obj) => obj.clone(),
            None => {
                log::warn!("sh: shading '{}' not found in resources", name);
                return;
            }
        };
        let reader = self.engine.document().reader();
        let Some(shading_dict) = resolve_to_dict(&shading_obj, reader) else {
            log::warn!("sh: shading '{}' did not resolve to a dictionary", name);
            return;
        };
        let ctm = self.ctm();
        let mesh_data = shading_mesh_data(&shading_obj, &shading_dict, reader);
        ShadingRenderer::paint(
            &shading_dict,
            &ctm,
            &self.viewport,
            &mut self.buf,
            reader,
            mesh_data.as_deref(),
        );
    }

    /// Paint a pattern fill for the current path. Dispatches on /PatternType.
    fn paint_pattern_fill(&mut self, rule: FillRule, pattern_name: &str) {
        let pattern_obj = match self.resources.patterns.get(pattern_name) {
            Some(obj) => obj.clone(),
            None => {
                log::warn!("pattern fill: pattern '{}' not found", pattern_name);
                return;
            }
        };
        let reader = self.engine.document().reader();
        let Some(pattern_dict) = resolve_to_dict(&pattern_obj, reader) else {
            log::warn!(
                "pattern fill: '{}' did not resolve to a dictionary",
                pattern_name
            );
            return;
        };

        match pattern_dict.get_integer("PatternType").unwrap_or(0) {
            1 => self.paint_tiling_pattern_fill(rule, &pattern_obj),
            2 => self.paint_shading_pattern_fill(rule, &pattern_dict),
            other => log::debug!("pattern fill: unknown PatternType {other}"),
        }
    }

    /// Paint a tiling pattern (PatternType 1) clipped to the current path.
    ///
    /// The tile content stream is rendered repeatedly across the path's
    /// device-space bounding box at `/XStep`/`/YStep` spacing, each repetition
    /// positioned via the pattern `/Matrix` (relative to the base CTM of the
    /// pattern's parent content stream) and clipped to the filled path.
    fn paint_tiling_pattern_fill(&mut self, rule: FillRule, pattern_obj: &PdfObject) {
        if self.form_depth >= 8 {
            log::warn!("tiling pattern: nesting depth limit reached; skipping");
            return;
        }
        let reader = self.engine.document().reader();
        let (pat_dict, raw_bytes) = match resolve_to_stream(pattern_obj, reader) {
            Some(pair) => pair,
            None => {
                log::warn!("tiling pattern: did not resolve to a content stream");
                return;
            }
        };

        let bbox = match get_float_array_dict(&pat_dict, "BBox") {
            Some(b) if b.len() >= 4 => [b[0], b[1], b[2], b[3]],
            _ => {
                log::warn!("tiling pattern: missing /BBox");
                return;
            }
        };
        let x_step = pat_dict
            .get("XStep")
            .and_then(PdfObject::as_number)
            .unwrap_or(0.0);
        let y_step = pat_dict
            .get("YStep")
            .and_then(PdfObject::as_number)
            .unwrap_or(0.0);
        if x_step.abs() < 1e-6 || y_step.abs() < 1e-6 {
            log::warn!("tiling pattern: zero XStep/YStep; skipping");
            return;
        }
        let paint_type = pat_dict.get_integer("PaintType").unwrap_or(1);

        // pattern space → device. The pattern /Matrix is relative to the base
        // CTM of the parent content stream (NOT the fill-time CTM).
        let pat_matrix = match get_float_array_dict(&pat_dict, "Matrix") {
            Some(m) if m.len() >= 6 => Transform2D::from([m[0], m[1], m[2], m[3], m[4], m[5]]),
            _ => Transform2D::identity(),
        };
        let pattern_ctm = pat_matrix.concat(&self.base_ctm);

        // Clip mask = the filled path, intersected with the existing clip.
        let path_ctm = self.ctm();
        let flat = flatten_path(&self.path, &path_ctm, &self.viewport, 0.5);
        let mut path_clip = ClipMask::from_path(&flat, self.buf.width, self.buf.height, rule);
        if let Some(existing) = self.buf.clip_mask() {
            path_clip.intersect(existing);
        }

        // Determine the device-space bounding box of the filled path to bound
        // how many tiles we need; then map that back into pattern space.
        let (dx0, dy0, dx1, dy1) = path_device_bounds(&flat, self.buf.width, self.buf.height);
        if dx1 < dx0 || dy1 < dy0 {
            return; // empty path
        }
        let full = pattern_ctm.concat(&self.viewport.to_transform());
        let inv = match full.inverse() {
            Some(inv) => inv,
            None => {
                log::warn!("tiling pattern: singular pattern transform");
                return;
            }
        };
        // Map the four device-bbox corners into pattern space.
        let corners = [
            inv.transform_point(dx0 as f64, dy0 as f64),
            inv.transform_point(dx1 as f64, dy0 as f64),
            inv.transform_point(dx0 as f64, dy1 as f64),
            inv.transform_point(dx1 as f64, dy1 as f64),
        ];
        let (mut pminx, mut pminy, mut pmaxx, mut pmaxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (px, py) in corners {
            pminx = pminx.min(px);
            pminy = pminy.min(py);
            pmaxx = pmaxx.max(px);
            pmaxy = pmaxy.max(py);
        }
        // Tile index range: which (i,j) translations of the tile overlap the
        // pattern-space region, accounting for the BBox extent vs the step.
        let i0 = ((pminx - bbox[2]) / x_step).floor() as i64;
        let i1 = ((pmaxx - bbox[0]) / x_step).ceil() as i64;
        let j0 = ((pminy - bbox[3]) / y_step).floor() as i64;
        let j1 = ((pmaxy - bbox[1]) / y_step).ceil() as i64;

        let tile_count = (i1 - i0 + 1).max(0) as i128 * (j1 - j0 + 1).max(0) as i128;
        const TILE_CAP: i128 = 20_000;
        if tile_count > TILE_CAP {
            log::warn!(
                "tiling pattern: {tile_count} tiles exceeds cap {TILE_CAP}; skipping (tile step \
                 too small for fill area)"
            );
            return;
        }
        if tile_count == 0 {
            return;
        }

        let content_bytes = {
            let stream_obj = PdfObject::Stream {
                dict: pat_dict.clone(),
                raw: raw_bytes,
            };
            match crate::filters::decode_stream(&stream_obj, reader) {
                Ok(bytes) => bytes,
                Err(err) => {
                    log::warn!("tiling pattern: content decode failed: {err}");
                    return;
                }
            }
        };
        let ops = match crate::content::ContentParser::parse(&content_bytes) {
            Ok(ops) => ops,
            Err(err) => {
                log::warn!("tiling pattern: content parse failed: {err}");
                return;
            }
        };

        let pat_resources = if let Some(res_obj) = pat_dict.get("Resources") {
            let pr = crate::engine::parse_resources_from_obj(res_obj, reader);
            merge_resources(pr, &self.resources)
        } else {
            self.resources.clone()
        };

        // For PaintType 2 (uncolored), the tile is painted in the current fill
        // color; the tile's own content stream must not set color. The fill
        // color space is the special Pattern space, so reconstruct the concrete
        // color from the numeric components recorded by `scn` (by component
        // count: 1 -> gray, 3 -> RGB, 4 -> CMYK).
        let forced_color = if paint_type == 2 {
            Some(uncolored_pattern_color(&self.gs.fill_color))
        } else {
            None
        };

        // Install the path clip, then render each tile (each clips additionally
        // to its own BBox). The path clip bounds the whole fill to the shape.
        let saved_clip = self.buf.clip_mask().cloned();
        self.buf.set_clip(path_clip);

        for j in j0..=j1 {
            for i in i0..=i1 {
                // Each tile replays the pattern's full content stream, so even
                // under the 20k-tile cap this loop can be expensive. Poll the
                // cancellation flag once per tile (cheap relative to a tile
                // render) so a pathological pattern stops promptly on timeout.
                if self.cancel.is_cancelled() {
                    self.buf.restore_clip(saved_clip);
                    return;
                }
                let translate =
                    Transform2D::new(1.0, 0.0, 0.0, 1.0, i as f64 * x_step, j as f64 * y_step);
                let tile_ctm = translate.concat(&pattern_ctm);
                self.render_pattern_tile(
                    &ops,
                    &pat_resources,
                    tile_ctm,
                    bbox,
                    forced_color.as_ref(),
                );
            }
        }

        self.buf.restore_clip(saved_clip);
    }

    /// Render a single tile of a tiling pattern at `tile_ctm`, clipped to the
    /// tile's BBox (the page-path clip is already installed on `self.buf`).
    fn render_pattern_tile(
        &mut self,
        ops: &[ContentOperation],
        resources: &PageResources,
        tile_ctm: Transform2D,
        bbox: [f64; 4],
        forced_color: Option<&(ColorSpace, crate::content::state::Color)>,
    ) {
        let saved_gs = self.gs.clone();
        let saved_resources = self.resources.clone();
        let saved_base_ctm = self.base_ctm;
        let saved_clip = self.buf.clip_mask().cloned();
        self.form_depth += 1;

        self.gs.ctm = tile_ctm.to_array();
        self.base_ctm = tile_ctm;
        self.resources = resources.clone();
        if let Some((space, color)) = forced_color {
            self.gs.fill_color_space = space.clone();
            self.gs.fill_color = color.clone();
            self.gs.stroke_color_space = space.clone();
            self.gs.stroke_color = color.clone();
        }

        // Intersect the tile BBox so tile content cannot bleed past one cell.
        let x_min = bbox[0].min(bbox[2]);
        let y_min = bbox[1].min(bbox[3]);
        let w = (bbox[2] - bbox[0]).abs();
        let h = (bbox[3] - bbox[1]).abs();
        if w > 0.0 && h > 0.0 {
            let mut bbox_path = Path::new();
            bbox_path.rect(x_min, y_min, w, h);
            let flat = flatten_path(&bbox_path, &self.ctm(), &self.viewport, 0.5);
            let bbox_clip =
                ClipMask::from_path(&flat, self.buf.width, self.buf.height, FillRule::NonZero);
            self.buf.set_clip(bbox_clip); // intersects with the installed path clip
        }

        self.dispatch_all(ops);

        // Restore.
        self.form_depth = self.form_depth.saturating_sub(1);
        self.gs = saved_gs;
        self.resources = saved_resources;
        self.base_ctm = saved_base_ctm;
        self.buf.restore_clip(saved_clip);
        self.sync_blend_mode();
    }

    /// Paint a shading pattern (PatternType 2) clipped to the current path.
    fn paint_shading_pattern_fill(&mut self, rule: FillRule, pattern_dict: &PdfDictionary) {
        let reader = self.engine.document().reader();
        let shading_obj = match pattern_dict.get("Shading") {
            Some(obj) => obj.clone(),
            None => {
                log::warn!("shading pattern: missing /Shading entry");
                return;
            }
        };
        let Some(shading_dict) = resolve_to_dict(&shading_obj, reader) else {
            log::warn!("shading pattern: /Shading did not resolve to a dictionary");
            return;
        };

        // The pattern carries its own /Matrix (pattern space → the default user
        // coordinate system of the pattern's parent content stream). Per PDF
        // 32000-1 §8.7.3.1 the pattern matrix is relative to that *base* CTM, not
        // the CTM in effect at the moment of the fill, so combine it with
        // `base_ctm` (matching the tiling-pattern path).
        let ctm = match get_float_array_dict(pattern_dict, "Matrix") {
            Some(m) if m.len() >= 6 => {
                let pat = Transform2D::from([m[0], m[1], m[2], m[3], m[4], m[5]]);
                pat.concat(&self.base_ctm)
            }
            _ => self.base_ctm,
        };

        // Clip to the path being filled, intersected with the existing clip.
        let path_ctm = self.ctm();
        let flat = flatten_path(&self.path, &path_ctm, &self.viewport, 0.5);
        let path_clip = ClipMask::from_path(&flat, self.buf.width, self.buf.height, rule);
        let saved_clip = self.buf.clip_mask().cloned();
        self.buf.set_clip(path_clip); // intersects with any existing clip

        let mesh_data = shading_mesh_data(&shading_obj, &shading_dict, reader);
        ShadingRenderer::paint(
            &shading_dict,
            &ctm,
            &self.viewport,
            &mut self.buf,
            reader,
            mesh_data.as_deref(),
        );

        // Restore the exact previous clip (restore_clip sets directly).
        self.buf.restore_clip(saved_clip);
    }

    fn render_text_array(&mut self, op: &ContentOperation) {
        let Some(items) = op.operand(0).and_then(Operand::as_array) else {
            return;
        };
        for item in items {
            match item {
                Operand::String(bytes) => self.render_text_string(bytes),
                Operand::Integer(value) => self.adjust_text_position(-(*value as f64)),
                Operand::Real(value) => self.adjust_text_position(-*value),
                _ => {}
            }
        }
    }

    fn render_text_string(&mut self, bytes: &[u8]) {
        let font_name = self.gs.text.font_name.clone();
        let font_size = self.gs.text.font_size;
        if font_size <= 0.0 {
            return;
        }
        let decoded = self.decode_text_bytes(bytes, &font_name);
        let font_bytes = self.get_font_bytes(&font_name);
        let variation = self.font_variation_request(&font_name);
        let font_hash = font_bytes
            .as_ref()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| GlyphCache::hash_font_bytes(bytes));
        let upem = font_bytes
            .as_ref()
            .and_then(|bytes| Self::get_upem(bytes))
            .map(f64::from)
            .filter(|value| *value > 0.0)
            .unwrap_or(1000.0);

        if let (Some(font_bytes), Some(font_hash)) = (font_bytes.as_ref(), font_hash) {
            if let Some(text) = decoded_text_for_shaping(&decoded) {
                if let Some(shaped) = crate::render::shaping::shape_run(font_bytes, &text, upem) {
                    self.render_shaped_glyphs(&shaped, font_bytes, font_hash, &variation, upem);
                    return;
                }
            }
        }

        for glyph in decoded {
            let mut ttf_advance = None;
            if !matches!(self.gs.text.rendering_mode, 3 | 7) {
                if let (Some(font_bytes), Some(font_hash)) = (font_bytes.as_ref(), font_hash) {
                    if !font_bytes.is_empty() {
                        ttf_advance = self.render_glyph_with_cache(GlyphRenderRequest {
                            code: glyph.code,
                            ch: glyph.unicode,
                            glyph_name: glyph.glyph_name.as_deref(),
                            is_gid: glyph.is_gid,
                            font_bytes,
                            font_hash,
                            variation: &variation,
                            upem,
                            offset_x: 0.0,
                            offset_y: 0.0,
                        });
                    }
                }
            }
            let advance = glyph.width.or(ttf_advance).unwrap_or(500.0);
            self.advance_text(advance, glyph.is_space);
        }
    }

    fn render_shaped_glyphs(
        &mut self,
        glyphs: &[crate::render::shaping::ShapedGlyph],
        font_bytes: &[u8],
        font_hash: u64,
        variation: &VariationRequest,
        upem: f64,
    ) {
        for glyph in glyphs {
            if !matches!(self.gs.text.rendering_mode, 3 | 7) {
                let _ = self.render_glyph_with_cache(GlyphRenderRequest {
                    code: glyph.gid,
                    ch: '\u{FFFD}',
                    glyph_name: None,
                    is_gid: true,
                    font_bytes,
                    font_hash,
                    variation,
                    upem,
                    offset_x: glyph.offset_x,
                    offset_y: glyph.offset_y,
                });
            }
            self.advance_text(glyph.advance, false);
        }
    }

    fn render_glyph_with_cache(&mut self, request: GlyphRenderRequest<'_>) -> Option<f64> {
        let cache_key = GlyphCacheKey {
            font_hash: request.font_hash,
            variation_hash: request.variation.cache_hash(),
            code: request.code,
            is_gid: request.is_gid,
        };
        let cached = self.glyph_cache.get(&cache_key).cloned();
        let cached = match cached {
            Some(cached) => cached,
            None => {
                let (path, advance_width) = if request.is_gid {
                    crate::render::glyph_outline::extract_glyph_path_by_gid_var(
                        request.font_bytes,
                        request.code,
                        request.variation,
                    )
                } else {
                    crate::render::glyph_outline::extract_glyph_path_for_simple_var(
                        request.font_bytes,
                        request.code,
                        request.ch,
                        request.glyph_name,
                        request.variation,
                    )
                };
                let cached = CachedGlyph {
                    path,
                    advance_width,
                };
                self.glyph_cache.insert(cache_key, cached.clone());
                cached
            }
        };

        let advance_width = cached.advance_width;
        let Some(glyph_path) = cached.path else {
            return Some(advance_width);
        };

        let scale = font_size_scale(self.gs.text.font_size, request.upem);
        let th = self.gs.text.horizontal_scaling / 100.0;
        let scale_x = scale * th;
        if scale <= 0.0 || !scale_x.is_finite() {
            return Some(advance_width);
        }

        let scale_t = Transform2D::scale(scale_x, scale);
        let offset_t = Transform2D::translation(
            request.offset_x / 1000.0 * self.gs.text.font_size * th,
            request.offset_y / 1000.0 * self.gs.text.font_size,
        );
        let rise_t = Transform2D::translation(0.0, self.gs.text.rise);
        let tm_t = Transform2D::from(self.gs.text.tm);
        let ctm = self.ctm();
        let glyph_ctm = scale_t
            .concat(&offset_t)
            .concat(&rise_t)
            .concat(&tm_t)
            .concat(&ctm);
        let fill_color = self.fill_pixel_color();
        let stroke_color = self.stroke_pixel_color();

        // Stem/baseline grid-fitting is intentionally not enabled by default:
        // the first R&D pass regressed Tracemonkey vs Poppler. Keep glyph
        // coverage/tighter flattening active and revisit hinting with fixtures.
        let glyph_hinting = GlyphHinting::disabled();

        match self.gs.text.rendering_mode {
            0 | 4 => PathPainter::fill_glyph(
                &mut self.buf,
                &glyph_path,
                &glyph_ctm,
                &self.viewport,
                fill_color,
                FillRule::NonZero,
                glyph_hinting,
            ),
            1 | 5 => PathPainter::stroke(
                &mut self.buf,
                &glyph_path,
                &glyph_ctm,
                &self.viewport,
                stroke_color,
                self.gs.line_width,
                &DashState::solid(),
            ),
            2 | 6 => {
                PathPainter::fill_glyph(
                    &mut self.buf,
                    &glyph_path,
                    &glyph_ctm,
                    &self.viewport,
                    fill_color,
                    FillRule::NonZero,
                    glyph_hinting,
                );
                PathPainter::stroke(
                    &mut self.buf,
                    &glyph_path,
                    &glyph_ctm,
                    &self.viewport,
                    stroke_color,
                    self.gs.line_width,
                    &DashState::solid(),
                );
            }
            3 | 7 => {}
            other => log::warn!("PageRenderer: unknown text render mode {}", other),
        }
        Some(advance_width)
    }

    #[cfg(test)]
    fn extract_glyph_path(font_bytes: &[u8], ch: char) -> (Option<Path>, f64) {
        crate::render::glyph_outline::extract_glyph_path_for_simple(
            font_bytes,
            glyph_cache_code(ch),
            ch,
            None,
        )
    }

    fn get_upem(font_bytes: &[u8]) -> Option<u16> {
        if let Ok(face) = ttf_parser::Face::parse(font_bytes, 0) {
            return Some(face.units_per_em());
        }
        // Bare CFF reports a 1000-unit em (FontMatrix 0.001 convention).
        if crate::render::font_rasterizer::cff_support::is_bare_cff(font_bytes) {
            return Some(crate::render::font_rasterizer::cff_support::units_per_em() as u16);
        }
        if crate::fonts::type1::Type1Font::is_type1(font_bytes) {
            return Some(crate::fonts::type1::units_per_em() as u16);
        }
        None
    }

    fn decode_text_bytes(&self, bytes: &[u8], font_name: &str) -> Vec<DecodedGlyph> {
        let Some(font_dict) = self.resources.fonts.get(font_name) else {
            return latin1_glyphs(bytes);
        };
        let reader = self.engine.document().reader();
        if detect_font_subtype(font_dict) == FontSubtype::Type0 {
            return self.decode_type0_text(bytes, font_dict, reader);
        }

        let resolver = FontResolver::new(font_dict, reader);
        let has_explicit_widths = font_dict.get_array("Widths").is_some();
        let mut glyphs = Vec::new();
        let code_size = resolver.code_size().max(1);
        let mut idx = 0usize;
        while idx < bytes.len() {
            let code = if code_size == 2 {
                let high = bytes[idx];
                let low = bytes.get(idx + 1).copied().unwrap_or(0);
                idx = idx.saturating_add(2);
                (u16::from(high) << 8) | u16::from(low)
            } else {
                let code = u16::from(bytes[idx]);
                idx = idx.saturating_add(1);
                code
            };
            let text = resolver.decode_char(code);
            let ch = text.chars().next().unwrap_or('\u{FFFD}');
            let glyph_name = resolver.glyph_name(code).map(str::to_string);
            let width = if has_explicit_widths {
                let width = resolver.glyph_width(code).max(0.0);
                if width > 0.0 {
                    Some(width)
                } else {
                    None
                }
            } else {
                None
            };
            glyphs.push(DecodedGlyph {
                code,
                unicode: ch,
                glyph_name,
                is_space: resolver.is_space_code(code) || ch == ' ',
                width,
                is_gid: false,
            });
        }
        glyphs
    }

    fn decode_type0_text(
        &self,
        bytes: &[u8],
        font_dict: &PdfDictionary,
        reader: &crate::reader::PdfReader,
    ) -> Vec<DecodedGlyph> {
        let descendant_font = get_descendant_font(font_dict, reader);
        let resolver = FontResolver::new(font_dict, reader);
        let render_as_gid = cid_font_has_embedded_program(descendant_font.as_ref(), reader);
        let mut glyphs = Vec::new();
        let mut idx = 0usize;
        let code_size = resolver.code_size().max(1);

        while idx < bytes.len() {
            let cid = if code_size == 2 {
                let high = bytes[idx];
                let low = bytes.get(idx + 1).copied().unwrap_or(0);
                idx = idx.saturating_add(2);
                (u16::from(high) << 8) | u16::from(low)
            } else {
                let code = u16::from(bytes[idx]);
                idx = idx.saturating_add(1);
                code
            };

            let text = resolver.decode_char(cid);
            let unicode = text.chars().next().unwrap_or('\u{FFFD}');
            let width = if render_as_gid {
                Some(resolver.glyph_width(cid)).filter(|width| *width > 0.0)
            } else {
                None
            };
            let code = if render_as_gid {
                cid_to_gid(cid, descendant_font.as_ref(), reader)
            } else {
                u16::try_from(unicode as u32).unwrap_or(cid)
            };

            glyphs.push(DecodedGlyph {
                code,
                unicode,
                glyph_name: None,
                is_space: resolver.is_space_code(cid) || unicode == ' ',
                width,
                is_gid: render_as_gid,
            });
        }

        glyphs
    }

    /// Build the variable-font [`VariationRequest`] for a font resource from its
    /// `FontDescriptor` (`/FontWeight` → `wght`, `/FontStretch` → `wdth`). Returns
    /// the empty request (default instance) when there is no descriptor or no
    /// non-normal weight/stretch — so static fonts and default-instance variable
    /// fonts keep the byte-identical pre-variation cache key and outline.
    fn font_variation_request(&self, font_name: &str) -> VariationRequest {
        let reader = self.engine.document().reader();
        let Some(font_dict) = self.resources.fonts.get(font_name) else {
            return VariationRequest::none();
        };
        // For Type0 fonts the descriptor lives on the descendant CIDFont.
        let descriptor = if detect_font_subtype(font_dict) == FontSubtype::Type0 {
            get_descendant_font(font_dict, reader)
                .and_then(|d| resolve_descriptor(&d, reader))
        } else {
            resolve_descriptor(font_dict, reader)
        };
        let Some(descriptor) = descriptor else {
            return VariationRequest::none();
        };
        let weight = descriptor.get("FontWeight").and_then(PdfObject::as_number);
        let stretch = descriptor.get_name("FontStretch");
        VariationRequest::from_descriptor(weight, stretch)
    }

    fn get_font_bytes(&self, font_name: &str) -> Option<Vec<u8>> {
        let reader = self.engine.document().reader();
        if let Some(font_dict) = self.resources.fonts.get(font_name) {
            if let Some(bytes) = FontRasterizer::extract_font_bytes(font_dict, reader) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
            if detect_font_subtype(font_dict) == FontSubtype::Type0 {
                if let Some(descendant_font) = get_descendant_font(font_dict, reader) {
                    if let Some(bytes) =
                        FontRasterizer::extract_font_bytes(&descendant_font, reader)
                    {
                        if !bytes.is_empty() {
                            return Some(bytes);
                        }
                    }
                }
            }
        }
        get_fallback_font(font_name).map(|bytes| bytes.to_vec())
    }

    fn advance_text(&mut self, glyph_width: f64, is_space: bool) {
        let th = self.gs.text.horizontal_scaling / 100.0;
        let mut advance =
            (glyph_width / 1000.0) * self.gs.text.font_size * th + self.gs.text.char_spacing * th;
        if is_space {
            advance += self.gs.text.word_spacing * th;
        }
        self.translate_text_matrix(advance, 0.0);
    }

    fn adjust_text_position(&mut self, adjustment: f64) {
        let tx = adjustment / 1000.0
            * self.gs.text.font_size
            * (self.gs.text.horizontal_scaling / 100.0);
        self.translate_text_matrix(tx, 0.0);
    }

    fn move_to_next_text_line(&mut self) {
        let op = ContentOperation::new("T*", Vec::new());
        self.gs.process(&op);
    }

    fn translate_text_matrix(&mut self, tx: f64, ty: f64) {
        let mut tm = self.gs.text.tm;
        tm[4] += tm[0] * tx + tm[2] * ty;
        tm[5] += tm[1] * tx + tm[3] * ty;
        self.gs.text.tm = tm;
    }
}

struct DecodedGlyph {
    code: u16,
    unicode: char,
    glyph_name: Option<String>,
    is_space: bool,
    width: Option<f64>,
    is_gid: bool,
}

struct GlyphRenderRequest<'a> {
    code: u16,
    ch: char,
    glyph_name: Option<&'a str>,
    is_gid: bool,
    font_bytes: &'a [u8],
    font_hash: u64,
    /// The variable-font instance to render (empty for static / default).
    variation: &'a VariationRequest,
    upem: f64,
    offset_x: f64,
    offset_y: f64,
}

#[cfg(test)]
fn glyph_cache_code(ch: char) -> u16 {
    u16::try_from(ch as u32).unwrap_or(0xFFFD)
}

/// Resolve a font dict's `/FontDescriptor` (which may be an indirect reference)
/// to its dictionary.
fn resolve_descriptor(
    font_dict: &PdfDictionary,
    reader: &crate::reader::PdfReader,
) -> Option<PdfDictionary> {
    match reader.resolve(font_dict.get("FontDescriptor")?.clone()).ok()? {
        PdfObject::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

fn latin1_glyphs(bytes: &[u8]) -> Vec<DecodedGlyph> {
    bytes
        .iter()
        .map(|byte| DecodedGlyph {
            code: u16::from(*byte),
            unicode: decode_win_ansi(*byte),
            glyph_name: None,
            is_space: *byte == b' ',
            width: None,
            is_gid: false,
        })
        .collect()
}

fn decoded_text_for_shaping(glyphs: &[DecodedGlyph]) -> Option<String> {
    let mut text = String::new();
    for glyph in glyphs {
        if glyph.is_gid || glyph.unicode == '\u{FFFD}' {
            return None;
        }
        text.push(glyph.unicode);
    }
    crate::render::shaping::needs_shaping(&text).then_some(text)
}

fn decode_win_ansi(byte: u8) -> char {
    match byte {
        0x80 => '€',
        0x82 => '‚',
        0x83 => 'ƒ',
        0x84 => '„',
        0x85 => '…',
        0x86 => '†',
        0x87 => '‡',
        0x88 => 'ˆ',
        0x89 => '‰',
        0x8A => 'Š',
        0x8B => '‹',
        0x8C => 'Œ',
        0x8E => 'Ž',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '•',
        0x96 => '–',
        0x97 => '—',
        0x98 => '˜',
        0x99 => '™',
        0x9A => 'š',
        0x9B => '›',
        0x9C => 'œ',
        0x9E => 'ž',
        0x9F => 'Ÿ',
        b => char::from_u32(u32::from(b)).unwrap_or('\u{FFFD}'),
    }
}

fn positive_u32(value: Option<i64>, default: u32) -> u32 {
    value
        .filter(|number| *number > 0)
        .and_then(|number| u32::try_from(number).ok())
        .unwrap_or(default)
}

fn extract_color_space_name(dict: &PdfDictionary) -> String {
    match dict.get("ColorSpace").or_else(|| dict.get("CS")) {
        Some(PdfObject::Name(name)) => match name.as_str() {
            "G" => "DeviceGray".to_string(),
            "RGB" => "DeviceRGB".to_string(),
            "CMYK" => "DeviceCMYK".to_string(),
            other => other.to_string(),
        },
        Some(PdfObject::Array(items)) => items
            .first()
            .and_then(PdfObject::as_name)
            .unwrap_or("DeviceRGB")
            .to_string(),
        _ => "DeviceRGB".to_string(),
    }
}

fn extract_filter_names(dict: &PdfDictionary) -> Vec<String> {
    match dict.get("Filter").or_else(|| dict.get("F")) {
        Some(PdfObject::Name(name)) => vec![name.clone()],
        Some(PdfObject::Array(items)) => items
            .iter()
            .filter_map(PdfObject::as_name)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn image_interpolate(dict: &PdfDictionary) -> bool {
    dict.get_bool("Interpolate")
        .or_else(|| dict.get_bool("I"))
        .unwrap_or(false)
}

fn font_size_scale(font_size: f64, upem: f64) -> f64 {
    if font_size <= 0.0 || upem <= 0.0 || !font_size.is_finite() || !upem.is_finite() {
        0.0
    } else {
        font_size / upem
    }
}

fn uses_top_level_transparency(
    ops: &[ContentOperation],
    resources: &PageResources,
    engine: &ContentEngine,
) -> bool {
    for op in ops {
        match op.operator.as_str() {
            "gs" => {
                if let Some(name) = op.name(0) {
                    if resources
                        .ext_g_states
                        .get(name)
                        .is_some_and(ext_g_state_needs_transparent_backdrop)
                    {
                        return true;
                    }
                }
            }
            "Do" => {
                if let Some(name) = op.name(0) {
                    if xobject_needs_transparent_backdrop(name, resources, engine) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn ext_g_state_needs_transparent_backdrop(dict: &PdfDictionary) -> bool {
    let alpha_changed = ["ca", "CA"].iter().any(|key| {
        dict.get(key)
            .and_then(PdfObject::as_number)
            .is_some_and(|v| v < 0.999)
    });
    if alpha_changed {
        return true;
    }

    let blend_changed = match dict.get("BM") {
        Some(PdfObject::Name(name)) => name != "Normal" && name != "Compatible",
        Some(PdfObject::Array(items)) => items
            .iter()
            .filter_map(PdfObject::as_name)
            .any(|name| name != "Normal" && name != "Compatible"),
        _ => false,
    };
    if blend_changed {
        return true;
    }

    match dict.get("SMask") {
        Some(PdfObject::Name(name)) if name == "None" => false,
        Some(_) => true,
        None => false,
    }
}

fn xobject_needs_transparent_backdrop(
    name: &str,
    resources: &PageResources,
    engine: &ContentEngine,
) -> bool {
    let Some(&(obj_num, gen_num)) = resources.xobjects.get(name) else {
        return false;
    };
    match engine.document().reader().get_object(obj_num, gen_num) {
        Ok(PdfObject::Stream { dict, .. }) => is_transparency_group(&dict),
        _ => false,
    }
}

fn is_transparency_group(form_dict: &PdfDictionary) -> bool {
    matches!(
        form_dict.get("Group"),
        Some(PdfObject::Dictionary(group)) if group.get_name("S") == Some("Transparency")
    )
}

/// Collect inline image `ID` operands (alternating key/value) into a map.
/// Keys arrive already normalized to full names by the content parser.
fn inline_params_to_map(operands: &[Operand]) -> std::collections::HashMap<String, Operand> {
    let mut map = std::collections::HashMap::new();
    let mut iter = operands.iter();
    while let Some(key_op) = iter.next() {
        if let Operand::Name(key) = key_op {
            if let Some(value) = iter.next() {
                map.insert(key.clone(), value.clone());
            }
        }
    }
    map
}

fn dict_int(map: &std::collections::HashMap<String, Operand>, key: &str) -> Option<i64> {
    match map.get(key)? {
        Operand::Integer(n) => Some(*n),
        Operand::Real(r) => Some(*r as i64),
        _ => None,
    }
}

fn dict_bool(map: &std::collections::HashMap<String, Operand>, key: &str) -> Option<bool> {
    match map.get(key)? {
        Operand::Boolean(b) => Some(*b),
        _ => None,
    }
}

fn dict_name<'a>(
    map: &'a std::collections::HashMap<String, Operand>,
    key: &str,
) -> Option<&'a str> {
    match map.get(key)? {
        Operand::Name(n) => Some(n.as_str()),
        _ => None,
    }
}

/// Extract the inline image filter chain (`/Filter`), accepting a single name
/// or a name array. Returns filter names verbatim (full forms after parser
/// normalization); `decode_inline` understands them.
fn dict_filter_list(map: &std::collections::HashMap<String, Operand>) -> Vec<&str> {
    match map.get("Filter") {
        Some(Operand::Name(n)) => vec![n.as_str()],
        Some(Operand::Array(items)) => items.iter().filter_map(Operand::as_name).collect(),
        _ => Vec::new(),
    }
}

/// Determine the opaque backdrop color for a luminosity soft mask.
///
/// Defaults to black `[0,0,0,255]` (the spec default, which yields mask=0 in
/// unpainted areas). An explicit `/BC` array overrides it, interpreted in the
/// mask group's color space (`/Group /CS`) by component count: 1 → gray, 3 →
/// RGB, 4 → CMYK. The result is always opaque (alpha 255) so luminosity is
/// well-defined everywhere.
fn smask_backdrop_color(smask_dict: &PdfDictionary, g_dict: &PdfDictionary) -> PixelColor {
    let Some(bc) = smask_dict
        .get("BC")
        .and_then(PdfObject::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(PdfObject::as_number)
                .collect::<Vec<f64>>()
        })
    else {
        return [0, 0, 0, 255];
    };
    if bc.is_empty() {
        return [0, 0, 0, 255];
    }

    // Try to honor the group color space's channel count; fall back to the
    // number of /BC components.
    let space_name = g_dict
        .get("Group")
        .and_then(PdfObject::as_dict)
        .and_then(|g| g.get("CS"))
        .and_then(PdfObject::as_name)
        .map(str::to_string);

    let rc = match space_name.as_deref() {
        Some(name) => ColorSpaceHandler::from_components(name, &bc, 1.0),
        None => match bc.len() {
            1 => ColorSpaceHandler::from_components("DeviceGray", &bc, 1.0),
            4 => ColorSpaceHandler::from_components("DeviceCMYK", &bc, 1.0),
            _ => ColorSpaceHandler::from_components("DeviceRGB", &bc, 1.0),
        },
    };
    let p = rc.to_pixel_color();
    [p[0], p[1], p[2], 255]
}

/// The `/Group` sub-dictionary of a Form XObject, if it is a transparency
/// group. Returns `None` for non-group Forms.
fn transparency_group_dict(form_dict: &PdfDictionary) -> Option<&PdfDictionary> {
    match form_dict.get("Group") {
        Some(PdfObject::Dictionary(group)) if group.get_name("S") == Some("Transparency") => {
            Some(group)
        }
        _ => None,
    }
}

/// Read the `/I` (isolated) flag of a transparency group dictionary
/// (default false).
fn group_is_isolated(group: &PdfDictionary) -> bool {
    group.get_bool("I").unwrap_or(false)
}

/// Read the `/K` (knockout) flag of a transparency group dictionary
/// (default false).
fn group_is_knockout(group: &PdfDictionary) -> bool {
    group.get_bool("K").unwrap_or(false)
}

/// Resolve a shading/pattern object (direct dict, stream, or indirect
/// reference) to its dictionary. Returns `None` for anything else.
fn resolve_to_dict(obj: &PdfObject, reader: &crate::reader::PdfReader) -> Option<PdfDictionary> {
    match obj {
        PdfObject::Dictionary(d) => Some(d.clone()),
        PdfObject::Stream { dict, .. } => Some(dict.clone()),
        PdfObject::Reference { number, generation } => {
            match reader.get_object(*number, *generation).ok()? {
                PdfObject::Dictionary(d) => Some(d),
                PdfObject::Stream { dict, .. } => Some(dict),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Reconstruct the concrete fill color for an uncolored (PaintType 2) tiling
/// pattern from the components recorded by `scn`. The fill color space is the
/// abstract Pattern space, so the concrete base space is inferred from the
/// number of numeric components.
fn uncolored_pattern_color(
    fill_color: &crate::content::state::Color,
) -> (ColorSpace, crate::content::state::Color) {
    let comps = fill_color.components.clone();
    let space = match comps.len() {
        1 => ColorSpace::DeviceGray,
        4 => ColorSpace::DeviceCMYK,
        _ => ColorSpace::DeviceRGB,
    };
    let color = crate::content::state::Color {
        space: space.clone(),
        components: comps,
    };
    (space, color)
}

/// For a mesh shading (ShadingType 4–7), decode and return the shading stream's
/// data (the packed vertex/patch records). Returns `None` for dictionary-only
/// shadings (Types 1–3) or if the object is not a stream.
fn shading_mesh_data(
    shading_obj: &PdfObject,
    shading_dict: &PdfDictionary,
    reader: &crate::reader::PdfReader,
) -> Option<Vec<u8>> {
    let st = shading_dict.get_integer("ShadingType").unwrap_or(0);
    if !(4..=7).contains(&st) {
        return None;
    }
    let (dict, raw) = resolve_to_stream(shading_obj, reader)?;
    let stream_obj = PdfObject::Stream { dict, raw };
    crate::filters::decode_stream(&stream_obj, reader).ok()
}

/// Resolve a pattern/function object to its (dictionary, raw stream bytes).
/// Returns `None` if it is not a stream.
fn resolve_to_stream(
    obj: &PdfObject,
    reader: &crate::reader::PdfReader,
) -> Option<(PdfDictionary, Vec<u8>)> {
    match obj {
        PdfObject::Stream { dict, raw } => Some((dict.clone(), raw.clone())),
        PdfObject::Reference { number, generation } => {
            match reader.get_object(*number, *generation).ok()? {
                PdfObject::Stream { dict, raw } => Some((dict, raw)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Compute the integer device-space bounding box of a flattened path, clamped to
/// the buffer. Returns `(x0, y0, x1, y1)` inclusive; an empty/degenerate path
/// yields `x1 < x0`.
fn path_device_bounds(flat: &FlatPath, width: u32, height: u32) -> (i32, i32, i32, i32) {
    let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for sub in &flat.subpaths {
        for &(x, y) in sub {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
    }
    if minx > maxx || miny > maxy {
        return (1, 1, 0, 0); // empty
    }
    let x0 = minx.floor().max(0.0) as i32;
    let y0 = miny.floor().max(0.0) as i32;
    let x1 = (maxx.ceil() as i32).min(width as i32 - 1);
    let y1 = (maxy.ceil() as i32).min(height as i32 - 1);
    (x0, y0, x1, y1)
}

/// Read a numeric array entry from a dictionary, e.g. a pattern `/Matrix`.
fn get_float_array_dict(dict: &PdfDictionary, key: &str) -> Option<Vec<f64>> {
    let arr = dict.get(key)?.as_array()?;
    let vals: Vec<f64> = arr.iter().filter_map(PdfObject::as_number).collect();
    if vals.is_empty() {
        None
    } else {
        Some(vals)
    }
}

fn annotation_is_hidden_or_no_view(dict: &PdfDictionary) -> bool {
    let flags = dict.get_integer("F").unwrap_or(0);
    const INVISIBLE: i64 = 1 << 0;
    const HIDDEN: i64 = 1 << 1;
    const NO_VIEW: i64 = 1 << 5;
    flags & (INVISIBLE | HIDDEN | NO_VIEW) != 0
}

fn select_annotation_appearance(
    annot: &PdfDictionary,
    reader: &crate::reader::PdfReader,
) -> Option<(PdfDictionary, Vec<u8>)> {
    let ap = annot.get("AP")?.clone();
    let ap = match reader.resolve(ap).ok()? {
        PdfObject::Dictionary(dict) => dict,
        _ => return None,
    };
    let normal = ap.get("N")?.clone();
    match reader.resolve(normal).ok()? {
        PdfObject::Stream { dict, raw } => Some((dict, raw)),
        PdfObject::Dictionary(states) => {
            let state_name = annot.get_name("AS").unwrap_or("Off");
            if let Some(selected) = states.get(state_name) {
                return resolve_appearance_stream(selected, reader);
            }
            if state_name != "Off" {
                if let Some(off) = states.get("Off") {
                    return resolve_appearance_stream(off, reader);
                }
            }
            states
                .entries()
                .find(|(name, _)| name.as_str() != "Off")
                .and_then(|(_, value)| resolve_appearance_stream(value, reader))
        }
        _ => None,
    }
}

const FIELD_FLAG_MULTILINE: i64 = 1 << 12;
const FIELD_FLAG_RADIO: i64 = 1 << 15;
const FIELD_FLAG_PUSHBUTTON: i64 = 1 << 16;
const FIELD_FLAG_COMBO: i64 = 1 << 17;

#[derive(Clone, Copy, Debug)]
struct DefaultAppearance {
    font_size: f64,
    color: (f64, f64, f64),
}

impl Default for DefaultAppearance {
    fn default() -> Self {
        Self {
            font_size: 10.0,
            color: (0.0, 0.0, 0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonAppearanceKind {
    Checkbox,
    Radio,
    PushButton,
}

#[derive(Clone, Copy)]
struct WidgetAppearanceBox<'a> {
    width: f64,
    height: f64,
    mk: Option<&'a PdfDictionary>,
}

#[derive(Clone, Copy)]
struct TextFieldAppearance {
    default_appearance: DefaultAppearance,
    alignment: i64,
    multiline: bool,
}

struct ButtonAppearance<'a> {
    kind: ButtonAppearanceKind,
    selected: bool,
    caption: &'a str,
    caption_bytes: &'a [u8],
    default_appearance: DefaultAppearance,
}

impl<'a> WidgetAppearanceBox<'a> {
    fn new(width: f64, height: f64, mk: Option<&'a PdfDictionary>) -> Self {
        Self { width, height, mk }
    }
}

fn synthesize_annotation_appearance(
    annot: &PdfDictionary,
    reader: &crate::reader::PdfReader,
    engine: &ContentEngine,
    rect: [f64; 4],
) -> Option<(PdfDictionary, Vec<u8>)> {
    let width = (rect[2] - rect[0]).abs();
    let height = (rect[3] - rect[1]).abs();
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let field_chain = collect_field_chain(annot, reader);
    let field_type_obj = inherited_field_object(&field_chain, "FT")?;
    let field_type = field_type_obj.as_name()?.to_string();
    let acroform = resolve_acroform_dict(engine, reader);
    let need_appearances = acroform
        .as_ref()
        .and_then(|dict| dict.get_bool("NeedAppearances"))
        .unwrap_or(false);
    let field_flags = inherited_field_integer(&field_chain, "Ff").unwrap_or(0);
    let alignment = inherited_field_integer(&field_chain, "Q")
        .or_else(|| acroform.as_ref().and_then(|dict| dict.get_integer("Q")))
        .unwrap_or(0);
    let default_appearance = inherited_field_object(&field_chain, "DA")
        .and_then(|obj| object_string_bytes(&obj))
        .or_else(|| {
            acroform
                .as_ref()
                .and_then(|dict| dict.get("DA"))
                .and_then(object_string_bytes)
        })
        .as_deref()
        .map(parse_default_appearance)
        .unwrap_or_default();
    let value = inherited_field_object(&field_chain, "V");
    let options = inherited_field_object(&field_chain, "Opt");
    let mk = annot.get_dict("MK").cloned();

    let mut content = String::new();
    match field_type.as_str() {
        "Tx" => {
            let (text, text_bytes) = value.as_ref().and_then(display_text_from_object)?;
            if text.is_empty() {
                return None;
            }
            append_text_field_appearance(
                &mut content,
                WidgetAppearanceBox::new(width, height, mk.as_ref()),
                &text,
                &text_bytes,
                TextFieldAppearance {
                    default_appearance,
                    alignment,
                    multiline: field_flags & FIELD_FLAG_MULTILINE != 0,
                },
            );
        }
        "Btn" => {
            let kind = if field_flags & FIELD_FLAG_PUSHBUTTON != 0 {
                ButtonAppearanceKind::PushButton
            } else if field_flags & FIELD_FLAG_RADIO != 0 {
                ButtonAppearanceKind::Radio
            } else {
                ButtonAppearanceKind::Checkbox
            };
            let selected = button_is_selected(kind, annot, value.as_ref());
            let caption = mk
                .as_ref()
                .and_then(|dict| dict.get("CA"))
                .and_then(display_text_from_object)
                .unwrap_or_else(|| (String::new(), Vec::new()));
            if kind == ButtonAppearanceKind::PushButton && caption.0.is_empty() {
                return None;
            }
            if kind != ButtonAppearanceKind::PushButton && caption.0.chars().count() > 1 {
                return None;
            }
            if kind != ButtonAppearanceKind::PushButton && !selected {
                return None;
            }
            let has_explicit_button_chrome = mk
                .as_ref()
                .map(|dict| {
                    dict.contains_key("CA") || dict.contains_key("BG") || dict.contains_key("BC")
                })
                .unwrap_or(false);
            if kind != ButtonAppearanceKind::PushButton
                && !need_appearances
                && !has_explicit_button_chrome
            {
                return None;
            }
            if kind == ButtonAppearanceKind::Checkbox && selected {
                if let Some(label) = checkbox_label_state(annot, value.as_ref()) {
                    append_text_field_appearance(
                        &mut content,
                        WidgetAppearanceBox::new(width, height, mk.as_ref()),
                        &label,
                        label.as_bytes(),
                        TextFieldAppearance {
                            default_appearance,
                            alignment,
                            multiline: false,
                        },
                    );
                } else {
                    append_button_appearance(
                        &mut content,
                        WidgetAppearanceBox::new(width, height, mk.as_ref()),
                        ButtonAppearance {
                            kind,
                            selected,
                            caption: &caption.0,
                            caption_bytes: &caption.1,
                            default_appearance,
                        },
                    );
                }
            } else {
                append_button_appearance(
                    &mut content,
                    WidgetAppearanceBox::new(width, height, mk.as_ref()),
                    ButtonAppearance {
                        kind,
                        selected,
                        caption: &caption.0,
                        caption_bytes: &caption.1,
                        default_appearance,
                    },
                );
            }
        }
        "Ch" => {
            let selected = choice_display_text(value.as_ref(), options.as_ref())?;
            if selected.0.is_empty() {
                return None;
            }
            append_text_field_appearance(
                &mut content,
                WidgetAppearanceBox::new(width, height, mk.as_ref()),
                &selected.0,
                &selected.1,
                TextFieldAppearance {
                    default_appearance,
                    alignment,
                    multiline: field_flags & FIELD_FLAG_COMBO == 0,
                },
            );
        }
        _ => return None,
    }

    if content.is_empty() {
        return None;
    }

    let mut form = synthesized_appearance_form_dict(width, height);
    form.insert("Length", PdfObject::Integer(content.len() as i64));
    Some((form, content.into_bytes()))
}

fn collect_field_chain(
    annot: &PdfDictionary,
    reader: &crate::reader::PdfReader,
) -> Vec<PdfDictionary> {
    let mut chain = vec![annot.clone()];
    let mut parent = annot.get("Parent").cloned();
    for _ in 0..16 {
        let Some(parent_obj) = parent else {
            break;
        };
        let Ok(PdfObject::Dictionary(parent_dict)) = reader.resolve(parent_obj) else {
            break;
        };
        parent = parent_dict.get("Parent").cloned();
        chain.push(parent_dict);
    }
    chain
}

fn inherited_field_object(chain: &[PdfDictionary], key: &str) -> Option<PdfObject> {
    chain.iter().find_map(|dict| dict.get(key).cloned())
}

fn inherited_field_integer(chain: &[PdfDictionary], key: &str) -> Option<i64> {
    chain.iter().find_map(|dict| dict.get_integer(key))
}

fn resolve_acroform_dict(
    engine: &ContentEngine,
    reader: &crate::reader::PdfReader,
) -> Option<PdfDictionary> {
    let catalog = engine.document().get_catalog().ok()?;
    let acroform = catalog.get("AcroForm")?.clone();
    match reader.resolve(acroform).ok()? {
        PdfObject::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

fn object_string_bytes(obj: &PdfObject) -> Option<Vec<u8>> {
    obj.as_string().map(|bytes| bytes.to_vec())
}

fn parse_default_appearance(bytes: &[u8]) -> DefaultAppearance {
    let mut appearance = DefaultAppearance::default();
    let Ok(operations) = crate::content::ContentParser::parse(bytes) else {
        return appearance;
    };
    for op in operations {
        match op.operator.as_str() {
            "Tf" => {
                if let Some(size) = op.number(1) {
                    appearance.font_size = size;
                }
            }
            "g" => {
                if let Some(gray) = op.number(0) {
                    let gray = clamp_unit(gray);
                    appearance.color = (gray, gray, gray);
                }
            }
            "rg" => {
                if let (Some(r), Some(g), Some(b)) = (op.number(0), op.number(1), op.number(2)) {
                    appearance.color = (clamp_unit(r), clamp_unit(g), clamp_unit(b));
                }
            }
            "k" => {
                if let (Some(c), Some(m), Some(y), Some(k)) =
                    (op.number(0), op.number(1), op.number(2), op.number(3))
                {
                    appearance.color = cmyk_to_rgb(c, m, y, k);
                }
            }
            _ => {}
        }
    }
    appearance
}

fn synthesized_appearance_form_dict(width: f64, height: f64) -> PdfDictionary {
    let mut font = PdfDictionary::empty();
    font.insert("Type", PdfObject::Name("Font".to_string()));
    font.insert("Subtype", PdfObject::Name("Type1".to_string()));
    font.insert("BaseFont", PdfObject::Name("Helvetica".to_string()));
    font.insert("Encoding", PdfObject::Name("WinAnsiEncoding".to_string()));

    let mut fonts = PdfDictionary::empty();
    fonts.insert("F1", PdfObject::Dictionary(font));

    let mut resources = PdfDictionary::empty();
    resources.insert("Font", PdfObject::Dictionary(fonts));

    let mut form = PdfDictionary::empty();
    form.insert("Type", PdfObject::Name("XObject".to_string()));
    form.insert("Subtype", PdfObject::Name("Form".to_string()));
    form.insert(
        "BBox",
        PdfObject::Array(vec![
            PdfObject::Real(0.0),
            PdfObject::Real(0.0),
            PdfObject::Real(width),
            PdfObject::Real(height),
        ]),
    );
    form.insert("Resources", PdfObject::Dictionary(resources));
    form
}

fn append_text_field_appearance(
    content: &mut String,
    geometry: WidgetAppearanceBox<'_>,
    text: &str,
    text_bytes: &[u8],
    appearance: TextFieldAppearance,
) {
    append_explicit_widget_chrome(content, geometry.width, geometry.height, geometry.mk);
    if text.is_empty() {
        return;
    }

    let font_size = effective_font_size(
        appearance.default_appearance.font_size,
        text,
        geometry.width,
        geometry.height,
        appearance.multiline,
    );
    if appearance.multiline {
        let line_height = font_size * 1.2;
        let max_lines = ((geometry.height - 6.0).max(font_size) / line_height)
            .floor()
            .max(1.0) as usize;
        let mut y = (geometry.height - font_size - 3.0).max(2.0);
        for line in text.lines().take(max_lines) {
            append_text_run(
                content,
                line.as_bytes(),
                font_size,
                appearance.default_appearance.color,
                text_x_for_alignment(line, font_size, geometry.width, appearance.alignment),
                y,
            );
            y -= line_height;
            if y < 2.0 {
                break;
            }
        }
    } else {
        let literal_bytes = if text_bytes.is_empty() {
            text.as_bytes()
        } else {
            text_bytes
        };
        let y = ((geometry.height - font_size) * 0.5).max(2.0);
        append_text_run(
            content,
            literal_bytes,
            font_size,
            appearance.default_appearance.color,
            text_x_for_alignment(text, font_size, geometry.width, appearance.alignment),
            y,
        );
    }
}

fn append_button_appearance(
    content: &mut String,
    geometry: WidgetAppearanceBox<'_>,
    appearance: ButtonAppearance<'_>,
) {
    match appearance.kind {
        ButtonAppearanceKind::Checkbox => {
            append_widget_background_and_border(
                content,
                geometry.width,
                geometry.height,
                geometry.mk,
                (1.0, 1.0, 1.0),
            );
            if appearance.selected {
                let stroke = (geometry.width.min(geometry.height) * 0.09).clamp(1.2, 3.0);
                let x1 = geometry.width * 0.22;
                let y1 = geometry.height * 0.50;
                let x2 = geometry.width * 0.42;
                let y2 = geometry.height * 0.28;
                let x3 = geometry.width * 0.80;
                let y3 = geometry.height * 0.76;
                let _ = writeln!(
                    content,
                    "q 0 0 0 RG {} w {} {} m {} {} l {} {} l S Q",
                    pdf_num(stroke),
                    pdf_num(x1),
                    pdf_num(y1),
                    pdf_num(x2),
                    pdf_num(y2),
                    pdf_num(x3),
                    pdf_num(y3)
                );
            }
        }
        ButtonAppearanceKind::Radio => {
            append_widget_background(
                content,
                geometry.width,
                geometry.height,
                geometry.mk,
                (1.0, 1.0, 1.0),
            );
            let border = mk_rgb(geometry.mk, "BC").unwrap_or((0.0, 0.0, 0.0));
            append_circle(
                content,
                geometry.width * 0.5,
                geometry.height * 0.5,
                geometry.width.min(geometry.height) * 0.42,
                border,
                false,
            );
            if appearance.selected {
                append_circle(
                    content,
                    geometry.width * 0.5,
                    geometry.height * 0.5,
                    geometry.width.min(geometry.height) * 0.20,
                    border,
                    true,
                );
            }
        }
        ButtonAppearanceKind::PushButton => {
            append_widget_background_and_border(
                content,
                geometry.width,
                geometry.height,
                geometry.mk,
                (0.92, 0.92, 0.92),
            );
            if !appearance.caption.is_empty() {
                let font_size = effective_font_size(
                    appearance.default_appearance.font_size,
                    appearance.caption,
                    geometry.width,
                    geometry.height,
                    false,
                );
                let literal_bytes = if appearance.caption_bytes.is_empty() {
                    appearance.caption.as_bytes()
                } else {
                    appearance.caption_bytes
                };
                append_text_run(
                    content,
                    literal_bytes,
                    font_size,
                    appearance.default_appearance.color,
                    text_x_for_alignment(appearance.caption, font_size, geometry.width, 1),
                    ((geometry.height - font_size) * 0.5).max(2.0),
                );
            }
        }
    }
}

fn append_widget_background_and_border(
    content: &mut String,
    width: f64,
    height: f64,
    mk: Option<&PdfDictionary>,
    default_bg: (f64, f64, f64),
) {
    append_widget_background(content, width, height, mk, default_bg);
    let border = mk_rgb(mk, "BC").unwrap_or((0.0, 0.0, 0.0));
    let _ = writeln!(
        content,
        "q {} {} {} RG 1 w 0.5 0.5 {} {} re S Q",
        pdf_num(border.0),
        pdf_num(border.1),
        pdf_num(border.2),
        pdf_num((width - 1.0).max(0.0)),
        pdf_num((height - 1.0).max(0.0))
    );
}

fn append_explicit_widget_chrome(
    content: &mut String,
    width: f64,
    height: f64,
    mk: Option<&PdfDictionary>,
) {
    if let Some(bg) = mk_rgb(mk, "BG") {
        let _ = writeln!(
            content,
            "q {} {} {} rg 0 0 {} {} re f Q",
            pdf_num(bg.0),
            pdf_num(bg.1),
            pdf_num(bg.2),
            pdf_num(width),
            pdf_num(height)
        );
    }
    if let Some(border) = mk_rgb(mk, "BC") {
        let _ = writeln!(
            content,
            "q {} {} {} RG 1 w 0.5 0.5 {} {} re S Q",
            pdf_num(border.0),
            pdf_num(border.1),
            pdf_num(border.2),
            pdf_num((width - 1.0).max(0.0)),
            pdf_num((height - 1.0).max(0.0))
        );
    }
}

fn append_widget_background(
    content: &mut String,
    width: f64,
    height: f64,
    mk: Option<&PdfDictionary>,
    default_bg: (f64, f64, f64),
) {
    let bg = mk_rgb(mk, "BG").unwrap_or(default_bg);
    let _ = writeln!(
        content,
        "q {} {} {} rg 0 0 {} {} re f Q",
        pdf_num(bg.0),
        pdf_num(bg.1),
        pdf_num(bg.2),
        pdf_num(width),
        pdf_num(height)
    );
}

fn append_text_run(
    content: &mut String,
    bytes: &[u8],
    font_size: f64,
    color: (f64, f64, f64),
    x: f64,
    y: f64,
) {
    let _ = writeln!(
        content,
        "q BT /F1 {} Tf {} {} {} rg 1 0 0 1 {} {} Tm {} Tj ET Q",
        pdf_num(font_size),
        pdf_num(color.0),
        pdf_num(color.1),
        pdf_num(color.2),
        pdf_num(x),
        pdf_num(y),
        pdf_literal_bytes(bytes)
    );
}

fn append_circle(
    content: &mut String,
    cx: f64,
    cy: f64,
    radius: f64,
    color: (f64, f64, f64),
    fill: bool,
) {
    let k = radius * 0.552_284_749_830_793_6;
    let op = if fill { "f" } else { "S" };
    let color_op = if fill { "rg" } else { "RG" };
    let _ = writeln!(
        content,
        "q {} {} {} {} 1 w {} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h {} Q",
        pdf_num(color.0),
        pdf_num(color.1),
        pdf_num(color.2),
        color_op,
        pdf_num(cx + radius),
        pdf_num(cy),
        pdf_num(cx + radius),
        pdf_num(cy + k),
        pdf_num(cx + k),
        pdf_num(cy + radius),
        pdf_num(cx),
        pdf_num(cy + radius),
        pdf_num(cx - k),
        pdf_num(cy + radius),
        pdf_num(cx - radius),
        pdf_num(cy + k),
        pdf_num(cx - radius),
        pdf_num(cy),
        pdf_num(cx - radius),
        pdf_num(cy - k),
        pdf_num(cx - k),
        pdf_num(cy - radius),
        pdf_num(cx),
        pdf_num(cy - radius),
        pdf_num(cx + k),
        pdf_num(cy - radius),
        pdf_num(cx + radius),
        pdf_num(cy - k),
        pdf_num(cx + radius),
        pdf_num(cy),
        op
    );
}

fn effective_font_size(
    requested: f64,
    text: &str,
    width: f64,
    height: f64,
    multiline: bool,
) -> f64 {
    let mut size = if requested > 0.0 {
        requested
    } else {
        (height * 0.55).clamp(4.0, 12.0)
    };
    size = size.min((height - 4.0).max(4.0));
    if !multiline {
        let available = (width - 6.0).max(1.0);
        while approximate_text_width(text, size) > available && size > 4.0 {
            size -= 0.5;
        }
    }
    size.max(4.0)
}

fn text_x_for_alignment(text: &str, font_size: f64, width: f64, alignment: i64) -> f64 {
    let padding = 3.0;
    let text_width = approximate_text_width(text, font_size);
    match alignment {
        1 => ((width - text_width) * 0.5).max(padding),
        2 => (width - text_width - padding).max(padding),
        _ => padding,
    }
}

fn approximate_text_width(text: &str, font_size: f64) -> f64 {
    text.chars().count() as f64 * font_size * 0.52
}

fn button_is_selected(
    kind: ButtonAppearanceKind,
    annot: &PdfDictionary,
    value: Option<&PdfObject>,
) -> bool {
    let appearance_state = annot
        .get_name("AS")
        .filter(|state| *state != "Off")
        .map(str::to_string);
    let value_state = value.and_then(object_state_name);
    match kind {
        ButtonAppearanceKind::PushButton => false,
        ButtonAppearanceKind::Checkbox => {
            appearance_state.is_some() || value_state_is_on(value_state)
        }
        ButtonAppearanceKind::Radio => match (appearance_state, value_state) {
            (Some(appearance), Some(value)) => appearance == value,
            (Some(_), None) => true,
            (None, Some(value)) => value != "Off",
            (None, None) => false,
        },
    }
}

fn checkbox_label_state(annot: &PdfDictionary, value: Option<&PdfObject>) -> Option<String> {
    let state = annot
        .get_name("AS")
        .filter(|state| *state != "Off")
        .map(str::to_string)
        .or_else(|| value.and_then(object_state_name))?;
    if is_label_like_button_state(&state) {
        Some(state)
    } else {
        None
    }
}

fn is_label_like_button_state(state: &str) -> bool {
    let normalized = state.trim();
    if matches!(normalized, "" | "Off" | "On" | "Yes" | "1") {
        return false;
    }
    normalized.chars().count() > 1 && normalized.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn value_state_is_on(value: Option<String>) -> bool {
    value
        .map(|value| !value.is_empty() && value != "Off")
        .unwrap_or(false)
}

fn object_state_name(obj: &PdfObject) -> Option<String> {
    match obj {
        PdfObject::Name(name) => Some(name.clone()),
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    }
}

fn display_text_from_object(obj: &PdfObject) -> Option<(String, Vec<u8>)> {
    match obj {
        PdfObject::String(bytes) => Some((decode_pdf_text_string(bytes), bytes.clone())),
        PdfObject::Name(name) => {
            if name == "Off" {
                None
            } else {
                Some((name.clone(), name.as_bytes().to_vec()))
            }
        }
        PdfObject::Integer(value) => {
            let text = value.to_string();
            Some((text.clone(), text.into_bytes()))
        }
        PdfObject::Real(value) => {
            let text = pdf_num(*value);
            Some((text.clone(), text.into_bytes()))
        }
        PdfObject::Array(items) => {
            let values: Vec<(String, Vec<u8>)> =
                items.iter().filter_map(display_text_from_object).collect();
            if values.is_empty() {
                None
            } else {
                let text = values
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Some((text.clone(), text.into_bytes()))
            }
        }
        _ => None,
    }
}

fn choice_display_text(
    value: Option<&PdfObject>,
    options: Option<&PdfObject>,
) -> Option<(String, Vec<u8>)> {
    value
        .and_then(display_text_from_object)
        .or_else(|| first_option_display_text(options?))
}

fn first_option_display_text(options: &PdfObject) -> Option<(String, Vec<u8>)> {
    let PdfObject::Array(items) = options else {
        return None;
    };
    items.iter().find_map(option_display_text)
}

fn option_display_text(option: &PdfObject) -> Option<(String, Vec<u8>)> {
    match option {
        PdfObject::Array(items) => items
            .get(1)
            .or_else(|| items.first())
            .and_then(display_text_from_object),
        other => display_text_from_object(other),
    }
}

fn mk_rgb(mk: Option<&PdfDictionary>, key: &str) -> Option<(f64, f64, f64)> {
    let arr = mk?.get_array(key)?;
    match arr.len() {
        1 => {
            let gray = clamp_unit(arr[0].as_number()?);
            Some((gray, gray, gray))
        }
        3 => Some((
            clamp_unit(arr[0].as_number()?),
            clamp_unit(arr[1].as_number()?),
            clamp_unit(arr[2].as_number()?),
        )),
        4 => Some(cmyk_to_rgb(
            arr[0].as_number()?,
            arr[1].as_number()?,
            arr[2].as_number()?,
            arr[3].as_number()?,
        )),
        _ => None,
    }
}

fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> (f64, f64, f64) {
    let c = clamp_unit(c);
    let m = clamp_unit(m);
    let y = clamp_unit(y);
    let k = clamp_unit(k);
    (
        clamp_unit((1.0 - c) * (1.0 - k)),
        clamp_unit((1.0 - m) * (1.0 - k)),
        clamp_unit((1.0 - y) * (1.0 - k)),
    )
}

fn clamp_unit(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn pdf_num(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{value:.3}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

fn pdf_literal_bytes(bytes: &[u8]) -> String {
    let mut out = String::from("(");
    for &byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(byte as char);
            }
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(byte as char),
            _ => {
                let _ = write!(out, "\\{byte:03o}");
            }
        }
    }
    out.push(')');
    out
}

fn resolve_appearance_stream(
    value: &PdfObject,
    reader: &crate::reader::PdfReader,
) -> Option<(PdfDictionary, Vec<u8>)> {
    match reader.resolve(value.clone()).ok()? {
        PdfObject::Stream { dict, raw } => Some((dict, raw)),
        _ => None,
    }
}

fn extract_rect(dict: &PdfDictionary) -> Option<[f64; 4]> {
    let arr = dict.get("Rect")?.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    let vals: Vec<f64> = arr
        .iter()
        .take(4)
        .filter_map(PdfObject::as_number)
        .collect();
    if vals.len() < 4 {
        return None;
    }
    Some([vals[0], vals[1], vals[2], vals[3]])
}

fn annotation_appearance_ctm(rect: [f64; 4], bbox: [f64; 4]) -> Option<Transform2D> {
    let rect_x0 = rect[0].min(rect[2]);
    let rect_y0 = rect[1].min(rect[3]);
    let rect_w = (rect[2] - rect[0]).abs();
    let rect_h = (rect[3] - rect[1]).abs();
    let bbox_x0 = bbox[0].min(bbox[2]);
    let bbox_y0 = bbox[1].min(bbox[3]);
    let bbox_w = (bbox[2] - bbox[0]).abs();
    let bbox_h = (bbox[3] - bbox[1]).abs();
    if rect_w <= 0.0 || rect_h <= 0.0 || bbox_w <= 0.0 || bbox_h <= 0.0 {
        return None;
    }
    let to_origin = Transform2D::translation(-bbox_x0, -bbox_y0);
    let scale = Transform2D::scale(rect_w / bbox_w, rect_h / bbox_h);
    let to_rect = Transform2D::translation(rect_x0, rect_y0);
    Some(to_origin.concat(&scale).concat(&to_rect))
}

/// Extract a Form XObject's `/BBox` as `[x_min, y_min, x_max, y_max]`.
/// Returns `None` when absent or not a 4-number array.
fn extract_bbox(dict: &PdfDictionary) -> Option<[f64; 4]> {
    let arr = dict.get("BBox")?.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    let vals: Vec<f64> = arr
        .iter()
        .take(4)
        .filter_map(PdfObject::as_number)
        .collect();
    if vals.len() < 4 {
        return None;
    }
    Some([vals[0], vals[1], vals[2], vals[3]])
}

/// Extract a Form XObject's `/Matrix`, defaulting to the identity matrix when
/// absent or malformed.
fn extract_form_matrix(dict: &PdfDictionary) -> crate::content::Matrix {
    const IDENTITY: crate::content::Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let Some(arr) = dict.get("Matrix").and_then(PdfObject::as_array) else {
        return IDENTITY;
    };
    let v: Vec<f64> = arr.iter().filter_map(PdfObject::as_number).collect();
    if v.len() < 6 {
        return IDENTITY;
    }
    [v[0], v[1], v[2], v[3], v[4], v[5]]
}

/// Merge a Form XObject's resources over the parent page's resources. The
/// Form's entries take priority on a name collision; names absent from the Form
/// fall through to the page's resources.
fn merge_resources(form_res: PageResources, page_res: &PageResources) -> PageResources {
    let mut merged = page_res.clone();
    for (k, v) in form_res.fonts {
        merged.fonts.insert(k, v);
    }
    for (k, v) in form_res.xobjects {
        merged.xobjects.insert(k, v);
    }
    for (k, v) in form_res.color_spaces {
        merged.color_spaces.insert(k, v);
    }
    for (k, v) in form_res.ext_g_states {
        merged.ext_g_states.insert(k, v);
    }
    for (k, v) in form_res.patterns {
        merged.patterns.insert(k, v);
    }
    for (k, v) in form_res.shadings {
        merged.shadings.insert(k, v);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::encoder::ImageEncoder;
    use crate::render::{flatten_path, Path, PathPainter, RenderColor, BLACK, BLUE, RED, WHITE};

    fn fixture(path: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), path)
    }

    fn simple_pdf_with_extgstate(
        content: &str,
        extgstates: &[&str],
        extgstate_resources: &str,
    ) -> Vec<u8> {
        fn add_obj(objects: &mut Vec<Vec<u8>>, body: impl AsRef<[u8]>) -> usize {
            objects.push(body.as_ref().to_vec());
            objects.len()
        }

        let mut objects = Vec::new();
        let font = add_obj(
            &mut objects,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        let ext_ids: Vec<usize> = extgstates
            .iter()
            .map(|body| add_obj(&mut objects, body.as_bytes()))
            .collect();
        let stream = format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        );
        let contents = add_obj(&mut objects, stream.as_bytes());
        let page = objects.len() + 1;
        let pages = objects.len() + 2;
        let root = objects.len() + 3;
        let resources = extgstate_resources
            .replace("{font}", &font.to_string())
            .replace("{gs1}", &ext_ids[0].to_string())
            .replace("{gs2}", &ext_ids[1].to_string());
        add_obj(
            &mut objects,
            format!(
                "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 100 100] \
                 /Resources {} /Contents {} 0 R >>",
                pages, resources, contents
            )
            .as_bytes(),
        );
        add_obj(
            &mut objects,
            format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", page).as_bytes(),
        );
        add_obj(
            &mut objects,
            format!("<< /Type /Catalog /Pages {} 0 R >>", pages).as_bytes(),
        );

        let mut out = bytearray_pdf_header();
        let mut offsets = vec![0usize];
        for (idx, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            out.extend_from_slice(obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let startxref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            out.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {} 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                root,
                startxref
            )
            .as_bytes(),
        );
        out
    }

    fn bytearray_pdf_header() -> Vec<u8> {
        b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec()
    }

    #[test]
    fn type0_font_decodes_two_byte_strings() {
        let bytes = [0x00u8, 0x48, 0x00, 0x69];
        let mut cmap = std::collections::HashMap::new();
        cmap.insert(72u32, 'H');
        cmap.insert(105u32, 'i');

        let chars: Vec<char> = bytes
            .chunks(2)
            .filter_map(|pair| {
                if pair.len() < 2 {
                    return None;
                }
                let cid = (u32::from(pair[0]) << 8) | u32::from(pair[1]);
                Some(cmap.get(&cid).copied().unwrap_or('\u{FFFD}'))
            })
            .collect();

        assert_eq!(chars, vec!['H', 'i']);
    }

    #[test]
    fn extract_glyph_path_by_gid_returns_positive_advance_for_gid_zero() {
        let font_bytes = get_fallback_font("Helvetica").expect("fallback font");
        let (_path, advance) =
            crate::render::glyph_outline::extract_glyph_path_by_gid(font_bytes, 0);
        assert!(advance > 0.0);
    }

    #[test]
    fn extract_glyph_path_by_gid_matches_char_lookup_for_ascii() {
        let font_bytes = get_fallback_font("Helvetica").expect("fallback font");
        let face = ttf_parser::Face::parse(font_bytes, 0).expect("parse fallback font");
        let gid_for_a = face.glyph_index('A').expect("glyph A").0;

        let (_path_by_char, adv_char) = RenderState::extract_glyph_path(font_bytes, 'A');
        let (_path_by_gid, adv_gid) =
            crate::render::glyph_outline::extract_glyph_path_by_gid(font_bytes, gid_for_a);

        assert!((adv_char - adv_gid).abs() < 1.0);
    }

    #[test]
    fn top_level_page_group_flattens_after_blending() {
        let pdf = simple_pdf_with_extgstate(
            "q /GS1 gs 1 0 0 rg 10 10 50 40 re f Q\n\
             q /GS2 gs 0 0 1 rg 35 30 50 40 re f Q",
            &[
                "<< /Type /ExtGState /ca 0.45 /CA 0.45 /BM /Multiply >>",
                "<< /Type /ExtGState /ca 0.55 /CA 0.55 /BM /Screen >>",
            ],
            "<< /Font << /F1 {font} 0 R >> /ExtGState << /GS1 {gs1} 0 R /GS2 {gs2} 0 R >> >>",
        );
        let engine = ContentEngine::open_bytes(pdf).expect("open transparency PDF");
        let buf = engine.render_page(1, 72).expect("render transparency PDF");

        assert_eq!(buf.get_pixel(5, 5), WHITE, "empty page area is white paper");
        let blue_only = buf.get_pixel(80, 40);
        assert!(
            blue_only[2] > 240 && blue_only[0] < 150 && blue_only[1] < 150,
            "Screen over initial transparent backdrop must survive final white flatten: {:?}",
            blue_only
        );
        let overlap = buf.get_pixel(45, 60);
        assert!(
            (overlap[0] as i32 - 178).abs() <= 2
                && (overlap[1] as i32 - 63).abs() <= 2
                && (overlap[2] as i32 - 203).abs() <= 2,
            "Screen over partially transparent red backdrop should match PDF blend math: {:?}",
            overlap
        );
    }

    #[test]
    fn annotation_appearance_stream_renders_selected_state_by_default() {
        let pdf = pdf_with_annotation_appearance(0, true, false);
        let engine = ContentEngine::open_bytes(pdf).expect("open annotation PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render annotation PDF");

        assert!(
            count_red_pixels(&buf) > 100,
            "selected /AS appearance should paint the widget"
        );
        assert_eq!(
            count_blue_pixels(&buf),
            0,
            "the /Off appearance must not be rendered when /AS selects /Yes"
        );
    }

    #[test]
    fn hidden_annotation_appearance_is_not_rendered() {
        let pdf = pdf_with_annotation_appearance(2, false, false);
        let engine = ContentEngine::open_bytes(pdf).expect("open hidden annotation PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render hidden annotation PDF");

        assert_eq!(
            count_red_pixels(&buf),
            0,
            "hidden annotations must not render their appearance streams"
        );
    }

    #[test]
    fn need_appearances_does_not_override_existing_widget_appearance() {
        let pdf = pdf_with_annotation_appearance(0, true, true);
        let engine = ContentEngine::open_bytes(pdf).expect("open NeedAppearances PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render NeedAppearances PDF");

        assert!(
            count_red_pixels(&buf) > 100,
            "a usable author-provided /AP stream should still take precedence"
        );
    }

    #[test]
    fn text_widget_without_appearance_synthesizes_value_from_da() {
        let widget = b"<< /Type /Annot /Subtype /Widget /Rect [20 35 90 60] \
                       /FT /Tx /T (name) /V (Hi) /DA (/F1 14 Tf 0 0 1 rg) /Q 1 >>";
        let pdf = pdf_with_form_objects(vec![widget.to_vec()], "5 0 R", "5 0 R", "");
        let engine = ContentEngine::open_bytes(pdf).expect("open missing-AP text widget PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render text widget PDF");

        assert!(
            count_blue_pixels(&buf) > 5,
            "text widget synthesis should honor blue fill color from /DA"
        );
    }

    #[test]
    fn checkbox_without_appearance_synthesizes_checked_and_unchecked_states() {
        let checked = b"<< /Type /Annot /Subtype /Widget /Rect [25 25 55 55] \
                         /FT /Btn /T (agree) /V /Yes /AS /Yes >>";
        let unchecked = b"<< /Type /Annot /Subtype /Widget /Rect [25 25 55 55] \
                           /FT /Btn /T (agree) /V /Off /AS /Off >>";
        let checked_pdf = pdf_with_form_objects(
            vec![checked.to_vec()],
            "5 0 R",
            "5 0 R",
            "/NeedAppearances true",
        );
        let unchecked_pdf = pdf_with_form_objects(
            vec![unchecked.to_vec()],
            "5 0 R",
            "5 0 R",
            "/NeedAppearances true",
        );
        let checked_engine = ContentEngine::open_bytes(checked_pdf).expect("open checked PDF");
        let unchecked_engine =
            ContentEngine::open_bytes(unchecked_pdf).expect("open unchecked PDF");
        let checked_buf =
            PageRenderer::render_page(&checked_engine, 1, 72).expect("render checked PDF");
        let unchecked_buf =
            PageRenderer::render_page(&unchecked_engine, 1, 72).expect("render unchecked PDF");

        assert!(
            count_dark_pixels(&checked_buf) > count_dark_pixels(&unchecked_buf) + 10,
            "checked checkbox synthesis should add a visible check mark"
        );
    }

    #[test]
    fn radio_widget_without_appearance_uses_parent_value() {
        let parent = b"<< /FT /Btn /Ff 32768 /V /Choice /Kids [6 0 R] >>";
        let widget = b"<< /Type /Annot /Subtype /Widget /Parent 5 0 R \
                       /Rect [25 25 55 55] /AS /Choice >>";
        let pdf = pdf_with_form_objects(
            vec![parent.to_vec(), widget.to_vec()],
            "6 0 R",
            "5 0 R",
            "/NeedAppearances true",
        );
        let engine = ContentEngine::open_bytes(pdf).expect("open missing-AP radio PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render radio PDF");

        assert!(
            count_dark_pixels(&buf) > 140,
            "selected radio synthesis should draw both ring and inner marker"
        );
    }

    #[test]
    fn pushbutton_without_appearance_synthesizes_caption() {
        let widget = b"<< /Type /Annot /Subtype /Widget /Rect [20 35 85 60] \
                       /FT /Btn /Ff 65536 /MK << /CA (Go) /BG [0.8 0.8 0.8] >> >>";
        let pdf = pdf_with_form_objects(vec![widget.to_vec()], "5 0 R", "5 0 R", "");
        let engine = ContentEngine::open_bytes(pdf).expect("open missing-AP pushbutton PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render pushbutton PDF");

        assert!(
            count_gray_pixels(&buf) > 500 && count_dark_pixels(&buf) > 50,
            "pushbutton synthesis should draw its background, border, and caption"
        );
    }

    #[test]
    fn choice_widget_without_appearance_synthesizes_selected_value() {
        let widget = b"<< /Type /Annot /Subtype /Widget /Rect [15 35 95 60] \
                       /FT /Ch /Ff 131072 /V (Banana) /Opt [(Apple) (Banana)] \
                       /DA (/F1 12 Tf 0 g) >>";
        let pdf = pdf_with_form_objects(vec![widget.to_vec()], "5 0 R", "5 0 R", "");
        let engine = ContentEngine::open_bytes(pdf).expect("open missing-AP choice PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render choice PDF");

        assert!(
            count_dark_pixels(&buf) > 20,
            "choice synthesis should render the selected option text"
        );
    }

    #[test]
    fn hidden_widget_without_appearance_is_not_synthesized() {
        let widget = b"<< /Type /Annot /Subtype /Widget /Rect [20 35 90 60] /F 2 \
                       /FT /Tx /T (hidden) /V (Hidden) /DA (/F1 14 Tf 0 g) >>";
        let pdf = pdf_with_form_objects(vec![widget.to_vec()], "5 0 R", "5 0 R", "");
        let engine = ContentEngine::open_bytes(pdf).expect("open hidden missing-AP PDF");
        let buf = PageRenderer::render_page(&engine, 1, 72).expect("render hidden PDF");

        assert_eq!(
            count_dark_pixels(&buf),
            0,
            "hidden missing-appearance widgets must stay hidden"
        );
    }

    #[test]
    fn font_rendering_regression_check() {
        let engine = ContentEngine::open_path(fixture("flate.pdf")).expect("open flate fixture");
        let buf = engine.render_page(1, 72).expect("render page");
        let raw = buf.to_raw_image();
        let channels = raw.channels as usize;
        let non_white = raw
            .pixels
            .chunks(channels)
            .filter(|pixel| pixel[0] < 200 || pixel[1] < 200 || pixel[2] < 200)
            .count();
        assert!(non_white > 20);
    }

    fn pdf_with_annotation_appearance(
        flags: i64,
        stateful: bool,
        need_appearances: bool,
    ) -> Vec<u8> {
        fn stream(body: &str, dict_extra: &str) -> Vec<u8> {
            format!(
                "<< {} /Length {} >>\nstream\n{}\nendstream",
                dict_extra,
                body.len(),
                body
            )
            .into_bytes()
        }

        let content = stream("", "");
        let red_appearance = stream(
            "1 0 0 rg 0 0 50 50 re f\n",
            "/Type /XObject /Subtype /Form /BBox [0 0 50 50] /Resources << >>",
        );
        let blue_appearance = stream(
            "0 0 1 rg 0 0 50 50 re f\n",
            "/Type /XObject /Subtype /Form /BBox [0 0 50 50] /Resources << >>",
        );

        let annot = if stateful {
            format!(
                "<< /Type /Annot /Subtype /Widget /Rect [20 20 70 70] /F {} \
                 /AS /Yes /AP << /N << /Yes 6 0 R /Off 7 0 R >> >> >>",
                flags
            )
        } else {
            format!(
                "<< /Type /Annot /Subtype /Widget /Rect [20 20 70 70] /F {} \
                 /AP << /N 6 0 R >> >>",
                flags
            )
        };

        let catalog = if need_appearances {
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /NeedAppearances true >> >>".to_vec()
        } else {
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()
        };

        let objects: Vec<Vec<u8>> = vec![
            catalog,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R /Annots [5 0 R] >>".to_vec(),
            content,
            annot.into_bytes(),
            red_appearance,
            blue_appearance,
        ];

        let mut out = bytearray_pdf_header();
        let mut offsets = vec![0usize];
        for (idx, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            out.extend_from_slice(obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let startxref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            out.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                startxref
            )
            .as_bytes(),
        );
        out
    }

    fn pdf_with_form_objects(
        form_objects: Vec<Vec<u8>>,
        page_annots: &str,
        acroform_fields: &str,
        acroform_extra: &str,
    ) -> Vec<u8> {
        fn stream(body: &str, dict_extra: &str) -> Vec<u8> {
            format!(
                "<< {} /Length {} >>\nstream\n{}\nendstream",
                dict_extra,
                body.len(),
                body
            )
            .into_bytes()
        }

        let font_number = 5 + form_objects.len();
        let catalog = format!(
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{}] \
             /DA (/F1 12 Tf 0 g) /DR << /Font << /F1 {} 0 R >> >> {} >> >>",
            acroform_fields, font_number, acroform_extra
        )
        .into_bytes();
        let content = stream("", "");
        let mut objects: Vec<Vec<u8>> = vec![
            catalog,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << >> /Contents 4 0 R /Annots [{}] >>",
                page_annots
            )
            .into_bytes(),
            content,
        ];
        objects.extend(form_objects);
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());

        let mut out = bytearray_pdf_header();
        let mut offsets = vec![0usize];
        for (idx, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            out.extend_from_slice(obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let startxref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            out.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                startxref
            )
            .as_bytes(),
        );
        out
    }

    fn count_red_pixels(buf: &PixelBuffer) -> usize {
        count_pixels_matching(buf, |pixel| {
            pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80
        })
    }

    fn count_blue_pixels(buf: &PixelBuffer) -> usize {
        count_pixels_matching(buf, |pixel| {
            pixel[2] > 200 && pixel[0] < 80 && pixel[1] < 80
        })
    }

    fn count_dark_pixels(buf: &PixelBuffer) -> usize {
        count_pixels_matching(buf, |pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80)
    }

    fn count_gray_pixels(buf: &PixelBuffer) -> usize {
        count_pixels_matching(buf, |pixel| {
            (pixel[0] as i16 - pixel[1] as i16).abs() < 4
                && (pixel[1] as i16 - pixel[2] as i16).abs() < 4
                && pixel[0] > 150
                && pixel[0] < 235
        })
    }

    fn count_pixels_matching(buf: &PixelBuffer, pred: impl Fn(PixelColor) -> bool) -> usize {
        let mut count = 0usize;
        for y in 0..buf.height {
            for x in 0..buf.width {
                if pred(buf.get_pixel(x as i32, y as i32)) {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn composite_group_with_full_alpha_paints_source() {
        let mut dst = PixelBuffer::new_filled(1, 1, WHITE);
        let mut src = PixelBuffer::new_transparent(1, 1);
        src.blend_pixel(0, 0, RED, 1.0);

        dst.composite_from(&src, 1.0, BlendMode::Normal, None);
        let result = dst.get_pixel(0, 0);
        assert!(result[0] > 200, "group should paint red: {:?}", result);
        assert!(result[1] < 50, "green channel should be low: {:?}", result);
    }

    #[test]
    fn composite_group_with_half_alpha_blends_with_destination() {
        let mut dst = PixelBuffer::new_filled(1, 1, WHITE);
        let mut src = PixelBuffer::new_transparent(1, 1);
        src.blend_pixel(0, 0, BLACK, 1.0);

        dst.composite_from(&src, 0.5, BlendMode::Normal, None);
        let result = dst.get_pixel(0, 0);
        assert!(
            result[0] > 100 && result[0] < 200,
            "50% black over white should be gray: {:?}",
            result
        );
    }

    #[test]
    fn is_transparency_group_detects_group_subtype() {
        let mut dict = PdfDictionary::empty();
        let mut group = PdfDictionary::empty();
        group.insert("S", PdfObject::Name("Transparency".to_string()));
        dict.insert("Group", PdfObject::Dictionary(group));
        assert!(is_transparency_group(&dict));
        assert!(!is_transparency_group(&PdfDictionary::empty()));
    }

    #[test]
    fn q_restore_restores_previous_smask_and_blend_mode() {
        let engine = ContentEngine::open_path(fixture("flate.pdf")).expect("open flate fixture");
        let viewport = Viewport::new([0.0, 0.0, 10.0, 10.0], 72);
        let buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut state = RenderState::new(buf, viewport, PageResources::default(), &engine, 1);

        state.dispatch(&ContentOperation::new("q", Vec::new()));
        state.buf.set_smask(AlphaMask::all_opaque(10, 10));
        state.buf.blend_mode = BlendMode::Multiply;
        state.dispatch(&ContentOperation::new("Q", Vec::new()));

        assert!(state.buf.smask_mask().is_none());
        assert_eq!(state.buf.blend_mode, BlendMode::Normal);
    }

    #[test]
    fn clip_mask_all_visible_pixels_are_visible() {
        let clip = ClipMask::all_visible(10, 10);
        assert!(clip.is_visible(0, 0));
        assert!(clip.is_visible(9, 9));
        assert!(clip.is_visible(5, 5));
    }

    #[test]
    fn clip_mask_set_and_is_visible() {
        let mut clip = ClipMask::all_visible(10, 10);
        clip.set(5, 5, false);
        assert!(!clip.is_visible(5, 5));
        assert!(clip.is_visible(4, 5));
    }

    #[test]
    fn clip_mask_out_of_bounds_is_visible() {
        let clip = ClipMask::all_visible(10, 10);
        assert!(clip.is_visible(-1, 0));
        assert!(clip.is_visible(10, 0));
        assert!(clip.is_visible(0, -1));
        assert!(clip.is_visible(0, 10));
    }

    #[test]
    fn clip_mask_intersect_produces_and_of_two_masks() {
        let mut a = ClipMask::all_visible(4, 1);
        let mut b = ClipMask::all_visible(4, 1);
        a.set(0, 0, false);
        b.set(1, 0, false);
        a.intersect(&b);
        assert!(!a.is_visible(0, 0));
        assert!(!a.is_visible(1, 0));
        assert!(a.is_visible(2, 0));
        assert!(a.is_visible(3, 0));
    }

    #[test]
    fn clip_mask_from_path_for_simple_rectangle() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut path = Path::new();
        path.rect(20.0, 20.0, 60.0, 60.0);
        let flat = flatten_path(&path, &ctm, &vp, 0.5);
        let clip = ClipMask::from_path(&flat, 100, 100, FillRule::NonZero);
        println!(
            "clip rect center={}, corner={}",
            clip.is_visible(50, 50),
            clip.is_visible(5, 5)
        );
        assert!(clip.is_visible(50, 50));
        assert!(!clip.is_visible(5, 5));
        assert!(!clip.is_visible(90, 90));
    }

    #[test]
    fn blend_pixel_respects_clip_mask() {
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut clip = ClipMask::all_visible(10, 10);
        clip.set(5, 5, false);
        buf.set_clip(clip);
        buf.blend_pixel(5, 5, RED, 1.0);
        assert_eq!(buf.get_pixel(5, 5), WHITE);
        buf.blend_pixel(3, 3, RED, 1.0);
        assert!(buf.get_pixel(3, 3)[0] > 100);
    }

    #[test]
    fn clear_clip_restores_all_visible() {
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut clip = ClipMask::all_visible(10, 10);
        clip.fill_rect(0, 0, 10, 10, false);
        buf.set_clip(clip);
        buf.blend_pixel(5, 5, RED, 1.0);
        assert_eq!(buf.get_pixel(5, 5), WHITE);
        buf.clear_clip();
        buf.blend_pixel(5, 5, RED, 1.0);
        assert!(buf.get_pixel(5, 5)[0] > 100);
    }

    #[test]
    fn clip_mask_fill_rect_marks_region_clipped() {
        let mut clip = ClipMask::all_visible(20, 20);
        clip.fill_rect(5, 5, 10, 10, false);
        assert!(!clip.is_visible(5, 5));
        assert!(!clip.is_visible(14, 14));
        assert!(clip.is_visible(4, 5));
        assert!(clip.is_visible(5, 4));
    }

    #[test]
    fn clip_mask_fill_rect_visible_restores_pixels() {
        let mut clip = ClipMask::all_visible(10, 10);
        clip.fill_rect(0, 0, 10, 10, false);
        clip.fill_rect(3, 3, 4, 4, true);
        assert!(!clip.is_visible(2, 2));
        assert!(clip.is_visible(5, 5));
    }

    #[test]
    fn clip_mask_from_path_evenodd_nested_rects() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut path = Path::new();
        path.rect(10.0, 10.0, 80.0, 80.0);
        path.rect(30.0, 30.0, 40.0, 40.0);
        let flat = flatten_path(&path, &ctm, &vp, 0.5);
        let clip_eo = ClipMask::from_path(&flat, 100, 100, FillRule::EvenOdd);
        let clip_nz = ClipMask::from_path(&flat, 100, 100, FillRule::NonZero);
        assert!(!clip_eo.is_visible(50, 50));
        assert!(clip_nz.is_visible(50, 50));
    }

    #[test]
    fn clip_is_preserved_across_simple_paint_operations() {
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let mut clip = ClipMask::all_visible(100, 100);
        clip.fill_rect(50, 0, 50, 100, false);
        buf.set_clip(clip);
        buf.fill_rect(0, 0, 100, 100, RED);
        assert_eq!(buf.get_pixel(25, 50), RED);
        assert_eq!(buf.get_pixel(75, 50), WHITE);
    }

    #[test]
    fn set_clip_intersects_with_existing_clip() {
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut clip1 = ClipMask::all_visible(10, 10);
        clip1.fill_rect(5, 0, 5, 10, false);
        buf.set_clip(clip1);
        let mut clip2 = ClipMask::all_visible(10, 10);
        clip2.fill_rect(0, 5, 10, 5, false);
        buf.set_clip(clip2);
        let clip = buf.clip_mask().expect("clip should be installed");
        assert!(clip.is_visible(2, 2));
        assert!(!clip.is_visible(7, 2));
        assert!(!clip.is_visible(2, 7));
        assert!(!clip.is_visible(7, 7));
    }

    #[test]
    fn q_restore_restores_previous_clip_mask() {
        let engine = ContentEngine::open_path(fixture("flate.pdf")).expect("open flate fixture");
        let viewport = Viewport::new([0.0, 0.0, 10.0, 10.0], 72);
        let mut buf = PixelBuffer::new_filled(10, 10, WHITE);
        let mut left_clip = ClipMask::all_visible(10, 10);
        left_clip.fill_rect(5, 0, 5, 10, false);
        buf.set_clip(left_clip);

        let mut state = RenderState::new(buf, viewport, PageResources::default(), &engine, 1);
        state.dispatch(&ContentOperation::new("q", Vec::new()));

        let mut top_clip = ClipMask::all_visible(10, 10);
        top_clip.fill_rect(0, 5, 10, 5, false);
        state.buf.set_clip(top_clip);
        state.dispatch(&ContentOperation::new("Q", Vec::new()));

        let clip = state.buf.clip_mask().expect("clip should be restored");
        assert!(
            clip.is_visible(2, 7),
            "left clip should restore bottom-left visibility"
        );
        assert!(
            !clip.is_visible(7, 2),
            "left clip should keep right side clipped"
        );
    }

    #[test]
    fn render_page_on_text_pdf_returns_non_trivial_buffer() {
        let engine = ContentEngine::open_path(fixture("flate.pdf")).expect("open flate fixture");
        let buf = engine.render_page(1, 72).expect("render page");
        assert_eq!(buf.width, 612);
        assert_eq!(buf.height, 792);
        let png = ImageEncoder::encode_png(&buf.to_raw_image()).expect("encode png");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn render_page_on_image_pdf_returns_modified_buffer() {
        let engine =
            ContentEngine::open_path(fixture("image_only.pdf")).expect("open image fixture");
        let buf = engine.render_page(1, 72).expect("render page");
        let any_non_white = (0..buf.height as i32)
            .flat_map(|y| (0..buf.width as i32).map(move |x| (x, y)))
            .any(|(x, y)| buf.get_pixel(x, y) != WHITE);
        assert!(any_non_white);
        let png = ImageEncoder::encode_png(&buf.to_raw_image()).expect("encode png");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(png.len() > 200);
    }

    #[test]
    fn render_page_invalid_page_returns_err() {
        let engine = ContentEngine::open_path(fixture("flate.pdf")).expect("open flate fixture");
        assert!(engine.render_page(999, 72).is_err());
    }

    #[test]
    fn fill_with_half_alpha_produces_semi_transparent_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let color = RenderColor::rgb(1.0, 0.0, 0.0)
            .with_alpha(0.5)
            .to_pixel_color();

        let mut path = Path::new();
        path.rect(20.0, 20.0, 60.0, 60.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, color, FillRule::NonZero);

        let center = buf.get_pixel(50, 50);
        println!("half-alpha red center pixel: {:?}", center);
        assert!(center[0] > 100);
        assert!(center[1] > 50 && center[1] < 255);
        assert!(center[2] > 50 && center[2] < 255);
    }

    #[test]
    fn fill_with_opaque_alpha_produces_opaque_pixels() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let color = RenderColor::rgb(1.0, 0.0, 0.0).to_pixel_color();

        let mut path = Path::new();
        path.rect(20.0, 20.0, 60.0, 60.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, color, FillRule::NonZero);

        let center = buf.get_pixel(50, 50);
        assert_eq!(center[0], 255);
        assert_eq!(center[1], 0);
        assert_eq!(center[2], 0);
    }

    #[test]
    fn fill_with_zero_alpha_leaves_buffer_unchanged() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);
        let color = RenderColor::rgb(1.0, 0.0, 0.0)
            .with_alpha(0.0)
            .to_pixel_color();

        let mut path = Path::new();
        path.rect(20.0, 20.0, 60.0, 60.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, color, FillRule::NonZero);

        assert_eq!(buf.get_pixel(50, 50), WHITE);
    }

    #[test]
    fn color_space_handler_respects_alpha_parameter() {
        let color = crate::content::state::Color {
            space: crate::content::state::ColorSpace::DeviceRGB,
            components: vec![1.0, 0.0, 0.0],
        };

        let full = ColorSpaceHandler::to_render_color(&color, 1.0);
        let half = ColorSpaceHandler::to_render_color(&color, 0.5);
        let zero = ColorSpaceHandler::to_render_color(&color, 0.0);

        assert_eq!(full.to_pixel_color()[3], 255);
        assert!((half.to_pixel_color()[3] as i32 - 128).abs() <= 1);
        assert_eq!(zero.to_pixel_color()[3], 0);
    }

    #[test]
    fn graphics_state_alpha_defaults_to_opaque() {
        let gs = GraphicsState::default();
        assert_eq!(gs.fill_alpha, 1.0);
        assert_eq!(gs.stroke_alpha, 1.0);
    }

    #[test]
    fn porter_duff_pixel_blend_matches_half_red_over_white() {
        // Half-red (rgb 128,0,0 at alpha 128/255≈0.502) over white, composited in
        // sRGB space (matches Poppler/Splash). Each channel mixes directly:
        // R = 0.502*128 + 0.498*255 ≈ 191; G = B = 0.502*0 + 0.498*255 ≈ 127.
        let mut buf = PixelBuffer::new_filled(1, 1, WHITE);
        buf.blend_pixel(0, 0, [128, 0, 0, 128], 1.0);
        let pixel = buf.get_pixel(0, 0);
        println!("porter-duff half-red pixel: {:?}", pixel);
        assert!((pixel[0] as i32 - 191).abs() <= 3, "R={}", pixel[0]);
        assert!((pixel[1] as i32 - 127).abs() <= 3, "G={}", pixel[1]);
        assert!((pixel[2] as i32 - 127).abs() <= 3, "B={}", pixel[2]);
    }

    #[test]
    fn alpha_composite_white_plus_half_red_is_pink() {
        // sRGB-space source-over (matches Poppler/Splash): the G/B channels land
        // at the sRGB midpoint 0.5, not the linear-light value ~0.735.
        let result = RenderColor::alpha_composite(
            RenderColor::white(),
            RenderColor::new(1.0, 0.0, 0.0, 0.5),
        );
        assert!((result.a - 1.0).abs() < 0.001);
        assert!((result.r - 1.0).abs() < 0.001);
        assert!((result.g - 0.5).abs() < 0.01, "g={}", result.g);
        assert!((result.b - 0.5).abs() < 0.01, "b={}", result.b);
    }

    #[test]
    fn blend_coverage_matches_buffer_blending_within_rounding() {
        let composited = RenderColor::alpha_composite(
            RenderColor::white(),
            RenderColor::new(1.0, 0.0, 0.0, 128.0 / 255.0),
        )
        .to_pixel_color();
        let mut buf = PixelBuffer::new_filled(1, 1, WHITE);
        buf.blend_pixel(0, 0, [255, 0, 0, 128], 1.0);
        let blended = buf.get_pixel(0, 0);

        assert!((blended[0] as i32 - composited[0] as i32).abs() <= 2);
        assert!((blended[1] as i32 - composited[1] as i32).abs() <= 2);
        assert!((blended[2] as i32 - composited[2] as i32).abs() <= 2);
    }

    #[test]
    fn transparent_stroke_over_fill_blends_border_only() {
        let vp = Viewport::new([0.0, 0.0, 100.0, 100.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(100, 100, WHITE);

        let mut path = Path::new();
        path.rect(20.0, 20.0, 60.0, 60.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, RED, FillRule::NonZero);

        let mut border = Path::new();
        border.rect(20.0, 20.0, 60.0, 60.0);
        let half_black = RenderColor::black().with_alpha(0.5).to_pixel_color();
        PathPainter::stroke(
            &mut buf,
            &border,
            &ctm,
            &vp,
            half_black,
            3.0,
            &DashState::solid(),
        );

        assert_eq!(buf.get_pixel(50, 50), RED);
        let border_pixel = buf.get_pixel(50, 20);
        assert!(border_pixel[0] < 255 || border_pixel[1] > 0 || border_pixel[2] > 0);
    }

    #[test]
    fn full_transparency_preserves_background() {
        let vp = Viewport::new([0.0, 0.0, 50.0, 50.0], 72);
        let ctm = Transform2D::identity();
        let mut buf = PixelBuffer::new_filled(50, 50, BLUE);
        let color = RenderColor::rgb(1.0, 0.0, 0.0)
            .with_alpha(0.0)
            .to_pixel_color();

        let mut path = Path::new();
        path.rect(5.0, 5.0, 40.0, 40.0);
        PathPainter::fill(&mut buf, &path, &ctm, &vp, color, FillRule::NonZero);

        for y in 0..50i32 {
            for x in 0..50i32 {
                assert_eq!(buf.get_pixel(x, y), BLUE);
            }
        }
    }

    // ── Form XObject helper tests ───────────────────────────────────────────

    fn dict_with(entries: &[(&str, PdfObject)]) -> PdfDictionary {
        PdfDictionary::new(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn extract_bbox_parses_valid_array() {
        let dict = dict_with(&[(
            "BBox",
            PdfObject::Array(vec![
                PdfObject::Real(0.0),
                PdfObject::Real(0.0),
                PdfObject::Real(100.0),
                PdfObject::Real(200.0),
            ]),
        )]);
        assert_eq!(extract_bbox(&dict).unwrap(), [0.0, 0.0, 100.0, 200.0]);
    }

    #[test]
    fn extract_bbox_accepts_integer_components() {
        let dict = dict_with(&[(
            "BBox",
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(50),
                PdfObject::Integer(50),
            ]),
        )]);
        assert_eq!(extract_bbox(&dict).unwrap(), [0.0, 0.0, 50.0, 50.0]);
    }

    #[test]
    fn extract_bbox_missing_returns_none() {
        assert!(extract_bbox(&PdfDictionary::empty()).is_none());
    }

    #[test]
    fn extract_bbox_short_array_returns_none() {
        let dict = dict_with(&[(
            "BBox",
            PdfObject::Array(vec![PdfObject::Real(0.0), PdfObject::Real(1.0)]),
        )]);
        assert!(extract_bbox(&dict).is_none());
    }

    #[test]
    fn extract_form_matrix_defaults_to_identity() {
        let m = extract_form_matrix(&PdfDictionary::empty());
        assert_eq!(m, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn extract_form_matrix_parses_translation() {
        let dict = dict_with(&[(
            "Matrix",
            PdfObject::Array(vec![
                PdfObject::Real(1.0),
                PdfObject::Real(0.0),
                PdfObject::Real(0.0),
                PdfObject::Real(1.0),
                PdfObject::Real(50.0),
                PdfObject::Real(100.0),
            ]),
        )]);
        let m = extract_form_matrix(&dict);
        assert_eq!(m[4], 50.0, "e (tx) = 50");
        assert_eq!(m[5], 100.0, "f (ty) = 100");
    }

    #[test]
    fn extract_form_matrix_short_array_falls_back_to_identity() {
        let dict = dict_with(&[(
            "Matrix",
            PdfObject::Array(vec![PdfObject::Real(2.0), PdfObject::Real(0.0)]),
        )]);
        assert_eq!(extract_form_matrix(&dict), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn merge_resources_form_xobjects_override_page() {
        let mut page_res = PageResources::default();
        page_res.xobjects.insert("X1".into(), (10, 0));
        page_res.xobjects.insert("X2".into(), (11, 0));

        let mut form_res = PageResources::default();
        form_res.xobjects.insert("X1".into(), (20, 0)); // overrides X1
        form_res.xobjects.insert("X3".into(), (30, 0)); // new

        let merged = merge_resources(form_res, &page_res);
        assert_eq!(merged.xobjects["X1"], (20, 0), "Form X1 overrides page X1");
        assert_eq!(merged.xobjects["X2"], (11, 0), "page X2 inherited");
        assert_eq!(merged.xobjects["X3"], (30, 0), "Form X3 added");
    }

    #[test]
    fn merge_resources_empty_form_yields_page_resources() {
        let mut page_res = PageResources::default();
        page_res.xobjects.insert("Im1".into(), (5, 0));
        page_res.fonts.insert("F1".into(), PdfDictionary::empty());
        let merged = merge_resources(PageResources::default(), &page_res);
        assert_eq!(merged.xobjects["Im1"], (5, 0));
        assert!(merged.fonts.contains_key("F1"));
    }

    #[test]
    fn merge_resources_form_font_overrides_page_font() {
        let mut page_res = PageResources::default();
        page_res
            .fonts
            .insert("F1".into(), dict_with(&[("Tag", PdfObject::Integer(1))]));

        let mut form_res = PageResources::default();
        form_res
            .fonts
            .insert("F1".into(), dict_with(&[("Tag", PdfObject::Integer(2))]));

        let merged = merge_resources(form_res, &page_res);
        assert_eq!(
            merged.fonts["F1"].get_integer("Tag"),
            Some(2),
            "Form font F1 should override page font F1"
        );
    }
}
