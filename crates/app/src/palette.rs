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
const TOOLS: [Tool; 18] = [
    Tool::Move,
    Tool::RectSelect,
    Tool::EllipseSelect,
    Tool::Lasso,
    Tool::PolyLasso,
    Tool::Wand,
    Tool::Crop,
    Tool::Brush,
    Tool::Eraser,
    Tool::Clone,
    Tool::Heal,
    Tool::Bucket,
    Tool::Gradient,
    Tool::Pen,
    Tool::Shape,
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
    ("Export Color Lookup Table (.cube)...", "export-lut"),
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
    ("Zoom in", "zoom-in"),
    ("Zoom out", "zoom-out"),
    ("Fit on screen", "fit"),
    ("Actual pixels", "actual"),
    ("Print size (approximate)", "print-size"),
    ("Show or hide the history strip", "toggle-history"),
    ("About Lumenply", "about"),
    ("Liquify...", "liquify"),
    ("Export As...", "export-as"),
    ("Edit smart object contents", "smart-edit"),
    ("Save selection...", "save-selection"),
    ("Trim...", "trim"),
    ("Cut", "cut"),
    ("Copy", "copy"),
    ("Copy merged", "copy-merged"),
    ("Paste", "paste"),
    ("Paste in place", "paste-in-place"),
    ("Keyboard shortcuts", "shortcuts"),
    ("Reveal all", "reveal-all"),
    ("Rotate image by angle...", "rot-angle"),
    ("Load selection...", "load-selection"),
    ("Replace smart object contents...", "smart-replace"),
    ("Duplicate layer", "duplicate-layer"),
    ("Merge down (group, clipping mask)", "merge-down"),
    ("Merge visible", "merge-visible"),
    ("Flatten image", "flatten"),
    ("Stamp visible to a new layer", "stamp-visible"),
    ("Lock transparent pixels (toggle)", "lock-transparency"),
    ("Lock image pixels (toggle)", "lock-pixels"),
    ("Lock position (toggle)", "lock-position"),
    ("Lock all (toggle)", "lock-all"),
    ("Align left edges", "align-left"),
    ("Align horizontal centres", "align-hcenter"),
    ("Align right edges", "align-right"),
    ("Align top edges", "align-top"),
    ("Align vertical centres", "align-vcenter"),
    ("Align bottom edges", "align-bottom"),
    ("Distribute left edges", "distribute-left"),
    ("Distribute horizontal centres", "distribute-hcenter"),
    ("Distribute right edges", "distribute-right"),
    ("Distribute top edges", "distribute-top"),
    ("Distribute vertical centres", "distribute-vcenter"),
    ("Distribute bottom edges", "distribute-bottom"),
    ("Expand selection...", "sel-expand"),
    ("Contract selection...", "sel-contract"),
    ("Border selection...", "sel-border"),
    ("Smooth selection...", "sel-smooth"),
    ("Grow selection", "sel-grow"),
    ("Select similar", "sel-similar"),
    ("Fill...", "fill-dialog"),
    ("Content-Aware Fill...", "content-aware"),
    ("Show or hide rulers", "rulers"),
    ("Show or hide guides", "guides"),
    ("Lock or unlock guides", "lock-guides"),
    ("Clear guides", "clear-guides"),
    ("New guide...", "new-guide"),
    ("Show or hide the grid", "grid"),
    ("Snap on or off", "snap"),
    ("New fill layer: solid color", "fill-solid"),
    ("New fill layer: gradient", "fill-gradient"),
    ("Load 3D LUT as a Color Lookup...", "load-lut"),
    ("New shape layer from path", "shape-from-path"),
    ("Select and Mask...", "select-mask"),
    ("Import brushes (.abr)...", "import-brushes"),
    ("Define brush tip from selection", "define-brush"),
    ("Convert for smart filters", "sf-convert"),
    ("Spot Healing Brush (Heal ▸ Spot)", "tool-spot-heal"),
    ("Patch tool (Heal ▸ Patch)", "tool-patch"),
    ("Content-Aware Move tool (Heal ▸ Move)", "tool-content-move"),
    ("Red Eye tool (Heal ▸ Red Eye)", "tool-red-eye"),
    ("Blur tool (Brush ▸ Blur)", "tool-blur"),
    ("Sharpen tool (Brush ▸ Sharpen)", "tool-sharpen"),
    ("History Brush (Brush ▸ History)", "tool-history-brush"),
    ("Background Eraser (Eraser ▸ Background)", "tool-bg-eraser"),
    ("Magic Eraser (Eraser ▸ Magic)", "tool-magic-eraser"),
    ("Layers panel", "panel-layers"),
    ("Channels panel", "panel-channels"),
    ("Paths panel", "panel-paths"),
    ("View the RGB composite", "channel-rgb"),
    ("View the red channel alone", "channel-red"),
    ("View the green channel alone", "channel-green"),
    ("View the blue channel alone", "channel-blue"),
    ("Show or hide the Navigator", "navigator"),
    ("Show or hide the Info panel", "info-panel"),
    ("Make work path from selection", "make-work-path"),
    ("Puppet Warp", "puppet-warp"),
    ("Camera Raw Filter...", crate::camera_raw_filter::CRF_ACTION),
    ("Layer via cut", "layer-via-cut"),
    ("Reselect", "reselect"),
    ("Stroke selection...", "stroke-selection"),
    ("Load layer pixels as selection", "select-layer-pixels"),
    ("Bring layer to front", "layer-front"),
    ("Send layer to back", "layer-back"),
    ("Duplicate document", "duplicate-doc"),
    ("Fill with background colour", "fill-bg"),
    ("Export PDF...", "export-pdf"),
    ("New history snapshot", "new-snapshot"),
    ("New fill layer: pattern...", "fill-pattern"),
    ("Define Pattern", "define-pattern"),
    ("Import patterns (.pat)...", "import-patterns"),
    ("Content-Aware Scale", "content-aware-scale"),
    (
        "Perspective Crop tool (Crop ▸ Perspective)",
        "tool-perspective-crop",
    ),
    ("Shadows/Highlights...", "adj-shadows-highlights"),
    ("Replace Color...", "adj-replace-color"),
    ("Match Color...", "adj-match-color"),
    ("Desaturate", "adj-desaturate"),
    ("Equalize", "adj-equalize"),
    ("Auto tone", "auto-tone"),
    (
        "Brightness/Contrast (apply to pixels)...",
        "adjd-brightness-contrast",
    ),
    ("Levels (apply to pixels)...", "adjd-levels"),
    ("Curves (apply to pixels)...", "adjd-curves"),
    ("Exposure (apply to pixels)...", "adjd-exposure"),
    ("Vibrance (apply to pixels)...", "adjd-vibrance"),
    ("Hue/Saturation (apply to pixels)...", "adjd-hue-saturation"),
    ("Color Balance (apply to pixels)...", "adjd-color-balance"),
    ("Black & White (apply to pixels)...", "adjd-black-white"),
    ("Photo Filter (apply to pixels)...", "adjd-photo-filter"),
    ("Channel Mixer (apply to pixels)...", "adjd-channel-mixer"),
    ("Invert (apply to pixels)", "adjd-invert"),
    ("Posterize (apply to pixels)...", "adjd-posterize"),
    ("Threshold (apply to pixels)...", "adjd-threshold"),
    ("Gradient Map (apply to pixels)...", "adjd-gradient-map"),
    ("Selective Color (apply to pixels)...", "adjd-selective-color"),
    (
        "Show or hide the Actions panel",
        crate::actions_panel::ACTIONS_PANEL,
    ),
    ("Record an action", crate::actions_panel::ACTION_RECORD),
    ("Stop recording the action", crate::actions_panel::ACTION_STOP),
    ("Play the selected action", crate::actions_panel::ACTION_PLAY),
    (
        "Proof colors (CMYK: U.S. Web Coated SWOP)",
        crate::soft_proof::PROOF_COLORS,
    ),
    ("Gamut warning (CMYK)", crate::soft_proof::GAMUT_WARNING),
];

impl App {
    /// Help ▸ Keyboard shortcuts: every tool key, every command with a
    /// shortcut (as currently bound), and the keys that are not commands.
    pub(crate) fn shortcuts_reference(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let row = |ui: &mut egui::Ui, what: &str, keys: &str| {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(230.0, 18.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(230.0);
                        ui.add(egui::Label::new(RichText::new(what).color(TEXT)).truncate());
                    },
                );
                ui.label(RichText::new(keys).monospace().color(MUTED));
            });
        };
        let list_h = (ctx.screen_rect().height() - 220.0).clamp(160.0, 560.0);
        let scroll = egui::ScrollArea::vertical()
            .id_salt("shortcuts-reference")
            .max_height(list_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.set_min_width(380.0);
                section_title(ui, "TOOLS");
                for t in Tool::ALL {
                    // The second tool of a pair takes Shift with the key.
                    let shifted = matches!(t, Tool::EllipseSelect | Tool::PolyLasso | Tool::Gradient);
                    let keys = if shifted {
                        format!("Shift+{}", t.key())
                    } else {
                        t.key().to_string()
                    };
                    row(ui, t.name(), &keys);
                }
                row(ui, "Quick selection (Wand sibling)", "Shift+W");
                section_title(ui, "COMMANDS");
                for (label, id) in ACTIONS {
                    let keys = self.action_keys(&ctx, id);
                    if !keys.is_empty() {
                        row(ui, label, &keys);
                    }
                }
                section_title(ui, "CANVAS");
                for (what, keys) in [
                    ("Brush size", "[  ]"),
                    ("Swap / default colours", "X  /  D"),
                    ("Quick mask", "Q"),
                    ("Pan", "Space + drag, scroll"),
                    ("Zoom at the pointer", "Alt + scroll, pinch"),
                    ("Commit / cancel (crop, transform)", "Enter  /  Esc"),
                    ("Add to / subtract from a selection", "Shift  /  Alt"),
                ] {
                    row(ui, what, keys);
                }
                section_title(ui, "TYPING ON THE CANVAS");
                let mac = cfg!(target_os = "macos");
                for (what, keys) in [
                    ("Edit the active text", "Enter (Text tool)"),
                    ("Commit the text", "Esc  /  Cmd+Enter"),
                    ("New line", "Enter"),
                    (
                        "Word / line jumps",
                        if mac {
                            "Alt+Arrow  /  Cmd+Arrow"
                        } else {
                            "Ctrl+Arrow  /  Home End"
                        },
                    ),
                    ("Select word / line / all", "2 / 3 / 4 clicks"),
                    ("Size of the selection", "Cmd+Shift+>  /  <"),
                    ("Move the text", "Cmd + drag"),
                ] {
                    row(ui, what, keys);
                }
            });
        a11y_scroll(ui.ctx(), &scroll, "Keyboard shortcuts");
        crate::dialogs::note(ui, "Command shortcuts can be changed in Preferences.");
    }
}

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
        Filter::Mosaic { .. } => "filter-mosaic",
        Filter::Emboss { .. } => "filter-emboss",
        Filter::FindEdges => "filter-find-edges",
        Filter::SurfaceBlur { .. } => "filter-surface",
        Filter::LensBlur { .. } => "filter-lens",
        Filter::DustScratches { .. } => "filter-dust",
        Filter::Develop { .. } => crate::camera_raw_filter::CRF_ACTION,
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
            .sense(BACKDROP_SENSE)
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
                        a11y_name(&edit, "Search commands");
                        edit.request_focus();
                        if edit.changed() {
                            state.selected = 0;
                        }
                        ui.separator();
                        let scroll_out = egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            for (i, e) in hits.iter().enumerate() {
                                let active = i == state.selected;
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), MENU_ITEM_H),
                                    Sense::click(),
                                );
                                resp.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        e.block.is_none(),
                                        active,
                                        &e.label,
                                    )
                                });
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
                        a11y_scroll(ui.ctx(), &scroll_out, "Commands");
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
        // Shapes transform and flip as vectors, like smart objects.
        let shape = layer.is_some_and(|l| l.shape_layer().is_some());
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
        if let Some(block) = self.layer_action_block(id) {
            return block;
        }
        if let Some(block) = self.smart_filter_action_block(id) {
            return block;
        }
        if let Some(block) = self.panel_action_block(id) {
            return block;
        }
        if let Some(block) = self.everyday_action_block(id) {
            return block;
        }
        if let Some(block) = self.pattern_action_block(id) {
            return block;
        }
        if let Some(block) = self.crf_action_block(id) {
            return block;
        }
        if let Some(block) = self.adjx_action_block(id) {
            return block;
        }
        if let Some(block) = self.actions_action_block(id) {
            return block;
        }
        match id {
            "export-lut" if !self.has_visible_adjustments() => Some("Add an adjustment layer first"),
            "undo" if !self.editor.can_undo() => Some("Nothing to undo"),
            "redo" if !self.editor.can_redo() => Some("Nothing to redo"),
            "flip-h" | "flip-v" if !pixel && !smart && !shape => {
                Some("Select a pixel layer, smart object or shape first")
            }
            "fill" | "clear" | "layer-via-copy" | "smart-object" if !pixel => need_pixel,
            "xform" if !pixel && !smart && !shape => {
                Some("Select a pixel layer, smart object or shape first")
            }
            "perspective" | "warp" if shape => Some("Rasterize the shape first"),
            "perspective" | "warp" if smart => Some("Rasterize the smart object first"),
            "perspective" | "warp" if !pixel => need_pixel,
            "liquify" if smart || layer.is_some_and(|l| l.text_layer().is_some()) => {
                Some("Rasterize the layer first")
            }
            "liquify" if !pixel => need_pixel,
            "puppet-warp" => self.puppet_block(),
            "content-aware-scale" => self.cas_block(),
            "smart-edit" | "smart-replace" if !smart => Some("Select a smart object first"),
            "save-selection" if !selection => need_selection,
            "cut" if !pixel => need_pixel,
            "copy" if layer.and_then(|l| l.raster_store()).is_none() => Some("Select a layer with pixels"),
            "load-selection" if doc.saved_selections.is_empty() => Some("No saved selections yet"),
            "reveal-all" if lumenply_core::canvas_ops::RevealAll::frame(doc) == doc.canvas() => {
                Some("Everything is already on the canvas")
            }
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
            "clear-guides" if doc.guides.is_empty() => Some("There are no guides"),
            "rasterize"
                if !layer.is_some_and(|l| {
                    l.smart_layer().is_some()
                        || l.text_layer().is_some()
                        || l.fill_layer().is_some()
                        || l.shape_layer().is_some()
                }) =>
            {
                Some("Select a smart object, text, fill or shape layer first")
            }
            "shape-from-path" if doc.work_path.as_ref().is_none_or(|p| p.is_empty()) => {
                Some("Draw a path with the Pen first")
            }
            "sel-expand" | "sel-contract" | "sel-border" | "sel-smooth" | "sel-grow" | "sel-similar"
                if !selection =>
            {
                need_selection
            }
            "fill-dialog" | "content-aware" if !pixel => need_pixel,
            "content-aware" if !selection => Some("Select the area to fill first"),
            "select-mask" if !selection => need_selection,
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
        if let Some(keys) = self.layer_action_keys(ctx, id) {
            return keys;
        }
        if let Some(keys) = self.everyday_action_keys(ctx, id) {
            return keys;
        }
        if let Some(keys) = self.panel_action_keys(ctx, id) {
            return keys;
        }
        let (m, k) = match id {
            "fill" => (M::SHIFT, Key::F5),
            "cut" => (M::COMMAND, Key::X),
            "copy" => (M::COMMAND, Key::C),
            "copy-merged" => (M::COMMAND | M::SHIFT, Key::C),
            "paste" => (M::COMMAND, Key::V),
            "paste-in-place" => (M::COMMAND | M::SHIFT, Key::V),
            "fill-dialog" => (M::SHIFT, Key::Backspace),
            "clear" => return "Delete".into(),
            // egui spells these keys "Equals"/"Minus"; show the symbols.
            "zoom-in" | "zoom-out" => {
                let base = shortcut_text(ctx, M::COMMAND, Key::A);
                let sym = if id == "zoom-in" { "=" } else { "−" };
                return format!("{}{sym}", base.trim_end_matches('A'));
            }
            "fit" => (M::NONE, Key::Num0),
            "actual" => (M::NONE, Key::Num1),
            "quick-mask" => (M::NONE, Key::Q),
            "palette" => (M::COMMAND, Key::K),
            "select-mask" => (M::COMMAND | M::ALT, Key::R),
            "content-aware-scale" => (M::COMMAND | M::SHIFT | M::ALT, Key::C),
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
        // While an action records, a recordable edit becomes a step.
        let recorded = self.action_block(id).is_none() && self.record_menu(id);
        self.run_menu_action_unrecorded(id);
        if recorded {
            self.record_menu_done();
        }
    }

    /// [`App::run_menu_action`] without recording (action playback).
    pub(crate) fn run_menu_action_unrecorded(&mut self, id: &str) {
        if let Some(why) = self.action_block(id) {
            self.status = why.into();
            return;
        }
        if self.run_layer_action(id)
            || self.run_smart_filter_action(id)
            || self.run_crf_action(id)
            || self.run_pattern_action(id)
        {
            return;
        }
        if self.run_panel_action(id) || self.run_everyday_action(id) || self.run_adjx_action(id) {
            return;
        }
        if self.run_actions_panel_action(id) || self.run_proof_action(id) || self.run_resolution_action(id) {
            return;
        }
        match id {
            "new" => self.dialog = Some(Dialog::New(1920, 1080, 72.0)),
            "open" => self.pick_open(),
            "demo" => self.open_demo(),
            "place" => self.pick_place(),
            "import-brushes" => self.pick_import_brushes(),
            "define-brush" => self.define_brush_tip(),
            "save" => self.save_live(),
            "saveas" => self.pick_save(),
            "export-png" => self.pick_export_png(),
            "export-pdf" => self.pick_export_pdf(),
            "export-jpeg" => self.pick_export_jpeg(),
            "export-psd" => self.pick_export_psd(),
            "export-psd16" => self.pick_export_psd16(),
            "export-ora" => self.pick_export_ora(),
            "export-16bit" => self.pick_export_16bit(),
            "export-exr" => self.pick_export_exr(),
            "export-lut" => self.pick_export_lut(),
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
            "fill-solid" | "fill-gradient" => self.add_fill_layer(id == "fill-gradient"),
            "load-lut" => self.load_lut_action(),
            "shape-from-path" => self.shape_from_path(),
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
                self.dialog = Some(Dialog::ImageSize(crate::image_size_ui::ImageSizeState::for_doc(
                    d,
                )));
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
            "shortcuts" => self.dialog = Some(Dialog::Shortcuts),
            "cut" => self.cut_pixels(),
            "copy" => {
                self.copy_pixels(false);
            }
            "copy-merged" => {
                self.copy_pixels(true);
            }
            "paste" => self.paste_pixels(false),
            "paste-in-place" => self.paste_pixels(true),
            "liquify" => self.open_liquify(),
            "puppet-warp" => self.open_puppet(),
            "content-aware-scale" => self.open_cas(),
            "tool-perspective-crop" => self.pick_perspective_crop(),
            "export-as" => self.open_export_as(),
            "smart-edit" => self.edit_smart_contents(),
            "save-selection" => {
                let n = self.editor.doc().saved_selections.len() + 1;
                self.dialog = Some(Dialog::SaveSelection(format!("Selection {n}")));
            }
            "load-selection" => self.dialog = Some(Dialog::LoadSelection(0, CombineOp::Replace, false)),
            "trim" => {
                // Transparent borders when there are any, else flat colour.
                let t = lumenply_core::canvas_ops::Trim {
                    basis: lumenply_core::canvas_ops::TrimBasis::Transparent,
                };
                let transparent = t.frame(self.editor.doc()).is_some();
                self.dialog = Some(Dialog::Trim(transparent));
            }
            "reveal-all" => {
                self.run(&lumenply_core::canvas_ops::RevealAll);
                self.view_cmd = Some(ViewCmd::Fit);
            }
            "rot-angle" => self.dialog = Some(Dialog::RotateBy(15.0, true)),
            "smart-replace" => self.pick_replace_smart_contents(),
            "zoom-in" => self.view_cmd = Some(ViewCmd::ZoomIn),
            "zoom-out" => self.view_cmd = Some(ViewCmd::ZoomOut),
            "toggle-history" => {
                self.prefs.history_collapsed = !self.prefs.history_collapsed;
                self.prefs.save();
            }
            "fit" => self.view_cmd = Some(ViewCmd::Fit),
            "actual" => self.view_cmd = Some(ViewCmd::Actual),
            "palette" => self.toggle_palette(),
            "sel-expand" => self.dialog = Some(Dialog::SelectEdge(EdgeOp::Expand(4.0), false)),
            "sel-contract" => self.dialog = Some(Dialog::SelectEdge(EdgeOp::Contract(4.0), false)),
            "sel-border" => self.dialog = Some(Dialog::SelectEdge(EdgeOp::Border(8.0), false)),
            "sel-smooth" => self.dialog = Some(Dialog::SelectEdge(EdgeOp::Smooth(4.0), false)),
            "sel-grow" | "sel-similar" => {
                let sample = match self.active {
                    Some(l) if !self.sample_merged && self.active_is_pixel() => SampleSource::Layer(l),
                    _ => SampleSource::Merged,
                };
                self.run(&GrowSelection {
                    tolerance: self.tolerance,
                    contiguous: id == "sel-grow",
                    sample,
                });
            }
            "fill-dialog" | "content-aware" => {
                // Content-aware needs something to fill; the dialog
                // offers it whenever there is a selection.
                let aware = self.editor.doc().selection.is_some();
                let margin = self.content_aware_margin();
                self.dialog = Some(Dialog::Fill(aware, margin, 0));
            }
            "select-mask" => self.open_select_mask(),
            aid if guides::VIEW_ACTIONS.contains(&aid) => self.run_view_aid(aid),
            tool if self.retouch_tool_action(tool) => {}
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
        assert_eq!(n, 188); // + Proof colors, gamut warning, print size
    }

    #[test]
    fn every_filter_preset_has_its_own_dialog_id() {
        let mut ids: Vec<&str> = filter_presets().iter().map(|(_, f)| filter_id(f)).collect();
        assert_eq!(ids.len(), 13);
        assert!(ids.iter().all(|id| id.starts_with("filter-")));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 13, "two filters share a dialog id");
    }

    #[test]
    fn the_palette_lists_every_tool_once() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 18);
    }
}
