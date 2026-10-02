use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewCmd {
    Fit,
    Actual,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Handle {
    Corner(usize),
    Inside,
    Rotate,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DragKind {
    Stroke,
    Select,
    Lasso,
    Move,
    Gradient,
    Xform(Handle),
}

/// In-progress free transform of one layer.
#[derive(Clone)]
pub(crate) struct Xform {
    pub(crate) layer: LayerId,
    pub(crate) bounds: Rect,
    pub(crate) scale: f32,
    pub(crate) angle: f32,
    pub(crate) dx: f32,
    pub(crate) dy: f32,
    // values at the start of the current drag
    pub(crate) base: (f32, f32, f32, f32),
    pub(crate) last_preview: Rect,
}

impl Xform {
    pub(crate) fn center(&self) -> (f32, f32) {
        (
            self.bounds.x as f32 + self.bounds.w as f32 / 2.0,
            self.bounds.y as f32 + self.bounds.h as f32 / 2.0,
        )
    }

    pub(crate) fn affine(&self) -> Affine {
        let (cx, cy) = self.center();
        Affine::around(cx, cy, self.scale, self.scale, self.angle).then(&Affine::translate(self.dx, self.dy))
    }

    /// Transformed corners, in document space.
    pub(crate) fn corners(&self) -> [(f32, f32); 4] {
        let a = self.affine();
        let b = self.bounds;
        [
            a.apply(b.x as f32, b.y as f32),
            a.apply(b.right() as f32, b.y as f32),
            a.apply(b.right() as f32, b.bottom() as f32),
            a.apply(b.x as f32, b.bottom() as f32),
        ]
    }

    pub(crate) fn bbox(&self) -> Rect {
        self.affine().transform_rect(self.bounds)
    }
}

impl App {
    // ---- rendering ---------------------------------------------------------------------

    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        let mut below = std::mem::take(&mut self.below);
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let partial_ok = self
            .last_flat
            .as_ref()
            .is_some_and(|f| f.width == doc.width && f.height == doc.height)
            && self.canvas_tex.is_some();
        match self.dirty_rect.take() {
            Some(r) if partial_ok => {
                let r = r.intersect(&canvas);
                if !r.is_empty() {
                    let patch = below.composite_rect(doc, r).to_raster(r);
                    if let Some(flat) = self.last_flat.as_mut() {
                        for y in 0..r.h {
                            for x in 0..r.w {
                                flat.set(r.x as u32 + x, r.y as u32 + y, patch.get(x, y));
                            }
                        }
                    }
                    if let Some(tex) = self.canvas_tex.as_mut() {
                        tex.set_partial(
                            [r.x as usize, r.y as usize],
                            raster_to_image(&patch),
                            nearest_when_zoomed(),
                        );
                    }
                }
                self.refresh_thumbs(ctx, Some(r));
            }
            _ => {
                let flat = below.composite_rect(doc, canvas).to_raster(canvas);
                let img = raster_to_image(&flat);
                self.last_flat = Some(flat);
                upload(&mut self.canvas_tex, ctx, "canvas", img, nearest_when_zoomed());
                match selection_overlay(self.editor.doc()) {
                    Some(img) => upload(
                        &mut self.overlay_tex,
                        ctx,
                        "selection",
                        img,
                        egui::TextureOptions::NEAREST,
                    ),
                    None => self.overlay_tex = None,
                }
                self.refresh_thumbs(ctx, None);
            }
        }
        self.below = below;
        self.update_histogram();
        self.capture_history_thumb(ctx);
        self.dirty = false;
    }

    pub(crate) fn refresh_thumbs(&mut self, ctx: &egui::Context, area: Option<Rect>) {
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let mut thumbs: Vec<(LayerId, egui::ColorImage)> = Vec::new();
        let mut masks: Vec<(LayerId, egui::ColorImage)> = Vec::new();
        // A group thumbnail needs a full composite of its children, which is
        // the expensive part: on a partial refresh only rebuild it when the
        // changed area could touch the group's content.
        fn group_dirty(children: &[Layer], r: Rect) -> bool {
            children.iter().any(|l| match &l.content {
                LayerContent::Pixel(store) => store.bounds().is_none_or(|b| !b.intersect(&r).is_empty()),
                LayerContent::Group(c) => group_dirty(c, r),
                _ => true,
            })
        }
        doc.for_each_layer(|l| {
            match &l.content {
                LayerContent::Pixel(store) => {
                    thumbs.push((l.id, thumb_image(canvas, |x, y| store.get_pixel(x, y))))
                }
                LayerContent::Group(children) if area.is_none_or(|r| group_dirty(children, r)) => {
                    let flat = nge_render::composite_layers(children, canvas, canvas);
                    thumbs.push((l.id, thumb_image(canvas, |x, y| flat.get_pixel(x, y))));
                }
                _ => {}
            }
            if let Some(m) = &l.mask {
                masks.push((
                    l.id,
                    thumb_image(canvas, |x, y| {
                        let v = m.value(x, y);
                        nge_tiles::Rgba::new(v, v, v, 1.0)
                    }),
                ));
            }
        });
        let live: Vec<LayerId> = {
            let mut v = Vec::new();
            doc.for_each_layer(|l| v.push(l.id));
            v
        };
        for (id, img) in thumbs {
            let slot = self.thumbs.entry(id);
            match slot {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.get_mut().set(img, egui::TextureOptions::LINEAR)
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(ctx.load_texture(format!("thumb-{id}"), img, egui::TextureOptions::LINEAR));
                }
            }
        }
        for (id, img) in masks {
            match self.mask_thumbs.entry(id) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.get_mut().set(img, egui::TextureOptions::LINEAR)
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(ctx.load_texture(format!("mask-{id}"), img, egui::TextureOptions::LINEAR));
                }
            }
        }
        self.thumbs.retain(|id, _| live.contains(id));
        let doc = self.editor.doc();
        self.mask_thumbs
            .retain(|id, _| doc.layer(*id).is_some_and(|l| l.mask.is_some()));
    }

    /// Upload a preview of `doc` for the given area (or the whole canvas).
    pub(crate) fn preview(&mut self, ctx: &egui::Context, doc: &Document, area: Option<Rect>) {
        // Previews change only the active layer, so the cached backdrop
        // below it applies to the preview document too.
        let mut below = std::mem::take(&mut self.below);
        below.note_change(doc, self.active);
        match (area, self.canvas_tex.as_mut()) {
            (Some(r), Some(tex)) => {
                let r = r.intersect(&doc.canvas());
                if !r.is_empty() {
                    let patch = below.composite_rect(doc, r).to_raster(r);
                    tex.set_partial(
                        [r.x as usize, r.y as usize],
                        raster_to_image(&patch),
                        nearest_when_zoomed(),
                    );
                }
            }
            _ => {
                let canvas = doc.canvas();
                let img = raster_to_image(&below.composite_rect(doc, canvas).to_raster(canvas));
                upload(&mut self.canvas_tex, ctx, "canvas", img, nearest_when_zoomed());
            }
        }
        self.below = below;
    }

    // ---- canvas ---------------------------------------------------------------------

    pub(crate) fn zoom_at(&mut self, rect: egui::Rect, p: Pos2, factor: f32) {
        let old = self.zoom;
        let new = (old * factor).clamp(0.05, 32.0);
        let origin = rect.min + self.pan;
        let doc_pt = (p - origin) / old;
        self.zoom = new;
        self.pan = p - rect.min - doc_pt * new;
    }

    pub(crate) fn apply_view_cmd(&mut self, rect: egui::Rect) {
        let Some(cmd) = self.view_cmd.take() else { return };
        let doc = self.editor.doc();
        let size = Vec2::new(doc.width as f32, doc.height as f32);
        self.zoom = match cmd {
            ViewCmd::Fit => ((rect.width() - 80.0) / size.x)
                .min((rect.height() - 80.0) / size.y)
                .clamp(0.05, 32.0),
            ViewCmd::Actual => 1.0,
        };
        self.pan = (rect.size() - size * self.zoom) / 2.0;
    }

    pub(crate) fn canvas(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
                let rect = resp.rect;
                let painter = painter.with_clip_rect(rect);
                self.apply_view_cmd(rect);

                // Scroll pans; Alt+scroll (or a pinch) zooms at the cursor.
                let space = !ctx.wants_keyboard_input() && ctx.input(|i| i.key_down(Key::Space));
                if resp.hovered() {
                    let (scroll, pinch, alt) =
                        ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.alt));
                    let factor = if alt {
                        (scroll.y * 0.004).exp() * pinch
                    } else {
                        pinch
                    };
                    if (factor - 1.0).abs() > 1e-4 {
                        if let Some(p) = resp.hover_pos() {
                            self.zoom_at(rect, p, factor);
                        }
                    } else if !alt && scroll != Vec2::ZERO {
                        self.pan += scroll;
                    }
                    if space {
                        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                }
                if resp.dragged_by(egui::PointerButton::Middle)
                    || (space && resp.dragged())
                    || (self.tool == Tool::Hand && resp.dragged_by(egui::PointerButton::Primary))
                {
                    self.pan += resp.drag_delta();
                }

                let (dw, dh) = (self.editor.doc().width as f32, self.editor.doc().height as f32);
                let origin = rect.min + self.pan;
                let zoom = self.zoom;
                let doc_rect = egui::Rect::from_min_size(origin, Vec2::new(dw, dh) * zoom);
                let to_doc = move |p: Pos2| ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom);
                let to_screen = move |x: f32, y: f32| egui::pos2(origin.x + x * zoom, origin.y + y * zoom);

                self.cursor_doc = resp.hover_pos().and_then(|p| {
                    let (x, y) = to_doc(p);
                    (x >= 0.0 && y >= 0.0 && x < dw && y < dh).then_some((x as i32, y as i32))
                });

                // While Space pans, the active tool must not also fire.
                if self.xform.is_some() {
                    self.handle_xform(ctx, &resp, to_doc, to_screen);
                } else if !space {
                    self.handle_tool(ctx, &resp, to_doc);
                }

                // A soft drop shadow under the document.
                for (grow, alpha) in [(14.0, 22), (8.0, 36), (3.0, 60)] {
                    painter.rect_filled(
                        doc_rect.translate(egui::vec2(0.0, grow * 0.5)).expand(grow),
                        grow,
                        Color32::from_black_alpha(alpha),
                    );
                }
                paint_checker(&painter, doc_rect, rect);
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                if let Some(tex) = &self.canvas_tex {
                    painter.image(tex.id(), doc_rect, uv, Color32::WHITE);
                }
                if let Some(tex) = &self.overlay_tex {
                    painter.image(tex.id(), doc_rect, uv, Color32::WHITE);
                }
                painter.rect_stroke(doc_rect, 0.0, Stroke::new(1.0, LINE));

                if let Some(x) = &self.xform {
                    paint_xform_box(&painter, x, to_screen);
                } else {
                    self.paint_tool_overlay(ctx, &painter, &resp);
                }
                self.selection_action_bar(ctx, rect, origin, zoom);
                self.zoom_pill(ctx, rect);
            });
    }

    /// A floating zoom control in the canvas corner: − / percentage / + / Fit.
    fn zoom_pill(&mut self, ctx: &egui::Context, clip: egui::Rect) {
        egui::Area::new("zoom-pill".into())
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(clip.right_bottom() - egui::vec2(14.0, 14.0))
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(RAISED)
                    .rounding(8.0)
                    .stroke(Stroke::new(1.0, LINE))
                    .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if ui.add(egui::Button::new("−").frame(false)).clicked() {
                                self.zoom_at(clip, clip.center(), 1.0 / 1.25);
                            }
                            ui.add_sized(
                                [52.0, 18.0],
                                egui::Label::new(
                                    RichText::new(format!("{:.0}%", self.zoom * 100.0))
                                        .monospace()
                                        .color(TEXT),
                                ),
                            );
                            if ui.add(egui::Button::new("+").frame(false)).clicked() {
                                self.zoom_at(clip, clip.center(), 1.25);
                            }
                            ui.separator();
                            if ui
                                .add(egui::Button::new(RichText::new("Fit").color(MUTED)).frame(false))
                                .clicked()
                            {
                                self.view_cmd = Some(ViewCmd::Fit);
                            }
                        });
                    });
            });
    }

    /// A floating bar of selection actions just below the active selection.
    fn selection_action_bar(&mut self, ctx: &egui::Context, clip: egui::Rect, origin: Pos2, zoom: f32) {
        if self.xform.is_some() || self.drag.is_some() {
            return;
        }
        let doc = self.editor.doc();
        let Some(sel) = &doc.selection else { return };
        let b = sel.bounds_within(doc.canvas());
        if b.is_empty() {
            return;
        }
        let cx = origin.x + (b.x as f32 + b.w as f32 / 2.0) * zoom;
        let cy = origin.y + (b.y as f32 + b.h as f32) * zoom + 12.0;
        let pos = egui::pos2(
            (cx - 140.0).clamp(clip.min.x + 8.0, (clip.max.x - 288.0).max(clip.min.x + 8.0)),
            cy.clamp(clip.min.y + 8.0, clip.max.y - 44.0),
        );
        let can_mask = self.active.is_some() && !self.active_has_mask();
        let mut act: Option<&'static str> = None;
        egui::Area::new("sel-actions".into())
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(RAISED)
                    .rounding(8.0)
                    .stroke(Stroke::new(1.0, LINE))
                    .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(can_mask, egui::Button::new("Mask"))
                                .on_hover_text("Mask the active layer with this selection")
                                .clicked()
                            {
                                act = Some("add-mask");
                            }
                            if ui
                                .add_enabled(self.active_is_pixel(), egui::Button::new("Fill"))
                                .on_hover_text("Fill the selection with the brush colour")
                                .clicked()
                            {
                                act = Some("fill");
                            }
                            if ui
                                .add_enabled(self.active_is_pixel(), egui::Button::new("Clear"))
                                .clicked()
                            {
                                act = Some("clear");
                            }
                            if ui
                                .add_enabled(self.active_is_pixel(), egui::Button::new("New layer"))
                                .on_hover_text("Copy the selected pixels onto a new layer")
                                .clicked()
                            {
                                act = Some("layer-via-copy");
                            }
                        });
                    });
            });
        if let Some(a) = act {
            self.run_menu_action(a);
        }
    }

    pub(crate) fn handle_xform(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
        to_screen: impl Fn(f32, f32) -> Pos2,
    ) {
        let primary = egui::PointerButton::Primary;
        let Some(mut x) = self.xform.clone() else { return };
        let (cx, cy) = x.center();
        let centre_s = to_screen(cx + x.dx, cy + x.dy);

        let hit = |q: Pos2| -> Handle {
            for (i, (px, py)) in x.corners().iter().enumerate() {
                if to_screen(*px, *py).distance(q) <= 10.0 {
                    return Handle::Corner(i);
                }
            }
            let poly: Vec<Pos2> = x.corners().iter().map(|(px, py)| to_screen(*px, *py)).collect();
            if point_in_convex(&poly, q) {
                Handle::Inside
            } else {
                Handle::Rotate
            }
        };
        if let Some(q) = resp.hover_pos() {
            ctx.set_cursor_icon(match hit(q) {
                Handle::Corner(_) => egui::CursorIcon::ResizeNwSe,
                Handle::Inside => egui::CursorIcon::Move,
                Handle::Rotate => egui::CursorIcon::Alias,
            });
        }
        if resp.drag_started_by(primary) {
            if let Some(q) = ctx.input(|i| i.pointer.press_origin()) {
                self.drag = Some(DragKind::Xform(hit(q)));
                self.drag_start = Some(q);
                x.base = (x.scale, x.angle, x.dx, x.dy);
            }
        }
        let mut changed = false;
        if let (Some(DragKind::Xform(h)), true) = (self.drag, resp.dragged_by(primary)) {
            if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                match h {
                    Handle::Corner(_) => {
                        let d0 = a.distance(centre_s).max(1.0);
                        let d1 = b.distance(centre_s);
                        x.scale = (x.base.0 * d1 / d0).clamp(0.02, 50.0);
                    }
                    Handle::Inside => {
                        let (ax, ay) = to_doc(a);
                        let (bx, by) = to_doc(b);
                        x.dx = x.base.2 + (bx - ax);
                        x.dy = x.base.3 + (by - ay);
                    }
                    Handle::Rotate => {
                        let a0 = (a.y - centre_s.y).atan2(a.x - centre_s.x);
                        let a1 = (b.y - centre_s.y).atan2(b.x - centre_s.x);
                        x.angle = x.base.1 + (a1 - a0);
                    }
                }
                changed = true;
            }
        }
        if resp.drag_stopped() {
            if let Some(DragKind::Xform(_)) = self.drag {
                self.drag = None;
                self.drag_start = None;
            }
        }
        if changed {
            // Preview: transform a copy-on-write clone and redraw the union of the old and new boxes.
            let mut preview = self.editor.doc().clone();
            let cmd = TransformLayer {
                layer: x.layer,
                transform: x.affine(),
            };
            if cmd.apply(&mut preview).is_ok() {
                let area = x.last_preview.union(&x.bbox());
                let pad = Rect::new(area.x - 2, area.y - 2, area.w + 4, area.h + 4);
                self.preview(ctx, &preview, Some(pad));
                x.last_preview = x.bbox();
            }
        }
        self.xform = Some(x);
    }

    pub(crate) fn handle_tool(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        match self.tool {
            Tool::Hand => {
                if resp.hovered() {
                    ctx.set_cursor_icon(if resp.dragged() {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    });
                }
            }
            Tool::Eyedropper => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.clicked_by(primary) {
                    if let (Some(p), Some(flat)) = (resp.interact_pointer_pos(), &self.last_flat) {
                        let (x, y) = to_doc(p);
                        if x >= 0.0 && y >= 0.0 && (x as u32) < flat.width && (y as u32) < flat.height {
                            let [r, g, b, _] = flat.get(x as u32, y as u32).to_straight();
                            let enc = |v: f32| nge_io::linear_to_srgb(v) as f32 / 255.0;
                            self.brush_rgb = [enc(r), enc(g), enc(b)];
                            self.status = format!("Picked colour at {}, {}", x as i32, y as i32);
                        }
                    }
                }
            }
            Tool::Bucket => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.clicked_by(primary) {
                    if let (Some(layer), Some(p), true) =
                        (self.active, resp.interact_pointer_pos(), self.active_is_pixel())
                    {
                        let (x, y) = to_doc(p);
                        let sample = if self.sample_merged {
                            SampleSource::Merged
                        } else {
                            SampleSource::Layer(layer)
                        };
                        self.run(&BucketFill {
                            layer,
                            x: x.floor() as i32,
                            y: y.floor() as i32,
                            color: linear_rgba(self.brush_rgb, self.brush.color[3]),
                            tolerance: self.tolerance,
                            contiguous: self.contiguous,
                            sample,
                        });
                    } else if !self.active_is_pixel() {
                        self.status = "Select a pixel layer to fill".into();
                    }
                }
            }
            Tool::Wand => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        let sample = match (self.sample_merged, self.active, self.active_is_pixel()) {
                            (false, Some(layer), true) => SampleSource::Layer(layer),
                            _ => SampleSource::Merged,
                        };
                        let mods = ctx.input(|i| i.modifiers);
                        let op = if mods.shift {
                            CombineOp::Union
                        } else if mods.alt {
                            CombineOp::Subtract
                        } else {
                            self.select_op
                        };
                        self.run(&MagicWandSelect {
                            x: x.floor() as i32,
                            y: y.floor() as i32,
                            tolerance: self.tolerance,
                            contiguous: self.contiguous,
                            sample,
                            op,
                        });
                    }
                }
            }
            Tool::Gradient => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.drag_started_by(primary) {
                    if self.active_is_pixel() {
                        self.drag = Some(DragKind::Gradient);
                        self.drag_start = ctx.input(|i| i.pointer.press_origin());
                    } else {
                        self.status = "Select a pixel layer for the gradient".into();
                    }
                }
                if self.drag == Some(DragKind::Gradient) && resp.dragged_by(primary) {
                    if let (Some(a), Some(b), Some(layer)) =
                        (self.drag_start, resp.interact_pointer_pos(), self.active)
                    {
                        let cmd = self.gradient_command(layer, to_doc(a), to_doc(b));
                        let mut preview = self.editor.doc().clone();
                        if cmd.apply(&mut preview).is_ok() {
                            let area = cmd.affected(self.editor.doc());
                            self.preview(ctx, &preview, area);
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Gradient) {
                    self.drag = None;
                    let start = self.drag_start.take();
                    let end = resp
                        .interact_pointer_pos()
                        .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
                    if let (Some(a), Some(b), Some(layer)) = (start, end, self.active) {
                        if a.distance(b) >= 2.0 {
                            let cmd = self.gradient_command(layer, to_doc(a), to_doc(b));
                            self.run(&cmd);
                        } else {
                            self.mark(None);
                        }
                    }
                }
            }
            Tool::Text => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Text);
                }
                if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        let mut t =
                            TextLayer::new("Text", x, y, self.text_size, linear_rgba(self.brush_rgb, 1.0));
                        t.bold = self.text_bold;
                        let new_id = self.editor.doc().next_id();
                        self.run(&AddTextLayer {
                            text: t,
                            above: self.active,
                        });
                        self.set_active(Some(new_id));
                        self.fix_active();
                        self.status = "Text added; edit it in the options bar or Properties".into();
                    }
                }
            }
            Tool::Move => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Move);
                }
                if resp.drag_started_by(primary) {
                    if self.active_is_pixel() || self.active_is_text() {
                        self.drag = Some(DragKind::Move);
                        self.drag_start = ctx.input(|i| i.pointer.press_origin());
                        self.move_offset = (0, 0);
                    } else {
                        self.status = "Select a pixel layer to move".into();
                    }
                }
                if self.drag == Some(DragKind::Move) && resp.dragged_by(primary) {
                    if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                        let d = (b - a) / self.zoom;
                        let off = (d.x.round() as i32, d.y.round() as i32);
                        if off != self.move_offset {
                            let prev = self.move_offset;
                            self.move_offset = off;
                            if let Some(layer) = self.active {
                                // Repaint only where the layer was and where
                                // it lands, not the whole canvas.
                                let bounds = self
                                    .editor
                                    .doc()
                                    .layer(layer)
                                    .and_then(|l| l.pixels())
                                    .and_then(|s| s.bounds());
                                let mut preview = self.editor.doc().clone();
                                let cmd = MoveLayer {
                                    layer,
                                    dx: off.0,
                                    dy: off.1,
                                };
                                if cmd.apply(&mut preview).is_ok() {
                                    let area = bounds.map(|b| {
                                        let at = |o: (i32, i32)| {
                                            Rect::new(
                                                b.x.saturating_add(o.0),
                                                b.y.saturating_add(o.1),
                                                b.w,
                                                b.h,
                                            )
                                        };
                                        at(prev).union(&at(off))
                                    });
                                    self.preview(ctx, &preview, area);
                                }
                            }
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Move) {
                    self.drag = None;
                    self.drag_start = None;
                    let (dx, dy) = self.move_offset;
                    if let (Some(layer), true) = (self.active, dx != 0 || dy != 0) {
                        self.run(&MoveLayer { layer, dx, dy });
                    } else {
                        // The drag went nowhere; drop any preview left on the texture.
                        self.mark(None);
                    }
                }
            }
            Tool::Brush | Tool::Eraser | Tool::Clone => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if self.tool == Tool::Clone {
                    let alt = ctx.input(|i| i.modifiers.alt);
                    let wants_source = self.clone_picking || self.clone_source.is_none() || alt;
                    if wants_source {
                        if resp.clicked_by(primary) || (alt && resp.drag_started_by(primary)) {
                            if let Some(p) = ctx
                                .input(|i| i.pointer.press_origin())
                                .or(resp.interact_pointer_pos())
                            {
                                self.clone_source = Some(to_doc(p));
                                self.clone_picking = false;
                                self.status = "Clone source set; now paint where the copy should go".into();
                            }
                        }
                        return;
                    }
                }
                let paintable = if self.editing_mask {
                    self.active_has_mask()
                } else {
                    self.active_is_pixel()
                };
                if resp.drag_started_by(primary) {
                    if paintable {
                        if self.tool == Tool::Clone {
                            if let (Some((sx, sy)), Some(p)) =
                                (self.clone_source, ctx.input(|i| i.pointer.press_origin()))
                            {
                                let (dx, dy) = to_doc(p);
                                self.clone_offset = ((sx - dx).round() as i32, (sy - dy).round() as i32);
                            }
                        }
                        self.drag = Some(DragKind::Stroke);
                        self.stroke.clear();
                        self.stroke_drawn = 0;
                        if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                            let (x, y) = to_doc(p);
                            self.stroke.push(StrokePoint::new(x, y, 1.0));
                        }
                    } else {
                        self.status = if self.editing_mask {
                            "The active layer has no mask to paint".into()
                        } else {
                            "Select a pixel layer to paint on".into()
                        };
                    }
                }
                if self.drag == Some(DragKind::Stroke) && resp.dragged_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        self.stroke.push(StrokePoint::new(x, y, 1.0));
                    }
                    if let (Some(layer), false) = (self.active, self.stroke.is_empty()) {
                        let cmd = self.stroke_command(layer, self.stroke.clone());
                        let mut preview = self.editor.doc().clone();
                        if cmd.apply(&mut preview).is_ok() {
                            // Catmull-Rom smoothing can bend the curve up to
                            // two points back, so repaint from there instead
                            // of the whole stroke: long strokes stay cheap.
                            let from = self.stroke_drawn.saturating_sub(3);
                            let area =
                                stroke_bounds(&self.make_brush(), &self.stroke[from..], preview.canvas());
                            self.preview(ctx, &preview, Some(area));
                            self.stroke_drawn = self.stroke.len();
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Stroke) {
                    self.drag = None;
                    if let Some(layer) = self.active {
                        if !self.stroke.is_empty() {
                            let pts = std::mem::take(&mut self.stroke);
                            let cmd = self.stroke_command(layer, pts);
                            self.run(cmd.as_ref());
                        }
                    }
                } else if resp.clicked_by(primary) && paintable {
                    if let (Some(layer), Some(p)) = (self.active, resp.interact_pointer_pos()) {
                        let (x, y) = to_doc(p);
                        if self.tool == Tool::Clone {
                            // A single dab locks its own offset, same as a
                            // drag; reusing the previous stroke's offset
                            // would clone from the wrong place.
                            if let Some((sx, sy)) = self.clone_source {
                                self.clone_offset = ((sx - x).round() as i32, (sy - y).round() as i32);
                            }
                        }
                        let cmd = self.stroke_command(layer, vec![StrokePoint::new(x, y, 1.0)]);
                        self.run(cmd.as_ref());
                    }
                }
            }
            Tool::Lasso => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.drag_started_by(primary) {
                    self.drag = Some(DragKind::Lasso);
                    self.lasso.clear();
                    if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                        self.lasso.push(to_doc(p));
                    }
                }
                if self.drag == Some(DragKind::Lasso) && resp.dragged_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let q = to_doc(p);
                        let far = self
                            .lasso
                            .last()
                            .is_none_or(|l| (l.0 - q.0).abs() + (l.1 - q.1).abs() > 0.5);
                        if far {
                            self.lasso.push(q);
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Lasso) {
                    self.drag = None;
                    self.finish_polygon(ctx);
                } else if resp.clicked_by(primary) {
                    self.run(&SetSelection { selection: None });
                }
            }
            Tool::PolyLasso => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.double_clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.lasso.push(to_doc(p));
                    }
                    self.finish_polygon(ctx);
                } else if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let q = to_doc(p);
                        // Clicking near the first point closes the polygon.
                        if self.lasso.len() >= 3 {
                            let f = self.lasso[0];
                            if ((f.0 - q.0).powi(2) + (f.1 - q.1).powi(2)).sqrt() * self.zoom < 8.0 {
                                self.finish_polygon(ctx);
                                return;
                            }
                        }
                        self.lasso.push(q);
                    }
                }
                if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Escape)) {
                    self.lasso.clear();
                }
            }
            Tool::RectSelect | Tool::EllipseSelect => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.drag_started_by(primary) {
                    self.drag = Some(DragKind::Select);
                    self.drag_start = ctx.input(|i| i.pointer.press_origin());
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Select) {
                    self.drag = None;
                    let press = self.drag_start.take();
                    let release = resp
                        .interact_pointer_pos()
                        .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
                    if let (Some(a), Some(b)) = (press, release) {
                        let canvas = self.editor.doc().canvas();
                        let r = drag_rect(to_doc(a), to_doc(b), canvas);
                        let mods = ctx.input(|i| i.modifiers);
                        let op = if mods.shift && mods.alt {
                            CombineOp::Intersect
                        } else if mods.shift {
                            CombineOp::Union
                        } else if mods.alt {
                            CombineOp::Subtract
                        } else {
                            self.select_op
                        };
                        if r.w < 2 || r.h < 2 {
                            self.run(&SetSelection { selection: None });
                        } else {
                            let shape = if self.tool == Tool::RectSelect {
                                Selection::rect(r)
                            } else {
                                Selection::ellipse(r)
                            };
                            self.run(&ModifySelection { shape, op });
                        }
                    }
                } else if resp.clicked_by(primary) {
                    self.run(&SetSelection { selection: None });
                }
            }
        }
    }

    pub(crate) fn paint_tool_overlay(
        &self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        resp: &egui::Response,
    ) {
        match self.tool {
            Tool::Brush | Tool::Eraser | Tool::Clone => {
                if let (true, Some(p)) = (
                    self.drag.is_none() || self.drag == Some(DragKind::Stroke),
                    resp.hover_pos(),
                ) {
                    let r = (self.brush.radius * self.zoom).max(1.5);
                    painter.circle_stroke(p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
                    painter.circle_stroke(p, r, Stroke::new(1.0, Color32::WHITE));
                    if self.tool == Tool::Clone {
                        if let Some((sx, sy)) = self.clone_source {
                            let origin = resp.rect.min + self.pan;
                            let sp = if self.drag == Some(DragKind::Stroke) {
                                // While stroking the source follows the brush at the locked offset.
                                p + egui::vec2(self.clone_offset.0 as f32, self.clone_offset.1 as f32)
                                    * self.zoom
                            } else {
                                egui::pos2(origin.x + sx * self.zoom, origin.y + sy * self.zoom)
                            };
                            painter.circle_stroke(sp, r, Stroke::new(1.0, Color32::from_rgb(255, 170, 60)));
                            painter.line_segment(
                                [sp + egui::vec2(-6.0, 0.0), sp + egui::vec2(6.0, 0.0)],
                                Stroke::new(1.0, Color32::from_rgb(255, 170, 60)),
                            );
                            painter.line_segment(
                                [sp + egui::vec2(0.0, -6.0), sp + egui::vec2(0.0, 6.0)],
                                Stroke::new(1.0, Color32::from_rgb(255, 170, 60)),
                            );
                        }
                    }
                }
            }
            Tool::RectSelect | Tool::EllipseSelect => {
                if self.drag == Some(DragKind::Select) {
                    let press = self.drag_start;
                    let cur = ctx.input(|i| i.pointer.latest_pos());
                    if let (Some(a), Some(b)) = (press, cur) {
                        let r = egui::Rect::from_two_pos(a, b);
                        let pts: Vec<Pos2> = if self.tool == Tool::RectSelect {
                            vec![
                                r.left_top(),
                                r.right_top(),
                                r.right_bottom(),
                                r.left_bottom(),
                                r.left_top(),
                            ]
                        } else {
                            ellipse_points(r, 96)
                        };
                        painter.add(Shape::line(pts.clone(), Stroke::new(1.0, Color32::WHITE)));
                        painter.extend(Shape::dashed_line(
                            &pts,
                            Stroke::new(1.0, Color32::BLACK),
                            5.0,
                            5.0,
                        ));
                    }
                }
            }
            Tool::Lasso | Tool::PolyLasso => {
                if !self.lasso.is_empty() {
                    let origin = resp.rect.min + self.pan;
                    let zoom = self.zoom;
                    let mut pts: Vec<Pos2> = self
                        .lasso
                        .iter()
                        .map(|(x, y)| egui::pos2(origin.x + x * zoom, origin.y + y * zoom))
                        .collect();
                    if self.tool == Tool::PolyLasso {
                        if let Some(cur) = ctx.input(|i| i.pointer.latest_pos()) {
                            pts.push(cur);
                        }
                    }
                    let mut closed = pts.clone();
                    closed.push(pts[0]);
                    painter.add(Shape::line(closed.clone(), Stroke::new(1.0, Color32::WHITE)));
                    painter.extend(Shape::dashed_line(
                        &closed,
                        Stroke::new(1.0, Color32::BLACK),
                        5.0,
                        5.0,
                    ));
                    if self.tool == Tool::PolyLasso {
                        for p in &pts {
                            painter.circle_filled(*p, 3.0, Color32::WHITE);
                        }
                    }
                }
            }
            Tool::Gradient => {
                if self.drag == Some(DragKind::Gradient) {
                    if let (Some(a), Some(b)) = (self.drag_start, ctx.input(|i| i.pointer.latest_pos())) {
                        painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(140)));
                        painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
                        painter.circle_filled(a, 4.0, Color32::WHITE);
                        painter.circle_stroke(b, 4.0, Stroke::new(1.5, Color32::WHITE));
                    }
                }
            }
            Tool::Hand | Tool::Move | Tool::Eyedropper | Tool::Bucket | Tool::Wand | Tool::Text => {}
        }
    }
}

pub(crate) fn nearest_when_zoomed() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..Default::default()
    }
}

pub(crate) fn upload(
    slot: &mut Option<TextureHandle>,
    ctx: &egui::Context,
    name: &str,
    img: egui::ColorImage,
    opts: egui::TextureOptions,
) {
    match slot {
        Some(t) => t.set(img, opts),
        None => *slot = Some(ctx.load_texture(name, img, opts)),
    }
}

pub(crate) fn srgb_lut() -> &'static [u8; 4096] {
    static LUT: OnceLock<[u8; 4096]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 4096];
        for (i, v) in t.iter_mut().enumerate() {
            *v = nge_io::linear_to_srgb(i as f32 / 4095.0);
        }
        t
    })
}

pub(crate) fn to_color32(p: nge_tiles::Rgba) -> Color32 {
    let lut = srgb_lut();
    let enc = |v: f32| lut[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize];
    let [r, g, b, a] = p.to_straight();
    Color32::from_rgba_unmultiplied(enc(r), enc(g), enc(b), (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

pub(crate) fn raster_to_image(flat: &Raster) -> egui::ColorImage {
    egui::ColorImage {
        size: [flat.width as usize, flat.height as usize],
        pixels: flat.pixels.iter().map(|p| to_color32(*p)).collect(),
    }
}

/// Sample a layer onto a small thumbnail (nearest, centre of each cell).
pub(crate) fn thumb_image(canvas: Rect, sample: impl Fn(i32, i32) -> nge_tiles::Rgba) -> egui::ColorImage {
    let (tw, th) = THUMB;
    let scale = (canvas.w as f32 / tw as f32)
        .max(canvas.h as f32 / th as f32)
        .max(1e-3);
    let (cw, ch) = (canvas.w as f32 / scale, canvas.h as f32 / scale);
    let (ox, oy) = ((tw as f32 - cw) / 2.0, (th as f32 - ch) / 2.0);
    let mut pixels = vec![Color32::TRANSPARENT; tw * th];
    for ty in 0..th {
        for tx in 0..tw {
            let fx = tx as f32 + 0.5 - ox;
            let fy = ty as f32 + 0.5 - oy;
            if fx < 0.0 || fy < 0.0 || fx >= cw || fy >= ch {
                continue;
            }
            let x = canvas.x + (fx * scale) as i32;
            let y = canvas.y + (fy * scale) as i32;
            pixels[ty * tw + tx] = to_color32(sample(x, y));
        }
    }
    egui::ColorImage {
        size: [tw, th],
        pixels,
    }
}

pub(crate) fn paint_thumb_bg(p: &egui::Painter, r: egui::Rect) {
    p.rect_filled(r, 2.0, Color32::from_gray(200));
    let cell = 5.0;
    let mut y = r.min.y;
    let mut row = 0;
    while y < r.max.y {
        let mut x = r.min.x + if row % 2 == 0 { 0.0 } else { cell };
        while x < r.max.x {
            let c = egui::Rect::from_min_size(egui::pos2(x, y), Vec2::splat(cell)).intersect(r);
            p.rect_filled(c, 0.0, Color32::from_gray(150));
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

/// Selection outline overlay: pixels on the 50% coverage boundary alternate
/// black and white.
pub(crate) fn selection_overlay(doc: &Document) -> Option<egui::ColorImage> {
    let sel = doc.selection.as_ref()?;
    let (w, h) = (doc.width as i32, doc.height as i32);
    let area = sel.bounds_within(doc.canvas());
    if area.is_empty() {
        return None;
    }
    let (x0, y0) = ((area.x - 1).max(0), (area.y - 1).max(0));
    let (x1, y1) = ((area.right() + 1).min(w), (area.bottom() + 1).min(h));
    let (gw, gh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut inside = vec![false; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            inside[gy * gw + gx] = sel.value(x0 + gx as i32, y0 + gy as i32) >= 0.5;
        }
    }
    let at = |gx: i32, gy: i32| {
        gx >= 0
            && gy >= 0
            && (gx as usize) < gw
            && (gy as usize) < gh
            && inside[gy as usize * gw + gx as usize]
    };
    let mut pixels = vec![Color32::TRANSPARENT; (w * h) as usize];
    for gy in 0..gh as i32 {
        for gx in 0..gw as i32 {
            if at(gx, gy) && !(at(gx - 1, gy) && at(gx + 1, gy) && at(gx, gy - 1) && at(gx, gy + 1)) {
                let (x, y) = (x0 + gx, y0 + gy);
                pixels[(y * w + x) as usize] = if ((x + y) / 4) % 2 == 0 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                };
            }
        }
    }
    Some(egui::ColorImage {
        size: [w as usize, h as usize],
        pixels,
    })
}

pub(crate) fn drag_rect(a: (f32, f32), b: (f32, f32), canvas: Rect) -> Rect {
    let (x0, x1) = (a.0.min(b.0).round() as i32, a.0.max(b.0).round() as i32);
    let (y0, y1) = (a.1.min(b.1).round() as i32, a.1.max(b.1).round() as i32);
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&canvas)
}

pub(crate) fn ellipse_points(r: egui::Rect, n: usize) -> Vec<Pos2> {
    let c = r.center();
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            egui::pos2(c.x + t.cos() * r.width() / 2.0, c.y + t.sin() * r.height() / 2.0)
        })
        .collect()
}

pub(crate) fn point_in_convex(poly: &[Pos2], q: Pos2) -> bool {
    let n = poly.len();
    let mut sign = 0.0f32;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let cross = (b.x - a.x) * (q.y - a.y) - (b.y - a.y) * (q.x - a.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

pub(crate) fn paint_xform_box(painter: &egui::Painter, x: &Xform, to_screen: impl Fn(f32, f32) -> Pos2) {
    let pts: Vec<Pos2> = x.corners().iter().map(|(px, py)| to_screen(*px, *py)).collect();
    let mut closed = pts.clone();
    closed.push(pts[0]);
    painter.add(Shape::line(
        closed.clone(),
        Stroke::new(2.0, Color32::from_black_alpha(140)),
    ));
    painter.add(Shape::line(closed, Stroke::new(1.0, Color32::WHITE)));
    for p in &pts {
        painter.rect_filled(
            egui::Rect::from_center_size(*p, Vec2::splat(8.0)),
            1.0,
            Color32::WHITE,
        );
        painter.rect_stroke(
            egui::Rect::from_center_size(*p, Vec2::splat(8.0)),
            1.0,
            Stroke::new(1.0, ACCENT),
        );
    }
    let (cx, cy) = x.center();
    let c = to_screen(cx + x.dx, cy + x.dy);
    painter.circle_stroke(c, 5.0, Stroke::new(1.0, Color32::WHITE));
    painter.line_segment(
        [c + egui::vec2(-8.0, 0.0), c + egui::vec2(8.0, 0.0)],
        Stroke::new(1.0, Color32::WHITE),
    );
    painter.line_segment(
        [c + egui::vec2(0.0, -8.0), c + egui::vec2(0.0, 8.0)],
        Stroke::new(1.0, Color32::WHITE),
    );
}

pub(crate) fn paint_checker(painter: &egui::Painter, doc_rect: egui::Rect, clip: egui::Rect) {
    let visible = doc_rect.intersect(clip);
    if visible.width() <= 0.0 || visible.height() <= 0.0 {
        return;
    }
    painter.rect_filled(visible, 0.0, Color32::from_gray(222));
    let cell = 12.0;
    let (i0, i1) = (
        ((visible.min.x - doc_rect.min.x) / cell).floor() as i32,
        ((visible.max.x - doc_rect.min.x) / cell).ceil() as i32,
    );
    let (j0, j1) = (
        ((visible.min.y - doc_rect.min.y) / cell).floor() as i32,
        ((visible.max.y - doc_rect.min.y) / cell).ceil() as i32,
    );
    for j in j0..j1 {
        for i in i0..i1 {
            if (i + j) % 2 != 0 {
                let r = egui::Rect::from_min_size(
                    doc_rect.min + egui::vec2(i as f32 * cell, j as f32 * cell),
                    Vec2::splat(cell),
                )
                .intersect(visible);
                painter.rect_filled(r, 0.0, Color32::from_gray(188));
            }
        }
    }
}
