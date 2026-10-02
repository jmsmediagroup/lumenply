use super::*;

impl App {
    pub(crate) fn history_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "HISTORY");
        let items: Vec<String> = self.editor.history().iter().map(|s| s.to_string()).collect();
        let redo: Vec<String> = self.editor.redo_history().iter().map(|s| s.to_string()).collect();
        let current = items.len();
        let mut jump = None;
        let row = |ui: &mut egui::Ui, idx: usize, text: &str, state: u8| -> bool {
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 20.0), Sense::click());
            if state == 1 {
                ui.painter()
                    .rect_filled(rect, 3.0, Color32::from_rgb(40, 62, 100));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, Color32::from_gray(50));
            }
            let col = match state {
                1 => Color32::from_rgb(170, 205, 255),
                2 => Color32::from_gray(95),
                _ => Color32::from_gray(160),
            };
            ui.painter().text(
                rect.left_center() + egui::vec2(6.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{idx:>2}  {text}"),
                FontId::proportional(13.0),
                col,
            );
            resp.clicked()
        };
        egui::ScrollArea::vertical()
            .id_salt("history")
            .max_height(260.0)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if row(ui, 0, "Original", u8::from(current == 0)) {
                    jump = Some(0);
                }
                for (i, label) in items.iter().enumerate() {
                    if row(ui, i + 1, label, u8::from(i + 1 == current)) {
                        jump = Some(i + 1);
                    }
                }
                for (k, label) in redo.iter().enumerate() {
                    if row(ui, current + k + 1, label, 2) {
                        jump = Some(current + k + 1);
                    }
                }
            });
        if let Some(n) = jump {
            self.editor.jump_to(n);
            self.mark(None);
            self.fix_active();
            self.status = format!("Jumped to history step {n}");
        }
    }
}
