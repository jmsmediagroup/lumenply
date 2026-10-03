//! The Move tool's layer picking and copies, as in Photoshop:
//!
//! - **Auto-Select** (options bar): a click or drag picks the topmost
//!   visible layer with pixels under the pointer. Cmd (Ctrl) held at the
//!   press does the same once with the box unticked, or skips it with
//!   the box ticked.
//! - **Alt-drag** moves a copy of the layer; the copy and its move are
//!   one undo step.

use super::*;
use lumenply_core::layer_ops::DuplicateLayer;

/// Move-tool state kept between frames.
#[derive(Default)]
pub(crate) struct MoveState {
    /// The coalescing key of an Alt-drag copy under way, so the duplicate
    /// and the move it ends with undo together.
    copy_key: Option<String>,
}

/// Coverage below which a pixel doesn't count as part of a layer.
const HIT_ALPHA: f32 = 0.05;

/// The topmost visible layer with content at document pixel (x, y), from
/// `layers` (bottom first), looking inside groups. Pixels, text, shapes,
/// smart objects and fill layers count where they (and their masks) are
/// opaque enough; adjustment and filter layers have no pixels to grab.
pub(crate) fn layer_at(layers: &[Layer], x: i32, y: i32) -> Option<LayerId> {
    for l in layers.iter().rev() {
        if !l.visible {
            continue;
        }
        let mask = l
            .mask
            .as_ref()
            .filter(|m| m.enabled)
            .map_or(1.0, |m| m.value(x, y));
        if mask < HIT_ALPHA {
            continue;
        }
        if let Some(children) = l.children() {
            if let Some(id) = layer_at(children, x, y) {
                return Some(id);
            }
            continue;
        }
        let cover = match &l.content {
            LayerContent::Adjustment(_) | LayerContent::Filter(_) => 0.0,
            LayerContent::Fill(_) => l.raster_store().map_or(1.0, |s| s.get_pixel(x, y).a),
            _ => l.raster_store().map_or(0.0, |s| s.get_pixel(x, y).a),
        };
        if cover * mask >= HIT_ALPHA {
            return Some(l.id);
        }
    }
    None
}

impl App {
    /// A Move-tool press at document point `at`: with Auto-Select on (or
    /// Cmd held with it off) the layer under the pointer becomes active.
    pub(crate) fn move_auto_select(&mut self, ctx: &egui::Context, at: (f32, f32)) {
        let cmd = ctx.input(|i| i.modifiers.command);
        if self.prefs.move_auto_select == cmd {
            return;
        }
        let (x, y) = (at.0.floor() as i32, at.1.floor() as i32);
        let doc = self.editor.doc();
        if x < 0 || y < 0 || x >= doc.width as i32 || y >= doc.height as i32 {
            return;
        }
        if let Some(id) = layer_at(doc.layers(), x, y) {
            if self.active != Some(id) {
                self.set_active(Some(id));
            }
        }
    }

    /// The Auto-Select box in the Move tool's options bar.
    pub(crate) fn auto_select_toggle(&mut self, ui: &mut egui::Ui) {
        let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
        if ui
            .checkbox(&mut self.prefs.move_auto_select, "Auto-Select")
            .on_hover_text(format!(
                "Click or drag picks the layer under the pointer. {cmd}-click does it once \
                 while this is off"
            ))
            .changed()
        {
            self.prefs.save();
        }
    }

    /// Alt pressed as a Move drag starts: duplicate the active layer and
    /// carry on with the copy, in an undo step the move then joins.
    pub(crate) fn begin_move_copy(&mut self) {
        let Some(layer) = self.active else { return };
        let copy = self.editor.doc().next_id();
        let key = format!("move-copy-{copy}");
        self.editor.end_coalescing();
        self.run_coalescing(&DuplicateLayer { layer }, &key);
        if self.editor.doc().layer(copy).is_some() {
            self.set_active(Some(copy));
            self.mover.copy_key = Some(key);
        }
    }

    /// End a Move drag: move `layer` by (dx, dy) (nothing for a drag that
    /// went nowhere), joining an Alt copy's undo step.
    pub(crate) fn finish_move(&mut self, layer: Option<LayerId>, dx: i32, dy: i32) {
        let copy = self.mover.copy_key.take();
        match (layer, dx != 0 || dy != 0) {
            (Some(layer), true) => {
                let cmd = MoveLayer { layer, dx, dy };
                match &copy {
                    Some(key) => self.run_coalescing(&cmd, key),
                    None => self.run(&cmd),
                }
            }
            // The drag went nowhere; drop any preview left on the texture.
            _ => self.mark(None),
        }
        if copy.is_some() {
            self.editor.end_coalescing();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::{Raster, Rgba, TileStore};

    fn block(d: &mut Document, name: &str, r: (i32, i32, u32, u32)) -> LayerId {
        let id = d.add_pixel_layer(name);
        let px = Raster::filled(r.2, r.3, Rgba::new(1.0, 0.0, 0.0, 1.0));
        d.layer_mut(id).unwrap().content = LayerContent::Pixel(TileStore::from_raster(&px, r.0, r.1));
        id
    }

    #[test]
    fn the_topmost_opaque_visible_layer_is_picked() {
        let mut d = Document::new(100, 100);
        let bg = block(&mut d, "bg", (0, 0, 100, 100));
        let a = block(&mut d, "a", (10, 10, 30, 30));
        let b = block(&mut d, "b", (20, 20, 30, 30));
        d.add_adjustment(lumenply_doc::Adjustment::Invert);
        assert_eq!(layer_at(d.layers(), 25, 25), Some(b), "b is on top");
        assert_eq!(layer_at(d.layers(), 12, 12), Some(a), "only a there");
        assert_eq!(layer_at(d.layers(), 90, 90), Some(bg));
        assert_eq!(layer_at(d.layers(), 49, 49), Some(b));
        assert_eq!(layer_at(d.layers(), 50, 50), Some(bg), "b ends at 49");
        // Hidden layers and masked-out pixels don't count.
        d.layer_mut(b).unwrap().visible = false;
        assert_eq!(layer_at(d.layers(), 25, 25), Some(a));
        let mut m = lumenply_doc::Mask::reveal_all();
        m.default = 0.0;
        d.layer_mut(a).unwrap().mask = Some(m);
        assert_eq!(layer_at(d.layers(), 25, 25), Some(bg));
        d.layer_mut(bg).unwrap().visible = false;
        assert_eq!(layer_at(d.layers(), 25, 25), None);
    }

    #[test]
    fn layers_inside_groups_are_picked_and_hidden_groups_skipped() {
        let mut d = Document::new(64, 64);
        let bg = block(&mut d, "bg", (0, 0, 64, 64));
        let g = d.add_group("g");
        let inner = d.alloc_id();
        let mut l = Layer::pixel(inner, "inner");
        l.content = LayerContent::Pixel(TileStore::from_raster(
            &Raster::filled(8, 8, Rgba::new(0.0, 0.0, 1.0, 1.0)),
            4,
            4,
        ));
        d.layer_mut(g).unwrap().children_mut().unwrap().push(l);
        assert_eq!(layer_at(d.layers(), 6, 6), Some(inner));
        assert_eq!(layer_at(d.layers(), 20, 20), Some(bg));
        d.layer_mut(g).unwrap().visible = false;
        assert_eq!(layer_at(d.layers(), 6, 6), Some(bg));
    }

    fn app_with(d: Document) -> App {
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(d), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app
    }

    #[test]
    fn auto_select_picks_on_a_press_and_cmd_toggles_it() {
        let mut d = Document::new(100, 100);
        let bg = block(&mut d, "bg", (0, 0, 100, 100));
        let a = block(&mut d, "a", (10, 10, 30, 30));
        let mut app = app_with(d);
        app.set_active(Some(bg));
        app.prefs.move_auto_select = false;
        let ctx = egui::Context::default();
        let press = |app: &mut App, cmd: bool, at: (f32, f32)| {
            let raw = egui::RawInput {
                modifiers: if cmd {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::NONE
                },
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| app.move_auto_select(ctx, at));
        };
        press(&mut app, false, (15.0, 15.0));
        assert_eq!(app.active, Some(bg), "off and no Cmd: the active layer stays");
        press(&mut app, true, (15.0, 15.0));
        assert_eq!(app.active, Some(a), "Cmd picks the layer under the pointer");
        app.prefs.move_auto_select = true;
        press(&mut app, false, (80.0, 80.0));
        assert_eq!(app.active, Some(bg), "Auto-Select on picks without Cmd");
        press(&mut app, true, (15.0, 15.0));
        assert_eq!(app.active, Some(bg), "Cmd skips it while it is on");
        press(&mut app, false, (-5.0, 15.0));
        assert_eq!(app.active, Some(bg), "off the canvas nothing changes");
    }

    #[test]
    fn an_alt_drag_copy_and_its_move_are_one_undo_step() {
        let mut d = Document::new(100, 100);
        let a = block(&mut d, "a", (10, 10, 20, 20));
        let mut app = app_with(d);
        app.set_active(Some(a));
        let steps = app.editor.history().len();
        app.begin_move_copy();
        let copy = app.active.unwrap();
        assert_ne!(copy, a);
        app.finish_move(Some(copy), 30, 5);
        assert_eq!(app.editor.history().len(), steps + 1, "one step");
        let b = app
            .editor
            .doc()
            .layer(copy)
            .unwrap()
            .pixels()
            .unwrap()
            .content_bounds();
        assert_eq!(b, Some(lumenply_tiles::Rect::new(40, 15, 20, 20)));
        // A nudge afterwards is a step of its own.
        app.tool = Tool::Move;
        app.nudge(1, 0);
        assert_eq!(app.editor.history().len(), steps + 2);
        app.undo();
        app.undo();
        assert_eq!(app.editor.doc().layers().len(), 1, "the copy is gone");
        assert!(app.editor.doc().layer(copy).is_none());
    }
}

/// What a screen reader (and a user) finds in the transform and crop UIs.
#[cfg(test)]
mod transform_ui_tests {
    use super::*;
    use lumenply_tiles::{Raster, Rgba, TileStore};

    /// Every named node after a few frames at `size`: (name, disabled, rect).
    fn nodes(app: &mut App, size: (f32, f32)) -> Vec<(String, bool, egui::Rect)> {
        let ctx = crate::a11y_tests::ctx();
        let mut out = Vec::new();
        for _ in 0..4 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(size.0, size.1))),
                ..Default::default()
            };
            let o = ctx.run(raw, |ctx| app.frame(ctx));
            let update = o.platform_output.accesskit_update.expect("accesskit is on");
            out = update
                .nodes
                .iter()
                .filter_map(|(_, n)| {
                    let b = n.bounds()?;
                    let r = egui::Rect::from_min_max(
                        egui::pos2(b.x0 as f32, b.y0 as f32),
                        egui::pos2(b.x1 as f32, b.y1 as f32),
                    );
                    Some((n.name()?.to_string(), n.is_disabled(), r))
                })
                .collect();
        }
        out
    }

    fn app_with_block() -> App {
        let mut d = Document::new(400, 300);
        let id = d.add_pixel_layer("Red");
        d.layer_mut(id).unwrap().content = LayerContent::Pixel(TileStore::from_raster(
            &Raster::filled(200, 100, Rgba::new(1.0, 0.0, 0.0, 1.0)),
            100,
            100,
        ));
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(Editor::new(d), None);
        app.set_active(Some(id));
        app
    }

    fn named<'a>(all: &'a [(String, bool, egui::Rect)], name: &str) -> Vec<&'a (String, bool, egui::Rect)> {
        all.iter().filter(|n| n.0 == name).collect()
    }

    #[test]
    fn properties_transform_controls_wait_while_free_transform_is_open() {
        let mut app = app_with_block();
        let before = nodes(&mut app, (1440.0, 900.0));
        let rotate = named(&before, "Rotate");
        assert!(
            !rotate.is_empty() && rotate.iter().all(|n| !n.1),
            "Properties' Rotate works"
        );
        app.begin_free_transform();
        let all = nodes(&mut app, (1440.0, 900.0));
        // The options bar (top) keeps its fields; Properties (the dock on
        // the right) waits.
        let dock = |n: &&(String, bool, egui::Rect)| n.2.min.x > 1100.0;
        let bar = named(&all, "Rotate")
            .into_iter()
            .filter(|n| n.2.min.y < 90.0 && !n.1)
            .count();
        assert!(bar >= 1, "the options bar's Rotate works");
        let live: Vec<_> = ["Rotate", "Scale", "Apply", "Flip H", "Free transform…"]
            .iter()
            .flat_map(|name| named(&all, name))
            .filter(dock)
            .filter(|n| !n.1)
            .collect();
        assert_eq!(
            live,
            Vec::<&(String, bool, egui::Rect)>::new(),
            "Properties waits"
        );
        let greyed = named(&all, "Rotate")
            .into_iter()
            .filter(dock)
            .filter(|n| n.1)
            .count();
        assert!(greyed >= 1, "Properties' Rotate is there, greyed out");
    }

    #[test]
    fn the_free_transform_bar_fits_a_900_point_window() {
        let mut app = app_with_block();
        app.begin_free_transform();
        let all = nodes(&mut app, (900.0, 600.0));
        for name in ["Width", "Height", "Rotate", "Skew", "Apply", "Cancel"] {
            let n = named(&all, name).into_iter().find(|n| n.2.min.y < 90.0);
            let n = n.unwrap_or_else(|| panic!("no {name} in the options bar"));
            assert!(n.2.max.x <= 900.0, "{name} ends at {} (window 900)", n.2.max.x);
        }
    }

    #[test]
    fn crop_and_content_aware_scale_name_their_commit_buttons() {
        let mut app = app_with_block();
        app.tool = Tool::Crop;
        let all = nodes(&mut app, (1440.0, 900.0));
        assert_eq!(named(&all, "Crop to frame").len(), 1);
        app.tool = Tool::Move;
        app.open_cas();
        assert!(app.cas.is_some(), "the workspace opened");
        let all = nodes(&mut app, (1440.0, 900.0));
        assert_eq!(named(&all, "Apply").len(), 1, "Apply, as Free Transform says");
        assert!(named(&all, "Commit").is_empty());
    }
}
