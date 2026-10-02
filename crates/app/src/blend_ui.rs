//! The blend-mode dropdown shared by the Properties panel and the tool
//! bars: all 27 modes in Photoshop's order and groups (normal, darken,
//! lighten, contrast, inversion, component) with a separator between
//! groups and Photoshop's display names.

use eframe::egui;
use lumenply_doc::BlendMode;

use crate::theme::{a11y_name, a11y_scroll, note_target, popup_style};

/// Popup height for a blend combo about to be laid out at `ui`'s cursor:
/// the larger of the room above and below it, so the long list shows
/// whole when it fits and scrolls when the window is short. egui opens
/// the popup on whichever side has room for it.
fn popup_height(ui: &egui::Ui) -> f32 {
    let screen = ui.ctx().screen_rect();
    let top = ui.next_widget_position().y;
    let below = screen.bottom() - (top + ui.spacing().interact_size.y);
    let above = top - screen.top();
    (below.max(above) - 28.0).max(120.0)
}

/// A blend-mode combo box. `sel` is the current mode; `None` stands for
/// Pass Through, offered first when `pass_through` is true (groups).
/// Returns the combo's response (already named `a11y` for screen readers;
/// `--screenshot-do popups:click=<id_salt>` opens it).
pub(crate) fn blend_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    a11y: &str,
    width: Option<f32>,
    sel: &mut Option<BlendMode>,
    pass_through: bool,
) -> egui::Response {
    let height = popup_height(ui);
    // The combo's own scroll area gets a little more room than the list
    // inside it, so it never scrolls (it can't be given a spoken name);
    // the inner, named one does.
    let mut combo = egui::ComboBox::from_id_salt(id_salt)
        .selected_text(sel.map_or("Pass Through", |m| m.label()))
        .height(height + 8.0);
    if let Some(w) = width {
        combo = combo.width(w);
    }
    // egui 0.29 sizes a popup's area from `default_area_size` (400 pt
    // tall) on its first frame and never grows it, which would cut the
    // list to ~15 rows; lift that cap while the combo lays out.
    let ctx = ui.ctx().clone();
    let area = ctx.style().spacing.default_area_size;
    ctx.style_mut(|s| s.spacing.default_area_size.y = area.y.max(height + 24.0));
    let list = format!("{a11y} list");
    let r = combo
        .show_ui(ui, |ui| {
            popup_style(ui);
            let out = egui::ScrollArea::vertical()
                .id_salt(("blend-list", id_salt))
                .max_height(height)
                .show(ui, |ui| {
                    if pass_through {
                        ui.selectable_value(sel, None, "Pass Through");
                        ui.separator();
                    }
                    for (i, group) in BlendMode::GROUPS.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        for &m in group.iter() {
                            ui.selectable_value(sel, Some(m), m.label());
                        }
                    }
                });
            a11y_scroll(ui.ctx(), &out, &list);
        })
        .response;
    ctx.style_mut(|s| s.spacing.default_area_size = area);
    // The popup's backdrop (egui gives it a focusable click surface).
    let backdrop = r.id.with("popup").with("move");
    if ctx.read_response(backdrop).is_some() {
        ctx.accesskit_node_builder(backdrop, |b| {
            b.set_role(egui::accesskit::Role::ListBox);
            b.set_name(list.as_str());
        });
    }
    a11y_name(&r, a11y);
    note_target(ui.ctx(), id_salt, r.rect);
    #[cfg(test)]
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            egui::Id::new("blend_ui:popup").with(id_salt),
            r.id.with("popup"), // egui::ComboBox::widget_to_popup_id (private)
        )
    });
    r
}

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui::accesskit::Action;
    use lumenply_doc::BlendMode;

    use crate::a11y_tests::{ctx, launch, nameless};

    /// The open Properties list offers every mode by its Photoshop name,
    /// as a named control a screen reader can reach.
    #[test]
    fn the_open_blend_list_names_every_mode() {
        let mut app = launch(&[]);
        app.open_in_new_tab(crate::blank(64, 64), None);
        let bg = app.editor.doc().layers().last().unwrap().id;
        app.set_active(Some(bg));
        let ctx = ctx();
        assert_eq!(nameless(&mut app, &ctx), Vec::<String>::new());
        let popup = ctx
            .data(|d| d.get_temp::<egui::Id>(egui::Id::new("blend_ui:popup").with("blend-mode")))
            .expect("the Properties panel lays out the blend combo");
        ctx.memory_mut(|m| m.open_popup(popup));
        assert_eq!(nameless(&mut app, &ctx), Vec::<String>::new());
        assert!(ctx.memory(|m| m.is_popup_open(popup)), "the list stayed open");
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| app.frame(ctx));
        let names: Vec<String> = out
            .platform_output
            .accesskit_update
            .expect("accesskit is enabled")
            .nodes
            .iter()
            .filter(|(_, n)| n.supports_action(Action::Focus))
            .filter_map(|(_, n)| n.name().map(str::to_string))
            .collect();
        for m in BlendMode::ALL {
            assert!(names.iter().any(|n| n == m.label()), "{} is offered", m.label());
        }
    }

    #[test]
    fn menu_groups_follow_photoshop() {
        let labels: Vec<Vec<&str>> = BlendMode::GROUPS
            .iter()
            .map(|g| g.iter().map(|m| m.label()).collect())
            .collect();
        assert_eq!(
            labels,
            vec![
                vec!["Normal", "Dissolve"],
                vec!["Darken", "Multiply", "Color Burn", "Linear Burn", "Darker Color"],
                vec![
                    "Lighten",
                    "Screen",
                    "Color Dodge",
                    "Linear Dodge (Add)",
                    "Lighter Color"
                ],
                vec![
                    "Overlay",
                    "Soft Light",
                    "Hard Light",
                    "Vivid Light",
                    "Linear Light",
                    "Pin Light",
                    "Hard Mix"
                ],
                vec!["Difference", "Exclusion", "Subtract", "Divide"],
                vec!["Hue", "Saturation", "Color", "Luminosity"],
            ]
        );
    }
}
