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
                let mut source = None;
                let mut snap_from: Option<usize> = None;
                let mut restore: Option<usize> = None;
                let mut drop_snap: Option<usize> = None;
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
                    let scroll_out = egui::ScrollArea::horizontal().id_salt("history").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            // Snapshots first, as in Photoshop's History panel.
                            for (k, (key, name, _)) in self.snapshots.iter().enumerate() {
                                if *key != self.doc_key {
                                    continue;
                                }
                                let resp = snapshot_card(ui, name);
                                if resp.clicked() {
                                    restore = Some(k);
                                }
                                context_menu(&resp, |ui| {
                                    if ui.button("Delete snapshot").clicked() {
                                        drop_snap = Some(k);
                                        ui.close_menu();
                                    }
                                });
                            }
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
                                context_menu(&resp, |ui| {
                                    if ui.button("New snapshot").clicked() {
                                        snap_from = Some(i);
                                        ui.close_menu();
                                    }
                                    if ui.button("Use as history brush source").clicked() {
                                        source = Some(i);
                                        ui.close_menu();
                                    }
                                });
                            }
                            ui.data_mut(|d| d.insert_temp(seen_id, current));
                        });
                    });
                    a11y_scroll(ui.ctx(), &scroll_out, "History");
                });
                if toggle {
                    self.toggle_history_strip();
                }
                if let Some(i) = source {
                    self.set_history_source(i);
                }
                if let Some(i) = snap_from {
                    self.new_snapshot(Some(i));
                }
                if let Some(k) = drop_snap {
                    let (_, name, _) = self.snapshots.remove(k);
                    self.status = format!("Deleted the snapshot \"{name}\"");
                }
                if let Some(k) = restore {
                    let (_, name, doc) = self.snapshots[k].clone();
                    self.run(&lumenply_core::everyday::RestoreSnapshot {
                        doc,
                        name: name.clone(),
                    });
                    self.fix_active();
                    self.status = format!("Restored the snapshot \"{name}\"");
                }
                if let Some(n) = jump {
                    self.editor.jump_to(n);
                    let r = self.editor.last_affected();
                    self.mark(r);
                    self.fix_active();
                    self.status = format!("Jumped to history step {n}");
                }
            });
    }

    /// Keep the document as it is at history step `step` (now when
    /// `None`) under a new name; clicking its card later brings it back.
    pub(crate) fn new_snapshot(&mut self, step: Option<usize>) {
        let doc = match step {
            Some(i) => self.editor.state(i).cloned(),
            None => Some(self.editor.doc().clone()),
        };
        let Some(doc) = doc else { return };
        let n = self.snapshots.iter().filter(|(k, ..)| *k == self.doc_key).count() + 1;
        let name = format!("Snapshot {n}");
        self.status = format!("Kept \"{name}\" (click its card in History to go back to it)");
        self.snapshots.push((self.doc_key, name, doc));
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
        // The history brush's source: a small brush tip in the corner.
        let source = i == self.history_source_step();
        if source {
            p.circle_filled(well.left_top() + egui::vec2(7.0, 7.0), 4.5, ACCENT);
            p.circle_filled(well.left_top() + egui::vec2(7.0, 7.0), 1.8, ACCENT_INK);
        }
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
        // Named with its number, so a step called "Select" or "Open" is
        // never mistaken for the menu of that name.
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, card_name(i, label)));
        let src = if source {
            "\nThe history brush paints from here"
        } else {
            ""
        };
        resp.on_hover_text(format!("{label}\n{what} {i} · click to jump here{src}"))
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

/// The accessible name of history card `i` (0 is the opened state):
/// "History step 3: Select".
pub(crate) fn card_name(i: usize, label: &str) -> String {
    format!("History step {i}: {label}")
}

/// The accessible name of a snapshot's card: "History snapshot: Snapshot 1".
pub(crate) fn snapshot_name(name: &str) -> String {
    format!("History snapshot: {name}")
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

/// A snapshot's card: its name in a well outlined in the accent colour.
fn snapshot_card(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let sense = Sense {
        click: true,
        drag: false,
        focusable: false,
    };
    let (rect, resp) = ui.allocate_exact_size(CARD, sense);
    let p = ui.painter();
    let well = egui::Rect::from_min_size(rect.min, egui::vec2(CARD.x, WELL_H));
    p.rect_filled(well, 4.0, ACCENT_TINT);
    p.rect_stroke(
        well,
        4.0,
        Stroke::new(if resp.hovered() { 2.0 } else { 1.0 }, ACCENT),
    );
    // A small camera: body and lens.
    let c = well.center();
    p.rect_stroke(
        egui::Rect::from_center_size(c, egui::vec2(18.0, 12.0)),
        2.0,
        Stroke::new(1.5, ACCENT),
    );
    p.circle_stroke(c, 3.5, Stroke::new(1.5, ACCENT));
    let (galley, _) = elided(ui, name, FontId::proportional(10.5), TEXT, CARD.x);
    p.galley(egui::pos2(rect.min.x, rect.min.y + WELL_H + 2.0), galley, TEXT);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, snapshot_name(name)));
    resp.on_hover_text(format!("{name}: click to go back to it (one undo step)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y_tests::{ctx, launch};

    /// The buttons on screen after a few frames, by accessible name.
    fn button_names(app: &mut App, ctx: &egui::Context) -> Vec<String> {
        let mut names = Vec::new();
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(raw, |ctx| app.frame(ctx));
            let update = out.platform_output.accesskit_update.expect("accesskit is on");
            names = update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == egui::accesskit::Role::Button)
                .filter_map(|(_, n)| n.name().map(str::to_string))
                .collect();
        }
        names
    }

    #[test]
    fn history_cards_are_named_by_number_and_never_like_a_menu() {
        let mut app = launch(&["--demo".to_string()]);
        let ctx = ctx();
        app.run_menu_action("select-all");
        app.new_snapshot(None);
        let names = button_names(&mut app, &ctx);
        let count = |n: &str| names.iter().filter(|x| x.as_str() == n).count();
        assert_eq!(app.editor.history(), vec!["Select"]);
        assert_eq!(count("History step 0: Open"), 1);
        assert_eq!(count("History step 1: Select"), 1);
        assert_eq!(count("History snapshot: Snapshot 1"), 1);
        // "Select" is the menu alone; no card is called "Open".
        assert_eq!(count("Select"), 1);
        assert_eq!(count("Open"), 0);
    }
}
