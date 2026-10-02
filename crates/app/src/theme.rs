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
/// Live (non-destructive) filter layers.
pub(crate) const LIVE_FILTER: Color32 = Color32::from_rgb(0x9F, 0xC9, 0xFF);

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
    v.widgets.inactive.bg_fill = RAISED;
    // Buttons sit flat on their panel until hovered; raised chrome is opted
    // into with an explicit fill (chips, Export, tool rail).
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x2B, 0x30, 0x36);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2B, 0x30, 0x36);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.active.bg_fill = Color32::from_rgb(0x32, 0x38, 0x3F);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x32, 0x38, 0x3F);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.open.bg_fill = RAISED;
    v.widgets.open.weak_bg_fill = RAISED;
    v.widgets.open.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);

    // Soft depth: rounded corners everywhere, gentle shadows on anything
    // that floats.
    let r = egui::Rounding::same(6.0);
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
    ctx.set_style(style);
}

/// A bar across the top or bottom: panel fill, fixed height, side padding,
/// vertically centred content.
pub(crate) fn bar_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .inner_margin(egui::Margin::symmetric(12.0, 0.0))
}

pub(crate) fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).small().strong().color(MUTED));
}

/// A labelled slider row: muted label on the left, slider filling the
/// middle, mono value on the right. Returns true when the user finished an
/// edit (drag released or value typed).
pub(crate) fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    let mut finished = false;
    let decimals = if range.end() - range.start() >= 10.0 { 0 } else { 2 };
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 18.0], egui::Label::new(RichText::new(label).color(MUTED)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized(
                [58.0, 18.0],
                egui::Label::new(
                    RichText::new(format!("{:.decimals$}{suffix}", v))
                        .monospace()
                        .color(TEXT),
                ),
            );
            ui.spacing_mut().slider_width = (ui.available_width() - 10.0).max(60.0);
            let r = ui.add(egui::Slider::new(v, range).show_value(false));
            finished = r.drag_stopped() || (r.changed() && !r.dragged());
        });
    });
    finished
}
