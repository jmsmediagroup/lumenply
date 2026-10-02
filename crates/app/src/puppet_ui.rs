//! Edit ▸ Puppet Warp: a full-window workspace, as Liquify is. The active
//! pixel layer sits over the composite below it with its mesh; a click on
//! the mesh drops a pin, dragging a pin warps the layer live (solved and
//! drawn at preview resolution), Alt-click or Delete removes one. OK bakes
//! at full size as one `PuppetWarp` undo step.

use std::sync::Arc;

use super::*;
use lumenply_core::puppet::PuppetWarp;
use lumenply_render::puppet::{
    MeshPainter, MeshParams, Pt, PuppetDensity, PuppetMesh, PuppetMode, PuppetPin, PuppetSolver,
};
use lumenply_tiles::{Rgba, TileStore};
use rayon::prelude::*;

/// Longest preview side in pixels (as Liquify).
const PREVIEW_MAX: u32 = 1400;
/// How close (screen px) a press must be to grab a pin.
const PIN_HIT: f32 = 9.0;
/// Pin dot radius on screen.
const PIN_R: f32 = 5.5;
/// Pin edits kept for Cmd+Z inside the workspace.
const UNDO_MAX: usize = 200;

pub(crate) struct PuppetState {
    layer: LayerId,
    layer_name: String,
    pub(crate) mode: PuppetMode,
    pub(crate) density: PuppetDensity,
    /// Mesh margin around the opaque pixels, canvas px.
    pub(crate) expansion: f32,
    pub(crate) show_mesh: bool,
    pub(crate) pins: Vec<PuppetPin>,
    pub(crate) selected: Option<usize>,
    /// Pin lists before each edit (Cmd+Z inside the workspace).
    undo: Vec<Vec<PuppetPin>>,
    /// The layer's pixels (shared tiles), for remeshing.
    store: TileStore,
    mesh: Arc<PuppetMesh>,
    /// Unique mesh edges, for the overlay.
    edges: Vec<(u32, u32)>,
    solver: PuppetSolver,
    /// Deformed vertex positions, canvas px.
    pub(crate) pos: Vec<Pt>,
    order: Vec<u32>,
    /// The layer over the canvas at `scale` (premultiplied linear).
    src: Raster,
    /// The composite below the layer, same size.
    backdrop: Raster,
    opacity: f32,
    scale: f32,
    canvas: Rect,
    tex: Option<egui::TextureHandle>,
    dirty: bool,
    /// The pin being dragged, its offset from the pointer (canvas px),
    /// and whether the drag is already on the undo stack.
    drag: Option<(usize, [f64; 2], bool)>,
    /// Milliseconds of the last solve, and of the last preview render.
    pub(crate) solve_ms: f32,
    pub(crate) draw_ms: f32,
}

/// The unique edges of a mesh.
fn mesh_edges(mesh: &PuppetMesh) -> Vec<(u32, u32)> {
    let mut e: Vec<(u32, u32)> = mesh
        .tris
        .iter()
        .flat_map(|t| (0..3).map(move |k| (t[k].min(t[(k + 1) % 3]), t[k].max(t[(k + 1) % 3]))))
        .collect();
    e.sort_unstable();
    e.dedup();
    e
}

impl PuppetState {
    fn params(&self) -> MeshParams {
        MeshParams {
            density: self.density,
            expansion: self.expansion,
        }
    }

    /// Rebuilds the mesh (Density or Expansion changed); pins keep their
    /// rest points.
    pub(crate) fn remesh(&mut self) {
        if let Some(m) = PuppetMesh::build(&self.store, self.params()) {
            self.mesh = Arc::new(m);
            self.edges = mesh_edges(&self.mesh);
            self.refactor();
        }
    }

    /// Re-factors the solver (pins added or removed), then re-solves.
    fn refactor(&mut self) {
        let from: Vec<Pt> = self.pins.iter().map(|p| p.from).collect();
        self.solver = PuppetSolver::new(self.mesh.clone(), &from);
        self.resolve();
    }

    /// Re-solves for the current pin targets.
    pub(crate) fn resolve(&mut self) {
        let t = std::time::Instant::now();
        let to: Vec<Pt> = self.pins.iter().map(|p| p.to).collect();
        self.pos = self.solver.solve(&to, self.mode);
        self.order = self.mesh.draw_order(&self.pins);
        self.solve_ms = t.elapsed().as_secs_f32() * 1000.0;
        self.dirty = true;
    }

    fn checkpoint(&mut self) {
        self.undo.push(self.pins.clone());
        if self.undo.len() > UNDO_MAX {
            self.undo.remove(0);
        }
    }

    pub(crate) fn undo_pins(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.pins = prev;
            self.selected = None;
            self.drag = None;
            self.refactor();
        }
    }

    /// Drops a pin on the warped mesh at canvas point `at`; `None` off the
    /// mesh. The new pin is selected.
    pub(crate) fn add_pin(&mut self, at: Pt) -> Option<usize> {
        let m = self.mesh.locate_deformed(&self.pos, &self.order, at)?;
        let from = self.mesh.point_in(&self.mesh.rest, m);
        self.checkpoint();
        self.pins.push(PuppetPin {
            from,
            to: at,
            depth: 0,
        });
        self.selected = Some(self.pins.len() - 1);
        self.refactor();
        self.selected
    }

    pub(crate) fn remove_pin(&mut self, k: usize) {
        if k < self.pins.len() {
            self.checkpoint();
            self.pins.remove(k);
            self.selected = None;
            self.drag = None;
            self.refactor();
        }
    }

    pub(crate) fn move_pin(&mut self, k: usize, to: Pt) {
        if let Some(p) = self.pins.get_mut(k) {
            p.to = to;
            self.resolve();
        }
    }

    /// Raises (or lowers) the selected pin's depth: its part of the mesh
    /// draws over (under) the rest where they overlap.
    pub(crate) fn shift_depth(&mut self, by: i32) {
        if let Some(k) = self.selected.filter(|&k| k < self.pins.len()) {
            self.checkpoint();
            self.pins[k].depth += by;
            self.resolve();
        }
    }

    pub(crate) fn clear_pins(&mut self) {
        if !self.pins.is_empty() {
            self.checkpoint();
            self.pins.clear();
            self.selected = None;
            self.refactor();
        }
    }

    /// The pin nearest canvas point `c` within `reach` canvas px.
    fn pin_at(&self, c: Pt, reach: f64) -> Option<usize> {
        self.pins
            .iter()
            .enumerate()
            .map(|(k, p)| (k, (p.to[0] - c[0]).hypot(p.to[1] - c[1])))
            .filter(|(_, d)| *d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }

    /// The command OK runs.
    pub(crate) fn command(&self) -> PuppetWarp {
        PuppetWarp {
            layer: self.layer,
            mesh: self.params(),
            mode: self.mode,
            pins: self.pins.clone(),
        }
    }

    /// The warped layer over the backdrop at preview size.
    fn render(&mut self) -> egui::ColorImage {
        let t = std::time::Instant::now();
        let (ox, oy, s) = (self.canvas.x as f64, self.canvas.y as f64, self.scale as f64);
        let to_prev = |p: &Pt| [(p[0] - ox) * s, (p[1] - oy) * s];
        let from: Vec<Pt> = self.mesh.rest.iter().map(to_prev).collect();
        let to: Vec<Pt> = self.pos.iter().map(to_prev).collect();
        let mut warped = Raster::new(self.src.width, self.src.height);
        MeshPainter::new(&self.src, (0, 0), &from, &to, &self.mesh.tris, &self.order)
            .paint_raster(&mut warped, (0, 0));
        let op = self.opacity;
        warped
            .pixels
            .par_iter_mut()
            .zip(self.backdrop.pixels.par_iter())
            .for_each(|(w, b)| {
                let k = 1.0 - w.a * op;
                *w = Rgba::new(
                    w.r * op + b.r * k,
                    w.g * op + b.g * k,
                    w.b * op + b.b * k,
                    w.a * op + b.a * k,
                );
            });
        let img = liquify::to_image(&warped);
        self.draw_ms = t.elapsed().as_secs_f32() * 1000.0;
        img
    }
}

/// The composite of what lies under `layer`: everything below its
/// top-level ancestor, plus that group's other contents.
fn composite_below(doc: &Document, layer: LayerId, rect: Rect) -> TileStore {
    let mut top = layer;
    while let Some(p) = doc.parent_of(top) {
        top = p;
    }
    let Some(i) = doc.layers().iter().position(|l| l.id == top) else {
        return TileStore::new();
    };
    let mut d = doc.clone();
    if let Some(l) = d.layer_mut(layer) {
        l.visible = false;
    }
    lumenply_render::composite_layers(&d.layers()[..=i], rect, d.canvas())
}

impl App {
    /// Why Puppet Warp can't open on the active layer, if it can't.
    pub(crate) fn puppet_block(&self) -> Option<&'static str> {
        let Some(layer) = self.active_layer() else {
            return Some("Select a pixel layer first");
        };
        if layer.smart_layer().is_some()
            || layer.text_layer().is_some()
            || layer.shape_layer().is_some()
            || layer.fill_layer().is_some()
        {
            return Some("Rasterize the layer first");
        }
        if !self.active_is_pixel() {
            return Some("Select a pixel layer first");
        }
        self.lock_block(layer_actions::LockNeed::Reshape)
    }

    /// Opens the workspace on the active pixel layer.
    pub(crate) fn open_puppet(&mut self) {
        let Some(id) = self.active else { return };
        let doc = self.editor.doc();
        let Some(layer) = doc.layer(id) else { return };
        let Some(store) = layer.pixels() else { return };
        let params = MeshParams::default();
        let Some(mesh) = PuppetMesh::build(store, params) else {
            self.status = "Puppet Warp needs a layer with pixels".into();
            return;
        };
        let canvas = doc.canvas();
        let scale = (PREVIEW_MAX as f32 / canvas.w.max(canvas.h) as f32).min(1.0);
        let src = liquify::downscale(store, canvas, scale);
        let below = composite_below(doc, id, canvas);
        let backdrop = liquify::downscale(&below, canvas, scale);
        let mesh = Arc::new(mesh);
        let solver = PuppetSolver::new(mesh.clone(), &[]);
        let mut st = PuppetState {
            layer: id,
            layer_name: layer.name.clone(),
            mode: PuppetMode::Normal,
            density: params.density,
            expansion: params.expansion,
            show_mesh: true,
            pins: Vec::new(),
            selected: None,
            undo: Vec::new(),
            store: store.clone(),
            edges: mesh_edges(&mesh),
            pos: mesh.rest.clone(),
            order: (0..mesh.tris.len() as u32).collect(),
            mesh,
            solver,
            src,
            backdrop,
            opacity: layer.opacity,
            scale,
            canvas,
            tex: None,
            dirty: true,
            drag: None,
            solve_ms: 0.0,
            draw_ms: 0.0,
        };
        st.resolve();
        self.puppet = Some(Box::new(st));
    }

    /// Bakes the warp (when any pin moved) and closes the workspace.
    fn finish_puppet(&mut self, st: Box<PuppetState>) {
        let cmd = st.command();
        if cmd.is_identity() {
            self.status = "Puppet Warp: nothing moved".into();
            return;
        }
        let t = std::time::Instant::now();
        self.run(&cmd);
        self.status = format!(
            "Puppet Warp applied in {:.0} ms",
            t.elapsed().as_secs_f32() * 1000.0
        );
    }

    /// The workspace; replaces the editor UI while open.
    pub(crate) fn puppet_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.puppet.take() else { return };
        let mut done: Option<bool> = None;
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(Key::Escape) {
                    done = Some(false);
                }
                if i.key_pressed(Key::Enter) {
                    done = Some(true);
                }
                if i.modifiers.command && i.key_pressed(Key::Z) {
                    st.undo_pins();
                }
                if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) {
                    if let Some(k) = st.selected {
                        st.remove_pin(k);
                    }
                }
            });
        }

        egui::TopBottomPanel::top("puppet-bar")
            .frame(bar_frame())
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Puppet Warp").strong().color(TEXT));
                    ui.label(RichText::new(&st.layer_name).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                "Click to pin  ·  drag a pin to warp  ·  Alt-click or Delete removes",
                            )
                            .color(MUTED),
                        );
                    });
                });
            });

        egui::SidePanel::right("puppet-props")
            .resizable(false)
            .exact_width(268.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                ui.label(RichText::new("MODE").small().strong().color(MUTED));
                ui.add_space(4.0);
                let modes: Vec<(PuppetMode, &str)> = PuppetMode::ALL.iter().map(|m| (*m, m.name())).collect();
                if segmented(ui, &mut st.mode, &modes) {
                    st.resolve();
                }
                ui.add_space(10.0);
                ui.label(RichText::new("DENSITY").small().strong().color(MUTED));
                ui.add_space(4.0);
                let dens = [
                    (PuppetDensity::Fewer, "Fewer"),
                    (PuppetDensity::Normal, "Normal"),
                    (PuppetDensity::More, "More"),
                ];
                if segmented(ui, &mut st.density, &dens) {
                    st.remesh();
                }
                ui.add_space(6.0);
                let before = st.expansion;
                let opts = RowOpts {
                    label_w: 70.0,
                    int: true,
                    ..RowOpts::default()
                };
                slider_row_ex(ui, "Expansion", &mut st.expansion, 0.0..=50.0, " px", opts);
                if st.expansion != before {
                    st.remesh();
                }
                ui.add_space(10.0);
                ui.label(RichText::new("VIEW").small().strong().color(MUTED));
                ui.add_space(4.0);
                check(ui, &mut st.show_mesh, "Show mesh");
                ui.add_space(10.0);
                ui.label(RichText::new("PINS").small().strong().color(MUTED));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let sel = st.selected.is_some();
                    if ui
                        .add_enabled(sel, egui::Button::new("Depth +"))
                        .on_hover_text("The selected pin's part draws on top where the mesh overlaps")
                        .clicked()
                    {
                        st.shift_depth(1);
                    }
                    if ui
                        .add_enabled(sel, egui::Button::new("Depth −"))
                        .on_hover_text("The selected pin's part draws underneath")
                        .clicked()
                    {
                        st.shift_depth(-1);
                    }
                    if ui
                        .add_enabled(sel, egui::Button::new("Remove"))
                        .on_hover_text("Remove the selected pin (Delete)")
                        .clicked()
                    {
                        if let Some(k) = st.selected {
                            st.remove_pin(k);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!st.pins.is_empty(), egui::Button::new("Remove all pins"))
                        .on_hover_text("Back to the unwarped layer (Cmd+Z brings the pins back)")
                        .clicked()
                    {
                        st.clear_pins();
                    }
                    if ui
                        .add_enabled(!st.undo.is_empty(), egui::Button::new("Undo"))
                        .on_hover_text("Undo the last pin edit (Cmd+Z)")
                        .clicked()
                    {
                        st.undo_pins();
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{} pins · {} mesh points\nsolve {:.1} ms · draw {:.0} ms",
                        st.pins.len(),
                        st.mesh.rest.len(),
                        st.solve_ms,
                        st.draw_ms
                    ))
                    .small()
                    .color(MUTED),
                );
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(primary_button("OK")).clicked() {
                            done = Some(true);
                        }
                        if ui.add(footer_button("Cancel")).clicked() {
                            done = Some(false);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(avail, Sense::click_and_drag());
                a11y_name(&resp, "Puppet Warp preview");
                let (cw, ch) = (st.canvas.w as f32, st.canvas.h as f32);
                let disp = ((avail.width() - 48.0) / cw)
                    .min((avail.height() - 48.0) / ch)
                    .max(0.01);
                let img = egui::Rect::from_center_size(avail.center(), egui::vec2(cw * disp, ch * disp));
                if st.dirty || st.tex.is_none() {
                    let image = st.render();
                    match &mut st.tex {
                        Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            st.tex = Some(ctx.load_texture("puppet", image, egui::TextureOptions::LINEAR))
                        }
                    }
                    st.dirty = false;
                }
                let p = ui.painter_at(avail);
                if let Some(t) = &st.tex {
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    p.image(t.id(), img, uv, Color32::WHITE);
                }
                p.rect_stroke(img, 0.0, Stroke::new(1.0, LINE));
                let (ox, oy) = (st.canvas.x as f64, st.canvas.y as f64);
                let to_screen = |c: Pt| {
                    egui::pos2(
                        img.min.x + ((c[0] - ox) as f32) * disp,
                        img.min.y + ((c[1] - oy) as f32) * disp,
                    )
                };
                let to_canvas = |s: Pos2| -> Pt {
                    [
                        ox + ((s.x - img.min.x) / disp) as f64,
                        oy + ((s.y - img.min.y) / disp) as f64,
                    ]
                };
                if st.show_mesh {
                    let ink = Stroke::new(1.0, Color32::from_rgba_unmultiplied(225, 228, 232, 110));
                    let shapes: Vec<Shape> = st
                        .edges
                        .iter()
                        .map(|&(a, b)| {
                            Shape::line_segment(
                                [to_screen(st.pos[a as usize]), to_screen(st.pos[b as usize])],
                                ink,
                            )
                        })
                        .collect();
                    p.extend(shapes);
                }
                let (pressed, down, alt, pointer) = ctx.input(|i| {
                    (
                        i.pointer.primary_pressed(),
                        i.pointer.primary_down(),
                        i.modifiers.alt,
                        i.pointer.interact_pos(),
                    )
                });
                let hover_pin = resp
                    .hover_pos()
                    .and_then(|h| st.pin_at(to_canvas(h), (PIN_HIT / disp) as f64));
                if pressed && resp.hovered() {
                    if let Some(at) = pointer.map(to_canvas) {
                        match st.pin_at(at, (PIN_HIT / disp) as f64) {
                            Some(k) if alt => st.remove_pin(k),
                            Some(k) => {
                                st.selected = Some(k);
                                let p = st.pins[k].to;
                                st.drag = Some((k, [p[0] - at[0], p[1] - at[1]], false));
                            }
                            None if !alt => {
                                if let Some(k) = st.add_pin(at) {
                                    // Adding the pin was the undo step;
                                    // dragging it on needs no other.
                                    st.drag = Some((k, [0.0, 0.0], true));
                                }
                            }
                            None => {}
                        }
                    }
                }
                if let Some((k, off, saved)) = st.drag {
                    match pointer.filter(|_| down) {
                        Some(at) => {
                            let c = to_canvas(at);
                            let to = [c[0] + off[0], c[1] + off[1]];
                            if st.pins.get(k).is_some_and(|p| p.to != to) {
                                if !saved {
                                    st.checkpoint();
                                    st.drag = Some((k, off, true));
                                }
                                st.move_pin(k, to);
                            }
                            ctx.request_repaint();
                        }
                        None => st.drag = None,
                    }
                }
                for (k, pin) in st.pins.iter().enumerate() {
                    let c = to_screen(pin.to);
                    let hot = hover_pin == Some(k) || st.drag.is_some_and(|d| d.0 == k);
                    let r = if hot { PIN_R + 1.5 } else { PIN_R };
                    p.circle_filled(c, r + 1.5, Color32::from_black_alpha(200));
                    p.circle_filled(c, r, Color32::from_rgb(255, 214, 0));
                    if st.selected == Some(k) {
                        p.circle_filled(c, r * 0.45, Color32::BLACK);
                    }
                }
                if resp.hovered() {
                    let icon = if hover_pin.is_some() {
                        egui::CursorIcon::Grab
                    } else {
                        egui::CursorIcon::Crosshair
                    };
                    ctx.set_cursor_icon(if st.drag.is_some() {
                        egui::CursorIcon::Grabbing
                    } else {
                        icon
                    });
                }
            });

        match done {
            Some(true) => self.finish_puppet(st),
            Some(false) => self.status = "Puppet Warp cancelled".into(),
            None => self.puppet = Some(st),
        }
    }

    /// `puppet:open` opens the workspace on the active layer;
    /// `puppet:pin=X:Y` drops a pin at canvas point (X, Y) of the warped
    /// mesh; `puppet:drag=I:X:Y` moves pin I there; `puppet:select=I`;
    /// `puppet:mesh` shows the mesh, `puppet:nomesh` hides it;
    /// `puppet:mode=rigid|normal|distort`; `puppet:density=fewer|normal|more`;
    /// `puppet:expansion=N`; `puppet:ok` commits.
    pub(crate) fn debug_puppet(&mut self, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("puppet:") else {
            return false;
        };
        if self.puppet.is_none() && rest != "ok" {
            self.open_puppet();
        }
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f64> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        if verb == "ok" {
            if let Some(st) = self.puppet.take() {
                self.finish_puppet(st);
            }
            return true;
        }
        let Some(st) = self.puppet.as_mut() else {
            return true;
        };
        match (verb, nums.as_slice()) {
            ("open", _) => {}
            ("pin", [x, y]) => {
                st.add_pin([*x, *y]);
            }
            ("drag", [i, x, y]) => {
                let k = *i as usize;
                st.selected = Some(k);
                st.move_pin(k, [*x, *y]);
            }
            ("select", [i]) => st.selected = Some(*i as usize),
            ("mesh", _) => st.show_mesh = true,
            ("nomesh", _) => st.show_mesh = false,
            ("mode", _) => {
                if let Some(m) = PuppetMode::ALL
                    .into_iter()
                    .find(|m| m.name().eq_ignore_ascii_case(arg))
                {
                    st.mode = m;
                    st.resolve();
                }
            }
            ("density", _) => {
                let d = match arg {
                    "fewer" => PuppetDensity::Fewer,
                    "more" => PuppetDensity::More,
                    _ => PuppetDensity::Normal,
                };
                st.density = d;
                st.remesh();
            }
            ("expansion", [e]) => {
                st.expansion = *e as f32;
                st.remesh();
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 120 × 80 document whose layer holds a 60 × 12 bar at (30, 34).
    fn launch_bar() -> App {
        let mut doc = Document::new(120, 80);
        let id = doc.add_pixel_layer("Bar");
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() =
            TileStore::from_raster(&Raster::filled(60, 12, Rgba::new(1.0, 0.0, 0.0, 1.0)), 30, 34);
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        let bar = app.editor.doc().layers()[0].id;
        app.set_active(Some(bar));
        app
    }

    #[test]
    fn pins_warp_the_layer_and_ok_is_one_undo_step() {
        let mut app = launch_bar();
        let id = app.active.unwrap();
        let steps = app.editor.history().len();
        app.run_menu_action("puppet-warp");
        let st = app.puppet.as_mut().expect("open");
        assert_eq!(st.scale, 1.0, "small canvases preview at full size");
        assert_eq!(st.add_pin([34.0, 40.0]), Some(0));
        assert_eq!(st.add_pin([86.0, 40.0]), Some(1));
        assert_eq!(st.add_pin([10.0, 10.0]), None, "off the mesh: no pin");
        // Both pins down 10 px: the bar moves down 10 px.
        st.move_pin(0, [34.0, 50.0]);
        st.move_pin(1, [86.0, 50.0]);
        assert!(app.debug_puppet("puppet:ok"));
        assert!(app.puppet.is_none());
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last(), Some(&"Puppet Warp"));
        let px = |app: &App, x, y| {
            app.editor
                .doc()
                .layer(id)
                .unwrap()
                .pixels()
                .unwrap()
                .get_pixel(x, y)
        };
        assert!((px(&app, 60, 50).r - 1.0).abs() < 1e-3, "{:?}", px(&app, 60, 50));
        assert_eq!(px(&app, 60, 36).a, 0.0, "the old place is empty");
    }

    #[test]
    fn pin_edits_undo_inside_the_workspace_and_cancel_changes_nothing() {
        let mut app = launch_bar();
        let steps = app.editor.history().len();
        app.run_menu_action("puppet-warp");
        let st = app.puppet.as_mut().unwrap();
        st.add_pin([40.0, 40.0]);
        st.add_pin([80.0, 40.0]);
        st.remove_pin(0);
        assert_eq!(st.pins.len(), 1);
        st.undo_pins();
        assert_eq!(st.pins.len(), 2, "undo brings the removed pin back");
        st.clear_pins();
        assert!(st.pins.is_empty());
        app.puppet = None; // Cancel
        assert_eq!(app.editor.history().len(), steps);
    }

    #[test]
    fn puppet_warp_is_blocked_where_it_cannot_work() {
        let mut app = launch_bar();
        assert_eq!(app.action_block("puppet-warp"), None);
        app.add_adjustment(lumenply_doc::Adjustment::Invert);
        assert_eq!(
            app.action_block("puppet-warp"),
            Some("Select a pixel layer first")
        );
        app.run_menu_action("puppet-warp");
        assert!(app.puppet.is_none());
    }

    #[test]
    fn every_control_in_the_workspace_has_a_spoken_name() {
        let mut app = launch_bar();
        app.run_menu_action("puppet-warp");
        app.puppet.as_mut().unwrap().add_pin([40.0, 40.0]);
        let ctx = crate::a11y_tests::ctx();
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.puppet.is_some(), "still open");
        assert_eq!(missing, Vec::<String>::new());
    }
}
