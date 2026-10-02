use super::*;

impl App {
    /// The status bar: document facts on the left (numbers in mono), the
    /// last message on the right, elided rather than run under the facts.
    pub(crate) fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(28.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let doc = self.editor.doc();
                    let mono = |s: String| RichText::new(s).monospace().color(TEXT);
                    ui.label(mono(format!("{} × {} px", doc.width, doc.height)))
                        .on_hover_text("Canvas size");
                    ui.separator();
                    let at = match self.cursor_doc {
                        Some((x, y)) => format!("x {x:<5} y {y:<5}"),
                        None => "x –     y –    ".to_string(),
                    };
                    ui.label(mono(at)).on_hover_text("Pointer position on the canvas");
                    ui.separator();
                    match &doc.selection {
                        Some(s) => {
                            let b = s.bounds_within(doc.canvas());
                            ui.label(RichText::new("Selection").color(MUTED));
                            ui.label(mono(format!("{} × {}", b.w, b.h)))
                                .on_hover_text(format!("Selection bounds at {}, {}", b.x, b.y));
                        }
                        None => {
                            ui.label(RichText::new("No selection").color(MUTED));
                        }
                    }
                    ui.separator();
                    let n = doc.layer_count();
                    ui.label(
                        RichText::new(format!("{n} layer{}", if n == 1 { "" } else { "s" })).color(MUTED),
                    );
                    if self.editing_mask {
                        ui.separator();
                        ui.label(RichText::new("Editing mask").color(ACCENT))
                            .on_hover_text("Painting edits the layer mask: white reveals, black hides");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&self.status).color(MUTED)).truncate());
                    });
                });
            });
    }
}
