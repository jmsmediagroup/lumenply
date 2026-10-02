//! The one colour control. Every colour in the app (foreground and
//! background, gradient ends, text, layer effects) is edited through the
//! same picker: a saturation/value square over a hue strip, the
//! original/new comparison, an eyedropper that samples the canvas
//! composite, a validated hex field, numeric R/G/B in Plex Mono and a row
//! of recent colours.
//!
//! Colours here are sRGB 0..1 (`[f32; 3]`), like `brush_rgb`. egui's own
//! `color_edit_button_rgb` reads `[f32; 3]` as *linear*, which made every
//! swatch built on it show a washed-out colour.
//!
//! State that must outlive a frame lives in egui's temp data: one
//! [`PickerState`] per popup, plus a few app-wide slots (the armed
//! eyedropper, a delivered sample, recent colours, the dismiss-click
//! guard). The canvas talks to the picker only through
//! [`App::picker_canvas_input`].

use super::*;

/// Content width of the picker popup (fixed, so nothing reflows).
const W: f32 = 232.0;
const SV_H: f32 = 148.0;
const HUE_H: f32 = 12.0;
const RECENT_MAX: usize = 10;
/// Colours closer than this per channel count as equal: it absorbs 8-bit
/// round trips (text colour is stored linear and re-encoded every frame)
/// so the picker doesn't resync and jitter the hue mid-drag.
const SAME_EPS: f32 = 0.6 / 255.0;
/// Invalid input (matches the quick-mask red).
const BAD: Color32 = Color32::from_rgb(0xE8, 0x5D, 0x5D);
/// Hover fill for the picker's small buttons.
const HOVER: Color32 = Color32::from_rgb(0x32, 0x38, 0x3F);

// ---- colour maths (pure, tested) ---------------------------------------------

/// sRGB 0..1 to HSV, all three in 0..1 (hue 0 = red).
pub(crate) fn rgb_to_hsv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max <= 0.0 { 0.0 } else { d / max };
    [h, s, max]
}

/// HSV (0..1 each; hue wraps) to sRGB 0..1.
pub(crate) fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i as i32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// HSV for `rgb`, keeping `prev`'s hue (and saturation) where `rgb`
/// leaves them undefined: a grey has no hue and black has neither, so
/// dragging through them must not snap the hue strip back to red.
pub(crate) fn hsv_keeping(prev: [f32; 3], rgb: [f32; 3]) -> [f32; 3] {
    let mut hsv = rgb_to_hsv(rgb);
    if hsv[2] <= 1e-4 {
        hsv[0] = prev[0];
        hsv[1] = prev[1];
    } else if hsv[1] <= 1e-4 {
        hsv[0] = prev[0];
    }
    hsv
}

/// `#RRGGBB`, `RRGGBB`, `#RGB` or `RGB` (any case, surrounding spaces
/// ignored) to sRGB 0..1.
pub(crate) fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let t = s.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    // ASCII-only from here, so byte slicing below is safe.
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    let bytes = match t.len() {
        6 => [byte(&t[0..2])?, byte(&t[2..4])?, byte(&t[4..6])?],
        3 => {
            let nib = |i: usize| byte(&t[i..i + 1]).map(|n| n * 17);
            [nib(0)?, nib(1)?, nib(2)?]
        }
        _ => return None,
    };
    Some(bytes.map(|b| b as f32 / 255.0))
}

/// sRGB 0..1 to 8-bit, rounded.
pub(crate) fn to_u8(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

/// `#RRGGBB`, upper case.
pub(crate) fn format_hex(rgb: [f32; 3]) -> String {
    let [r, g, b] = to_u8(rgb);
    format!("#{r:02X}{g:02X}{b:02X}")
}

pub(crate) fn to_color32(rgb: [f32; 3]) -> Color32 {
    let [r, g, b] = to_u8(rgb);
    Color32::from_rgb(r, g, b)
}

fn same(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < SAME_EPS)
}

/// The composite's colour at document position (x, y) as sRGB 0..1 —
/// the same encoding the Eyedropper tool has always used (straight
/// colour, 8-bit sRGB). None outside the image.
pub(crate) fn sample_srgb(flat: &Raster, x: f32, y: f32) -> Option<[f32; 3]> {
    // `!(x >= 0)` also rejects NaN.
    if !(x >= 0.0 && y >= 0.0) {
        return None;
    }
    let (px, py) = (x as u32, y as u32);
    if px >= flat.width || py >= flat.height {
        return None;
    }
    let [r, g, b, _] = flat.get(px, py).to_straight();
    Some([r, g, b].map(|v| lumenply_io::linear_to_srgb(v) as f32 / 255.0))
}

/// Move `c` to the front of the recent list, without duplicates, capped.
pub(crate) fn push_recent(list: &mut Vec<[u8; 3]>, c: [u8; 3]) {
    list.retain(|x| *x != c);
    list.insert(0, c);
    list.truncate(RECENT_MAX);
}

// ---- app-wide state ----------------------------------------------------------------

fn gid(name: &str) -> egui::Id {
    egui::Id::new(("lumenply-color-picker", name))
}

/// The eyedropper is armed for the picker `target`; `frame` is refreshed
/// by that picker every frame it is shown, so the arm lapses on its own
/// if the picker disappears.
#[derive(Clone, Copy)]
struct Armed {
    target: egui::Id,
    frame: u64,
}

/// A canvas sample on its way to the picker that asked for it.
#[derive(Clone, Copy)]
struct Sample {
    target: egui::Id,
    rgb: [f32; 3],
    frame: u64,
}

#[derive(Clone)]
struct Recent(Vec<[u8; 3]>);

/// The picker that armed the eyedropper, if it is still armed.
pub(crate) fn eyedropper_target(ctx: &egui::Context) -> Option<egui::Id> {
    let a = ctx.data(|d| d.get_temp::<Armed>(gid("armed")))?;
    (ctx.cumulative_pass_nr().saturating_sub(a.frame) <= 2).then_some(a.target)
}

fn arm(ctx: &egui::Context, target: egui::Id) {
    let frame = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(gid("armed"), Armed { target, frame }));
}

fn disarm(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Armed>(gid("armed")));
}

/// A message for the status bar (shown via the canvas hook).
fn say(ctx: &egui::Context, msg: impl Into<String>) {
    let msg: String = msg.into();
    ctx.data_mut(|d| d.insert_temp(gid("status"), msg));
}

/// True while any colour picker popup is open. Esc then belongs to the
/// picker (cancel the eyedropper, close) rather than to Deselect.
pub(crate) fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<(egui::Id, u64)>(gid("open")))
        .is_some_and(|(id, f)| {
            ctx.cumulative_pass_nr().saturating_sub(f) <= 2 && ctx.memory(|m| m.is_popup_open(id))
        })
}

/// True while a drag inside a picker (square, hue strip, R/G/B) is under
/// way this frame; callers keep coalescing their undo step until it ends.
pub(crate) fn is_dragging(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(gid("dragging"))) == Some(ctx.cumulative_pass_nr())
}

fn recent_file() -> Option<PathBuf> {
    session::data_dir().map(|d| d.join("recent-colors.json"))
}

fn recent(ctx: &egui::Context) -> Vec<[u8; 3]> {
    if let Some(Recent(list)) = ctx.data(|d| d.get_temp::<Recent>(gid("recent"))) {
        return list;
    }
    let list: Vec<[u8; 3]> = recent_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|h| parse_hex(h).map(to_u8))
        .take(RECENT_MAX)
        .collect();
    ctx.data_mut(|d| d.insert_temp(gid("recent"), Recent(list.clone())));
    list
}

/// Add a colour to the recent row (and the small file that keeps it
/// across sessions).
pub(crate) fn remember(ctx: &egui::Context, rgb: [f32; 3]) {
    let mut list = recent(ctx);
    push_recent(&mut list, to_u8(rgb));
    ctx.data_mut(|d| d.insert_temp(gid("recent"), Recent(list.clone())));
    // Screenshot runs never write the user's file.
    if ctx.data(|d| d.get_temp::<bool>(gid("no-persist"))).is_some() {
        return;
    }
    if let Some(p) = recent_file() {
        let hex: Vec<String> = list
            .iter()
            .map(|c| format_hex(c.map(|b| b as f32 / 255.0)))
            .collect();
        if let Ok(json) = serde_json::to_string(&hex) {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json);
        }
    }
}

// ---- the widget ------------------------------------------------------------------

/// Where a picker popup opens.
pub(crate) enum Placement {
    /// Below the anchor, or above it when there is no room below.
    Auto,
    /// With the popup's `Align2` corner at this point.
    At(Pos2, Align2),
}

/// Where an auto-placed popup of height `h` goes: 6 px below the
/// anchor when it fits on `screen`, else 6 px above when that fits,
/// else below (the area then clamps it on screen).
pub(crate) fn auto_place(anchor: egui::Rect, h: f32, screen: egui::Rect) -> (Pos2, Align2) {
    let fits_below = anchor.bottom() + 6.0 + h <= screen.bottom() - 4.0;
    let fits_above = anchor.top() - 6.0 - h >= screen.top() + 4.0;
    if fits_below || !fits_above {
        (anchor.left_bottom() + egui::vec2(0.0, 6.0), Align2::LEFT_TOP)
    } else {
        (anchor.left_top() - egui::vec2(0.0, 6.0), Align2::LEFT_BOTTOM)
    }
}

/// What a picker did to its colour this frame.
#[derive(Default, Clone, Copy)]
pub(crate) struct Edit {
    pub changed: bool,
    /// A drag inside the picker is under way.
    pub dragging: bool,
    /// A drag inside the picker ended this frame.
    pub drag_stopped: bool,
}

#[derive(Clone, Default)]
struct PickerState {
    /// The popup was open last time this picker ran.
    open: bool,
    /// Kept separately from the colour so greys and black keep a hue.
    hsv: [f32; 3],
    /// The colour `hsv` describes; anything else is an outside edit.
    synced: [f32; 3],
    /// The colour when the popup opened (left half of the comparison).
    original: [f32; 3],
    hex: String,
    hex_bad: bool,
    /// Popup height last frame, to decide between below and above.
    height: f32,
}

/// Drop-in replacement for `egui::color_picker::color_edit_button_rgb`,
/// but `rgb` is **sRGB** 0..1 and the popup is the Lumenply picker. The
/// response behaves like a slider's: `changed()` on every edit, `dragged()`
/// while a drag inside the picker is under way, `drag_stopped()` when it
/// ends.
pub(crate) fn color_edit_button_rgb(ui: &mut egui::Ui, rgb: &mut [f32; 3]) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 20.0), Sense::click());
    let hex = format_hex(*rgb);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ColorButton, ui.is_enabled(), &hex));
    let popup_id = resp.id.with("color-picker");
    debug_open_nth(ui.ctx(), popup_id);
    let open = ui.memory(|m| m.is_popup_open(popup_id));
    if ui.is_rect_visible(rect) {
        let edge = if open {
            ACCENT
        } else if resp.hovered() {
            MUTED
        } else {
            LINE
        };
        paint_swatch(ui.painter(), rect, *rgb, edge);
    }
    let mut resp = if open {
        resp
    } else {
        resp.on_hover_text(format!("{}  ·  click to edit", format_hex(*rgb)))
    };
    let edit = picker_popup(ui, &resp, popup_id, None, rgb, Placement::Auto);
    // The end of a drag reports as a change too, so callers that only
    // listen to `changed()` still see the moment to close their undo step.
    if edit.changed || edit.drag_stopped {
        resp.mark_changed();
    }
    resp.dragged |= edit.dragging;
    resp.drag_stopped |= edit.drag_stopped;
    resp
}

/// A colour swatch: the colour with a hairline edge.
pub(crate) fn paint_swatch(p: &egui::Painter, rect: egui::Rect, rgb: [f32; 3], edge: Color32) {
    p.rect_filled(rect, 4.0, to_color32(rgb));
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, edge));
}

/// `--screenshot-do color:open-N`: open the N-th generic colour button
/// drawn in the next frame (0-based, in drawing order).
fn debug_open_nth(ctx: &egui::Context, popup_id: egui::Id) {
    let frame = ctx.cumulative_pass_nr();
    let n = ctx.data_mut(|d| {
        let e = d.get_temp_mut_or_insert_with(gid("nth"), || (frame, 0usize));
        if e.0 != frame {
            *e = (frame, 0);
        }
        e.1 += 1;
        e.1 - 1
    });
    if ctx.data(|d| d.get_temp::<usize>(gid("debug-open"))) == Some(n) {
        ctx.data_mut(|d| d.remove::<usize>(gid("debug-open")));
        ctx.memory_mut(|m| m.open_popup(popup_id));
    }
}

/// The picker popup for `rgb`, anchored to `anchor`; clicking the anchor
/// opens and closes it. Call it every frame the anchor is shown.
pub(crate) fn picker_popup(
    ui: &egui::Ui,
    anchor: &egui::Response,
    popup_id: egui::Id,
    title: Option<&str>,
    rgb: &mut [f32; 3],
    place: Placement,
) -> Edit {
    let ctx = ui.ctx().clone();
    let frame = ctx.cumulative_pass_nr();
    let mut out = Edit::default();

    // A canvas sample delivered for this picker.
    let sample = ctx.data(|d| d.get_temp::<Sample>(gid("sample")));
    if let Some(s) = sample.filter(|s| s.target == popup_id) {
        ctx.data_mut(|d| d.remove::<Sample>(gid("sample")));
        if frame.saturating_sub(s.frame) <= 2 {
            *rgb = s.rgb;
            out.changed = true;
            out.drag_stopped = true;
            remember(&ctx, s.rgb);
        }
    }

    if anchor.clicked() {
        ctx.memory_mut(|m| m.toggle_popup(popup_id));
    }
    let state_id = popup_id.with("state");
    let mut st: PickerState = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    if !ctx.memory(|m| m.is_popup_open(popup_id)) {
        if st.open {
            // Just closed: a colour that changed joins the recent row.
            st.open = false;
            if !same(*rgb, st.original) {
                remember(&ctx, *rgb);
            }
            if eyedropper_target(&ctx) == Some(popup_id) {
                disarm(&ctx);
            }
            ctx.data_mut(|d| d.insert_temp(state_id, st));
        }
        return out;
    }
    if !st.open {
        st.open = true;
        st.original = *rgb;
        st.hsv = hsv_keeping(st.hsv, *rgb);
        st.synced = *rgb;
        st.hex = format_hex(*rgb);
        st.hex_bad = false;
    } else if !same(*rgb, st.synced) {
        // Changed elsewhere (X swaps, the Eyedropper tool, a canvas
        // sample): follow it.
        st.hsv = hsv_keeping(st.hsv, *rgb);
        st.synced = *rgb;
    }
    // The hex field mirrors the colour except while the user types in it.
    if !ctx.memory(|m| m.has_focus(popup_id.with("hex"))) {
        st.hex = format_hex(*rgb);
        st.hex_bad = false;
    }
    ctx.data_mut(|d| d.insert_temp(gid("open"), (popup_id, frame)));
    let armed = eyedropper_target(&ctx) == Some(popup_id);
    if armed {
        arm(&ctx, popup_id);
    }

    let screen = ctx.screen_rect();
    let (pos, pivot) = match place {
        Placement::At(p, a) => (p, a),
        Placement::Auto => {
            let h = if st.height > 0.0 { st.height } else { 360.0 };
            auto_place(anchor.rect, h, screen)
        }
    };

    let mut toggle_arm = false;
    let area = egui::Area::new(popup_id)
        .kind(egui::UiKind::Popup)
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .pivot(pivot)
        // Appear at once: a fading picker reads as sluggish and lets the
        // canvas show through while the first click lands.
        .fade_in(false)
        .sense(BACKDROP_SENSE)
        .constrain_to(screen.shrink(4.0))
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(10.0))
                .show(ui, |ui| {
                    ui.set_width(W);
                    picker_body(
                        ui,
                        popup_id,
                        title,
                        rgb,
                        &mut st,
                        &mut out,
                        armed,
                        &mut toggle_arm,
                    );
                });
        });
    st.height = area.response.rect.height();
    let popup_rect = area.response.rect;

    let mut armed = armed;
    if toggle_arm {
        if armed {
            disarm(&ctx);
            say(&ctx, "Eyedropper cancelled");
        } else {
            arm(&ctx, popup_id);
            say(
                &ctx,
                "Eyedropper: click the canvas to sample a colour — Esc cancels",
            );
        }
        armed = !armed;
    }

    // Esc cancels the eyedropper first, then closes the picker.
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        if armed {
            disarm(&ctx);
            say(&ctx, "Eyedropper cancelled");
        } else {
            ctx.memory_mut(|m| m.close_popup());
        }
    }
    // A press outside closes the picker; while the eyedropper is armed a
    // press on the canvas is the sample instead. A dismissing press on the
    // canvas must not also paint, so the canvas swallows it.
    let canvas = ctx.data(|d| d.get_temp::<egui::Rect>(gid("canvas")));
    let press = ctx.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    if let Some(p) = press {
        if !popup_rect.contains(p) && !anchor.rect.contains(p) {
            let on_canvas = canvas.is_some_and(|c| c.contains(p));
            if !(armed && on_canvas) {
                if armed {
                    disarm(&ctx);
                }
                ctx.memory_mut(|m| {
                    if m.is_popup_open(popup_id) {
                        m.close_popup();
                    }
                });
                if on_canvas {
                    ctx.data_mut(|d| d.insert_temp(gid("swallow"), true));
                }
            }
        }
    }

    if out.dragging {
        ctx.data_mut(|d| d.insert_temp(gid("dragging"), frame));
    }
    ctx.data_mut(|d| d.insert_temp(state_id, st));
    out
}

/// Everything inside the popup frame.
#[allow(clippy::too_many_arguments)]
fn picker_body(
    ui: &mut egui::Ui,
    popup_id: egui::Id,
    title: Option<&str>,
    rgb: &mut [f32; 3],
    st: &mut PickerState,
    out: &mut Edit,
    armed: bool,
    toggle_arm: &mut bool,
) {
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
    let hex_id = popup_id.with("hex");
    let hex_focus = ui.memory(|m| m.has_focus(hex_id));
    // Apply a new colour, keeping the hex field in step unless the user is
    // typing in it.
    let set = |rgb: &mut [f32; 3], st: &mut PickerState, out: &mut Edit, c: [f32; 3]| {
        *rgb = c;
        st.synced = c;
        out.changed = true;
        if !hex_focus {
            st.hex = format_hex(c);
            st.hex_bad = false;
        }
    };

    if let Some(t) = title {
        ui.label(RichText::new(t.to_uppercase()).small().strong().color(MUTED));
    }

    // Saturation (x) / value (y) square.
    let (sv_rect, sv) = ui.allocate_exact_size(egui::vec2(W, SV_H), Sense::click_and_drag());
    sv.widget_info(|| {
        egui::WidgetInfo::slider(
            true,
            (st.hsv[1] * 100.0).round() as f64,
            "Saturation and brightness",
        )
    });
    if sv.is_pointer_button_down_on() {
        if let Some(p) = sv.interact_pointer_pos() {
            st.hsv[1] = ((p.x - sv_rect.left()) / sv_rect.width()).clamp(0.0, 1.0);
            st.hsv[2] = 1.0 - ((p.y - sv_rect.top()) / sv_rect.height()).clamp(0.0, 1.0);
            set(rgb, st, out, hsv_to_rgb(st.hsv));
        }
        out.dragging = true;
    }
    out.drag_stopped |= sv.drag_stopped() || sv.clicked();
    if sv.hovered() || sv.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    paint_sv(ui.painter(), sv_rect, st.hsv);

    // Hue strip: a pill, red at both rounded ends.
    let (hue_rect, hue) = ui.allocate_exact_size(egui::vec2(W, HUE_H), Sense::click_and_drag());
    hue.widget_info(|| egui::WidgetInfo::slider(true, (st.hsv[0] * 360.0).round() as f64, "Hue"));
    let cap = HUE_H / 2.0;
    let (hx0, hx1) = (hue_rect.left() + cap, hue_rect.right() - cap);
    if hue.is_pointer_button_down_on() {
        if let Some(p) = hue.interact_pointer_pos() {
            st.hsv[0] = ((p.x - hx0) / (hx1 - hx0)).clamp(0.0, 1.0);
            set(rgb, st, out, hsv_to_rgb(st.hsv));
        }
        out.dragging = true;
    }
    out.drag_stopped |= hue.drag_stopped() || hue.clicked();
    paint_hue(ui.painter(), hue_rect, st.hsv[0]);

    // Original | new, eyedropper, hex.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (cr, cmp) = ui.allocate_exact_size(egui::vec2(56.0, 28.0), Sense::click());
        cmp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Previous colour"));
        {
            let p = ui.painter();
            let mid = cr.center().x;
            let left = egui::Rect::from_min_max(cr.min, egui::pos2(mid, cr.max.y));
            let right = egui::Rect::from_min_max(egui::pos2(mid, cr.min.y), cr.max);
            let rl = egui::Rounding {
                nw: 5.0,
                sw: 5.0,
                ..Default::default()
            };
            let rr = egui::Rounding {
                ne: 5.0,
                se: 5.0,
                ..Default::default()
            };
            p.rect_filled(left, rl, to_color32(st.original));
            p.rect_filled(right, rr, to_color32(*rgb));
            let hovering_old = cmp.hover_pos().is_some_and(|q| q.x < mid);
            p.rect_stroke(cr, 5.0, Stroke::new(1.0, LINE));
            if hovering_old {
                p.rect_stroke(left.shrink(0.5), rl, Stroke::new(1.0, MUTED));
            }
        }
        if cmp.clicked() && cmp.interact_pointer_pos().is_some_and(|q| q.x < cr.center().x) {
            st.hsv = hsv_keeping(st.hsv, st.original);
            let c = st.original;
            set(rgb, st, out, c);
        }
        cmp.on_hover_text("Left: the colour before (click to go back)  ·  Right: new");

        let (er, eye) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::click());
        eye.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, armed, "Pick colour from canvas")
        });
        let (fill, ink) = if armed {
            (ACCENT, ACCENT_INK)
        } else if eye.hovered() {
            (HOVER, TEXT)
        } else {
            (GROUND, MUTED)
        };
        ui.painter().rect_filled(er, 6.0, fill);
        if !armed {
            ui.painter().rect_stroke(er, 6.0, Stroke::new(1.0, LINE));
        }
        crate::tools::draw_icon(ui.painter(), er.shrink(6.0), Tool::Eyedropper, ink);
        if eye
            .on_hover_text("Sample a colour from the canvas (Esc cancels)")
            .clicked()
        {
            *toggle_arm = true;
        }

        let w = ui.available_width();
        let pad = egui::vec2(8.0, 4.0);
        let te = egui::TextEdit::singleline(&mut st.hex)
            .id(hex_id)
            .font(egui::TextStyle::Monospace)
            .char_limit(9)
            .margin(pad)
            .desired_width(w - 2.0 * pad.x)
            .min_size(egui::vec2(w, 28.0))
            .vertical_align(egui::Align::Center)
            .text_color(if st.hex_bad { BAD } else { TEXT });
        let r = ui.add(te).on_hover_text("Hex: #RRGGBB or #RGB");
        a11y_name(&r, "Hex colour");
        let ring = if r.has_focus() {
            ACCENT
        } else if r.hovered() {
            MUTED
        } else {
            LINE
        };
        // The response rect is the text area; the ring goes round the
        // whole field, margin included.
        ui.painter()
            .rect_stroke(r.rect.expand2(pad), 6.0, Stroke::new(1.0, ring));
        if r.changed() {
            let parsed = parse_hex(&st.hex);
            st.hex_bad = parsed.is_none();
            // Six digits are unambiguous: apply while typing. Three wait
            // for Enter so typing "FF0000" doesn't flash yellow at "FF0".
            let digits = st.hex.trim().trim_start_matches('#').len();
            if let (Some(c), 6) = (parsed, digits) {
                st.hsv = hsv_keeping(st.hsv, c);
                *rgb = c;
                st.synced = c;
                out.changed = true;
            }
        }
        if r.lost_focus() {
            if let Some(c) = parse_hex(&st.hex) {
                if !same(c, *rgb) {
                    st.hsv = hsv_keeping(st.hsv, c);
                    *rgb = c;
                    st.synced = c;
                    out.changed = true;
                }
            }
            st.hex = format_hex(*rgb);
            st.hex_bad = false;
        }
    });

    // Numeric R, G, B (0-255) in mono.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
        let v = ui.visuals_mut();
        v.widgets.inactive.weak_bg_fill = GROUND;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.hovered.weak_bg_fill = GROUND;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED);
        v.widgets.active.weak_bg_fill = GROUND;
        let mut b = to_u8(*rgb);
        let mut edited = false;
        let field = (W - 3.0 * 12.0 - 5.0 * 6.0) / 3.0;
        for (i, (label, name)) in [("R", "Red"), ("G", "Green"), ("B", "Blue")]
            .into_iter()
            .enumerate()
        {
            ui.add_sized([12.0, 24.0], egui::Label::new(RichText::new(label).color(MUTED)));
            let r = ui.add_sized(
                [field, 24.0],
                egui::DragValue::new(&mut b[i]).range(0..=255).speed(0.5),
            );
            a11y_name(&r, name);
            edited |= r.changed();
            out.dragging |= r.dragged();
            out.drag_stopped |= r.drag_stopped();
        }
        if edited {
            let c = b.map(|x| x as f32 / 255.0);
            st.hsv = hsv_keeping(st.hsv, c);
            set(rgb, st, out, c);
        }
    });

    // Recent colours, newest first; empty slots keep the row's shape.
    ui.label(RichText::new("RECENT").small().strong().color(MUTED));
    let list = recent(ui.ctx());
    ui.horizontal(|ui| {
        let gap = 4.0;
        ui.spacing_mut().item_spacing.x = gap;
        let side = (W - gap * (RECENT_MAX as f32 - 1.0)) / RECENT_MAX as f32;
        for i in 0..RECENT_MAX {
            // An empty slot is only a placeholder: not clickable, not a Tab stop.
            let sense = if i < list.len() {
                Sense::click()
            } else {
                Sense::hover()
            };
            let (r, resp) = ui.allocate_exact_size(egui::vec2(side, side), sense);
            if let Some(&c8) = list.get(i) {
                let label = format!("Recent colour {}", format_hex(c8.map(|x| x as f32 / 255.0)));
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ColorButton, true, &label));
            }
            match list.get(i) {
                Some(&c8) => {
                    let c = c8.map(|x| x as f32 / 255.0);
                    let edge = if resp.hovered() { TEXT } else { LINE };
                    paint_swatch(ui.painter(), r, c, edge);
                    if resp.on_hover_text(format_hex(c)).clicked() {
                        st.hsv = hsv_keeping(st.hsv, c);
                        set(rgb, st, out, c);
                    }
                }
                None => {
                    ui.painter()
                        .rect_stroke(r.shrink(0.5), 4.0, Stroke::new(1.0, LINE));
                }
            }
        }
    });
}

/// The saturation/value square for `hsv`'s hue, with a ring thumb.
fn paint_sv(p: &egui::Painter, r: egui::Rect, hsv: [f32; 3]) {
    const N: usize = 16;
    let mut mesh = egui::Mesh::default();
    for j in 0..=N {
        for i in 0..=N {
            let s = i as f32 / N as f32;
            let t = j as f32 / N as f32;
            let pos = egui::pos2(r.left() + s * r.width(), r.top() + t * r.height());
            mesh.colored_vertex(pos, to_color32(hsv_to_rgb([hsv[0], s, 1.0 - t])));
        }
    }
    let row = (N + 1) as u32;
    for j in 0..N as u32 {
        for i in 0..N as u32 {
            let a = j * row + i;
            mesh.add_triangle(a, a + 1, a + row);
            mesh.add_triangle(a + 1, a + row + 1, a + row);
        }
    }
    p.add(Shape::mesh(mesh));
    p.rect_stroke(r, 2.0, Stroke::new(1.0, LINE));
    let tp = egui::pos2(
        r.left() + hsv[1] * r.width(),
        r.top() + (1.0 - hsv[2]) * r.height(),
    );
    p.circle_filled(tp, 5.0, to_color32(hsv_to_rgb(hsv)));
    p.circle_stroke(tp, 6.0, Stroke::new(2.0, Color32::WHITE));
    p.circle_stroke(tp, 7.5, Stroke::new(1.0, Color32::from_black_alpha(150)));
}

/// The hue pill with its thumb at `hue`.
fn paint_hue(p: &egui::Painter, r: egui::Rect, hue: f32) {
    let cap = r.height() / 2.0;
    let (x0, x1) = (r.left() + cap, r.right() - cap);
    let red = to_color32([1.0, 0.0, 0.0]);
    p.circle_filled(egui::pos2(x0, r.center().y), cap, red);
    p.circle_filled(egui::pos2(x1, r.center().y), cap, red);
    let mut mesh = egui::Mesh::default();
    for k in 0..=6 {
        let h = k as f32 / 6.0;
        let x = x0 + h * (x1 - x0);
        let c = to_color32(hsv_to_rgb([h, 1.0, 1.0]));
        mesh.colored_vertex(egui::pos2(x, r.top()), c);
        mesh.colored_vertex(egui::pos2(x, r.bottom()), c);
    }
    for k in 0..6u32 {
        let a = 2 * k;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    p.add(Shape::mesh(mesh));
    let tp = egui::pos2(x0 + hue.clamp(0.0, 1.0) * (x1 - x0), r.center().y);
    p.circle_filled(tp, cap + 1.0, to_color32(hsv_to_rgb([hue, 1.0, 1.0])));
    p.circle_stroke(tp, cap + 1.0, Stroke::new(2.0, Color32::WHITE));
    p.circle_stroke(tp, cap + 2.5, Stroke::new(1.0, Color32::from_black_alpha(150)));
}

/// A chip beside the cursor showing the colour under it.
fn paint_loupe(ctx: &egui::Context, at: Pos2, c: Option<[f32; 3]>) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, gid("loupe")));
    let size = egui::vec2(112.0, 32.0);
    let screen = ctx.screen_rect();
    let mut min = at + egui::vec2(18.0, 18.0);
    if min.x + size.x > screen.right() - 4.0 {
        min.x = at.x - 18.0 - size.x;
    }
    if min.y + size.y > screen.bottom() - 4.0 {
        min.y = at.y - 18.0 - size.y;
    }
    let r = egui::Rect::from_min_size(min, size);
    p.rect_filled(
        r.translate(egui::vec2(0.0, 3.0)),
        8.0,
        Color32::from_black_alpha(70),
    );
    p.rect_filled(r, 8.0, RAISED);
    p.rect_stroke(r, 8.0, Stroke::new(1.0, LINE));
    let chip = egui::Rect::from_min_size(r.min + egui::vec2(5.0, 5.0), egui::vec2(22.0, 22.0));
    match c {
        Some(c) => {
            paint_swatch(&p, chip, c, Color32::WHITE);
            p.text(
                chip.right_center() + egui::vec2(9.0, 0.0),
                Align2::LEFT_CENTER,
                format_hex(c),
                FontId::monospace(12.5),
                TEXT,
            );
        }
        None => {
            p.rect_stroke(chip, 4.0, Stroke::new(1.0, LINE));
            p.text(
                chip.right_center() + egui::vec2(9.0, 0.0),
                Align2::LEFT_CENTER,
                "outside",
                FontId::proportional(12.0),
                MUTED,
            );
        }
    }
}

impl App {
    /// Canvas input hook, run before any tool. Returns true when the
    /// colour picker owns the pointer this frame — an armed eyedropper
    /// (crosshair, loupe; a click samples the composite into the picker
    /// that armed it, the active tool untouched) or the press that
    /// dismissed a picker (which must not also paint).
    pub(crate) fn picker_canvas_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) -> bool {
        ctx.data_mut(|d| d.insert_temp(gid("canvas"), resp.rect));
        if self.shot.is_some() {
            ctx.data_mut(|d| d.insert_temp(gid("no-persist"), true));
        }
        if let Some(msg) = ctx.data_mut(|d| d.remove_temp::<String>(gid("status"))) {
            self.status = msg;
        }
        if ctx.data(|d| d.get_temp::<bool>(gid("swallow"))).is_some() {
            if !ctx.input(|i| i.pointer.any_down()) {
                ctx.data_mut(|d| d.remove::<bool>(gid("swallow")));
            }
            return true;
        }
        let Some(target) = eyedropper_target(ctx) else {
            return false;
        };
        let debug_at = ctx.data(|d| d.get_temp::<Pos2>(gid("debug-hover")));
        if let Some(at) = resp.hover_pos().or(debug_at) {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            let (x, y) = to_doc(at);
            let under = self.last_flat.as_ref().and_then(|f| sample_srgb(f, x, y));
            paint_loupe(ctx, at, under);
            self.status = match under {
                Some(c) => format!(
                    "Eyedropper: {} at {}, {} — click to sample, Esc cancels",
                    format_hex(c),
                    x as i32,
                    y as i32
                ),
                None => "Eyedropper: click inside the image to sample — Esc cancels".into(),
            };
        }
        let debug_click = ctx.data_mut(|d| d.remove_temp::<Pos2>(gid("debug-click")));
        let primary = egui::PointerButton::Primary;
        let click = if resp.clicked_by(primary) || resp.drag_stopped_by(primary) {
            resp.interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.interact_pos()))
        } else {
            debug_click
        };
        if let Some(at) = click {
            let (x, y) = to_doc(at);
            match self.last_flat.as_ref().and_then(|f| sample_srgb(f, x, y)) {
                Some(c) => {
                    let frame = ctx.cumulative_pass_nr();
                    ctx.data_mut(|d| {
                        d.insert_temp(
                            gid("sample"),
                            Sample {
                                target,
                                rgb: c,
                                frame,
                            },
                        )
                    });
                    disarm(ctx);
                    // Keep (or bring back) the picker that asked.
                    ctx.memory_mut(|m| m.open_popup(target));
                    ctx.request_repaint();
                    self.status = format!("Sampled {} at {}, {}", format_hex(c), x as i32, y as i32);
                }
                None => self.status = "Click inside the image to sample a colour".into(),
            }
        }
        true
    }

    /// `--screenshot-do` tokens for the colour picker (`color:...`).
    pub(crate) fn debug_color_token(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("color:") else {
            return false;
        };
        let xy = |s: &str| -> Option<Pos2> {
            let (x, y) = s.split_once('-')?;
            Some(egui::pos2(x.parse().ok()?, y.parse().ok()?))
        };
        match rest {
            "fg" => ctx.memory_mut(|m| m.open_popup(fg_picker())),
            "bg" => ctx.memory_mut(|m| m.open_popup(bg_picker())),
            "fg-eyedropper" => {
                ctx.memory_mut(|m| m.open_popup(fg_picker()));
                arm(ctx, fg_picker());
                say(
                    ctx,
                    "Eyedropper: click the canvas to sample a colour — Esc cancels",
                );
            }
            "recent" => {
                // Session-only demo swatches (never written to disk).
                let list = [
                    "#E85D5D", "#FFB547", "#F2E8CF", "#6A994E", "#386641", "#9FC9FF", "#1A2E8C",
                ]
                .iter()
                .filter_map(|h| parse_hex(h).map(to_u8))
                .collect();
                ctx.data_mut(|d| d.insert_temp(gid("recent"), Recent(list)));
            }
            "tool-gradient" => self.tool = Tool::Gradient,
            "tool-text" => self.tool = Tool::Text,
            "tool-brush" => self.tool = Tool::Brush,
            "fx" => {
                if let Some(layer) = self.active {
                    let fx = lumenply_doc::LayerEffects {
                        drop_shadow: Some(Default::default()),
                        outer_glow: Some(Default::default()),
                        stroke: Some(Default::default()),
                        color_overlay: Some(Default::default()),
                        ..Default::default()
                    };
                    self.run(&SetLayerEffects { layer, effects: fx });
                }
            }
            "add-text" => {
                let t = TextLayer::new("Colour", 420.0, 360.0, 96.0, linear_rgba(self.brush_rgb, 1.0));
                let id = self.editor.doc().next_id();
                self.run(&AddTextLayer {
                    text: t,
                    above: self.active,
                });
                self.set_active(Some(id));
            }
            _ => {
                if let Some(n) = rest.strip_prefix("open-").and_then(|n| n.parse::<usize>().ok()) {
                    ctx.data_mut(|d| d.insert_temp(gid("debug-open"), n));
                } else if let Some(c) = rest.strip_prefix("set-").and_then(parse_hex) {
                    self.brush_rgb = c;
                } else if let Some(p) = rest.strip_prefix("hover-").and_then(xy) {
                    ctx.data_mut(|d| d.insert_temp(gid("debug-hover"), p));
                } else if let Some(p) = rest.strip_prefix("click-").and_then(xy) {
                    ctx.data_mut(|d| d.insert_temp(gid("debug-click"), p));
                } else {
                    return false;
                }
            }
        }
        true
    }
}

/// Popup id of the colour well's foreground picker.
pub(crate) fn fg_picker() -> egui::Id {
    gid("foreground")
}

/// Popup id of the colour well's background picker.
pub(crate) fn bg_picker() -> egui::Id {
    gid("background")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn hsv_of_primaries_and_greys() {
        assert!(close(rgb_to_hsv([1.0, 0.0, 0.0]), [0.0, 1.0, 1.0]));
        assert!(close(rgb_to_hsv([0.0, 1.0, 0.0]), [1.0 / 3.0, 1.0, 1.0]));
        assert!(close(rgb_to_hsv([0.0, 0.0, 1.0]), [2.0 / 3.0, 1.0, 1.0]));
        assert!(close(rgb_to_hsv([1.0, 0.0, 1.0]), [5.0 / 6.0, 1.0, 1.0]));
        // #1A2E8C, the default brush colour: h = 229.47°, s = 0.814, v = 0.549.
        let hsv = rgb_to_hsv([26.0 / 255.0, 46.0 / 255.0, 140.0 / 255.0]);
        assert!((hsv[0] * 360.0 - 229.47).abs() < 0.01, "{hsv:?}");
        assert!((hsv[1] - 0.814_285_7).abs() < 1e-5);
        assert!((hsv[2] - 140.0 / 255.0).abs() < 1e-6);
        assert!(close(rgb_to_hsv([0.5, 0.5, 0.5]), [0.0, 0.0, 0.5]));
        assert!(close(rgb_to_hsv([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]));
    }

    #[test]
    fn hsv_to_rgb_hits_each_sector() {
        assert!(close(hsv_to_rgb([0.0, 1.0, 1.0]), [1.0, 0.0, 0.0]));
        assert!(close(hsv_to_rgb([1.0 / 6.0, 1.0, 1.0]), [1.0, 1.0, 0.0]));
        assert!(close(hsv_to_rgb([0.5, 1.0, 1.0]), [0.0, 1.0, 1.0]));
        assert!(close(hsv_to_rgb([0.75, 0.5, 0.8]), [0.6, 0.4, 0.8]));
        // Hue 1.0 wraps to red, as at the right end of the strip.
        assert!(close(hsv_to_rgb([1.0, 1.0, 1.0]), [1.0, 0.0, 0.0]));
        assert!(close(hsv_to_rgb([0.3, 0.0, 0.25]), [0.25, 0.25, 0.25]));
    }

    #[test]
    fn rgb_hsv_round_trip_on_a_grid() {
        for r in 0..=8 {
            for g in 0..=8 {
                for b in 0..=8 {
                    let c = [r as f32 / 8.0, g as f32 / 8.0, b as f32 / 8.0];
                    let back = hsv_to_rgb(rgb_to_hsv(c));
                    assert!(close(c, back), "{c:?} -> {back:?}");
                }
            }
        }
    }

    #[test]
    fn greys_and_black_keep_the_previous_hue() {
        let prev = [0.6, 0.7, 0.8];
        // Grey: no hue of its own, so the previous one stays.
        assert!(close(hsv_keeping(prev, [0.4, 0.4, 0.4]), [0.6, 0.0, 0.4]));
        // Black: neither hue nor saturation.
        assert!(close(hsv_keeping(prev, [0.0, 0.0, 0.0]), [0.6, 0.7, 0.0]));
        // A real colour replaces both.
        assert!(close(hsv_keeping(prev, [1.0, 0.0, 0.0]), [0.0, 1.0, 1.0]));
    }

    #[test]
    fn hex_parses_long_short_and_bare_forms() {
        let red = Some([1.0, 0.0, 0.0]);
        assert_eq!(parse_hex("#FF0000"), red);
        assert_eq!(parse_hex("ff0000"), red);
        assert_eq!(parse_hex("  #f00 "), red);
        assert_eq!(parse_hex("F00"), red);
        assert_eq!(
            parse_hex("#1a2e8c"),
            Some([26.0 / 255.0, 46.0 / 255.0, 140.0 / 255.0])
        );
        // #ABC expands each digit: AA BB CC.
        assert_eq!(
            parse_hex("#abc"),
            Some([170.0 / 255.0, 187.0 / 255.0, 204.0 / 255.0])
        );
        for bad in [
            "", "#", "#FF00", "#FF00000", "GG0000", "#12345Z", "##FF0000", "#ÄÄÄ", "+12345",
        ] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn hex_formats_rounded_upper_case_and_round_trips() {
        assert_eq!(format_hex([26.0 / 255.0, 46.0 / 255.0, 140.0 / 255.0]), "#1A2E8C");
        assert_eq!(format_hex([0.10, 0.18, 0.55]), "#1A2E8C");
        assert_eq!(format_hex([1.0, 0.0, 0.5]), "#FF0080");
        // Out-of-range channels clamp.
        assert_eq!(format_hex([1.5, -0.2, 0.0]), "#FF0000");
        for v in [0u8, 1, 17, 128, 254, 255] {
            let c = [v as f32 / 255.0, 0.0, (255 - v) as f32 / 255.0];
            assert_eq!(parse_hex(&format_hex(c)), Some(c));
        }
        // 8-bit conversion rounds to nearest; byte colours (the canvas
        // surround preference) survive the trip through the picker.
        assert_eq!(to_u8([0.0, 0.5, 1.0]), [0, 128, 255]);
        assert_eq!(to_u8([0.501 / 255.0, 1.499 / 255.0, 0.2]), [1, 1, 51]);
        for b in [
            [0x14u8, 0x16, 0x19],
            [0, 0, 0],
            [0x80, 0x80, 0x80],
            [0xD8, 0xD8, 0xD8],
        ] {
            assert_eq!(to_u8(b.map(|x| x as f32 / 255.0)), b);
        }
    }

    #[test]
    fn sampling_reads_straight_srgb_at_the_pixel() {
        let mut flat = Raster::new(4, 3);
        // Linear 0.5 grey, fully opaque, encodes to sRGB 188.
        flat.set(2, 1, lumenply_tiles::Rgba::new(0.5, 0.5, 0.5, 1.0));
        // Premultiplied half-transparent pure red: straight red 1.0.
        flat.set(3, 2, lumenply_tiles::Rgba::new(0.5, 0.0, 0.0, 0.5));
        let g = 188.0 / 255.0;
        assert_eq!(sample_srgb(&flat, 2.7, 1.2), Some([g, g, g]));
        assert_eq!(sample_srgb(&flat, 3.0, 2.0), Some([1.0, 0.0, 0.0]));
        assert_eq!(sample_srgb(&flat, 0.0, 0.0), Some([0.0, 0.0, 0.0]));
        assert_eq!(sample_srgb(&flat, 4.0, 0.0), None);
        assert_eq!(sample_srgb(&flat, 0.0, 3.0), None);
        assert_eq!(sample_srgb(&flat, -0.5, 1.0), None);
        assert_eq!(sample_srgb(&flat, f32::NAN, 1.0), None);
    }

    /// One egui frame with `f` drawing into a central panel.
    fn frame(ctx: &egui::Context, f: &mut dyn FnMut(&mut egui::Ui)) {
        frame_with(ctx, Vec::new(), f);
    }

    fn frame_with(ctx: &egui::Context, events: Vec<egui::Event>, f: &mut dyn FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| f(ui));
        });
    }

    fn press(at: Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]
    }

    /// Draws one colour button with its picker open; returns the popup
    /// id and the popup's screen rect. The canvas is the right half.
    fn open_picker(ctx: &egui::Context, rgb: &mut [f32; 3]) -> (egui::Id, egui::Rect) {
        let canvas = egui::Rect::from_min_max(egui::pos2(600.0, 0.0), egui::pos2(1200.0, 800.0));
        ctx.data_mut(|d| d.insert_temp(gid("canvas"), canvas));
        let mut id = egui::Id::NULL;
        frame(ctx, &mut |ui| {
            id = color_edit_button_rgb(ui, rgb).id.with("color-picker")
        });
        ctx.memory_mut(|m| m.open_popup(id));
        for _ in 0..3 {
            frame(ctx, &mut |ui| {
                color_edit_button_rgb(ui, rgb);
            });
        }
        let rect = ctx.memory(|m| m.area_rect(id)).expect("picker drawn");
        (id, rect)
    }

    #[test]
    fn a_press_on_the_canvas_closes_the_picker_and_is_swallowed() {
        let ctx = quiet_ctx();
        let mut rgb = [0.2, 0.4, 0.6];
        let (id, rect) = open_picker(&ctx, &mut rgb);
        assert_eq!(rect.width(), W + 20.0, "fixed width: content plus margins");
        assert!(rect.right() < 600.0, "picker sits left of the test canvas");
        // A press inside the picker keeps it open and leaves the canvas alone.
        frame_with(&ctx, press(rect.center()), &mut |ui| {
            color_edit_button_rgb(ui, &mut rgb);
        });
        assert!(ctx.memory(|m| m.is_popup_open(id)));
        assert!(ctx.data(|d| d.get_temp::<bool>(gid("swallow"))).is_none());
        // A press on the canvas closes it and must not reach a tool.
        frame_with(&ctx, press(egui::pos2(900.0, 400.0)), &mut |ui| {
            color_edit_button_rgb(ui, &mut rgb);
        });
        assert!(!ctx.memory(|m| m.is_popup_open(id)));
        assert_eq!(ctx.data(|d| d.get_temp::<bool>(gid("swallow"))), Some(true));
    }

    #[test]
    fn the_square_sets_saturation_and_value_and_reports_like_a_slider() {
        let ctx = quiet_ctx();
        // h = 0.5833 (210°), s = 0.667, v = 0.6.
        let mut rgb = [0.2, 0.4, 0.6];
        let (_, rect) = open_picker(&ctx, &mut rgb);
        // The square is the first thing inside the 10 px margin; press at
        // half saturation, a quarter of the way down (v = 0.75).
        let at = rect.min + egui::vec2(10.0 + W * 0.5, 10.0 + SV_H * 0.25);
        let mut seen = (false, false, false);
        frame_with(&ctx, press(at), &mut |ui| {
            let r = color_edit_button_rgb(ui, &mut rgb);
            seen = (r.changed(), r.dragged(), is_dragging(ui.ctx()));
        });
        // While the button is down: changed, dragging, undo step held open.
        assert_eq!(seen, (true, true, true));
        // hsv (0.5833, 0.5, 0.75) is rgb (0.375, 0.5625, 0.75).
        assert!(close(rgb, [0.375, 0.5625, 0.75]), "{rgb:?}");
        let release = vec![egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }];
        let mut end = (false, false, false);
        frame_with(&ctx, release, &mut |ui| {
            let r = color_edit_button_rgb(ui, &mut rgb);
            end = (r.changed(), r.drag_stopped(), is_dragging(ui.ctx()));
        });
        // Release: one more `changed` so listeners close the undo step.
        assert_eq!(end, (true, true, false));
        assert!(close(rgb, [0.375, 0.5625, 0.75]));
    }

    fn key(k: Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    /// Focus the hex field, clear it, type `text`, then press Enter.
    /// Returns whether the field showed the text as invalid before Enter.
    fn type_hex(ctx: &egui::Context, id: egui::Id, rgb: &mut [f32; 3], text: &str) -> bool {
        ctx.memory_mut(|m| m.request_focus(id.with("hex")));
        frame(ctx, &mut |ui| {
            color_edit_button_rgb(ui, rgb);
        });
        let mut events = vec![key(Key::End)];
        events.extend((0..12).map(|_| key(Key::Backspace)));
        events.push(egui::Event::Text(text.into()));
        frame_with(ctx, events, &mut |ui| {
            color_edit_button_rgb(ui, rgb);
        });
        let st: PickerState = ctx.data(|d| d.get_temp(id.with("state"))).unwrap();
        assert_eq!(st.hex, text);
        frame_with(ctx, vec![key(Key::Enter)], &mut |ui| {
            color_edit_button_rgb(ui, rgb);
        });
        st.hex_bad
    }

    #[test]
    fn the_hex_field_takes_short_forms_and_rejects_garbage() {
        let ctx = quiet_ctx();
        let mut rgb = [0.2, 0.4, 0.6];
        let (id, _) = open_picker(&ctx, &mut rgb);
        // Three digits, no '#': valid, applied on Enter as #FF0000.
        assert!(!type_hex(&ctx, id, &mut rgb, "f00"));
        assert_eq!(rgb, [1.0, 0.0, 0.0]);
        // Six digits apply as typed.
        assert!(!type_hex(&ctx, id, &mut rgb, "#1a2e8c"));
        assert_eq!(rgb, [26.0 / 255.0, 46.0 / 255.0, 140.0 / 255.0]);
        // Garbage shows as invalid and Enter leaves the colour alone; the
        // field then reads the colour again.
        assert!(type_hex(&ctx, id, &mut rgb, "#12zz"));
        assert_eq!(rgb, [26.0 / 255.0, 46.0 / 255.0, 140.0 / 255.0]);
        frame(&ctx, &mut |ui| {
            color_edit_button_rgb(ui, &mut rgb);
        });
        let st: PickerState = ctx.data(|d| d.get_temp(id.with("state"))).unwrap();
        assert_eq!((st.hex.as_str(), st.hex_bad), ("#1A2E8C", false));
    }

    #[test]
    fn with_the_eyedropper_armed_a_canvas_press_keeps_the_picker() {
        let ctx = quiet_ctx();
        let mut rgb = [0.2, 0.4, 0.6];
        let (id, _) = open_picker(&ctx, &mut rgb);
        arm(&ctx, id);
        frame_with(&ctx, press(egui::pos2(900.0, 400.0)), &mut |ui| {
            color_edit_button_rgb(ui, &mut rgb);
        });
        // The press is the sample (taken by the canvas hook): picker open,
        // eyedropper still armed, nothing swallowed.
        assert!(ctx.memory(|m| m.is_popup_open(id)));
        assert_eq!(eyedropper_target(&ctx), Some(id));
        assert!(ctx.data(|d| d.get_temp::<bool>(gid("swallow"))).is_none());
        // Esc cancels the eyedropper first and only then closes the picker.
        let esc = || {
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }]
        };
        frame_with(&ctx, esc(), &mut |ui| {
            color_edit_button_rgb(ui, &mut rgb);
        });
        assert_eq!(eyedropper_target(&ctx), None);
        assert!(ctx.memory(|m| m.is_popup_open(id)));
        frame_with(&ctx, esc(), &mut |ui| {
            color_edit_button_rgb(ui, &mut rgb);
        });
        assert!(!ctx.memory(|m| m.is_popup_open(id)));
        assert_eq!(rgb, [0.2, 0.4, 0.6]);
    }

    /// A context that never touches the user's recent-colours file.
    fn quiet_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(gid("no-persist"), true);
            d.insert_temp(gid("recent"), Recent(Vec::new()));
        });
        ctx
    }

    #[test]
    fn a_canvas_sample_reaches_only_the_picker_that_asked() {
        let ctx = quiet_ctx();
        let (mut a, mut b) = ([0.1, 0.2, 0.3], [0.4, 0.5, 0.6]);
        let mut ids = (egui::Id::NULL, egui::Id::NULL);
        frame(&ctx, &mut |ui| {
            let ra = color_edit_button_rgb(ui, &mut a);
            let rb = color_edit_button_rgb(ui, &mut b);
            ids = (ra.id.with("color-picker"), rb.id.with("color-picker"));
        });
        let now = ctx.cumulative_pass_nr();
        let rgb = [1.0, 0.5, 0.0];
        ctx.data_mut(|d| {
            d.insert_temp(
                gid("sample"),
                Sample {
                    target: ids.0,
                    rgb,
                    frame: now,
                },
            )
        });
        let mut seen = (false, false, false);
        frame(&ctx, &mut |ui| {
            let ra = color_edit_button_rgb(ui, &mut a);
            let rb = color_edit_button_rgb(ui, &mut b);
            seen = (ra.changed(), ra.drag_stopped(), rb.changed());
        });
        assert_eq!(a, [1.0, 0.5, 0.0]);
        assert_eq!(b, [0.4, 0.5, 0.6]);
        // One finished edit for the target, nothing for its neighbour.
        assert_eq!(seen, (true, true, false));
        assert_eq!(recent(&ctx), vec![[255, 128, 0]]);
    }

    #[test]
    fn a_stale_sample_is_dropped() {
        let ctx = quiet_ctx();
        let mut a = [0.1, 0.2, 0.3];
        let mut id = egui::Id::NULL;
        for _ in 0..4 {
            frame(&ctx, &mut |ui| {
                id = color_edit_button_rgb(ui, &mut a).id.with("color-picker")
            });
        }
        // Delivered three passes ago: the picker it was for has gone.
        let old = ctx.cumulative_pass_nr() - 3;
        let s = Sample {
            target: id,
            rgb: [1.0, 1.0, 1.0],
            frame: old,
        };
        ctx.data_mut(|d| d.insert_temp(gid("sample"), s));
        frame(&ctx, &mut |ui| {
            color_edit_button_rgb(ui, &mut a);
        });
        assert_eq!(a, [0.1, 0.2, 0.3]);
        assert!(ctx.data(|d| d.get_temp::<Sample>(gid("sample"))).is_none());
    }

    #[test]
    fn an_armed_eyedropper_lapses_when_its_picker_stops_drawing() {
        let ctx = quiet_ctx();
        frame(&ctx, &mut |_| {});
        let target = egui::Id::new("some picker");
        arm(&ctx, target);
        assert_eq!(eyedropper_target(&ctx), Some(target));
        frame(&ctx, &mut |_| {});
        frame(&ctx, &mut |_| {});
        // Two passes without a refresh still count (dialogs draw after the
        // canvas, so their pickers refresh a pass late)...
        assert_eq!(eyedropper_target(&ctx), Some(target));
        frame(&ctx, &mut |_| {});
        // ...three do not.
        assert_eq!(eyedropper_target(&ctx), None);
        arm(&ctx, target);
        disarm(&ctx);
        assert_eq!(eyedropper_target(&ctx), None);
    }

    #[test]
    fn auto_placement_prefers_below_then_above() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        let swatch = |y: f32| egui::Rect::from_min_size(egui::pos2(300.0, y), egui::vec2(34.0, 20.0));
        // Options bar: room below.
        assert_eq!(
            auto_place(swatch(51.0), 360.0, screen),
            (egui::pos2(300.0, 77.0), Align2::LEFT_TOP)
        );
        // Near the bottom (720 + 6 + 360 > 996): above instead.
        assert_eq!(
            auto_place(swatch(700.0), 360.0, screen),
            (egui::pos2(300.0, 694.0), Align2::LEFT_BOTTOM)
        );
        // Room on neither side: below, and the area clamps it on screen.
        let short = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 400.0));
        assert_eq!(
            auto_place(swatch(190.0), 360.0, short),
            (egui::pos2(300.0, 216.0), Align2::LEFT_TOP)
        );
    }

    #[test]
    fn recent_list_is_newest_first_unique_and_capped() {
        let mut l = Vec::new();
        for i in 0..12u8 {
            push_recent(&mut l, [i, 0, 0]);
        }
        assert_eq!(l.len(), RECENT_MAX);
        assert_eq!(l[0], [11, 0, 0]);
        assert_eq!(l[9], [2, 0, 0]);
        // Re-using a colour moves it to the front instead of duplicating.
        push_recent(&mut l, [5, 0, 0]);
        assert_eq!(l.len(), RECENT_MAX);
        assert_eq!(l[0], [5, 0, 0]);
        assert_eq!(l[1], [11, 0, 0]);
        assert_eq!(l.iter().filter(|c| **c == [5, 0, 0]).count(), 1);
    }
}
