//! Smart filters in the app (ADR 0011).
//!
//! * Filter menu: on a smart object every filter dialog adds a smart filter
//!   instead of baking (Photoshop's rule); Filter ▸ Convert for smart
//!   filters turns a pixel layer into a smart object first.
//! * Layers panel: a "Smart Filters" row under the layer (its eye switches
//!   the whole stack) and one row per filter, newest on top, each with an
//!   eye; clicking one opens it in Properties.
//! * Properties: the SMART FILTERS section lists the stack with eyes,
//!   up/down/delete, and the focused filter's settings, opacity and blend
//!   mode; the filter mask comes from the selection.
//!
//! Which filter is open in Properties is UI state kept in egui's memory.

use super::*;
use lumenply_core::smart_filter_cmds::accepts_smart_filters;
use lumenply_doc::SmartFilter;

/// Filter ▸ Convert for smart filters.
pub(crate) const SF_CONVERT: &str = "sf-convert";

const SUB_ROW_H: f32 = 22.0;

fn focus_key() -> egui::Id {
    egui::Id::new("smart-filter-focus")
}

/// The smart filter open in Properties: (layer, index in application order).
pub(crate) fn sf_focus(ctx: &egui::Context) -> Option<(LayerId, usize)> {
    ctx.data(|d| d.get_temp(focus_key()))
}

pub(crate) fn set_sf_focus(ctx: &egui::Context, v: Option<(LayerId, usize)>) {
    ctx.data_mut(|d| match v {
        Some(v) => d.insert_temp(focus_key(), v),
        None => d.remove::<(LayerId, usize)>(focus_key()),
    });
}

/// A preview document (built by applying a command outside the editor)
/// with its smart-filter caches brought up to date, or `None` when it has
/// no smart filters. Only the changed tiles' neighbourhood re-filters: the
/// editor's document still holds the old tiles, so changed ones always
/// have new addresses (ADR 0011).
pub(crate) fn refreshed_preview(doc: &Document) -> Option<Document> {
    let mut any = false;
    doc.for_each_layer(|l| any |= l.smart_filters.is_active());
    if !any {
        return None;
    }
    let mut d = doc.clone();
    lumenply_render::smart_filters::refresh_stale(&mut d);
    Some(d)
}

fn blend_title(m: BlendMode) -> String {
    let n = m.name().replace('-', " ");
    let mut c = n.chars();
    c.next().map_or(String::new(), |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// One filter's settings as sliders (the ranges of the filter dialogs and
/// live filter layers). Returns true when a drag or edit finished.
pub(crate) fn smart_filter_params(ui: &mut egui::Ui, f: &mut Filter) -> bool {
    let mut finished = false;
    let int = RowOpts {
        int: true,
        ..RowOpts::default()
    };
    match f {
        Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
            finished |= slider_row_log(ui, "Radius", radius, 0.5..=60.0, " px");
        }
        Filter::Sharpen { amount, radius } => {
            finished |= slider_row_scaled(ui, "Amount", amount, 0.0..=5.0, 100.0, "%");
            finished |= slider_row(ui, "Radius", radius, 0.5..=20.0, " px");
        }
        Filter::Noise { amount } => {
            finished |= slider_row_scaled(ui, "Amount", amount, 0.0..=1.0, 100.0, "%");
        }
        Filter::MotionBlur { angle, distance } => {
            finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
            finished |= slider_row(ui, "Distance", distance, 1.0..=200.0, " px");
        }
        Filter::Median { radius } => {
            finished |= slider_row_ex(ui, "Radius", radius, 1.0..=8.0, " px", int);
        }
        Filter::HighPass { radius } => {
            finished |= slider_row(ui, "Radius", radius, 0.5..=60.0, " px");
        }
        Filter::Mosaic { size } => {
            let o = RowOpts { log: true, ..int };
            finished |= slider_row_ex(ui, "Cell size", size, 2.0..=200.0, " px", o);
        }
        Filter::Emboss {
            angle,
            height,
            amount,
        } => {
            finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
            finished |= slider_row(ui, "Height", height, 1.0..=10.0, " px");
            finished |= slider_row_scaled(ui, "Amount", amount, 0.0..=5.0, 100.0, "%");
        }
        Filter::FindEdges => {
            ui.label(RichText::new("No settings.").small().color(MUTED));
        }
        Filter::SurfaceBlur { radius, threshold } => {
            let o = RowOpts { log: true, ..int };
            finished |= slider_row_ex(ui, "Radius", radius, 1.0..=100.0, " px", o);
            finished |= slider_row_ex(ui, "Threshold", threshold, 2.0..=255.0, " levels", int);
        }
        Filter::LensBlur { radius, highlights } => {
            finished |= slider_row_log(ui, "Radius", radius, 1.0..=100.0, " px");
            finished |= slider_row_scaled(ui, "Highlights", highlights, 0.0..=1.0, 100.0, "%");
        }
        Filter::DustScratches { radius, threshold } => {
            finished |= slider_row_ex(ui, "Radius", radius, 1.0..=8.0, " px", int);
            finished |= slider_row_ex(ui, "Threshold", threshold, 0.0..=255.0, " levels", int);
        }
        Filter::Develop { settings, .. } => crate::camera_raw_filter::develop_params(ui, settings),
    }
    finished
}

/// What a click in the smart-filter UI asks for, run after drawing.
enum SfAct {
    Toggle(usize),
    Master(bool),
    Focus(Option<usize>),
    Up(usize),
    Down(usize),
    Delete(usize),
    Set(usize, SmartFilter, bool),
    MaskFromSelection,
    DeleteMask,
}

impl App {
    /// Do Filter-menu filters go onto the active layer as smart filters?
    /// As in Photoshop: when it is a smart object.
    pub(crate) fn filters_go_smart(&self) -> bool {
        self.active_layer().is_some_and(|l| l.smart_layer().is_some())
    }

    /// Why one of this module's actions can't run (see
    /// [`App::action_block`]); `None` for ids it doesn't decide.
    pub(crate) fn smart_filter_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        match id {
            SF_CONVERT => Some(match self.active_layer() {
                None => Some("Select a layer first"),
                Some(l) if l.smart_layer().is_some() => {
                    Some("Already a smart object: filters apply as smart filters")
                }
                Some(_) if !self.active_is_pixel() => Some("Select a pixel layer first"),
                Some(_) => self.lock_block(crate::layer_actions::LockNeed::Paint),
            }),
            // Filters on a smart object become smart filters.
            id if id.starts_with("filter-") && self.filters_go_smart() => Some(None),
            _ => None,
        }
    }

    /// Run one of this module's actions; false for ids it doesn't own.
    pub(crate) fn run_smart_filter_action(&mut self, id: &str) -> bool {
        if id != SF_CONVERT {
            return false;
        }
        if let Some(layer) = self.active {
            let before = self.editor.history().len();
            self.run(&ConvertToSmartObject { layer });
            if self.editor.history().len() > before {
                self.status = "Converted to a smart object: filters now apply as smart filters".into();
            }
        }
        true
    }

    /// The document the filter dialog previews: the filter baked into a
    /// pixel layer, or added as a smart filter on a smart object.
    pub(crate) fn filter_dialog_preview(&self, layer: LayerId, f: &Filter) -> Option<Document> {
        let mut doc = self.editor.doc().clone();
        if self.filters_go_smart() {
            AddSmartFilter::new(layer, f.clone()).apply(&mut doc).ok()?;
            lumenply_render::smart_filters::refresh_stale(&mut doc);
        } else {
            ApplyFilter {
                layer,
                filter: f.clone(),
            }
            .apply(&mut doc)
            .ok()?;
        }
        Some(doc)
    }

    /// The filter dialog's Apply: bake, or add a smart filter (and open it
    /// in Properties).
    pub(crate) fn apply_filter_dialog(&mut self, ctx: &egui::Context, layer: LayerId, f: Filter) {
        if self.filters_go_smart() {
            self.run(&AddSmartFilter::new(layer, f));
            let n = self
                .editor
                .doc()
                .layer(layer)
                .map_or(0, |l| l.smart_filters.filters.len());
            if n > 0 {
                set_sf_focus(ctx, Some((layer, n - 1)));
            }
        } else {
            self.run(&ApplyFilter { layer, filter: f });
        }
    }

    fn run_sf_act(&mut self, ctx: &egui::Context, id: LayerId, act: SfAct) {
        let filters = match self.editor.doc().layer(id) {
            Some(l) => l.smart_filters.filters.clone(),
            None => return,
        };
        match act {
            SfAct::Toggle(i) => {
                if let Some(f) = filters.get(i) {
                    let mut f = f.clone();
                    f.enabled = !f.enabled;
                    self.run(&SetSmartFilter {
                        layer: id,
                        index: i,
                        filter: f,
                    });
                }
            }
            SfAct::Master(on) => self.run(&SetSmartFiltersEnabled {
                layer: id,
                enabled: on,
            }),
            SfAct::Focus(i) => {
                self.set_active(Some(id));
                set_sf_focus(ctx, i.map(|i| (id, i)));
            }
            SfAct::Up(i) => {
                self.run(&ReorderSmartFilter {
                    layer: id,
                    from: i,
                    to: i + 1,
                });
                set_sf_focus(ctx, Some((id, i + 1)));
            }
            SfAct::Down(i) => {
                self.run(&ReorderSmartFilter {
                    layer: id,
                    from: i,
                    to: i - 1,
                });
                set_sf_focus(ctx, Some((id, i - 1)));
            }
            SfAct::Delete(i) => {
                self.run(&RemoveSmartFilter { layer: id, index: i });
                set_sf_focus(ctx, None);
            }
            SfAct::Set(i, f, drag) => {
                let cmd = SetSmartFilter {
                    layer: id,
                    index: i,
                    filter: f,
                };
                if drag {
                    self.run_coalescing(&cmd, &format!("smart-filter-{id}-{i}"));
                } else {
                    self.run(&cmd);
                }
            }
            SfAct::MaskFromSelection => self.run(&SetSmartFilterMask {
                layer: id,
                mask: None,
                from_selection: true,
            }),
            SfAct::DeleteMask => self.run(&SetSmartFilterMask {
                layer: id,
                mask: None,
                from_selection: false,
            }),
        }
    }

    /// Properties ▸ SMART FILTERS for layer `id` (nothing for layers that
    /// can't take them; a hint on a smart object without any).
    pub(crate) fn smart_filters_properties(&mut self, ui: &mut egui::Ui, id: LayerId) {
        let ctx = ui.ctx().clone();
        let Some(layer) = self.editor.doc().layer(id) else {
            return;
        };
        if !accepts_smart_filters(layer) {
            return;
        }
        let sf = layer.smart_filters.clone();
        let is_smart = layer.smart_layer().is_some();
        if sf.filters.is_empty() {
            if is_smart {
                section_title(ui, "SMART FILTERS");
                ui.label(
                    RichText::new("Filters from the Filter menu are added here and stay editable.")
                        .small()
                        .color(MUTED),
                );
            }
            return;
        }
        let n = sf.filters.len();
        let focus = sf_focus(&ctx).filter(|&(l, i)| l == id && i < n).map(|(_, i)| i);
        let has_sel = self.editor.doc().selection.is_some();
        let mut act: Option<SfAct> = None;
        let mut finished = false;

        ui.horizontal(|ui| {
            let mut on = sf.enabled;
            let r = check(ui, &mut on, "");
            a11y_name(&r, "Show smart filters");
            let r = r.on_hover_text(if sf.enabled {
                "Hide every smart filter"
            } else {
                "Show the smart filters"
            });
            if r.changed() {
                act = Some(SfAct::Master(on));
            }
            section_title(ui, "SMART FILTERS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if sf.mask.is_some() {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Delete mask").small().color(MUTED)).frame(false),
                        )
                        .on_hover_text("Remove the filter mask: the filters cover the whole layer again")
                        .clicked()
                    {
                        act = Some(SfAct::DeleteMask);
                    }
                } else if ui
                    .add_enabled(
                        has_sel,
                        egui::Button::new(RichText::new("Mask").small().color(MUTED)).frame(false),
                    )
                    .on_hover_text("Limit the filters to the selection (a filter mask)")
                    .on_disabled_hover_text("Make a selection to mask the filters")
                    .clicked()
                {
                    act = Some(SfAct::MaskFromSelection);
                }
            });
        });
        // Photoshop's order: the filter applied last on top.
        for i in (0..n).rev() {
            let f = &sf.filters[i];
            let open = focus == Some(i);
            ui.horizontal(|ui| {
                let mut on = f.enabled;
                let r = check(ui, &mut on, "");
                a11y_name(&r, &format!("Show {}", f.filter.name()));
                if r.changed() {
                    act = Some(SfAct::Toggle(i));
                }
                let ink = if f.enabled && sf.enabled { TEXT } else { MUTED };
                let label = if f.opacity < 0.999 || f.blend != BlendMode::Normal {
                    format!(
                        "{}  ·  {:.0}% {}",
                        f.filter.name(),
                        f.opacity * 100.0,
                        blend_title(f.blend)
                    )
                } else {
                    f.filter.name().to_string()
                };
                let r = ui
                    .add(egui::SelectableLabel::new(open, RichText::new(label).color(ink)))
                    .on_hover_text(if open {
                        "Close its settings"
                    } else {
                        "Edit its settings"
                    });
                if r.clicked() {
                    act = Some(SfAct::Focus((!open).then_some(i)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let small = |t: &str| egui::Button::new(RichText::new(t).color(MUTED)).frame(false);
                    let r = ui.add(small("×")).on_hover_text("Delete this smart filter");
                    a11y_name(&r, &format!("Delete {}", f.filter.name()));
                    if r.clicked() {
                        act = Some(SfAct::Delete(i));
                    }
                    let r = ui
                        .add_enabled(i > 0, small("↓"))
                        .on_hover_text("Apply it earlier")
                        .on_disabled_hover_text("Already applied first");
                    a11y_name(&r, &format!("Move {} down", f.filter.name()));
                    if r.clicked() {
                        act = Some(SfAct::Down(i));
                    }
                    let r = ui
                        .add_enabled(i + 1 < n, small("↑"))
                        .on_hover_text("Apply it later")
                        .on_disabled_hover_text("Already applied last");
                    a11y_name(&r, &format!("Move {} up", f.filter.name()));
                    if r.clicked() {
                        act = Some(SfAct::Up(i));
                    }
                });
            });
            if open {
                let before = f.clone();
                let mut edit = f.clone();
                ui.indent(("smart-filter-body", id, i), |ui| {
                    finished |= smart_filter_params(ui, &mut edit.filter);
                    let mut op = edit.opacity * 100.0;
                    finished |= slider_row(ui, "Opacity", &mut op, 0.0..=100.0, "%");
                    edit.opacity = op / 100.0;
                    ui.horizontal(|ui| {
                        row_label(ui, "Mode", LABEL_W);
                        ui.spacing_mut().combo_width = ui.available_width();
                        let mut mode = edit.blend;
                        let r = egui::ComboBox::from_id_salt(("smart-filter-blend", id, i))
                            .selected_text(blend_title(mode))
                            .show_ui(ui, |ui| {
                                popup_style(ui);
                                for m in BlendMode::ALL {
                                    ui.selectable_value(&mut mode, m, blend_title(m));
                                }
                            });
                        a11y_name(&r.response, "Smart filter blend mode");
                        edit.blend = mode;
                    });
                });
                if edit != before {
                    // Sliders coalesce into one step per drag; the mode
                    // menu is a single click.
                    let drag = edit.blend == before.blend;
                    act = Some(SfAct::Set(i, edit, drag));
                }
            }
        }
        if sf.mask.is_some() {
            ui.label(
                RichText::new("A filter mask limits the filters to the selection it came from.")
                    .small()
                    .color(MUTED),
            );
        }
        if let Some(a) = act {
            self.run_sf_act(&ctx, id, a);
        }
        if finished {
            self.editor.end_coalescing();
        }
        // "Edit in Camera Raw…" on the open (Camera Raw) filter.
        if crate::camera_raw_filter::take_request(&ctx) {
            if let Some(i) = focus {
                self.open_camera_raw_filter_on_smart(id, i);
            }
        }
    }

    /// The Layers panel's rows under layer `id`: "Smart Filters" (eye =
    /// the whole stack, plus the mask) and one row per filter, newest on
    /// top. `depth` is the layer's indent.
    pub(crate) fn smart_filter_rows(&mut self, ui: &mut egui::Ui, id: LayerId, depth: usize) {
        let ctx = ui.ctx().clone();
        let Some(layer) = self.editor.doc().layer(id) else {
            return;
        };
        if layer.smart_filters.is_empty() {
            return;
        }
        let sf = layer.smart_filters.clone();
        let layer_name = layer.name.clone();
        let focus = sf_focus(&ctx).filter(|&(l, _)| l == id).map(|(_, i)| i);
        let mut act: Option<SfAct> = None;
        let mut reopen: Option<usize> = None;
        let w = ui.available_width();
        let eye_x = |rect: egui::Rect| rect.min.x + 6.0 + depth as f32 * 16.0 + 8.0;
        let text_x = |rect: egui::Rect| rect.min.x + 6.0 + depth as f32 * 16.0 + 30.0;

        // Header row.
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, SUB_ROW_H), Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                true,
                sf.enabled,
                format!("Smart filters of {layer_name}"),
            )
        });
        let p = ui.painter();
        let cy = rect.center().y;
        let eye = egui::Rect::from_center_size(egui::pos2(eye_x(rect), cy), Vec2::splat(14.0));
        let hover = resp.hover_pos();
        if hover.is_some_and(|q| eye.expand(3.0).contains(q)) {
            p.rect_filled(eye.expand(3.0), 4.0, HOVER);
        } else if resp.hovered() {
            p.rect_filled(rect, RADIUS, RAISED);
        }
        crate::layers::paint_eye(p, eye, sf.enabled);
        let mut x = text_x(rect);
        if sf.mask.is_some() {
            let m = egui::Rect::from_min_size(egui::pos2(x, cy - 7.0), egui::vec2(20.0, 14.0));
            p.rect_filled(m, 2.0, Color32::from_gray(235));
            p.rect_filled(
                egui::Rect::from_min_max(m.min, egui::pos2(m.center().x, m.max.y)),
                2.0,
                Color32::from_gray(20),
            );
            p.rect_stroke(m, 2.0, Stroke::new(1.0, LINE));
            x += 26.0;
        }
        p.text(
            egui::pos2(x, cy),
            Align2::LEFT_CENTER,
            "Smart Filters",
            FontId::proportional(12.5),
            if sf.enabled { MUTED } else { DISABLED_SUB },
        );
        let on_eye = resp
            .interact_pointer_pos()
            .is_some_and(|q| eye.expand(3.0).contains(q));
        let resp = resp.on_hover_text(if hover.is_some_and(|q| eye.expand(3.0).contains(q)) {
            if sf.enabled {
                "Hide the smart filters"
            } else {
                "Show the smart filters"
            }
        } else {
            "Smart filters: non-destructive, on this layer only"
        });
        if resp.clicked() {
            act = Some(if on_eye {
                SfAct::Master(!sf.enabled)
            } else {
                SfAct::Focus(None)
            });
        }

        for i in (0..sf.filters.len()).rev() {
            let f = &sf.filters[i];
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, SUB_ROW_H), Sense::click());
            let name = f.filter.name();
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    focus == Some(i),
                    format!("Smart filter {name}"),
                )
            });
            let p = ui.painter();
            let cy = rect.center().y;
            let eye = egui::Rect::from_center_size(egui::pos2(eye_x(rect), cy), Vec2::splat(14.0));
            let hover = resp.hover_pos();
            let on_eye_hover = hover.is_some_and(|q| eye.expand(3.0).contains(q));
            if focus == Some(i) {
                p.rect_filled(rect, RADIUS, RAISED);
            } else if resp.hovered() && !on_eye_hover {
                p.rect_filled(rect, RADIUS, HOVER);
            }
            if on_eye_hover {
                p.rect_filled(eye.expand(3.0), 4.0, HOVER);
            }
            crate::layers::paint_eye(p, eye, f.enabled);
            let live = f.enabled && sf.enabled;
            let x = text_x(rect) + 14.0;
            p.text(
                egui::pos2(x, cy),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(13.0),
                if live { LIVE_FILTER } else { DISABLED_SUB },
            );
            if f.opacity < 0.999 || f.blend != BlendMode::Normal {
                let tag = if f.blend == BlendMode::Normal {
                    format!("{:.0}%", f.opacity * 100.0)
                } else {
                    format!("{:.0}% {}", f.opacity * 100.0, blend_title(f.blend))
                };
                p.text(
                    egui::pos2(rect.max.x - 8.0, cy),
                    Align2::RIGHT_CENTER,
                    tag,
                    FontId::monospace(10.5),
                    MUTED,
                );
            }
            let on_eye = resp
                .interact_pointer_pos()
                .is_some_and(|q| eye.expand(3.0).contains(q));
            let resp = resp.on_hover_text(if on_eye_hover {
                if f.enabled {
                    format!("Hide {name}")
                } else {
                    format!("Show {name}")
                }
            } else {
                format!("{name} — click to edit it in Properties")
            });
            if resp.clicked() || resp.double_clicked() {
                act = Some(if on_eye {
                    SfAct::Toggle(i)
                } else {
                    SfAct::Focus(Some(i))
                });
            }
            // Double-clicking a Camera Raw filter re-opens its workspace.
            if resp.double_clicked() && !on_eye && matches!(f.filter, Filter::Develop { .. }) {
                reopen = Some(i);
            }
        }
        if let Some(a) = act {
            self.run_sf_act(&ctx, id, a);
        }
        if let Some(i) = reopen {
            self.open_camera_raw_filter_on_smart(id, i);
        }
    }

    /// `--screenshot-do` tokens (`sf:...`): `sf:add=<filter action id>`
    /// adds that Filter-menu preset as a smart filter on the active layer,
    /// `sf:focus=<index>` opens one in Properties, `sf:dialog=<filter
    /// action id>` opens the filter dialog.
    pub(crate) fn debug_smart_filters(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("sf:") else {
            return false;
        };
        let (key, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let preset = || {
            filter_presets()
                .into_iter()
                .find(|(_, f)| palette::filter_id(f) == arg)
                .map(|(_, f)| f)
        };
        match (key, self.active) {
            ("add", Some(layer)) => {
                if let Some(f) = preset() {
                    self.run(&AddSmartFilter::new(layer, f));
                }
            }
            ("focus", Some(layer)) => {
                set_sf_focus(ctx, arg.parse().ok().map(|i| (layer, i)));
            }
            ("dialog", _) => {
                if let Some(f) = preset() {
                    self.dialog = Some(Dialog::Filter(f));
                }
            }
            _ => {}
        }
        true
    }
}

/// Ink for switched-off smart filter rows.
const DISABLED_SUB: Color32 = Color32::from_rgb(0x6E, 0x75, 0x7E);

#[cfg(test)]
mod tests {
    use super::*;

    use lumenply_tiles::{Rgba, TileStore};

    #[test]
    fn blend_titles_read_like_menu_items() {
        assert_eq!(blend_title(BlendMode::HardLight), "Hard light");
        assert_eq!(blend_title(BlendMode::Normal), "Normal");
    }

    /// A 64×32 document, white left of x = 32 and black from there, in an
    /// app that never autosaves; returns the app and the layer's id.
    fn step_app() -> (App, LayerId) {
        let mut doc = Document::new(64, 32);
        let id = doc.add_pixel_layer("Step");
        let mut r = Raster::new(64, 32);
        for y in 0..32 {
            for x in 0..64 {
                let v = if x < 32 { 1.0 } else { 0.0 };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        (app, id)
    }

    fn shown(app: &App, id: LayerId, x: i32) -> f32 {
        app.editor
            .doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(x, 16)
            .r
    }

    #[test]
    fn filters_bake_on_pixels_and_go_smart_on_smart_objects() {
        let (mut app, id) = step_app();
        let ctx = crate::a11y_tests::ctx();
        let blur = Filter::BoxBlur { radius: 2.0 };
        // A plain pixel layer: the dialog's Apply bakes the filter.
        assert!(!app.filters_go_smart());
        assert_eq!(app.action_block(SF_CONVERT), None);
        app.apply_filter_dialog(&ctx, id, blur.clone());
        let l = app.editor.doc().layer(id).unwrap();
        assert!(l.smart_filters.is_empty());
        assert!((l.pixels().unwrap().get_pixel(32, 16).r - 0.4).abs() < 1e-4);
        app.undo();

        // Convert for smart filters, then the same Apply adds a smart filter.
        app.run_menu_action(SF_CONVERT);
        assert!(app.filters_go_smart());
        assert_eq!(
            app.action_block(SF_CONVERT),
            Some("Already a smart object: filters apply as smart filters")
        );
        assert_eq!(app.action_block("filter-gauss"), None);
        let preview = app.filter_dialog_preview(id, &blur).unwrap();
        let p = preview
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(32, 16);
        assert!((p.r - 0.4).abs() < 1e-4, "the preview shows the smart filter");
        assert_eq!(shown(&app, id, 32), 0.0, "previewing changes nothing");
        app.apply_filter_dialog(&ctx, id, blur);
        assert!((shown(&app, id, 32) - 0.4).abs() < 1e-4);
        assert_eq!(
            sf_focus(&ctx),
            Some((id, 0)),
            "the new filter opens in Properties"
        );
        let source = app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .smart_layer()
            .unwrap()
            .source
            .get_pixel(32, 16);
        assert_eq!(source.r, 0.0, "the smart object's pixels stay untouched");
    }

    #[test]
    fn canvas_previews_re_filter_the_previewed_pixels() {
        let (mut app, id) = step_app();
        app.run_menu_action(SF_CONVERT);
        app.run(&AddSmartFilter::new(id, Filter::BoxBlur { radius: 2.0 }));
        // A move preview (as the Move tool builds it, outside the editor).
        let mut preview = app.editor.doc().clone();
        MoveLayer {
            layer: id,
            dx: 10,
            dy: 0,
        }
        .apply(&mut preview)
        .unwrap();
        let fresh = refreshed_preview(&preview).expect("the document has smart filters");
        let at = |d: &Document, x| d.layer(id).unwrap().raster_store().unwrap().get_pixel(x, 16).r;
        // The ramp follows the edge from x = 32 to x = 42.
        assert!((at(&fresh, 42) - 0.4).abs() < 1e-4, "{}", at(&fresh, 42));
        assert!((at(&fresh, 32) - 1.0).abs() < 1e-4);
        // Without the refresh the stale ramp would still sit at x = 32.
        assert!((at(&preview, 32) - 0.4).abs() < 1e-4);
        // A document without smart filters needs no copy.
        app.run(&RemoveSmartFilter { layer: id, index: 0 });
        assert!(refreshed_preview(app.editor.doc()).is_none());
    }

    #[test]
    fn row_and_section_clicks_are_single_undo_steps() {
        let (mut app, id) = step_app();
        let ctx = crate::a11y_tests::ctx();
        app.run_menu_action(SF_CONVERT);
        app.run(&AddSmartFilter::new(id, Filter::BoxBlur { radius: 2.0 }));
        app.run(&AddSmartFilter::new(id, Filter::Mosaic { size: 4.0 }));
        let steps = app.editor.history().len();
        // Blur then mosaic: the cell 32..36 averages 0.4, 0.2, 0, 0.
        assert!((shown(&app, id, 32) - 0.15).abs() < 1e-4);
        app.run_sf_act(&ctx, id, SfAct::Toggle(1));
        assert!((shown(&app, id, 32) - 0.4).abs() < 1e-4, "mosaic hidden");
        app.run_sf_act(&ctx, id, SfAct::Toggle(1));
        app.run_sf_act(&ctx, id, SfAct::Down(1));
        assert_eq!(
            sf_focus(&ctx),
            Some((id, 0)),
            "the focus follows the moved filter"
        );
        // Mosaic first keeps the step; the blur ramp follows.
        assert!((shown(&app, id, 32) - 0.4).abs() < 1e-4);
        app.run_sf_act(&ctx, id, SfAct::Master(false));
        assert_eq!(shown(&app, id, 32), 0.0);
        app.run_sf_act(&ctx, id, SfAct::Delete(0));
        assert_eq!(app.editor.history().len(), steps + 5);
        for _ in 0..5 {
            app.undo();
        }
        assert!((shown(&app, id, 32) - 0.15).abs() < 1e-4);
    }

    #[test]
    fn smart_filter_rows_and_properties_name_every_control() {
        let (mut app, id) = step_app();
        let ctx = crate::a11y_tests::ctx();
        app.run_menu_action(SF_CONVERT);
        let hint_only = crate::a11y_tests::nameless(&mut app, &ctx);
        assert_eq!(hint_only, Vec::<String>::new());
        app.run(&AddSmartFilter::new(id, Filter::GaussianBlur { radius: 3.0 }));
        app.run(&AddSmartFilter::new(id, Filter::FindEdges));
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 32, 32))),
        });
        app.run(&SetSmartFilterMask {
            layer: id,
            mask: None,
            from_selection: true,
        });
        set_sf_focus(&ctx, Some((id, 0)));
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
    }
}
