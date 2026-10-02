use super::*;

impl App {
    // ---- edits used by menus and panels ----------------------------------------------

    pub(crate) fn undo(&mut self) {
        if let Some(l) = self.editor.undo() {
            self.status = format!("Undid {l}");
            self.below.note_change(self.editor.doc(), None);
            let r = self.editor.last_affected();
            self.mark(r);
            self.fix_active();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(l) = self.editor.redo() {
            self.status = format!("Redid {l}");
            self.below.note_change(self.editor.doc(), None);
            let r = self.editor.last_affected();
            self.mark(r);
            self.fix_active();
        }
    }

    pub(crate) fn fill_active(&mut self) {
        if let Some(layer) = self.active {
            let color = self.make_brush().color;
            self.run(&Fill { layer, color });
        }
    }

    pub(crate) fn clear_active(&mut self) {
        if let Some(layer) = self.active {
            self.run(&Clear { layer });
        }
    }

    pub(crate) fn reorder_active(&mut self, delta: i32) {
        if let Some(layer) = self.active {
            self.run(&ReorderLayer { layer, delta });
        }
    }

    pub(crate) fn flip_active(&mut self, horizontal: bool) {
        let Some(layer) = self.active else { return };
        // A smart object composes the flip into its transform (about its
        // painted centre), so it stays lossless; FlipLayer is pixel-only.
        let smart_pivot = self
            .active_layer()
            .filter(|l| l.smart_layer().is_some())
            .and_then(|l| l.raster_store())
            .and_then(|s| s.content_bounds())
            .map(|b| (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0));
        match smart_pivot {
            Some((cx, cy)) => {
                let (sx, sy) = if horizontal { (-1.0, 1.0) } else { (1.0, -1.0) };
                self.run(&TransformLayer {
                    layer,
                    transform: lumenply_tiles::Affine::around(cx, cy, sx, sy, 0.0),
                });
            }
            None => self.run(&FlipLayer { layer, horizontal }),
        }
    }

    pub(crate) fn add_pixel_layer(&mut self) {
        let n = self.editor.doc().layer_count() + 1;
        self.run(&AddPixelLayer::new(format!("Layer {n}")));
        self.select_top();
    }

    pub(crate) fn add_adjustment(&mut self, adj: Adjustment) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddAdjustmentLayer::new(adj);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    pub(crate) fn add_filter_layer(&mut self, f: Filter) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddFilterLayer::new(f);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    pub(crate) fn delete_active(&mut self) {
        if let Some(id) = self.active {
            self.run(&RemoveLayer { layer: id });
            self.select_top();
        }
    }

    /// Group the multi-selection, or the active layer with the one below it.
    pub(crate) fn group_selected(&mut self) {
        let doc = self.editor.doc();
        let mut ids: Vec<LayerId> = self.selected.clone();
        if ids.len() < 2 {
            let Some(active) = self.active else { return };
            let parent = doc.parent_of(active);
            let siblings: Vec<LayerId> = match parent {
                None => doc.layers().iter().map(|l| l.id).collect(),
                Some(p) => doc.layer(p).map_or(Vec::new(), |g| {
                    g.children().unwrap_or(&[]).iter().map(|l| l.id).collect()
                }),
            };
            let i = siblings.iter().position(|id| *id == active).unwrap_or(0);
            ids = vec![active];
            if i > 0 {
                ids.push(siblings[i - 1]);
            }
        }
        let new_id = doc.next_id();
        let n = doc.layer_count();
        self.run(&GroupLayers {
            layers: ids,
            name: format!("Group {}", n),
        });
        if self.editor.doc().layer(new_id).is_some() {
            self.set_active(Some(new_id));
        }
    }

    pub(crate) fn ungroup_active(&mut self) {
        if let Some(id) = self.active {
            self.run(&UngroupLayer { layer: id });
            self.select_top();
        }
    }

    pub(crate) fn begin_free_transform(&mut self) {
        let Some(id) = self.active else { return };
        // Pixel layers, smart objects and shapes transform; a smart
        // object's or shape's bounds come from its rendered cache (a shape
        // then composes the transform into its outline, staying vector).
        let Some(b) = self
            .active_layer()
            .filter(|l| l.pixels().is_some() || l.smart_layer().is_some() || l.shape_layer().is_some())
            .and_then(|l| l.raster_store())
            .and_then(|p| p.content_bounds())
        else {
            self.status = "Free transform needs a pixel layer, smart object or shape with content".into();
            return;
        };
        self.tool = Tool::Move;
        self.xform = Some(Xform {
            layer: id,
            bounds: b,
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
            last_preview: b,
        });
        self.status =
            "Free transform: corners scale, edges stretch one axis, outside rotates, inside moves".into();
    }

    /// Free transform straight into perspective mode.
    pub(crate) fn begin_perspective(&mut self) {
        if !self.active_is_pixel() {
            self.status = "Perspective needs a pixel layer (rasterize smart objects first)".into();
            return;
        }
        self.begin_free_transform();
        if let Some(x) = self.xform.as_mut() {
            x.quad = Some(x.corners());
            self.status = "Perspective: drag each corner freely; Enter applies".into();
        }
    }

    /// Free transform straight into warp mode.
    pub(crate) fn begin_warp(&mut self) {
        if !self.active_is_pixel() {
            self.status = "Warp needs a pixel layer (rasterize smart objects first)".into();
            return;
        }
        self.begin_free_transform();
        if let Some(x) = self.xform.as_mut() {
            let mesh = x.initial_warp();
            x.warp = Some(mesh);
            self.status = "Warp: drag any mesh point; drag elsewhere moves it all; Enter applies".into();
        }
    }

    pub(crate) fn commit_free_transform(&mut self) {
        if let Some(x) = self.xform.take() {
            if let Some(grid) = x.warp_grid() {
                let identity = lumenply_render::WarpGrid::identity(x.bounds, grid.cols, grid.rows);
                let moved = grid
                    .points
                    .iter()
                    .zip(&identity.points)
                    .any(|(a, b)| (a.0 - b.0).abs() > 1e-3 || (a.1 - b.1).abs() > 1e-3);
                if moved {
                    self.run(&WarpLayer { layer: x.layer, grid });
                }
            } else if let Some(quad) = x.quad {
                let b = x.bounds;
                let identity = [
                    (b.x as f32, b.y as f32),
                    (b.right() as f32, b.y as f32),
                    (b.right() as f32, b.bottom() as f32),
                    (b.x as f32, b.bottom() as f32),
                ];
                if quad != identity {
                    self.run(&PerspectiveLayer { layer: x.layer, quad });
                }
            } else {
                let t = x.affine();
                if t.integer_translation() != Some((0, 0)) {
                    self.run(&TransformLayer {
                        layer: x.layer,
                        transform: t,
                    });
                }
            }
            self.mark(None);
        }
    }

    pub(crate) fn cancel_free_transform(&mut self) {
        if self.xform.take().is_some() {
            self.mark(None);
            self.status = "Transform cancelled".into();
        }
    }

    // ---- menu bar ----------------------------------------------------------------------

    pub(crate) fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu")
            .exact_height(40.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // The mark, top-left; click for About.
                    let (chip, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "About Lumenply")
                    });
                    ui.painter().rect_filled(chip, 7.0, GROUND);
                    brand::paint_mark(ui.painter(), chip.shrink(4.0), TEXT, ACCENT, GROUND);
                    if resp.on_hover_text("About Lumenply").clicked() {
                        self.dialog = Some(Dialog::About);
                    }
                    ui.add_space(2.0);
                    menu(ui, "File", |ui| self.file_menu(ui));
                    menu(ui, "Edit", |ui| self.edit_menu(ui));
                    menu(ui, "Image", |ui| self.image_menu(ui));
                    menu(ui, "Select", |ui| self.select_menu(ui));
                    menu(ui, "Layer", |ui| self.layer_menu(ui));
                    menu(ui, "Filter", |ui| self.filter_menu(ui));
                    menu(ui, "View", |ui| self.view_menu(ui));
                    menu(ui, "Help", |ui| self.help_menu(ui));

                    ui.add_space(10.0);
                    self.document_tab(ui);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let export = ui.add(
                            egui::Button::new(RichText::new("Export").color(ACCENT_INK).strong())
                                .fill(ACCENT),
                        );
                        note_target(ui.ctx(), "export-button", export.rect);
                        // The same list as File ▸ Export.
                        button_menu(&export, |ui| self.export_items(ui));
                        // Shrink (shorter label) rather than overlap the
                        // document tabs on a narrow window.
                        let keys = self.action_keys(ui.ctx(), "palette");
                        let room = ui.available_width() - 8.0;
                        let search = if room >= 230.0 {
                            Some((format!("Search tools, filters...   {keys}"), 230.0))
                        } else if room >= 140.0 {
                            Some((format!("Search...   {keys}"), room))
                        } else if room >= 64.0 {
                            Some((keys.clone(), room.min(80.0)))
                        } else {
                            None // too tight: the shortcut still opens it
                        };
                        if let Some((label, w)) = search {
                            let r = ui.add(
                                egui::Button::new(RichText::new(label).color(MUTED))
                                    .fill(GROUND)
                                    .stroke(Stroke::new(1.0, LINE))
                                    .min_size(egui::vec2(w, 26.0)),
                            );
                            if r.on_hover_text(format!("Search tools, filters and commands ({keys})"))
                                .clicked()
                            {
                                self.toggle_palette();
                            }
                        }
                    });
                });
            });
    }

    /// A menu item bound to a shared action (see palette.rs): enabled only
    /// when the action can run, with the real key and, when greyed out, a
    /// tooltip saying why.
    pub(crate) fn act(&mut self, ui: &mut egui::Ui, label: &str, id: &str) {
        let ctx = ui.ctx().clone();
        let block = self.action_block(id);
        let keys = self.action_keys(&ctx, id);
        let r = menu_item_response(ui, block.is_none(), label, &keys);
        let r = match block {
            Some(why) => r.on_disabled_hover_text(why),
            None => r,
        };
        if r.clicked() {
            self.run_action(&ctx, id);
        }
    }

    /// [`App::act`] for an on/off setting, checked while on.
    pub(crate) fn act_check(&mut self, ui: &mut egui::Ui, label: &str, id: &str, on: bool) -> egui::Response {
        let ctx = ui.ctx().clone();
        let keys = self.action_keys(&ctx, id);
        let r = menu_check(ui, on, label, &keys);
        if r.clicked() {
            self.run_action(&ctx, id);
        }
        r
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "New...", "new");
        self.act(ui, "Open...", "open");
        menu(ui, "Open recent", |ui| {
            if self.recent.is_empty() {
                menu_note(ui, "Nothing yet");
            }
            let mut open: Option<String> = None;
            for p in &self.recent {
                if menu_item_response(ui, true, &file_name(p), "")
                    .on_hover_text(p)
                    .clicked()
                {
                    open = Some(p.clone());
                }
            }
            if let Some(p) = open {
                self.open_path(&p);
            }
        });
        self.act(ui, "Open demo document", "demo");
        menu_separator(ui);
        self.act(ui, "Place image as layer...", "place");
        self.act(ui, "Import brushes...", "import-brushes");
        menu_separator(ui);
        self.act(ui, "Save", "save");
        self.act(ui, "Save as...", "saveas");
        menu(ui, "Export", |ui| self.export_items(ui));
        menu_separator(ui);
        self.act(ui, "Close document", "close");
        self.act(ui, "Quit Lumenply", "quit");
    }

    /// Every export format, grouped by what survives: File ▸ Export and
    /// the Export button both show this list.
    fn export_items(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "Export As...", "export-as");
        menu_heading(ui, "FLATTENED IMAGE");
        self.act(ui, "PNG...", "export-png");
        self.act(ui, "JPEG...", "export-jpeg");
        menu_heading(ui, "WITH LAYERS");
        self.act(ui, "Photoshop PSD...", "export-psd");
        self.act(ui, "Photoshop PSD (16-bit)...", "export-psd16");
        self.act(ui, "OpenRaster (.ora)...", "export-ora");
        menu_heading(ui, "HIGH BIT DEPTH");
        self.act(ui, "16-bit PNG / TIFF...", "export-16bit");
        self.act(ui, "OpenEXR (linear float)...", "export-exr");
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        // Name the step, as the history strip does.
        let undo = match self.editor.history().last() {
            Some(l) => format!("Undo {l}"),
            None => "Undo".into(),
        };
        let redo = match self.editor.redo_history().first() {
            Some(l) => format!("Redo {l}"),
            None => "Redo".into(),
        };
        self.act(ui, &undo, "undo");
        self.act(ui, &redo, "redo");
        menu_separator(ui);
        self.act(ui, "Cut", "cut");
        self.act(ui, "Copy", "copy");
        self.act(ui, "Copy merged", "copy-merged");
        self.act(ui, "Paste", "paste");
        self.act(ui, "Paste in place", "paste-in-place");
        menu_separator(ui);
        self.act(ui, "Free transform", "xform");
        self.act(ui, "Perspective", "perspective");
        self.act(ui, "Warp", "warp");
        menu_separator(ui);
        self.act(ui, "Fill with brush colour", "fill");
        self.act(ui, "Fill...", "fill-dialog");
        self.act(ui, "Content-Aware Fill...", "content-aware");
        self.act(ui, "Clear", "clear");
        self.act(ui, "Define brush tip", "define-brush");
        menu_separator(ui);
        self.act(ui, "Preferences...", "prefs");
    }

    fn image_menu(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "Image size...", "image-size");
        self.act(ui, "Canvas size...", "canvas-size");
        self.act(ui, "Crop to selection", "crop");
        self.act(ui, "Trim...", "trim");
        self.act(ui, "Reveal all", "reveal-all");
        menu_separator(ui);
        self.act(ui, "Rotate 90° clockwise", "rot-cw");
        self.act(ui, "Rotate 90° counter-clockwise", "rot-ccw");
        self.act(ui, "Rotate 180°", "rot-180");
        self.act(ui, "Rotate by angle...", "rot-angle");
        self.act(ui, "Flip image horizontal", "img-flip-h");
        self.act(ui, "Flip image vertical", "img-flip-v");
        menu_separator(ui);
        self.act(ui, "Auto contrast", "auto-contrast");
        self.act(ui, "Auto color", "auto-color");
        menu_separator(ui);
        let float = self.editor.doc().float_mode;
        self.act_check(ui, "32-bit float (HDR)", "float-mode", float)
            .on_hover_text(
                "Keep values outside 0–1 through every edit (HDR). Off, tiles \
                 return to 16-bit as they are next edited, clamping the range",
            );
    }

    fn select_menu(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "All", "select-all");
        self.act(ui, "Deselect", "deselect");
        self.act(ui, "Invert", "invert-sel");
        menu_separator(ui);
        self.act(ui, "Colour range...", "color-range");
        let quick = self.quick_mask;
        self.act_check(ui, "Quick mask", "quick-mask", quick)
            .on_hover_text("Paint the selection: white selects, black deselects");
        self.act(ui, "Select and Mask...", "select-mask");
        menu_separator(ui);
        menu(ui, "Modify", |ui| {
            self.act(ui, "Border...", "sel-border");
            self.act(ui, "Smooth...", "sel-smooth");
            self.act(ui, "Expand...", "sel-expand");
            self.act(ui, "Contract...", "sel-contract");
        });
        self.act(ui, "Grow", "sel-grow");
        self.act(ui, "Similar", "sel-similar");
        menu_separator(ui);
        let feather = format!("Feather {:.0} px", self.feather);
        self.act(ui, &feather, "feather");
        self.act(ui, "Layer mask from selection", "mask-from-sel");
        menu_separator(ui);
        self.act(ui, "Save selection...", "save-selection");
        self.act(ui, "Load selection...", "load-selection");
    }

    fn layer_menu(&mut self, ui: &mut egui::Ui) {
        // Two columns keep the menu inside a 600 px window (egui menus
        // can't scroll): making and arranging layers on the left,
        // combining, aligning, locking and converting them on the right.
        ui.horizontal_top(|ui| {
            layer_actions::menu_column(ui, "layer-left", |ui| self.layer_menu_left(ui));
            ui.add_space(6.0);
            layer_actions::menu_column(ui, "layer-right", |ui| self.layer_menu_right(ui));
        });
    }

    fn layer_menu_left(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "New pixel layer", "new-layer");
        self.act(ui, "Duplicate layer", "duplicate-layer");
        self.act(ui, "Layer via copy", "layer-via-copy");
        menu(ui, "New adjustment layer", |ui| {
            for (name, adj) in adjustment_presets() {
                if menu_item(ui, name, "") {
                    self.add_adjustment(adj);
                }
            }
        });
        menu(ui, "New live filter layer", |ui| {
            for (name, f) in filter_presets() {
                if menu_item(ui, name, "") {
                    self.add_filter_layer(f);
                }
            }
        });
        menu(ui, "New fill layer", |ui| {
            self.act(ui, "Solid color", "fill-solid");
            self.act(ui, "Gradient", "fill-gradient");
        });
        self.act(ui, "New shape from path", "shape-from-path");
        menu_separator(ui);
        layer_actions::column_separator(ui);
        self.act(ui, "Rename", "rename");
        self.act(ui, "Delete layer", "delete-layer");
        layer_actions::column_separator(ui);
        self.act(ui, "Group", "group");
        self.act(ui, "Ungroup", "ungroup");
        layer_actions::column_separator(ui);
        self.act(ui, "Move up", "layer-up");
        self.act(ui, "Move down", "layer-down");
        layer_actions::column_separator(ui);
        self.act(ui, "Flip layer horizontal", "flip-h");
        self.act(ui, "Flip layer vertical", "flip-v");
    }

    fn layer_menu_right(&mut self, ui: &mut egui::Ui) {
        self.merge_menu_items(ui);
        layer_actions::column_separator(ui);
        self.align_menus(ui);
        self.lock_menu(ui);
        layer_actions::column_separator(ui);
        let mask = self
            .active_layer()
            .and_then(|l| l.mask.as_ref())
            .map(|m| m.enabled);
        if mask.is_some() {
            self.act(ui, "Remove mask", "rm-mask");
            let label = if mask == Some(true) {
                "Disable mask"
            } else {
                "Enable mask"
            };
            self.act(ui, label, "mask-toggle");
        } else {
            self.act(ui, "Add mask", "add-mask");
        }
        if self.active_layer().is_some_and(|l| l.clip) {
            self.act(ui, "Release clip", "unclip");
        } else {
            self.act(ui, "Clip to layer below", "clip");
        }
        layer_actions::column_separator(ui);
        self.act(ui, "Convert to smart object", "smart-object");
        self.act(ui, "Edit smart object contents", "smart-edit");
        self.act(ui, "Replace smart object contents...", "smart-replace");
        self.act(ui, "Rasterize", "rasterize");
    }

    /// The layers panel's "More" menu: the active layer's actions, in the
    /// order of its right-click menu (move and delete have their own
    /// buttons beside it).
    pub(crate) fn layer_more_menu(&mut self, ui: &mut egui::Ui) {
        let Some(l) = self.active_layer() else {
            menu_note(ui, "Select a layer first");
            return;
        };
        let group = l.children().is_some();
        let pixel = l.pixels().is_some();
        let rasterizable = l.smart_layer().is_some()
            || l.text_layer().is_some()
            || l.fill_layer().is_some()
            || l.shape_layer().is_some();
        let clip = l.clip;
        let mask = l.mask.as_ref().map(|m| m.enabled);
        self.act(ui, "Rename", "rename");
        self.act(ui, "Duplicate layer", "duplicate-layer");
        let merge = self.merge_label();
        self.act(ui, merge, "merge-down");
        menu_separator(ui);
        if clip {
            self.act(ui, "Release clip", "unclip");
        } else {
            self.act(ui, "Clip to layer below", "clip");
        }
        match mask {
            Some(on) => {
                self.act(ui, "Remove mask", "rm-mask");
                self.act(ui, if on { "Disable mask" } else { "Enable mask" }, "mask-toggle");
            }
            None => self.act(ui, "Add mask", "add-mask"),
        }
        menu_separator(ui);
        if group {
            self.act(ui, "Ungroup", "ungroup");
        }
        if rasterizable {
            self.act(ui, "Rasterize", "rasterize");
        }
        if pixel {
            self.act(ui, "Convert to smart object", "smart-object");
        }
        self.act(ui, "Flip horizontal", "flip-h");
        self.act(ui, "Flip vertical", "flip-v");
    }

    fn filter_menu(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "Liquify...", "liquify");
        menu_separator(ui);
        // Photoshop's grouping: a submenu per kind of filter.
        let presets = filter_presets();
        for cat in ["Blur", "Noise", "Pixelate", "Sharpen", "Stylize", "Other"] {
            menu(ui, cat, |ui| {
                for (name, f) in presets.iter().filter(|(_, f)| filter_category(f) == cat) {
                    self.act(ui, &format!("{name}..."), palette::filter_id(f));
                }
            });
        }
        menu_separator(ui);
        menu(ui, "Live filter layer", |ui| {
            for (name, f) in filter_presets() {
                if menu_item(ui, name, "") {
                    self.add_filter_layer(f);
                }
            }
        });
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "Zoom in", "zoom-in");
        self.act(ui, "Zoom out", "zoom-out");
        self.act(ui, "Fit on screen", "fit");
        self.act(ui, "Actual pixels", "actual");
        menu_separator(ui);
        for (label, id) in [
            ("Rulers", "rulers"),
            ("Show guides", "guides"),
            ("Lock guides", "lock-guides"),
            ("Show grid", "grid"),
            ("Snap", "snap"),
        ] {
            let on = self.view_aid_on(id);
            self.act_check(ui, label, id, on);
        }
        self.act(ui, "New guide...", "new-guide");
        self.act(ui, "Clear guides", "clear-guides");
        menu_separator(ui);
        let shown = !self.prefs.history_collapsed;
        self.act_check(ui, "History strip", "toggle-history", shown);
    }

    fn help_menu(&mut self, ui: &mut egui::Ui) {
        self.act(ui, "Search commands...", "palette");
        self.act(ui, "Keyboard shortcuts", "shortcuts");
        menu_separator(ui);
        self.act(ui, "About Lumenply", "about");
    }
}

impl App {
    /// The open documents as tabs: the live one raised with an accent
    /// underline, the rest flat; unsaved dot, middle-click or × to close,
    /// and a + for a new blank document.
    fn document_tab(&mut self, ui: &mut egui::Ui) {
        let infos = self.tab_infos();
        let mut switch = None;
        let mut close = None;
        for (i, (name, unsaved)) in infos.into_iter().enumerate() {
            let live = i == self.cur_tab;
            let label = if unsaved {
                format!("{name}  ")
            } else {
                name.clone()
            };
            let text = RichText::new(label).color(if live { TEXT } else { MUTED });
            let resp = ui.add(
                egui::Button::new(text)
                    .fill(if live { RAISED } else { Color32::TRANSPARENT })
                    .stroke(Stroke::new(1.0, if live { LINE } else { Color32::TRANSPARENT }))
                    .rounding(6.0),
            );
            if live {
                ui.painter().line_segment(
                    [
                        resp.rect.left_bottom() + egui::vec2(4.0, 0.0),
                        resp.rect.right_bottom() + egui::vec2(-4.0, 0.0),
                    ],
                    Stroke::new(2.0, ACCENT),
                );
            }
            if unsaved {
                let c = resp.rect.right_center() + egui::vec2(-9.0, 0.0);
                ui.painter().circle_filled(c, 3.0, ACCENT);
            }
            let resp = resp.on_hover_text(if unsaved {
                "Unsaved changes — middle-click closes"
            } else {
                "Middle-click closes"
            });
            if resp.clicked() && !live {
                switch = Some(i);
            }
            if resp.middle_clicked() {
                close = Some(i);
            }
            if live {
                // A visible close affordance on the active tab only, to
                // keep the strip quiet.
                let x = ui.add(
                    egui::Button::new(RichText::new("×").color(MUTED))
                        .fill(Color32::TRANSPARENT)
                        .frame(false),
                );
                if x.on_hover_text("Close document").clicked() {
                    close = Some(i);
                }
            }
        }
        let plus = ui.add(
            egui::Button::new(RichText::new("+").color(MUTED))
                .fill(Color32::TRANSPARENT)
                .frame(false),
        );
        if plus.on_hover_text("New document (tab)").clicked() {
            self.dialog = Some(Dialog::New(1920, 1080));
        }
        if let Some(i) = switch {
            self.switch_tab(i);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
    }
}
