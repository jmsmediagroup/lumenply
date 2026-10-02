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
    name: String,
    visible: bool,
    opacity: f32,
    kind: Kind,
    masked: bool,
    mask_enabled: bool,
    depth: usize,
    collapsed: bool,
}

impl App {
    pub(crate) fn layer_rows(&self) -> Vec<LayerRow> {
        fn walk(layers: &[Layer], depth: usize, out: &mut Vec<LayerRow>) {
            for l in layers.iter().rev() {
                let kind = match l.content {
                    LayerContent::Pixel(_) => Kind::Pixel,
                    LayerContent::Group(_) => Kind::Group,
                    LayerContent::Adjustment(_) => Kind::Adjustment,
                    LayerContent::Filter(_) => Kind::Filter,
                    LayerContent::Text(_) => Kind::Text,
                };
                out.push(LayerRow {
                    id: l.id,
                    name: l.name.clone(),
                    visible: l.visible,
                    opacity: l.opacity,
                    kind,
                    masked: l.mask.is_some(),
                    mask_enabled: l.mask.as_ref().is_some_and(|m| m.enabled),
                    depth,
                    collapsed: l.collapsed,
                });
                if let (Some(children), false) = (l.children(), l.collapsed) {
                    walk(children, depth + 1, out);
                }
            }
        }
        let mut rows = Vec::new();
        walk(self.editor.doc().layers(), 0, &mut rows);
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
        let mut renaming = self.renaming.take();

        egui::ScrollArea::vertical()
            .id_salt("layers")
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &rows {
                    let selected = self.selected.contains(&row.id);
                    let is_active = Some(row.id) == self.active;
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 38.0), Sense::click());
                    let p = ui.painter();
                    if is_active {
                        p.rect_filled(rect, 4.0, ACCENT);
                    } else if selected {
                        p.rect_filled(rect, 4.0, Color32::from_rgb(45, 70, 115));
                    } else if resp.hovered() {
                        p.rect_filled(rect, 4.0, Color32::from_gray(50));
                    }
                    let mut x = rect.min.x + 6.0 + row.depth as f32 * 16.0;
                    let cy = rect.center().y;

                    // visibility checkbox
                    let vis_rect = egui::Rect::from_center_size(egui::pos2(x + 8.0, cy), Vec2::splat(14.0));
                    p.rect_stroke(vis_rect, 2.0, Stroke::new(1.0, Color32::from_gray(150)));
                    if row.visible {
                        p.circle_filled(vis_rect.center(), 4.0, Color32::WHITE);
                    }
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
                        let c = Color32::from_gray(210);
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
                    p.rect_stroke(t_rect, 2.0, Stroke::new(1.0, Color32::from_gray(90)));
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
                            Color32::from_rgb(230, 200, 90)
                        } else if row.mask_enabled {
                            Color32::from_gray(90)
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
                    let text_col = if is_active {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(215)
                    };
                    p.text(
                        egui::pos2(x, cy),
                        Align2::LEFT_CENTER,
                        &row.name,
                        FontId::proportional(14.0),
                        text_col,
                    );
                    let mut extra = String::new();
                    if row.kind == Kind::Adjustment {
                        extra.push_str("adj  ");
                    }
                    if row.kind == Kind::Filter {
                        extra.push_str("live filter  ");
                    }
                    if row.kind == Kind::Text {
                        extra.push_str("text  ");
                    }
                    if row.opacity < 0.999 {
                        extra.push_str(&format!("{:.0}%", row.opacity * 100.0));
                    }
                    if !extra.is_empty() {
                        p.text(
                            rect.right_center() - egui::vec2(8.0, 0.0),
                            Align2::RIGHT_CENTER,
                            extra.trim_end(),
                            FontId::proportional(11.5),
                            if is_active {
                                Color32::from_gray(225)
                            } else {
                                Color32::from_gray(140)
                            },
                        );
                    }
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
        if let Some((layer, visible)) = toggle_vis {
            self.run(&SetVisible { layer, visible });
        }
        if let Some((layer, collapsed)) = toggle_collapse {
            self.run(&SetCollapsed { layer, collapsed });
        }

        let mut action: Option<&str> = None;
        let mut add_adj = None;
        let mut add_filter = None;
        ui.horizontal(|ui| {
            if ui.button("+ Layer").clicked() {
                action = Some("add");
            }
            ui.menu_button("+ Adjustment", |ui| {
                for (name, adj) in adjustment_presets() {
                    if ui.button(name).clicked() {
                        add_adj = Some(adj);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("+ Filter", |ui| {
                for (name, f) in filter_presets() {
                    if ui.button(name).clicked() {
                        add_filter = Some(f);
                        ui.close_menu();
                    }
                }
            });
            if ui
                .button("Group")
                .on_hover_text("Group selected layers (Ctrl+click to multi-select)")
                .clicked()
            {
                action = Some("group");
            }
            if ui
                .add_enabled(self.active_is_group(), egui::Button::new("Ungroup"))
                .clicked()
            {
                action = Some("ungroup");
            }
            if ui.button("Delete").clicked() {
                action = Some("delete");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Up").clicked() {
                action = Some("up");
            }
            if ui.button("Down").clicked() {
                action = Some("down");
            }
            let has_mask = self.active_has_mask();
            if ui
                .add_enabled(self.active.is_some() && !has_mask, egui::Button::new("Add mask"))
                .clicked()
            {
                action = Some("addmask");
            }
            if ui
                .add_enabled(has_mask, egui::Button::new("Remove mask"))
                .clicked()
            {
                action = Some("rmmask");
            }
            if has_mask {
                let enabled = self
                    .active_layer()
                    .and_then(|l| l.mask.as_ref())
                    .is_some_and(|m| m.enabled);
                let mut e = enabled;
                if ui.checkbox(&mut e, "On").changed() {
                    action = Some(if e { "maskon" } else { "maskoff" });
                }
            }
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
    let (col, txt) = match kind {
        Kind::Pixel => (Color32::from_rgb(70, 130, 200), "P"),
        Kind::Adjustment => (Color32::from_rgb(205, 140, 45), "A"),
        Kind::Group => (Color32::from_rgb(90, 170, 110), "G"),
        Kind::Filter => (Color32::from_rgb(150, 90, 190), "F"),
        Kind::Text => (Color32::from_rgb(60, 150, 150), "T"),
    };
    p.rect_filled(rect, 2.0, col);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        txt,
        FontId::proportional(13.0),
        Color32::WHITE,
    );
}
