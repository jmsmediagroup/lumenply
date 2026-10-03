//! Photoshop-standard layer operations in the shell: duplicate, merge
//! down / merge group / merge clipping mask, merge visible, flatten and
//! stamp visible. The commands live in `lumenply_core::layer_ops`; this
//! module picks the target layer, keeps the active layer pointing at the
//! result, and says why an operation can't run.

use super::*;
use lumenply_core::align::{align_block, AlignEdge, AlignLayers, AlignOp};
use lumenply_core::layer_masks::{apply_mask_block, AddLayerMask, ApplyLayerMask, MaskFrom};
use lumenply_core::layer_ops::{self as ops, MergeKind};
use lumenply_core::locks::{effective_locks, SetLayerLocks};
use lumenply_doc::LayerLocks;

/// Align and distribute actions: (id, op, menu label, icon).
type AlignAction = (
    &'static str,
    AlignOp,
    &'static str,
    fn(&egui::Painter, egui::Rect, Color32),
);
const ALIGN_ACTIONS: [AlignAction; 12] = [
    (
        "align-left",
        AlignOp::Align(AlignEdge::Left),
        "Left edges",
        icon_left,
    ),
    (
        "align-hcenter",
        AlignOp::Align(AlignEdge::HCenter),
        "Horizontal centres",
        icon_hcenter,
    ),
    (
        "align-right",
        AlignOp::Align(AlignEdge::Right),
        "Right edges",
        icon_right,
    ),
    ("align-top", AlignOp::Align(AlignEdge::Top), "Top edges", icon_top),
    (
        "align-vcenter",
        AlignOp::Align(AlignEdge::VCenter),
        "Vertical centres",
        icon_vcenter,
    ),
    (
        "align-bottom",
        AlignOp::Align(AlignEdge::Bottom),
        "Bottom edges",
        icon_bottom,
    ),
    (
        "distribute-left",
        AlignOp::Distribute(AlignEdge::Left),
        "Left edges",
        icon_left,
    ),
    (
        "distribute-hcenter",
        AlignOp::Distribute(AlignEdge::HCenter),
        "Horizontal centres",
        icon_hcenter,
    ),
    (
        "distribute-right",
        AlignOp::Distribute(AlignEdge::Right),
        "Right edges",
        icon_right,
    ),
    (
        "distribute-top",
        AlignOp::Distribute(AlignEdge::Top),
        "Top edges",
        icon_top,
    ),
    (
        "distribute-vcenter",
        AlignOp::Distribute(AlignEdge::VCenter),
        "Vertical centres",
        icon_vcenter,
    ),
    (
        "distribute-bottom",
        AlignOp::Distribute(AlignEdge::Bottom),
        "Bottom edges",
        icon_bottom,
    ),
];

/// Tooltips for the Move bar's align buttons (an IconButton needs a
/// static string).
const ALIGN_TIPS: [&str; 6] = [
    "Align left edges",
    "Align horizontal centres",
    "Align right edges",
    "Align top edges",
    "Align vertical centres",
    "Align bottom edges",
];

/// Layer ▸ Layer mask's ways to start a mask: (menu label, action id).
/// "mask-from-sel" is also Select ▸ Layer mask from selection.
const MASK_NEW: [(&str, &str, MaskFrom); 4] = [
    ("Reveal all", "mask-reveal-all", MaskFrom::RevealAll),
    ("Hide all", "mask-hide-all", MaskFrom::HideAll),
    ("Reveal selection", "mask-from-sel", MaskFrom::RevealSelection),
    ("Hide selection", "mask-hide-sel", MaskFrom::HideSelection),
];

/// Photoshop's layer chords that aren't rebindable: Shift+Cmd+G ungroups,
/// Alt+Cmd+G clips (or releases) the active layer, Cmd+] and Cmd+[ move
/// it up and down. Called before Cmd+G and the bare bracket keys, which
/// these chords contain.
pub(crate) fn layer_chords(i: &mut egui::InputState, fired: &mut Vec<&'static str>) {
    use egui::Modifiers as M;
    if i.consume_key(M::COMMAND | M::ALT, Key::G) {
        fired.push("clip-toggle");
    }
    if i.consume_key(M::COMMAND | M::SHIFT, Key::G) {
        fired.push("ungroup");
    }
    if i.consume_key(M::COMMAND, Key::CloseBracket) {
        fired.push("layer-up");
    }
    if i.consume_key(M::COMMAND, Key::OpenBracket) {
        fired.push("layer-down");
    }
}

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
        if self.multi_selected().is_some() {
            return "Merge layers";
        }
        self.merge_kind().map_or("Merge down", MergeKind::label)
    }

    /// The layers picked in the Layers panel, when there are several:
    /// Cmd+E then merges them (Photoshop's Merge Layers).
    pub(crate) fn multi_selected(&self) -> Option<Vec<LayerId>> {
        let mut ids = self.selected.clone();
        ids.sort_unstable();
        ids.dedup();
        (ids.len() >= 2).then_some(ids)
    }

    /// Ctrl/Cmd+E: merge the active layer down (or its group, or the
    /// layers clipped to it) and keep the result active.
    pub(crate) fn merge_down_active(&mut self) {
        if let Some(ids) = self.multi_selected() {
            let doc = self.editor.doc();
            // The result keeps the topmost visible selected layer's id.
            let list = match doc.parent_of(ids[0]) {
                None => doc.layers(),
                Some(p) => doc.layer(p).and_then(|g| g.children()).unwrap_or(&[]),
            };
            let top = list
                .iter()
                .rev()
                .find(|l| l.visible && ids.contains(&l.id))
                .map(|l| l.id);
            self.run(&ops::MergeSelected { layers: ids });
            if let Some(t) = top.filter(|&t| self.editor.doc().layer(t).is_some()) {
                self.selected.clear();
                self.set_active(Some(t));
            }
            return;
        }
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

    /// Add a mask to the active layer and paint on it next.
    pub(crate) fn add_layer_mask(&mut self, from: MaskFrom) {
        let Some(layer) = self.active else { return };
        self.run(&AddLayerMask { layer, from });
        if self.active_has_mask() {
            self.editing_mask = true;
            self.status = format!("Added a layer mask ({})", from.label().to_lowercase());
        }
    }

    /// The mask button and Add mask: from the selection when there is one,
    /// else revealing everything; `hide` (Alt-click) hides instead.
    pub(crate) fn add_mask_auto(&mut self, hide: bool) {
        let sel = self.editor.doc().selection.is_some();
        self.add_layer_mask(match (sel, hide) {
            (true, false) => MaskFrom::RevealSelection,
            (true, true) => MaskFrom::HideSelection,
            (false, false) => MaskFrom::RevealAll,
            (false, true) => MaskFrom::HideAll,
        });
    }

    /// Layer via Copy (Cmd+J with a selection): the selected pixels on a
    /// new layer right above, which becomes the active one.
    pub(crate) fn layer_via_copy(&mut self) {
        let Some(layer) = self.active else { return };
        let name = self
            .active_layer()
            .map_or("Layer copy".into(), |l| format!("{} copy", l.name));
        let next = self.editor.doc().next_id();
        self.run(&NewLayerFromSelection { layer, name });
        if self.editor.doc().layer(next).is_some() {
            self.set_active(Some(next));
        }
    }

    /// Alt+Cmd+G: clip the active layer, or release it when clipped.
    fn clip_toggle_id(&self) -> &'static str {
        if self.active_layer().is_some_and(|l| l.clip) {
            "unclip"
        } else {
            "clip"
        }
    }

    /// Layer ▸ Layer mask, as in Photoshop: start a mask four ways, then
    /// delete, apply, disable or enable it.
    pub(crate) fn layer_mask_menu(&mut self, ui: &mut egui::Ui) {
        menu(ui, "Layer mask", |ui| {
            for (label, id, _) in MASK_NEW {
                self.act(ui, label, id);
            }
            menu_separator(ui);
            self.act(ui, "Delete mask", "rm-mask");
            self.act(ui, "Apply mask", "mask-apply");
            let off = self
                .active_layer()
                .and_then(|l| l.mask.as_ref())
                .is_some_and(|m| !m.enabled);
            self.act(
                ui,
                if off { "Enable mask" } else { "Disable mask" },
                "mask-toggle",
            );
        });
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
            // Edits that change the layer's pixels (Layer via Cut takes them
            // from it) are refused by the engine too; say so up front.
            "fill-bg" | "fill-dialog" | "content-aware" | "stroke-selection" | "cut" | "layer-via-cut"
            | "mask-apply" => Some(LockNeed::Paint),
            "xform" | "perspective" | "warp" | "flip-h" | "flip-v" => Some(LockNeed::Reshape),
            "add-mask" | "rm-mask" | "mask-toggle" | "mask-from-sel" | "mask-reveal-all"
            | "mask-hide-all" | "mask-hide-sel" | "clip" | "unclip" | "clip-toggle" => Some(LockNeed::Props),
            _ => None,
        };
        if let Some(why) = need.and_then(|n| self.lock_block(n)) {
            return Some(Some(why));
        }
        if let Some((_, _, from)) = MASK_NEW.iter().find(|m| m.1 == id) {
            return Some(match self.active_layer() {
                None => Some("Select a layer first"),
                Some(l) if l.mask.is_some() => Some("This layer already has a mask"),
                Some(_) if from.uses_selection() && doc.selection.is_none() => Some("Make a selection first"),
                Some(_) => None,
            });
        }
        Some(match id {
            "mask-apply" => match self.active {
                None => Some("Select a layer first"),
                Some(l) => apply_mask_block(doc, l),
            },
            "clip-toggle" => return Some(self.action_block(self.clip_toggle_id())),
            "duplicate-layer" => self.active_layer().is_none().then_some("Select a layer first"),
            "merge-down" => match self.multi_selected() {
                Some(ids) => ops::merge_selected_block(doc, &ids),
                None => self.merge_kind().err(),
            },
            "merge-visible" => ops::merge_visible_block(doc),
            "flatten" => doc
                .layers()
                .is_empty()
                .then_some("There are no layers to flatten"),
            "stamp-visible" => ops::stamp_visible_block(doc),
            id if id.starts_with("align-") || id.starts_with("distribute-") => {
                let (_, op, ..) = ALIGN_ACTIONS.iter().find(|a| a.0 == id)?;
                let (layers, to) = self.align_targets();
                align_block(doc, &layers, *op, to)
            }
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
        // As tall as the toggles, no taller (`horizontal` would pad the
        // row to the interact height and push the list down).
        let row = egui::vec2(ui.available_width(), 18.0);
        let mut filter = std::mem::take(&mut self.layer_filter);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
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
                ui.add_space(6.0);
                // The rest of the row: find layers by name.
                let w = (ui.available_width() - 2.0).max(40.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut filter)
                        .hint_text("Filter layers")
                        .desired_width(w)
                        .font(egui::TextStyle::Small),
                );
                a11y_name(&r, "Filter layers by name");
            });
        });
        self.layer_filter = filter;
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
            "add-mask" => self.add_mask_auto(false),
            "layer-via-copy" => self.layer_via_copy(),
            "mask-apply" => {
                if let Some(layer) = self.active {
                    self.run(&ApplyLayerMask { layer });
                    self.editing_mask = false;
                }
            }
            "clip-toggle" => self.run_menu_action(self.clip_toggle_id()),
            id if MASK_NEW.iter().any(|m| m.1 == id) => {
                if let Some((_, _, from)) = MASK_NEW.iter().find(|m| m.1 == id) {
                    self.add_layer_mask(*from);
                }
            }
            "lock-transparency" => self.toggle_lock(LockKind::Transparency),
            "lock-pixels" => self.toggle_lock(LockKind::Pixels),
            "lock-position" => self.toggle_lock(LockKind::Position),
            "lock-all" => self.toggle_lock(LockKind::All),
            id if id.starts_with("align-") || id.starts_with("distribute-") => {
                let Some((_, op, ..)) = ALIGN_ACTIONS.iter().find(|a| a.0 == id) else {
                    return false;
                };
                let (layers, to) = self.align_targets();
                self.run(&AlignLayers { layers, op: *op, to });
            }
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
            "ungroup" => Some(shortcut_text(ctx, M::COMMAND | M::SHIFT, Key::G)),
            "clip" | "unclip" | "clip-toggle" => Some(shortcut_text(ctx, M::COMMAND | M::ALT, Key::G)),
            "layer-up" => Some(shortcut_text(ctx, M::COMMAND, Key::CloseBracket)),
            "layer-down" => Some(shortcut_text(ctx, M::COMMAND, Key::OpenBracket)),
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

    /// The layers Align / Distribute act on, and what a lone layer aligns
    /// to: the selected layers (or just the active one); with one layer,
    /// the selection's bounds when there is a selection, else the canvas.
    pub(crate) fn align_targets(&self) -> (Vec<LayerId>, Option<Rect>) {
        let mut layers = self.selected.clone();
        if layers.is_empty() {
            layers.extend(self.active);
        }
        let doc = self.editor.doc();
        let to = (layers.len() == 1).then(|| {
            doc.selection
                .as_ref()
                .map(|s| s.tight_bounds(doc.canvas()))
                .filter(|r| !r.is_empty())
                .unwrap_or(doc.canvas())
        });
        (layers, to)
    }

    /// Layer ▸ Align and Layer ▸ Distribute submenus.
    pub(crate) fn align_menus(&mut self, ui: &mut egui::Ui) {
        menu(ui, "Align", |ui| {
            for (id, _, label, _) in &ALIGN_ACTIONS[..6] {
                self.act(ui, label, id);
            }
        });
        menu(ui, "Distribute", |ui| {
            for (id, _, label, _) in &ALIGN_ACTIONS[6..] {
                self.act(ui, label, id);
            }
        });
    }

    /// The Move tool bar's align buttons, plus a Distribute menu.
    pub(crate) fn align_bar(&mut self, ui: &mut egui::Ui) {
        let mut run = None;
        ui.spacing_mut().item_spacing.x = 2.0;
        for ((id, _, _, icon), tip) in ALIGN_ACTIONS[..6].iter().zip(ALIGN_TIPS) {
            let block = self.action_block(id);
            let r = ui.add_enabled(block.is_none(), layers::IconButton::new(*icon, tip));
            note_target(ui.ctx(), id, r.rect);
            let r = match block {
                Some(why) => r.on_disabled_hover_text(why),
                None => r,
            };
            if r.clicked() {
                run = Some(*id);
            }
        }
        ui.spacing_mut().item_spacing.x = 8.0;
        let dist = ui.add(egui::Button::new("Distribute"));
        note_target(ui.ctx(), "distribute-menu", dist.rect);
        button_menu(&dist, |ui| {
            for (id, _, label, _) in &ALIGN_ACTIONS[6..] {
                self.act(ui, label, id);
            }
        });
        if let Some(id) = run {
            self.run_menu_action(id);
        }
    }

    /// The Layer menu's merge section.
    pub(crate) fn merge_menu_items(&mut self, ui: &mut egui::Ui) {
        // (Each item is one row; the caller places separators.)
        let label = self.merge_label();
        self.act(ui, label, "merge-down");
        self.act(ui, "Merge visible", "merge-visible");
        self.act(ui, "Stamp visible to new layer", "stamp-visible");
        self.act(ui, "Flatten image", "flatten");
    }
}

/// One 18 px lock toggle: the kind's line icon, warm-tinted while on.
fn lock_toggle(ui: &mut egui::Ui, kind: LockKind, on: bool, enabled: bool, name: &str) -> egui::Response {
    // Disabled toggles still answer hovers (for the tooltip), never clicks.
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), sense);
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
    paint_lock_icon(p, rect.shrink(4.0), kind, ink);
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

/// One column of a multi-column menu. Items fill the column's width (the
/// widest item's, remembered from the previous frame, so hover rows line
/// up) and [`column_separator`] lines span exactly the column; egui's own
/// separator would run across the whole row.
pub(crate) fn menu_column<R>(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let key = egui::Id::new("menu-column").with(id);
    let w: f32 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or(0.0);
    let r = ui.allocate_ui_with_layout(
        egui::vec2(w, 0.0),
        egui::Layout::top_down_justified(egui::Align::Min),
        add,
    );
    let got = r.response.rect.width();
    if (got - w).abs() > 0.5 {
        ui.ctx().data_mut(|d| d.insert_temp(key, got));
        ui.ctx().request_repaint();
    }
    r.inner
}

/// A menu separator spanning just the current [`menu_column`].
pub(crate) fn column_separator(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(0.0, 9.0), Sense::hover());
    let x = ui.max_rect().x_range();
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter().hline(x, r.center().y, stroke);
}

/// An align glyph: the reference line plus a long and a short bar lined
/// up against it.
fn paint_align(p: &egui::Painter, r: egui::Rect, c: Color32, edge: AlignEdge) {
    let s = Stroke::new(1.4, c);
    let (w, h) = (r.width(), r.height());
    let bar = 3.5;
    // Horizontal alignments: a vertical line and two horizontal bars.
    let horizontal = matches!(edge, AlignEdge::Left | AlignEdge::HCenter | AlignEdge::Right);
    let at = |t: f32| match edge {
        AlignEdge::Left | AlignEdge::Top => 0.0,
        AlignEdge::HCenter | AlignEdge::VCenter => 0.5 - t / 2.0,
        AlignEdge::Right | AlignEdge::Bottom => 1.0 - t,
    };
    let line_at = match edge {
        AlignEdge::Left | AlignEdge::Top => 0.0,
        AlignEdge::HCenter | AlignEdge::VCenter => 0.5,
        AlignEdge::Right | AlignEdge::Bottom => 1.0,
    };
    for (len, pos) in [(0.85f32, 0.22f32), (0.5, 0.62)] {
        let rect = if horizontal {
            egui::Rect::from_min_size(
                egui::pos2(r.min.x + w * at(len), r.min.y + h * pos),
                egui::vec2(w * len, bar),
            )
        } else {
            egui::Rect::from_min_size(
                egui::pos2(r.min.x + w * pos, r.min.y + h * at(len)),
                egui::vec2(bar, h * len),
            )
        };
        p.rect_filled(rect, 1.0, c);
    }
    if horizontal {
        let x = r.min.x + w * line_at;
        p.line_segment([egui::pos2(x, r.min.y - 1.0), egui::pos2(x, r.max.y + 1.0)], s);
    } else {
        let y = r.min.y + h * line_at;
        p.line_segment([egui::pos2(r.min.x - 1.0, y), egui::pos2(r.max.x + 1.0, y)], s);
    }
}

fn icon_left(p: &egui::Painter, r: egui::Rect, c: Color32) {
    paint_align(p, r, c, AlignEdge::Left)
}
fn icon_hcenter(p: &egui::Painter, r: egui::Rect, c: Color32) {
    paint_align(p, r, c, AlignEdge::HCenter)
}
fn icon_right(p: &egui::Painter, r: egui::Rect, c: Color32) {
    paint_align(p, r, c, AlignEdge::Right)
}
fn icon_top(p: &egui::Painter, r: egui::Rect, c: Color32) {
    paint_align(p, r, c, AlignEdge::Top)
}
fn icon_vcenter(p: &egui::Painter, r: egui::Rect, c: Color32) {
    paint_align(p, r, c, AlignEdge::VCenter)
}
fn icon_bottom(p: &egui::Painter, r: egui::Rect, c: Color32) {
    paint_align(p, r, c, AlignEdge::Bottom)
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
    fn cmd_e_with_several_layers_selected_merges_them() {
        let mut doc = Document::new(16, 16);
        let a = doc.add_pixel_layer("a");
        let b = doc.add_pixel_layer("b");
        let c = doc.add_pixel_layer("c");
        for id in [a, b, c] {
            doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                id as i32,
                0,
                lumenply_tiles::Rgba::new(1.0, 0.0, 0.0, 1.0),
            );
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(c));
        app.selected = vec![a, c];
        assert_eq!(app.merge_label(), "Merge layers");
        assert_eq!(app.action_block("merge-down"), None);
        app.run_menu_action("merge-down");
        let ids: Vec<_> = app.editor.doc().layers().iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![b, c], "a merged into c's slot");
        assert_eq!(app.active, Some(c));
        let px = app.editor.doc().layer(c).unwrap().pixels().unwrap();
        assert_eq!(
            px.get_pixel(a as i32, 0).a,
            1.0,
            "a's pixel is in the merged layer"
        );
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

    #[test]
    fn layer_mask_actions_say_why_and_use_up_the_selection() {
        let mut app = doc_app();
        app.add_pixel_layer();
        let id = app.active.unwrap();
        app.run_menu_action("fill");
        assert_eq!(app.action_block("mask-from-sel"), Some("Make a selection first"));
        assert_eq!(app.action_block("mask-hide-sel"), Some("Make a selection first"));
        assert_eq!(app.action_block("mask-apply"), Some("This layer has no mask"));
        assert_eq!(app.action_block("mask-hide-all"), None);
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(8, 8, 16, 16))),
        });
        app.run_menu_action("mask-hide-sel");
        let m = app.editor.doc().layer(id).unwrap().mask.clone().unwrap();
        assert_eq!((m.value(10, 10), m.value(40, 40)), (0.0, 1.0));
        assert!(
            app.editor.doc().selection.is_none(),
            "deselected, as in Photoshop"
        );
        assert!(app.editing_mask, "the new mask is the paint target");
        assert_eq!(
            app.action_block("mask-reveal-all"),
            Some("This layer already has a mask")
        );
        app.run_menu_action("mask-apply");
        let l = app.editor.doc().layer(id).unwrap();
        assert!(l.mask.is_none());
        assert!(!app.editing_mask);
        let px = l.pixels().unwrap();
        assert_eq!((px.get_pixel(10, 10).a, px.get_pixel(40, 40).a), (0.0, 1.0));
        // The mask button without a selection reveals all; Alt hides all.
        app.add_mask_auto(true);
        let m = app.editor.doc().layer(id).unwrap().mask.clone().unwrap();
        assert_eq!(m.value(40, 40), 0.0);
        // A pixel lock refuses Apply (it changes pixels), not the mask.
        app.run_menu_action("lock-pixels");
        assert_eq!(
            app.action_block("mask-apply"),
            Some("The layer's pixels are locked")
        );
        assert_eq!(app.action_block("fill-bg"), Some("The layer's pixels are locked"));
        assert_eq!(
            app.action_block("fill-dialog"),
            Some("The layer's pixels are locked")
        );
        assert_eq!(
            app.action_block("layer-via-cut"),
            Some("The layer's pixels are locked")
        );
        assert_eq!(app.action_block("rm-mask"), None);
    }

    #[test]
    fn alt_cmd_g_toggles_the_clip_and_layer_via_copy_selects_the_copy() {
        let mut app = doc_app();
        app.add_pixel_layer();
        app.run_menu_action("fill");
        let id = app.active.unwrap();
        assert_eq!(app.action_block("clip-toggle"), None);
        app.run_menu_action("clip-toggle");
        assert!(app.editor.doc().layer(id).unwrap().clip);
        app.run_menu_action("clip-toggle");
        assert!(!app.editor.doc().layer(id).unwrap().clip);
        app.run_menu_action("select-all");
        let copy = app.editor.doc().next_id();
        app.run_menu_action(app.cmd_j_action());
        assert_eq!(app.active, Some(copy), "the new layer is the active one");
        let ids: Vec<LayerId> = app.editor.doc().layers().iter().map(|l| l.id).collect();
        assert_eq!(ids.last(), Some(&copy), "on top, right above its source");
    }

    #[test]
    fn align_acts_on_the_selected_layers_or_one_layer_against_the_canvas() {
        let mut app = doc_app();
        let add = |app: &mut App, x: i32, y: i32| {
            let id = app.editor.doc().next_id();
            let r = Raster::filled(10, 10, lumenply_tiles::Rgba::WHITE);
            app.run(&AddPixelLayer::from_raster("Box", r, x, y));
            id
        };
        let a = add(&mut app, 5, 5);
        let b = add(&mut app, 30, 20);
        let bounds = |app: &App, id| {
            lumenply_core::align::content_bounds(app.editor.doc().layer(id).unwrap()).unwrap()
        };
        // One layer: aligned to the 64×64 canvas.
        app.set_active(Some(b));
        assert_eq!(app.action_block("align-right"), None);
        assert_eq!(
            app.action_block("distribute-left"),
            Some("Select three or more layers")
        );
        app.run_menu_action("align-right");
        assert_eq!(bounds(&app, b), Rect::new(54, 20, 10, 10));
        // Two selected layers: to each other.
        app.selected = vec![a, b];
        app.run_menu_action("align-top");
        assert_eq!((bounds(&app, a).y, bounds(&app, b).y), (5, 5));
        assert_eq!(app.editor.history().last().copied(), Some("Align top edges"));
        // Nothing with content selected.
        let empty = app.editor.doc().next_id();
        app.add_pixel_layer();
        assert_eq!(app.active, Some(empty));
        assert_eq!(
            app.action_block("align-left"),
            Some("Select a layer with content first")
        );
    }
}
