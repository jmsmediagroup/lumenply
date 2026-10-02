//! Window ▸ Navigator: the whole image in small with the part the canvas
//! shows outlined in red. Click or drag in it to pan; the slider and the
//! percentage field zoom about the canvas centre. Floats at the canvas's
//! top-right corner, above the Info panel when both are open.

use super::*;
use crate::channels_panel::{full_uv, thumb_with};
use crate::panels::{close_button, float_frame, REBUILD_EVERY};

/// The navigator picture's box.
pub(crate) const NAV_W: f32 = 200.0;
pub(crate) const NAV_H: f32 = 140.0;

/// The picture's size for a `w`×`h` document: as large as fits the box.
pub(crate) fn nav_size(w: u32, h: u32) -> (usize, usize) {
    let s = (NAV_W / w.max(1) as f32).min(NAV_H / h.max(1) as f32);
    (
        ((w as f32 * s).round() as usize).max(1),
        ((h as f32 * s).round() as usize).max(1),
    )
}

/// The part of the document a canvas of size `view` shows at `pan` and
/// `zoom`, in document pixels.
pub(crate) fn visible_doc_rect(view: Vec2, pan: Vec2, zoom: f32) -> egui::Rect {
    egui::Rect::from_min_size((-pan / zoom).to_pos2(), view / zoom)
}

/// The pan that puts document point `c` at the centre of a canvas of
/// size `view`.
pub(crate) fn pan_to_centre(view: Vec2, c: Pos2, zoom: f32) -> Vec2 {
    view / 2.0 - c.to_vec2() * zoom
}

/// A floating panel's title row: a small caps title and a close button.
pub(crate) fn float_header(ui: &mut egui::Ui, title: &str, close_name: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).small().strong().color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = close_button(ui, close_name);
        });
    });
    close
}

impl App {
    /// Rebuild the navigator picture when the document changed, at most a
    /// few times a second.
    fn nav_texture(&mut self, ctx: &egui::Context) {
        let have = self.panels.nav_tex.is_some();
        if have && !self.panels.nav_stale {
            return;
        }
        if have && self.panels.nav_at.is_some_and(|t| t.elapsed() < REBUILD_EVERY) {
            ctx.request_repaint_after(REBUILD_EVERY);
            return;
        }
        let Some(flat) = &self.last_flat else { return };
        let doc = self.editor.doc();
        if flat.width != doc.width || flat.height != doc.height {
            return;
        }
        let size = nav_size(doc.width, doc.height);
        let img = thumb_with(doc.canvas(), size, |x, y| {
            to_color32(flat.get(
                x.clamp(0, flat.width as i32 - 1) as u32,
                y.clamp(0, flat.height as i32 - 1) as u32,
            ))
        });
        upload(
            &mut self.panels.nav_tex,
            ctx,
            "navigator",
            img,
            egui::TextureOptions::LINEAR,
        );
        self.panels.nav_stale = false;
        self.panels.nav_at = Some(std::time::Instant::now());
    }

    /// The Navigator and Info panels, stacked at the canvas's top-right.
    pub(crate) fn floating_panels(&mut self, ctx: &egui::Context) {
        let Some(canvas) = self.panels.canvas_rect else {
            return;
        };
        if self.no_doc {
            return;
        }
        let ruler = if self.prefs.show_rulers {
            guides::RULER
        } else {
            0.0
        };
        let mut at = egui::pos2(canvas.max.x - 12.0, canvas.min.y + 12.0 + ruler);
        // Info goes under the Navigator, or beside it when a short canvas
        // would push it onto the zoom pill.
        let info_h_id = egui::Id::new("info-panel-h");
        let info_h = ctx.data(|d| d.get_temp::<f32>(info_h_id)).unwrap_or(250.0);
        if self.prefs.panels.navigator {
            let h = self.navigator_ui(ctx, at, canvas);
            if at.y + h + 10.0 + info_h > canvas.max.y - 56.0 {
                at.x -= NAV_W + 30.0;
            } else {
                at.y += h + 10.0;
            }
        }
        if self.prefs.panels.info {
            let h = self.info_ui(ctx, at);
            ctx.data_mut(|d| d.insert_temp(info_h_id, h));
        }
    }

    /// Returns the panel's height.
    fn navigator_ui(&mut self, ctx: &egui::Context, anchor: Pos2, canvas: egui::Rect) -> f32 {
        self.nav_texture(ctx);
        let (dw, dh) = (self.editor.doc().width, self.editor.doc().height);
        let mut close = false;
        let mut pan_to: Option<Pos2> = None;
        let mut zoom_to: Option<f32> = None;
        let out = egui::Area::new("navigator".into())
            .order(egui::Order::Middle)
            .sense(BACKDROP_SENSE)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                float_frame().show(ui, |ui| {
                    ui.set_width(NAV_W);
                    raise_controls(ui);
                    close = float_header(ui, "NAVIGATOR", "Close Navigator");
                    let (area, resp) =
                        ui.allocate_exact_size(egui::vec2(NAV_W, NAV_H), Sense::click_and_drag());
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Other,
                            true,
                            "Navigator: click or drag to pan",
                        )
                    });
                    let p = ui.painter();
                    p.rect_filled(area, 4.0, GROUND);
                    let (tw, th) = nav_size(dw, dh);
                    let img = egui::Rect::from_center_size(area.center(), egui::vec2(tw as f32, th as f32));
                    paint_thumb_bg(p, img);
                    if let Some(tex) = &self.panels.nav_tex {
                        p.image(tex.id(), img, full_uv(), Color32::WHITE);
                    }
                    let s = img.width() / dw.max(1) as f32;
                    let vis = visible_doc_rect(canvas.size(), self.pan, self.zoom);
                    let r = egui::Rect::from_min_max(
                        img.min + vis.min.to_vec2() * s,
                        img.min + vis.max.to_vec2() * s,
                    )
                    .intersect(area.shrink(1.0));
                    if r.is_positive() {
                        p.rect_stroke(r, 0.0, Stroke::new(1.5, Color32::from_rgb(235, 64, 52)));
                    }
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(if resp.dragged() {
                            egui::CursorIcon::Grabbing
                        } else {
                            egui::CursorIcon::Grab
                        });
                    }
                    if resp.clicked() || resp.dragged() {
                        if let Some(q) = resp.interact_pointer_pos() {
                            pan_to = Some(((q - img.min) / s.max(1e-6)).to_pos2());
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.spacing_mut().slider_width = 84.0;
                        let out = ui.add(
                            egui::Button::new("−")
                                .frame(false)
                                .min_size(egui::vec2(16.0, 20.0)),
                        );
                        a11y_name(&out, "Zoom out");
                        if out.on_hover_text("Zoom out").clicked() {
                            zoom_to = Some(self.zoom / 1.25);
                        }
                        let mut pct = self.zoom * 100.0;
                        let r = ui.add(
                            egui::Slider::new(&mut pct, 5.0..=3200.0)
                                .logarithmic(true)
                                .show_value(false),
                        );
                        a11y_name(&r, "Zoom");
                        if r.changed() {
                            zoom_to = Some(pct / 100.0);
                        }
                        let zin = ui.add(
                            egui::Button::new("+")
                                .frame(false)
                                .min_size(egui::vec2(16.0, 20.0)),
                        );
                        a11y_name(&zin, "Zoom in");
                        if zin.on_hover_text("Zoom in").clicked() {
                            zoom_to = Some(self.zoom * 1.25);
                        }
                        let mut typed = self.zoom * 100.0;
                        let f = num_field(
                            ui,
                            egui::DragValue::new(&mut typed)
                                .range(5.0..=3200.0)
                                .suffix("%")
                                .max_decimals(0)
                                .speed(1.0),
                            ui.available_width().max(48.0),
                        );
                        a11y_name(&f, "Zoom percentage");
                        if f.changed() {
                            zoom_to = Some(typed / 100.0);
                        }
                    });
                });
            });
        if close {
            self.prefs.panels.navigator = false;
            self.prefs.save();
        }
        if let Some(c) = pan_to {
            self.pan = pan_to_centre(canvas.size(), c, self.zoom);
        }
        if let Some(z) = zoom_to {
            let z = z.clamp(0.05, 32.0);
            self.zoom_at(canvas, canvas.center(), z / self.zoom);
        }
        out.response.rect.height()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_fits_the_box() {
        assert_eq!(nav_size(4000, 3000), (187, 140));
        assert_eq!(nav_size(100, 50), (200, 100));
        assert_eq!(nav_size(10, 1000), (1, 140));
    }

    #[test]
    fn the_view_rectangle_and_panning_agree() {
        // An 800×600 canvas at 50% zoom panned 100 px right, 40 down
        // shows document pixels (−200, −80) to (1400, 1120).
        let v = visible_doc_rect(egui::vec2(800.0, 600.0), egui::vec2(100.0, 40.0), 0.5);
        assert_eq!(
            (v.min, v.max),
            (egui::pos2(-200.0, -80.0), egui::pos2(1400.0, 1120.0))
        );
        // Centring document point (1000, 500) at 200%...
        let pan = pan_to_centre(egui::vec2(800.0, 600.0), egui::pos2(1000.0, 500.0), 2.0);
        assert_eq!(pan, egui::vec2(-1600.0, -700.0));
        // ...puts it in the middle of the visible rectangle.
        let v = visible_doc_rect(egui::vec2(800.0, 600.0), pan, 2.0);
        assert_eq!(v.center(), egui::pos2(1000.0, 500.0));
    }

    #[test]
    fn navigator_and_info_name_every_control() {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(blank(64, 32), None);
        app.prefs.panels.navigator = true;
        app.prefs.panels.info = true;
        let ctx = crate::a11y_tests::ctx();
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
        assert!(app.panels.nav_tex.is_some(), "the picture was built");
        assert_eq!(app.panels.nav_tex.as_ref().unwrap().size(), [200, 100]);
    }
}
