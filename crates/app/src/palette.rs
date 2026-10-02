//! The command palette (Ctrl+K): type-to-search across every menu action.

use super::*;

#[derive(Default)]
pub(crate) struct Palette {
    query: String,
    selected: usize,
}

/// One searchable action: label, shortcut hint, and an id the runner matches.
struct Entry {
    label: String,
    hint: &'static str,
    id: PaletteAct,
}

#[derive(Clone, PartialEq)]
pub(crate) enum PaletteAct {
    Menu(&'static str),
    Adjustment(Adjustment),
    FilterLayer(Filter),
}

impl App {
    pub(crate) fn toggle_palette(&mut self) {
        self.palette = match self.palette {
            Some(_) => None,
            None => Some(Palette::default()),
        };
    }

    fn palette_entries(&self) -> Vec<Entry> {
        let m = |label: &str, hint: &'static str, id: &'static str| Entry {
            label: label.into(),
            hint,
            id: PaletteAct::Menu(id),
        };
        let mut v = vec![
            m("New document...", "", "new"),
            m("Open...", "Ctrl+O", "open"),
            m("Open image as layer (place)...", "", "place"),
            m("Save", "Ctrl+S", "save"),
            m("Save as...", "", "saveas"),
            m("Export PNG...", "", "export-png"),
            m("Export JPEG...", "", "export-jpeg"),
            m("Export PSD...", "", "export-psd"),
            m("Export OpenRaster...", "", "export-ora"),
            m("Undo", "Ctrl+Z", "undo"),
            m("Redo", "Ctrl+Shift+Z", "redo"),
            m("Fill with brush colour", "Shift+F5", "fill"),
            m("Clear", "Delete", "clear"),
            m("Free transform", "Ctrl+T", "xform"),
            m("Select all", "Ctrl+A", "select-all"),
            m("Deselect", "Ctrl+D", "deselect"),
            m("Invert selection", "Ctrl+Shift+I", "invert-sel"),
            m("New layer", "", "new-layer"),
            m("Group layers", "Ctrl+G", "group"),
            m("Ungroup", "", "ungroup"),
            m("Delete layer", "", "delete-layer"),
            m("Move layer up", "", "layer-up"),
            m("Move layer down", "", "layer-down"),
            m("Flip layer horizontal", "", "flip-h"),
            m("Flip layer vertical", "", "flip-v"),
            m("Add layer mask", "", "add-mask"),
            m("Remove layer mask", "", "rm-mask"),
            m("Image size...", "", "image-size"),
            m("Canvas size...", "", "canvas-size"),
            m("Rotate image 90° clockwise", "", "rot-cw"),
            m("Rotate image 90° counter-clockwise", "", "rot-ccw"),
            m("Rotate image 180°", "", "rot-180"),
            m("Flip image horizontal", "", "img-flip-h"),
            m("Flip image vertical", "", "img-flip-v"),
            m("Crop to selection", "", "crop"),
            m("Fit on screen", "0", "fit"),
            m("Actual pixels", "1", "actual"),
        ];
        for (name, adj) in adjustment_presets() {
            v.push(Entry {
                label: format!("New adjustment layer: {name}"),
                hint: "",
                id: PaletteAct::Adjustment(adj),
            });
        }
        for (name, f) in filter_presets() {
            v.push(Entry {
                label: format!("Apply filter: {name}..."),
                hint: "",
                id: PaletteAct::Menu(match f {
                    Filter::GaussianBlur { .. } => "filter-gauss",
                    Filter::BoxBlur { .. } => "filter-box",
                    Filter::Sharpen { .. } => "filter-sharpen",
                }),
            });
            v.push(Entry {
                label: format!("New live filter layer: {name}"),
                hint: "",
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
        let entries = self.palette_entries();
        let mut run: Option<PaletteAct> = None;
        let mut close = false;
        let screen = ctx.screen_rect();
        let state = self.palette.as_mut().expect("checked above");

        let q = state.query.to_lowercase();
        let mut hits: Vec<&Entry> = entries
            .iter()
            .filter(|e| q.is_empty() || e.label.to_lowercase().contains(&q))
            .collect();
        hits.sort_by_key(|e| !e.label.to_lowercase().starts_with(&q));
        if state.selected >= hits.len() {
            state.selected = hits.len().saturating_sub(1);
        }
        if down && state.selected + 1 < hits.len() {
            state.selected += 1;
        }
        if up {
            state.selected = state.selected.saturating_sub(1);
        }
        if enter {
            if let Some(e) = hits.get(state.selected) {
                run = Some(e.id.clone());
            }
            close = true;
        }

        egui::Area::new("palette".into())
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(screen.center().x - 240.0, screen.min.y + 80.0))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.style())
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0, LINE))
                    .show(ui, |ui| {
                        ui.set_width(480.0);
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
                            for (i, e) in hits.iter().enumerate() {
                                let active = i == state.selected;
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), 24.0),
                                    Sense::click(),
                                );
                                let p = ui.painter();
                                if active {
                                    p.rect_filled(rect, 4.0, ACCENT);
                                } else if resp.hovered() {
                                    p.rect_filled(rect, 4.0, PANEL);
                                }
                                let ink = if active { ACCENT_INK } else { TEXT };
                                p.text(
                                    rect.left_center() + egui::vec2(8.0, 0.0),
                                    Align2::LEFT_CENTER,
                                    &e.label,
                                    FontId::proportional(13.0),
                                    ink,
                                );
                                if !e.hint.is_empty() {
                                    p.text(
                                        rect.right_center() - egui::vec2(8.0, 0.0),
                                        Align2::RIGHT_CENTER,
                                        e.hint,
                                        FontId::monospace(11.0),
                                        if active { ACCENT_INK } else { MUTED },
                                    );
                                }
                                if resp.clicked() {
                                    run = Some(e.id.clone());
                                    close = true;
                                }
                            }
                            if hits.is_empty() {
                                ui.label(RichText::new("No matching command").weak());
                            }
                        });
                    });
            });

        if close {
            self.palette = None;
        }
        if let Some(act) = run {
            self.run_palette(act);
        }
    }

    fn run_palette(&mut self, act: PaletteAct) {
        match act {
            PaletteAct::Adjustment(a) => self.add_adjustment(a),
            PaletteAct::FilterLayer(f) => self.add_filter_layer(f),
            PaletteAct::Menu(id) => self.run_menu_action(id),
        }
    }

    /// Shared runner for actions reachable from both menus and the palette.
    pub(crate) fn run_menu_action(&mut self, id: &str) {
        match id {
            "new" => self.dialog = Some(Dialog::New(1200, 800)),
            "open" => self.pick_open(),
            "place" => self.pick_place(),
            "save" => match self.path.clone() {
                Some(p) => self.save_path(&p.to_string_lossy()),
                None => self.pick_save(),
            },
            "saveas" => self.pick_save(),
            "export-png" => self.pick_export_png(),
            "export-jpeg" => self.pick_export_jpeg(),
            "export-psd" => self.pick_export_psd(),
            "export-ora" => self.pick_export_ora(),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "fill" => self.fill_active(),
            "clear" => self.clear_active(),
            "xform" => self.begin_free_transform(),
            "select-all" => self.run(&SetSelection {
                selection: Some(Selection::all()),
            }),
            "deselect" => self.run(&SetSelection { selection: None }),
            "invert-sel" => self.run(&InvertSelection),
            "new-layer" => self.add_pixel_layer(),
            "group" => self.group_selected(),
            "ungroup" => self.ungroup_active(),
            "delete-layer" => self.delete_active(),
            "layer-up" => self.reorder_active(1),
            "layer-down" => self.reorder_active(-1),
            "flip-h" => self.flip_active(true),
            "flip-v" => self.flip_active(false),
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
            "image-size" => {
                let d = self.editor.doc();
                self.dialog = Some(Dialog::ImageSize(d.width, d.height, true));
            }
            "canvas-size" => {
                let d = self.editor.doc();
                self.dialog = Some(Dialog::CanvasSize(d.width, d.height, (0.5, 0.5)));
            }
            "rot-cw" => self.run(&RotateImage { quarter_turns: 1 }),
            "rot-ccw" => self.run(&RotateImage { quarter_turns: 3 }),
            "rot-180" => self.run(&RotateImage { quarter_turns: 2 }),
            "img-flip-h" => self.run(&FlipImage { horizontal: true }),
            "img-flip-v" => self.run(&FlipImage { horizontal: false }),
            "crop" => {
                let doc = self.editor.doc();
                let r = doc.selection.as_ref().map(|s| s.tight_bounds(doc.canvas()));
                match r {
                    Some(rect) if !rect.is_empty() => self.run(&CropDocument { rect }),
                    _ => self.status = "Crop needs a selection".into(),
                }
            }
            "fit" => self.view_cmd = Some(ViewCmd::Fit),
            "actual" => self.view_cmd = Some(ViewCmd::Actual),
            "filter-gauss" => self.dialog = Some(Dialog::Filter(Filter::GaussianBlur { radius: 8.0 })),
            "filter-box" => self.dialog = Some(Dialog::Filter(Filter::BoxBlur { radius: 6.0 })),
            "filter-sharpen" => {
                self.dialog = Some(Dialog::Filter(Filter::Sharpen {
                    amount: 0.6,
                    radius: 2.0,
                }))
            }
            other => self.status = format!("Unknown command '{other}'"),
        }
    }
}
