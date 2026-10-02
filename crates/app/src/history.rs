use super::*;

/// Card size in the strip: a thumbnail over a one-line label.
const CARD: egui::Vec2 = egui::vec2(58.0, 48.0);
/// Height of a card's thumbnail well.
const WELL_H: f32 = 34.0;
/// Pixel height of the captured thumbnails (drawn scaled into the well).
const THUMB_H: usize = 40;
/// Strip height when folded to its header line, and when showing cards.
const COLLAPSED_H: f32 = 28.0;
const EXPANDED_H: f32 = 62.0;
/// Width of the header (title, step count) at the strip's left.
const HEADER_W: f32 = 92.0;

impl App {
    /// Fold the strip down to one line, or open it again; the choice is
    /// remembered in the preferences.
    pub(crate) fn toggle_history_strip(&mut self) {
        self.prefs.history_collapsed = !self.prefs.history_collapsed;
        self.prefs.save();
    }

    /// The history strip along the bottom: one thumbnail card per step,
    /// the current step highlighted, redo steps dimmed. Clicking jumps.
    /// The header folds the strip to a single line.
    pub(crate) fn history_strip(&mut self, ctx: &egui::Context) {
        let collapsed = self.prefs.history_collapsed;
        egui::TopBottomPanel::bottom("history-strip")
            .exact_height(if collapsed { COLLAPSED_H } else { EXPANDED_H })
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(10.0, if collapsed { 0.0 } else { 6.0 })),
            )
            .show(ctx, |ui| {
                let current = self.editor.history().len();
                let labels: Vec<String> = std::iter::once("Open".to_string())
                    .chain(self.editor.history().iter().map(|s| s.to_string()))
                    .chain(self.editor.redo_history().iter().map(|s| s.to_string()))
                    .collect();
                let mut jump = None;
                let mut toggle = false;
                // Folded: one vertically centred line. Open: cards from the top.
                let align = if collapsed {
                    egui::Align::Center
                } else {
                    egui::Align::Min
                };
                ui.with_layout(egui::Layout::left_to_right(align), |ui| {
                    toggle = history_header(ui, collapsed, current, labels.len());
                    if collapsed {
                        // One line: where we are, in words.
                        let now = labels.get(current).map_or("", String::as_str);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("Step {} of {} · {now}", current, labels.len() - 1))
                                    .color(MUTED),
                            )
                            .truncate(),
                        );
                        return;
                    }
                    ui.style_mut().always_scroll_the_only_direction = true;
                    egui::ScrollArea::horizontal().id_salt("history").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            // Keep the current step in view as history grows
                            // or jumps.
                            let seen_id = ui.id().with("seen-current");
                            let seen = ui.data(|d| d.get_temp::<usize>(seen_id));
                            for (i, label) in labels.iter().enumerate() {
                                let resp = self.history_card(ui, i, label, current);
                                if i == current && seen != Some(current) {
                                    resp.scroll_to_me(Some(egui::Align::Max));
                                }
                                if resp.clicked() {
                                    jump = Some(i);
                                }
                            }
                            ui.data_mut(|d| d.insert_temp(seen_id, current));
                        });
                    });
                });
                if toggle {
                    self.toggle_history_strip();
                }
                if let Some(n) = jump {
                    self.editor.jump_to(n);
                    self.below.note_change(self.editor.doc(), None);
                    let r = self.editor.last_affected();
                    self.mark(r);
                    self.fix_active();
                    self.status = format!("Jumped to history step {n}");
                }
            });
    }

    fn history_card(&self, ui: &mut egui::Ui, i: usize, label: &str, current: usize) -> egui::Response {
        // Clickable but kept out of the Tab order: stepping through dozens
        // of cards would stand between the options bar and the tools, and
        // Undo/Redo already walk history from the keyboard.
        let sense = Sense {
            click: true,
            drag: false,
            focusable: false,
        };
        let (rect, resp) = ui.allocate_exact_size(CARD, sense);
        let p = ui.painter();
        let well = egui::Rect::from_min_size(rect.min, egui::vec2(CARD.x, WELL_H));
        p.rect_filled(well, 4.0, RAISED);
        if let Some(tex) = self.hist_thumbs.get(i) {
            let size = tex.size_vec2();
            let scale = ((well.width() - 4.0) / size.x).min((well.height() - 4.0) / size.y);
            let draw = egui::Rect::from_center_size(well.center(), size * scale);
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
        p.rect_stroke(well, 4.0, stroke);
        let col = if i == current {
            TEXT
        } else if i > current {
            Color32::from_rgb(0x6B, 0x72, 0x7C)
        } else {
            MUTED
        };
        let (galley, _) = elided(ui, label, FontId::proportional(10.5), col, CARD.x);
        let pos = egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.max.y - galley.size().y,
        );
        ui.painter().galley(pos, galley, col);
        let what = if i > current {
            "redo step"
        } else if i == current {
            "current step"
        } else {
            "step"
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        resp.on_hover_text(format!("{label}\n{what} {i} · click to jump here"))
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

/// The strip's title block: a disclosure chevron, HISTORY and the step
/// count. Returns true when clicked (fold or unfold).
fn history_header(ui: &mut egui::Ui, collapsed: bool, current: usize, steps: usize) -> bool {
    let h = if collapsed { COLLAPSED_H - 4.0 } else { CARD.y };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(HEADER_W, h), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, RADIUS, HOVER);
    }
    focus_ring(ui, &resp, rect, RADIUS);
    let ink = if resp.hovered() { TEXT } else { MUTED };
    let title_y = if collapsed {
        rect.center().y
    } else {
        rect.min.y + 13.0
    };
    // Chevron: right when folded, down when open.
    let c = egui::pos2(rect.min.x + 9.0, title_y);
    let pts = if collapsed {
        vec![
            c + egui::vec2(-2.0, -4.0),
            c + egui::vec2(2.5, 0.0),
            c + egui::vec2(-2.0, 4.0),
        ]
    } else {
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(0.0, 2.5),
            c + egui::vec2(4.0, -2.0),
        ]
    };
    p.add(Shape::line(pts, Stroke::new(1.5, ink)));
    p.text(
        egui::pos2(rect.min.x + 19.0, title_y),
        Align2::LEFT_CENTER,
        "HISTORY",
        FontId::new(10.5, egui::FontFamily::Name("semibold".into())),
        ink,
    );
    if !collapsed {
        let n = steps.saturating_sub(1);
        p.text(
            egui::pos2(rect.min.x + 19.0, title_y + 16.0),
            Align2::LEFT_CENTER,
            format!("{n} step{}", if n == 1 { "" } else { "s" }),
            FontId::proportional(10.5),
            MUTED,
        );
    }
    let tip = if collapsed {
        "Show the history strip"
    } else {
        "Fold the history strip to one line"
    };
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("History, step {current} of {}", steps.saturating_sub(1)),
        )
    });
    resp.on_hover_text(tip).clicked()
}
