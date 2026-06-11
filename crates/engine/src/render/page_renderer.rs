use crate::content::operation::{ContentOperation, Operand};
use crate::content::state::{BlendMode, ColorSpace, GraphicsState};
use crate::engine::{ContentEngine, PageResources};
use crate::error::Result;
use crate::fonts::cmap::extract_to_unicode_map;
use crate::fonts::resolver::{
    detect_font_subtype, get_descendant_font, lookup_cid_width, FontSubtype,
};
use crate::fonts::FontResolver;
use crate::images::decoder::ImageDecoder;
use crate::images::locator::ImageReference;
use crate::object::{PdfDictionary, PdfObject};
use crate::render::buffer::{AlphaMask, ClipMask, PixelBuffer, PixelColor};
use crate::render::color::ColorSpaceHandler;
use crate::render::font_rasterizer::{get_fallback_font, FontRasterizer, GlyphToPath};
use crate::render::glyph_cache::{CachedGlyph, GlyphCache, GlyphCacheKey};
use crate::render::image_painter::ImagePainter;
use crate::render::line::DashState;
use crate::render::path::{flatten_path, FillRule, Path, PathPainter};
use crate::render::shading::ShadingRenderer;
use crate::render::transform::{Transform2D, Viewport};

pub struct PageRenderer;

impl PageRenderer {
    /// Render a single PDF page to a PixelBuffer at the given DPI.
    pub fn render_page(
        engine: &ContentEngine,
        page_number: usize,
        dpi: u32,
    ) -> Result<PixelBuffer> {
        let ops = engine.get_page_content(page_number)?;
        let viewport = engine.page_viewport(page_number, dpi)?;
        let buf = engine.create_page_buffer(page_number, dpi)?;
        let resources = engine.get_page_resources(page_number)?;

        let mut state = RenderState::new(buf, viewport, resources, engine, page_number);
        state.dispatch_all(&ops);
        Ok(state.into_buffer())
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
        }
    }

    fn into_buffer(self) -> PixelBuffer {
        self.buf
    }

    fn dispatch_all(&mut self, ops: &[ContentOperation]) {
        for op in ops {
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
                self.buf.blend_mode = self.gs.blend_mode;
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
            "BMC" | "BDC" | "EMC" | "MP" | "DP" | "BX" | "EX" | "BI" | "ID" | "EI"
            | "inline_image_data" => {}
            _ => self.gs.process(op),
        }
    }

    fn ctm(&self) -> Transform2D {
        Transform2D::from(self.gs.ctm)
    }

    fn fill_pixel_color(&self) -> PixelColor {
        ColorSpaceHandler::to_render_color(&self.gs.fill_color, self.gs.fill_alpha as f32)
            .to_pixel_color()
    }

    fn stroke_pixel_color(&self) -> PixelColor {
        ColorSpaceHandler::to_render_color(&self.gs.stroke_color, self.gs.stroke_alpha as f32)
            .to_pixel_color()
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
        PathPainter::stroke_with_cap(
            &mut self.buf,
            &self.path,
            &ctm,
            &self.viewport,
            color,
            width,
            &dash,
            &self.gs.line_cap,
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
        PathPainter::stroke_with_cap(
            &mut self.buf,
            &self.path,
            &ctm,
            &self.viewport,
            stroke,
            width,
            &dash,
            &self.gs.line_cap,
        );
        self.path.clear();
    }

    /// True when the current fill color space is the special Pattern space.
    fn is_pattern_fill(&self) -> bool {
        matches!(&self.gs.fill_color.space, ColorSpace::Named(name) if name == "Pattern")
    }

    fn apply_ext_g_state(&mut self, op: &ContentOperation) {
        let Some(name) = op.name(0) else {
            return;
        };
        if let Some(dict) = self.resources.ext_g_states.get(name).cloned() {
            self.gs.apply_ext_g_state(&dict);
            self.buf.blend_mode = self.gs.blend_mode;
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
        let s = smask_dict.get_name("S").unwrap_or("Luminosity");
        if s != "Luminosity" && s != "Alpha" {
            log::debug!(
                "PageRenderer: SMask /S '{}' is not supported; using luminosity",
                s
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

        let mut mask_buf =
            PixelBuffer::new_filled(self.buf.width, self.buf.height, crate::render::WHITE);
        mask_buf.blend_mode = mask_gs.blend_mode;

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
        self.buf.set_smask(AlphaMask::from_luminosity(&mask_buf));
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
        };

        match ImageDecoder::decode(&image_ref, self.engine.document().reader()) {
            Ok(raw) => {
                let ctm = self.ctm();
                ImagePainter::paint_image(&mut self.buf, &raw, &ctm, &self.viewport);
            }
            Err(err) => log::warn!("PageRenderer: image '{}' decode failed: {}", name, err),
        }
    }

    fn handle_do_form(&mut self, name: &str, obj_num: u32, gen_num: u16) {
        // Depth guard: prevent runaway recursion from malformed or cyclic PDFs.
        // TODO(cycle-detection): track object numbers on the stack to catch a
        // direct A->B->A cycle immediately instead of after 8 levels.
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
        // get_object already decrypts (Mega 18); decode_stream then decompresses.
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

        if form_dict.get("Group").is_some() {
            // Transparency groups need off-screen compositing (deferred to Mega 21).
            // Rendering directly onto the page buffer is correct for the common
            // non-transparent case.
            log::debug!(
                "PageRenderer: Form XObject '{}' has /Group (transparency) — rendering directly (TODO Mega 21)",
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
        self.form_depth += 1;

        // ── Step 4: Apply the Form matrix to the CTM ─────────────────────────
        // The Form /Matrix maps Form space → the user space in effect at the Do.
        // concat(self, other) applies `self` first then `other`, so to apply the
        // form matrix before the current CTM we compute form_matrix.concat(ctm).
        let current_ctm = Transform2D::from(self.gs.ctm);
        let form_t = Transform2D::from(form_matrix);
        self.gs.ctm = form_t.concat(&current_ctm).to_array();

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
                self.cleanup_after_form(saved_gs, saved_resources);
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
                self.cleanup_after_form(saved_gs, saved_resources);
                return;
            }
        };

        // ── Step 8: Render the Form's content stream ─────────────────────────
        self.dispatch_all(&ops);

        // ── Step 9: Restore the saved state ──────────────────────────────────
        self.cleanup_after_form(saved_gs, saved_resources);
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
        let mut group_buf = PixelBuffer::new_transparent(self.buf.width, self.buf.height);
        group_buf.blend_mode = self.gs.blend_mode;

        let form_matrix = extract_form_matrix(form_dict);
        let current_ctm = Transform2D::from(self.gs.ctm);
        let form_t = Transform2D::from(form_matrix);
        let mut group_gs = self.gs.clone();
        group_gs.ctm = form_t.concat(&current_ctm).to_array();

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
        };

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
        let group_buf = group_state.into_buffer();
        Self::composite_group(
            &mut self.buf,
            &group_buf,
            self.gs.fill_alpha as f32,
            self.gs.blend_mode,
        );
    }

    fn composite_group(
        dst: &mut PixelBuffer,
        src: &PixelBuffer,
        group_alpha: f32,
        blend_mode: BlendMode,
    ) {
        let old_blend_mode = dst.blend_mode;
        dst.blend_mode = blend_mode;
        let alpha = group_alpha.clamp(0.0, 1.0);
        for y in 0..src.height as i32 {
            for x in 0..src.width as i32 {
                let src_px = src.get_pixel(x, y);
                if src_px[3] == 0 {
                    continue;
                }
                dst.blend_pixel(x, y, src_px, alpha);
            }
        }
        dst.blend_mode = old_blend_mode;
    }

    /// Restore the graphics state, clip mask, and resources saved before a Form
    /// XObject was rendered, and decrement the depth counter.
    fn cleanup_after_form(&mut self, saved_gs: GraphicsState, saved_resources: PageResources) {
        self.form_depth = self.form_depth.saturating_sub(1);
        self.resources = saved_resources;
        self.gs = saved_gs;
        self.buf.blend_mode = self.gs.blend_mode;
        match self.clip_stack.pop() {
            Some(saved) => self.buf.restore_clip(saved),
            None => log::warn!("PageRenderer: Form cleanup with empty clip stack"),
        }
        match self.smask_stack.pop() {
            Some(saved) => self.buf.restore_smask(saved),
            None => log::warn!("PageRenderer: Form cleanup with empty SMask stack"),
        }
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
        ShadingRenderer::paint(&shading_dict, &ctm, &self.viewport, &mut self.buf, reader);
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
            1 => {
                // Tiling patterns require recursive content-stream rendering;
                // deferred. Skipping leaves the area unpainted (TODO).
                log::debug!("PatternType 1 (tiling) not yet implemented; skipping fill");
            }
            2 => self.paint_shading_pattern_fill(rule, &pattern_dict),
            other => log::debug!("pattern fill: unknown PatternType {other}"),
        }
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

        // The pattern carries its own /Matrix (pattern space → default user
        // space). Combine it with the current CTM for the shading geometry.
        let ctm = match get_float_array_dict(pattern_dict, "Matrix") {
            Some(m) if m.len() >= 6 => {
                let pat = Transform2D::from([m[0], m[1], m[2], m[3], m[4], m[5]]);
                pat.concat(&self.ctm())
            }
            _ => self.ctm(),
        };

        // Clip to the path being filled, intersected with the existing clip.
        let path_ctm = self.ctm();
        let flat = flatten_path(&self.path, &path_ctm, &self.viewport, 0.5);
        let path_clip = ClipMask::from_path(&flat, self.buf.width, self.buf.height, rule);
        let saved_clip = self.buf.clip_mask().cloned();
        self.buf.set_clip(path_clip); // intersects with any existing clip

        ShadingRenderer::paint(&shading_dict, &ctm, &self.viewport, &mut self.buf, reader);

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

        for glyph in decoded {
            let mut ttf_advance = None;
            if !matches!(self.gs.text.rendering_mode, 3 | 7) {
                if let (Some(font_bytes), Some(font_hash)) = (font_bytes.as_ref(), font_hash) {
                    if !font_bytes.is_empty() {
                        ttf_advance = self.render_glyph_with_cache(
                            glyph.code,
                            glyph.unicode,
                            glyph.is_gid,
                            font_bytes,
                            font_hash,
                            upem,
                        );
                    }
                }
            }
            let advance = glyph.width.or(ttf_advance).unwrap_or(500.0);
            self.advance_text(advance, glyph.is_space);
        }
    }

    fn render_glyph_with_cache(
        &mut self,
        code: u16,
        ch: char,
        is_gid: bool,
        font_bytes: &[u8],
        font_hash: u64,
        upem: f64,
    ) -> Option<f64> {
        let cache_key = GlyphCacheKey {
            font_hash,
            code,
            is_gid,
        };
        let cached = self.glyph_cache.get(&cache_key).cloned();
        let cached = match cached {
            Some(cached) => cached,
            None => {
                let (path, advance_width) = if is_gid {
                    Self::extract_glyph_path_by_gid(font_bytes, code)
                } else {
                    Self::extract_glyph_path(font_bytes, ch)
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

        let scale = font_size_scale(self.gs.text.font_size, upem);
        if scale <= 0.0 {
            return Some(advance_width);
        }

        let scale_t = Transform2D::scale(scale, scale);
        let tm_t = Transform2D::from(self.gs.text.tm);
        let ctm = self.ctm();
        let glyph_ctm = scale_t.concat(&tm_t).concat(&ctm);
        let fill_color = self.fill_pixel_color();
        let stroke_color = self.stroke_pixel_color();

        match self.gs.text.rendering_mode {
            0 | 4 => PathPainter::fill(
                &mut self.buf,
                &glyph_path,
                &glyph_ctm,
                &self.viewport,
                fill_color,
                FillRule::NonZero,
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
                PathPainter::fill(
                    &mut self.buf,
                    &glyph_path,
                    &glyph_ctm,
                    &self.viewport,
                    fill_color,
                    FillRule::NonZero,
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

    fn extract_glyph_path(font_bytes: &[u8], ch: char) -> (Option<Path>, f64) {
        let face = match ttf_parser::Face::parse(font_bytes, 0) {
            Ok(face) => face,
            Err(err) => {
                log::warn!(
                    "PageRenderer: extract_glyph_path font parse failed: {:?}",
                    err
                );
                return (None, 500.0);
            }
        };

        let upem = f64::from(face.units_per_em());
        let glyph_id = face
            .glyph_index(ch)
            .unwrap_or_else(|| ttf_parser::GlyphId(glyph_cache_code(ch).saturating_sub(1)));
        let advance = face
            .glyph_hor_advance(glyph_id)
            .map(|width| f64::from(width) / upem * 1000.0)
            .unwrap_or(500.0);

        let mut builder = GlyphToPath::new();
        if face.outline_glyph(glyph_id, &mut builder).is_none() {
            return (None, advance);
        }

        (Some(builder.into_path()), advance)
    }

    fn extract_glyph_path_by_gid(font_bytes: &[u8], gid: u16) -> (Option<Path>, f64) {
        let face = match ttf_parser::Face::parse(font_bytes, 0) {
            Ok(face) => face,
            Err(err) => {
                log::warn!(
                    "PageRenderer: extract_glyph_path_by_gid font parse failed: {:?}",
                    err
                );
                return (None, 500.0);
            }
        };

        let upem = f64::from(face.units_per_em());
        if upem <= 0.0 {
            return (None, 500.0);
        }
        let glyph_id = ttf_parser::GlyphId(gid);
        let advance = face
            .glyph_hor_advance(glyph_id)
            .map(|width| f64::from(width) / upem * 1000.0)
            .unwrap_or(1000.0);

        let mut builder = GlyphToPath::new();
        if face.outline_glyph(glyph_id, &mut builder).is_none() {
            return (None, advance);
        }

        (Some(builder.into_path()), advance)
    }

    fn get_upem(font_bytes: &[u8]) -> Option<u16> {
        ttf_parser::Face::parse(font_bytes, 0)
            .ok()
            .map(|face| face.units_per_em())
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
        let unicode_map = extract_to_unicode_map(font_dict, reader);
        let mut glyphs = Vec::new();
        let mut idx = 0usize;

        while idx + 1 < bytes.len() {
            let cid = (u16::from(bytes[idx]) << 8) | u16::from(bytes[idx + 1]);
            idx += 2;

            let unicode = unicode_map
                .as_ref()
                .and_then(|map| map.get(&u32::from(cid)))
                .copied()
                .unwrap_or('\u{FFFD}');
            let width = Some(
                descendant_font
                    .as_ref()
                    .map(|dict| lookup_cid_width(u32::from(cid), dict))
                    .unwrap_or(1000.0),
            )
            .filter(|width| *width > 0.0);
            let gid = cid_to_gid(cid, descendant_font.as_ref());

            glyphs.push(DecodedGlyph {
                code: gid,
                unicode,
                is_space: unicode == ' ' || cid == 0x0020,
                width,
                is_gid: true,
            });
        }

        glyphs
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
        let new_tm = Transform2D::from(self.gs.text.tm)
            .concat(&Transform2D::translation(tx, ty))
            .to_array();
        self.gs.text.tm = new_tm;
    }
}

struct DecodedGlyph {
    code: u16,
    unicode: char,
    is_space: bool,
    width: Option<f64>,
    is_gid: bool,
}

fn glyph_cache_code(ch: char) -> u16 {
    u16::try_from(ch as u32).unwrap_or(0xFFFD)
}

fn latin1_glyphs(bytes: &[u8]) -> Vec<DecodedGlyph> {
    bytes
        .iter()
        .map(|byte| DecodedGlyph {
            code: u16::from(*byte),
            unicode: decode_win_ansi(*byte),
            is_space: *byte == b' ',
            width: None,
            is_gid: false,
        })
        .collect()
}

fn cid_to_gid(cid: u16, desc_dict: Option<&PdfDictionary>) -> u16 {
    match desc_dict.and_then(|dict| dict.get("CIDToGIDMap")) {
        Some(PdfObject::Name(name)) if name == "Identity" => cid,
        Some(PdfObject::Stream { .. }) | Some(PdfObject::Reference { .. }) => {
            log::debug!("CIDToGIDMap stream is not implemented; using identity mapping");
            cid
        }
        _ => cid,
    }
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

fn font_size_scale(font_size: f64, upem: f64) -> f64 {
    if font_size <= 0.0 || upem <= 0.0 || !font_size.is_finite() || !upem.is_finite() {
        0.0
    } else {
        font_size / upem
    }
}

fn is_transparency_group(form_dict: &PdfDictionary) -> bool {
    matches!(
        form_dict.get("Group"),
        Some(PdfObject::Dictionary(group)) if group.get_name("S") == Some("Transparency")
    )
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

    #[test]
    fn type0_font_decodes_two_byte_strings() {
        let bytes = vec![0x00u8, 0x48, 0x00, 0x69];
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
        let (_path, advance) = RenderState::extract_glyph_path_by_gid(font_bytes, 0);
        assert!(advance > 0.0);
    }

    #[test]
    fn extract_glyph_path_by_gid_matches_char_lookup_for_ascii() {
        let font_bytes = get_fallback_font("Helvetica").expect("fallback font");
        let face = ttf_parser::Face::parse(font_bytes, 0).expect("parse fallback font");
        let gid_for_a = face.glyph_index('A').expect("glyph A").0;

        let (_path_by_char, adv_char) = RenderState::extract_glyph_path(font_bytes, 'A');
        let (_path_by_gid, adv_gid) = RenderState::extract_glyph_path_by_gid(font_bytes, gid_for_a);

        assert!((adv_char - adv_gid).abs() < 1.0);
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

    #[test]
    fn composite_group_with_full_alpha_paints_source() {
        let mut dst = PixelBuffer::new_filled(1, 1, WHITE);
        let mut src = PixelBuffer::new_transparent(1, 1);
        src.blend_pixel(0, 0, RED, 1.0);

        RenderState::composite_group(&mut dst, &src, 1.0, BlendMode::Normal);
        let result = dst.get_pixel(0, 0);
        assert!(result[0] > 200, "group should paint red: {:?}", result);
        assert!(result[1] < 50, "green channel should be low: {:?}", result);
    }

    #[test]
    fn composite_group_with_half_alpha_blends_with_destination() {
        let mut dst = PixelBuffer::new_filled(1, 1, WHITE);
        let mut src = PixelBuffer::new_transparent(1, 1);
        src.blend_pixel(0, 0, BLACK, 1.0);

        RenderState::composite_group(&mut dst, &src, 0.5, BlendMode::Normal);
        let result = dst.get_pixel(0, 0);
        assert!(
            result[0] > 100 && result[0] < 200,
            "50% black over white should be gray: {:?}",
            result
        );
        println!("composite_group 50% black over white: {:?}", result);
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
        let mut buf = PixelBuffer::new_filled(1, 1, WHITE);
        buf.blend_pixel(0, 0, [128, 0, 0, 128], 1.0);
        let pixel = buf.get_pixel(0, 0);
        println!("porter-duff half-red pixel: {:?}", pixel);
        assert!((pixel[0] as i32 - 191).abs() <= 3);
        assert!((pixel[1] as i32 - 128).abs() <= 3);
        assert!((pixel[2] as i32 - 128).abs() <= 3);
    }

    #[test]
    fn alpha_composite_white_plus_half_red_is_pink() {
        let result = RenderColor::alpha_composite(
            RenderColor::white(),
            RenderColor::new(1.0, 0.0, 0.0, 0.5),
        );
        assert!((result.a - 1.0).abs() < 0.001);
        assert!((result.r - 1.0).abs() < 0.001);
        assert!((result.g - 0.5).abs() < 0.001);
        assert!((result.b - 0.5).abs() < 0.001);
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

    // ── Form XObject helper tests (Mega 19) ─────────────────────────────────

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
