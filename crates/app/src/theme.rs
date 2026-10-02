//! "Graphite & Signal" visual language: near-black graphite surfaces,
//! one warm signal colour for everything active, IBM Plex for the type.

use super::*;

/// Window background and the canvas surround.
pub(crate) const GROUND: Color32 = Color32::from_rgb(0x14, 0x16, 0x19);
/// Panels: tool rail, docks, bars.
pub(crate) const PANEL: Color32 = Color32::from_rgb(0x1C, 0x1F, 0x23);
/// Raised interactive surfaces: buttons, fields, rows.
pub(crate) const RAISED: Color32 = Color32::from_rgb(0x24, 0x28, 0x2D);
/// Hairlines and separators.
pub(crate) const LINE: Color32 = Color32::from_rgb(0x2E, 0x33, 0x39);
/// Primary text.
pub(crate) const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEE, 0xF1);
/// Secondary text: labels, shortcut hints, section titles.
pub(crate) const MUTED: Color32 = Color32::from_rgb(0xA7, 0xAE, 0xB8);
/// The signal: active tool, selection, sliders, primary button.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(0xFF, 0xB5, 0x47);
/// Text drawn on top of the accent.
pub(crate) const ACCENT_INK: Color32 = Color32::from_rgb(0x1A, 0x13, 0x00);
/// The active layer row's fill: RAISED warmed faintly toward the accent.
pub(crate) const ACCENT_TINT: Color32 = Color32::from_rgb(0x41, 0x3A, 0x2D);
/// Live (non-destructive) filter layers.
pub(crate) const LIVE_FILTER: Color32 = Color32::from_rgb(0x9F, 0xC9, 0xFF);
/// Hover fill for flat buttons and custom-painted hit areas.
pub(crate) const HOVER: Color32 = Color32::from_rgb(0x2B, 0x30, 0x36);
/// Checkbox boxes, slider rails and handles, scroll handles.
pub(crate) const CONTROL: Color32 = Color32::from_rgb(0x38, 0x3E, 0x46);
/// Destructive or warning ink (quick mask, disabled masks).
pub(crate) const DANGER: Color32 = Color32::from_rgb(0xE8, 0x5D, 0x5D);
/// Corner radius of buttons, fields, chips and rows.
pub(crate) const RADIUS: f32 = 6.0;

pub(crate) const THUMB: (usize, usize) = (44, 30);

/// Install fonts, palette and widget styling on the context. Called once.
pub(crate) fn install(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};

    // Graphite & Signal has one look. Without this, eframe follows the
    // system theme every frame and replaces our visuals with egui's stock
    // light palette on a light-mode OS.
    ctx.set_theme(egui::ThemePreference::Dark);

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "plex-sans".into(),
        FontData::from_static(include_bytes!("../../render/fonts/IBMPlexSans-Regular.ttf")),
    );
    fonts.font_data.insert(
        "plex-sans-semibold".into(),
        FontData::from_static(include_bytes!("../../render/fonts/IBMPlexSans-SemiBold.ttf")),
    );
    fonts.font_data.insert(
        "plex-mono".into(),
        FontData::from_static(include_bytes!("../../render/fonts/IBMPlexMono-Regular.ttf")),
    );
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "plex-sans".into());
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(0, "plex-mono".into());
    fonts.families.insert(
        FontFamily::Name("semibold".into()),
        vec!["plex-sans-semibold".into(), "plex-sans".into()],
    );
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.extreme_bg_color = GROUND;
    v.faint_bg_color = RAISED;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, ACCENT_INK);
    v.hyperlink_color = ACCENT;
    v.slider_trailing_fill = true;

    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    // `bg_fill` paints checkbox and radio boxes, slider rails and handles
    // and scroll handles: it must read against both PANEL and RAISED (a
    // dialog), so it sits a clear step above both.
    v.widgets.inactive.bg_fill = CONTROL;
    // Buttons sit flat on their panel until hovered; raised chrome is opted
    // into per region with `raise_controls` (bars, docks, dialogs) or an
    // explicit fill (Export, tool rail).
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x45, 0x4C, 0x55);
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3D, 0x44, 0x4C));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    // Pressed and keyboard-focused widgets share these: the accent outline
    // is the visible focus indicator. (fg_stroke's colour doubles as the
    // `strong()` text colour, so it stays TEXT.)
    v.widgets.active.bg_fill = Color32::from_rgb(0x4A, 0x52, 0x5B);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x32, 0x38, 0x3F);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.5, TEXT);
    v.widgets.open.bg_fill = RAISED;
    v.widgets.open.weak_bg_fill = RAISED;
    v.widgets.open.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);

    // Soft depth: rounded corners everywhere, gentle shadows on anything
    // that floats.
    let r = egui::Rounding::same(RADIUS);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = r;
    }
    v.window_rounding = egui::Rounding::same(10.0);
    v.menu_rounding = egui::Rounding::same(8.0);
    let shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 6.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(110),
    };
    v.window_shadow = shadow;
    v.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 14.0,
        spread: 0.0,
        color: Color32::from_black_alpha(90),
    };
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 5.0);
    style.spacing.menu_margin = egui::Margin::symmetric(10.0, 8.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.slider_width = 120.0;
    style.spacing.combo_width = 120.0;
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    use egui::TextStyle::*;
    style.text_styles.insert(Body, FontId::proportional(13.0));
    style.text_styles.insert(Button, FontId::proportional(13.0));
    style.text_styles.insert(Small, FontId::proportional(10.5));
    style.text_styles.insert(Heading, FontId::proportional(16.0));
    style.text_styles.insert(Monospace, FontId::monospace(12.5));
    // Numbers are set in Plex Mono everywhere, including slider values
    // and number fields.
    style.drag_value_text_style = Monospace;
    install_popups(&mut style);
    ctx.set_style(style);
}

/// Width of the label column in label-left rows (sliders, fields).
pub(crate) const LABEL_W: f32 = 84.0;

/// Give every button, combo box, selectable chip and number field in
/// `ui` (and its children) a visible raised frame, so controls read as
/// controls and not as text. Menus keep the flat default.
pub(crate) fn raise_controls(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x30, 0x36, 0x3D);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x45, 0x4C, 0x55));
    w.active.weak_bg_fill = Color32::from_rgb(0x36, 0x3D, 0x45);
}

/// The accent call-to-action button (OK, Apply, Export).
pub(crate) fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(ACCENT_INK).strong())
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(72.0, 26.0))
}

/// A standard footer button at the shared size (Cancel, Discard...).
pub(crate) fn footer_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(text.to_string()).min_size(egui::vec2(72.0, 26.0))
}

/// Paint the keyboard-focus ring around a custom-painted widget.
/// A checkbox with a square-ish box. egui draws checkboxes with the
/// widgets' corner radius, which on a 14 px box reads as a radio button.
pub(crate) fn check(
    ui: &mut egui::Ui,
    value: &mut bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.visuals_mut().widgets;
        for w in [&mut v.inactive, &mut v.hovered, &mut v.active, &mut v.open] {
            w.rounding = egui::Rounding::same(3.0);
        }
        ui.checkbox(value, label)
    })
    .inner
}

pub(crate) fn focus_ring(ui: &egui::Ui, resp: &egui::Response, rect: egui::Rect, rounding: f32) {
    if resp.has_focus() {
        ui.painter()
            .rect_stroke(rect.expand(1.5), rounding + 1.5, Stroke::new(1.5, ACCENT));
    }
}

/// Lay `text` out on one line no wider than `max_w`, ending in "…" when
/// it doesn't fit. Returns the galley and whether it was shortened.
pub(crate) fn elided(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> (std::sync::Arc<egui::Galley>, bool) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(8.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.fonts(|f| f.layout_job(job));
    let cut = galley.elided;
    (galley, cut)
}

/// A segmented control: one framed strip of mutually exclusive options,
/// the chosen one filled with the accent. Returns true on change.
pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    egui::Frame::none()
        .stroke(Stroke::new(1.0, LINE))
        .rounding(RADIUS)
        .inner_margin(egui::Margin::same(2.0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.spacing_mut().button_padding = egui::vec2(9.0, 2.0);
            // Unselected segments sit flat inside the frame.
            let w = &mut ui.visuals_mut().widgets;
            w.inactive.weak_bg_fill = Color32::TRANSPARENT;
            w.inactive.bg_stroke = Stroke::NONE;
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.bg_stroke = Stroke::NONE;
            ui.horizontal(|ui| {
                for (v, label) in options {
                    let on = *value == *v;
                    if ui.add(egui::SelectableLabel::new(on, *label)).clicked() && !on {
                        *value = *v;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A number field: a framed, mono, typeable value (drag to scrub). The
/// value commits on Enter or focus loss, not on every keystroke.
pub(crate) fn num_field(ui: &mut egui::Ui, dv: egui::DragValue<'_>, width: f32) -> egui::Response {
    ui.scope(|ui| {
        raise_controls(ui);
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.weak_bg_fill = GROUND;
        w.hovered.weak_bg_fill = GROUND;
        w.active.weak_bg_fill = GROUND;
        ui.spacing_mut().interact_size.x = width;
        ui.add_sized([width, 22.0], dv.update_while_editing(false))
    })
    .inner
}

/// A bar across the top or bottom: panel fill, fixed height, side padding,
/// vertically centred content.
pub(crate) fn bar_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .inner_margin(egui::Margin::symmetric(12.0, 0.0))
}

pub(crate) fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).small().strong().color(MUTED));
}

/// The muted label cell of a label-left row, `w` wide; longer text is
/// elided with the full label in a tooltip.
pub(crate) fn row_label(ui: &mut egui::Ui, text: &str, w: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, 22.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(w);
            ui.add(egui::Label::new(RichText::new(text).color(MUTED)).truncate());
        },
    );
}

/// A labelled slider row: muted label on the left, slider filling the
/// middle, a typeable mono value on the right. Returns true when the user
/// finished an edit (drag released or value typed).
pub(crate) fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    slider_row_ex(ui, label, v, range, suffix, RowOpts::default())
}

/// A slider row over a value stored in other units, shown as `v × scale`
/// in whole numbers (a 0–1 opacity as 0–100 %, a −1…1 shift as −100…100,
/// a 0–1 level as 0–255). The value is written back only when the row
/// changes it, so an untouched row never drifts by float rounding.
pub(crate) fn slider_row_scaled(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    scale: f32,
    suffix: &str,
) -> bool {
    slider_row_scaled_w(ui, label, v, range, scale, suffix, LABEL_W)
}

/// [`slider_row_scaled`] with a wider label column (long pair labels).
pub(crate) fn slider_row_scaled_w(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    scale: f32,
    suffix: &str,
    label_w: f32,
) -> bool {
    let shown_before = *v * scale;
    let mut shown = shown_before;
    let r = (range.start() * scale)..=(range.end() * scale);
    // Not `int`: egui's integer slider rounds the value as soon as it is
    // drawn, which would edit the document just by showing the panel. The
    // spans here are ≥ 100 units, so the field already shows whole numbers.
    let opts = RowOpts {
        label_w,
        ..RowOpts::default()
    };
    let finished = slider_row_ex(ui, label, &mut shown, r, suffix, opts);
    if shown != shown_before {
        *v = shown / scale;
    }
    finished
}

/// [`slider_row`] on a logarithmic scale (radii, sizes).
pub(crate) fn slider_row_log(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    let opts = RowOpts {
        log: true,
        ..RowOpts::default()
    };
    slider_row_ex(ui, label, v, range, suffix, opts)
}

/// Options for [`slider_row_ex`].
#[derive(Clone, Copy)]
pub(crate) struct RowOpts {
    /// Width of the label column.
    pub label_w: f32,
    /// Logarithmic slider (radii, sizes, memory).
    pub log: bool,
    /// Whole numbers only.
    pub int: bool,
}

impl Default for RowOpts {
    fn default() -> Self {
        RowOpts {
            label_w: LABEL_W,
            log: false,
            int: false,
        }
    }
}

/// [`slider_row`] with an explicit label column width, scale and
/// precision.
pub(crate) fn slider_row_ex(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    opts: RowOpts,
) -> bool {
    let mut finished = false;
    let span = range.end() - range.start();
    let decimals = if opts.int || (span >= 10.0 && !(opts.log && *range.start() < 2.0)) {
        0
    } else if opts.log {
        1
    } else {
        2
    };
    ui.horizontal(|ui| {
        row_label(ui, label, opts.label_w);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let n = num_field(
                ui,
                egui::DragValue::new(v)
                    .range(range.clone())
                    .speed(span / 300.0)
                    .fixed_decimals(decimals)
                    .suffix(suffix),
                68.0,
            );
            ui.spacing_mut().slider_width = (ui.available_width() - 6.0).max(48.0);
            let mut slider = egui::Slider::new(v, range)
                .logarithmic(opts.log)
                .show_value(false);
            if opts.int {
                slider = slider.integer();
            }
            let r = ui.add(slider);
            finished = r.drag_stopped()
                || (r.changed() && !r.dragged())
                || n.drag_stopped()
                || (n.changed() && !n.dragged());
        });
    });
    finished
}

/// A labelled number-field row (`Width [1200 px]`) in the label-left
/// convention. Returns the field's response.
pub(crate) fn field_row(ui: &mut egui::Ui, label: &str, dv: egui::DragValue<'_>) -> egui::Response {
    ui.horizontal(|ui| {
        row_label(ui, label, LABEL_W);
        num_field(ui, dv, 96.0)
    })
    .inner
}

// ---- Popups & menus ------------------------------------------------------------------
//
// Every dropdown, popup, context menu and combo list goes through these
// helpers, so they share one look and one rule: a popup sizes to its
// content and an item never wraps (egui otherwise wraps text at the
// popup's current width, which for a popup sized to a 26 px icon button
// stacks the label one letter per line).

/// A hovered item in any menu, popup or list: a neutral lift off the
/// RAISED popup surface. The warm tint is kept for the current value.
pub(crate) const MENU_HOVER: Color32 = Color32::from_rgb(0x34, 0x3A, 0x42);
/// An item while the pointer is pressed on it.
const MENU_PRESS: Color32 = Color32::from_rgb(0x3C, 0x43, 0x4C);
/// Height of one item row.
pub(crate) const MENU_ITEM_H: f32 = 26.0;
/// No menu or popup list is narrower than this.
const MENU_MIN_W: f32 = 180.0;
/// Label inset from the edge of the item's highlight.
pub(crate) const MENU_PAD_X: f32 = 10.0;
/// Minimum space between an item's label and its shortcut.
const MENU_SHORTCUT_GAP: f32 = 32.0;
/// Width kept at the right of a toggle item for its check mark.
const MENU_CHECK_W: f32 = 18.0;
/// Corner radius of an item's highlight (the frame itself uses 8).
pub(crate) const MENU_ITEM_ROUNDING: f32 = 5.0;

/// Popup-wide values, applied once from [`install`]. The frame margin is
/// shared with tooltips, which use the same popup frame.
pub(crate) fn install_popups(style: &mut egui::Style) {
    style.spacing.menu_margin = egui::Margin::same(6.0);
    style.spacing.menu_spacing = 4.0;
    style.visuals.menu_rounding = egui::Rounding::same(8.0);
}

/// Style the inside of a menu, popup or combo list: one line per item,
/// a sensible minimum width, the shared item height, padding and
/// colours. The helpers below call it; a `ComboBox::show_ui` closure
/// calls it first thing.
pub(crate) fn popup_style(ui: &mut egui::Ui) {
    let s = ui.style_mut();
    s.wrap_mode = Some(egui::TextWrapMode::Extend);
    s.spacing.button_padding = egui::vec2(MENU_PAD_X, 3.0);
    s.spacing.item_spacing = egui::vec2(8.0, 1.0);
    s.spacing.interact_size.y = MENU_ITEM_H;
    let w = &mut s.visuals.widgets;
    for v in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        v.bg_stroke = Stroke::NONE;
        v.rounding = egui::Rounding::same(MENU_ITEM_ROUNDING);
        v.expansion = 0.0;
        v.fg_stroke = Stroke::new(1.0, TEXT);
    }
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.hovered.weak_bg_fill = MENU_HOVER;
    w.hovered.bg_fill = MENU_HOVER;
    w.active.weak_bg_fill = MENU_PRESS;
    w.active.bg_fill = MENU_PRESS;
    w.open.weak_bg_fill = MENU_HOVER;
    w.open.bg_fill = MENU_HOVER;
    // The current value in a list (a combo's selection) wears the active
    // layer row's look: warm tint, signal-coloured text.
    s.visuals.selection.bg_fill = ACCENT_TINT;
    s.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    ui.set_min_width(MENU_MIN_W);
}

/// One menu row: label on the left, the shortcut right-aligned in muted
/// text, and for toggles a check mark at the far right. Sizes to its
/// content; the menu stretches it to the full width.
struct MenuItem<'a> {
    label: &'a str,
    shortcut: &'a str,
    checked: Option<bool>,
}

impl egui::Widget for MenuItem<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let font = egui::TextStyle::Button.resolve(ui.style());
        let label = ui.painter().layout_no_wrap(self.label.to_owned(), font, TEXT);
        let keys = (!self.shortcut.is_empty()).then(|| {
            ui.painter()
                .layout_no_wrap(self.shortcut.to_owned(), FontId::proportional(12.0), MUTED)
        });
        let check_w = if self.checked.is_some() { MENU_CHECK_W } else { 0.0 };
        let mut width = 2.0 * MENU_PAD_X + label.size().x + check_w;
        if let Some(k) = &keys {
            width += MENU_SHORTCUT_GAP + k.size().x;
        }
        let (rect, resp) = ui.allocate_at_least(egui::vec2(width, MENU_ITEM_H), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), self.label));
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            if ui.is_enabled() && (resp.hovered() || resp.has_focus() || resp.highlighted()) {
                let fill = if resp.is_pointer_button_down_on() {
                    MENU_PRESS
                } else {
                    MENU_HOVER
                };
                p.rect_filled(rect, MENU_ITEM_ROUNDING, fill);
            }
            let cy = rect.center().y;
            let text_y = cy - label.size().y / 2.0;
            p.galley(egui::pos2(rect.left() + MENU_PAD_X, text_y), label, TEXT);
            // The shortcut keeps the shared right edge so shortcuts line
            // up down the menu; a toggle's check sits just left of it.
            let mut right = rect.right() - MENU_PAD_X;
            if let Some(k) = keys {
                right -= k.size().x;
                p.galley(egui::pos2(right, cy - k.size().y / 2.0), k, MUTED);
                right -= 8.0;
            }
            if self.checked == Some(true) {
                paint_check(p, egui::pos2(right - 6.0, cy), ACCENT);
            }
        }
        resp
    }
}

/// A small check mark centred on `c`.
fn paint_check(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let pts = vec![
        c + egui::vec2(-5.0, 0.0),
        c + egui::vec2(-1.5, 3.5),
        c + egui::vec2(5.0, -4.0),
    ];
    p.add(Shape::line(pts, Stroke::new(1.8, color)));
}

fn add_menu_item(ui: &mut egui::Ui, enabled: bool, item: MenuItem) -> egui::Response {
    let label = item.label;
    let r = ui.add_enabled(enabled, item);
    note_target(ui.ctx(), label, r.rect);
    if r.clicked() {
        ui.close_menu();
    }
    r
}

/// A menu item that closes its menu when clicked; true when it was.
pub(crate) fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> bool {
    menu_item_if(ui, true, label, shortcut)
}

/// [`menu_item`], greyed out and inert unless `enabled`.
pub(crate) fn menu_item_if(ui: &mut egui::Ui, enabled: bool, label: &str, shortcut: &str) -> bool {
    menu_item_response(ui, enabled, label, shortcut).clicked()
}

/// [`menu_item_if`] returning the response, for hover text.
pub(crate) fn menu_item_response(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    shortcut: &str,
) -> egui::Response {
    let item = MenuItem {
        label,
        shortcut,
        checked: None,
    };
    add_menu_item(ui, enabled, item)
}

/// A toggle item: a check mark at the right while `checked`.
pub(crate) fn menu_check(ui: &mut egui::Ui, checked: bool, label: &str, shortcut: &str) -> egui::Response {
    let item = MenuItem {
        label,
        shortcut,
        checked: Some(checked),
    };
    add_menu_item(ui, true, item)
}

/// A greyed line of explanation inside a menu ("Nothing yet").
pub(crate) fn menu_note(ui: &mut egui::Ui, text: &str) {
    let item = MenuItem {
        label: text,
        shortcut: "",
        checked: None,
    };
    ui.add_enabled(false, item);
}

/// A small muted caption that heads a group of items.
pub(crate) fn menu_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(MENU_PAD_X);
        ui.label(RichText::new(text).small().strong().color(MUTED));
    });
    ui.add_space(2.0);
}

/// The hairline between groups of items.
pub(crate) fn menu_separator(ui: &mut egui::Ui) {
    ui.add(egui::Separator::default().spacing(9.0));
}

/// A menu-bar menu, or a submenu inside another menu.
pub(crate) fn menu<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let r = ui.menu_button(title, |ui| {
        popup_style(ui);
        add(ui)
    });
    note_target(ui.ctx(), title, r.response.rect);
    r.inner
}

/// A menu opened by clicking `trigger` (an icon button, a chip, the
/// Export button). It behaves like a menu-bar menu (Esc or a click
/// outside closes it, items close it) but opens below the trigger, or
/// above when there is no room, and end-aligns when it would run off the
/// right edge.
pub(crate) fn button_menu<R>(trigger: &egui::Response, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let ctx = trigger.ctx.clone();
    let bar_id = trigger.id.with("popups:menu");
    let mut bar = egui::menu::BarState::load(&ctx, bar_id);
    egui::menu::MenuRoot::stationary_click_interaction(trigger, &mut bar);
    if let Some(root) = bar.as_ref() {
        place_menu(&ctx, root, trigger.rect, ctx.style().spacing.menu_spacing);
    }
    let inner = bar.show(trigger, |ui| {
        popup_style(ui);
        add(ui)
    });
    bar.store(&ctx, bar_id);
    inner.map(|r| r.inner)
}

/// Drop-in for `egui::menu::menu_custom_button` with the shared popup
/// style and [`button_menu`]'s placement.
pub(crate) fn menu_custom_button<R>(
    ui: &mut egui::Ui,
    button: egui::Button<'_>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let resp = ui.add(button);
    let inner = button_menu(&resp, add);
    egui::InnerResponse::new(inner, resp)
}

/// A right-click menu on `resp` (which must sense clicks). Opens at the
/// pointer, flipping left or up instead of sliding under the pointer when
/// it would cross the window edge.
pub(crate) fn context_menu(resp: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    let ctx = resp.ctx.clone();
    // egui's own id, so this stays the one context menu open app-wide.
    let bar_id = egui::Id::new("__egui::context_menu");
    let anchor_id = egui::Id::new("popups:context-anchor");
    if resp.secondary_clicked() {
        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
            ctx.data_mut(|d| d.insert_temp(anchor_id, p));
        }
    }
    let mut bar = egui::menu::BarState::load(&ctx, bar_id);
    egui::menu::MenuRoot::context_click_interaction(resp, &mut bar);
    if let Some(root) = bar.as_ref().filter(|r| r.id == resp.id) {
        if let Some(p) = ctx.data(|d| d.get_temp::<egui::Pos2>(anchor_id)) {
            place_menu(&ctx, root, egui::Rect::from_min_size(p, Vec2::ZERO), 0.0);
        }
    }
    bar.show(resp, |ui| {
        popup_style(ui);
        add(ui)
    });
    bar.store(&ctx, bar_id);
}

/// Put an open menu next to `anchor`: below it and start-aligned when it
/// fits, else above and/or end-aligned, always inside the window.
fn place_menu(ctx: &egui::Context, root: &egui::menu::MenuRoot, anchor: egui::Rect, gap: f32) {
    // egui names a menu's area after its root id; its size is known from
    // the previous frame (or the sizing pass on the first).
    let area = root.id.with("__menu");
    let mut state = root.menu_state.write();
    let size = ctx
        .memory(|m| m.area_rect(area))
        .map_or(state.rect.size(), |r| r.size());
    let pos = menu_pos(anchor, size, ctx.screen_rect().shrink(4.0), gap);
    state.rect = egui::Rect::from_min_size(pos, size);
}

/// Where a menu of `size` goes beside `anchor` within `screen`: below
/// and start-aligned when that fits, else above and/or end-aligned, and
/// clamped inside `screen` when it fits neither way.
fn menu_pos(anchor: egui::Rect, size: Vec2, screen: egui::Rect, gap: f32) -> egui::Pos2 {
    let x = if anchor.left() + size.x <= screen.right() {
        anchor.left()
    } else {
        anchor.right() - size.x
    };
    let y = if anchor.bottom() + gap + size.y <= screen.bottom() {
        anchor.bottom() + gap
    } else {
        anchor.top() - gap - size.y
    };
    egui::pos2(
        x.clamp(screen.left(), (screen.right() - size.x).max(screen.left())),
        y.clamp(screen.top(), (screen.bottom() - size.y).max(screen.top())),
    )
}

/// A shortcut written the way this platform writes it: "Shift+Cmd+Z" on
/// macOS ("⇧⌘Z" when the UI font has the symbols), "Ctrl+Shift+Z"
/// elsewhere.
pub(crate) fn shortcut_text(ctx: &egui::Context, modifiers: egui::Modifiers, key: Key) -> String {
    ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, key))
}

/// Remember where a named menu, item or trigger was drawn this frame, so
/// `--screenshot-do popups:click=<name>` can aim real pointer input at it.
pub(crate) fn note_target(ctx: &egui::Context, name: &str, rect: egui::Rect) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("popups:target").with(name), rect));
}

/// Where [`note_target`] last saw `name`.
pub(crate) fn target_rect(ctx: &egui::Context, name: &str) -> Option<egui::Rect> {
    ctx.data(|d| d.get_temp(egui::Id::new("popups:target").with(name)))
}

#[cfg(test)]
mod popup_tests {
    use super::*;

    #[test]
    fn scaled_rows_never_drift_values_they_do_not_change() {
        // 0.6 × 100 ÷ 100 is not 0.6 in f32; a row that wrote back every
        // frame would nudge the value and record an undo step per frame.
        let ctx = egui::Context::default();
        let mut vals = [0.6f32, 0.07, -0.06, 0.333_333_34, 1.0];
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    for v in vals.iter_mut() {
                        slider_row_scaled(ui, "Opacity", v, -1.0..=1.0, 100.0, "%");
                    }
                });
            });
        }
        assert_eq!(vals, [0.6f32, 0.07, -0.06, 0.333_333_34, 1.0], "bit-identical");
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    #[test]
    fn menus_open_below_and_flip_at_the_window_edges() {
        let screen = rect(4.0, 4.0, 1592.0, 992.0); // a 1600x1000 window less the margin
        let size = egui::vec2(200.0, 300.0);
        // Room below and to the right: below, start-aligned, `gap` under it.
        let p = menu_pos(rect(100.0, 100.0, 26.0, 26.0), size, screen, 4.0);
        assert_eq!(p, egui::pos2(100.0, 130.0));
        // A button in the bottom-right corner (the layers panel's icons):
        // above the button and end-aligned with it.
        let p = menu_pos(rect(1500.0, 900.0, 26.0, 26.0), size, screen, 4.0);
        assert_eq!(p, egui::pos2(1326.0, 596.0));
        // A right-click near the corner: the menu's corner sits on the
        // pointer instead of sliding underneath it.
        let p = menu_pos(
            rect(1560.0, 780.0, 0.0, 0.0),
            egui::vec2(190.0, 280.0),
            screen,
            0.0,
        );
        assert_eq!(p, egui::pos2(1370.0, 500.0));
    }

    #[test]
    fn a_menu_too_big_for_either_side_stays_inside_the_window() {
        let screen = rect(4.0, 4.0, 1592.0, 992.0);
        let p = menu_pos(
            rect(100.0, 500.0, 26.0, 26.0),
            egui::vec2(200.0, 900.0),
            screen,
            4.0,
        );
        assert_eq!(p, egui::pos2(100.0, 4.0));
        // Wider than the window: pinned to the left edge.
        let p = menu_pos(
            rect(800.0, 100.0, 26.0, 26.0),
            egui::vec2(1700.0, 100.0),
            screen,
            4.0,
        );
        assert_eq!(p, egui::pos2(4.0, 130.0));
    }
}
