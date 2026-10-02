//! Photoshop-standard layer operations in the shell: duplicate, merge
//! down / merge group / merge clipping mask, merge visible, flatten and
//! stamp visible. The commands live in `lumenply_core::layer_ops`; this
//! module picks the target layer, keeps the active layer pointing at the
//! result, and says why an operation can't run.

use super::*;
use lumenply_core::layer_ops::{self as ops, MergeKind};
use lumenply_core::locks::{effective_locks, SetLayerLocks};
use lumenply_doc::LayerLocks;

/// What an edit would do to the active layer, for lock checks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockNeed {
    /// Change its pixels (paint, fill, filters, rasterize).
    Paint,
    /// Shift it by whole pixels (the Move tool).
    Move,
    /// Scale, rotate, flip, distort or warp it.
    Reshape,
    /// Change its opacity, blend, mask, clipping or effects.
    Props,
}

/// The four lock toggles of the Layers header, in Photoshop's order.
const LOCK_TOGGLES: [(LockKind, &str); 4] = [
    (LockKind::Transparency, "Lock transparent pixels"),
    (LockKind::Pixels, "Lock image pixels"),
    (LockKind::Position, "Lock position"),
    (LockKind::All, "Lock all"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum LockKind {
    Transparency,
    Pixels,
    Position,
    All,
}

impl LockKind {
    fn get(self, l: &LayerLocks) -> bool {
        match self {
            LockKind::Transparency => l.transparency,
            LockKind::Pixels => l.pixels,
            LockKind::Position => l.position,
            LockKind::All => l.all,
        }
    }

    fn set(self, l: &mut LayerLocks, on: bool) {
        match self {
            LockKind::Transparency => l.transparency = on,
            LockKind::Pixels => l.pixels = on,
            LockKind::Position => l.position = on,
            LockKind::All => l.all = on,
        }
    }
}

impl App {
    /// Duplicate the active layer directly above itself and make the copy
    /// active (Layer ▸ Duplicate, Ctrl/Cmd+J without a selection).
    pub(crate) fn duplicate_active(&mut self) {
        let Some(layer) = self.active else { return };
        let copy = self.editor.doc().next_id();
        self.run(&ops::DuplicateLayer { layer });
        if self.editor.doc().layer(copy).is_some() {
            self.set_active(Some(copy));
            self.status = format!(
                "Duplicated as \"{}\"",
                self.active_layer().map_or("", |l| l.name.as_str())
            );
        }
    }

    /// The action Ctrl/Cmd+J runs: "layer via copy" with a selection,
    /// "duplicate layer" without one.
    pub(crate) fn cmd_j_action(&self) -> &'static str {
        if self.editor.doc().selection.is_some() {
            "layer-via-copy"
        } else {
            "duplicate-layer"
        }
    }

    /// What Ctrl/Cmd+E would do with the active layer, or why it can't.
    pub(crate) fn merge_kind(&self) -> Result<MergeKind, &'static str> {
        let id = self.active.ok_or("Select a layer first")?;
        ops::merge_down_kind(self.editor.doc(), id)
    }

    /// The Merge Down menu label for the active layer: "Merge down",
    /// "Merge group" or "Merge clipping mask", as in Photoshop.
    pub(crate) fn merge_label(&self) -> &'static str {
        self.merge_kind().map_or("Merge down", MergeKind::label)
    }

    /// Ctrl/Cmd+E: merge the active layer down (or its group, or the
    /// layers clipped to it) and keep the result active.
    pub(crate) fn merge_down_active(&mut self) {
        let Some(layer) = self.active else { return };
        let Ok(kind) = self.merge_kind() else { return };
        let result = match kind {
            MergeKind::Down => {
                let doc = self.editor.doc();
                let list = match doc.parent_of(layer) {
                    None => doc.layers(),
                    Some(p) => doc.layer(p).and_then(|g| g.children()).unwrap_or(&[]),
                };
                let i = list.iter().position(|l| l.id == layer).unwrap_or(0);
                list.get(i.wrapping_sub(1)).map_or(layer, |l| l.id)
            }
            MergeKind::Group | MergeKind::ClippingMask => layer,
        };
        self.run(&ops::MergeDown { layer });
        if self.editor.doc().layer(result).is_some() {
            self.set_active(Some(result));
        }
    }

    /// Shift+Ctrl/Cmd+E: merge every visible layer into one.
    pub(crate) fn merge_visible(&mut self) {
        let result = self
            .editor
            .doc()
            .layers()
            .iter()
            .find(|l| l.visible)
            .map(|l| l.id);
        self.run(&ops::MergeVisible);
        if result.is_some_and(|id| self.editor.doc().layer(id).is_some()) {
            self.set_active(result);
        }
    }

    /// Layer ▸ Flatten image: one opaque "Background" layer.
    pub(crate) fn flatten_image(&mut self) {
        let id = self.editor.doc().next_id();
        self.run(&ops::FlattenImage);
        if self.editor.doc().layer(id).is_some() {
            self.set_active(Some(id));
        }
    }

    /// Shift+Alt+Ctrl/Cmd+E: the visible composite on a new layer on top.
    pub(crate) fn stamp_visible(&mut self) {
        let id = self.editor.doc().next_id();
        let n = self.editor.doc().layer_count() + 1;
        self.run(&ops::StampVisible {
            name: format!("Stamp {n}"),
        });
        if self.editor.doc().layer(id).is_some() {
            self.set_active(Some(id));
        }
    }

    /// Why one of this module's actions can't run (see
    /// [`App::action_block`]); `None` for ids it doesn't own.
    pub(crate) fn layer_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        let doc = self.editor.doc();
        // Actions the active layer's locks refuse say so first; otherwise
        // the usual checks in `action_block` decide.
        let need = match id {
            "fill" | "smart-object" | "rasterize" => Some(LockNeed::Paint),
            id if id.starts_with("filter-") => Some(LockNeed::Paint),
            "clear" => {
                let l = self.active.map(|a| effective_locks(doc, a));
                if l.is_some_and(|l| l.transparency && !l.pixels) {
                    return Some(Some("The layer's transparency is locked"));
                }
                Some(LockNeed::Paint)
            }
            "xform" | "perspective" | "warp" | "flip-h" | "flip-v" => Some(LockNeed::Reshape),
            "add-mask" | "rm-mask" | "mask-toggle" | "mask-from-sel" | "clip" | "unclip" => {
                Some(LockNeed::Props)
            }
            _ => None,
        };
        if let Some(need) = need {
            return self.lock_block(need).map(Some);
        }
        Some(match id {
            "duplicate-layer" => self.active_layer().is_none().then_some("Select a layer first"),
            "merge-down" => self.merge_kind().err(),
            "merge-visible" => ops::merge_visible_block(doc),
            "flatten" => doc
                .layers()
                .is_empty()
                .then_some("There are no layers to flatten"),
            "stamp-visible" => ops::stamp_visible_block(doc),
            "lock-transparency" | "lock-pixels" | "lock-position" | "lock-all" => match self.active_layer() {
                None => Some("Select a layer first"),
                Some(l) if l.locks.all && id != "lock-all" => Some("Lock all already covers this"),
                Some(_) => None,
            },
            _ => return None,
        })
    }

    /// Why the active layer's locks refuse `need`, if they do.
    pub(crate) fn lock_block(&self, need: LockNeed) -> Option<&'static str> {
        let l = effective_locks(self.editor.doc(), self.active?);
        if l.is_empty() {
            return None;
        }
        let all = "The layer is locked";
        match need {
            _ if l.all => Some(all),
            LockNeed::Paint if l.pixels => Some("The layer's pixels are locked"),
            LockNeed::Move | LockNeed::Reshape if l.position => Some("The layer's position is locked"),
            LockNeed::Reshape if l.pixels => Some("The layer's pixels are locked"),
            LockNeed::Reshape if l.transparency => Some("The layer's transparency is locked"),
            _ => None,
        }
    }

    /// Why a brush-type stroke can't start on the active layer: on its
    /// mask only "lock all" refuses, on its pixels a pixel lock does.
    pub(crate) fn paint_lock_block(&self) -> Option<&'static str> {
        if self.editing_mask {
            self.lock_block(LockNeed::Props)
        } else {
            self.lock_block(LockNeed::Paint)
        }
    }

    /// Flip one of the active layer's locks.
    fn toggle_lock(&mut self, kind: LockKind) {
        let Some(l) = self.active_layer() else { return };
        let (layer, mut locks) = (l.id, l.locks);
        let on = !kind.get(&locks);
        kind.set(&mut locks, on);
        self.run(&SetLayerLocks { layer, locks });
        self.status = if locks.is_empty() {
            "Layer unlocked".into()
        } else {
            format!("Locked: {}", locks.describe())
        };
    }

    /// The Layers section title with Photoshop's four lock toggles at its
    /// right: transparent pixels, image pixels, position, all. They show
    /// and change the active layer's own locks; "all" covers the others.
    pub(crate) fn layers_header(&mut self, ui: &mut egui::Ui) {
        let own = self.active_layer().map(|l| l.locks);
        let inherited = self.active.map(|id| {
            let doc = self.editor.doc();
            doc.parent_of(id)
                .map_or(LayerLocks::NONE, |p| effective_locks(doc, p))
        });
        let mut clicked = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("LAYERS").small().strong().color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (kind, name) in LOCK_TOGGLES.iter().rev() {
                    let on = own.is_some_and(|l| kind.get(&l) || (*kind != LockKind::All && l.all));
                    let from_group = inherited.is_some_and(|g| kind.get(&g.effective()));
                    let enabled = own.is_some() && (*kind == LockKind::All || !own.is_some_and(|l| l.all));
                    let resp = lock_toggle(ui, *kind, on || from_group, enabled, name);
                    let tip = match (own.is_some(), on, from_group) {
                        (false, ..) => format!("{name} (select a layer first)"),
                        (true, _, true) => format!("{name}: locked by its group"),
                        (true, true, _) if *kind != LockKind::All && own.is_some_and(|l| l.all) => {
                            format!("{name}: on with Lock all")
                        }
                        (true, true, _) => format!("{name}: on (click to unlock)"),
                        (true, false, _) => format!("{name}: off"),
                    };
                    let resp = resp.on_hover_text(&tip).on_disabled_hover_text(&tip);
                    if resp.clicked() {
                        clicked = Some(*kind);
                    }
                }
                ui.label(RichText::new("Lock").small().color(MUTED));
            });
        });
        if let Some(kind) = clicked {
            self.toggle_lock(kind);
        }
    }

    /// Run one of this module's actions; false for ids it doesn't own.
    pub(crate) fn run_layer_action(&mut self, id: &str) -> bool {
        match id {
            "duplicate-layer" => self.duplicate_active(),
            "merge-down" => self.merge_down_active(),
            "merge-visible" => self.merge_visible(),
            "flatten" => self.flatten_image(),
            "stamp-visible" => self.stamp_visible(),
            "lock-transparency" => self.toggle_lock(LockKind::Transparency),
            "lock-pixels" => self.toggle_lock(LockKind::Pixels),
            "lock-position" => self.toggle_lock(LockKind::Position),
            "lock-all" => self.toggle_lock(LockKind::All),
            _ => return false,
        }
        true
    }

    /// Shortcut labels for this module's fixed keys; `None` for others.
    pub(crate) fn layer_action_keys(&self, ctx: &egui::Context, id: &str) -> Option<String> {
        use egui::Modifiers as M;
        match id {
            // Ctrl/Cmd+J duplicates when nothing is selected (with a
            // selection it is "layer via copy"); it follows that binding.
            "duplicate-layer" => Some(session::chord_label(ctx, &self.prefs, "layer-via-copy")),
            "stamp-visible" => Some(shortcut_text(ctx, M::COMMAND | M::SHIFT | M::ALT, Key::E)),
            _ => None,
        }
    }

    /// Layer ▸ Lock: the active layer's four locks as checkable items.
    pub(crate) fn lock_menu(&mut self, ui: &mut egui::Ui) {
        let l = self.active_layer().map_or(LayerLocks::NONE, |l| l.locks);
        menu(ui, "Lock layer", |ui| {
            for ((kind, label), id) in
                LOCK_TOGGLES
                    .iter()
                    .zip(["lock-transparency", "lock-pixels", "lock-position", "lock-all"])
            {
                let on = kind.get(&l) || (*kind != LockKind::All && l.all);
                self.act_check(ui, label, id, on);
            }
        });
    }

    /// The Layer menu's merge section.
    pub(crate) fn merge_menu_items(&mut self, ui: &mut egui::Ui) {
        let label = self.merge_label();
        self.act(ui, label, "merge-down");
        self.act(ui, "Merge visible", "merge-visible");
        self.act(ui, "Stamp visible to new layer", "stamp-visible");
        self.act(ui, "Flatten image", "flatten");
    }
}

/// One 20 px lock toggle: the kind's line icon, warm-tinted while on.
fn lock_toggle(ui: &mut egui::Ui, kind: LockKind, on: bool, enabled: bool, name: &str) -> egui::Response {
    // Disabled toggles still answer hovers (for the tooltip), never clicks.
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), sense);
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 4.0, ACCENT_TINT);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 4.0, RAISED);
    }
    focus_ring(ui, &resp, rect, 4.0);
    let ink = match (enabled, on) {
        (_, true) => ACCENT,
        (true, false) => MUTED,
        (false, false) => Color32::from_rgb(0x5A, 0x61, 0x6B),
    };
    paint_lock_icon(p, rect.shrink(4.5), kind, ink);
    let label = name.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, &label));
    resp
}

fn paint_lock_icon(p: &egui::Painter, r: egui::Rect, kind: LockKind, c: Color32) {
    let s = Stroke::new(1.4, c);
    match kind {
        LockKind::Transparency => {
            // A 2×2 checkerboard: the transparent-pixel motif.
            p.rect_stroke(r, 1.5, s);
            let h = r.width() / 2.0;
            p.rect_filled(egui::Rect::from_min_size(r.min, Vec2::splat(h)), 0.0, c);
            p.rect_filled(
                egui::Rect::from_min_size(r.min + egui::vec2(h, h), Vec2::splat(h)),
                0.0,
                c,
            );
        }
        LockKind::Pixels => tools::draw_icon(p, r, Tool::Brush, c),
        LockKind::Position => tools::draw_icon(p, r, Tool::Move, c),
        LockKind::All => paint_padlock(p, r, c, true),
    }
}

/// A padlock: filled body when `solid`, outlined otherwise.
pub(crate) fn paint_padlock(p: &egui::Painter, r: egui::Rect, c: Color32, solid: bool) {
    let s = Stroke::new(1.4, c);
    let body = egui::Rect::from_min_max(
        egui::pos2(r.min.x + r.width() * 0.1, r.min.y + r.height() * 0.45),
        egui::pos2(r.max.x - r.width() * 0.1, r.max.y),
    );
    if solid {
        p.rect_filled(body, 1.5, c);
    } else {
        p.rect_stroke(body, 1.5, s);
    }
    // Shackle: a half circle on two short legs.
    let (cx, top) = (r.center().x, r.min.y + r.height() * 0.08);
    let rad = r.width() * 0.26;
    let leg = body.min.y;
    let arc_cy = top + rad;
    let n = 10;
    let mut pts: Vec<egui::Pos2> = vec![egui::pos2(cx - rad, leg)];
    for i in 0..=n {
        let a = std::f32::consts::PI * (1.0 + i as f32 / n as f32);
        pts.push(egui::pos2(cx + rad * a.cos(), arc_cy + rad * a.sin()));
    }
    pts.push(egui::pos2(cx + rad, leg));
    p.add(Shape::line(pts, s));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app for a command line, with autosave and the crash-recovery
    /// prompt kept out of the way (see `a11y_tests::launch`).
    fn launch(args: &[String]) -> App {
        let mut app = App::launch(args);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        app
    }

    fn doc_app() -> App {
        let mut app = launch(&[]);
        app.open_in_new_tab(blank(64, 64), None);
        app
    }

    #[test]
    fn duplicate_makes_the_copy_active_and_cmd_j_picks_by_selection() {
        let mut app = doc_app();
        app.add_pixel_layer();
        let orig = app.active.unwrap();
        app.run_menu_action("fill");
        assert_eq!(app.action_block("duplicate-layer"), None);
        app.run_menu_action("duplicate-layer");
        let copy = app.active.unwrap();
        assert_ne!(copy, orig);
        let doc = app.editor.doc();
        let name = &doc.layer(orig).unwrap().name;
        assert_eq!(doc.layer(copy).unwrap().name, format!("{name} copy"));
        let ids: Vec<LayerId> = doc.layers().iter().map(|l| l.id).collect();
        let at = ids.iter().position(|i| *i == orig).unwrap();
        assert_eq!(ids[at + 1], copy, "directly above the original");
        assert_eq!(app.cmd_j_action(), "duplicate-layer");
        app.run_menu_action("select-all");
        assert_eq!(app.cmd_j_action(), "layer-via-copy");
    }

    #[test]
    fn merge_actions_block_with_reasons_and_keep_the_result_active() {
        let mut app = doc_app();
        // The blank document has one white "Background" layer.
        let bg = app.active.unwrap();
        assert_eq!(
            app.action_block("merge-down"),
            Some("Nothing below to merge into")
        );
        assert_eq!(
            app.action_block("merge-visible"),
            Some("Only one layer is visible")
        );
        app.set_active(None);
        assert_eq!(app.action_block("merge-down"), Some("Select a layer first"));
        assert_eq!(app.action_block("duplicate-layer"), Some("Select a layer first"));
        app.set_active(Some(bg));
        app.add_pixel_layer();
        assert_eq!(app.action_block("merge-down"), None);
        assert_eq!(app.merge_label(), "Merge down");
        app.run_menu_action("merge-down");
        assert_eq!(app.active, Some(bg), "the merged layer keeps the lower id");
        assert_eq!(app.editor.doc().layers().len(), 1);
        app.run_menu_action("stamp-visible");
        assert_eq!(app.editor.doc().layers().len(), 2);
        assert_eq!(app.action_block("merge-visible"), None);
        app.run_menu_action("merge-visible");
        assert_eq!(app.editor.doc().layers().len(), 1);
        assert_eq!(app.active, Some(bg));
        app.run_menu_action("flatten");
        let d = app.editor.doc();
        assert_eq!(d.layers().len(), 1);
        assert_eq!(d.layers()[0].name, "Background");
        assert_ne!(d.layers()[0].id, bg, "a new layer");
        assert_eq!(app.active, Some(d.layers()[0].id));
        assert_eq!(app.action_block("flatten"), None);
        app.run(&RemoveLayer {
            layer: d.layers()[0].id,
        });
        assert_eq!(
            app.action_block("flatten"),
            Some("There are no layers to flatten")
        );
        assert_eq!(app.action_block("stamp-visible"), Some("Nothing is visible"));
    }

    #[test]
    fn locks_toggle_from_actions_and_block_what_they_protect() {
        let mut app = doc_app();
        let bg = app.active.unwrap();
        assert_eq!(app.action_block("fill"), None);
        app.run_menu_action("lock-pixels");
        assert!(app.editor.doc().layer(bg).unwrap().locks.pixels);
        assert_eq!(app.action_block("fill"), Some("The layer's pixels are locked"));
        assert_eq!(
            app.action_block("filter-gauss"),
            Some("The layer's pixels are locked")
        );
        assert_eq!(app.action_block("xform"), Some("The layer's pixels are locked"));
        assert_eq!(app.paint_lock_block(), Some("The layer's pixels are locked"));
        assert_eq!(
            app.lock_block(LockNeed::Move),
            None,
            "pixel-locked layers still move"
        );
        // Fill run anyway (a key, say) is refused by the engine too.
        let steps = app.editor.history().len();
        app.run(&Fill {
            layer: bg,
            color: [1.0, 0.0, 0.0, 1.0],
        });
        assert_eq!(app.editor.history().len(), steps);
        assert!(app.status.contains("its pixels are locked"), "{}", app.status);
        app.run_menu_action("lock-pixels");
        app.run_menu_action("lock-position");
        assert_eq!(
            app.lock_block(LockNeed::Move),
            Some("The layer's position is locked")
        );
        assert_eq!(app.action_block("flip-h"), Some("The layer's position is locked"));
        assert_eq!(app.action_block("fill"), None);
        app.run_menu_action("lock-all");
        assert_eq!(app.action_block("add-mask"), Some("The layer is locked"));
        assert_eq!(
            app.action_block("lock-pixels"),
            Some("Lock all already covers this")
        );
        assert_eq!(app.action_block("merge-down"), Some("The layer is locked"));
        // Undo walks the lock changes back.
        app.undo();
        app.undo();
        assert!(app.editor.doc().layer(bg).unwrap().locks.is_empty());
    }
}
