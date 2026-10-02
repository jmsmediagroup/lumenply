use super::*;

/// Layer row height and the gap below it.
const ROW_H: f32 = 38.0;
const ROW_GAP: f32 = 2.0;
/// Vertical pitch of the layer list, for the dock's height budget.
pub(crate) const ROW_PITCH: f32 = ROW_H + ROW_GAP;
/// Height of the LAYERS title and of the icon footer.
pub(crate) const HEADER_H: f32 = 30.0;
pub(crate) const FOOTER_H: f32 = 40.0;
/// Ink for disabled icons: dimmed, but still legible.
const DISABLED_INK: Color32 = Color32::from_rgb(0x5A, 0x61, 0x6B);

/// A shorter kind label for a narrow row ("Hue/Saturation" → "Hue/Sat").
fn short_chip(label: &str) -> &str {
    match label {
        "Hue/Saturation" => "Hue/Sat",
        "Brightness/Contrast" => "Bri/Con",
        "Black & White" => "B & W",
        "Color Balance" => "Balance",
        "Live high pass" => "Live HP",
        "Live sharpen" => "Live sharp",
        "Live motion" => "Live blur",
        other => other,
    }
}

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
    clip: bool,
    /// Kind label shown at the right edge: "Text", "Curves", "Live blur"...
    chip: Option<&'static str>,
    masked: bool,
    mask_enabled: bool,
    depth: usize,
    collapsed: bool,
}

impl LayerRow {
    pub(crate) fn id(&self) -> LayerId {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
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
                    LayerContent::Smart(_) => (Kind::Pixel, Some("Smart")),
                };
                out.push(LayerRow {
                    id: l.id,
                    parent,
                    name: l.name.clone(),
                    visible: l.visible,
                    opacity: l.opacity,
                    kind,
                    clip: l.clip,
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

        if rows.is_empty() {
            // Empty document: say what to do instead of showing a void.
            let h = (ui.available_height() - FOOTER_H).clamp(48.0, 120.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), h),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("No layers yet").color(TEXT));
                    ui.label(
                        RichText::new("Add one with + below, or drop an image onto the window.")
                            .small()
                            .color(MUTED),
                    );
                },
            );
        }
        let scroll_out = egui::ScrollArea::vertical()
            .id_salt("layers")
            .max_height((ui.available_height() - FOOTER_H).max(ROW_H))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = ROW_GAP;
                for row in &rows {
                    let selected = self.selected.contains(&row.id);
                    let is_active = Some(row.id) == self.active;
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click_and_drag());
                    row_rects.push(rect);
                    if resp.drag_started() {
                        self.layer_drag = Some(row.id);
                        self.set_active(Some(row.id));
                    }
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            is_active,
                            format!("Layer {}", row.name),
                        )
                    });
                    let p = ui.painter();
                    if is_active {
                        p.rect_filled(rect, RADIUS, ACCENT_TINT);
                        p.rect_stroke(rect.shrink(1.0), RADIUS, Stroke::new(1.5, ACCENT));
                    } else if selected {
                        p.rect_filled(rect, RADIUS, RAISED);
                        p.rect_stroke(rect.shrink(1.0), RADIUS, Stroke::new(1.0, MUTED));
                    } else if resp.hovered() {
                        p.rect_filled(rect, RADIUS, RAISED);
                    }
                    if resp.has_focus() {
                        p.rect_stroke(rect, RADIUS, Stroke::new(1.5, ACCENT));
                    }
                    let hover = resp.hover_pos();
                    let mut tip: Option<String> = None;
                    let mut x = rect.min.x + 6.0 + row.depth as f32 * 16.0;
                    let cy = rect.center().y;
                    if row.clip {
                        // Clipped layers tuck under their base with a bent
                        // arrow pointing at it.
                        x += 14.0;
                        let s = Stroke::new(1.4, MUTED);
                        let bx = x - 8.0;
                        p.line_segment([egui::pos2(bx, cy - 6.0), egui::pos2(bx, cy + 2.0)], s);
                        p.line_segment([egui::pos2(bx, cy + 2.0), egui::pos2(bx + 5.0, cy + 2.0)], s);
                        p.line_segment(
                            [egui::pos2(bx + 5.0, cy + 2.0), egui::pos2(bx + 2.0, cy - 1.0)],
                            s,
                        );
                        p.line_segment(
                            [egui::pos2(bx + 5.0, cy + 2.0), egui::pos2(bx + 2.0, cy + 5.0)],
                            s,
                        );
                    }

                    // visibility checkbox
                    let vis_rect = egui::Rect::from_center_size(egui::pos2(x + 8.0, cy), Vec2::splat(16.0));
                    if hover.is_some_and(|q| vis_rect.expand(3.0).contains(q)) {
                        p.rect_filled(vis_rect.expand(3.0), 4.0, HOVER);
                        tip = Some(if row.visible { "Hide layer" } else { "Show layer" }.into());
                    }
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
                        if hover.is_some_and(|q| tri_rect.expand(4.0).contains(q)) {
                            p.rect_filled(tri_rect.expand(3.0), 4.0, HOVER);
                            tip = Some(
                                if row.collapsed {
                                    "Expand group"
                                } else {
                                    "Collapse group"
                                }
                                .into(),
                            );
                        }
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
                        let on_mask = hover.is_some_and(|q| m_rect.contains(q));
                        let col = if editing {
                            ACCENT
                        } else if !row.mask_enabled {
                            DANGER
                        } else if on_mask {
                            MUTED
                        } else {
                            LINE
                        };
                        p.rect_stroke(m_rect, 2.0, Stroke::new(if editing { 2.0 } else { 1.0 }, col));
                        if !row.mask_enabled {
                            p.line_segment(
                                [m_rect.left_top(), m_rect.right_bottom()],
                                Stroke::new(1.5, DANGER),
                            );
                        }
                        if on_mask {
                            tip = Some(
                                if editing {
                                    "Painting on the mask — click to edit the layer again"
                                } else if row.mask_enabled {
                                    "Layer mask — click to paint on it"
                                } else {
                                    "Layer mask (disabled) — click to paint on it"
                                }
                                .into(),
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
                            a11y_name(&r, "Layer name");
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
                            // `put` leaves the cursor under the (shorter) field;
                            // keep the next row at its usual place.
                            let short = rect.max.y + ROW_GAP - ui.cursor().top();
                            if short > 0.0 {
                                ui.add_space(short);
                            }
                            continue;
                        }
                    }
                    // Right edge: opacity (when not 100%), then the kind
                    // chip; the name takes the rest and is elided with "…"
                    // rather than clipped. The chip shortens, then drops,
                    // before the name gets too short to read.
                    let mut right = rect.max.x - 8.0;
                    let name_font = FontId::proportional(14.0);
                    let name_w = ui.fonts(|f| {
                        f.layout_no_wrap(row.name.clone(), name_font.clone(), TEXT)
                            .size()
                            .x
                    });
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
                            + 8.0;
                    }
                    // A chip that only repeats the name ("Curves" named
                    // Curves) adds nothing; the badge already shows the kind.
                    let chip = row.chip.filter(|c| !c.eq_ignore_ascii_case(&row.name));
                    let mut chip_dropped = false;
                    if let Some(label) = chip {
                        let ink = if row.kind == Kind::Filter {
                            LIVE_FILTER
                        } else {
                            MUTED
                        };
                        let room = right - x;
                        let full = kind_chip_width(ui, label);
                        let short = short_chip(label);
                        let pick = if name_w + 10.0 + full <= room {
                            Some(label)
                        } else if room - kind_chip_width(ui, short) - 10.0 >= 56.0_f32.min(name_w) {
                            Some(short)
                        } else {
                            None
                        };
                        match pick {
                            Some(l) => right -= kind_chip(p, egui::pos2(right, cy), l, ink) + 8.0,
                            None => chip_dropped = true,
                        }
                    }
                    let (galley, cut) = elided(ui, &row.name, name_font, TEXT, right - x);
                    p.galley(egui::pos2(x, cy - galley.size().y / 2.0), galley, TEXT);
                    if tip.is_none() && (cut || chip_dropped) {
                        tip = Some(match row.chip {
                            Some(c) => format!("{} — {c}", row.name),
                            None => row.name.clone(),
                        });
                    }
                    let resp = match tip {
                        Some(t) => resp.on_hover_text(t),
                        None => resp,
                    };
                    context_menu(&resp, |ui| {
                        // Where the row sits among its siblings decides
                        // whether it can move or clip.
                        let doc = self.editor.doc();
                        let siblings = match row.parent {
                            None => Some(doc.layers()),
                            Some(g) => doc.layer(g).and_then(|l| l.children()),
                        };
                        let (pos, count) = siblings
                            .and_then(|l| l.iter().position(|x| x.id == row.id).map(|i| (i, l.len())))
                            .unwrap_or((0, 1));
                        let pixel = row.kind == Kind::Pixel && row.chip.is_none();
                        let smart = row.chip == Some("Smart");
                        let merge = lumenply_core::layer_ops::merge_down_kind(doc, row.id);
                        if menu_item(ui, "Rename", "") {
                            rename_start = Some((row.id, row.name.clone()));
                        }
                        let mut act = |ui: &mut egui::Ui, enabled: bool, label: &str, act: &'static str| {
                            let r = menu_item_response(ui, enabled, label, "");
                            if r.clicked() {
                                ctx_action = Some((act, row.id));
                            }
                            r
                        };
                        act(ui, true, "Duplicate layer", "dup");
                        match merge {
                            Ok(kind) => {
                                act(ui, true, kind.label(), "merge");
                            }
                            Err(why) => {
                                act(ui, false, "Merge down", "merge").on_disabled_hover_text(why);
                            }
                        }
                        menu_separator(ui);
                        act(ui, pos + 1 < count, "Move up", "up")
                            .on_disabled_hover_text("Already at the top");
                        act(ui, pos > 0, "Move down", "down").on_disabled_hover_text("Already at the bottom");
                        menu_separator(ui);
                        if row.clip {
                            act(ui, true, "Release clip", "unclip");
                        } else {
                            act(ui, pos > 0, "Clip to layer below", "clip")
                                .on_disabled_hover_text("Nothing below to clip to");
                        }
                        if row.masked {
                            act(ui, true, "Remove mask", "rmmask");
                            if row.mask_enabled {
                                act(ui, true, "Disable mask", "maskoff");
                            } else {
                                act(ui, true, "Enable mask", "maskon");
                            }
                        } else {
                            act(ui, true, "Add mask", "addmask");
                        }
                        menu_separator(ui);
                        if row.kind == Kind::Group {
                            act(ui, true, "Ungroup", "ungroup");
                        }
                        if smart || row.kind == Kind::Text {
                            act(ui, true, "Rasterize", "rasterize");
                        }
                        if pixel {
                            act(ui, true, "Convert to smart object", "smart")
                                .on_hover_text("Transforms re-render from the source: no quality loss");
                        }
                        act(ui, pixel || smart, "Flip horizontal", "fliph")
                            .on_disabled_hover_text("Flips apply to pixel layers and smart objects");
                        act(ui, pixel || smart, "Flip vertical", "flipv")
                            .on_disabled_hover_text("Flips apply to pixel layers and smart objects");
                        menu_separator(ui);
                        act(ui, true, "Delete layer", "delete");
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
        a11y_scroll(ui.ctx(), &scroll_out, "Layers");

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
                "dup" => self.duplicate_active(),
                "merge" => self.merge_down_active(),
                "ungroup" => self.ungroup_active(),
                "smart" => self.run(&ConvertToSmartObject { layer: id }),
                "rasterize" => self.run(&RasterizeLayer { layer: id }),
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
                "clip" | "unclip" => self.run(&SetClipped {
                    layer: id,
                    clip: act == "clip",
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
        // Up/down only where there is room among the layer's siblings, so
        // the buttons never record a do-nothing history step.
        let (can_up, can_down) = self
            .active
            .and_then(|id| {
                let doc = self.editor.doc();
                let list = match doc.parent_of(id) {
                    None => Some(doc.layers()),
                    Some(g) => doc.layer(g).and_then(|l| l.children()),
                }?;
                let i = list.iter().position(|l| l.id == id)?;
                Some((i + 1 < list.len(), i > 0))
            })
            .unwrap_or((false, false));
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.add(IconButton::new(icon_plus, "New layer")).clicked() {
                action = Some("add");
            }
            if ui
                .add_enabled(
                    self.active.is_some(),
                    IconButton::new(icon_folder, "Group selected layers (Ctrl+click to multi-select)"),
                )
                .clicked()
            {
                action = Some("group");
            }
            icon_menu(ui, "add-adj", icon_adjustment, "New adjustment layer", |ui| {
                for (name, adj) in adjustment_presets() {
                    if menu_item(ui, name, "") {
                        add_adj = Some(adj);
                    }
                }
            });
            icon_menu(ui, "add-filter", icon_filter, "New live filter layer", |ui| {
                for (name, f) in filter_presets() {
                    if menu_item(ui, name, "") {
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
            if ui
                .add_enabled(can_up, IconButton::new(icon_up, "Move layer up"))
                .clicked()
            {
                action = Some("up");
            }
            if ui
                .add_enabled(can_down, IconButton::new(icon_down, "Move layer down"))
                .clicked()
            {
                action = Some("down");
            }
            // The active layer's own actions, as on its right-click menu.
            icon_menu(ui, "layer-more", icon_more, "More layer actions", |ui| {
                self.layer_more_menu(ui)
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

/// The thumbnail tile of a layer without pixels: a dark tile carrying the
/// kind's line glyph, the same family as the footer and tool icons.
pub(crate) fn badge(p: &egui::Painter, rect: egui::Rect, kind: Kind) {
    p.rect_filled(rect, 2.0, RAISED);
    let glyph = egui::Rect::from_center_size(rect.center(), Vec2::splat(rect.height() * 0.55));
    match kind {
        Kind::Adjustment => icon_adjustment(p, glyph, ACCENT),
        Kind::Filter => icon_filter(p, glyph, LIVE_FILTER),
        Kind::Group => icon_folder(p, glyph, TEXT),
        Kind::Text => tools::draw_icon(p, glyph, Tool::Text, TEXT),
        Kind::Pixel => {}
    }
}

const CHIP_FONT: f32 = 10.5;

/// Width of a kind chip for `label`, as [`kind_chip`] paints it.
fn kind_chip_width(ui: &egui::Ui, label: &str) -> f32 {
    ui.fonts(|f| {
        f.layout_no_wrap(label.into(), FontId::proportional(CHIP_FONT), MUTED)
            .size()
            .x
    }) + 12.0
}

/// Paint a small kind chip whose right edge sits at `right`; returns its width.
fn kind_chip(p: &egui::Painter, right: egui::Pos2, label: &str, ink: Color32) -> f32 {
    let galley = p.layout_no_wrap(label.into(), FontId::proportional(CHIP_FONT), ink);
    let size = galley.size() + egui::vec2(12.0, 6.0);
    let rect = egui::Rect::from_min_size(right - egui::vec2(size.x, size.y / 2.0), size);
    p.rect_filled(rect, 4.0, GROUND);
    p.galley(rect.min + egui::vec2(6.0, 3.0), galley, ink);
    size.x
}

/// An [`IconButton`] that opens a menu of actions. The menu sizes to its
/// items (a popup sized to the 26 px button wrapped every label one
/// letter per line) and opens above the button when there is no room
/// below, as at the foot of the layers panel.
fn icon_menu(
    ui: &mut egui::Ui,
    id: &str,
    draw: fn(&egui::Painter, egui::Rect, Color32),
    tip: &'static str,
    content: impl FnOnce(&mut egui::Ui),
) {
    let resp = ui.add(IconButton::new(draw, tip));
    note_target(ui.ctx(), id, resp.rect);
    button_menu(&resp, content);
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
        if enabled && resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, RADIUS, CONTROL);
        } else if enabled && (resp.hovered() || resp.has_focus()) {
            ui.painter().rect_filled(rect, RADIUS, RAISED);
        }
        focus_ring(ui, &resp, rect, RADIUS);
        let ink = if enabled { TEXT } else { DISABLED_INK };
        (self.draw)(ui.painter(), rect.shrink(5.0), ink);
        // Icon-only: the tooltip text doubles as the accessible name.
        let tip = self.tip;
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
        resp.on_hover_text(tip).on_disabled_hover_text(tip)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_kind_chips_have_short_forms() {
        assert_eq!(short_chip("Hue/Saturation"), "Hue/Sat");
        assert_eq!(short_chip("Brightness/Contrast"), "Bri/Con");
        assert_eq!(short_chip("Live high pass"), "Live HP");
        assert_eq!(short_chip("Curves"), "Curves");
        assert_eq!(ROW_PITCH, 40.0);
    }
}
