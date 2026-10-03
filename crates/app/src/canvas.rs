use super::*;
use crate::soft_proof::display_image;

/// Nanoseconds spent compositing the canvas (refresh and previews), for
/// the render timings in `render_bench`.
pub(crate) static COMPOSITE_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// An angle (radians) to the nearest 15°, for Shift-rotating.
pub(crate) fn snap_angle(a: f32) -> f32 {
    let step = 15f32.to_radians();
    (a / step).round() * step
}

pub(crate) fn timed<T>(f: impl FnOnce() -> T) -> T {
    let t = std::time::Instant::now();
    let out = f();
    COMPOSITE_NANOS.fetch_add(
        t.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    out
}

/// Outlines longer than this animate as a static texture instead.
const ANTS_MAX: usize = 20_000;

/// Warp mesh resolution: cells per side (so (n+1)² control points).
pub(crate) const WARP_CELLS: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewCmd {
    Fit,
    Actual,
    /// One step in or out about the canvas centre (the zoom pill's step).
    ZoomIn,
    ZoomOut,
    /// View ▸ Print size: an inch of the document over about an inch of
    /// screen (image_size_ui.rs).
    PrintSize,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Handle {
    /// Edge midpoints: 0 top, 1 right, 2 bottom, 3 left — one-axis scale.
    Edge(usize),
    Corner(usize),
    Inside,
    Rotate,
    /// One control point of the warp mesh (row-major index).
    WarpPoint(usize),
}

/// Which part of a path node a pen drag grabbed.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PenPart {
    Anchor,
    In,
    Out,
}

/// An in-progress pen edit of an existing node.
#[derive(Clone, Copy)]
pub(crate) struct PenHit {
    pub(crate) sub: usize,
    pub(crate) node: usize,
    pub(crate) part: PenPart,
    /// The handles were symmetric when grabbed, so keep them so.
    pub(crate) mirrored: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DragKind {
    Stroke,
    Select,
    Lasso,
    Move,
    Gradient,
    Xform(Handle),
    /// Shape tool: drawing a new shape (shape_tool.rs).
    Shape,
    /// A retouching mode's own drag (retouch_ui.rs).
    Retouch,
}

/// In-progress free transform of one layer.
#[derive(Clone)]
pub(crate) struct Xform {
    pub(crate) layer: LayerId,
    pub(crate) bounds: Rect,
    pub(crate) sx: f32,
    pub(crate) sy: f32,
    /// Horizontal shear factor (tan of the skew angle).
    pub(crate) shear: f32,
    pub(crate) angle: f32,
    pub(crate) dx: f32,
    pub(crate) dy: f32,
    // (sx, sy, angle, dx, dy) at the start of the current drag
    pub(crate) base: (f32, f32, f32, f32, f32),
    /// Perspective mode: each corner moves freely; the affine fields are
    /// frozen into these four points while it is on.
    pub(crate) quad: Option<[(f32, f32); 4]>,
    /// The quad at the start of the current drag.
    pub(crate) qbase: [(f32, f32); 4],
    /// Warp mode: a (WARP_CELLS+1)² mesh of free control points over the
    /// layer bounds, row-major; exclusive with perspective.
    pub(crate) warp: Option<Vec<(f32, f32)>>,
    /// The mesh at the start of the current drag.
    pub(crate) wbase: Vec<(f32, f32)>,
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
        Affine::translate(-cx, -cy)
            .then(&Affine::scale(self.sx, self.sy))
            .then(&Affine::shear_x(self.shear))
            .then(&Affine::rotate(self.angle))
            .then(&Affine::translate(cx + self.dx, cy + self.dy))
    }

    /// Transformed corners, in document space.
    pub(crate) fn corners(&self) -> [(f32, f32); 4] {
        if let Some(w) = &self.warp {
            let n = WARP_CELLS;
            return [w[0], w[n], w[(n + 1) * (n + 1) - 1], w[n * (n + 1)]];
        }
        if let Some(q) = self.quad {
            return q;
        }
        let a = self.affine();
        let b = self.bounds;
        [
            a.apply(b.x as f32, b.y as f32),
            a.apply(b.right() as f32, b.y as f32),
            a.apply(b.right() as f32, b.bottom() as f32),
            a.apply(b.x as f32, b.bottom() as f32),
        ]
    }

    /// A warp mesh that starts exactly where the box is now: the current
    /// corners, bilinearly interpolated (exact for any affine state).
    pub(crate) fn initial_warp(&self) -> Vec<(f32, f32)> {
        let [a, b, c, d] = self.corners();
        let n = WARP_CELLS;
        let mut pts = Vec::with_capacity((n + 1) * (n + 1));
        for j in 0..=n {
            let v = j as f32 / n as f32;
            for i in 0..=n {
                let u = i as f32 / n as f32;
                let top = (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u);
                let bot = (d.0 + (c.0 - d.0) * u, d.1 + (c.1 - d.1) * u);
                pts.push((top.0 + (bot.0 - top.0) * v, top.1 + (bot.1 - top.1) * v));
            }
        }
        pts
    }

    pub(crate) fn warp_grid(&self) -> Option<lumenply_render::WarpGrid> {
        self.warp.as_ref().map(|w| lumenply_render::WarpGrid {
            src: self.bounds,
            cols: WARP_CELLS,
            rows: WARP_CELLS,
            points: w.clone(),
        })
    }

    /// The eight scale handles in document space: the corners (top-left,
    /// top-right, bottom-right, bottom-left), then the edge midpoints
    /// (top, right, bottom, left), where the canvas grabs them.
    pub(crate) fn handles(&self) -> [(Handle, (f32, f32)); 8] {
        let c = self.corners();
        let mid = |i: usize| {
            let (a, b) = (c[i], c[(i + 1) % 4]);
            (Handle::Edge(i), ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0))
        };
        [
            (Handle::Corner(0), c[0]),
            (Handle::Corner(1), c[1]),
            (Handle::Corner(2), c[2]),
            (Handle::Corner(3), c[3]),
            mid(0),
            mid(1),
            mid(2),
            mid(3),
        ]
    }

    /// The local point (centre-relative, before scaling) of a scale
    /// handle: a corner or an edge's midpoint.
    fn handle_local(&self, h: Handle) -> Option<(f32, f32)> {
        let (w, ht) = (self.bounds.w as f32 / 2.0, self.bounds.h as f32 / 2.0);
        match h {
            Handle::Corner(i) => Some(([-w, w, w, -w][i % 4], [-ht, -ht, ht, ht][i % 4])),
            Handle::Edge(i) => Some([(0.0, -ht), (w, 0.0), (0.0, ht), (-w, 0.0)][i % 4]),
            _ => None,
        }
    }

    /// Scale for a drag of handle `h` (a corner or edge) that pressed at
    /// `press` and is now at `to` (document points), from the box as it
    /// was when the drag began (`base`). The opposite corner or edge stays
    /// where it was, or the centre with `from_centre`; `proportional`
    /// scales both axes by one ratio (along the press–anchor diagonal).
    pub(crate) fn drag_scale(
        &mut self,
        h: Handle,
        press: (f32, f32),
        to: (f32, f32),
        proportional: bool,
        from_centre: bool,
    ) {
        let Some((hx, hy)) = self.handle_local(h) else {
            return;
        };
        let (px, py) = if from_centre { (0.0, 0.0) } else { (-hx, -hy) };
        let (cx, cy) = self.center();
        let mut start = self.clone();
        (start.sx, start.sy, start.angle, start.dx, start.dy) = self.base;
        let anchor = start.affine().apply(cx + px, cy + py);
        let clamp = |v: f32| v.abs().clamp(0.02, 50.0) * if v < 0.0 { -1.0 } else { 1.0 };
        if proportional {
            let a = (press.0 - anchor.0, press.1 - anchor.1);
            let b = (to.0 - anchor.0, to.1 - anchor.1);
            let k = (a.0 * b.0 + a.1 * b.1) / (a.0 * a.0 + a.1 * a.1).max(1e-6);
            self.sx = clamp(self.base.0 * k);
            self.sy = clamp(self.base.1 * k);
        } else {
            // The pointer in the box's un-rotated frame, from the anchor.
            let (s, c) = (-self.base.2).sin_cos();
            let v = (to.0 - anchor.0, to.1 - anchor.1);
            let local = (v.0 * c - v.1 * s, v.0 * s + v.1 * c);
            let span = (hx - px, hy - py);
            if span.0.abs() > 1e-6 {
                self.sx = clamp(local.0 / span.0);
            }
            if span.1.abs() > 1e-6 {
                self.sy = clamp(local.1 / span.1);
            }
        }
        let now = self.affine().apply(cx + px, cy + py);
        self.dx += anchor.0 - now.0;
        self.dy += anchor.1 - now.1;
    }

    pub(crate) fn bbox(&self) -> Rect {
        if let Some(w) = &self.warp {
            let x0 = w.iter().map(|p| p.0).fold(f32::INFINITY, f32::min).floor() as i32;
            let x1 = w.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
            let y0 = w.iter().map(|p| p.1).fold(f32::INFINITY, f32::min).floor() as i32;
            let y1 = w.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
            return Rect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        }
        if self.quad.is_some() {
            let cs = self.corners();
            let xs = cs.iter().map(|p| p.0);
            let ys = cs.iter().map(|p| p.1);
            let x0 = xs.clone().fold(f32::INFINITY, f32::min).floor() as i32;
            let x1 = xs.fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
            let y0 = ys.clone().fold(f32::INFINITY, f32::min).floor() as i32;
            let y1 = ys.fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
            return Rect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        }
        self.affine().transform_rect(self.bounds)
    }
}

impl App {
    // ---- rendering ---------------------------------------------------------------------

    /// The document's composite over `rect`, through the edit graph (ADR
    /// 0025): the editor's renderer caches every node's tiles by content,
    /// so whatever an edit didn't change below or above it, and the whole
    /// previous version after an undo, comes from the cache.
    pub(crate) fn render_view(&self, rect: Rect) -> lumenply_tiles::TileStore {
        let ed = &self.editor;
        timed(|| ed.renderer().render(ed.graph(), ed.blobs(), rect))
    }

    /// `doc`, the live document with the active layer changed as a preview
    /// makes it, over `rect`. Everything below the active layer's top-level
    /// layer is unchanged, so that backdrop is the graph's own output for the
    /// layer below (from the render cache), and only the layers from the
    /// active one up are composited over it. Takes the reference path when
    /// there is nothing below, when a live filter at or above would read the
    /// layers below as a stack, or when the preview's lower layers aren't the
    /// graph's.
    pub(crate) fn preview_composite(&self, doc: &Document, rect: Rect) -> lumenply_tiles::TileStore {
        use rayon::prelude::*;
        let layers = doc.layers();
        let graph = self.editor.graph();
        let records = &self.editor.doc_state().layers;
        let contains = |l: &Layer, id: LayerId| {
            fn walk(l: &Layer, id: LayerId) -> bool {
                l.id == id || l.children().is_some_and(|c| c.iter().any(|ch| walk(ch, id)))
            }
            walk(l, id)
        };
        // A clip chain composites as one unit with its base: split below it.
        let split = self
            .active
            .and_then(|id| layers.iter().position(|l| contains(l, id)))
            .map(|mut s| {
                while s > 0 && layers[s].clip {
                    s -= 1;
                }
                s
            })
            .filter(|&s| {
                s > 0
                    && (doc.width, doc.height) == (graph.width, graph.height)
                    && records.len() >= s
                    && records[..s]
                        .iter()
                        .zip(&layers[..s])
                        .all(|(r, l)| r.layer.id == l.id)
                    && !layers[s..]
                        .iter()
                        .any(|l| l.visible && l.opacity > 0.0 && matches!(l.content, LayerContent::Filter(_)))
            });
        let Some(split) = split else {
            return timed(|| lumenply_render::composite_rect(doc, rect));
        };
        timed(|| {
            let ed = &self.editor;
            let backdrop = ed
                .renderer()
                .render_node(graph, ed.blobs(), records[split - 1].node, rect);
            let canvas = doc.canvas();
            let tiles: Vec<_> = rect
                .tiles()
                .into_par_iter()
                .map(|c| {
                    let under = backdrop.tile(c).cloned();
                    (
                        c,
                        lumenply_render::render_tile_over(under, &layers[split..], c, canvas),
                    )
                })
                .collect();
            let mut out = lumenply_tiles::TileStore::new();
            for (c, t) in tiles {
                if let Some(t) = t {
                    out.insert(c, std::sync::Arc::new(t));
                }
            }
            out
        })
    }

    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        let proof = self.proof_view();
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
                    let patch = flatten(&self.render_view(r), r);
                    if let Some(flat) = self.last_flat.as_mut() {
                        // The histogram follows the pixels that changed.
                        let gone = histogram::luminance_histogram_in(flat, r);
                        let came = histogram::luminance_histogram(&patch);
                        for ((h, g), c) in self.histogram.iter_mut().zip(gone).zip(came) {
                            *h = (*h + c).saturating_sub(g);
                        }
                        for y in 0..r.h {
                            let row = (r.y as usize + y as usize) * flat.width as usize + r.x as usize;
                            let src = y as usize * r.w as usize;
                            flat.pixels[row..row + r.w as usize]
                                .copy_from_slice(&patch.pixels[src..src + r.w as usize]);
                        }
                    }
                    if let Some(tex) = self.canvas_tex.as_mut() {
                        tex.set_partial(
                            [r.x as usize, r.y as usize],
                            display_image(&patch, proof),
                            nearest_when_zoomed(),
                        );
                    }
                }
                if self.quick_mask {
                    let img = quick_mask_overlay(self.editor.doc());
                    upload(
                        &mut self.overlay_tex,
                        ctx,
                        "selection",
                        img,
                        egui::TextureOptions::NEAREST,
                    );
                }
                self.refresh_thumbs(ctx, Some(r));
            }
            _ => {
                let flat = flatten(&self.render_view(canvas), canvas);
                let img = display_image(&flat, proof);
                self.histogram = histogram::luminance_histogram(&flat);
                self.last_flat = Some(flat);
                upload(&mut self.canvas_tex, ctx, "canvas", img, nearest_when_zoomed());
                if self.quick_mask {
                    self.sel_points.clear();
                    let img = quick_mask_overlay(self.editor.doc());
                    upload(
                        &mut self.overlay_tex,
                        ctx,
                        "selection",
                        img,
                        egui::TextureOptions::NEAREST,
                    );
                } else {
                    self.sel_points = selection_outline_points(self.editor.doc()).unwrap_or_default();
                    // The static texture stays as the fallback for outlines too
                    // big to animate shape-by-shape.
                    if self.sel_points.len() > ANTS_MAX {
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
                    } else {
                        self.overlay_tex = None;
                    }
                }
                self.refresh_thumbs(ctx, None);
            }
        }
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
                LayerContent::Fill(f) => {
                    if let Some(store) = &f.cache {
                        thumbs.push((l.id, thumb_image(canvas, |x, y| store.get_pixel(x, y))))
                    }
                }
                LayerContent::Shape(sh) => {
                    if let Some(store) = &sh.cache {
                        thumbs.push((l.id, thumb_image(canvas, |x, y| store.get_pixel(x, y))))
                    }
                }
                LayerContent::Group(children) if area.is_none_or(|r| group_dirty(children, r)) => {
                    let flat = lumenply_render::composite_layers(children, canvas, canvas);
                    thumbs.push((l.id, thumb_image(canvas, |x, y| flat.get_pixel(x, y))));
                }
                _ => {}
            }
            if let Some(m) = &l.mask {
                masks.push((
                    l.id,
                    thumb_image(canvas, |x, y| {
                        let v = m.value(x, y);
                        lumenply_tiles::Rgba::new(v, v, v, 1.0)
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
        // Smart filters re-render over the previewed pixels, and reach
        // past the edited area (see smart_filters_ui).
        let fresh = crate::smart_filters_ui::refreshed_preview(doc);
        let doc = fresh.as_ref().unwrap_or(doc);
        let area = lumenply_core::smart_filter_cmds::widen_affected(doc, area);
        // Previews change only the active layer, so the graph's composite of
        // everything below it applies to the preview document too.
        let proof = self.proof_view();
        match area {
            Some(r) if self.canvas_tex.is_some() => {
                let r = r.intersect(&doc.canvas());
                if !r.is_empty() {
                    let patch = flatten(&self.preview_composite(doc, r), r);
                    let Some(tex) = self.canvas_tex.as_mut() else {
                        return;
                    };
                    tex.set_partial(
                        [r.x as usize, r.y as usize],
                        display_image(&patch, proof),
                        nearest_when_zoomed(),
                    );
                    self.panels.preview_patch(r, &patch);
                }
            }
            _ => {
                let canvas = doc.canvas();
                let img = display_image(&flatten(&self.preview_composite(doc, canvas), canvas), proof);
                upload(&mut self.canvas_tex, ctx, "canvas", img, nearest_when_zoomed());
            }
        }
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
        match cmd {
            ViewCmd::ZoomIn => return self.zoom_at(rect, rect.center(), 1.25),
            ViewCmd::ZoomOut => return self.zoom_at(rect, rect.center(), 1.0 / 1.25),
            ViewCmd::Fit | ViewCmd::Actual | ViewCmd::PrintSize => {}
        }
        let doc = self.editor.doc();
        let size = Vec2::new(doc.width as f32, doc.height as f32);
        self.zoom = match cmd {
            ViewCmd::Fit => ((rect.width() - 80.0) / size.x)
                .min((rect.height() - 80.0) / size.y)
                .clamp(0.05, 32.0),
            ViewCmd::Actual => 1.0,
            ViewCmd::PrintSize => crate::image_size_ui::print_size_zoom(doc.resolution),
            ViewCmd::ZoomIn | ViewCmd::ZoomOut => return,
        };
        self.pan = (rect.size() - size * self.zoom) / 2.0;
    }

    pub(crate) fn canvas(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
                a11y_name(&resp, "Canvas");
                let rect = resp.rect;
                let painter = painter.with_clip_rect(rect);
                self.apply_view_cmd(rect);
                self.crop_sync();

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
                // Zoom mode: click zooms in at the pointer, Alt-click out.
                if self.tool == Tool::Hand
                    && self.hand_zoom
                    && !space
                    && resp.clicked_by(egui::PointerButton::Primary)
                {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let out = ctx.input(|i| i.modifiers.alt);
                        self.zoom_at(rect, p, if out { 0.5 } else { 2.0 });
                    }
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

                // ---- colour picker hook (color_picker.rs) ------------------
                // An armed picker eyedropper, or the press that dismissed a
                // picker, owns the pointer: no tool may act on it.
                let picker_owns = self.picker_canvas_input(ctx, &resp, to_doc);
                // ---- end colour picker hook ----------------------------------

                // While Space pans, the active tool must not also fire.
                if picker_owns {
                    // Handled by the colour picker above.
                } else if self.xform.is_some() {
                    self.handle_xform(ctx, &resp, to_doc, to_screen);
                } else if !space && !self.guides_canvas_input(ctx, &resp) {
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
                self.paint_panels_on_canvas(&painter, rect, doc_rect);
                if let Some(tex) = &self.overlay_tex {
                    painter.image(tex.id(), doc_rect, uv, Color32::WHITE);
                } else if !self.sel_points.is_empty() {
                    // Marching ants: 4-pixel dashes whose phase walks along
                    // the outline over time.
                    let phase = (ui.input(|i| i.time) * 12.0) as i32;
                    let px = zoom.max(1.0);
                    for &(x, y) in &self.sel_points {
                        let on = ((x + y + phase) / 4) % 2 == 0;
                        let c = if on { Color32::BLACK } else { Color32::WHITE };
                        let min = to_screen(x as f32, y as f32);
                        painter.rect_filled(egui::Rect::from_min_size(min, Vec2::splat(px)), 0.0, c);
                    }
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(90));
                }
                painter.rect_stroke(doc_rect, 0.0, Stroke::new(1.0, LINE));
                self.paint_view_aids(&painter, rect, doc_rect);

                if let Some(x) = &self.xform {
                    paint_xform_box(&painter, x, to_screen);
                } else if !picker_owns {
                    self.paint_tool_overlay(ctx, &painter, &resp);
                }
                self.paint_snap_hint(&painter, rect, doc_rect);
                self.paint_smart_guides(&painter, rect, doc_rect);
                self.rulers_ui(ui, rect);
                self.selection_action_bar(ctx, rect, origin, zoom);
                self.zoom_pill(ctx, rect);
            });
    }

    /// A floating zoom control in the canvas corner: − / percentage / + / Fit.
    fn zoom_pill(&mut self, ctx: &egui::Context, clip: egui::Rect) {
        egui::Area::new("zoom-pill".into())
            .order(egui::Order::Foreground)
            .sense(BACKDROP_SENSE)
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
                            let out = ui.add(egui::Button::new("−").frame(false));
                            a11y_name(&out, "Zoom out");
                            if out.clicked() {
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
                            let zin = ui.add(egui::Button::new("+").frame(false));
                            a11y_name(&zin, "Zoom in");
                            if zin.clicked() {
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
            .sense(BACKDROP_SENSE)
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
                            if ui
                                .add_enabled(self.active_is_pixel(), egui::Button::new("Content-Aware"))
                                .on_hover_text(
                                    "Rebuild the selected area from its surroundings (Content-Aware Fill)",
                                )
                                .clicked()
                            {
                                act = Some("content-aware");
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
            if let Some(w) = &x.warp {
                // Nearest control point under the pointer, else move all.
                let best = w
                    .iter()
                    .enumerate()
                    .map(|(k, (px, py))| (k, to_screen(*px, *py).distance(q)))
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                return match best {
                    Some((k, d)) if d <= 10.0 => Handle::WarpPoint(k),
                    _ => Handle::Inside,
                };
            }
            for (i, (px, py)) in x.corners().iter().enumerate() {
                if to_screen(*px, *py).distance(q) <= 10.0 {
                    return Handle::Corner(i);
                }
            }
            let cs = x.corners();
            for i in 0..4 {
                let (ax, ay) = cs[i];
                let (bx, by) = cs[(i + 1) % 4];
                let m = to_screen((ax + bx) / 2.0, (ay + by) / 2.0);
                if m.distance(q) <= 9.0 {
                    return Handle::Edge(i);
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
                Handle::Edge(1) | Handle::Edge(3) => egui::CursorIcon::ResizeHorizontal,
                Handle::Edge(_) => egui::CursorIcon::ResizeVertical,
                Handle::Inside => egui::CursorIcon::Move,
                Handle::Rotate => egui::CursorIcon::Alias,
                Handle::WarpPoint(_) => egui::CursorIcon::Crosshair,
            });
        }
        if resp.drag_started_by(primary) {
            if let Some(q) = ctx.input(|i| i.pointer.press_origin()) {
                self.drag = Some(DragKind::Xform(hit(q)));
                self.drag_start = Some(q);
                self.begin_snap(&[x.layer]);
                self.begin_smart_guides(&[x.layer]);
                x.base = (x.sx, x.sy, x.angle, x.dx, x.dy);
                if let Some(quad) = x.quad {
                    x.qbase = quad;
                }
                if let Some(w) = &x.warp {
                    x.wbase = w.clone();
                }
            }
        }
        let mut changed = false;
        if let (Some(DragKind::Xform(h)), true) = (self.drag, resp.dragged_by(primary)) {
            let qbase = x.qbase;
            let wbase = x.wbase.clone();
            if let (Some(a), Some(b), Some(w)) =
                (self.drag_start, resp.interact_pointer_pos(), x.warp.as_mut())
            {
                let (ax, ay) = to_doc(a);
                let (bx, by) = to_doc(b);
                let (ddx, ddy) = (bx - ax, by - ay);
                if wbase.len() == w.len() {
                    match h {
                        Handle::WarpPoint(k) => w[k] = (wbase[k].0 + ddx, wbase[k].1 + ddy),
                        _ => {
                            for (p, q) in w.iter_mut().zip(&wbase) {
                                *p = (q.0 + ddx, q.1 + ddy);
                            }
                        }
                    }
                    changed = true;
                }
            } else if let (Some(a), Some(b), Some(quad)) =
                (self.drag_start, resp.interact_pointer_pos(), x.quad.as_mut())
            {
                // Perspective: corners move freely, edges carry both of
                // their corners, inside moves the whole quad.
                let (ax, ay) = to_doc(a);
                let (bx, by) = to_doc(b);
                let (ddx, ddy) = (bx - ax, by - ay);
                let mut shift = |idx: &[usize]| {
                    for &i in idx {
                        quad[i] = (qbase[i].0 + ddx, qbase[i].1 + ddy);
                    }
                };
                match h {
                    Handle::Corner(i) => shift(&[i]),
                    Handle::Edge(i) => shift(&[i, (i + 1) % 4]),
                    Handle::Inside | Handle::Rotate | Handle::WarpPoint(_) => shift(&[0, 1, 2, 3]),
                }
                changed = true;
            } else if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                match h {
                    Handle::Corner(_) | Handle::Edge(_) => {
                        // As in Photoshop: the opposite corner or edge stays
                        // put (Alt: the centre does); corners keep the
                        // proportions unless Shift is held.
                        let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                        let proportional = matches!(h, Handle::Corner(_)) && !shift;
                        x.drag_scale(h, to_doc(a), to_doc(b), proportional, alt);
                    }
                    Handle::Inside => {
                        let (ax, ay) = to_doc(a);
                        let (bx, by) = to_doc(b);
                        x.dx = x.base.3 + (bx - ax);
                        x.dy = x.base.4 + (by - ay);
                        // Snap the moved box by its edges or centre.
                        let cs = x.corners();
                        let xs = cs.iter().map(|c| c.0);
                        let ys = cs.iter().map(|c| c.1);
                        let (x0, x1) = (
                            xs.clone().fold(f32::INFINITY, f32::min),
                            xs.fold(f32::NEG_INFINITY, f32::max),
                        );
                        let (y0, y1) = (
                            ys.clone().fold(f32::INFINITY, f32::min),
                            ys.fold(f32::NEG_INFINITY, f32::max),
                        );
                        // Cmd (Ctrl) held: no snapping, as in Photoshop.
                        let free = ctx.input(|i| i.modifiers.command);
                        let (sx, sy) = if free {
                            self.clear_snap_hint();
                            (0.0, 0.0)
                        } else {
                            self.snap_rect_delta(x0, y0, x1, y1)
                        };
                        // Then Smart Guides, on the axes View ▸ Snap left free.
                        let held = self.snap_held();
                        let bx = crate::smart_guides::Bx::new(x0 + sx, y0 + sy, x1 + sx, y1 + sy);
                        let moved = (x.dx + sx - x.base.3, x.dy + sy - x.base.4);
                        self.aids.smart.pointer = Some(b);
                        let (mx, my) = self.smart_xform_nudge(bx, moved, held, free);
                        x.dx += sx + mx;
                        x.dy += sy + my;
                    }
                    Handle::Rotate => {
                        let a0 = (a.y - centre_s.y).atan2(a.x - centre_s.x);
                        let a1 = (b.y - centre_s.y).atan2(b.x - centre_s.x);
                        x.angle = x.base.2 + (a1 - a0);
                        // Shift turns in 15° steps, as in Photoshop.
                        if ctx.input(|i| i.modifiers.shift) {
                            x.angle = snap_angle(x.angle);
                        }
                    }
                    Handle::WarpPoint(_) => {}
                }
                changed = true;
            }
        }
        if resp.drag_stopped() {
            if let Some(DragKind::Xform(_)) = self.drag {
                self.drag = None;
                self.drag_start = None;
                self.end_snap();
            }
        }
        if changed {
            self.preview_xform(ctx, &mut x);
        }
        self.xform = Some(x);
    }

    /// Redraw the live transform preview after its numbers changed (canvas
    /// drag or options-bar fields).
    pub(crate) fn preview_xform(&mut self, ctx: &egui::Context, x: &mut Xform) {
        let mut preview = self.editor.doc().clone();
        let ok = if let Some(grid) = x.warp_grid() {
            WarpLayer { layer: x.layer, grid }.apply(&mut preview).is_ok()
        } else if let Some(quad) = x.quad {
            PerspectiveLayer { layer: x.layer, quad }
                .apply(&mut preview)
                .is_ok()
        } else {
            TransformLayer {
                layer: x.layer,
                transform: x.affine(),
            }
            .apply(&mut preview)
            .is_ok()
        };
        if ok {
            let area = x.last_preview.union(&x.bbox());
            let pad = Rect::new(area.x - 2, area.y - 2, area.w + 4, area.h + 4);
            self.preview(ctx, &preview, Some(pad));
            x.last_preview = x.bbox();
        }
    }

    pub(crate) fn handle_tool(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        if self.retouch_canvas(ctx, resp, &to_doc) {
            return;
        }
        match self.tool {
            Tool::Pen => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                let mut path = self.editor.doc().work_path.clone().unwrap_or_default();
                let mut changed = false;
                let hit_r = 8.0 / self.zoom.max(0.01);
                let near =
                    |a: (f32, f32), x: f32, y: f32| ((a.0 - x).powi(2) + (a.1 - y).powi(2)).sqrt() < hit_r;
                // Handles are only grabbable where they are drawn: on the
                // selected node and on the open subpath's newest node.
                let handle_hit = |path: &lumenply_doc::VectorPath, x: f32, y: f32| -> Option<PenHit> {
                    let mut spots: Vec<(usize, usize)> = self.pen_sel.into_iter().collect();
                    if self.pen_open {
                        if let Some(sp) = path.subpaths.last() {
                            if !sp.nodes.is_empty() {
                                spots.push((path.subpaths.len() - 1, sp.nodes.len() - 1));
                            }
                        }
                    }
                    for (si, ni) in spots {
                        let n = path.subpaths.get(si)?.nodes.get(ni)?;
                        let mirrored = near(
                            (2.0 * n.point.0 - n.handle_out.0, 2.0 * n.point.1 - n.handle_out.1),
                            n.handle_in.0,
                            n.handle_in.1,
                        );
                        for (part, h) in [(PenPart::In, n.handle_in), (PenPart::Out, n.handle_out)] {
                            if h != n.point && near(h, x, y) {
                                return Some(PenHit {
                                    sub: si,
                                    node: ni,
                                    part,
                                    mirrored,
                                });
                            }
                        }
                    }
                    None
                };
                let anchor_hit = |path: &lumenply_doc::VectorPath, x: f32, y: f32| {
                    path.subpaths.iter().enumerate().find_map(|(si, sp)| {
                        sp.nodes
                            .iter()
                            .position(|n| near(n.point, x, y))
                            .map(|ni| (si, ni))
                    })
                };

                // Drag: grab a handle or an anchor if one is under the
                // pointer, else place a smooth node and pull its handles out.
                if resp.drag_started_by(primary) {
                    if let Some(q) = ctx.input(|i| i.pointer.press_origin()) {
                        let (x, y) = to_doc(q);
                        if let Some(hit) = handle_hit(&path, x, y) {
                            self.pen_sel = Some((hit.sub, hit.node));
                            self.pen_hit = Some(hit);
                        } else if let Some((si, ni)) = anchor_hit(&path, x, y) {
                            self.pen_sel = Some((si, ni));
                            self.pen_hit = Some(PenHit {
                                sub: si,
                                node: ni,
                                part: PenPart::Anchor,
                                mirrored: false,
                            });
                        } else {
                            if !self.pen_open {
                                path.subpaths.push(lumenply_doc::SubPath::default());
                                self.pen_open = true;
                            }
                            let si = path.subpaths.len() - 1;
                            if let Some(sp) = path.subpaths.last_mut() {
                                sp.nodes.push(lumenply_doc::PathNode::corner(x, y));
                                self.pen_sel = Some((si, sp.nodes.len() - 1));
                            }
                            self.pen_dragging = true;
                            changed = true;
                        }
                    }
                }
                if resp.dragged_by(primary) {
                    if let (Some(hit), Some(q)) = (self.pen_hit, resp.interact_pointer_pos()) {
                        let (x, y) = to_doc(q);
                        if let Some(n) = path
                            .subpaths
                            .get_mut(hit.sub)
                            .and_then(|sp| sp.nodes.get_mut(hit.node))
                        {
                            match hit.part {
                                PenPart::Anchor => {
                                    let (dx, dy) = (x - n.point.0, y - n.point.1);
                                    n.point = (x, y);
                                    n.handle_in = (n.handle_in.0 + dx, n.handle_in.1 + dy);
                                    n.handle_out = (n.handle_out.0 + dx, n.handle_out.1 + dy);
                                }
                                PenPart::In => {
                                    n.handle_in = (x, y);
                                    if hit.mirrored {
                                        n.handle_out = (2.0 * n.point.0 - x, 2.0 * n.point.1 - y);
                                    }
                                }
                                PenPart::Out => {
                                    n.handle_out = (x, y);
                                    if hit.mirrored {
                                        n.handle_in = (2.0 * n.point.0 - x, 2.0 * n.point.1 - y);
                                    }
                                }
                            }
                            changed = true;
                        }
                    } else if self.pen_dragging {
                        if let Some(q) = resp.interact_pointer_pos() {
                            let (hx, hy) = to_doc(q);
                            if let Some(n) = path.subpaths.last_mut().and_then(|sp| sp.nodes.last_mut()) {
                                n.handle_out = (hx, hy);
                                n.handle_in = (2.0 * n.point.0 - hx, 2.0 * n.point.1 - hy);
                                changed = true;
                            }
                        }
                    }
                }
                if resp.drag_stopped() {
                    self.pen_dragging = false;
                    if self.pen_hit.take().is_some() {
                        self.editor.end_coalescing();
                    }
                }

                // A click closes on the first node, selects an anchor it
                // lands on, or adds a corner node.
                if resp.clicked_by(primary) {
                    if let Some(q) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(q);
                        let close = self.pen_open
                            && path
                                .subpaths
                                .last()
                                .is_some_and(|sp| sp.nodes.len() >= 2 && near(sp.nodes[0].point, x, y));
                        if close {
                            if let Some(sp) = path.subpaths.last_mut() {
                                sp.closed = true;
                            }
                            self.pen_open = false;
                            changed = true;
                        } else if let Some((si, ni)) = anchor_hit(&path, x, y) {
                            self.pen_sel = Some((si, ni));
                            self.status = "Drag the anchor or its handles; Backspace deletes it".into();
                        } else {
                            if !self.pen_open {
                                path.subpaths.push(lumenply_doc::SubPath::default());
                                self.pen_open = true;
                            }
                            let si = path.subpaths.len() - 1;
                            if let Some(sp) = path.subpaths.last_mut() {
                                sp.nodes.push(lumenply_doc::PathNode::corner(x, y));
                                self.pen_sel = Some((si, sp.nodes.len() - 1));
                            }
                            changed = true;
                        }
                    }
                }

                // Enter finishes the path; Esc drops the open subpath;
                // Backspace removes the selected anchor.
                let (enter, esc, back) = if ctx.wants_keyboard_input() {
                    (false, false, false)
                } else {
                    ctx.input(|i| {
                        (
                            i.key_pressed(Key::Enter),
                            i.key_pressed(Key::Escape),
                            i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete),
                        )
                    })
                };
                if enter && self.pen_open {
                    self.pen_open = false;
                    self.editor.end_coalescing();
                    self.status = "Path finished — Fill, Stroke or Make selection in the bar".into();
                }
                if esc && self.pen_open {
                    path.subpaths.pop();
                    self.pen_open = false;
                    self.pen_sel = None;
                    changed = true;
                }
                if back {
                    if let Some((si, ni)) = self.pen_sel.take() {
                        if let Some(sp) = path.subpaths.get_mut(si) {
                            if ni < sp.nodes.len() {
                                sp.nodes.remove(ni);
                                if sp.nodes.is_empty() {
                                    path.subpaths.remove(si);
                                    if self.pen_open && si == path.subpaths.len() {
                                        self.pen_open = false;
                                    }
                                }
                                changed = true;
                            }
                        }
                    }
                }

                if changed {
                    let path = Some(path).filter(|p| !p.subpaths.is_empty());
                    self.run_coalescing(&SetWorkPath { path }, "pen");
                }
            }
            Tool::Crop => self.crop_input(ctx, resp, to_doc),
            Tool::Shape => self.shape_input(ctx, resp, to_doc),
            Tool::Hand => {
                if resp.hovered() {
                    let out = ctx.input(|i| i.modifiers.alt);
                    ctx.set_cursor_icon(if resp.dragged() {
                        egui::CursorIcon::Grabbing
                    } else if self.hand_zoom && out {
                        egui::CursorIcon::ZoomOut
                    } else if self.hand_zoom {
                        egui::CursorIcon::ZoomIn
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
                        if let Some(c) = color_picker::sample_srgb(flat, x, y) {
                            self.brush_rgb = c;
                            color_picker::remember(ctx, c);
                            let hex = color_picker::format_hex(c);
                            self.status = format!("Picked {hex} at {}, {}", x as i32, y as i32);
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
            Tool::Wand if self.quick.object => self.object_select_input(ctx, resp, &to_doc),
            Tool::Wand if self.quick.on => self.quick_select_input(ctx, resp, &to_doc),
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
            Tool::Gradient => self.gradient_input(ctx, resp, to_doc),
            Tool::Text => self.type_tool_input(ctx, resp, &to_doc),
            Tool::Move => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Move);
                }
                // Double-clicking text edits it, as in Photoshop.
                if resp.double_clicked_by(primary) {
                    let hit = resp.interact_pointer_pos().map(&to_doc).and_then(|(x, y)| {
                        text_layer_at(self.editor.doc().layers(), x, y).map(|id| (id, x, y))
                    });
                    if let Some((id, x, y)) = hit {
                        self.begin_text_edit(ctx, id, crate::text_edit::EditStart::At(x, y));
                    }
                }
                // Auto-Select (or Cmd at the press) picks the layer first.
                if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.move_auto_select(ctx, to_doc(p));
                    }
                }
                if resp.drag_started_by(primary) {
                    if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                        self.move_auto_select(ctx, to_doc(p));
                    }
                    let fill = self
                        .active_layer()
                        .is_some_and(|l| l.fill_layer().is_some() || l.shape_layer().is_some());
                    // Alt-drag moves a copy, as in Photoshop (one undo step).
                    if ctx.input(|i| i.modifiers.alt)
                        && (self.active_is_pixel() || self.active_is_text() || fill)
                    {
                        self.begin_move_copy();
                    }
                    if let Some(why) = self.lock_block(layer_actions::LockNeed::Move) {
                        self.status = why.into();
                    } else if self.active_is_pixel() || self.active_is_text() || fill {
                        self.drag = Some(DragKind::Move);
                        self.drag_start = ctx.input(|i| i.pointer.press_origin());
                        self.move_offset = (0, 0);
                        self.begin_move_snap();
                    } else {
                        self.status = "Select a pixel layer to move".into();
                    }
                }
                if self.drag == Some(DragKind::Move) && resp.dragged_by(primary) {
                    if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                        let d = (b - a) / self.zoom;
                        // Cmd (Ctrl) held: no snapping, as in Photoshop.
                        let free = ctx.input(|i| i.modifiers.command);
                        self.aids.smart.pointer = Some(b);
                        let off = self.snap_move_offset((d.x.round() as i32, d.y.round() as i32), free);
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
                                    .and_then(|l| l.pixels().or_else(|| l.shape_layer()?.cache.as_ref()))
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
                    self.end_snap();
                    let (dx, dy) = self.move_offset;
                    self.finish_move(self.active, dx, dy);
                }
            }
            Tool::Brush | Tool::Eraser | Tool::Clone | Tool::Heal => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                // Alt with a painting brush is Photoshop's temporary
                // eyedropper: click or drag to pick the colour underneath.
                let picking = self.tool == Tool::Brush
                    && self.brush.mode == BrushMode::Paint
                    && self.drag.is_none()
                    && ctx.input(|i| i.modifiers.alt);
                if picking {
                    if resp.is_pointer_button_down_on() || resp.clicked_by(primary) {
                        if let (Some(p), Some(flat)) = (resp.interact_pointer_pos(), &self.last_flat) {
                            let (x, y) = to_doc(p);
                            if let Some(c) = color_picker::sample_srgb(flat, x, y) {
                                self.brush_rgb = c;
                                let hex = color_picker::format_hex(c);
                                self.status = format!("Picked {hex} (Alt-click with the brush)");
                            }
                        }
                    }
                    return;
                }
                let needs_source = self.tool == Tool::Clone
                    || (self.tool == Tool::Heal && self.retouch.heal_mode == retouch_ui::HealMode::Healing);
                if needs_source {
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
                // Quick mask paints the selection, which no layer lock covers.
                let lock_why = if self.quick_mask {
                    None
                } else {
                    self.paint_lock_block()
                };
                let paintable = lock_why.is_none()
                    && if self.quick_mask {
                        true
                    } else if self.editing_mask {
                        self.active_has_mask()
                    } else {
                        self.active_is_pixel()
                    };
                if resp.drag_started_by(primary) {
                    if paintable {
                        if needs_source {
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
                            self.stroke.push(self.pen_point(ctx, x, y));
                        }
                    } else {
                        self.status = match lock_why {
                            Some(why) => why.into(),
                            None if self.editing_mask => "The active layer has no mask to paint".into(),
                            None => "Select a pixel layer to paint on".into(),
                        };
                    }
                }
                if self.drag == Some(DragKind::Stroke) && resp.dragged_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        self.stroke.push(self.pen_point(ctx, x, y));
                    }
                    if let (Some(layer), false) = (self.active, self.stroke.is_empty()) {
                        let cmd = self.stroke_command(layer, self.stroke.clone());
                        let mut preview = self.editor.doc().clone();
                        // The preview obeys layer locks as the commit will
                        // (a transparency-locked layer keeps its alpha).
                        let applied = cmd.apply(&mut preview).is_ok()
                            && lumenply_core::locks::enforce(self.editor.doc(), &mut preview, cmd.as_ref())
                                .is_ok();
                        if applied {
                            if self.quick_mask {
                                // The stroke edits the selection, not pixels:
                                // refresh the red overlay from the preview.
                                let img = quick_mask_overlay(&preview);
                                upload(
                                    &mut self.overlay_tex,
                                    ctx,
                                    "selection",
                                    img,
                                    egui::TextureOptions::NEAREST,
                                );
                                self.stroke_drawn = self.stroke.len();
                                return;
                            }
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
                            let end = pts.last().map(|p| (self.doc_key, p.x, p.y));
                            let cmd = self.stroke_command(layer, pts);
                            self.run(cmd.as_ref());
                            self.last_stroke_end = end;
                        }
                    }
                } else if resp.clicked_by(primary) && lock_why.is_some() {
                    self.status = lock_why.unwrap_or_default().into();
                } else if resp.clicked_by(primary) && paintable {
                    if let (Some(layer), Some(p)) = (self.active, resp.interact_pointer_pos()) {
                        let (x, y) = to_doc(p);
                        if needs_source {
                            // A single dab locks its own offset, same as a
                            // drag; reusing the previous stroke's offset
                            // would clone from the wrong place.
                            if let Some((sx, sy)) = self.clone_source {
                                self.clone_offset = ((sx - x).round() as i32, (sy - y).round() as i32);
                            }
                        }
                        // Shift-click: a straight line from where the last
                        // stroke ended on this document.
                        let from = self
                            .last_stroke_end
                            .filter(|(key, ..)| *key == self.doc_key && ctx.input(|i| i.modifiers.shift));
                        let mut points = Vec::with_capacity(2);
                        if let Some((_, fx, fy)) = from {
                            points.push(self.pen_point(ctx, fx, fy));
                        }
                        points.push(self.pen_point(ctx, x, y));
                        let cmd = self.stroke_command(layer, points);
                        self.run(cmd.as_ref());
                        self.last_stroke_end = Some((self.doc_key, x, y));
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
                    self.begin_snap(&[]);
                }
                if self.drag == Some(DragKind::Select) && resp.dragged_by(primary) {
                    if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                        self.track_marquee(to_doc(a), to_doc(b));
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Select) {
                    self.drag = None;
                    let tracked = self.marquee_doc();
                    self.end_snap();
                    let press = self.drag_start.take();
                    let release = resp
                        .interact_pointer_pos()
                        .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
                    if let (Some(a), Some(b)) = (press, release) {
                        let canvas = self.editor.doc().canvas();
                        let (da, db) = tracked.unwrap_or((to_doc(a), to_doc(b)));
                        let r = drag_rect(da, db, canvas);
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
        if self.retouch_overlay(ctx, painter, resp) {
            return;
        }
        match self.tool {
            Tool::Brush | Tool::Eraser | Tool::Clone | Tool::Heal => {
                if let (true, Some(p)) = (
                    self.drag.is_none() || self.drag == Some(DragKind::Stroke),
                    resp.hover_pos(),
                ) {
                    let r = (self.brush.radius * self.zoom).max(1.5);
                    if !self.tip_cursor(painter, p) {
                        painter.circle_stroke(p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
                        painter.circle_stroke(p, r, Stroke::new(1.0, Color32::WHITE));
                    }
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
                        // Draw the snapped corners when the drag tracked them.
                        let o = resp.rect.min + self.pan;
                        let (a, b) = self.marquee_doc().map_or((a, b), |(p, q)| {
                            (
                                o + egui::vec2(p.0, p.1) * self.zoom,
                                o + egui::vec2(q.0, q.1) * self.zoom,
                            )
                        });
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
            Tool::Gradient => self.paint_gradient_overlay(painter, resp),
            Tool::Pen => {
                let origin = resp.rect.min + self.pan;
                let zoom = self.zoom;
                let ts = |x: f32, y: f32| egui::pos2(origin.x + x * zoom, origin.y + y * zoom);
                let Some(path) = &self.editor.doc().work_path else {
                    return;
                };
                for (pts, _closed) in path.flatten() {
                    let line: Vec<Pos2> = pts.iter().map(|&(x, y)| ts(x, y)).collect();
                    painter.add(Shape::line(
                        line.clone(),
                        Stroke::new(2.0, Color32::from_black_alpha(140)),
                    ));
                    painter.add(Shape::line(line, Stroke::new(1.2, ACCENT)));
                }
                for (si, sp) in path.subpaths.iter().enumerate() {
                    let open_last = self.pen_open && si + 1 == path.subpaths.len();
                    for (ni, n) in sp.nodes.iter().enumerate() {
                        let c = ts(n.point.0, n.point.1);
                        let first_of_open = open_last && ni == 0 && sp.nodes.len() >= 2;
                        let selected = self.pen_sel == Some((si, ni));
                        let fill = if first_of_open || selected {
                            ACCENT
                        } else {
                            Color32::WHITE
                        };
                        painter.rect_filled(egui::Rect::from_center_size(c, Vec2::splat(6.0)), 1.0, fill);
                        painter.rect_stroke(
                            egui::Rect::from_center_size(c, Vec2::splat(6.0)),
                            1.0,
                            Stroke::new(1.0, Color32::from_black_alpha(180)),
                        );
                        let newest_of_open = open_last && ni + 1 == sp.nodes.len();
                        if (selected || newest_of_open) && n.handle_out != n.point {
                            for h in [n.handle_in, n.handle_out] {
                                let hp = ts(h.0, h.1);
                                painter.line_segment([c, hp], Stroke::new(1.0, MUTED));
                                painter.circle_filled(hp, 2.5, if selected { ACCENT } else { MUTED });
                            }
                        }
                    }
                }
            }
            Tool::Text => self.paint_text_overlay(painter, resp),
            Tool::Crop => self.paint_crop(ctx, painter, resp),
            Tool::Wand if self.quick.object => self.paint_object_select(painter),
            Tool::Wand if self.quick.on => self.paint_quick_select(painter, resp),
            Tool::Shape => self.paint_shape_overlay(painter, resp),
            Tool::Hand | Tool::Move | Tool::Eyedropper | Tool::Bucket | Tool::Wand => {}
        }
    }
}

/// Topmost visible text layer whose rendered glyphs sit under (x, y),
/// searched through groups; the bounds get a small grab margin.
pub(crate) fn text_layer_at(layers: &[Layer], x: f32, y: f32) -> Option<LayerId> {
    for l in layers.iter().rev() {
        if !l.visible {
            continue;
        }
        if let Some(children) = l.children() {
            if let Some(id) = text_layer_at(children, x, y) {
                return Some(id);
            }
            continue;
        }
        let Some(t) = l.text_layer() else {
            continue;
        };
        // Paragraph text: anywhere inside its box.
        if let Some([w, h]) = t.box_size {
            if x >= t.x && y >= t.y && x <= t.x + w && y <= t.y + h {
                return Some(l.id);
            }
        }
        if let Some(b) = l.raster_store().and_then(|s| s.content_bounds()) {
            let pad = 4;
            let grab = Rect::new(b.x - pad, b.y - pad, b.w + 2 * pad as u32, b.h + 2 * pad as u32);
            if grab.contains(x.floor() as i32, y.floor() as i32) {
                return Some(l.id);
            }
        }
    }
    None
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
            *v = lumenply_io::linear_to_srgb(i as f32 / 4095.0);
        }
        t
    })
}

pub(crate) fn to_color32(p: lumenply_tiles::Rgba) -> Color32 {
    let lut = srgb_lut();
    let enc = |v: f32| lut[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize];
    let [r, g, b, a] = p.to_straight();
    Color32::from_rgba_unmultiplied(enc(r), enc(g), enc(b), (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

pub(crate) fn raster_to_image(flat: &Raster) -> egui::ColorImage {
    use rayon::prelude::*;
    egui::ColorImage {
        size: [flat.width as usize, flat.height as usize],
        pixels: flat.pixels.par_iter().map(|p| to_color32(*p)).collect(),
    }
}

/// `store` over `rect` as one raster (`TileStore::to_raster`, a band of
/// tile rows per thread).
pub(crate) fn flatten(store: &lumenply_tiles::TileStore, rect: Rect) -> Raster {
    use rayon::prelude::*;
    let mut out = Raster::new(rect.w, rect.h);
    let w = rect.w as usize;
    if w == 0 || rect.h == 0 {
        return out;
    }
    let ts = lumenply_tiles::TILE_SIZE as i32;
    // Rows of the raster from one tile row to the next.
    let first = rect.y.div_euclid(ts) * ts;
    let mut bands: Vec<(i32, &mut [lumenply_tiles::Rgba])> = Vec::new();
    let mut rest: &mut [lumenply_tiles::Rgba] = &mut out.pixels;
    let mut y = rect.y;
    let mut edge = first + ts;
    while y < rect.bottom() {
        let end = edge.min(rect.bottom());
        let (band, tail) = rest.split_at_mut((end - y) as usize * w);
        bands.push((y, band));
        rest = tail;
        y = end;
        edge += ts;
    }
    bands.into_par_iter().for_each(|(y0, band)| {
        let rows = (band.len() / w) as i32;
        let area = Rect::new(rect.x, y0, rect.w, rows as u32);
        for c in area.tiles() {
            let Some(tile) = store.tile(c) else { continue };
            let (ox, oy) = c.origin();
            let sub = area.intersect(&c.rect());
            for py in sub.y..sub.bottom() {
                let dst = (py - y0) as usize * w + (sub.x - rect.x) as usize;
                for (i, px) in (sub.x..sub.right()).enumerate() {
                    band[dst + i] = tile.get((px - ox) as usize, (py - oy) as usize);
                }
            }
        }
    });
    out
}

/// Sample a layer onto a small thumbnail (nearest, centre of each cell).
pub(crate) fn thumb_image(
    canvas: Rect,
    sample: impl Fn(i32, i32) -> lumenply_tiles::Rgba,
) -> egui::ColorImage {
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
/// Boundary pixels of the selection (the 50% coverage edge), for the
/// animated marching ants. `None` when there is no selection.
pub(crate) fn selection_outline_points(doc: &Document) -> Option<Vec<(i32, i32)>> {
    let sel = doc.selection.as_ref()?;
    let (w, h) = (doc.width as i32, doc.height as i32);
    let area = sel.bounds_within(doc.canvas());
    if area.is_empty() {
        return Some(Vec::new());
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
    let mut pts = Vec::new();
    for gy in 0..gh as i32 {
        for gx in 0..gw as i32 {
            if at(gx, gy) && !(at(gx - 1, gy) && at(gx + 1, gy) && at(gx, gy - 1) && at(gx, gy + 1)) {
                pts.push((x0 + gx, y0 + gy));
            }
        }
    }
    Some(pts)
}

/// The quick-mask overlay: unselected areas tinted red, like the classic
/// mode, so painting the selection reads as painting a mask.
pub(crate) fn quick_mask_overlay(doc: &Document) -> egui::ColorImage {
    let (w, h) = (doc.width as usize, doc.height as usize);
    let mut pixels = vec![Color32::TRANSPARENT; w * h];
    let sel = doc.selection.as_ref();
    for (i, px) in pixels.iter_mut().enumerate() {
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        let cover = sel.map_or(0.0, |s| s.value(x, y));
        let a = ((1.0 - cover) * 128.0) as u8;
        if a > 0 {
            *px = Color32::from_rgba_unmultiplied(220, 40, 40, a);
        }
    }
    egui::ColorImage { size: [w, h], pixels }
}

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
    if let Some(w) = &x.warp {
        let n = WARP_CELLS + 1;
        let at = |i: usize, j: usize| {
            let (px, py) = w[j * n + i];
            to_screen(px, py)
        };
        let mut lines: Vec<Vec<Pos2>> = Vec::new();
        for j in 0..n {
            lines.push((0..n).map(|i| at(i, j)).collect());
        }
        for i in 0..n {
            lines.push((0..n).map(|j| at(i, j)).collect());
        }
        for l in lines {
            painter.add(Shape::line(
                l.clone(),
                Stroke::new(2.0, Color32::from_black_alpha(140)),
            ));
            painter.add(Shape::line(l, Stroke::new(1.0, Color32::WHITE)));
        }
        for &(px, py) in w {
            let p = to_screen(px, py);
            painter.circle_filled(p, 4.0, Color32::WHITE);
            painter.circle_stroke(p, 4.0, Stroke::new(1.0, ACCENT));
        }
        return;
    }
    let pts: Vec<Pos2> = x.corners().iter().map(|(px, py)| to_screen(*px, *py)).collect();
    let mut closed = pts.clone();
    closed.push(pts[0]);
    painter.add(Shape::line(
        closed.clone(),
        Stroke::new(2.0, Color32::from_black_alpha(140)),
    ));
    painter.add(Shape::line(closed, Stroke::new(1.0, Color32::WHITE)));
    // Eight handles, as in Photoshop: corners, and the edge midpoints
    // that stretch one axis (smaller, so the corners still read first).
    for (h, (px, py)) in x.handles() {
        let size = if matches!(h, Handle::Corner(_)) { 8.0 } else { 6.0 };
        let r = egui::Rect::from_center_size(to_screen(px, py), Vec2::splat(size));
        painter.rect_filled(r, 1.0, Color32::WHITE);
        painter.rect_stroke(r, 1.0, Stroke::new(1.0, ACCENT));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn xform(bounds: Rect) -> Xform {
        Xform {
            layer: 1,
            bounds,
            sx: 1.0,
            sy: 1.0,
            shear: 0.0,
            angle: 0.0,
            dx: 0.0,
            dy: 0.0,
            base: (1.0, 1.0, 0.0, 0.0, 0.0),
            quad: None,
            qbase: [(0.0, 0.0); 4],
            warp: None,
            wbase: Vec::new(),
            last_preview: bounds,
        }
    }

    #[test]
    fn a_fresh_warp_mesh_starts_exactly_where_the_box_is() {
        // Scaled, rotated and moved: toggling warp on must not shift a
        // single mesh point, or merely switching modes would distort.
        let mut x = xform(Rect::new(10, 20, 90, 60));
        x.sx = 1.5;
        x.sy = 0.75;
        x.angle = 0.4;
        x.dx = 12.0;
        x.dy = -7.0;
        let a = x.affine();
        let mesh = x.initial_warp();
        let identity = lumenply_render::WarpGrid::identity(x.bounds, WARP_CELLS, WARP_CELLS);
        assert_eq!(mesh.len(), identity.points.len());
        for (m, p) in mesh.iter().zip(&identity.points) {
            let (ex, ey) = a.apply(p.0, p.1);
            assert!(
                (m.0 - ex).abs() < 1e-3 && (m.1 - ey).abs() < 1e-3,
                "mesh point {m:?} vs affine {:?}",
                (ex, ey)
            );
        }
        // With the mesh on, the box corners are the mesh corners and the
        // bounding box covers them.
        x.warp = Some(mesh.clone());
        assert_eq!(x.corners()[0], mesh[0]);
        assert_eq!(x.corners()[2], mesh[mesh.len() - 1]);
        let b = x.bbox();
        assert!(mesh
            .iter()
            .all(|p| b.contains(p.0.floor() as i32, p.1.floor() as i32)));
    }

    fn corner(x: &Xform, i: usize) -> (f32, f32) {
        let c = x.corners()[i];
        ((c.0 * 100.0).round() / 100.0, (c.1 * 100.0).round() / 100.0)
    }

    #[test]
    fn corner_drags_scale_from_the_opposite_corner_and_alt_from_the_centre() {
        // A 200 × 100 box at (100, 100): drag the bottom-right corner
        // (300, 200) to (400, 250), along its diagonal from (100, 100).
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.drag_scale(Handle::Corner(2), (300.0, 200.0), (400.0, 250.0), true, false);
        assert_eq!((x.sx, x.sy), (1.5, 1.5));
        assert_eq!(corner(&x, 0), (100.0, 100.0), "the top-left corner stays");
        assert_eq!(corner(&x, 2), (400.0, 250.0));
        // Shift: each axis follows the pointer on its own.
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.drag_scale(Handle::Corner(2), (300.0, 200.0), (500.0, 200.0), false, false);
        assert_eq!((x.sx, x.sy), (2.0, 1.0));
        assert_eq!(corner(&x, 0), (100.0, 100.0));
        assert_eq!(corner(&x, 2), (500.0, 200.0));
        // Alt: about the centre (200, 150), both sides grow.
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.drag_scale(Handle::Corner(2), (300.0, 200.0), (400.0, 250.0), true, true);
        assert_eq!((x.sx, x.sy), (2.0, 2.0));
        assert_eq!(corner(&x, 0), (0.0, 50.0));
        assert_eq!(corner(&x, 2), (400.0, 250.0));
    }

    #[test]
    fn edge_drags_move_one_side_and_keep_the_other_even_when_turned() {
        // The left edge in to x 150: the right edge stays at 300.
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.drag_scale(Handle::Edge(3), (100.0, 150.0), (150.0, 150.0), false, false);
        assert_eq!((x.sx, x.sy), (0.75, 1.0));
        assert_eq!(corner(&x, 0), (150.0, 100.0));
        assert_eq!(corner(&x, 2), (300.0, 200.0));
        // Turned 90° clockwise about (200, 150): the box spans (150, 50)
        // to (250, 250) and its "bottom" edge (local +y) is on the left at
        // x 150. Dragging it out to x 100 grows only that side.
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.angle = std::f32::consts::FRAC_PI_2;
        x.base.2 = x.angle;
        x.drag_scale(Handle::Edge(2), (150.0, 150.0), (100.0, 150.0), false, false);
        assert!((x.sy - 1.5).abs() < 1e-4 && (x.sx - 1.0).abs() < 1e-4);
        let b = x.bbox();
        assert_eq!((b.x, b.y, b.w, b.h), (100, 50, 150, 200), "{b:?}");
        // Alt: both sides move, about the centre.
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.drag_scale(Handle::Edge(1), (300.0, 150.0), (350.0, 150.0), false, true);
        assert_eq!((x.sx, x.sy), (1.5, 1.0));
        assert_eq!(corner(&x, 0), (50.0, 100.0));
    }

    #[test]
    fn the_box_shows_eight_handles_where_the_canvas_grabs_them() {
        let mut x = xform(Rect::new(100, 100, 200, 100));
        x.drag_scale(Handle::Corner(2), (300.0, 200.0), (400.0, 250.0), true, false);
        let pts: Vec<(f32, f32)> = x.handles().iter().map(|(_, p)| *p).collect();
        assert_eq!(
            pts,
            vec![
                (100.0, 100.0),
                (400.0, 100.0),
                (400.0, 250.0),
                (100.0, 250.0),
                (250.0, 100.0),
                (400.0, 175.0),
                (250.0, 250.0),
                (100.0, 175.0),
            ]
        );
        // Each edge handle is the one that stretches that side only.
        let (h, at) = x.handles()[5];
        assert!(h == Handle::Edge(1));
        let mut y = x.clone();
        y.base = (y.sx, y.sy, y.angle, y.dx, y.dy);
        y.drag_scale(h, at, (500.0, 175.0), false, false);
        assert_eq!((y.corners()[0], y.corners()[2]), ((100.0, 100.0), (500.0, 250.0)));
    }

    #[test]
    fn shift_rotation_snaps_to_fifteen_degrees() {
        let deg = |a: f32| (snap_angle(a.to_radians()).to_degrees() * 1000.0).round() / 1000.0;
        assert_eq!(deg(40.05), 45.0);
        assert_eq!(deg(37.4), 30.0);
        assert_eq!(deg(-8.0), -15.0);
        assert_eq!(deg(7.4), 0.0);
        assert_eq!(deg(91.0), 90.0);
    }
}
