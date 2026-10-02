use super::*;

/// Card size in the strip.
const CARD: egui::Vec2 = egui::vec2(76.0, 64.0);
const THUMB_H: usize = 40;

impl App {
    /// The history strip along the bottom: one thumbnail card per step,
    /// the current step highlighted, redo steps dimmed. Clicking jumps.
    pub(crate) fn history_strip(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("history-strip")
            .exact_height(88.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(10.0, 7.0)),
            )
            .show(ctx, |ui| {
                let current = self.editor.history().len();
                let labels: Vec<String> = std::iter::once("Open".to_string())
                    .chain(self.editor.history().iter().map(|s| s.to_string()))
                    .chain(self.editor.redo_history().iter().map(|s| s.to_string()))
                    .collect();
                let mut jump = None;
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.add_space(14.0);
                        ui.label(RichText::new("HISTORY").small().strong().color(MUTED));
                        ui.label(RichText::new("click a step").small().color(MUTED));
                    });
                    ui.add_space(4.0);
                    egui::ScrollArea::horizontal().id_salt("history").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (i, label) in labels.iter().enumerate() {
                                if self.history_card(ui, i, label, current) {
                                    jump = Some(i);
                                }
                            }
                        });
                    });
                });
                if let Some(n) = jump {
                    self.editor.jump_to(n);
                    self.mark(None);
                    self.fix_active();
                    self.status = format!("Jumped to history step {n}");
                }
            });
    }

    fn history_card(&self, ui: &mut egui::Ui, i: usize, label: &str, current: usize) -> bool {
        let (rect, resp) = ui.allocate_exact_size(CARD, Sense::click());
        let p = ui.painter();
        let thumb_rect = egui::Rect::from_min_size(rect.min, egui::vec2(CARD.x, THUMB_H as f32 + 6.0));
        p.rect_filled(thumb_rect, 4.0, RAISED);
        if let Some(tex) = self.hist_thumbs.get(i) {
            let size = tex.size_vec2();
            let scale = ((thumb_rect.width() - 6.0) / size.x).min((thumb_rect.height() - 6.0) / size.y);
            let draw = egui::Rect::from_center_size(thumb_rect.center(), size * scale);
            let tint = if i > current {
                Color32::from_white_alpha(90)
            } else {
                Color32::WHITE
            };
            p.image(
                tex.id(),
                draw,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                tint,
            );
        }
        let stroke = if i == current {
            Stroke::new(2.0, ACCENT)
        } else if resp.hovered() {
            Stroke::new(1.0, MUTED)
        } else {
            Stroke::new(1.0, LINE)
        };
        p.rect_stroke(thumb_rect, 4.0, stroke);
        let mut text = label.to_string();
        if text.chars().count() > 11 {
            text = text.chars().take(10).collect::<String>() + "…";
        }
        let col = if i == current {
            TEXT
        } else if i > current {
            LINE
        } else {
            MUTED
        };
        p.text(
            egui::pos2(rect.center().x, rect.max.y - 2.0),
            Align2::CENTER_BOTTOM,
            text,
            FontId::proportional(10.5),
            col,
        );
        resp.on_hover_text(format!("{i}  {label}")).clicked()
    }

    /// Record a thumbnail for the current history step from the composited
    /// canvas. Called at the end of every refresh; steps already captured
    /// (undo, jumps) are left alone, and a new edit that destroys the redo
    /// branch drops the stale thumbnails with it.
    pub(crate) fn capture_history_thumb(&mut self, ctx: &egui::Context) {
        let cur = self.editor.history().len();
        let total = cur + self.editor.redo_history().len();
        self.hist_thumbs.truncate(total + 1);
        if self.hist_thumbs.len() > cur {
            return;
        }
        let Some(flat) = self.last_flat.as_ref() else {
            return;
        };
        if flat.width == 0 || flat.height == 0 {
            return;
        }
        let th = THUMB_H;
        let tw = ((flat.width as f32 / flat.height as f32 * th as f32) as usize).clamp(16, 72);
        let mut img = egui::ColorImage::new([tw, th], Color32::TRANSPARENT);
        for y in 0..th {
            for x in 0..tw {
                let sx = (x as f32 + 0.5) / tw as f32 * flat.width as f32;
                let sy = (y as f32 + 0.5) / th as f32 * flat.height as f32;
                let px = flat.get((sx as u32).min(flat.width - 1), (sy as u32).min(flat.height - 1));
                img.pixels[y * tw + x] = to_color32(px);
            }
        }
        while self.hist_thumbs.len() <= cur {
            let tex = ctx.load_texture(
                format!("hist-{}", self.hist_thumbs.len()),
                img.clone(),
                egui::TextureOptions::LINEAR,
            );
            self.hist_thumbs.push(tex);
        }
    }
}
