//! The command palette (Ctrl+K / ⌘K): type-to-search across every menu
//! action, plus the shared action registry the menus use too: one runner
//! ([`App::run_menu_action`]), one availability check
//! ([`App::action_block`]) and one shortcut label ([`App::action_keys`]),
//! so a menu item, its palette entry and its key always agree.

use super::*;

#[derive(Default)]
pub(crate) struct Palette {
    query: String,
    selected: usize,
}

/// One searchable action: label, shortcut hint, why it can't run now (if
/// it can't), and what it runs.
struct Entry {
    label: String,
    keys: String,
    block: Option<&'static str>,
    id: PaletteAct,
}

#[derive(Clone, PartialEq)]
pub(crate) enum PaletteAct {
    Menu(&'static str),
    Adjustment(Adjustment),
    FilterLayer(Filter),
    Tool(Tool),
}

/// Every tool, so "Search tools..." finds them, in rail order.
const TOOLS: [Tool; 16] = [
    Tool::Move,
    Tool::RectSelect,
    Tool::EllipseSelect,
    Tool::Lasso,
    Tool::PolyLasso,
    Tool::Wand,
    Tool::Brush,
    Tool::Eraser,
    Tool::Clone,
    Tool::Heal,
    Tool::Bucket,
    Tool::Gradient,
    Tool::Pen,
    Tool::Text,
    Tool::Eyedropper,
    Tool::Hand,
];

/// Every action reachable from the menus, in palette order: (label, id).
const ACTIONS: &[(&str, &str)] = &[
    ("New document...", "new"),
    ("Open...", "open"),
    ("Open demo document", "demo"),
    ("Place image as layer...", "place"),
    ("Save", "save"),
    ("Save as...", "saveas"),
    ("Export PNG...", "export-png"),
    ("Export JPEG...", "export-jpeg"),
    ("Export Photoshop PSD...", "export-psd"),
    ("Export Photoshop PSD (16-bit)...", "export-psd16"),
    ("Export OpenRaster...", "export-ora"),
    ("Export 16-bit PNG/TIFF...", "export-16bit"),
    ("Export OpenEXR (linear float)...", "export-exr"),
    ("Close document", "close"),
    ("Quit Lumenply", "quit"),
    ("Undo", "undo"),
    ("Redo", "redo"),
    ("Free transform", "xform"),
    ("Perspective transform", "perspective"),
    ("Warp", "warp"),
    ("Fill with brush colour", "fill"),
    ("Clear", "clear"),
    ("Preferences...", "prefs"),
    ("Select all", "select-all"),
    ("Deselect", "deselect"),
    ("Invert selection", "invert-sel"),
    ("Select colour range...", "color-range"),
    ("Toggle quick mask", "quick-mask"),
    ("Feather selection", "feather"),
    ("Layer mask from selection", "mask-from-sel"),
    ("New layer", "new-layer"),
    ("New layer from selection (layer via copy)", "layer-via-copy"),
    ("Rename layer", "rename"),
    ("Delete layer", "delete-layer"),
    ("Group layers", "group"),
    ("Ungroup", "ungroup"),
    ("Move layer up", "layer-up"),
    ("Move layer down", "layer-down"),
    ("Add layer mask", "add-mask"),
    ("Remove layer mask", "rm-mask"),
    ("Disable / enable layer mask", "mask-toggle"),
    ("Clip layer to the one below", "clip"),
    ("Release layer clip", "unclip"),
    ("Convert to smart object", "smart-object"),
    ("Rasterize layer", "rasterize"),
    ("Flip layer horizontal", "flip-h"),
    ("Flip layer vertical", "flip-v"),
    ("Image size...", "image-size"),
    ("Canvas size...", "canvas-size"),
    ("Crop to selection", "crop"),
    ("Rotate image 90° clockwise", "rot-cw"),
    ("Rotate image 90° counter-clockwise", "rot-ccw"),
    ("Rotate image 180°", "rot-180"),
    ("Flip image horizontal", "img-flip-h"),
    ("Flip image vertical", "img-flip-v"),
    ("Auto contrast", "auto-contrast"),
    ("Auto color", "auto-color"),
    ("32-bit float (HDR) on/off", "float-mode"),
    ("Fit on screen", "fit"),
    ("Actual pixels", "actual"),
    ("About Lumenply", "about"),
];

/// The id of the destructive filter dialog for a filter kind.
pub(crate) fn filter_id(f: &Filter) -> &'static str {
    match f {
        Filter::GaussianBlur { .. } => "filter-gauss",
        Filter::BoxBlur { .. } => "filter-box",
        Filter::Sharpen { .. } => "filter-sharpen",
        Filter::Noise { .. } => "filter-noise",
        Filter::MotionBlur { .. } => "filter-motion",
        Filter::Median { .. } => "filter-median",
        Filter::HighPass { .. } => "filter-highpass",
    }
}

impl App {
    pub(crate) fn toggle_palette(&mut self) {
        self.palette = match self.palette {
            Some(_) => None,
            None => Some(Palette::default()),
        };
    }

    fn palette_entries(&self, ctx: &egui::Context) -> Vec<Entry> {
        let mut v: Vec<Entry> = ACTIONS
            .iter()
            .map(|(label, id)| Entry {
                label: (*label).into(),
                keys: self.action_keys(ctx, id),
                block: self.action_block(id),
                id: PaletteAct::Menu(id),
            })
            .collect();
        for t in TOOLS {
            // The second tool on a key takes Shift (see App::shortcuts).
            let shift = matches!(t, Tool::Gradient | Tool::EllipseSelect | Tool::PolyLasso);
            let m = if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            };
            v.push(Entry {
                label: format!("{} tool", t.name()),
                keys: Key::from_name(t.key()).map_or(String::new(), |k| shortcut_text(ctx, m, k)),
                block: None,
                id: PaletteAct::Tool(t),
            });
        }
        for (name, adj) in adjustment_presets() {
            v.push(Entry {
                label: format!("New adjustment layer: {name}"),
                keys: String::new(),
                block: None,
                id: PaletteAct::Adjustment(adj),
            });
        }
        for (name, f) in filter_presets() {
            let id = filter_id(&f);
            v.push(Entry {
                label: format!("Apply filter: {name}..."),
                keys: String::new(),
                block: self.action_block(id),
                id: PaletteAct::Menu(id),
            });
            v.push(Entry {
                label: format!("New live filter layer: {name}"),
                keys: String::new(),
                block: None,
                id: PaletteAct::FilterLayer(f),
            });
        }
        v
    }

    pub(crate) fn palette_ui(&mut self, ctx: &egui::Context) {
        if self.palette.is_none() {
            return;
        }
        let (esc, enter, down, up) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            )
        });
        if esc {
            self.palette = None;
            return;
        }
        let entries = self.palette_entries(ctx);
        let mut run: Option<PaletteAct> = None;
        let mut blocked: Option<&'static str> = None;
        let mut close = false;
        let screen = ctx.screen_rect();
        let state = self.palette.as_mut().expect("checked above");

        let q = state.query.to_lowercase();
        let mut hits: Vec<&Entry> = entries
            .iter()
            .filter(|e| q.is_empty() || e.label.to_lowercase().contains(&q))
            .collect();
        // Prefix matches first, then whatever can run right now.
        hits.sort_by_key(|e| (!e.label.to_lowercase().starts_with(&q), e.block.is_some()));
        if state.selected >= hits.len() {
            state.selected = hits.len().saturating_sub(1);
        }
        if down && state.selected + 1 < hits.len() {
            state.selected += 1;
        }
        if up {
            state.selected = state.selected.saturating_sub(1);
        }
        // The row chosen by Enter or a click this frame.
        let mut picked: Option<usize> = enter.then_some(state.selected);

        let width = 480.0;
        egui::Area::new("palette".into())
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(screen.center().x - width / 2.0, screen.min.y + 80.0))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.style())
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0, LINE))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut state.query)
                                .hint_text("Search tools, filters, commands...")
                                .desired_width(f32::INFINITY),
                        );
                        edit.request_focus();
                        if edit.changed() {
                            state.selected = 0;
                        }
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            for (i, e) in hits.iter().enumerate() {
                                let active = i == state.selected;
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), MENU_ITEM_H),
                                    Sense::click(),
                                );
                                if active {
                                    resp.scroll_to_me(None);
                                }
                                let p = ui.painter();
                                // The row Enter will run wears the current-
                                // value look (warm tint, signal edge); the
                                // pointer's row the plain menu hover.
                                if active {
                                    p.rect_filled(rect, MENU_ITEM_ROUNDING, ACCENT_TINT);
                                    p.rect_stroke(
                                        rect.shrink(0.5),
                                        MENU_ITEM_ROUNDING,
                                        Stroke::new(1.0, ACCENT),
                                    );
                                } else if resp.hovered() {
                                    p.rect_filled(rect, MENU_ITEM_ROUNDING, MENU_HOVER);
                                }
                                let ink = if e.block.is_some() { MUTED } else { TEXT };
                                p.text(
                                    rect.left_center() + egui::vec2(MENU_PAD_X, 0.0),
                                    Align2::LEFT_CENTER,
                                    &e.label,
                                    FontId::proportional(13.0),
                                    ink,
                                );
                                // An unavailable action says why instead of
                                // showing its key.
                                let (hint, hint_ink) = match e.block {
                                    Some(why) => (why, Color32::from_rgb(0x7C, 0x83, 0x8D)),
                                    None => (e.keys.as_str(), MUTED),
                                };
                                if !hint.is_empty() {
                                    p.text(
                                        rect.right_center() - egui::vec2(MENU_PAD_X, 0.0),
                                        Align2::RIGHT_CENTER,
                                        hint,
                                        FontId::proportional(12.0),
                                        hint_ink,
                                    );
                                }
                                if resp.clicked() {
                                    picked = Some(i);
                                }
                            }
                            if hits.is_empty() {
                                ui.add_space(4.0);
                                ui.label(RichText::new("No matching command").color(MUTED));
                            }
                        });
                    });
            });

        if let Some(i) = picked {
            match hits.get(i) {
                Some(e) => match e.block {
                    None => {
                        run = Some(e.id.clone());
                        close = true;
                    }
                    Some(why) => blocked = Some(why),
                },
                None => close = true,
            }
        }
        if let Some(why) = blocked {
            self.status = why.into();
        }
        if close {
            self.palette = None;
        }
        if let Some(act) = run {
            match act {
                PaletteAct::Adjustment(a) => self.add_adjustment(a),
                PaletteAct::FilterLayer(f) => self.add_filter_layer(f),
                PaletteAct::Menu(id) => self.run_action(ctx, id),
                PaletteAct::Tool(t) => self.tool = t,
            }
        }
    }

    /// The active layer's position among its siblings: (index from the
    /// bottom, sibling count).
    fn active_slot(&self) -> Option<(usize, usize)> {
        let id = self.active?;
        let doc = self.editor.doc();
        let list = match doc.parent_of(id) {
            None => doc.layers(),
            Some(p) => doc.layer(p)?.children()?,
        };
        let i = list.iter().position(|l| l.id == id)?;
        Some((i, list.len()))
    }

    /// Why action `id` can't run right now, or `None` when it can. Menus
    /// grey such items out (with this as the tooltip), the palette shows
    /// it in place of the shortcut, and a blocked key press reports it in
    /// the status bar instead of silently doing nothing.
    pub(crate) fn action_block(&self, id: &str) -> Option<&'static str> {
        let doc = self.editor.doc();
        let layer = self.active_layer();
        let pixel = self.active_is_pixel();
        let smart = layer.is_some_and(|l| l.smart_layer().is_some());
        let selection = doc.selection.is_some();
        let need_pixel = Some("Select a pixel layer first");
        let need_layer = Some("Select a layer first");
        let need_selection = Some("Make a selection first");
        // On the welcome screen only the actions that open, create or
        // configure something make sense; the rest act on a document.
        if self.no_doc
            && !matches!(
                id,
                "new" | "open" | "demo" | "quit" | "prefs" | "about" | "palette"
            )
        {
            return Some("Open or create a document first");
        }
        match id {
            "undo" if !self.editor.can_undo() => Some("Nothing to undo"),
            "redo" if !self.editor.can_redo() => Some("Nothing to redo"),
            "fill" | "clear" | "layer-via-copy" | "flip-h" | "flip-v" | "smart-object" if !pixel => {
                need_pixel
            }
            "xform" if !pixel && !smart => Some("Select a pixel layer or smart object first"),
            "perspective" | "warp" if smart => Some("Rasterize the smart object first"),
            "perspective" | "warp" if !pixel => need_pixel,
            id if id.starts_with("filter-") && !pixel => Some("Filters apply to a pixel layer"),
            "deselect" | "feather" if !selection => need_selection,
            "crop" => {
                let r = doc.selection.as_ref().map(|s| s.tight_bounds(doc.canvas()));
                r.is_none_or(|r| r.is_empty()).then_some("Make a selection first")
            }
            "mask-from-sel" if !selection => need_selection,
            "mask-from-sel" | "rename" | "delete-layer" | "group" if layer.is_none() => need_layer,
            "ungroup" if !self.active_is_group() => Some("Select a group first"),
            "layer-up" | "layer-down" => match self.active_slot() {
                None => need_layer,
                Some((i, n)) if id == "layer-up" && i + 1 >= n => Some("Already at the top"),
                Some((0, _)) if id == "layer-down" => Some("Already at the bottom"),
                _ => None,
            },
            "add-mask" => match layer {
                None => need_layer,
                Some(l) if l.mask.is_some() => Some("This layer already has a mask"),
                _ => None,
            },
            "rm-mask" | "mask-toggle" if layer.is_none_or(|l| l.mask.is_none()) => {
                Some("This layer has no mask")
            }
            "clip" => match (layer, self.active_slot()) {
                (None, _) => need_layer,
                (Some(l), _) if l.clip => Some("This layer is already clipped"),
                (_, Some((0, _))) => Some("Nothing below to clip to"),
                _ => None,
            },
            "unclip" if !layer.is_some_and(|l| l.clip) => Some("This layer is not clipped"),
            "rasterize" if !layer.is_some_and(|l| l.smart_layer().is_some() || l.text_layer().is_some()) => {
                Some("Select a smart object or text layer first")
            }
            _ => None,
        }
    }

    /// The key that runs `id`, written the way this platform writes it,
    /// or "" when it has none. Rebindable chords follow the user's
    /// Preferences; the rest mirror the fixed keys in `App::shortcuts`.
    pub(crate) fn action_keys(&self, ctx: &egui::Context, id: &str) -> String {
        use egui::Modifiers as M;
        if let Some((m, k)) = session::resolve_chord(&self.prefs, id) {
            return shortcut_text(ctx, m, k);
        }
        let (m, k) = match id {
            "fill" => (M::SHIFT, Key::F5),
            "clear" => return "Delete".into(),
            "fit" => (M::NONE, Key::Num0),
            "actual" => (M::NONE, Key::Num1),
            "quick-mask" => (M::NONE, Key::Q),
            "palette" => (M::COMMAND, Key::K),
            _ => return String::new(),
        };
        shortcut_text(ctx, m, k)
    }

    /// Run an action from a menu or the palette. Handles the few actions
    /// that need the window, then defers to [`App::run_menu_action`].
    pub(crate) fn run_action(&mut self, ctx: &egui::Context, id: &str) {
        match id {
            "quit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            "palette" => self.toggle_palette(),
            _ => self.run_menu_action(id),
        }
    }

    /// Shared runner for actions reachable from menus, the palette and
    /// keys. A blocked action reports why in the status bar.
    pub(crate) fn run_menu_action(&mut self, id: &str) {
        if let Some(why) = self.action_block(id) {
            self.status = why.into();
            return;
        }
        match id {
            "new" => self.dialog = Some(Dialog::New(1920, 1080)),
            "open" => self.pick_open(),
            "demo" => self.open_demo(),
            "place" => self.pick_place(),
            "save" => match self.path.clone() {
                Some(p) => self.save_path(&p.to_string_lossy()),
                None => self.pick_save(),
            },
            "saveas" => self.pick_save(),
            "export-png" => self.pick_export_png(),
            "export-jpeg" => self.pick_export_jpeg(),
            "export-psd" => self.pick_export_psd(),
            "export-psd16" => self.pick_export_psd16(),
            "export-ora" => self.pick_export_ora(),
            "export-16bit" => self.pick_export_16bit(),
            "export-exr" => self.pick_export_exr(),
            "close" => self.close_tab(self.cur_tab),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "fill" => self.fill_active(),
            "clear" => self.clear_active(),
            "xform" => self.begin_free_transform(),
            "perspective" => self.begin_perspective(),
            "warp" => self.begin_warp(),
            "select-all" => self.run(&SetSelection {
                selection: Some(Selection::all()),
            }),
            "deselect" => self.run(&SetSelection { selection: None }),
            "invert-sel" => self.run(&InvertSelection),
            "color-range" => self.dialog = Some(Dialog::ColorRange(25.0, false)),
            "quick-mask" => self.toggle_quick_mask(),
            "feather" => self.run(&FeatherSelection { radius: self.feather }),
            "mask-from-sel" => {
                if let Some(layer) = self.active {
                    self.run(&MaskFromSelection { layer });
                }
            }
            "new-layer" => self.add_pixel_layer(),
            "layer-via-copy" => {
                if let Some(layer) = self.active {
                    let name = self
                        .active_layer()
                        .map_or("Layer copy".into(), |l| format!("{} copy", l.name));
                    self.run(&NewLayerFromSelection { layer, name });
                }
            }
            "rename" => {
                if let Some(l) = self.active_layer() {
                    self.renaming = Some((l.id, l.name.clone()));
                }
            }
            "group" => self.group_selected(),
            "ungroup" => self.ungroup_active(),
            "smart-object" => {
                if let Some(layer) = self.active {
                    self.run(&ConvertToSmartObject { layer });
                }
            }
            "rasterize" => {
                if let Some(layer) = self.active {
                    self.run(&RasterizeLayer { layer });
                }
            }
            "delete-layer" => self.delete_active(),
            "layer-up" => self.reorder_active(1),
            "layer-down" => self.reorder_active(-1),
            "flip-h" => self.flip_active(true),
            "flip-v" => self.flip_active(false),
            "clip" | "unclip" => {
                if let Some(layer) = self.active {
                    self.run(&SetClipped {
                        layer,
                        clip: id == "clip",
                    });
                }
            }
            "add-mask" => {
                if let Some(l) = self.active {
                    self.run(&AddMask { layer: l });
                    self.editing_mask = true;
                }
            }
            "rm-mask" => {
                if let Some(l) = self.active {
                    self.run(&RemoveMask { layer: l });
                    self.editing_mask = false;
                }
            }
            "mask-toggle" => {
                let on = self
                    .active_layer()
                    .and_then(|l| l.mask.as_ref())
                    .map(|m| m.enabled);
                if let (Some(layer), Some(on)) = (self.active, on) {
                    self.run(&SetMaskEnabled { layer, enabled: !on });
                }
            }
            "image-size" => {
                let d = self.editor.doc();
                self.dialog = Some(Dialog::ImageSize(d.width, d.height, true));
            }
            "canvas-size" => {
                let d = self.editor.doc();
                self.dialog = Some(Dialog::CanvasSize(d.width, d.height, (0.5, 0.5)));
            }
            // A quarter turn swaps width and height: refit the view.
            "rot-cw" => {
                self.run(&RotateImage { quarter_turns: 1 });
                self.view_cmd = Some(ViewCmd::Fit);
            }
            "rot-ccw" => {
                self.run(&RotateImage { quarter_turns: -1 });
                self.view_cmd = Some(ViewCmd::Fit);
            }
            "rot-180" => self.run(&RotateImage { quarter_turns: 2 }),
            "img-flip-h" => self.run(&FlipImage { horizontal: true }),
            "img-flip-v" => self.run(&FlipImage { horizontal: false }),
            "crop" => {
                let doc = self.editor.doc();
                if let Some(rect) = doc.selection.as_ref().map(|s| s.tight_bounds(doc.canvas())) {
                    self.run(&CropDocument { rect });
                }
            }
            "auto-contrast" => self.auto_contrast(),
            "auto-color" => self.auto_color(),
            "float-mode" => {
                let on = !self.editor.doc().float_mode;
                self.run(&SetFloatMode { on });
            }
            "prefs" => self.dialog = Some(Dialog::Preferences(self.prefs.clone(), None)),
            "about" => self.dialog = Some(Dialog::About),
            "fit" => self.view_cmd = Some(ViewCmd::Fit),
            "actual" => self.view_cmd = Some(ViewCmd::Actual),
            "palette" => self.toggle_palette(),
            filter if filter.starts_with("filter-") => {
                match filter_presets().into_iter().find(|(_, f)| filter_id(f) == filter) {
                    Some((_, f)) => self.dialog = Some(Dialog::Filter(f)),
                    None => self.status = format!("Unknown filter '{filter}'"),
                }
            }
            other => self.status = format!("Unknown command '{other}'"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_one_id_and_one_label() {
        let mut ids: Vec<&str> = ACTIONS.iter().map(|(_, id)| *id).collect();
        let mut labels: Vec<&str> = ACTIONS.iter().map(|(l, _)| *l).collect();
        let n = ACTIONS.len();
        ids.sort_unstable();
        ids.dedup();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(ids.len(), n, "duplicate action id");
        assert_eq!(labels.len(), n, "duplicate action label");
        assert_eq!(n, 61);
    }

    #[test]
    fn every_filter_preset_has_its_own_dialog_id() {
        let mut ids: Vec<&str> = filter_presets().iter().map(|(_, f)| filter_id(f)).collect();
        assert_eq!(ids.len(), 7);
        assert!(ids.iter().all(|id| id.starts_with("filter-")));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 7, "two filters share a dialog id");
    }

    #[test]
    fn the_palette_lists_every_tool_once() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 16);
    }
}
