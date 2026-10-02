use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Pixel,
    Group,
    Adjustment,
    Filter,
    Text,
}

pub(crate) struct LayerRow {
    id: LayerId,
    /// Group this row sits in (`None` = root), for drag-and-drop targets.
    parent: Option<LayerId>,
    name: String,
    visible: bool,
    opacity: f32,
    kind: Kind,
    /// Kind label shown at the right edge: "Text", "Curves", "Live blur"...
    chip: Option<&'static str>,
    masked: bool,
    mask_enabled: bool,
    depth: usize,
    collapsed: bool,
}

impl App {
    pub(crate) fn layer_rows(&self) -> Vec<LayerRow> {
        fn walk(layers: &[Layer], parent: Option<LayerId>, depth: usize, out: &mut Vec<LayerRow>) {
            for l in layers.iter().rev() {
                let (kind, chip) = match &l.content {
                    LayerContent::Pixel(_) => (Kind::Pixel, None),
                    LayerContent::Group(_) => (Kind::Group, None),
                    LayerContent::Adjustment(a) => (Kind::Adjustment, Some(a.name())),
                    LayerContent::Filter(f) => (
                        Kind::Filter,
                        Some(match f {
                            Filter::GaussianBlur { .. } | Filter::BoxBlur { .. } => "Live blur",
                            Filter::Sharpen { .. } => "Live sharpen",
                            Filter::Noise { .. } => "Live noise",
                            Filter::MotionBlur { .. } => "Live motion",
                            Filter::Median { .. } => "Live median",
                            Filter::HighPass { .. } => "Live high pass",
                        }),
                    ),
                    LayerContent::Text(_) => (Kind::Text, Some("Text")),
                };
                out.push(LayerRow {
                    id: l.id,
                    parent,
                    name: l.name.clone(),
                    visible: l.visible,
                    opacity: l.opacity,
                    kind,
                    chip,
                    masked: l.mask.is_some(),
                    mask_enabled: l.mask.as_ref().is_some_and(|m| m.enabled),
                    depth,
                    collapsed: l.collapsed,
                });
                if let (Some(children), false) = (l.children(), l.collapsed) {
                    walk(children, Some(l.id), depth + 1, out);
                }
            }
        }
        let mut rows = Vec::new();
        walk(self.editor.doc().layers(), None, 0, &mut rows);
        rows
    }

    pub(crate) fn layers_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "LAYERS");
        let rows = self.layer_rows();
        let ctrl = ui.input(|i| i.modifiers.command);

        let mut toggle_vis = None;
        let mut select: Option<(LayerId, bool)> = None;
        let mut toggle_collapse = None;
        let mut rename_start = None;
        let mut rename_commit = None;
        let mut rename_cancel = false;
        let mut mask_click = None;
        let mut ctx_action: Option<(&'static str, LayerId)> = None;
        let mut drop_action: Option<(LayerId, Option<LayerId>, usize)> = None;
        let mut row_rects: Vec<egui::Rect> = Vec::new();
        let mut renaming = self.renaming.take();

        egui::ScrollArea::vertical()
            .id_salt("layers")
            .max_height((ui.available_height() - 46.0).max(160.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &rows {
                    let selected = self.selected.contains(&row.id);
                    let is_active = Some(row.id) == self.active;
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 38.0), Sense::click_and_drag());
                    row_rects.push(rect);
                    if resp.drag_started() {
                        self.layer_drag = Some(row.id);
                        self.set_active(Some(row.id));
                    }
                    let p = ui.painter();
                    if is_active {
                        p.rect_filled(rect, 6.0, ACCENT_TINT);
                        p.rect_stroke(rect.shrink(1.0), 6.0, Stroke::new(1.5, ACCENT));
                    } else if selected {
                        p.rect_filled(rect, 6.0, RAISED);
                        p.rect_stroke(rect.shrink(1.0), 6.0, Stroke::new(1.0, MUTED));
                    } else if resp.hovered() {
                        p.rect_filled(rect, 6.0, RAISED);
                    }
                    let mut x = rect.min.x + 6.0 + row.depth as f32 * 16.0;
                    let cy = rect.center().y;

                    // visibility checkbox
                    let vis_rect = egui::Rect::from_center_size(egui::pos2(x + 8.0, cy), Vec2::splat(16.0));
                    paint_eye(p, vis_rect, row.visible);
                    if resp.clicked()
                        && resp
                            .interact_pointer_pos()
                            .is_some_and(|q| vis_rect.expand(3.0).contains(q))
                    {
                        toggle_vis = Some((row.id, !row.visible));
                    }
                    x += 22.0;

                    // group disclosure triangle
                    if row.kind == Kind::Group {
                        let tri_rect =
                            egui::Rect::from_center_size(egui::pos2(x + 6.0, cy), Vec2::splat(12.0));
                        let c = TEXT;
                        let pts = if row.collapsed {
                            vec![
                                egui::pos2(tri_rect.min.x + 2.0, tri_rect.min.y),
                                egui::pos2(tri_rect.max.x, tri_rect.center().y),
                                egui::pos2(tri_rect.min.x + 2.0, tri_rect.max.y),
                            ]
                        } else {
                            vec![
                                egui::pos2(tri_rect.min.x, tri_rect.min.y + 2.0),
                                egui::pos2(tri_rect.max.x, tri_rect.min.y + 2.0),
                                egui::pos2(tri_rect.center().x, tri_rect.max.y),
                            ]
                        };
                        p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
                        if resp.clicked()
                            && resp
                                .interact_pointer_pos()
                                .is_some_and(|q| tri_rect.expand(4.0).contains(q))
                        {
                            toggle_collapse = Some((row.id, !row.collapsed));
                        }
                        x += 16.0;
                    }

                    // thumbnail
                    let t_rect = egui::Rect::from_min_size(
                        egui::pos2(x, cy - 15.0),
                        egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                    );
                    paint_thumb_bg(p, t_rect);
                    if let Some(tex) = self.thumbs.get(&row.id) {
                        p.image(
                            tex.id(),
                            t_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        badge(p, t_rect, row.kind);
                    }
                    p.rect_stroke(t_rect, 2.0, Stroke::new(1.0, LINE));
                    x += THUMB.0 as f32 + 6.0;

                    // mask thumbnail
                    if row.masked {
                        let m_rect = egui::Rect::from_min_size(
                            egui::pos2(x, cy - 15.0),
                            egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                        );
                        if let Some(tex) = self.mask_thumbs.get(&row.id) {
                            p.image(
                                tex.id(),
                                m_rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        let editing = is_active && self.editing_mask;
                        let col = if editing {
                            ACCENT
                        } else if row.mask_enabled {
                            LINE
                        } else {
                            Color32::from_rgb(200, 70, 70)
                        };
                        p.rect_stroke(m_rect, 2.0, Stroke::new(if editing { 2.0 } else { 1.0 }, col));
                        if !row.mask_enabled {
                            p.line_segment(
                                [m_rect.left_top(), m_rect.right_bottom()],
                                Stroke::new(1.5, Color32::from_rgb(200, 70, 70)),
                            );
                        }
                        if resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| m_rect.contains(q)) {
                            mask_click = Some(row.id);
                        }
                        x += THUMB.0 as f32 + 6.0;
                    }

                    // name (or rename box)
                    if let Some((rid, text)) = renaming.as_mut() {
                        if *rid == row.id {
                            let edit_rect = egui::Rect::from_min_max(
                                egui::pos2(x, cy - 11.0),
                                egui::pos2(rect.max.x - 6.0, cy + 11.0),
                            );
                            let r = ui.put(edit_rect, egui::TextEdit::singleline(text));
                            r.request_focus();
                            let (enter, escape, click_away) = ui.input(|i| {
                                (
                                    i.key_pressed(Key::Enter),
                                    i.key_pressed(Key::Escape),
                                    i.pointer.any_pressed() && !r.hovered(),
                                )
                            });
                            if escape {
                                rename_cancel = true;
                            } else if enter || click_away || r.lost_focus() {
                                rename_commit = Some((row.id, text.clone()));
                            }
                            continue;
                        }
                    }
                    let mut right = rect.max.x - 8.0;
                    if let Some(label) = row.chip {
                        let ink = if row.kind == Kind::Filter {
                            LIVE_FILTER
                        } else {
                            MUTED
                        };
                        right -= kind_chip(p, egui::pos2(right, cy), label, ink) + 6.0;
                    }
                    if row.opacity < 0.999 {
                        right -= p
                            .text(
                                egui::pos2(right, cy),
                                Align2::RIGHT_CENTER,
                                format!("{:.0}%", row.opacity * 100.0),
                                FontId::monospace(10.5),
                                MUTED,
                            )
                            .width()
                            + 6.0;
                    }
                    // Name last, clipped so it never runs under the chips.
                    let name_clip = egui::Rect::from_min_max(
                        egui::pos2(x, rect.min.y),
                        egui::pos2(right - 2.0, rect.max.y),
                    );
                    p.with_clip_rect(name_clip).text(
                        egui::pos2(x, cy),
                        Align2::LEFT_CENTER,
                        &row.name,
                        FontId::proportional(14.0),
                        TEXT,
                    );
                    resp.context_menu(|ui| {
                        if ui.button("Rename").clicked() {
                            rename_start = Some((row.id, row.name.clone()));
                            ui.close_menu();
                        }
                        for (label, act) in [
                            ("Move up", "up"),
                            ("Move down", "down"),
                            ("Flip horizontal", "fliph"),
                            ("Flip vertical", "flipv"),
                        ] {
                            if ui.button(label).clicked() {
                                ctx_action = Some((act, row.id));
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        if row.masked {
                            if ui.button("Remove mask").clicked() {
                                ctx_action = Some(("rmmask", row.id));
                                ui.close_menu();
                            }
                            let label = if row.mask_enabled {
                                "Disable mask"
                            } else {
                                "Enable mask"
                            };
                            if ui.button(label).clicked() {
                                ctx_action =
                                    Some((if row.mask_enabled { "maskoff" } else { "maskon" }, row.id));
                                ui.close_menu();
                            }
                        } else if ui.button("Add mask").clicked() {
                            ctx_action = Some(("addmask", row.id));
                            ui.close_menu();
                        }
                        ui.separator();
                        if row.kind == Kind::Group && ui.button("Ungroup").clicked() {
                            ctx_action = Some(("ungroup", row.id));
                            ui.close_menu();
                        }
                        if ui.button("Delete").clicked() {
                            ctx_action = Some(("delete", row.id));
                            ui.close_menu();
                        }
                    });
                    if resp.double_clicked() {
                        rename_start = Some((row.id, row.name.clone()));
                    } else if resp.clicked() {
                        let on_control = resp
                            .interact_pointer_pos()
                            .is_some_and(|q| q.x < x - 2.0 && q.x > vis_rect.max.x + 2.0);
                        if !on_control
                            && !resp
                                .interact_pointer_pos()
                                .is_some_and(|q| vis_rect.expand(3.0).contains(q))
                        {
                            select = Some((row.id, ctrl));
                        }
                    }
                }
                // Drag-to-reorder: an accent insertion line follows the
                // pointer; releasing moves the layer there.
                if let Some(dragged) = self.layer_drag {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    let pointer = ui.ctx().pointer_latest_pos();
                    let released = ui.ctx().input(|i| i.pointer.any_released());
                    if let (Some(p), false) = (pointer, row_rects.is_empty()) {
                        let mut slot = row_rects.len();
                        for (i, r) in row_rects.iter().enumerate() {
                            if p.y < r.center().y {
                                slot = i;
                                break;
                            }
                        }
                        let y = if slot < row_rects.len() {
                            row_rects[slot].top() - 1.0
                        } else {
                            row_rects[row_rects.len() - 1].bottom() + 1.0
                        };
                        let x0 = row_rects[0].left();
                        let x1 = row_rects[0].right();
                        ui.painter().hline(x0..=x1, y, Stroke::new(2.0, ACCENT));
                        if released {
                            let target = rows.get(slot).map(|r| (r.parent, r.id));
                            drop_action = Some(match target {
                                // Above the row below the line, in that row's group.
                                Some((parent, below)) => {
                                    let doc = self.editor.doc();
                                    let list = match parent {
                                        None => Some(doc.layers()),
                                        Some(g) => doc.layer(g).and_then(|l| l.children()),
                                    };
                                    let pos = list
                                        .and_then(|l| l.iter().position(|x| x.id == below))
                                        .map_or(0, |e| e + 1);
                                    // Removal shifts the slot when moving
                                    // upward within the same list.
                                    let adjust = list
                                        .and_then(|l| l.iter().position(|x| x.id == dragged))
                                        .is_some_and(|m| m < pos);
                                    (dragged, parent, pos - usize::from(adjust))
                                }
                                // Below everything: the bottom of the root.
                                None => (dragged, None, 0),
                            });
                        }
                    }
                    if released {
                        self.layer_drag = None;
                    }
                }
            });

        if rename_cancel {
            renaming = None;
        }
        if let Some((layer, name)) = rename_commit {
            renaming = None;
            self.run(&RenameLayer { layer, name });
        }
        if rename_start.is_some() {
            renaming = rename_start;
        }
        self.renaming = renaming;
        if let Some((id, add)) = select {
            if add {
                if let Some(i) = self.selected.iter().position(|x| *x == id) {
                    if self.selected.len() > 1 {
                        self.selected.remove(i);
                    }
                } else {
                    self.selected.push(id);
                }
                self.active = Some(id);
                self.editing_mask = false;
            } else {
                self.set_active(Some(id));
            }
        }
        if let Some(id) = mask_click {
            if Some(id) == self.active {
                self.editing_mask = !self.editing_mask;
            } else {
                self.set_active(Some(id));
                self.editing_mask = true;
            }
        }
        if let Some((layer, parent, index)) = drop_action {
            self.run(&RelocateLayer { layer, parent, index });
        }
        if let Some((act, id)) = ctx_action {
            // Context-menu actions act on the clicked row.
            self.set_active(Some(id));
            match act {
                "up" => self.reorder_active(1),
                "down" => self.reorder_active(-1),
                "fliph" => self.flip_active(true),
                "flipv" => self.flip_active(false),
                "delete" => self.delete_active(),
                "ungroup" => self.ungroup_active(),
                "addmask" => {
                    self.run(&AddMask { layer: id });
                    self.editing_mask = true;
                }
                "rmmask" => {
                    self.run(&RemoveMask { layer: id });
                    self.editing_mask = false;
                }
                "maskon" | "maskoff" => self.run(&SetMaskEnabled {
                    layer: id,
                    enabled: act == "maskon",
                }),
                _ => {}
            }
        }
        if let Some((layer, visible)) = toggle_vis {
            self.run(&SetVisible { layer, visible });
        }
        if let Some((layer, collapsed)) = toggle_collapse {
            self.run(&SetCollapsed { layer, collapsed });
        }

        let mut action: Option<&str> = None;
        let mut add_adj = None;
        let mut add_filter = None;
        let has_mask = self.active_has_mask();
        let mask_enabled = self
            .active_layer()
            .and_then(|l| l.mask.as_ref())
            .is_some_and(|m| m.enabled);
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui.add(IconButton::new(icon_plus, "New layer")).clicked() {
                action = Some("add");
            }
            if ui
                .add(IconButton::new(
                    icon_folder,
                    "Group selected layers (Ctrl+click to multi-select)",
                ))
                .clicked()
            {
                action = Some("group");
            }
            icon_menu(ui, "add-adj", icon_adjustment, "New adjustment layer", |ui| {
                for (name, adj) in adjustment_presets() {
                    if ui.button(name).clicked() {
                        add_adj = Some(adj);
                    }
                }
            });
            icon_menu(ui, "add-filter", icon_filter, "New live filter layer", |ui| {
                for (name, f) in filter_presets() {
                    if ui.button(name).clicked() {
                        add_filter = Some(f);
                    }
                }
            });
            if ui
                .add_enabled(
                    self.active.is_some() && !has_mask,
                    IconButton::new(icon_mask, "Add mask (from the selection if there is one)"),
                )
                .clicked()
            {
                action = Some("addmask");
            }
            if ui.add(IconButton::new(icon_up, "Move layer up")).clicked() {
                action = Some("up");
            }
            if ui.add(IconButton::new(icon_down, "Move layer down")).clicked() {
                action = Some("down");
            }
            let is_group = self.active_is_group();
            icon_menu(ui, "layer-more", icon_more, "More", |ui| {
                if ui.add_enabled(is_group, egui::Button::new("Ungroup")).clicked() {
                    action = Some("ungroup");
                }
                if ui
                    .add_enabled(has_mask, egui::Button::new("Remove mask"))
                    .clicked()
                {
                    action = Some("rmmask");
                }
                if has_mask {
                    let label = if mask_enabled {
                        "Disable mask"
                    } else {
                        "Enable mask"
                    };
                    if ui.button(label).clicked() {
                        action = Some(if mask_enabled { "maskoff" } else { "maskon" });
                    }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.active.is_some(), IconButton::new(icon_trash, "Delete layer"))
                    .clicked()
                {
                    action = Some("delete");
                }
            });
        });
        match action {
            Some("add") => self.add_pixel_layer(),
            Some("group") => self.group_selected(),
            Some("ungroup") => self.ungroup_active(),
            Some("delete") => self.delete_active(),
            Some("up") => self.reorder_active(1),
            Some("down") => self.reorder_active(-1),
            Some("addmask") => {
                if let Some(l) = self.active {
                    self.run(&AddMask { layer: l });
                    self.editing_mask = true;
                }
            }
            Some("rmmask") => {
                if let Some(l) = self.active {
                    self.run(&RemoveMask { layer: l });
                    self.editing_mask = false;
                }
            }
            Some("maskon") | Some("maskoff") => {
                if let Some(l) = self.active {
                    self.run(&SetMaskEnabled {
                        layer: l,
                        enabled: action == Some("maskon"),
                    });
                }
            }
            _ => {}
        }
        if let Some(adj) = add_adj {
            self.add_adjustment(adj);
        }
        if let Some(f) = add_filter {
            self.add_filter_layer(f);
        }
    }
}

pub(crate) fn badge(p: &egui::Painter, rect: egui::Rect, kind: Kind) {
    let (col, ink, txt) = match kind {
        Kind::Pixel => (RAISED, TEXT, "P"),
        Kind::Adjustment => (ACCENT, ACCENT_INK, "A"),
        Kind::Group => (RAISED, TEXT, "G"),
        Kind::Filter => (LIVE_FILTER, ACCENT_INK, "F"),
        Kind::Text => (RAISED, TEXT, "T"),
    };
    p.rect_filled(rect, 2.0, col);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        txt,
        FontId::proportional(13.0),
        ink,
    );
}

/// Paint a small kind chip whose right edge sits at `right`; returns its width.
fn kind_chip(p: &egui::Painter, right: egui::Pos2, label: &str, ink: Color32) -> f32 {
    let galley = p.layout_no_wrap(label.into(), FontId::proportional(10.5), ink);
    let size = galley.size() + egui::vec2(12.0, 6.0);
    let rect = egui::Rect::from_min_size(right - egui::vec2(size.x, size.y / 2.0), size);
    p.rect_filled(rect, 4.0, GROUND);
    p.galley(rect.min + egui::vec2(6.0, 3.0), galley, ink);
    size.x
}

/// An [`IconButton`] that opens a popup of actions below itself.
fn icon_menu(
    ui: &mut egui::Ui,
    id: &str,
    draw: fn(&egui::Painter, egui::Rect, Color32),
    tip: &'static str,
    content: impl FnOnce(&mut egui::Ui),
) {
    let resp = ui.add(IconButton::new(draw, tip));
    let popup = ui.make_persistent_id(id);
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup::popup_below_widget(ui, popup, &resp, egui::PopupCloseBehavior::CloseOnClick, content);
}

/// A square button drawn with one of the original line icons below.
pub(crate) struct IconButton {
    draw: fn(&egui::Painter, egui::Rect, Color32),
    tip: &'static str,
}

impl IconButton {
    pub(crate) fn new(draw: fn(&egui::Painter, egui::Rect, Color32), tip: &'static str) -> Self {
        IconButton { draw, tip }
    }
}

impl egui::Widget for IconButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
        let enabled = ui.is_enabled();
        if resp.hovered() && enabled {
            ui.painter().rect_filled(rect, 5.0, RAISED);
        }
        let ink = if enabled { TEXT } else { LINE };
        (self.draw)(ui.painter(), rect.shrink(5.0), ink);
        resp.on_hover_text(self.tip)
    }
}

// Original icons on a nominal 20 px grid, 1.6 px stroke. `r` is the box.
/// A small eye: open when the layer is visible, a slashed outline when not.
pub(crate) fn paint_eye(p: &egui::Painter, r: egui::Rect, open: bool) {
    let c = if open { TEXT } else { MUTED };
    let s = Stroke::new(1.4, c);
    let (cx, cy) = (r.center().x, r.center().y);
    let w = r.width() * 0.46;
    // Two arcs approximated with short polylines.
    let n = 8;
    for dir in [-1.0f32, 1.0] {
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32 * 2.0 - 1.0; // -1..1
                egui::pos2(cx + t * w, cy + dir * (1.0 - t * t) * r.height() * 0.28)
            })
            .collect();
        p.add(Shape::line(pts, s));
    }
    if open {
        p.circle_filled(egui::pos2(cx, cy), r.width() * 0.14, c);
    } else {
        p.line_segment(
            [
                egui::pos2(r.min.x + 1.0, r.max.y - 1.0),
                egui::pos2(r.max.x - 1.0, r.min.y + 1.0),
            ],
            s,
        );
    }
}

fn icon_plus(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    p.line_segment([r.center_top(), r.center_bottom()], s);
    p.line_segment([r.left_center(), r.right_center()], s);
}

fn icon_folder(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    let top = r.min.y + r.height() * 0.25;
    let pts = vec![
        egui::pos2(r.min.x, r.max.y),
        egui::pos2(r.min.x, top),
        egui::pos2(r.min.x + r.width() * 0.4, top),
        egui::pos2(r.min.x + r.width() * 0.55, r.min.y + r.height() * 0.42),
        egui::pos2(r.max.x, r.min.y + r.height() * 0.42),
        egui::pos2(r.max.x, r.max.y),
        egui::pos2(r.min.x, r.max.y),
    ];
    p.add(Shape::line(pts, s));
}

fn icon_adjustment(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let radius = r.width() * 0.46;
    p.circle_stroke(r.center(), radius, Stroke::new(1.6, c));
    // Filled right half: a contrast dial.
    let n = 9;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / n as f32;
            r.center() + egui::vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
}

fn icon_filter(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    let w = r.width();
    let y = |f: f32| r.min.y + r.height() * f;
    p.line_segment([egui::pos2(r.min.x, y(0.25)), egui::pos2(r.max.x, y(0.25))], s);
    p.line_segment(
        [
            egui::pos2(r.min.x + w * 0.15, y(0.55)),
            egui::pos2(r.max.x - w * 0.15, y(0.55)),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(r.min.x + w * 0.3, y(0.85)),
            egui::pos2(r.max.x - w * 0.3, y(0.85)),
        ],
        s,
    );
}

fn icon_mask(p: &egui::Painter, r: egui::Rect, c: Color32) {
    p.rect_stroke(r, 2.0, Stroke::new(1.6, c));
    p.circle_filled(r.center(), r.width() * 0.26, c);
}

fn icon_up(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    let m = r.center_top() + egui::vec2(0.0, r.height() * 0.2);
    p.line_segment([m, egui::pos2(r.min.x + r.width() * 0.2, r.center().y)], s);
    p.line_segment([m, egui::pos2(r.max.x - r.width() * 0.2, r.center().y)], s);
    p.line_segment(
        [
            r.center_top() + egui::vec2(0.0, r.height() * 0.2),
            r.center_bottom(),
        ],
        s,
    );
}

fn icon_down(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    let m = r.center_bottom() - egui::vec2(0.0, r.height() * 0.2);
    p.line_segment([m, egui::pos2(r.min.x + r.width() * 0.2, r.center().y)], s);
    p.line_segment([m, egui::pos2(r.max.x - r.width() * 0.2, r.center().y)], s);
    p.line_segment([r.center_top(), m], s);
}

fn icon_more(p: &egui::Painter, r: egui::Rect, c: Color32) {
    for i in -1..=1 {
        p.circle_filled(r.center() + egui::vec2(i as f32 * r.width() * 0.33, 0.0), 1.4, c);
    }
}

fn icon_trash(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    let top = r.min.y + r.height() * 0.2;
    p.line_segment([egui::pos2(r.min.x, top), egui::pos2(r.max.x, top)], s);
    p.line_segment(
        [
            egui::pos2(r.center().x - 3.0, r.min.y),
            egui::pos2(r.center().x + 3.0, r.min.y),
        ],
        s,
    );
    let body = egui::Rect::from_min_max(
        egui::pos2(r.min.x + r.width() * 0.12, top + 2.0),
        egui::pos2(r.max.x - r.width() * 0.12, r.max.y),
    );
    p.rect_stroke(body, 1.5, s);
}
