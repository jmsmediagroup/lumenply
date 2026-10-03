//! The Channels panel: the RGB composite and its red, green and blue
//! channels with thumbnails, then the saved selections as alpha channels.
//! Clicking a channel shows it alone on the canvas (display only: Cmd+2
//! returns to RGB); Cmd-click loads an alpha channel as the selection,
//! double-click renames it, its eye shows it as a red overlay.

use super::*;
use crate::layers::IconButton;
use crate::panels::{channel_gray, panel_row, rename_field, ChannelView, REBUILD_EVERY, ROW_H};
use lumenply_core::channels::{ColourChannel, SelectionFromChannel};

/// The composite channel a colour view reads (RGB loads luminosity).
pub(crate) fn colour_channel(v: ChannelView) -> Option<ColourChannel> {
    match v {
        ChannelView::Composite => Some(ColourChannel::Luminosity),
        ChannelView::Red => Some(ColourChannel::Red),
        ChannelView::Green => Some(ColourChannel::Green),
        ChannelView::Blue => Some(ColourChannel::Blue),
        ChannelView::Alpha(_) => None,
    }
}

/// What a click in the Channels panel asks for.
enum ChanAct {
    View(ChannelView),
    Overlay(usize),
    Load(usize, CombineOp),
    LoadColour(ColourChannel, CombineOp),
    Rename(usize),
    Delete(usize),
    Save,
}

/// The composite's picture sampled onto a thumbnail, as `thumb_image`
/// lays it out, with `sample` giving each cell's colour directly.
pub(crate) fn thumb_with(
    canvas: Rect,
    size: (usize, usize),
    sample: impl Fn(i32, i32) -> Color32,
) -> egui::ColorImage {
    let (tw, th) = size;
    let scale = (canvas.w as f32 / tw as f32)
        .max(canvas.h as f32 / th as f32)
        .max(1e-3);
    let (cw, ch) = (canvas.w as f32 / scale, canvas.h as f32 / scale);
    let (ox, oy) = ((tw as f32 - cw) / 2.0, (th as f32 - ch) / 2.0);
    let mut pixels = vec![Color32::TRANSPARENT; tw * th];
    for ty in 0..th {
        for tx in 0..tw {
            let fx = tx as f32 + 0.5 - ox;
            let fy = ty as f32 + 0.5 - oy;
            if fx < 0.0 || fy < 0.0 || fx >= cw || fy >= ch {
                continue;
            }
            let x = canvas.x + (fx * scale) as i32;
            let y = canvas.y + (fy * scale) as i32;
            pixels[ty * tw + tx] = sample(x, y);
        }
    }
    egui::ColorImage {
        size: [tw, th],
        pixels,
    }
}

/// Load-selection operation for a Cmd-click with these modifiers
/// (Photoshop: Shift adds, Alt subtracts, both intersect).
pub(crate) fn load_op(m: egui::Modifiers) -> CombineOp {
    match (m.shift, m.alt) {
        (true, true) => CombineOp::Intersect,
        (true, false) => CombineOp::Union,
        (false, true) => CombineOp::Subtract,
        _ => CombineOp::Replace,
    }
}

impl App {
    /// Rebuild the channel thumbnails when the document changed, at most
    /// a few times a second while it keeps changing.
    fn channel_thumbs(&mut self, ctx: &egui::Context) {
        let n = 4 + self.editor.doc().saved_selections.len();
        let missing = self.panels.thumbs.len() != n;
        if !missing && !self.panels.thumbs_stale {
            return;
        }
        if !missing && self.panels.thumbs_at.is_some_and(|t| t.elapsed() < REBUILD_EVERY) {
            ctx.request_repaint_after(REBUILD_EVERY);
            return;
        }
        let Some(flat) = &self.last_flat else { return };
        let doc = self.editor.doc();
        if flat.width != doc.width || flat.height != doc.height {
            return;
        }
        let canvas = doc.canvas();
        let get = |x: i32, y: i32| {
            flat.get(
                x.clamp(0, flat.width as i32 - 1) as u32,
                y.clamp(0, flat.height as i32 - 1) as u32,
            )
        };
        let mut imgs = vec![thumb_image(canvas, get)];
        for ch in 0..3 {
            imgs.push(thumb_with(canvas, THUMB, |x, y| {
                Color32::from_gray(channel_gray(get(x, y), ch))
            }));
        }
        for s in &doc.saved_selections {
            imgs.push(thumb_with(canvas, THUMB, |x, y| {
                Color32::from_gray((s.mask.value(x, y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            }));
        }
        let thumbs = &mut self.panels.thumbs;
        thumbs.truncate(imgs.len());
        for (i, img) in imgs.into_iter().enumerate() {
            match thumbs.get_mut(i) {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => thumbs.push(ctx.load_texture(
                    format!("channel-thumb-{i}"),
                    img,
                    egui::TextureOptions::LINEAR,
                )),
            }
        }
        self.panels.thumbs_stale = false;
        self.panels.thumbs_at = Some(std::time::Instant::now());
    }

    pub(crate) fn channels_ui(&mut self, ui: &mut egui::Ui) {
        self.channel_thumbs(ui.ctx());
        let ctx = ui.ctx().clone();
        let view = self.panels.view;
        let overlay = self.panels.overlay;
        let alphas: Vec<String> = self
            .editor
            .doc()
            .saved_selections
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let has_selection = self.editor.doc().selection.is_some();
        let mods = ui.input(|i| i.modifiers);
        let mut act: Option<ChanAct> = None;
        let mut renaming = self.panels.channel_renaming.take();
        let mut rename_done: Option<Result<(usize, String), ()>> = None;
        let colour_rows = [
            ("RGB", ChannelView::Composite, "channel-rgb"),
            ("Red", ChannelView::Red, "channel-red"),
            ("Green", ChannelView::Green, "channel-green"),
            ("Blue", ChannelView::Blue, "channel-blue"),
        ];
        let scroll = egui::ScrollArea::vertical()
            .id_salt("channels")
            .max_height((ui.available_height() - layers::FOOTER_H).max(ROW_H))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, (name, v, id)) in colour_rows.iter().enumerate() {
                    let shown = view == *v || (view == ChannelView::Composite && i > 0);
                    let active = view == *v;
                    let (rect, resp) = panel_row(ui, active, shown && !active, &format!("Channel {name}"));
                    let p = ui.painter();
                    let cy = rect.center().y;
                    let eye =
                        egui::Rect::from_center_size(egui::pos2(rect.min.x + 14.0, cy), Vec2::splat(16.0));
                    layers::paint_eye(p, eye, shown);
                    let t = egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + 28.0, cy - 15.0),
                        egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                    );
                    paint_thumb_bg(p, t);
                    if let Some(tex) = self.panels.thumbs.get(i) {
                        p.image(tex.id(), t, full_uv(), Color32::WHITE);
                    }
                    p.rect_stroke(t, 2.0, Stroke::new(1.0, LINE));
                    let keys = self.panel_action_keys(&ctx, id).unwrap_or_default();
                    p.text(
                        egui::pos2(rect.max.x - 8.0, cy),
                        Align2::RIGHT_CENTER,
                        keys,
                        FontId::monospace(11.0),
                        MUTED,
                    );
                    p.text(
                        egui::pos2(t.max.x + 8.0, cy),
                        Align2::LEFT_CENTER,
                        *name,
                        FontId::proportional(14.0),
                        TEXT,
                    );
                    let tip = if *v == ChannelView::Composite {
                        "Show all channels · Cmd-click to load luminosity as a selection".to_string()
                    } else {
                        format!("Show the {name} channel alone · Cmd-click to load it as a selection")
                    };
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        act = Some(match colour_channel(*v) {
                            Some(c) if mods.command => ChanAct::LoadColour(c, load_op(mods)),
                            _ => ChanAct::View(*v),
                        });
                    }
                }
                if alphas.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Saved selections appear here as alpha channels.")
                            .small()
                            .color(MUTED),
                    );
                }
                for (i, name) in alphas.iter().enumerate() {
                    let active = view == ChannelView::Alpha(i);
                    let shown = active || overlay == Some(i);
                    let (rect, resp) = panel_row(ui, active, false, &format!("Alpha channel {name}"));
                    let p = ui.painter();
                    let cy = rect.center().y;
                    let eye =
                        egui::Rect::from_center_size(egui::pos2(rect.min.x + 14.0, cy), Vec2::splat(16.0));
                    let on_eye = resp.hover_pos().is_some_and(|q| eye.expand(3.0).contains(q));
                    if on_eye {
                        p.rect_filled(eye.expand(3.0), 4.0, HOVER);
                    }
                    layers::paint_eye(p, eye, shown);
                    let t = egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + 28.0, cy - 15.0),
                        egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                    );
                    p.rect_filled(t, 2.0, Color32::BLACK);
                    if let Some(tex) = self.panels.thumbs.get(4 + i) {
                        p.image(tex.id(), t, full_uv(), Color32::WHITE);
                    }
                    p.rect_stroke(
                        t,
                        2.0,
                        Stroke::new(1.0, if overlay == Some(i) { DANGER } else { LINE }),
                    );
                    let x = t.max.x + 8.0;
                    if let Some((ri, text)) = renaming.as_mut() {
                        if *ri == i {
                            let r = egui::Rect::from_min_max(
                                egui::pos2(x, cy - 11.0),
                                egui::pos2(rect.max.x - 6.0, cy + 11.0),
                            );
                            if let Some(done) = rename_field(ui, r, text, "Channel name") {
                                rename_done = Some(done.map(|n| (i, n)));
                            }
                            continue;
                        }
                    }
                    let (galley, _) =
                        elided(ui, name, FontId::proportional(14.0), TEXT, rect.max.x - 8.0 - x);
                    p.galley(egui::pos2(x, cy - galley.size().y / 2.0), galley, TEXT);
                    let tip = if on_eye {
                        "Show or hide as a red overlay on the image".to_string()
                    } else {
                        "Click to view alone · Cmd-click to load as selection · double-click to rename"
                            .to_string()
                    };
                    let resp = resp.on_hover_text(tip);
                    context_menu(&resp, |ui| {
                        if menu_item(ui, "View alone", "") {
                            act = Some(ChanAct::View(ChannelView::Alpha(i)));
                        }
                        let label = if overlay == Some(i) {
                            "Hide overlay"
                        } else {
                            "Show as overlay"
                        };
                        if menu_item(ui, label, "") {
                            act = Some(ChanAct::Overlay(i));
                        }
                        menu_separator(ui);
                        for (label, op) in [
                            ("Load as selection", CombineOp::Replace),
                            ("Add to selection", CombineOp::Union),
                            ("Subtract from selection", CombineOp::Subtract),
                            ("Intersect with selection", CombineOp::Intersect),
                        ] {
                            if menu_item(ui, label, "") {
                                act = Some(ChanAct::Load(i, op));
                            }
                        }
                        menu_separator(ui);
                        if menu_item(ui, "Rename", "") {
                            act = Some(ChanAct::Rename(i));
                        }
                        if menu_item(ui, "Delete channel", "") {
                            act = Some(ChanAct::Delete(i));
                        }
                    });
                    if resp.double_clicked() {
                        act = Some(ChanAct::Rename(i));
                    } else if resp.clicked() {
                        act = Some(if on_eye {
                            ChanAct::Overlay(i)
                        } else if mods.command {
                            ChanAct::Load(i, load_op(mods))
                        } else {
                            ChanAct::View(ChannelView::Alpha(i))
                        });
                    }
                }
            });
        a11y_scroll(ui.ctx(), &scroll, "Channels");

        // The footer: Photoshop's load / save / delete channel buttons.
        let target = match view {
            ChannelView::Alpha(i) => Some(i),
            _ => overlay,
        };
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Loads the alpha channel in view (or overlaid), else the
            // colour channel in view, else the luminosity.
            if ui
                .add(IconButton::new(icon_load_selection, "Load channel as selection"))
                .clicked()
            {
                act = Some(match (target, colour_channel(view)) {
                    (Some(i), _) => ChanAct::Load(i, load_op(mods)),
                    (None, Some(c)) => ChanAct::LoadColour(c, load_op(mods)),
                    (None, None) => ChanAct::LoadColour(ColourChannel::Luminosity, load_op(mods)),
                });
            }
            if ui
                .add_enabled(
                    has_selection,
                    IconButton::new(icon_save_selection, "Save selection as channel"),
                )
                .clicked()
            {
                act = Some(ChanAct::Save);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(target.is_some(), IconButton::new(icon_trash, "Delete channel"))
                    .clicked()
                {
                    if let Some(i) = target {
                        act = Some(ChanAct::Delete(i));
                    }
                }
            });
        });

        match rename_done {
            Some(Ok((index, name))) => {
                renaming = None;
                let old = self
                    .editor
                    .doc()
                    .saved_selections
                    .get(index)
                    .map(|s| s.name.clone());
                if old.as_deref() != Some(name.trim()) {
                    self.run(&lumenply_core::channels::RenameSavedSelection { index, name });
                }
            }
            Some(Err(())) => renaming = None,
            None => {}
        }
        self.panels.channel_renaming = renaming;
        match act {
            Some(ChanAct::View(v)) => self.set_channel_view(v),
            Some(ChanAct::Overlay(i)) => {
                self.panels.overlay = if overlay == Some(i) { None } else { Some(i) };
            }
            Some(ChanAct::LoadColour(channel, op)) => {
                self.run(&SelectionFromChannel { channel, op });
                self.status = format!("Loaded the {channel:?} channel as the selection")
                    .replace("Luminosity channel", "luminosity");
            }
            Some(ChanAct::Load(index, op)) => {
                self.run(&lumenply_core::channels::LoadSelection {
                    index,
                    op,
                    invert: false,
                });
                self.status = format!(
                    "Loaded \"{}\" as the selection",
                    alphas.get(index).map_or("", |s| s)
                );
            }
            Some(ChanAct::Rename(i)) => {
                self.panels.channel_renaming = alphas.get(i).map(|n| (i, n.clone()));
            }
            Some(ChanAct::Delete(index)) => {
                self.run(&lumenply_core::channels::DeleteSavedSelection { index });
                // Views of later channels follow them down one slot.
                let shift = |j: usize| (j != index).then(|| if j > index { j - 1 } else { j });
                self.panels.overlay = self.panels.overlay.and_then(shift);
                if let ChannelView::Alpha(j) = self.panels.view {
                    self.panels.view = shift(j).map_or(ChannelView::Composite, ChannelView::Alpha);
                }
            }
            Some(ChanAct::Save) => {
                let n = self.editor.doc().saved_selections.len() + 1;
                self.run(&lumenply_core::channels::SaveSelection {
                    name: format!("Alpha {n}"),
                });
            }
            None => {}
        }
    }
}

pub(crate) fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// A dotted circle: load as selection.
pub(crate) fn icon_load_selection(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let n = 12;
    let rad = r.width() * 0.45;
    for i in (0..n).step_by(2) {
        let a0 = i as f32 / n as f32 * std::f32::consts::TAU;
        let a1 = (i as f32 + 1.0) / n as f32 * std::f32::consts::TAU;
        p.line_segment(
            [
                r.center() + egui::vec2(a0.cos(), a0.sin()) * rad,
                r.center() + egui::vec2(a1.cos(), a1.sin()) * rad,
            ],
            Stroke::new(1.6, c),
        );
    }
}

/// A frame holding a filled disc: save the selection as a channel.
pub(crate) fn icon_save_selection(p: &egui::Painter, r: egui::Rect, c: Color32) {
    p.rect_stroke(r, 2.0, Stroke::new(1.6, c));
    p.circle_filled(r.center(), r.width() * 0.26, c);
}

pub(crate) fn icon_trash(p: &egui::Painter, r: egui::Rect, c: Color32) {
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
    fn a_channel_alone_says_how_back_in_the_apps_shortcut_spelling() {
        let mut app = crate::a11y_tests::launch(&["--demo".to_string()]);
        app.set_channel_view(ChannelView::Green);
        let back = if cfg!(target_os = "macos") {
            "Cmd+2"
        } else {
            "Ctrl+2"
        };
        assert_eq!(
            app.status,
            format!("Viewing the green channel alone ({back} returns to RGB)")
        );
    }

    #[test]
    fn colour_rows_load_their_channel_and_rgb_loads_luminosity() {
        assert_eq!(
            colour_channel(ChannelView::Composite),
            Some(ColourChannel::Luminosity)
        );
        assert_eq!(colour_channel(ChannelView::Red), Some(ColourChannel::Red));
        assert_eq!(colour_channel(ChannelView::Green), Some(ColourChannel::Green));
        assert_eq!(colour_channel(ChannelView::Blue), Some(ColourChannel::Blue));
        assert_eq!(colour_channel(ChannelView::Alpha(0)), None);
    }

    #[test]
    fn cmd_click_modifiers_pick_the_load_operation() {
        let m = |shift, alt| egui::Modifiers {
            command: true,
            shift,
            alt,
            ..Default::default()
        };
        assert_eq!(load_op(m(false, false)), CombineOp::Replace);
        assert_eq!(load_op(m(true, false)), CombineOp::Union);
        assert_eq!(load_op(m(false, true)), CombineOp::Subtract);
        assert_eq!(load_op(m(true, true)), CombineOp::Intersect);
    }

    #[test]
    fn channel_thumbnails_fit_the_canvas_and_sample_cell_centres() {
        // A 88×30 canvas fills a 44×30 thumbnail across, letterboxed down.
        let img = thumb_with(Rect::new(0, 0, 88, 30), (44, 30), |x, _| {
            Color32::from_gray(x as u8)
        });
        assert_eq!(img.size, [44, 30]);
        assert_eq!(img.pixels[0].a(), 0, "letterbox rows stay transparent");
        let row = 15 * 44;
        assert_eq!((img.pixels[row].r(), img.pixels[row + 43].r()), (1, 87));
    }

    #[test]
    fn the_channels_panel_views_loads_renames_and_deletes() {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(blank(40, 20), None);
        let ctx = crate::a11y_tests::ctx();
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 20, 20))),
        });
        app.run(&lumenply_core::channels::SaveSelection { name: "Left".into() });
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(20, 0, 20, 20))),
        });
        app.run(&lumenply_core::channels::SaveSelection { name: "Right".into() });
        app.run(&SetSelection { selection: None });
        app.prefs.panels.tab = panels::DockTab::Channels;
        // Frames build the thumbnails: RGB, R, G, B, Left, Right.
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
        assert_eq!(app.panels.thumbs.len(), 6);
        // Viewing the second alpha channel, then deleting the first: the
        // view follows the channel down a slot.
        app.set_channel_view(ChannelView::Alpha(1));
        app.panels.overlay = Some(1);
        let _ = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.status.contains("Right"), "{}", app.status);
        app.run(&lumenply_core::channels::DeleteSavedSelection { index: 0 });
        let _ = crate::a11y_tests::nameless(&mut app, &ctx);
        // The index-shifting is the panel's own (Delete button); the
        // refresh hook drops views of channels that are gone.
        assert_eq!(app.panels.view, ChannelView::Composite);
        assert_eq!(app.panels.overlay, None);
        app.run(&lumenply_core::channels::LoadSelection {
            index: 0,
            op: CombineOp::Replace,
            invert: false,
        });
        let sel = app.editor.doc().selection.as_ref().unwrap();
        assert_eq!((sel.value(5, 5), sel.value(30, 5)), (0.0, 1.0));
        // An open rename box is named too.
        app.panels.channel_renaming = Some((0, "Right".into()));
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
    }
}
