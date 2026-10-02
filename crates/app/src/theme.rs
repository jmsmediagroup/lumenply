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
