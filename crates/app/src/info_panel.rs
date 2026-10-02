//! Window ▸ Info: the colour under the pointer as 8-bit RGB and HSB, the
//! pointer position, the selection's size and the document's size, in
//! Photoshop's two-column layout.

use super::*;
use crate::navigator::{float_header, NAV_W};
use crate::panels::{float_frame, REBUILD_EVERY};

/// Hue (degrees), saturation and brightness (percent) of an 8-bit RGB
/// colour, rounded as Photoshop's Info panel shows them.
pub(crate) fn rgb_to_hsb(rgb: [u8; 3]) -> (u32, u32, u32) {
    let [r, g, b] = rgb.map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= 0.0 { 0.0 } else { d / max };
    (
        (h.round() as u32) % 360,
        (s * 100.0).round() as u32,
        (max * 100.0).round() as u32,
    )
}

/// The selection's pixel-tight size for the Info panel: rescanned when
/// the document changed, at most a few times a second.
#[derive(Default)]
pub(crate) struct InfoCache {
    pub(crate) stale: bool,
    sel: Option<Rect>,
    at: Option<std::time::Instant>,
}

impl App {
    fn info_selection(&mut self, ctx: &egui::Context) -> Option<Rect> {
        let c = &mut self.panels.info_cache;
        if c.at.is_none() || c.stale {
            if c.at.is_some_and(|t| t.elapsed() < REBUILD_EVERY) {
                ctx.request_repaint_after(REBUILD_EVERY);
                return c.sel;
            }
            let doc = self.editor.doc();
            c.sel = doc
                .selection
                .as_ref()
                .map(|s| s.tight_bounds(doc.canvas()))
                .filter(|r| !r.is_empty());
            c.at = Some(std::time::Instant::now());
            c.stale = false;
        }
        c.sel
    }

    /// Returns the panel's height.
    pub(crate) fn info_ui(&mut self, ctx: &egui::Context, anchor: Pos2) -> f32 {
        let rgb = self
            .cursor_doc
            .zip(self.last_flat.as_ref())
            .and_then(|((x, y), f)| crate::color_picker::sample_srgb(f, x as f32 + 0.5, y as f32 + 0.5))
            .map(|c| c.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8));
        let hsb = rgb.map(rgb_to_hsb);
        let sel = self.info_selection(ctx);
        let (dw, dh) = (self.editor.doc().width, self.editor.doc().height);
        let at = self.cursor_doc;
        let mut close = false;
        let out = egui::Area::new("info-panel".into())
            .order(egui::Order::Middle)
            .sense(BACKDROP_SENSE)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                float_frame().show(ui, |ui| {
                    ui.set_width(NAV_W);
                    close = float_header(ui, "INFO", "Close Info");
                    let key = |s: &str| RichText::new(s).monospace().color(MUTED);
                    let val = |s: String| RichText::new(s).monospace().color(TEXT);
                    let dash = || "–".to_string();
                    let two = |ui: &mut egui::Ui, id: &str, rows: [(&str, String, &str, String); 3]| {
                        egui::Grid::new(id)
                            .num_columns(4)
                            .spacing([10.0, 2.0])
                            .min_col_width(14.0)
                            .show(ui, |ui| {
                                for (k1, v1, k2, v2) in rows {
                                    if k1.is_empty() && k2.is_empty() {
                                        continue;
                                    }
                                    ui.label(key(k1));
                                    ui.add_sized([60.0, 16.0], egui::Label::new(val(v1)));
                                    ui.label(key(k2));
                                    ui.add_sized([60.0, 16.0], egui::Label::new(val(v2)));
                                    ui.end_row();
                                }
                            });
                    };
                    if let Some([r, g, b]) = rgb {
                        let (sw, _) = ui.allocate_exact_size(egui::vec2(NAV_W, 6.0), Sense::hover());
                        ui.painter().rect_filled(sw, 2.0, Color32::from_rgb(r, g, b));
                    }
                    let c = |i: usize| rgb.map_or_else(dash, |v| v[i].to_string());
                    let (h, s, br) = hsb.map_or((dash(), dash(), dash()), |(h, s, b)| {
                        (format!("{h}°"), format!("{s}%"), format!("{b}%"))
                    });
                    two(
                        ui,
                        "info-colour",
                        [("R", c(0), "H", h), ("G", c(1), "S", s), ("B", c(2), "B", br)],
                    );
                    ui.separator();
                    let (x, y) = at.map_or((dash(), dash()), |(x, y)| (x.to_string(), y.to_string()));
                    let (w, hh) = sel.map_or((dash(), dash()), |r| (r.w.to_string(), r.h.to_string()));
                    two(
                        ui,
                        "info-where",
                        [
                            ("X", x, "W", w),
                            ("Y", y, "H", hh),
                            ("", String::new(), "", String::new()),
                        ],
                    );
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Document").color(MUTED));
                        ui.label(val(format!("{dw} × {dh} px")));
                    });
                });
            });
        if close {
            self.prefs.panels.info = false;
            self.prefs.save();
        }
        out.response.rect.height()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsb_matches_photoshop_rounding() {
        assert_eq!(rgb_to_hsb([255, 0, 0]), (0, 100, 100));
        assert_eq!(rgb_to_hsb([0, 128, 255]), (210, 100, 100));
        assert_eq!(rgb_to_hsb([128, 128, 128]), (0, 0, 50));
        assert_eq!(rgb_to_hsb([0, 0, 0]), (0, 0, 0));
        assert_eq!(rgb_to_hsb([255, 0, 128]), (330, 100, 100));
        assert_eq!(rgb_to_hsb([60, 120, 90]), (150, 50, 47));
    }
}
