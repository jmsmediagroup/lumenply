//! The dock's tabbed area — Layers | Channels | Paths, grouped as in
//! Photoshop — the floating Navigator and Info panels, the canvas's
//! channel view, and the state they share.
//!
//! Everything here is display state: which tab is up, which channel the
//! canvas shows, which path is highlighted. Document edits (rename a
//! channel, fill a path...) go through commands like every other edit.

use std::time::{Duration, Instant};

use super::*;

/// Height of the tab strip above the panel content.
pub(crate) const TABS_H: f32 = 30.0;
/// Row height inside the Channels and Paths panels (the Layers pitch).
pub(crate) const ROW_H: f32 = layers::ROW_PITCH - 2.0;
/// Thumbnails and the navigator image rebuild at most this often while
/// the document keeps changing.
pub(crate) const REBUILD_EVERY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum DockTab {
    #[default]
    Layers,
    Channels,
    Paths,
}

impl DockTab {
    pub(crate) const ALL: [DockTab; 3] = [DockTab::Layers, DockTab::Channels, DockTab::Paths];

    pub(crate) fn name(self) -> &'static str {
        match self {
            DockTab::Layers => "Layers",
            DockTab::Channels => "Channels",
            DockTab::Paths => "Paths",
        }
    }

    fn action(self) -> &'static str {
        match self {
            DockTab::Layers => "panel-layers",
            DockTab::Channels => "panel-channels",
            DockTab::Paths => "panel-paths",
        }
    }
}

/// Panel choices remembered between sessions (a part of `Prefs`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PanelPrefs {
    /// The tab showing in the Layers area of the dock.
    pub tab: DockTab,
    /// Window ▸ Navigator.
    pub navigator: bool,
    /// Window ▸ Info.
    pub info: bool,
    /// Window ▸ Histogram.
    pub histogram: bool,
}

/// What the canvas shows: the composite, one colour channel alone as
/// grayscale, or a saved selection (alpha channel) alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChannelView {
    #[default]
    Composite,
    Red,
    Green,
    Blue,
    Alpha(usize),
}

impl ChannelView {
    /// 0, 1, 2 for the colour channels.
    pub(crate) fn colour_index(self) -> Option<usize> {
        match self {
            ChannelView::Red => Some(0),
            ChannelView::Green => Some(1),
            ChannelView::Blue => Some(2),
            _ => None,
        }
    }
}

/// A row of the Paths panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathRow {
    Work,
    Saved(usize),
}

/// Per-window panel state (never saved, never part of the document).
#[derive(Default)]
pub(crate) struct PanelState {
    pub(crate) view: ChannelView,
    /// A saved selection shown as a red overlay over the image (its eye).
    pub(crate) overlay: Option<usize>,
    /// The canvas texture for `view` and `overlay`, and what it shows.
    view_tex: Option<TextureHandle>,
    view_shown: Option<(ChannelView, Option<usize>)>,
    /// Channels panel thumbnails: RGB, R, G, B, then the alpha channels.
    pub(crate) thumbs: Vec<TextureHandle>,
    pub(crate) thumbs_stale: bool,
    pub(crate) thumbs_at: Option<Instant>,
    /// The Navigator's picture of the composite.
    pub(crate) nav_tex: Option<TextureHandle>,
    pub(crate) nav_stale: bool,
    pub(crate) nav_at: Option<Instant>,
    /// The document these belong to (`App::doc_key`).
    doc_key: Option<u64>,
    pub(crate) channel_renaming: Option<(usize, String)>,
    pub(crate) path_sel: Option<PathRow>,
    pub(crate) path_renaming: Option<(usize, String)>,
    /// The canvas area on screen, as last painted.
    pub(crate) canvas_rect: Option<egui::Rect>,
    /// The Info panel's selection size.
    pub(crate) info_cache: crate::info_panel::InfoCache,
}

impl PanelState {
    /// A live preview (a brush stroke in progress) patched the composite
    /// over `r` with `patch`: patch a colour-channel view the same way,
    /// so painting shows while one channel is viewed alone.
    pub(crate) fn preview_patch(&mut self, r: Rect, patch: &Raster) {
        let Some(ch) = self.view.colour_index() else {
            return;
        };
        if self.overlay.is_some() || self.view_shown != Some((self.view, None)) {
            return;
        }
        let Some(tex) = self.view_tex.as_mut() else { return };
        let [tw, th] = tex.size();
        if r.right() > tw as i32 || r.bottom() > th as i32 || r.x < 0 || r.y < 0 {
            return;
        }
        let img = egui::ColorImage {
            size: [patch.width as usize, patch.height as usize],
            pixels: patch
                .pixels
                .iter()
                .map(|p| Color32::from_gray(channel_gray(*p, ch)))
                .collect(),
        };
        tex.set_partial([r.x as usize, r.y as usize], img, nearest_when_zoomed());
    }
}

/// A row in the Channels or Paths panel: the click target, its spoken
/// name and the active / hover / focus look of a layer row.
pub(crate) fn panel_row(
    ui: &mut egui::Ui,
    active: bool,
    soft: bool,
    name: &str,
) -> (egui::Rect, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let name = name.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, &name));
    let p = ui.painter();
    if active {
        p.rect_filled(rect, RADIUS, ACCENT_TINT);
        p.rect_stroke(rect.shrink(1.0), RADIUS, Stroke::new(1.5, ACCENT));
    } else if soft || resp.hovered() {
        p.rect_filled(rect, RADIUS, RAISED);
    }
    if resp.has_focus() {
        p.rect_stroke(rect, RADIUS, Stroke::new(1.5, ACCENT));
    }
    (rect, resp)
}

/// An inline rename box over a row; returns Some(Ok(name)) to commit,
/// Some(Err(())) to cancel.
pub(crate) fn rename_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    text: &mut String,
    name: &str,
) -> Option<Result<String, ()>> {
    let r = ui.put(rect, egui::TextEdit::singleline(text));
    a11y_name(&r, name);
    r.request_focus();
    let (enter, escape, away) = ui.input(|i| {
        (
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Escape),
            i.pointer.any_pressed() && !r.hovered(),
        )
    });
    if escape {
        Some(Err(()))
    } else if enter || away || r.lost_focus() {
        Some(Ok(text.clone()))
    } else {
        None
    }
}

/// A small close (×) button for a floating panel's header.
pub(crate) fn close_button(ui: &mut egui::Ui, name: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
    let n = name.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &n));
    if resp.hovered() || resp.has_focus() {
        ui.painter().rect_filled(rect, 4.0, HOVER);
    }
    focus_ring(ui, &resp, rect, 4.0);
    let c = rect.center();
    let s = Stroke::new(1.4, if resp.hovered() { TEXT } else { MUTED });
    ui.painter()
        .line_segment([c - Vec2::splat(4.0), c + Vec2::splat(4.0)], s);
    ui.painter()
        .line_segment([c + egui::vec2(-4.0, 4.0), c + egui::vec2(4.0, -4.0)], s);
    resp.on_hover_text(name.to_string()).clicked()
}

/// The frame of the floating Navigator and Info panels.
pub(crate) fn float_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .rounding(8.0)
        .stroke(Stroke::new(1.0, LINE))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 4.0),
            blur: 14.0,
            spread: 0.0,
            color: Color32::from_black_alpha(90),
        })
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
}

/// One colour channel of a composite pixel as Photoshop shows it alone:
/// the gamma-encoded 8-bit value, over white where the image is
/// transparent. `ch` is 0, 1, 2 for R, G, B.
pub(crate) fn channel_gray(p: lumenply_tiles::Rgba, ch: usize) -> u8 {
    // Color32 holds premultiplied sRGB bytes: v + (255 − a) is "over white".
    let c = to_color32(p);
    let v = [c.r(), c.g(), c.b()][ch.min(2)] as u32;
    (v + (255 - c.a() as u32)).min(255) as u8
}

/// The red "rubylith" a saved selection shows as over the image:
/// unselected areas tinted 50% red.
pub(crate) fn overlay_tint(coverage: f32) -> Color32 {
    let a = ((1.0 - coverage.clamp(0.0, 1.0)) * 128.0 + 0.5) as u8;
    // Pure red, premultiplied: the red byte equals the alpha.
    Color32::from_rgba_premultiplied(a, 0, 0, a)
}

/// The canvas image for a channel view (and optional overlay) over
/// `area` of the composite `flat`; `None` for the plain composite.
pub(crate) fn view_image(
    doc: &Document,
    flat: &Raster,
    view: ChannelView,
    overlay: Option<usize>,
    area: Rect,
) -> Option<egui::ColorImage> {
    let base_mask = match view {
        ChannelView::Alpha(i) => Some(&doc.saved_selections.get(i)?.mask),
        _ => None,
    };
    let over = overlay.and_then(|i| doc.saved_selections.get(i)).map(|s| &s.mask);
    if view == ChannelView::Composite && over.is_none() {
        return None;
    }
    let (w, h) = (area.w as usize, area.h as usize);
    let mut pixels = vec![Color32::TRANSPARENT; w * h];
    for y in 0..h {
        let dy = area.y + y as i32;
        for x in 0..w {
            let dx = area.x + x as i32;
            let mut c = match (view.colour_index(), base_mask) {
                (Some(ch), _) => Color32::from_gray(channel_gray(flat.get(dx as u32, dy as u32), ch)),
                (None, Some(m)) => Color32::from_gray((m.value(dx, dy).clamp(0.0, 1.0) * 255.0 + 0.5) as u8),
                _ => Color32::TRANSPARENT,
            };
            if let Some(m) = over {
                let t = overlay_tint(m.value(dx, dy));
                c = if c == Color32::TRANSPARENT {
                    t
                } else {
                    blend_over(c, t)
                };
            }
            pixels[y * w + x] = c;
        }
    }
    Some(egui::ColorImage { size: [w, h], pixels })
}

/// Premultiplied `top` over an opaque `base`.
fn blend_over(base: Color32, top: Color32) -> Color32 {
    let keep = (255 - top.a()) as f32 / 255.0;
    let mix = |b: u8, t: u8| (t as f32 + b as f32 * keep + 0.5).min(255.0) as u8;
    Color32::from_rgb(
        mix(base.r(), top.r()),
        mix(base.g(), top.g()),
        mix(base.b(), top.b()),
    )
}

impl App {
    // ---- the dock's tabs --------------------------------------------------

    /// Rows the active tab wants, for the dock's height budget.
    pub(crate) fn dock_rows(&self) -> usize {
        let doc = self.editor.doc();
        match self.prefs.panels.tab {
            DockTab::Layers => self.layer_rows().len().max(1),
            DockTab::Channels => 4 + doc.saved_selections.len(),
            DockTab::Paths => (usize::from(doc.work_path.is_some()) + doc.saved_paths.len()).max(1),
        }
    }

    /// Persist the panel choices. Unit tests share one prefs file and run
    /// in parallel: a tab saved by one would leak into the others.
    pub(crate) fn save_panel_prefs(&self) {
        if !cfg!(test) {
            self.prefs.save();
        }
    }

    pub(crate) fn set_dock_tab(&mut self, tab: DockTab) {
        if self.prefs.panels.tab != tab {
            self.prefs.panels.tab = tab;
            self.save_panel_prefs();
        }
    }

    /// The Layers | Channels | Paths tab strip and the chosen panel.
    pub(crate) fn dock_tabs_ui(&mut self, ui: &mut egui::Ui) {
        let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), TABS_H), Sense::hover());
        let mut pick = None;
        let mut x = strip.min.x;
        for tab in DockTab::ALL {
            let on = self.prefs.panels.tab == tab;
            let font = FontId::proportional(13.5);
            let ink = if on { TEXT } else { MUTED };
            let galley = ui.painter().layout_no_wrap(tab.name().into(), font, ink);
            let w = galley.size().x + 22.0;
            let r = egui::Rect::from_min_size(egui::pos2(x, strip.min.y + 2.0), egui::vec2(w, TABS_H - 6.0));
            let resp = ui.interact(r, egui::Id::new(("dock-tab", tab.name())), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    on,
                    format!("{} panel", tab.name()),
                )
            });
            note_target(ui.ctx(), &format!("tab-{}", tab.name()), r);
            let p = ui.painter();
            if on {
                p.rect_filled(r, RADIUS, RAISED);
                p.hline(r.x_range().shrink(6.0), r.max.y + 1.0, Stroke::new(2.0, ACCENT));
            } else if resp.hovered() {
                p.rect_filled(r, RADIUS, HOVER);
            }
            focus_ring(ui, &resp, r, RADIUS);
            p.galley(r.center() - galley.size() / 2.0, galley, ink);
            if resp.clicked() {
                pick = Some(tab);
            }
            x += w + 2.0;
        }
        ui.painter()
            .hline(strip.x_range(), strip.max.y - 0.5, Stroke::new(1.0, LINE));
        if let Some(t) = pick {
            self.set_dock_tab(t);
        }
        match self.prefs.panels.tab {
            DockTab::Layers => self.layers_ui(ui),
            DockTab::Channels => self.channels_ui(ui),
            DockTab::Paths => self.paths_ui(ui),
        }
    }

    // ---- channel view ---------------------------------------------------------

    /// Show `view` on the canvas (display only; Cmd+2 returns to RGB).
    pub(crate) fn set_channel_view(&mut self, view: ChannelView) {
        self.panels.view = view;
        self.status = match view {
            ChannelView::Composite => "Viewing the RGB composite".into(),
            ChannelView::Red | ChannelView::Green | ChannelView::Blue => format!(
                "Viewing the {} channel alone ({} returns to RGB)",
                match view {
                    ChannelView::Red => "red",
                    ChannelView::Green => "green",
                    _ => "blue",
                },
                // Written as every other shortcut in the app is.
                if cfg!(target_os = "macos") {
                    "Cmd+2"
                } else {
                    "Ctrl+2"
                }
            ),
            ChannelView::Alpha(i) => format!(
                "Viewing channel \"{}\" alone",
                self.editor
                    .doc()
                    .saved_selections
                    .get(i)
                    .map_or("", |s| s.name.as_str())
            ),
        };
    }

    /// After every canvas refresh: forget state from another document,
    /// drop views of channels and paths that no longer exist, mark the
    /// thumbnails stale and bring the channel view up to date (`area`
    /// is what changed; `None` = everything).
    pub(crate) fn panels_refreshed(&mut self, ctx: &egui::Context, area: Option<Rect>) {
        let doc = self.editor.doc();
        let p = &mut self.panels;
        if p.doc_key != Some(self.doc_key) {
            p.doc_key = Some(self.doc_key);
            p.view = ChannelView::Composite;
            p.overlay = None;
            p.path_sel = None;
            p.channel_renaming = None;
            p.path_renaming = None;
            p.view_shown = None;
        }
        let n = doc.saved_selections.len();
        if matches!(p.view, ChannelView::Alpha(i) if i >= n) {
            p.view = ChannelView::Composite;
        }
        if p.overlay.is_some_and(|i| i >= n) {
            p.overlay = None;
        }
        match p.path_sel {
            Some(PathRow::Saved(i)) if i >= doc.saved_paths.len() => p.path_sel = None,
            Some(PathRow::Work) if doc.work_path.is_none() => p.path_sel = None,
            _ => {}
        }
        p.thumbs_stale = true;
        p.nav_stale = true;
        p.info_cache.stale = true;
        self.update_channel_view(ctx, area);
    }

    /// Rebuild (or patch, for a colour channel and a known `area`) the
    /// channel-view texture from the composite.
    fn update_channel_view(&mut self, ctx: &egui::Context, area: Option<Rect>) {
        let want = (self.panels.view, self.panels.overlay);
        if want == (ChannelView::Composite, None) {
            self.panels.view_tex = None;
            self.panels.view_shown = None;
            return;
        }
        let Some(flat) = &self.last_flat else { return };
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let sized = self
            .panels
            .view_tex
            .as_ref()
            .is_some_and(|t| t.size() == [doc.width as usize, doc.height as usize]);
        let patch = self.panels.view_shown == Some(want)
            && sized
            && want.0.colour_index().is_some()
            && want.1.is_none();
        match (area, patch) {
            (Some(r), true) => {
                let r = r.intersect(&canvas);
                if !r.is_empty() {
                    if let (Some(img), Some(tex)) = (
                        view_image(doc, flat, want.0, want.1, r),
                        self.panels.view_tex.as_mut(),
                    ) {
                        tex.set_partial([r.x as usize, r.y as usize], img, nearest_when_zoomed());
                    }
                }
            }
            _ => match view_image(doc, flat, want.0, want.1, canvas) {
                Some(img) => upload(
                    &mut self.panels.view_tex,
                    ctx,
                    "channel-view",
                    img,
                    nearest_when_zoomed(),
                ),
                None => self.panels.view_tex = None,
            },
        }
        self.panels.view_shown = Some(want);
    }

    /// Canvas hook, painted right over the composite: the channel view,
    /// the path picked in the Paths panel, and where the canvas is (for
    /// the Navigator).
    pub(crate) fn paint_panels_on_canvas(
        &mut self,
        painter: &egui::Painter,
        rect: egui::Rect,
        doc_rect: egui::Rect,
    ) {
        self.panels.canvas_rect = Some(rect);
        let want = (self.panels.view, self.panels.overlay);
        if want != (ChannelView::Composite, None) && self.panels.view_shown != Some(want) {
            self.update_channel_view(painter.ctx(), None);
        }
        if want != (ChannelView::Composite, None) {
            if let Some(tex) = &self.panels.view_tex {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(tex.id(), doc_rect, uv, Color32::WHITE);
            }
        }
        self.paint_selected_path(painter, rect);
    }

    // ---- actions (menus, palette, keys) -------------------------------------

    /// Run one of the panels' actions; false for ids they don't own.
    pub(crate) fn run_panel_action(&mut self, id: &str) -> bool {
        match id {
            "panel-layers" => self.set_dock_tab(DockTab::Layers),
            "panel-channels" => self.set_dock_tab(DockTab::Channels),
            "panel-paths" => self.set_dock_tab(DockTab::Paths),
            "channel-rgb" => self.set_channel_view(ChannelView::Composite),
            "channel-red" => self.set_channel_view(ChannelView::Red),
            "channel-green" => self.set_channel_view(ChannelView::Green),
            "channel-blue" => self.set_channel_view(ChannelView::Blue),
            "navigator" => {
                self.prefs.panels.navigator = !self.prefs.panels.navigator;
                self.save_panel_prefs();
            }
            "info-panel" => {
                self.prefs.panels.info = !self.prefs.panels.info;
                self.save_panel_prefs();
            }
            "histogram-panel" => {
                self.prefs.panels.histogram = !self.prefs.panels.histogram;
                self.save_panel_prefs();
            }
            "make-work-path" => {
                self.run(&lumenply_core::path_ops::SelectionToWorkPath { tolerance: 2.0 });
                if self.editor.doc().work_path.is_some() {
                    self.panels.path_sel = Some(PathRow::Work);
                }
            }
            _ => return false,
        }
        true
    }

    /// Why a panel action can't run: `Some(reason)`, `Some(None)` when it
    /// can, `None` for ids the panels don't own.
    pub(crate) fn panel_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        Some(match id {
            "make-work-path" if self.editor.doc().selection.is_none() => Some("Make a selection first"),
            "panel-layers" | "panel-channels" | "panel-paths" | "channel-rgb" | "channel-red"
            | "channel-green" | "channel-blue" | "navigator" | "info-panel" | "histogram-panel"
            | "make-work-path" => None,
            _ => return None,
        })
    }

    /// Cmd+2 RGB, Cmd+3/4/5 red, green, blue, as in Photoshop.
    pub(crate) fn panel_action_keys(&self, ctx: &egui::Context, id: &str) -> Option<String> {
        let key = match id {
            "channel-rgb" => Key::Num2,
            "channel-red" => Key::Num3,
            "channel-green" => Key::Num4,
            "channel-blue" => Key::Num5,
            _ => return None,
        };
        Some(shortcut_text(ctx, egui::Modifiers::COMMAND, key))
    }

    /// The channel keys, and Cmd+, for Preferences as on every Mac app
    /// (called from `App::shortcuts`).
    pub(crate) fn panel_keys(&mut self, ctx: &egui::Context) {
        let mut fired = None;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::COMMAND, Key::Comma) {
                fired = Some("prefs");
            }
            for (k, id) in [
                (Key::Num2, "channel-rgb"),
                (Key::Num3, "channel-red"),
                (Key::Num4, "channel-green"),
                (Key::Num5, "channel-blue"),
            ] {
                if i.consume_key(egui::Modifiers::COMMAND, k) {
                    fired = Some(id);
                }
            }
        });
        if let Some(id) = fired {
            self.run_menu_action(id);
        }
    }

    /// Window menu: the dock's panels and the floating ones.
    pub(crate) fn window_menu(&mut self, ui: &mut egui::Ui) {
        for tab in DockTab::ALL {
            let on = self.prefs.panels.tab == tab;
            self.act_check(ui, tab.name(), tab.action(), on);
        }
        menu_separator(ui);
        let (nav, info) = (self.prefs.panels.navigator, self.prefs.panels.info);
        self.act_check(ui, "Navigator", "navigator", nav);
        self.act_check(ui, "Info", "info-panel", info);
        let hist = self.prefs.panels.histogram;
        self.act_check(ui, "Histogram", "histogram-panel", hist);
        let actions = self.actions.shown;
        self.act_check(ui, "Actions", crate::actions_panel::ACTIONS_PANEL, actions);
        menu_separator(ui);
        let shown = !self.prefs.history_collapsed;
        self.act_check(ui, "History strip", "toggle-history", shown);
        // The open documents, as at the foot of Photoshop's Window menu:
        // every one is reachable even when the tab strip can't show them all.
        let docs = self.tab_infos();
        if !docs.is_empty() {
            menu_separator(ui);
            let mut pick = None;
            for (i, (name, unsaved)) in docs.iter().enumerate() {
                let label = if *unsaved {
                    format!("{name} •")
                } else {
                    name.clone()
                };
                if menu_check(ui, i == self.cur_tab, &label, "").clicked() {
                    pick = Some(i);
                }
            }
            if let Some(i) = pick {
                self.switch_tab(i);
                ui.close_menu();
            }
        }
    }

    // ---- debug tokens ------------------------------------------------------------

    /// `--screenshot-do` tokens for the panels (`panels:...`):
    /// `panels:tab=layers|channels|paths`, `panels:view=rgb|red|green|blue`,
    /// `panels:view=alpha:N`, `panels:overlay=N`, `panels:navigator`,
    /// `panels:info` (both switch the panel on), `panels:save-channel=Name`
    /// (the current selection as an alpha channel), `panels:save-path=Name`
    /// (the work path into the Paths list), `panels:path=work|N` (pick a
    /// path row), `panels:rename-channel=N` (open the inline rename box),
    /// `panels:zoom=P` (percent, about the canvas centre).
    pub(crate) fn debug_panels(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("panels:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let num = arg.trim_start_matches("alpha:").parse::<usize>().ok();
        match verb {
            "tab" => {
                let t = DockTab::ALL
                    .into_iter()
                    .find(|t| t.name().eq_ignore_ascii_case(arg));
                if let Some(t) = t {
                    self.set_dock_tab(t);
                }
            }
            "view" => {
                let v = match arg {
                    "red" => ChannelView::Red,
                    "green" => ChannelView::Green,
                    "blue" => ChannelView::Blue,
                    _ => num.map_or(ChannelView::Composite, ChannelView::Alpha),
                };
                self.set_channel_view(v);
            }
            "overlay" => self.panels.overlay = num,
            "navigator" => self.prefs.panels.navigator = true,
            "info" => self.prefs.panels.info = true,
            "save-channel" => self.run(&lumenply_core::channels::SaveSelection { name: arg.into() }),
            "save-path" => self.run(&SaveWorkPath { name: arg.into() }),
            "path" => {
                self.panels.path_sel = match arg {
                    "work" => Some(PathRow::Work),
                    _ => num.map(PathRow::Saved),
                }
            }
            "rename-channel" => {
                let doc = self.editor.doc();
                self.panels.channel_renaming =
                    num.and_then(|i| doc.saved_selections.get(i).map(|s| (i, s.name.clone())));
            }
            "zoom" => {
                if let (Ok(pct), Some(r)) = (arg.parse::<f32>(), self.panels.canvas_rect) {
                    self.zoom_at(r, r.center(), (pct / 100.0) / self.zoom);
                } else if let Ok(pct) = arg.parse::<f32>() {
                    // Before the first frame there is no canvas yet.
                    self.zoom = (pct / 100.0).clamp(0.05, 32.0);
                }
            }
            _ => return false,
        }
        let _ = ctx;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    #[test]
    fn a_channel_shows_its_encoded_value_over_white() {
        let red = Rgba::from_straight(1.0, 0.0, 0.0, 1.0);
        assert_eq!(
            (channel_gray(red, 0), channel_gray(red, 1), channel_gray(red, 2)),
            (255, 0, 0)
        );
        // Linear 0.2159 is sRGB 128: channels show gamma-encoded values.
        let mid = Rgba::from_straight(0.2159, 0.2159, 0.2159, 1.0);
        assert_eq!(channel_gray(mid, 1), 128);
        // Transparent shows white; half-covered black is half gray.
        assert_eq!(channel_gray(Rgba::TRANSPARENT, 0), 255);
        assert_eq!(channel_gray(Rgba::from_straight(0.0, 0.0, 0.0, 0.5), 2), 127);
    }

    #[test]
    fn the_overlay_tints_unselected_areas_half_red() {
        assert_eq!(overlay_tint(1.0).a(), 0);
        assert_eq!(
            overlay_tint(0.0),
            Color32::from_rgba_premultiplied(128, 0, 0, 128)
        );
        assert_eq!(overlay_tint(0.5).a(), 64);
        assert_eq!(
            blend_over(Color32::from_gray(200), overlay_tint(0.0)),
            Color32::from_rgb(228, 100, 100)
        );
    }

    #[test]
    fn view_images_show_one_channel_or_a_mask() {
        let mut doc = Document::new(4, 2);
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 2, 2)));
        let mut ed = Editor::new(doc);
        ed.execute(&lumenply_core::channels::SaveSelection { name: "Left".into() })
            .unwrap();
        let mut flat = Raster::new(4, 2);
        flat.set(1, 0, Rgba::from_straight(1.0, 0.2159, 0.0, 1.0));
        let doc = ed.doc();
        let all = doc.canvas();
        assert!(view_image(doc, &flat, ChannelView::Composite, None, all).is_none());
        let g = view_image(doc, &flat, ChannelView::Green, None, all).unwrap();
        assert_eq!(g.pixels[1], Color32::from_gray(128));
        assert_eq!(g.pixels[0], Color32::WHITE, "transparent shows white");
        let a = view_image(doc, &flat, ChannelView::Alpha(0), None, all).unwrap();
        assert_eq!((a.pixels[0], a.pixels[3]), (Color32::WHITE, Color32::BLACK));
        // The overlay alone tints only the unselected right half.
        let o = view_image(doc, &flat, ChannelView::Composite, Some(0), all).unwrap();
        assert_eq!((o.pixels[1].a(), o.pixels[2].a()), (0, 128));
        // A patch covers just its area.
        let p = view_image(doc, &flat, ChannelView::Red, None, Rect::new(1, 0, 2, 1)).unwrap();
        assert_eq!(p.size, [2, 1]);
        assert_eq!(p.pixels[0], Color32::from_gray(255));
        assert!(view_image(doc, &flat, ChannelView::Alpha(3), None, all).is_none());
    }

    #[test]
    fn panel_prefs_default_for_older_prefs_files() {
        let p: session::Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p.panels, PanelPrefs::default());
        assert_eq!(p.panels.tab, DockTab::Layers);
        let p: session::Prefs = serde_json::from_str(r#"{"panels":{"tab":"Channels"}}"#).unwrap();
        assert_eq!(
            (p.panels.tab, p.panels.navigator, p.panels.info),
            (DockTab::Channels, false, false)
        );
    }

    #[test]
    fn channel_keys_and_tabs_run_through_the_registry() {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(blank(32, 16), None);
        assert_eq!(app.action_block("channel-red"), None);
        app.run_menu_action("channel-red");
        assert_eq!(app.panels.view, ChannelView::Red);
        app.run_menu_action("channel-rgb");
        assert_eq!(app.panels.view, ChannelView::Composite);
        app.run_menu_action("panel-paths");
        assert_eq!(app.prefs.panels.tab, DockTab::Paths);
        assert_eq!(app.action_block("make-work-path"), Some("Make a selection first"));
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(2, 2, 8, 6))),
        });
        app.run_menu_action("make-work-path");
        assert_eq!(app.panels.path_sel, Some(PathRow::Work));
        let n = app.editor.doc().work_path.as_ref().unwrap().subpaths[0]
            .nodes
            .len();
        assert_eq!(n, 4);
        app.run_menu_action("panel-layers");
        assert_eq!(app.prefs.panels.tab, DockTab::Layers);
    }
}
