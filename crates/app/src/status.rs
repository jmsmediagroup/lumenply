use super::*;

impl App {
    pub(crate) fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let doc = self.editor.doc();
                ui.label(RichText::new(format!("{:.0}%", self.zoom * 100.0)).monospace());
                ui.separator();
                ui.label(RichText::new(format!("{} × {} px", doc.width, doc.height)).monospace());
                ui.separator();
                match self.cursor_doc {
                    Some((x, y)) => ui.label(RichText::new(format!("x {x}  y {y}")).monospace()),
                    None => ui.label(RichText::new("x –  y –").monospace()),
                };
                ui.separator();
                match &doc.selection {
                    Some(s) => {
                        let b = s.bounds_within(doc.canvas());
                        ui.label(format!("Selection ≈ {} × {} at {}, {}", b.w, b.h, b.x, b.y));
                    }
                    None => {
                        ui.label(RichText::new("No selection").weak());
                    }
                }
                ui.separator();
                ui.label(format!("{} layers", doc.layer_count()));
                if self.editing_mask {
                    ui.separator();
                    ui.label(RichText::new("Editing mask").color(Color32::from_rgb(230, 200, 90)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&self.status).weak());
                });
            });
        });
    }
}
