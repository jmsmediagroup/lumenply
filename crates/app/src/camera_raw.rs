//! Opening a camera RAW file goes through a Camera Raw–style develop
//! workspace first, as in Photoshop: white balance, tone and presence
//! sliders over a live preview, then Open develops at full resolution into
//! a new document.

use super::*;
use lumenply_render::develop::{auto_develop, develop, Develop};
use rayon::prelude::*;

/// Longest preview side in pixels.
pub(crate) const PREVIEW_MAX: u32 = 1400;

pub(crate) struct CameraRawState {
    path: String,
    full: Raster,
    pub(crate) proxy: Raster,
    pub(crate) dev: Develop,
    /// Show the image as decoded, without the sliders (P).
    pub(crate) before: bool,
    tex: Option<egui::TextureHandle>,
    hist: [u32; histogram::BINS],
    pub(crate) shown: Option<(Develop, bool)>,
    /// Set when the workspace runs as Filter ▸ Camera Raw Filter on a layer
    /// (camera_raw_filter.rs) instead of opening a RAW file.
    pub(crate) filter: Option<crate::camera_raw_filter::CrfTarget>,
}

impl CameraRawState {
    /// The workspace as the Camera Raw Filter: `proxy` is the layer's
    /// pixels at preview size, `name` the layer's name.
    pub(crate) fn for_filter(
        name: &str,
        proxy: Raster,
        dev: Develop,
        target: crate::camera_raw_filter::CrfTarget,
    ) -> Self {
        CameraRawState {
            path: name.to_string(),
            full: Raster::new(0, 0),
            proxy,
            dev,
            before: false,
            tex: None,
            hist: [0; histogram::BINS],
            shown: None,
            filter: Some(target),
        }
    }

    /// Pixel size of what OK develops: the RAW file, or the canvas.
    fn size(&self) -> (u32, u32) {
        match &self.filter {
            Some(t) => (t.frame[2].max(0) as u32, t.frame[3].max(0) as u32),
            None => (self.full.width, self.full.height),
        }
    }
}

/// Box-filtered copy of `src` whose longest side is at most `max`.
pub(crate) fn shrink(src: &Raster, max: u32) -> Raster {
    let f = src.width.max(src.height).div_ceil(max).max(1);
    if f == 1 {
        return src.clone();
    }
    let (w, h) = (src.width.div_ceil(f), src.height.div_ceil(f));
    let mut out = Raster::new(w, h);
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            for (x, p) in row.iter_mut().enumerate() {
                let x = x as u32;
                let mut acc = [0.0f32; 4];
                let mut n = 0.0;
                for sy in y * f..((y + 1) * f).min(src.height) {
                    for sx in x * f..((x + 1) * f).min(src.width) {
                        let q = src.get(sx, sy);
                        acc[0] += q.r;
                        acc[1] += q.g;
                        acc[2] += q.b;
                        acc[3] += q.a;
                        n += 1.0;
                    }
                }
                *p = lumenply_tiles::Rgba::new(acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n);
            }
        });
    out
}

fn to_image(r: &Raster) -> egui::ColorImage {
    let px: Vec<Color32> = r
        .pixels
        .par_iter()
        .map(|p| {
            let [cr, cg, cb, _] = p.to_straight();
            Color32::from_rgb(
                lumenply_io::linear_to_srgb(cr),
                lumenply_io::linear_to_srgb(cg),
                lumenply_io::linear_to_srgb(cb),
            )
        })
        .collect();
    egui::ColorImage {
        size: [r.width as usize, r.height as usize],
        pixels: px,
    }
}

/// A slider row for one Basic control: label, track, typeable value.
fn basic_row(ui: &mut egui::Ui, label: &str, v: &mut f32, range: RangeInclusive<f32>, suffix: &str) {
    let opts = RowOpts {
        label_w: 92.0,
        ..RowOpts::default()
    };
    slider_row_ex(ui, label, v, range, suffix, opts);
}

impl App {
    /// Decodes `path` and opens the develop workspace on it.
    pub(crate) fn open_camera_raw(&mut self, path: &str) {
        match lumenply_io::raw::load_raw(path) {
            Ok(full) => {
                let proxy = shrink(&full, PREVIEW_MAX);
                self.camera_raw = Some(Box::new(CameraRawState {
                    path: path.to_string(),
                    full,
                    proxy,
                    dev: Develop::default(),
                    before: false,
                    tex: None,
                    hist: [0; histogram::BINS],
                    shown: None,
                    filter: None,
                }));
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    /// Develops at full resolution and opens the result as a document.
    fn finish_camera_raw(&mut self, st: CameraRawState) {
        let developed = develop(&st.full, &st.dev);
        let (w, h) = (developed.width, developed.height);
        let mut ed = Editor::new(Document::new(w, h));
        let _ = ed.execute(&AddPixelLayer::from_raster("Background", developed, 0, 0));
        self.open_in_new_tab(Editor::new(ed.doc().clone()), None);
        self.mark_imported(&st.path);
        self.recent = session::push_recent(&st.path);
        self.status = format!("Opened {} ({w}×{h})", st.path);
    }

    /// The workspace; replaces the editor UI while open.
    pub(crate) fn camera_raw_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.camera_raw.take() else {
            return;
        };
        let mut done: Option<bool> = None;
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(Key::Escape) {
                    done = Some(false);
                }
                if i.key_pressed(Key::Enter) {
                    done = Some(true);
                }
                if i.key_pressed(Key::P) {
                    st.before = !st.before;
                }
            });
        }
        if st.shown != Some((st.dev, st.before)) {
            let shown = if st.before {
                develop(&st.proxy, &Develop::NEUTRAL)
            } else {
                develop(&st.proxy, &st.dev)
            };
            st.hist = histogram::luminance_histogram(&shown);
            let image = to_image(&shown);
            match &mut st.tex {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => st.tex = Some(ctx.load_texture("camera-raw", image, egui::TextureOptions::LINEAR)),
            }
            st.shown = Some((st.dev, st.before));
        }

        let name = if st.filter.is_some() {
            st.path.clone()
        } else {
            file_name(&st.path)
        };
        let (title, enter) = if st.filter.is_some() {
            ("Camera Raw Filter", "P before / after  ·  Enter applies")
        } else {
            ("Camera Raw", "P before / after  ·  Enter opens")
        };
        let (fw, fh) = st.size();
        egui::TopBottomPanel::top("camera-raw-bar")
            .frame(bar_frame())
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(title).strong().color(TEXT));
                    ui.label(RichText::new(&name).color(MUTED));
                    ui.label(RichText::new(format!("{fw} × {fh} px")).monospace().color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(enter).color(MUTED));
                    });
                });
            });

        egui::SidePanel::right("camera-raw-panel")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                // Histogram of what the preview shows.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), Sense::hover());
                let p = ui.painter();
                p.rect_filled(rect, 4.0, GROUND);
                let max = st.hist.iter().copied().max().unwrap_or(1).max(1) as f32;
                let bw = rect.width() / histogram::BINS as f32;
                for (i, &c) in st.hist.iter().enumerate() {
                    if c == 0 {
                        continue;
                    }
                    let hgt = (c as f32 / max).sqrt() * (rect.height() - 4.0);
                    let x = rect.min.x + i as f32 * bw;
                    p.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x, rect.max.y - 2.0 - hgt),
                            egui::pos2(x + bw.max(1.0), rect.max.y - 2.0),
                        ),
                        0.0,
                        MUTED,
                    );
                }
                p.rect_stroke(rect, 4.0, Stroke::new(1.0, LINE));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Auto")
                        .on_hover_text("Set exposure, whites and blacks from the image")
                        .clicked()
                    {
                        let a = auto_develop(&st.proxy);
                        st.dev = Develop {
                            temperature: st.dev.temperature,
                            tint: st.dev.tint,
                            // The filter's input is a finished image: Auto
                            // keeps its curve choice.
                            tone_curve: if st.filter.is_some() {
                                st.dev.tone_curve
                            } else {
                                a.tone_curve
                            },
                            ..a
                        };
                    }
                    if ui
                        .button("Reset")
                        .on_hover_text(if st.filter.is_some() {
                            "Back to the layer as it is"
                        } else {
                            "Back to the camera's rendering"
                        })
                        .clicked()
                    {
                        st.dev = if st.filter.is_some() {
                            Develop::NEUTRAL
                        } else {
                            Develop::default()
                        };
                    }
                    check(ui, &mut st.before, "Before (P)");
                });
                ui.add_space(6.0);
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("camera-raw-sliders")
                    .max_height(ui.available_height() - if st.filter.is_some() { 76.0 } else { 44.0 })
                    .show(ui, |ui| {
                        let d = &mut st.dev;
                        section_title(ui, "WHITE BALANCE");
                        basic_row(ui, "Temperature", &mut d.temperature, -100.0..=100.0, "");
                        basic_row(ui, "Tint", &mut d.tint, -100.0..=100.0, "");
                        section_title(ui, "TONE");
                        let opts = RowOpts {
                            label_w: 92.0,
                            decimals: Some(2),
                            ..RowOpts::default()
                        };
                        slider_row_ex(ui, "Exposure", &mut d.exposure, -5.0..=5.0, " EV", opts);
                        basic_row(ui, "Contrast", &mut d.contrast, -100.0..=100.0, "");
                        basic_row(ui, "Highlights", &mut d.highlights, -100.0..=100.0, "");
                        basic_row(ui, "Shadows", &mut d.shadows, -100.0..=100.0, "");
                        basic_row(ui, "Whites", &mut d.whites, -100.0..=100.0, "");
                        basic_row(ui, "Blacks", &mut d.blacks, -100.0..=100.0, "");
                        check(ui, &mut d.tone_curve, "Camera tone curve")
                            .on_hover_text("The gentle contrast curve raw converters apply by default");
                        section_title(ui, "PRESENCE");
                        basic_row(ui, "Texture", &mut d.texture, -100.0..=100.0, "");
                        basic_row(ui, "Clarity", &mut d.clarity, -100.0..=100.0, "");
                        basic_row(ui, "Dehaze", &mut d.dehaze, -100.0..=100.0, "");
                        basic_row(ui, "Vibrance", &mut d.vibrance, -100.0..=100.0, "");
                        basic_row(ui, "Saturation", &mut d.saturation, -100.0..=100.0, "");
                        section_title(ui, "VIGNETTE");
                        basic_row(ui, "Amount", &mut d.vignette, -100.0..=100.0, "");
                        basic_row(ui, "Midpoint", &mut d.vignette_midpoint, 0.0..=100.0, "");
                    });
                a11y_scroll(ui.ctx(), &scroll, "Camera Raw settings");
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.horizontal(|ui| {
                        let ok = if st.filter.is_some() { "OK" } else { "Open" };
                        if ui.add(primary_button(ok)).clicked() {
                            done = Some(true);
                        }
                        if ui.add(footer_button("Cancel")).clicked() {
                            done = Some(false);
                        }
                    });
                    if let Some(t) = st.filter.as_mut() {
                        crate::camera_raw_filter::apply_as_row(ui, t);
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(avail, Sense::hover());
                a11y_name(&resp, "Camera Raw preview");
                let (pw, ph) = (st.proxy.width as f32, st.proxy.height as f32);
                let s = ((avail.width() - 48.0) / pw)
                    .min((avail.height() - 48.0) / ph)
                    .max(0.01);
                let img = egui::Rect::from_center_size(avail.center(), egui::vec2(pw * s, ph * s));
                if let Some(t) = &st.tex {
                    ui.painter().image(
                        t.id(),
                        img,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                if st.before {
                    ui.painter().text(
                        img.left_top() + egui::vec2(10.0, 10.0),
                        Align2::LEFT_TOP,
                        "Before",
                        FontId::proportional(13.0),
                        TEXT,
                    );
                }
            });

        if st.filter.is_some() {
            // Filter mode: a changed "apply as" may change what it previews.
            self.crf_refresh_source(&mut st);
        }
        match done {
            Some(true) if st.filter.is_some() => self.finish_camera_raw_filter(ctx, *st),
            Some(true) => self.finish_camera_raw(*st),
            Some(false) if st.filter.is_some() => self.status = "Camera Raw Filter cancelled".into(),
            Some(false) => self.status = format!("Did not open {}", st.path),
            None => self.camera_raw = Some(st),
        }
    }

    /// `raw:open=PATH` opens the develop workspace on a file; `raw:auto`
    /// presses Auto; `raw:open-it` presses Open.
    pub(crate) fn debug_camera_raw(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("raw:") else {
            return false;
        };
        if let Some(path) = rest.strip_prefix("open=") {
            self.open_camera_raw(path);
        } else if rest == "auto" {
            if let Some(st) = self.camera_raw.as_mut() {
                st.dev = auto_develop(&st.proxy);
            }
        } else if rest == "open-it" {
            if let Some(st) = self.camera_raw.take() {
                self.finish_camera_raw(*st);
            }
        } else {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(full: Raster) -> CameraRawState {
        let proxy = shrink(&full, PREVIEW_MAX);
        CameraRawState {
            path: "/tmp/shot.CR3".into(),
            full,
            proxy,
            dev: Develop::default(),
            before: false,
            tex: None,
            hist: [0; histogram::BINS],
            shown: None,
            filter: None,
        }
    }

    fn quiet() -> App {
        let mut app = App::launch(&[]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app
    }

    #[test]
    fn open_develops_at_full_size_into_a_new_document() {
        let mut full = Raster::new(3000, 2000);
        for p in &mut full.pixels {
            *p = lumenply_tiles::Rgba::new(0.05, 0.05, 0.05, 1.0);
        }
        let mut app = quiet();
        let mut st = state(full);
        assert!(st.proxy.width <= PREVIEW_MAX && st.proxy.height <= PREVIEW_MAX);
        st.dev = Develop {
            exposure: 1.0,
            ..Develop::NEUTRAL
        };
        app.finish_camera_raw(st);
        let doc = app.editor.doc();
        assert_eq!((doc.canvas().w, doc.canvas().h), (3000, 2000));
        let p = doc.layers()[0].pixels().unwrap().get_pixel(1500, 1000);
        assert!((p.r - 0.1).abs() < 2e-3, "exposure +1 doubled it: {p:?}");
        assert_eq!(app.untitled, "shot.CR3");
        assert_eq!(app.editor.history().len(), 0, "opening is not an undo step");
    }

    #[test]
    fn every_control_in_the_workspace_has_a_spoken_name() {
        let mut full = Raster::new(64, 48);
        for p in &mut full.pixels {
            *p = lumenply_tiles::Rgba::new(0.2, 0.1, 0.05, 1.0);
        }
        let mut app = quiet();
        app.camera_raw = Some(Box::new(state(full)));
        let ctx = crate::a11y_tests::ctx();
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.camera_raw.is_some(), "still open");
        assert_eq!(missing, Vec::<String>::new());
    }

    #[test]
    fn a_broken_raw_file_reports_instead_of_opening() {
        let p = std::env::temp_dir().join(format!("lumenply-broken-{}.cr2", std::process::id()));
        std::fs::write(&p, b"not a raw").unwrap();
        let mut app = quiet();
        app.open_path(&p.to_string_lossy());
        let _ = std::fs::remove_file(&p);
        assert!(app.camera_raw.is_none());
        assert!(app.status.starts_with("Could not open"), "{}", app.status);
    }
}
