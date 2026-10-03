//! Filter ▸ Camera Raw Filter (Shift+Cmd+A): the Camera Raw develop
//! workspace (camera_raw.rs) run on the active layer instead of a RAW file,
//! as in Photoshop.
//!
//! * A pixel layer: OK bakes the develop into its pixels (one undo step),
//!   or adds a live Camera Raw filter layer above it.
//! * A smart object: OK adds a Camera Raw smart filter (or the live layer).
//!   Double-clicking the smart filter's row in the Layers panel, or "Edit in
//!   Camera Raw…" in Properties, re-opens the workspace with its settings;
//!   OK updates it as one undo step.
//! * A live Camera Raw filter layer: the action re-opens it.
//!
//! The preview develops the layer's pixels (for a live layer, everything
//! below it) at preview size, with the local controls measured on the
//! shrunken canvas so they look as they will at full size.

use super::*;
use crate::camera_raw::{shrink, CameraRawState, PREVIEW_MAX};
use lumenply_doc::SmartFilter;
use lumenply_render::develop::Develop;

/// The palette / menu / key action.
pub(crate) const CRF_ACTION: &str = "camera-raw-filter";

/// How OK applies a new Camera Raw Filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CrfApply {
    /// Baked into the pixel layer (destructive).
    Pixels,
    /// A smart filter on the smart object.
    SmartFilter,
    /// A live filter layer above the active layer (filters everything
    /// below it).
    LiveLayer,
}

impl CrfApply {
    fn title(self) -> &'static str {
        match self {
            CrfApply::Pixels => "Apply to pixels",
            CrfApply::SmartFilter => "Smart filter",
            CrfApply::LiveLayer => "Live filter layer",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            CrfApply::Pixels => "Bake the develop into the layer's pixels",
            CrfApply::SmartFilter => "Add it as a smart filter you can re-open and edit later",
            CrfApply::LiveLayer => "Add a live Camera Raw layer above: it develops everything below it",
        }
    }
}

/// What the workspace edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CrfEdit {
    /// A new filter, applied as [`CrfTarget::apply`] says.
    New,
    /// The smart filter at this index (application order) of the layer.
    SmartFilter(usize),
    /// The layer is a live Camera Raw filter layer.
    LiveLayer,
}

/// The Camera Raw Filter's target, kept in the workspace state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CrfTarget {
    pub layer: LayerId,
    /// Canvas rectangle the local controls and the vignette measure on.
    pub frame: [i32; 4],
    pub edit: CrfEdit,
    pub apply: CrfApply,
    /// The ways a new filter can be applied to this layer.
    pub choices: Vec<CrfApply>,
    /// The `apply` the preview's pixels were taken for.
    pub source_for: CrfApply,
}

/// Properties' "Edit in Camera Raw…" asks for the workspace through
/// egui's memory, since the parameter rows don't know their layer.
fn request_key() -> egui::Id {
    egui::Id::new("camera-raw-filter-request")
}

/// The settings of a Camera Raw filter as a short summary plus an "Edit in
/// Camera Raw…" button; a click asks the app (through egui's memory) to
/// open the workspace on the filter shown in Properties.
pub(crate) fn develop_params(ui: &mut egui::Ui, d: &Develop) {
    let mut parts: Vec<String> = Vec::new();
    for (name, v, unit) in [
        ("Temp", d.temperature, ""),
        ("Tint", d.tint, ""),
        ("Exposure", d.exposure, " EV"),
        ("Contrast", d.contrast, ""),
        ("Highlights", d.highlights, ""),
        ("Shadows", d.shadows, ""),
        ("Whites", d.whites, ""),
        ("Blacks", d.blacks, ""),
        ("Texture", d.texture, ""),
        ("Clarity", d.clarity, ""),
        ("Dehaze", d.dehaze, ""),
        ("Vibrance", d.vibrance, ""),
        ("Saturation", d.saturation, ""),
        ("Vignette", d.vignette, ""),
    ] {
        if v != 0.0 {
            if unit.is_empty() {
                parts.push(format!("{name} {v:+.0}"));
            } else {
                parts.push(format!("{name} {v:+.2}{unit}"));
            }
        }
    }
    if d.tone_curve {
        parts.push("tone curve".into());
    }
    let text = if parts.is_empty() {
        "No changes yet.".to_string()
    } else {
        parts.join(" · ")
    };
    ui.label(RichText::new(text).small().color(MUTED));
    if ui
        .button("Edit in Camera Raw…")
        .on_hover_text("Open the Camera Raw workspace with these settings")
        .clicked()
    {
        ui.ctx().data_mut(|m| m.insert_temp(request_key(), true));
    }
}

/// Takes a pending "Edit in Camera Raw…" click.
pub(crate) fn take_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|m| m.remove_temp::<bool>(request_key()).unwrap_or(false))
}

/// The workspace footer's "Apply as" choice (or, when re-editing, what OK
/// updates).
pub(crate) fn apply_as_row(ui: &mut egui::Ui, t: &mut CrfTarget) {
    ui.add_space(4.0);
    match t.edit {
        CrfEdit::SmartFilter(_) => {
            ui.label(RichText::new("OK updates the smart filter").small().color(MUTED));
        }
        CrfEdit::LiveLayer => {
            ui.label(
                RichText::new("OK updates the live filter layer")
                    .small()
                    .color(MUTED),
            );
        }
        CrfEdit::New => {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().combo_width = 150.0;
                    let mut pick = t.apply;
                    let r = egui::ComboBox::from_id_salt("camera-raw-filter-apply")
                        .selected_text(pick.title())
                        .show_ui(ui, |ui| {
                            popup_style(ui);
                            for c in &t.choices {
                                ui.selectable_value(&mut pick, *c, c.title())
                                    .on_hover_text(c.hint());
                            }
                        });
                    a11y_name(&r.response, "Apply the Camera Raw Filter as");
                    r.response.on_hover_text(pick.hint());
                    t.apply = pick;
                    ui.label(RichText::new("Apply as").color(MUTED));
                });
            });
        }
    }
}

impl App {
    /// Why Filter ▸ Camera Raw Filter can't run (see [`App::action_block`]);
    /// `None` for other ids.
    pub(crate) fn crf_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        if id != CRF_ACTION {
            return None;
        }
        let Some(l) = self.active_layer() else {
            return Some(Some("Select a layer first"));
        };
        let ok = l.pixels().is_some()
            || l.smart_layer().is_some()
            || matches!(l.content, LayerContent::Filter(Filter::Develop { .. }));
        Some((!ok).then_some("Select a pixel layer or smart object first"))
    }

    /// Runs [`CRF_ACTION`]; false for other ids.
    pub(crate) fn run_crf_action(&mut self, id: &str) -> bool {
        if id != CRF_ACTION {
            return false;
        }
        self.open_camera_raw_filter();
        true
    }

    /// Filter ▸ Camera Raw Filter on the active layer: a new filter on a
    /// pixel layer or smart object, or the live Camera Raw layer re-opened.
    pub(crate) fn open_camera_raw_filter(&mut self) {
        let Some(id) = self.active else {
            self.status = "Select a layer first".into();
            return;
        };
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let Some(l) = doc.layer(id) else {
            return;
        };
        let canvas_frame = [canvas.x, canvas.y, canvas.w as i32, canvas.h as i32];
        let (edit, dev, frame, choices) = match &l.content {
            LayerContent::Filter(Filter::Develop { settings, frame }) => {
                (CrfEdit::LiveLayer, *settings, *frame, vec![CrfApply::LiveLayer])
            }
            _ if l.smart_layer().is_some() => (
                CrfEdit::New,
                Develop::NEUTRAL,
                canvas_frame,
                vec![CrfApply::SmartFilter, CrfApply::LiveLayer],
            ),
            _ if l.pixels().is_some() => (
                CrfEdit::New,
                Develop::NEUTRAL,
                canvas_frame,
                vec![CrfApply::Pixels, CrfApply::LiveLayer],
            ),
            _ => {
                self.status = "Camera Raw Filter works on a pixel layer or smart object".into();
                return;
            }
        };
        self.open_crf(id, edit, dev, frame, choices);
    }

    /// Re-opens smart filter `index` (a Camera Raw filter) of `layer`.
    pub(crate) fn open_camera_raw_filter_on_smart(&mut self, layer: LayerId, index: usize) {
        let Some(f) = self
            .editor
            .doc()
            .layer(layer)
            .and_then(|l| l.smart_filters.filters.get(index))
        else {
            return;
        };
        let Filter::Develop { settings, frame } = &f.filter else {
            return;
        };
        let (dev, frame) = (*settings, *frame);
        self.set_active(Some(layer));
        self.open_crf(
            layer,
            CrfEdit::SmartFilter(index),
            dev,
            frame,
            vec![CrfApply::SmartFilter],
        );
    }

    fn open_crf(
        &mut self,
        layer: LayerId,
        edit: CrfEdit,
        dev: Develop,
        frame: [i32; 4],
        choices: Vec<CrfApply>,
    ) {
        let apply = choices[0];
        let target = CrfTarget {
            layer,
            frame,
            edit,
            apply,
            choices,
            source_for: apply,
        };
        let Some(proxy) = self.crf_source(&target) else {
            self.status = "Nothing to develop on this layer".into();
            return;
        };
        let name = self
            .editor
            .doc()
            .layer(layer)
            .map_or(String::new(), |l| l.name.clone());
        self.camera_raw = Some(Box::new(CameraRawState::for_filter(&name, proxy, dev, target)));
    }

    /// The pixels the filter will develop, over its frame, at preview size:
    /// the layer's own (for a smart filter, as the filters before it leave
    /// them), or for a live layer the composite below it.
    fn crf_source(&self, t: &CrfTarget) -> Option<Raster> {
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let frame = Rect::new(
            t.frame[0],
            t.frame[1],
            t.frame[2].max(1) as u32,
            t.frame[3].max(1) as u32,
        );
        let l = doc.layer(t.layer)?;
        let store = match (t.edit, t.apply) {
            (CrfEdit::SmartFilter(i), _) => {
                // The stack up to (not including) this filter.
                let mut d = doc.clone();
                let lm = d.layer_mut(t.layer)?;
                lm.smart_filters.filters.truncate(i);
                lm.smart_filters.cache = None;
                lumenply_render::smart_filters::refresh_stale(&mut d);
                d.layer(t.layer)?.raster_store()?.clone()
            }
            (CrfEdit::LiveLayer, _) | (CrfEdit::New, CrfApply::LiveLayer) => {
                let top = doc.layers();
                match top.iter().position(|x| x.id == t.layer) {
                    // A top-level layer: exactly the layers the filter sees.
                    Some(k) => {
                        let end = if t.edit == CrfEdit::LiveLayer { k } else { k + 1 };
                        lumenply_render::composite_layers(&top[..end], canvas, canvas)
                    }
                    // Inside a group: the whole picture is close enough
                    // for a preview.
                    None => lumenply_render::composite(doc),
                }
            }
            (CrfEdit::New, CrfApply::SmartFilter) => l.raster_store()?.clone(),
            (CrfEdit::New, CrfApply::Pixels) => l.content_store()?.clone(),
        };
        Some(shrink(&store.to_raster(frame), PREVIEW_MAX))
    }

    /// Re-takes the preview's pixels when "Apply as" changed what they are.
    pub(crate) fn crf_refresh_source(&mut self, st: &mut CameraRawState) {
        let Some(t) = st.filter.as_ref() else {
            return;
        };
        if t.apply == t.source_for {
            return;
        }
        let t = t.clone();
        if let Some(proxy) = self.crf_source(&t) {
            st.proxy = proxy;
            st.shown = None;
        }
        if let Some(t) = st.filter.as_mut() {
            t.source_for = t.apply;
        }
    }

    /// The workspace's OK in filter mode: one undo step.
    pub(crate) fn finish_camera_raw_filter(&mut self, ctx: &egui::Context, st: CameraRawState) {
        let Some(t) = st.filter else {
            return;
        };
        let layer = t.layer;
        let filter = Filter::Develop {
            settings: st.dev,
            frame: t.frame,
        };
        let started = std::time::Instant::now();
        let before = self.editor.history().len();
        match (t.edit, t.apply) {
            (CrfEdit::New, CrfApply::Pixels) => {
                if st.dev.is_neutral() {
                    self.status = "Camera Raw Filter: nothing changed".into();
                    return;
                }
                self.run(&ApplyFilter { layer, filter });
            }
            (CrfEdit::New, CrfApply::SmartFilter) => {
                self.run(&AddSmartFilter::new(layer, filter));
                let n = self
                    .editor
                    .doc()
                    .layer(layer)
                    .map_or(0, |l| l.smart_filters.filters.len());
                if n > 0 {
                    crate::smart_filters_ui::set_sf_focus(ctx, Some((layer, n - 1)));
                }
            }
            (CrfEdit::New, CrfApply::LiveLayer) => {
                self.set_active(Some(layer));
                self.add_filter_layer(filter);
            }
            (CrfEdit::SmartFilter(i), _) => {
                let Some(old) = self
                    .editor
                    .doc()
                    .layer(layer)
                    .and_then(|l| l.smart_filters.filters.get(i))
                    .cloned()
                else {
                    return;
                };
                if old.filter != filter {
                    self.run(&SetSmartFilter {
                        layer,
                        index: i,
                        filter: SmartFilter { filter, ..old },
                    });
                }
                crate::smart_filters_ui::set_sf_focus(ctx, Some((layer, i)));
            }
            (CrfEdit::LiveLayer, _) => {
                let same = matches!(
                    self.editor.doc().layer(layer).map(|l| &l.content),
                    Some(LayerContent::Filter(f)) if *f == filter
                );
                if !same {
                    self.run(&SetFilter { layer, filter });
                }
            }
        }
        if self.editor.history().len() > before {
            self.status = format!(
                "Camera Raw Filter ({}) in {:.2} s",
                t.apply.title().to_lowercase(),
                started.elapsed().as_secs_f32()
            );
        }
    }

    /// `--screenshot-do` tokens (`crf:...`): `crf:open` runs Filter ▸
    /// Camera Raw Filter on the active layer; `crf:set=KEY:VALUE` sets a
    /// slider (temperature, tint, exposure, contrast, highlights, shadows,
    /// whites, blacks, texture, clarity, dehaze, vibrance, saturation,
    /// vignette, midpoint), `crf:set=curve:1`, or `crf:set=as:pixels|smart|live`;
    /// `crf:before` toggles before / after; `crf:ok` and `crf:cancel` press
    /// the buttons; `crf:edit=INDEX` re-opens that smart filter of the
    /// active layer.
    pub(crate) fn debug_camera_raw_filter(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("crf:") else {
            return false;
        };
        let (key, arg) = rest.split_once('=').unwrap_or((rest, ""));
        match key {
            "open" => self.open_camera_raw_filter(),
            "edit" => {
                if let (Some(layer), Ok(i)) = (self.active, arg.parse::<usize>()) {
                    self.open_camera_raw_filter_on_smart(layer, i);
                }
            }
            "set" => {
                let (k, v) = arg.split_once(':').unwrap_or((arg, ""));
                if let Some(st) = self.camera_raw.as_mut() {
                    if k == "as" {
                        if let Some(t) = st.filter.as_mut() {
                            let want = match v {
                                "pixels" => CrfApply::Pixels,
                                "smart" => CrfApply::SmartFilter,
                                _ => CrfApply::LiveLayer,
                            };
                            if t.choices.contains(&want) {
                                t.apply = want;
                            }
                        }
                    } else if let Ok(x) = v.parse::<f32>() {
                        let d = &mut st.dev;
                        match k {
                            "temperature" => d.temperature = x,
                            "tint" => d.tint = x,
                            "exposure" => d.exposure = x,
                            "contrast" => d.contrast = x,
                            "highlights" => d.highlights = x,
                            "shadows" => d.shadows = x,
                            "whites" => d.whites = x,
                            "blacks" => d.blacks = x,
                            "texture" => d.texture = x,
                            "clarity" => d.clarity = x,
                            "dehaze" => d.dehaze = x,
                            "vibrance" => d.vibrance = x,
                            "saturation" => d.saturation = x,
                            "vignette" => d.vignette = x,
                            "midpoint" => d.vignette_midpoint = x,
                            "curve" => d.tone_curve = x != 0.0,
                            _ => {}
                        }
                    }
                }
            }
            "before" => {
                if let Some(st) = self.camera_raw.as_mut() {
                    st.before = !st.before;
                }
            }
            "ok" | "cancel" => {
                if let Some(mut st) = self.camera_raw.take() {
                    if st.filter.is_some() {
                        self.crf_refresh_source(&mut st);
                        if key == "ok" {
                            self.finish_camera_raw_filter(ctx, *st);
                        }
                    } else {
                        self.camera_raw = Some(st);
                    }
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::{Rgba, TileStore};

    /// A 120×80 document with one grey (0.2) pixel layer, in an app that
    /// never autosaves.
    fn grey_app() -> (App, LayerId) {
        let mut doc = Document::new(120, 80);
        let id = doc.add_pixel_layer("Photo");
        let r = Raster::filled(120, 80, Rgba::new(0.2, 0.2, 0.2, 1.0));
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.set_active(Some(id));
        (app, id)
    }

    fn px(app: &App, id: LayerId) -> f32 {
        app.editor
            .doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(60, 40)
            .r
    }

    fn exposure(ev: f32) -> Develop {
        Develop {
            exposure: ev,
            ..Develop::NEUTRAL
        }
    }

    #[test]
    fn on_a_pixel_layer_ok_bakes_the_develop_as_one_step() {
        let (mut app, id) = grey_app();
        let ctx = crate::a11y_tests::ctx();
        assert_eq!(app.action_block(CRF_ACTION), None);
        app.run_menu_action(CRF_ACTION);
        let st = app.camera_raw.as_mut().expect("workspace open");
        let t = st.filter.clone().unwrap();
        assert_eq!((t.edit, t.apply), (CrfEdit::New, CrfApply::Pixels));
        assert_eq!(t.frame, [0, 0, 120, 80]);
        assert_eq!(st.dev, Develop::NEUTRAL, "the filter starts at the identity");
        assert_eq!((st.proxy.width, st.proxy.height), (120, 80));
        st.dev = exposure(1.0);
        let st = app.camera_raw.take().unwrap();
        let before = app.editor.history().len();
        app.finish_camera_raw_filter(&ctx, *st);
        assert_eq!(app.editor.history().len(), before + 1, "one undo step");
        assert!((px(&app, id) - 0.4).abs() < 1e-3, "{}", px(&app, id));
        let l = app.editor.doc().layer(id).unwrap();
        assert!(l.smart_filters.is_empty(), "baked, not a smart filter");
        app.run_menu_action("undo");
        assert!((px(&app, id) - 0.2).abs() < 1e-3);
    }

    #[test]
    fn on_a_smart_object_it_is_a_smart_filter_that_reopens_and_updates() {
        let (mut app, id) = grey_app();
        let ctx = crate::a11y_tests::ctx();
        app.run(&ConvertToSmartObject { layer: id });
        app.open_camera_raw_filter();
        let st = app.camera_raw.as_mut().unwrap();
        assert_eq!(st.filter.as_ref().unwrap().apply, CrfApply::SmartFilter);
        st.dev = exposure(1.0);
        let st = app.camera_raw.take().unwrap();
        app.finish_camera_raw_filter(&ctx, *st);
        let sf = &app.editor.doc().layer(id).unwrap().smart_filters;
        assert_eq!(sf.filters.len(), 1);
        assert!(matches!(
            sf.filters[0].filter,
            Filter::Develop { settings, frame: [0, 0, 120, 80] } if settings == exposure(1.0)
        ));
        assert!((px(&app, id) - 0.4).abs() < 1e-3, "{}", px(&app, id));

        // Re-open it: the workspace comes back with its settings.
        app.open_camera_raw_filter_on_smart(id, 0);
        let st = app.camera_raw.as_mut().unwrap();
        assert_eq!(st.filter.as_ref().unwrap().edit, CrfEdit::SmartFilter(0));
        assert_eq!(st.dev, exposure(1.0));
        // Its preview starts from the unfiltered pixels.
        assert!((st.proxy.get(60, 40).r - 0.2).abs() < 1e-3);
        st.dev = exposure(-1.0);
        let st = app.camera_raw.take().unwrap();
        let before = app.editor.history().len();
        app.finish_camera_raw_filter(&ctx, *st);
        assert_eq!(app.editor.history().len(), before + 1, "the update is one step");
        let sf = &app.editor.doc().layer(id).unwrap().smart_filters;
        assert_eq!(sf.filters.len(), 1, "updated in place");
        assert!((px(&app, id) - 0.1).abs() < 1e-3, "{}", px(&app, id));
        app.run_menu_action("undo");
        assert!((px(&app, id) - 0.4).abs() < 1e-3);
    }

    #[test]
    fn as_a_live_layer_it_filters_what_is_below_and_reopens() {
        let (mut app, id) = grey_app();
        let ctx = crate::a11y_tests::ctx();
        app.open_camera_raw_filter();
        let st = app.camera_raw.as_mut().unwrap();
        st.dev = exposure(1.0);
        st.filter.as_mut().unwrap().apply = CrfApply::LiveLayer;
        let st = app.camera_raw.take().unwrap();
        app.finish_camera_raw_filter(&ctx, *st);
        let doc = app.editor.doc();
        assert_eq!(doc.layers().len(), 2);
        let live = doc.layers()[1].id;
        assert_eq!(app.active, Some(live));
        assert!((px(&app, id) - 0.2).abs() < 1e-3, "the pixels stay");
        let shown = lumenply_render::composite_raster(app.editor.doc()).get(60, 40).r;
        assert!((shown - 0.4).abs() < 1e-3, "{shown}");

        // The action on the live layer re-opens it.
        app.run_menu_action(CRF_ACTION);
        let st = app.camera_raw.as_mut().unwrap();
        assert_eq!(st.filter.as_ref().unwrap().edit, CrfEdit::LiveLayer);
        assert_eq!(st.dev, exposure(1.0));
        assert!(
            (st.proxy.get(60, 40).r - 0.2).abs() < 1e-3,
            "previews what is below"
        );
        st.dev = exposure(2.0);
        let st = app.camera_raw.take().unwrap();
        app.finish_camera_raw_filter(&ctx, *st);
        let shown = lumenply_render::composite_raster(app.editor.doc()).get(60, 40).r;
        assert!((shown - 0.8).abs() < 1e-3, "{shown}");
    }

    #[test]
    fn other_layers_are_refused_with_a_reason() {
        let (mut app, _) = grey_app();
        let adj = app.editor.doc().next_id();
        app.add_filter_layer(Filter::GaussianBlur { radius: 2.0 });
        assert_eq!(app.active, Some(adj));
        assert_eq!(
            app.action_block(CRF_ACTION),
            Some("Select a pixel layer or smart object first")
        );
    }

    #[test]
    fn the_filter_workspace_names_every_control() {
        let (mut app, _) = grey_app();
        app.open_camera_raw_filter();
        let ctx = crate::a11y_tests::ctx();
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.camera_raw.is_some(), "still open");
        assert_eq!(missing, Vec::<String>::new());
    }
}
