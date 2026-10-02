use super::*;

pub(crate) const ACCENT: Color32 = Color32::from_rgb(58, 112, 196);
pub(crate) const THUMB: (usize, usize) = (44, 30);

pub(crate) fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(text)
            .small()
            .strong()
            .color(Color32::from_gray(150)),
    );
}
/// Returns true when the user finished an edit (drag released or value typed).
pub(crate) fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    let r = ui.add(
        egui::Slider::new(v, range)
            .text(label)
            .suffix(suffix)
            .fixed_decimals(2),
    );
    r.drag_stopped() || (r.changed() && !r.dragged())
}
