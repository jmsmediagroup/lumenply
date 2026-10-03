//! Image ▸ Adjustments: Photoshop's destructive adjustments — every
//! adjustment-layer kind applied to the pixels (Levels, Curves, ...) plus
//! Shadows/Highlights, Equalize, Desaturate, Replace Color and Match
//! Color — with their dialogs (live canvas preview, Preview toggle, OK as
//! one undo step), Auto Tone, and the submenu that also offers every
//! adjustment layer.
//!
//! The pixel maths is `lumenply_render::adjust_more`; the commands are
//! `lumenply_core::adjust_cmds`.

use super::*;
use lumenply_core::adjust_cmds::{
    ApplyAdjustment, ApplyMatchColor, ApplyReplaceColor, ApplyShadowsHighlights, Desaturate, Equalize,
    ToneZone,
};
use lumenply_render::adjust_more::{self, LabStats, MatchColor, ReplaceColor, ShadowsHighlights};

pub(crate) const ADJ_SH: &str = "adj-shadows-highlights";
pub(crate) const ADJ_EQUALIZE: &str = "adj-equalize";
pub(crate) const ADJ_DESATURATE: &str = "adj-desaturate";
pub(crate) const ADJ_REPLACE: &str = "adj-replace-color";
pub(crate) const ADJ_MATCH: &str = "adj-match-color";
pub(crate) const AUTO_TONE: &str = "auto-tone";
/// Prefix of the destructive forms of the adjustment-layer kinds
/// (`adjd-levels`, `adjd-hue-saturation`, ...).
pub(crate) const ADJD: &str = "adjd-";

/// Photoshop's Image ▸ Adjustments order for the kinds that also exist as
/// adjustment layers; kinds not listed follow at the end.
const PS_ORDER: [&str; 15] = [
    "Brightness/Contrast",
    "Levels",
    "Curves",
    "Exposure",
    "Vibrance",
    "Hue/Saturation",
    "Color Balance",
    "Black & White",
    "Photo Filter",
    "Channel Mixer",
    "Invert",
    "Posterize",
    "Threshold",
    "Gradient Map",
    "Selective Color",
];

/// The action id of an adjustment kind's destructive form.
pub(crate) fn adjd_id(name: &str) -> String {
    let mut id = String::from(ADJD);
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_end_matches('-').to_string()
}

/// The adjustment layer presets in Photoshop's menu order.
fn ps_ordered_presets() -> Vec<(&'static str, Adjustment)> {
    let mut v = adjustment_presets();
    v.sort_by_key(|(n, _)| PS_ORDER.iter().position(|o| o == n).unwrap_or(PS_ORDER.len()));
    v
}

/// The starting settings for a destructive-adjustment action id.
fn adjd_preset(id: &str) -> Option<Adjustment> {
    adjustment_presets()
        .into_iter()
        .find(|(n, _)| adjd_id(n) == id)
        .map(|(_, a)| a)
}

/// The same dialog: the same variant, and for applied adjustment-layer
/// kinds the same kind.
fn same_kind(a: &AdjxKind, b: &AdjxKind) -> bool {
    match (a, b) {
        (AdjxKind::Adjust(x), AdjxKind::Adjust(y)) => x.name() == y.name(),
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

fn is_ours(id: &str) -> bool {
    matches!(
        id,
        ADJ_SH | ADJ_EQUALIZE | ADJ_DESATURATE | ADJ_REPLACE | ADJ_MATCH
    ) || id.starts_with(ADJD)
}

/// Where Match Color takes its colours from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SourcePick {
    /// None: no source (only the sliders act); Some(0): this document;
    /// Some(i): the i-th other open document.
    doc: Option<usize>,
    /// A layer of that document, or None for the merged image.
    layer: Option<LayerId>,
}

#[derive(Clone, Debug)]
pub(crate) enum AdjxKind {
    /// An adjustment-layer kind applied to the pixels (Levels, Curves...).
    Adjust(Adjustment),
    ShadowsHighlights(ShadowsHighlights),
    /// With a selection: equalise the entire layer based on it.
    Equalize {
        entire: bool,
    },
    /// The settings and the colour being replaced, as an sRGB swatch.
    ReplaceColor {
        rc: ReplaceColor,
        swatch: [f32; 3],
        /// The thumbnail shows the image instead of the mask.
        show_image: bool,
    },
    MatchColor {
        m: MatchColor,
        from_selection: bool,
        pick: SourcePick,
    },
}

/// An open Image ▸ Adjustments dialog.
pub(crate) struct AdjxState {
    kind: AdjxKind,
    layer: LayerId,
    preview: bool,
    /// What the canvas shows now (`{kind:?}`); None = the original.
    shown: Option<String>,
    last_preview: Option<std::time::Instant>,
    thumb: Option<egui::TextureHandle>,
    thumb_key: String,
    /// Match Color: the source statistics and what they were computed for.
    source_stats: Option<(SourcePick, Option<LabStats>)>,
    /// Set by the debug hook: press OK on the next frame.
    debug_ok: bool,
}

impl AdjxKind {
    fn title(&self) -> &'static str {
        match self {
            AdjxKind::Adjust(a) => a.name(),
            AdjxKind::ShadowsHighlights(_) => "Shadows/Highlights",
            AdjxKind::Equalize { .. } => "Equalize",
            AdjxKind::ReplaceColor { .. } => "Replace Color",
            AdjxKind::MatchColor { .. } => "Match Color",
        }
    }
}

fn pct(ui: &mut egui::Ui, label: &str, v: &mut f32, range: RangeInclusive<f32>, sfx: &str) -> bool {
    let before = *v;
    slider_row_scaled(ui, label, v, range, 100.0, sfx);
    *v != before
}

fn px_row(ui: &mut egui::Ui, label: &str, v: &mut f32) -> bool {
    let before = *v;
    let o = RowOpts {
        log: true,
        int: true,
        ..RowOpts::default()
    };
    slider_row_ex(ui, label, v, 1.0..=500.0, " px", o);
    *v != before
}

/// Shadows/Highlights starting points: Photoshop's defaults, then
/// typical recoveries of a backlit or high-contrast photo.
pub(crate) fn sh_presets() -> Vec<(&'static str, ShadowsHighlights)> {
    let z = |amount: f32, tone: f32, radius: f32| ToneZone { amount, tone, radius };
    let sh = |s: ToneZone, h: ToneZone, color: f32, midtone: f32| ShadowsHighlights {
        shadows: s,
        highlights: h,
        color,
        midtone,
    };
    vec![
        ("Default", ShadowsHighlights::default()),
        (
            "Lift shadows",
            sh(z(0.6, 0.55, 40.0), z(0.0, 0.5, 30.0), 0.25, 0.0),
        ),
        (
            "Recover highlights",
            sh(z(0.0, 0.5, 30.0), z(0.5, 0.45, 40.0), 0.15, 0.0),
        ),
        (
            "Backlit subject",
            sh(z(0.75, 0.6, 60.0), z(0.3, 0.4, 60.0), 0.3, 0.1),
        ),
        (
            "Balanced tones",
            sh(z(0.4, 0.5, 80.0), z(0.4, 0.5, 80.0), 0.2, 0.15),
        ),
        (
            "Detail punch",
            sh(z(0.5, 0.45, 20.0), z(0.45, 0.45, 20.0), 0.3, 0.35),
        ),
    ]
}

/// The document pixel under `at`, a point `size` across the Replace Color
/// thumbnail of a `doc` sized image (the thumbnail shows all of it).
fn thumb_to_doc(at: egui::Vec2, size: egui::Vec2, doc: (u32, u32)) -> (i32, i32) {
    let fx = (at.x / size.x.max(1.0)).clamp(0.0, 1.0);
    let fy = (at.y / size.y.max(1.0)).clamp(0.0, 1.0);
    let x = (fx * doc.0 as f32).floor() as i32;
    let y = (fy * doc.1 as f32).floor() as i32;
    (x.min(doc.0 as i32 - 1), y.min(doc.1 as i32 - 1))
}

fn zone_rows(ui: &mut egui::Ui, z: &mut ToneZone) {
    pct(ui, "Amount", &mut z.amount, 0.0..=1.0, " %");
    pct(ui, "Tone", &mut z.tone, 0.0..=1.0, " %");
    px_row(ui, "Radius", &mut z.radius);
}

impl App {
    /// Why one of this module's actions can't run; `None` for ids it
    /// doesn't own (see [`App::action_block`]).
    pub(crate) fn adjx_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        if !is_ours(id) {
            return None;
        }
        let layer = self.active_layer();
        Some(match layer {
            None => Some("Select a pixel layer first"),
            Some(l) if l.smart_layer().is_some() => Some("Rasterize the smart object first"),
            Some(_) if !self.active_is_pixel() => Some("Select a pixel layer first"),
            Some(_) => self.lock_block(crate::layer_actions::LockNeed::Paint),
        })
    }

    /// Run one of this module's actions; false for ids it doesn't own.
    pub(crate) fn run_adjx_action(&mut self, id: &str) -> bool {
        if id == AUTO_TONE {
            self.auto_tone();
            return true;
        }
        let Some(layer) = self.active else {
            return is_ours(id);
        };
        let kind = match id {
            id if id.starts_with(ADJD) => match adjd_preset(id) {
                // Invert has no settings: it applies at once, as Cmd+I does.
                Some(Adjustment::Invert) => {
                    self.run(&ApplyAdjustment {
                        layer,
                        adjustment: Adjustment::Invert,
                    });
                    return true;
                }
                Some(a) => AdjxKind::Adjust(a),
                None => {
                    self.status = format!("Unknown adjustment '{id}'");
                    return true;
                }
            },
            ADJ_SH => AdjxKind::ShadowsHighlights(ShadowsHighlights::default()),
            ADJ_DESATURATE => {
                self.run(&Desaturate { layer });
                return true;
            }
            ADJ_EQUALIZE => {
                // Photoshop asks only when there is a selection.
                if self.editor.doc().selection.is_none() {
                    self.run(&Equalize {
                        layer,
                        entire_layer: false,
                    });
                    return true;
                }
                AdjxKind::Equalize { entire: false }
            }
            ADJ_REPLACE => {
                let swatch = self.brush_rgb;
                AdjxKind::ReplaceColor {
                    rc: ReplaceColor {
                        color: swatch.map(lumenply_io::srgb_to_linear_f),
                        added: Vec::new(),
                        removed: Vec::new(),
                        fuzziness: 0.25,
                        hue: 0.0,
                        saturation: 0.0,
                        lightness: 0.0,
                    },
                    swatch,
                    show_image: false,
                }
            }
            ADJ_MATCH => AdjxKind::MatchColor {
                m: MatchColor::default(),
                from_selection: true,
                pick: SourcePick {
                    doc: None,
                    layer: None,
                },
            },
            _ => return false,
        };
        // As in Photoshop, a dialog opens with the settings it was last
        // applied with (the Match Color source is picked afresh).
        let kind = match self.adjx_last.iter().find(|k| same_kind(k, &kind)) {
            Some(AdjxKind::MatchColor {
                m, from_selection, ..
            }) => AdjxKind::MatchColor {
                m: *m,
                from_selection: *from_selection,
                pick: SourcePick {
                    doc: None,
                    layer: None,
                },
            },
            Some(AdjxKind::ReplaceColor { rc, swatch, .. }) => AdjxKind::ReplaceColor {
                rc: rc.clone(),
                swatch: *swatch,
                show_image: false,
            },
            Some(k) => k.clone(),
            None => kind,
        };
        self.adjx = Some(Box::new(AdjxState {
            kind,
            layer,
            preview: true,
            shown: None,
            last_preview: None,
            thumb: None,
            thumb_key: String::new(),
            source_stats: None,
            debug_ok: false,
        }));
        true
    }

    /// Image ▸ Adjustments: every adjustment applied to the pixels, as in
    /// Photoshop (left), the ones that exist only that way (middle), and
    /// the adjustment layers, the non-destructive way to the same looks
    /// (right).
    pub(crate) fn adjustments_menu(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            crate::layer_actions::menu_column(ui, "adjx-left", |ui| {
                section_title(ui, "APPLY TO PIXELS");
                for (name, adj) in ps_ordered_presets() {
                    let label = if matches!(adj, Adjustment::Invert) {
                        name.to_string()
                    } else {
                        format!("{name}...")
                    };
                    let hint = format!(
                        "Changes the layer's pixels; the {name} adjustment layer on the right \
                         stays editable instead"
                    );
                    self.act_hint(ui, &label, &adjd_id(name), &hint);
                }
            });
            ui.add_space(6.0);
            // Three short columns rather than two long ones, so the menu
            // fits a 600 px window (egui menus can't scroll).
            crate::layer_actions::menu_column(ui, "adjx-mid", |ui| {
                section_title(ui, "MORE");
                self.act(ui, "Shadows/Highlights...", ADJ_SH);
                self.act(ui, "Desaturate", ADJ_DESATURATE);
                self.act(ui, "Match Color...", ADJ_MATCH);
                self.act(ui, "Replace Color...", ADJ_REPLACE);
                self.act(ui, "Equalize", ADJ_EQUALIZE);
            });
            ui.add_space(6.0);
            crate::layer_actions::menu_column(ui, "adjx-right", |ui| {
                section_title(ui, "AS ADJUSTMENT LAYER");
                for (name, adj) in ps_ordered_presets() {
                    let r = menu_item_response(ui, !self.no_doc, name, "");
                    let r = r.on_hover_text(format!(
                        "Adds a {name} adjustment layer: editable any time in Properties, \
                         the pixels below stay untouched"
                    ));
                    if r.clicked() {
                        ui.close_menu();
                        self.add_adjustment(adj);
                    }
                }
            });
        });
    }

    /// [`App::act`] with a hover hint while the action can run.
    fn act_hint(&mut self, ui: &mut egui::Ui, label: &str, id: &str, hint: &str) {
        let ctx = ui.ctx().clone();
        let block = self.action_block(id);
        let keys = self.action_keys(&ctx, id);
        let r = menu_item_response(ui, block.is_none(), label, &keys);
        let r = match block {
            Some(why) => r.on_disabled_hover_text(why),
            None => r.on_hover_text(hint),
        };
        if r.clicked() {
            ui.close_menu();
            self.run_action(&ctx, id);
        }
    }

    /// Image ▸ Auto Tone: one black and one white point shared by R, G and
    /// B (from their pooled histograms, clipping 0.1% at each end), as a
    /// Levels adjustment layer. Unlike Auto Color it never shifts the
    /// colour balance; unlike Auto Contrast (luminance) no single channel
    /// clips more than the 0.1%.
    pub(crate) fn auto_tone(&mut self) {
        let Some(flat) = &self.last_flat else {
            self.status = "Auto tone: nothing composited yet".into();
            return;
        };
        let h = histogram::channel_histograms(flat);
        let mut pooled = [0u32; histogram::BINS];
        for (i, p) in pooled.iter_mut().enumerate() {
            *p = h[0][i] + h[1][i] + h[2][i];
        }
        match auto_tone_levels(&pooled) {
            Some((in_black, in_white)) => {
                let before = self.editor.history().len();
                self.add_adjustment(Adjustment::Levels {
                    in_black,
                    in_white,
                    gamma: 1.0,
                    out_black: 0.0,
                    out_white: 1.0,
                    channels: Default::default(),
                });
                self.name_auto_layer(before, "Auto Tone");
                self.status = format!(
                    "Auto tone: black {:.2}, white {:.2} for all channels (as an adjustment layer)",
                    in_black, in_white
                );
            }
            None => self.status = "Auto tone: the image already spans the full range".into(),
        }
    }

    /// Name the Levels layer an auto command just added after the command
    /// ("Auto Tone"), in the same undo step, so the layer says where it
    /// came from. `before`: the history length before the command.
    pub(crate) fn name_auto_layer(&mut self, before: usize, name: &str) {
        let Some(layer) = self.active else {
            return;
        };
        if self.editor.history().len() != before + 1 {
            return;
        }
        self.run(&RenameLayer {
            layer,
            name: name.into(),
        });
        if self.editor.history().len() == before + 2 {
            self.editor.squash_newest(2, name);
        }
    }

    fn adjx_command(&self, st: &AdjxState) -> Option<Box<dyn Command>> {
        let layer = st.layer;
        Some(match &st.kind {
            AdjxKind::Adjust(a) => Box::new(ApplyAdjustment {
                layer,
                adjustment: a.clone(),
            }),
            AdjxKind::ShadowsHighlights(p) => Box::new(ApplyShadowsHighlights { layer, params: *p }),
            AdjxKind::Equalize { entire } => Box::new(Equalize {
                layer,
                entire_layer: *entire,
            }),
            AdjxKind::ReplaceColor { rc, .. } => Box::new(ApplyReplaceColor {
                layer,
                params: rc.clone(),
            }),
            AdjxKind::MatchColor {
                m, from_selection, ..
            } => {
                let source = st.source_stats.and_then(|(_, s)| s);
                Box::new(ApplyMatchColor {
                    layer,
                    params: MatchColor { source, ..*m },
                    target_from_selection: *from_selection,
                })
            }
        })
    }

    /// The document a Match Color source index names.
    fn source_doc(&self, i: usize) -> Option<&Document> {
        if i == 0 {
            Some(self.editor.doc())
        } else {
            self.tabs.get(i - 1).map(|t| t.editor.doc())
        }
    }

    fn source_stats(&self, pick: SourcePick) -> Option<LabStats> {
        let doc = self.source_doc(pick.doc?)?;
        let canvas = doc.canvas();
        match pick.layer {
            None => adjust_more::raster_lab_stats(&lumenply_render::composite_raster(doc)),
            Some(id) => {
                let store = doc.layer(id)?.raster_store()?;
                adjust_more::store_lab_stats(store, |x, y| canvas.contains(x, y) as u8 as f32)
            }
        }
    }

    /// Replace Color's thumbnail: the soft mask (white = replaced) or the
    /// layer, nearest-sampled to at most 220 px wide.
    fn replace_thumb(&self, rc: &ReplaceColor, show_image: bool, layer: LayerId) -> egui::ColorImage {
        let doc = self.editor.doc();
        let (w, h) = (doc.width.max(1), doc.height.max(1));
        let f = (w as f32 / 220.0).max(h as f32 / 150.0).max(1.0);
        let (tw, th) = (((w as f32 / f) as usize).max(1), ((h as f32 / f) as usize).max(1));
        let store = doc.layer(layer).and_then(|l| l.pixels());
        let mut px = Vec::with_capacity(tw * th);
        for ty in 0..th {
            for tx in 0..tw {
                let (x, y) = (((tx as f32 + 0.5) * f) as i32, ((ty as f32 + 0.5) * f) as i32);
                let p = store.map_or(lumenply_tiles::Rgba::TRANSPARENT, |s| s.get_pixel(x, y));
                px.push(if show_image {
                    let [r, g, b, _] = p.to_straight();
                    Color32::from_rgb(
                        lumenply_io::linear_to_srgb(r),
                        lumenply_io::linear_to_srgb(g),
                        lumenply_io::linear_to_srgb(b),
                    )
                } else {
                    let v = (rc.weight(p) * 255.0).round() as u8;
                    Color32::from_gray(v)
                });
            }
        }
        egui::ColorImage {
            size: [tw, th],
            pixels: px,
        }
    }

    /// Replace Color's eyedropper at document pixel `(x, y)`, from a click
    /// on the canvas or on the dialog's thumbnail.
    fn replace_sample(&mut self, ctx: &egui::Context, st: &mut AdjxState, (x, y): (i32, i32)) {
        let AdjxKind::ReplaceColor { rc, swatch, .. } = &mut st.kind else {
            return;
        };
        let p = self
            .editor
            .doc()
            .layer(st.layer)
            .and_then(|l| l.pixels())
            .map(|s| s.get_pixel(x, y))
            .filter(|p| p.a > 0.0);
        let Some(p) = p else {
            return;
        };
        // Photoshop's eyedroppers: Shift adds to the sample, Alt
        // subtracts from it, a plain click starts over.
        let [r, g, b, _] = p.to_straight();
        let m = ctx.input(|i| i.modifiers);
        let hex = color_picker::format_hex([r, g, b].map(lumenply_doc::adjust::srgb_encode));
        if m.shift {
            rc.added.push([r, g, b]);
            self.status = format!("Replace Color: added {hex} to the sample");
        } else if m.alt {
            rc.removed.push([r, g, b]);
            self.status = format!("Replace Color: took {hex} out of the sample");
        } else {
            rc.color = [r, g, b];
            rc.added.clear();
            rc.removed.clear();
            *swatch = [r, g, b].map(lumenply_doc::adjust::srgb_encode);
            self.status = format!("Replace Color: sampled {hex}");
        }
    }

    /// The open Image ▸ Adjustments dialog, if any: drawn over the editor,
    /// previewing on the canvas.
    pub(crate) fn adjx_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.adjx.take() else {
            return;
        };
        // The layer went away (undo from the history strip, a closed tab).
        if self
            .editor
            .doc()
            .layer(st.layer)
            .and_then(|l| l.pixels())
            .is_none()
        {
            self.mark(None);
            return;
        }
        let title = st.kind.title();
        let (enter, esc) = if ctx.wants_keyboard_input() {
            (false, false)
        } else {
            ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)))
        };
        // A backdrop that swallows clicks meant for the canvas; with
        // Replace Color a click on the image samples the colour instead.
        let sampling = matches!(st.kind, AdjxKind::ReplaceColor { .. });
        let backdrop = egui::Area::new(egui::Id::new("adjx-backdrop"))
            .order(egui::Order::Foreground)
            .sense(BACKDROP_SENSE)
            .fixed_pos(ctx.screen_rect().min)
            .show(ctx, |ui| {
                let (_, r) = ui.allocate_exact_size(ctx.screen_rect().size(), BACKDROP_SENSE);
                r
            });
        ctx.move_to_top(backdrop.response.layer_id);
        let canvas_rect = self.panels.canvas_rect;
        let to_doc = |p: Pos2| -> Option<(i32, i32)> {
            let r = canvas_rect?;
            if !r.contains(p) {
                return None;
            }
            let d = (p - (r.min + self.pan)) / self.zoom;
            Some((d.x.floor() as i32, d.y.floor() as i32))
        };
        let mut sampled: Option<(i32, i32)> = None;
        if sampling {
            let r = &backdrop.inner;
            if r.hovered() && r.hover_pos().and_then(to_doc).is_some() {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if r.clicked() {
                sampled = r.interact_pointer_pos().and_then(to_doc);
            }
        }
        if let Some(at) = sampled {
            self.replace_sample(ctx, &mut st, at);
        }
        // Match Color's source statistics follow the source picked.
        if let AdjxKind::MatchColor { pick, .. } = &st.kind {
            if st.source_stats.map(|(p, _)| p) != Some(*pick) {
                st.source_stats = Some((*pick, self.source_stats(*pick)));
            }
        }

        let mut ok = std::mem::take(&mut st.debug_ok);
        let mut cancel = false;
        let selection = self.editor.doc().selection.is_some();
        let doc_titles: Vec<String> = std::iter::once(format!("{} (this document)", self.tab_title()))
            .chain(self.tabs.iter().map(|t| t.title()))
            .collect();
        let default_pos = canvas_rect.map_or(egui::pos2(80.0, 90.0), |r| r.min + egui::vec2(16.0, 16.0));
        let thumb_tex = st.thumb.clone();
        let mut thumb_pick: Option<(i32, i32)> = None;
        let doc_size = (self.editor.doc().width, self.editor.doc().height);
        let shown = egui::Window::new(title)
            .id(egui::Id::new("adjx-dialog"))
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .order(egui::Order::Foreground)
            .pivot(Align2::LEFT_TOP)
            .default_pos(default_pos)
            .constrain(true)
            .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| {
                crate::dialogs::titled(ui, title, |ui| {
                    raise_controls(ui);
                    ui.set_min_width(300.0);
                    ui.set_max_width(320.0);
                    // Tighter rows in short windows (the tallest dialog
                    // still fits 900×600).
                    let short = ui.ctx().screen_rect().height() < 700.0;
                    ui.spacing_mut().item_spacing.y = if short { 2.0 } else { 5.0 };
                    match &mut st.kind {
                        AdjxKind::Adjust(a) => {
                            self.adjustment_controls(ui, st.layer, a);
                            crate::dialogs::note(
                                ui,
                                "Changes the layer's pixels; Layer > New adjustment layer keeps \
                                 it editable instead.",
                            );
                        }
                        AdjxKind::ShadowsHighlights(p) => {
                            ui.horizontal(|ui| {
                                row_label(ui, "Preset", LABEL_W);
                                let presets = sh_presets();
                                let cur = presets.iter().find(|(_, q)| q == p).map_or("Custom", |(n, _)| *n);
                                let r = egui::ComboBox::from_id_salt("adjx-sh-preset")
                                    .selected_text(cur)
                                    .width(190.0)
                                    .show_ui(ui, |ui| {
                                        popup_style(ui);
                                        for (n, q) in &presets {
                                            if ui.selectable_label(q == p, *n).clicked() {
                                                *p = *q;
                                            }
                                        }
                                    });
                                a11y_name(&r.response, "Shadows/Highlights preset");
                            });
                            section_title(ui, "SHADOWS");
                            zone_rows(ui, &mut p.shadows);
                            section_title(ui, "HIGHLIGHTS");
                            zone_rows(ui, &mut p.highlights);
                            section_title(ui, "ADJUSTMENTS");
                            slider_row_scaled(ui, "Color", &mut p.color, -1.0..=1.0, 100.0, "");
                            slider_row_scaled(ui, "Midtone", &mut p.midtone, -1.0..=1.0, 100.0, "");
                        }
                        AdjxKind::Equalize { entire } => {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    if ui.radio(!*entire, "Equalize selected area only").clicked() {
                                        *entire = false;
                                    }
                                    if ui
                                        .radio(*entire, "Equalize entire layer based on selected area")
                                        .clicked()
                                    {
                                        *entire = true;
                                    }
                                });
                            });
                            crate::dialogs::note(
                                ui,
                                "Spreads the selection's tones evenly from black to white; \
                                 hues are kept.",
                            );
                        }
                        AdjxKind::ReplaceColor {
                            rc,
                            swatch,
                            show_image,
                        } => {
                            ui.horizontal(|ui| {
                                row_label(ui, "Colour", LABEL_W);
                                let r = color_picker::color_edit_button_rgb(ui, swatch);
                                if r.changed() {
                                    rc.color = swatch.map(lumenply_io::srgb_to_linear_f);
                                }
                                ui.label(RichText::new("or click the image").small().color(MUTED));
                            });
                            let (na, nr) = (rc.added.len(), rc.removed.len());
                            ui.horizontal(|ui| {
                                row_label(ui, "", LABEL_W);
                                let text = if na + nr == 0 {
                                    "Shift-click adds colours, Alt-click removes".to_string()
                                } else {
                                    format!("+{na} added, −{nr} removed")
                                };
                                ui.label(RichText::new(text).small().color(MUTED));
                                if na + nr > 0 && ui.small_button("Reset").clicked() {
                                    rc.added.clear();
                                    rc.removed.clear();
                                }
                            });
                            pct(ui, "Fuzziness", &mut rc.fuzziness, 0.01..=1.0, " %");
                            if let Some(tex) = &thumb_tex {
                                ui.vertical_centered(|ui| {
                                    let size = tex.size_vec2();
                                    // As in Photoshop, the thumbnail takes
                                    // eyedropper clicks too: the canvas may
                                    // be under this dialog.
                                    let r = ui
                                        .add(egui::Image::new((tex.id(), size)).sense(Sense::click()))
                                        .on_hover_cursor(egui::CursorIcon::Crosshair)
                                        .on_hover_text("Click to sample a colour (Shift adds, Alt removes)");
                                    a11y_name(&r, "Replace Color preview");
                                    if r.clicked() {
                                        thumb_pick = r
                                            .interact_pointer_pos()
                                            .map(|p| thumb_to_doc(p - r.rect.min, r.rect.size(), doc_size));
                                    }
                                });
                            }
                            ui.horizontal(|ui| {
                                row_label(ui, "", LABEL_W);
                                segmented(ui, show_image, &[(false, "Selection"), (true, "Image")]);
                            });
                            section_title(ui, "REPLACEMENT");
                            slider_row_ex(ui, "Hue", &mut rc.hue, -180.0..=180.0, "°", RowOpts::default());
                            slider_row_scaled(ui, "Saturation", &mut rc.saturation, -1.0..=1.0, 100.0, "");
                            slider_row_scaled(ui, "Lightness", &mut rc.lightness, -1.0..=1.0, 100.0, "");
                        }
                        AdjxKind::MatchColor {
                            m,
                            from_selection,
                            pick,
                        } => {
                            section_title(ui, "IMAGE OPTIONS");
                            let mut lum = m.luminance;
                            slider_row_scaled(ui, "Luminance", &mut lum, 0.01..=2.0, 100.0, "");
                            m.luminance = lum.clamp(0.01, 2.0);
                            slider_row_scaled(ui, "Intensity", &mut m.intensity, 0.0..=2.0, 100.0, "");
                            slider_row_scaled(ui, "Fade", &mut m.fade, 0.0..=1.0, 100.0, " %");
                            ui.horizontal(|ui| {
                                row_label(ui, "", LABEL_W);
                                check(ui, &mut m.neutralize, "Neutralize");
                            });
                            if selection {
                                ui.horizontal(|ui| {
                                    row_label(ui, "", LABEL_W);
                                    check(ui, from_selection, "Use the selection's colours");
                                });
                            }
                            section_title(ui, "SOURCE");
                            ui.horizontal(|ui| {
                                row_label(ui, "Image", LABEL_W);
                                let cur = pick.doc.map_or("None".to_string(), |i| {
                                    doc_titles.get(i).cloned().unwrap_or_default()
                                });
                                let r = egui::ComboBox::from_id_salt("adjx-source-doc")
                                    .selected_text(cur)
                                    .width(190.0)
                                    .show_ui(ui, |ui| {
                                        popup_style(ui);
                                        if ui.selectable_label(pick.doc.is_none(), "None").clicked() {
                                            *pick = SourcePick {
                                                doc: None,
                                                layer: None,
                                            };
                                        }
                                        for (i, t) in doc_titles.iter().enumerate() {
                                            if ui.selectable_label(pick.doc == Some(i), t).clicked() {
                                                *pick = SourcePick {
                                                    doc: Some(i),
                                                    layer: None,
                                                };
                                            }
                                        }
                                    });
                                a11y_name(&r.response, "Source image");
                            });
                            let layers: Vec<(LayerId, String)> = pick
                                .doc
                                .and_then(|i| self.source_doc(i))
                                .map(|d| {
                                    let mut v = Vec::new();
                                    d.for_each_layer(|l| {
                                        if l.raster_store().is_some() {
                                            v.push((l.id, l.name.clone()));
                                        }
                                    });
                                    v.reverse();
                                    v
                                })
                                .unwrap_or_default();
                            ui.add_enabled_ui(pick.doc.is_some(), |ui| {
                                ui.horizontal(|ui| {
                                    row_label(ui, "Layer", LABEL_W);
                                    let cur = pick
                                        .layer
                                        .and_then(|id| layers.iter().find(|(l, _)| *l == id))
                                        .map_or("Merged".to_string(), |(_, n)| n.clone());
                                    let r = egui::ComboBox::from_id_salt("adjx-source-layer")
                                        .selected_text(cur)
                                        .width(190.0)
                                        .show_ui(ui, |ui| {
                                            popup_style(ui);
                                            if ui.selectable_label(pick.layer.is_none(), "Merged").clicked() {
                                                pick.layer = None;
                                            }
                                            for (id, n) in &layers {
                                                if ui.selectable_label(pick.layer == Some(*id), n).clicked() {
                                                    pick.layer = Some(*id);
                                                }
                                            }
                                        });
                                    a11y_name(&r.response, "Source layer");
                                });
                            });
                            if pick.doc.is_none() {
                                crate::dialogs::note(
                                    ui,
                                    "Pick a source to match its colours, or use the sliders \
                                     and Neutralize alone.",
                                );
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        row_label(ui, "", LABEL_W);
                        check(ui, &mut st.preview, "Preview");
                    });
                    crate::dialogs::footer(ui, |ui| {
                        if ui.add(primary_button("OK")).clicked() || enter {
                            ok = true;
                        }
                        if ui.add(footer_button("Cancel")).clicked() || esc {
                            cancel = true;
                        }
                    });
                })
            });
        if let Some(shown) = shown {
            ctx.move_to_top(shown.response.layer_id);
            // Always just above its backdrop, as dialogs.rs does: never
            // left below it, out of the mouse's reach, by the layer order
            // of an earlier dialog.
            ctx.set_sublayer(backdrop.response.layer_id, shown.response.layer_id);
            ctx.accesskit_node_builder(shown.response.id, |b| {
                b.set_role(egui::accesskit::Role::Dialog);
                b.set_name(title);
            });
        }

        if let Some(at) = thumb_pick {
            self.replace_sample(ctx, &mut st, at);
        }
        // Replace Color's thumbnail follows the mask settings.
        if let AdjxKind::ReplaceColor { rc, show_image, .. } = &st.kind {
            let key = format!("{rc:?}{show_image}");
            if key != st.thumb_key || st.thumb.is_none() {
                let img = self.replace_thumb(rc, *show_image, st.layer);
                upload(
                    &mut st.thumb,
                    ctx,
                    "adjx-thumb",
                    img,
                    egui::TextureOptions::LINEAR,
                );
                st.thumb_key = key;
            }
        }

        if ok {
            if let AdjxKind::Adjust(a) = &st.kind {
                self.record_apply(a);
            }
            if let Some(cmd) = self.adjx_command(&st) {
                let t = std::time::Instant::now();
                self.run(cmd.as_ref());
                self.status = format!("{title}: {} ms", t.elapsed().as_millis());
            }
            self.adjx_last.retain(|k| !same_kind(k, &st.kind));
            self.adjx_last.push(st.kind.clone());
            self.mark(None);
            return;
        }
        if cancel {
            self.mark(None);
            return;
        }
        // Live preview: whenever the settings change, at most every 120 ms
        // while a slider is being dragged.
        let want = st.preview.then(|| format!("{:?}", st.kind));
        // A pending canvas refresh (an edit this frame) would paint over
        // the preview: wait for it, then preview.
        if self.dirty && want.is_some() {
            st.shown = None;
            ctx.request_repaint();
        } else if want != st.shown {
            let dragging = ctx.input(|i| i.pointer.any_down());
            let due = st.last_preview.is_none_or(|t| t.elapsed().as_millis() >= 120);
            if !dragging || due {
                match &want {
                    Some(_) => {
                        if let Some(cmd) = self.adjx_command(&st) {
                            let mut doc = self.editor.doc().clone();
                            if cmd.apply(&mut doc).is_ok() {
                                self.preview(ctx, &doc, None);
                            }
                        }
                    }
                    None => self.mark(None),
                }
                st.shown = want;
                st.last_preview = Some(std::time::Instant::now());
            } else {
                ctx.request_repaint();
            }
        }
        self.adjx = Some(st);
    }

    /// The live tab's title, as the tab strip shows it.
    fn tab_title(&self) -> String {
        self.path
            .as_ref()
            .map(|p| file_name(&p.to_string_lossy()))
            .unwrap_or_else(|| self.untitled.clone())
    }

    /// Debug tokens (`adjx:...`) for `--screenshot-do`:
    /// `adjx:open=sh|equalize|replace|match|adjd-<kind>` opens a dialog on
    /// the active pixel layer (the bottom pixel layer when the active one
    /// isn't); `adjx:hs=H:S:L` and `adjx:levels=IB:IW:GAMMA` (0–255) set
    /// an open Hue/Saturation or Levels dialog;
    /// `adjx:sh=SA:ST:SR:HA:HT:HR:COLOR:MID` sets Shadows/Highlights
    /// (percent, px); `adjx:replace=RRGGBB:FUZZ:HUE:SAT:LIGHT` (percent,
    /// degrees); `adjx:replace-image` shows the image thumbnail;
    /// `adjx:pick=X:Y` samples Replace Color at a canvas pixel
    /// (`adjx:pick-add=X:Y`, `adjx:pick-remove=X:Y` as Shift/Alt-clicks);
    /// `adjx:match=LUM:INT:FADE:NEUTRAL` and `adjx:match-source=DOC` (0 =
    /// this document, 1.. = other tabs); `adjx:ok` presses OK;
    /// `adjx:desaturate`, `adjx:equalize`, `adjx:auto-tone` run directly.
    pub(crate) fn debug_adjx(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("adjx:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f32> = arg.split(':').filter_map(|s| s.parse().ok()).collect();
        if !self.active_is_pixel() {
            let first = self
                .editor
                .doc()
                .layers()
                .iter()
                .find(|l| l.pixels().is_some())
                .map(|l| l.id);
            if first.is_some() {
                self.set_active(first);
            }
        }
        match verb {
            "open" => {
                let id = match arg {
                    "sh" => ADJ_SH,
                    "equalize" => ADJ_EQUALIZE,
                    "replace" => ADJ_REPLACE,
                    "match" => ADJ_MATCH,
                    other if other.starts_with(ADJD) => other,
                    _ => return true,
                };
                self.run_menu_action(id);
            }
            "hs" | "levels" => {
                let g = |i: usize, d: f32| nums.get(i).copied().unwrap_or(d);
                match self.adjx.as_mut().map(|s| &mut s.kind) {
                    Some(AdjxKind::Adjust(Adjustment::HueSaturation {
                        hue,
                        saturation,
                        lightness,
                        ..
                    })) => {
                        *hue = g(0, 0.0);
                        *saturation = g(1, 0.0) / 100.0;
                        *lightness = g(2, 0.0) / 100.0;
                    }
                    Some(AdjxKind::Adjust(Adjustment::Levels {
                        in_black,
                        in_white,
                        gamma,
                        ..
                    })) => {
                        *in_black = g(0, 0.0) / 255.0;
                        *in_white = g(1, 255.0) / 255.0;
                        *gamma = g(2, 1.0);
                    }
                    _ => {}
                }
            }
            "desaturate" => self.run_menu_action(ADJ_DESATURATE),
            "equalize" => self.run_menu_action(ADJ_EQUALIZE),
            "auto-tone" => self.run_menu_action(AUTO_TONE),
            "ok" => {
                if let Some(st) = self.adjx.as_mut() {
                    st.debug_ok = true;
                }
            }
            "sh" => {
                if let Some(AdjxKind::ShadowsHighlights(p)) = self.adjx.as_mut().map(|s| &mut s.kind) {
                    let g = |i: usize, d: f32| nums.get(i).copied().unwrap_or(d);
                    p.shadows.amount = g(0, 35.0) / 100.0;
                    p.shadows.tone = g(1, 50.0) / 100.0;
                    p.shadows.radius = g(2, 30.0);
                    p.highlights.amount = g(3, 0.0) / 100.0;
                    p.highlights.tone = g(4, 50.0) / 100.0;
                    p.highlights.radius = g(5, 30.0);
                    p.color = g(6, 20.0) / 100.0;
                    p.midtone = g(7, 0.0) / 100.0;
                }
            }
            "replace" | "replace-image" => {
                if let Some(AdjxKind::ReplaceColor {
                    rc,
                    swatch,
                    show_image,
                }) = self.adjx.as_mut().map(|s| &mut s.kind)
                {
                    if verb == "replace-image" {
                        *show_image = true;
                        return true;
                    }
                    let mut parts = arg.split(':');
                    if let Some(hex) = parts.next().and_then(|h| u32::from_str_radix(h, 16).ok()) {
                        *swatch = [(hex >> 16) & 255, (hex >> 8) & 255, hex & 255].map(|v| v as f32 / 255.0);
                        rc.color = swatch.map(lumenply_io::srgb_to_linear_f);
                    }
                    let n: Vec<f32> = parts.filter_map(|s| s.parse().ok()).collect();
                    let g = |i: usize, d: f32| n.get(i).copied().unwrap_or(d);
                    rc.fuzziness = g(0, 25.0) / 100.0;
                    rc.hue = g(1, 0.0);
                    rc.saturation = g(2, 0.0) / 100.0;
                    rc.lightness = g(3, 0.0) / 100.0;
                }
            }
            "pick-add" | "pick-remove" => {
                let (x, y) = (
                    nums.first().copied().unwrap_or(0.0),
                    nums.get(1).copied().unwrap_or(0.0),
                );
                let layer = self.adjx.as_ref().map(|s| s.layer);
                let p = layer
                    .and_then(|l| self.editor.doc().layer(l))
                    .and_then(|l| l.pixels())
                    .map(|s| s.get_pixel(x as i32, y as i32).to_straight());
                if let (Some([r, g, b, _]), Some(AdjxKind::ReplaceColor { rc, .. })) =
                    (p, self.adjx.as_mut().map(|s| &mut s.kind))
                {
                    if verb == "pick-add" {
                        rc.added.push([r, g, b]);
                    } else {
                        rc.removed.push([r, g, b]);
                    }
                }
            }
            "pick" => {
                let (x, y) = (
                    nums.first().copied().unwrap_or(0.0),
                    nums.get(1).copied().unwrap_or(0.0),
                );
                let layer = self.adjx.as_ref().map(|s| s.layer);
                let p = layer
                    .and_then(|l| self.editor.doc().layer(l))
                    .and_then(|l| l.pixels())
                    .map(|s| s.get_pixel(x as i32, y as i32));
                if let (Some(p), Some(AdjxKind::ReplaceColor { rc, swatch, .. })) =
                    (p, self.adjx.as_mut().map(|s| &mut s.kind))
                {
                    let [r, g, b, _] = p.to_straight();
                    rc.color = [r, g, b];
                    *swatch = [r, g, b].map(lumenply_doc::adjust::srgb_encode);
                }
            }
            "match" => {
                if let Some(AdjxKind::MatchColor { m, .. }) = self.adjx.as_mut().map(|s| &mut s.kind) {
                    let g = |i: usize, d: f32| nums.get(i).copied().unwrap_or(d);
                    m.luminance = g(0, 100.0) / 100.0;
                    m.intensity = g(1, 100.0) / 100.0;
                    m.fade = g(2, 0.0) / 100.0;
                    m.neutralize = g(3, 0.0) > 0.0;
                }
            }
            "match-source" => {
                if let Some(AdjxKind::MatchColor { pick, .. }) = self.adjx.as_mut().map(|s| &mut s.kind) {
                    pick.doc = nums.first().map(|&d| d as usize);
                    pick.layer = None;
                }
            }
            _ => return false,
        }
        true
    }
}

/// Auto Tone's shared black and white points from the pooled R+G+B
/// histogram (0.1% clipped at each end); `None` when nothing would move.
pub(crate) fn auto_tone_levels(pooled: &[u32; histogram::BINS]) -> Option<(f32, f32)> {
    let (lo, hi) = histogram::auto_contrast_levels(pooled, 0.001)?;
    (lo > 0.0 || hi < 1.0).then_some((lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_tone_shares_one_stretch_across_channels() {
        // Red spans bins 10..=40, green 5..=30, blue 20..=50, 100 counts
        // per bin: pooled, the darkest is bin 5 and the brightest bin 50
        // (0.1% of the 8800 values is less than one bin).
        let mut pooled = [0u32; histogram::BINS];
        for (lo, hi) in [(10usize, 40usize), (5, 30), (20, 50)] {
            for b in pooled.iter_mut().take(hi + 1).skip(lo) {
                *b += 100;
            }
        }
        let (lo, hi) = auto_tone_levels(&pooled).unwrap();
        assert!((lo - 5.0 / 63.0).abs() < 1e-6, "{lo}");
        assert!((hi - 50.0 / 63.0).abs() < 1e-6, "{hi}");
        // Already full range: nothing to do.
        let mut full = [0u32; histogram::BINS];
        full[0] = 10;
        full[histogram::BINS - 1] = 10;
        assert!(auto_tone_levels(&full).is_none());
    }

    /// The app on a small blank document (white), with no dialog and no
    /// autosave.
    fn small_app() -> App {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(blank(64, 48), None);
        app
    }

    fn frames(app: &mut App, ctx: &egui::Context, n: usize) {
        for _ in 0..n {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| app.frame(ctx));
        }
    }

    #[test]
    fn auto_commands_name_their_layer_in_one_step() {
        // A grey ramp from 64 to 190: every auto command has range to add.
        let mut ramp = Raster::new(64, 8);
        for x in 0..64u32 {
            let v = lumenply_io::srgb_to_linear_f((64.0 + 2.0 * x as f32) / 255.0);
            for y in 0..8 {
                ramp.set(x, y, lumenply_tiles::Rgba::new(v, v, v, 1.0));
            }
        }
        let mut ed = Editor::new(Document::new(64, 8));
        ed.execute(&AddPixelLayer::from_raster("Background", ramp, 0, 0))
            .unwrap();
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(ed, None);
        for (id, name) in [
            ("auto-tone", "Auto Tone"),
            ("auto-contrast", "Auto Contrast"),
            ("auto-color", "Auto Color"),
        ] {
            app.last_flat = Some(lumenply_render::composite_raster(app.editor.doc()));
            app.update_histogram();
            let before = app.editor.history().len();
            app.run_menu_action(id);
            assert_eq!(
                app.active_layer().map(|l| l.name.clone()),
                Some(name.to_string()),
                "{id}"
            );
            assert_eq!(app.editor.history().len(), before + 1, "{id}: one undo step");
            assert_eq!(app.editor.history().last().copied(), Some(name), "{id}");
            app.undo();
            assert_eq!(app.editor.doc().layer_count(), 1, "{id}: one undo removes it");
        }
    }

    #[test]
    fn a_click_on_the_replace_color_thumbnail_samples_that_pixel() {
        // A 220 × 147 thumbnail of a 600 × 400 image: its centre is pixel
        // (300, 200), its corners the image's corners.
        let size = egui::vec2(220.0, 146.67);
        assert_eq!(
            thumb_to_doc(egui::vec2(110.0, 73.3), size, (600, 400)),
            (300, 199)
        );
        assert_eq!(thumb_to_doc(egui::vec2(0.0, 0.0), size, (600, 400)), (0, 0));
        assert_eq!(
            thumb_to_doc(egui::vec2(220.0, 146.67), size, (600, 400)),
            (599, 399)
        );
        assert_eq!(thumb_to_doc(egui::vec2(-5.0, 400.0), size, (600, 400)), (0, 399));

        // Sampling the white canvas makes white the colour to replace.
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        app.brush_rgb = [1.0, 0.0, 0.0];
        app.run_menu_action(ADJ_REPLACE);
        let mut st = app.adjx.take().unwrap();
        app.replace_sample(&ctx, &mut st, (10, 10));
        match &st.kind {
            AdjxKind::ReplaceColor { rc, swatch, .. } => {
                assert!(rc.color.iter().all(|c| (c - 1.0).abs() < 1e-5), "{:?}", rc.color);
                assert!(swatch.iter().all(|c| (c - 1.0).abs() < 1e-5), "{swatch:?}");
            }
            _ => panic!("not Replace Color"),
        }
    }

    #[test]
    fn every_adjustment_dialog_names_its_controls() {
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        // A selection, so Equalize asks and Match Color offers its option.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(4, 4, 20, 20))),
        });
        for id in [ADJ_SH, ADJ_EQUALIZE, ADJ_REPLACE, ADJ_MATCH] {
            app.run_menu_action(id);
            assert!(app.adjx.is_some(), "{id} opens a dialog");
            if let Some(AdjxKind::MatchColor { pick, .. }) = app.adjx.as_mut().map(|s| &mut s.kind) {
                pick.doc = Some(0); // the layer list shows too
            }
            let missing = crate::a11y_tests::nameless(&mut app, &ctx);
            assert_eq!(missing, Vec::<String>::new(), "in {id}");
            app.adjx = None;
        }
    }

    #[test]
    fn dialogs_reopen_with_the_settings_last_applied() {
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        app.run_menu_action(ADJ_SH);
        if let Some(AdjxKind::ShadowsHighlights(p)) = app.adjx.as_mut().map(|s| &mut s.kind) {
            p.shadows.amount = 0.8;
            p.midtone = 0.25;
        }
        app.adjx.as_mut().unwrap().debug_ok = true;
        frames(&mut app, &ctx, 1);
        app.run_menu_action(ADJ_SH);
        match app.adjx.as_ref().map(|s| &s.kind) {
            Some(AdjxKind::ShadowsHighlights(p)) => {
                assert_eq!((p.shadows.amount, p.midtone), (0.8, 0.25));
            }
            _ => panic!("Shadows/Highlights did not open"),
        }
        // Cancel keeps the last applied settings; other kinds start fresh.
        app.adjx = None;
        app.run_menu_action("adjd-levels");
        assert!(matches!(
            app.adjx.as_ref().map(|s| &s.kind),
            Some(AdjxKind::Adjust(a)) if *a == Adjustment::levels_default()
        ));
    }

    #[test]
    fn shortcuts_wait_while_a_dialog_is_open() {
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        app.run_menu_action(ADJ_DESATURATE);
        let n = app.editor.history().len();
        app.run_menu_action(ADJ_SH);
        let cmd = egui::Modifiers::COMMAND;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            modifiers: cmd,
            events: vec![egui::Event::Key {
                key: Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: cmd,
            }],
            ..Default::default()
        };
        let _ = ctx.run(raw.clone(), |ctx| app.frame(ctx));
        // Cmd+Z did not undo under the dialog, which stays open.
        assert_eq!(app.editor.history().len(), n);
        assert!(app.adjx.is_some());
        // Without the dialog the same key undoes.
        app.adjx = None;
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
        assert_eq!(app.editor.history().len(), n - 1);
    }

    #[test]
    fn an_applied_adjustment_records_as_an_action_step() {
        use lumenply_core::actions::Step;
        let mut app = small_app();
        let path = std::env::temp_dir().join(format!("lumenply-actions-apply-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        app.actions = crate::actions_panel::ActionsState::load_from(Some(path.clone()));
        let ctx = crate::a11y_tests::ctx();
        app.start_recording();
        app.run_menu_action("adjd-posterize");
        frames(&mut app, &ctx, 1);
        let AdjxKind::Adjust(posterize) = app.adjx.as_ref().unwrap().kind.clone() else {
            panic!("a Posterize dialog");
        };
        app.adjx.as_mut().unwrap().debug_ok = true;
        frames(&mut app, &ctx, 1);
        // Shadows/Highlights has no step kind yet: a note, not a silent gap.
        app.run_menu_action(ADJ_SH);
        frames(&mut app, &ctx, 1);
        app.adjx.as_mut().unwrap().debug_ok = true;
        frames(&mut app, &ctx, 1);
        app.stop_recording();
        let steps = app.actions.list.last().unwrap().action.steps.clone();
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!(
            steps[0],
            Step::Apply {
                adjustment: posterize.clone()
            }
        );
        assert!(matches!(&steps[1], Step::Skipped { .. }), "{steps:?}");
        assert_eq!(steps[0].describe(), format!("{} (pixels)", posterize.name()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ok_is_one_undo_step_and_cancel_none() {
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        let before = app.editor.history().len();
        app.run_menu_action(ADJ_SH);
        frames(&mut app, &ctx, 2);
        // Previewing never touches the history.
        assert_eq!(app.editor.history().len(), before);
        app.adjx.as_mut().unwrap().debug_ok = true;
        frames(&mut app, &ctx, 1);
        assert!(app.adjx.is_none());
        assert_eq!(app.editor.history().len(), before + 1);
        assert_eq!(app.editor.history().last().copied(), Some("Shadows/Highlights"));
        // Cancel (Esc) closes without a step.
        app.run_menu_action(ADJ_REPLACE);
        frames(&mut app, &ctx, 1);
        app.adjx = None;
        frames(&mut app, &ctx, 1);
        assert_eq!(app.editor.history().len(), before + 1);
        // Desaturate runs straight away: white stays white, one step.
        app.run_menu_action(ADJ_DESATURATE);
        assert_eq!(app.editor.history().last().copied(), Some("Desaturate"));
    }

    #[test]
    fn every_adjustment_kind_has_an_applied_form() {
        assert_eq!(adjd_id("Black & White"), "adjd-black-white");
        assert_eq!(adjd_id("Hue/Saturation"), "adjd-hue-saturation");
        let presets = adjustment_presets();
        assert!(presets.len() >= 15);
        for (name, adj) in &presets {
            assert_eq!(adjd_preset(&adjd_id(name)).as_ref(), Some(adj), "{name}");
        }
        // Photoshop's order: Brightness/Contrast, Levels, Curves first.
        let names: Vec<&str> = ps_ordered_presets().iter().map(|(n, _)| *n).collect();
        assert_eq!(&names[..3], &["Brightness/Contrast", "Levels", "Curves"]);
        // Invert applies at once; Levels opens its dialog and OK is one step.
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        let n = app.editor.history().len();
        app.run_menu_action("adjd-invert");
        assert!(app.adjx.is_none());
        assert_eq!(app.editor.history().last().copied(), Some("Invert"));
        app.run_menu_action("adjd-levels");
        assert!(matches!(
            app.adjx.as_ref().map(|s| &s.kind),
            Some(AdjxKind::Adjust(Adjustment::Levels { .. }))
        ));
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert_eq!(missing, Vec::<String>::new());
        app.adjx.as_mut().unwrap().debug_ok = true;
        frames(&mut app, &ctx, 1);
        assert_eq!(app.editor.history().len(), n + 2);
        assert_eq!(app.editor.history().last().copied(), Some("Levels"));
    }

    #[test]
    fn adjustments_need_an_unlocked_pixel_layer() {
        let mut app = small_app();
        assert_eq!(app.adjx_action_block(ADJ_SH), Some(None));
        app.add_adjustment(Adjustment::Invert);
        assert_eq!(
            app.adjx_action_block(ADJ_SH),
            Some(Some("Select a pixel layer first"))
        );
        assert_eq!(app.adjx_action_block("auto-contrast"), None, "not ours");
        app.run_menu_action(ADJ_SH);
        assert!(app.adjx.is_none(), "blocked: no dialog");
    }
}
