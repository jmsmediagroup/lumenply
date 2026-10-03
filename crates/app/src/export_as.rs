//! File ▸ Export ▸ Export As…: one dialog for the everyday web and print
//! exports, as in Photoshop — format (PNG, JPEG, WebP), quality,
//! transparency and output size, with a preview of what will be written
//! and its real file size. Encoding runs on a worker thread so dragging
//! a slider never stalls the UI.

use super::*;
use lumenply_render::resample::resample;
use std::sync::mpsc;

/// Longest preview side in pixels.
const PREVIEW_MAX: u32 = 1400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ExportFormat {
    fn ext(self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Webp => "webp",
            ExportFormat::Gif => "gif",
        }
    }

    fn what(self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG image",
            ExportFormat::Jpeg => "JPEG image",
            ExportFormat::Webp => "WebP image (lossless)",
            ExportFormat::Gif => "GIF image (256 colours)",
        }
    }

    fn has_alpha(self) -> bool {
        self != ExportFormat::Jpeg
    }
}

/// Everything that decides the bytes written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ExportSettings {
    pub(crate) format: ExportFormat,
    /// JPEG quality, 1..=100.
    pub(crate) quality: u8,
    pub(crate) transparency: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// What a worker hands back: the encoded file and a preview of it.
struct Encoded {
    settings: ExportSettings,
    bytes: Result<Vec<u8>, String>,
    preview: Raster,
}

pub(crate) struct ExportAsState {
    flat: std::sync::Arc<Raster>,
    pub(crate) settings: ExportSettings,
    last: Option<Encoded>,
    job: Option<(ExportSettings, mpsc::Receiver<Encoded>)>,
    tex: Option<egui::TextureHandle>,
    shown: Option<ExportSettings>,
}

/// The output for `s`: resampled, encoded, and (for JPEG) decoded back so
/// the preview shows the compression.
fn encode(flat: &Raster, s: ExportSettings) -> Encoded {
    let out = resample(flat, s.width, s.height);
    let bytes = match s.format {
        ExportFormat::Png => lumenply_io::encode_png(&out, s.transparency),
        ExportFormat::Jpeg => lumenply_io::encode_jpeg(&out, s.quality),
        ExportFormat::Webp => lumenply_io::encode_webp(&out, s.transparency),
        ExportFormat::Gif => lumenply_io::encode_gif(&out, s.transparency),
    }
    .map_err(|e| e.to_string());
    let shown = match (&bytes, s.format) {
        (Ok(b), ExportFormat::Jpeg | ExportFormat::Gif) => image::load_from_memory(b)
            .map(|img| {
                let img = img.to_rgba8();
                let mut r = Raster::new(img.width(), img.height());
                for (p, q) in r.pixels.iter_mut().zip(img.pixels()) {
                    *p = lumenply_tiles::Rgba::from_straight(
                        lumenply_io::srgb_to_linear(q[0]),
                        lumenply_io::srgb_to_linear(q[1]),
                        lumenply_io::srgb_to_linear(q[2]),
                        q[3] as f32 / 255.0,
                    );
                }
                r
            })
            .unwrap_or(out),
        _ if !s.transparency => {
            let mut o = out;
            for p in &mut o.pixels {
                *p = p.over(lumenply_tiles::Rgba::WHITE);
            }
            o
        }
        _ => out,
    };
    let f = (shown.width.max(shown.height) as f32 / PREVIEW_MAX as f32).max(1.0);
    let preview = resample(
        &shown,
        (shown.width as f32 / f).round() as u32,
        (shown.height as f32 / f).round() as u32,
    );
    Encoded {
        settings: s,
        bytes,
        preview,
    }
}

fn to_image(r: &Raster) -> egui::ColorImage {
    let (light, dark) = (
        lumenply_io::srgb_to_linear_f(0.80),
        lumenply_io::srgb_to_linear_f(0.66),
    );
    let w = r.width as usize;
    let pixels = r
        .pixels
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let bg = if ((i % w) / 8 + (i / w) / 8) % 2 == 0 {
                light
            } else {
                dark
            };
            let k = 1.0 - p.a.clamp(0.0, 1.0);
            Color32::from_rgb(
                lumenply_io::linear_to_srgb(p.r + bg * k),
                lumenply_io::linear_to_srgb(p.g + bg * k),
                lumenply_io::linear_to_srgb(p.b + bg * k),
            )
        })
        .collect();
    egui::ColorImage {
        size: [w, r.height as usize],
        pixels,
    }
}

/// "1.4 MB", "820 KB".
pub(crate) fn human_size(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", n.div_ceil(1024))
    }
}

impl App {
    pub(crate) fn open_export_as(&mut self) {
        if self.no_doc {
            return;
        }
        let flat = lumenply_render::composite_raster(self.editor.doc());
        let settings = ExportSettings {
            format: ExportFormat::Png,
            quality: 85,
            transparency: true,
            width: flat.width,
            height: flat.height,
        };
        self.export_as = Some(Box::new(ExportAsState {
            flat: std::sync::Arc::new(flat),
            settings,
            last: None,
            job: None,
            tex: None,
            shown: None,
        }));
    }

    /// Starts encoding the current settings unless that's already done or
    /// running; collects a finished job.
    fn export_as_pump(st: &mut ExportAsState, ctx: &egui::Context) {
        if let Some((_, rx)) = &st.job {
            match rx.try_recv() {
                Ok(done) => {
                    st.last = Some(done);
                    st.job = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                }
                Err(mpsc::TryRecvError::Disconnected) => st.job = None,
            }
        }
        let current = st.last.as_ref().map(|e| e.settings) == Some(st.settings);
        if !current && st.job.is_none() {
            let (tx, rx) = mpsc::channel();
            let flat = st.flat.clone();
            let s = st.settings;
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(encode(&flat, s));
                ctx2.request_repaint();
            });
            st.job = Some((s, rx));
        }
    }

    /// Writes the encoded file, waiting for the current settings' encode.
    fn export_as_write(&mut self, mut st: ExportAsState, path: &str) -> bool {
        let s = st.settings;
        let ready = st
            .last
            .as_ref()
            .filter(|e| e.settings == s)
            .map(|e| e.bytes.clone());
        let bytes = match ready {
            Some(b) => b,
            None => encode(&st.flat, s).bytes,
        };
        st.job = None;
        // PNG and JPEG carry the document's print resolution.
        let ppi = self.editor.doc().resolution;
        let bytes = bytes.map(|b| lumenply_io::resolution::with_ppi(b, ppi));
        match bytes.and_then(|b| std::fs::write(path, b).map_err(|e| e.to_string())) {
            Ok(()) => {
                self.status = format!("Exported {path} ({}×{})", s.width, s.height);
                true
            }
            Err(e) => {
                self.status = format!("Could not export {path}: {e}");
                false
            }
        }
    }

    pub(crate) fn export_as_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.export_as.take() else {
            return;
        };
        Self::export_as_pump(&mut st, ctx);
        let mut done: Option<bool> = None;
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(Key::Escape) {
                    done = Some(false);
                }
                if i.key_pressed(Key::Enter) {
                    done = Some(true);
                }
            });
        }
        if let Some(e) = &st.last {
            if st.shown != Some(e.settings) {
                let image = to_image(&e.preview);
                match &mut st.tex {
                    Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                    None => st.tex = Some(ctx.load_texture("export-as", image, egui::TextureOptions::LINEAR)),
                }
                st.shown = Some(e.settings);
            }
        }

        egui::TopBottomPanel::top("export-as-bar")
            .frame(bar_frame())
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Export As").strong().color(TEXT));
                    let name = self
                        .path
                        .as_ref()
                        .map_or(self.untitled.clone(), |p| file_name(&p.to_string_lossy()));
                    ui.label(RichText::new(name).color(MUTED));
                });
            });

        let (fw, fh) = (st.flat.width, st.flat.height);
        egui::SidePanel::right("export-as-panel")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                let s = &mut st.settings;
                section_title(ui, "FILE SETTINGS");
                segmented(
                    ui,
                    &mut s.format,
                    &[
                        (ExportFormat::Png, "PNG"),
                        (ExportFormat::Jpeg, "JPEG"),
                        (ExportFormat::Webp, "WebP"),
                        (ExportFormat::Gif, "GIF"),
                    ],
                );
                ui.add_space(4.0);
                if s.format.has_alpha() {
                    check(ui, &mut s.transparency, "Transparency")
                        .on_hover_text("Untick to place the image on white");
                } else {
                    let mut q = s.quality as f32;
                    let opts = RowOpts {
                        int: true,
                        ..RowOpts::default()
                    };
                    slider_row_ex(ui, "Quality", &mut q, 1.0..=100.0, "", opts);
                    s.quality = q.round().clamp(1.0, 100.0) as u8;
                }
                ui.add_space(8.0);
                section_title(ui, "IMAGE SIZE");
                let mut w = s.width as f32;
                let mut h = s.height as f32;
                let wr = field_row(
                    ui,
                    "Width",
                    egui::DragValue::new(&mut w).range(1.0..=32768.0).suffix(" px"),
                );
                let hr = field_row(
                    ui,
                    "Height",
                    egui::DragValue::new(&mut h).range(1.0..=32768.0).suffix(" px"),
                );
                if wr.changed() {
                    s.width = w.round().max(1.0) as u32;
                    s.height = ((s.width as f32 * fh as f32 / fw as f32).round() as u32).max(1);
                } else if hr.changed() {
                    s.height = h.round().max(1.0) as u32;
                    s.width = ((s.height as f32 * fw as f32 / fh as f32).round() as u32).max(1);
                }
                let mut pct = s.width as f32 / fw as f32 * 100.0;
                let before = pct;
                let opts = RowOpts {
                    log: true,
                    ..RowOpts::default()
                };
                slider_row_ex(ui, "Scale", &mut pct, 1.0..=400.0, "%", opts);
                if (pct - before).abs() > 1e-3 {
                    s.width = ((fw as f32 * pct / 100.0).round() as u32).max(1);
                    s.height = ((fh as f32 * pct / 100.0).round() as u32).max(1);
                }
                ui.horizontal(|ui| {
                    for p in [25u32, 50, 100, 200] {
                        if ui.button(format!("{p}%")).clicked() {
                            s.width = (fw * p / 100).max(1);
                            s.height = (fh * p / 100).max(1);
                        }
                    }
                });
                ui.add_space(10.0);
                let size = match &st.last {
                    Some(e) if e.settings == st.settings => match &e.bytes {
                        Ok(b) => human_size(b.len()),
                        Err(err) => format!("Error: {err}"),
                    },
                    _ => "Measuring…".to_string(),
                };
                ui.label(
                    RichText::new(format!(
                        "{} × {} px  ·  {size}",
                        st.settings.width, st.settings.height
                    ))
                    .monospace()
                    .color(TEXT),
                );
                ui.label(
                    RichText::new("sRGB, colour profile embedded")
                        .small()
                        .color(MUTED),
                );
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(primary_button("Export…")).clicked() {
                            done = Some(true);
                        }
                        if ui.add(footer_button("Cancel")).clicked() {
                            done = Some(false);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(avail, Sense::hover());
                a11y_name(&resp, "Export preview");
                if let (Some(t), Some(e)) = (&st.tex, &st.last) {
                    let (pw, ph) = (e.preview.width as f32, e.preview.height as f32);
                    let s = ((avail.width() - 48.0) / pw)
                        .min((avail.height() - 48.0) / ph)
                        .clamp(0.01, 1.0);
                    let img = egui::Rect::from_center_size(avail.center(), egui::vec2(pw * s, ph * s));
                    ui.painter().image(
                        t.id(),
                        img,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if st.job.is_some() {
                        ui.painter().text(
                            img.left_top() + egui::vec2(10.0, 10.0),
                            Align2::LEFT_TOP,
                            "Updating…",
                            FontId::proportional(13.0),
                            TEXT,
                        );
                    }
                }
            });

        match done {
            Some(true) => {
                let f = st.settings.format;
                match self.pick_save_path("Export As", f.what(), &[f.ext()]) {
                    Some(path) => {
                        self.export_as_write(*st, &path);
                    }
                    None => self.export_as = Some(st),
                }
            }
            Some(false) => self.status = "Export cancelled".into(),
            None => self.export_as = Some(st),
        }
    }

    /// `export-as:jpeg=Q`, `export-as:scale=P`, `export-as:write=PATH`.
    pub(crate) fn debug_export_as(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("export-as:") else {
            return false;
        };
        if self.export_as.is_none() {
            self.open_export_as();
        }
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        match verb {
            "open" => {}
            "jpeg" | "webp" | "png" | "gif" => {
                if let Some(st) = self.export_as.as_mut() {
                    st.settings.format = match verb {
                        "jpeg" => ExportFormat::Jpeg,
                        "webp" => ExportFormat::Webp,
                        "gif" => ExportFormat::Gif,
                        _ => ExportFormat::Png,
                    };
                    if let Ok(q) = arg.parse() {
                        st.settings.quality = q;
                    }
                }
            }
            "scale" => {
                if let (Some(st), Ok(p)) = (self.export_as.as_mut(), arg.parse::<u32>()) {
                    st.settings.width = (st.flat.width * p / 100).max(1);
                    st.settings.height = (st.flat.height * p / 100).max(1);
                }
            }
            "write" => {
                if let Some(st) = self.export_as.take() {
                    self.export_as_write(*st, arg);
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(w: u32, h: u32) -> App {
        let mut app = App::launch(&[]);
        app.open_in_new_tab(blank(w, h), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app
    }

    #[test]
    fn exports_each_format_at_the_chosen_size() {
        let dir = std::env::temp_dir().join(format!("lumenply-export-as-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (fmt, ext) in [("png", "png"), ("jpeg=70", "jpg"), ("webp", "webp")] {
            let mut app = app_with(120, 80);
            let ctx = egui::Context::default();
            assert!(app.debug_export_as(&ctx, &format!("export-as:{fmt}")));
            assert!(app.debug_export_as(&ctx, "export-as:scale=50"));
            let path = dir.join(format!("out.{ext}"));
            assert!(app.debug_export_as(&ctx, &format!("export-as:write={}", path.display())));
            assert!(app.export_as.is_none());
            let bytes = std::fs::read(&path).unwrap();
            let (w, h) = if ext == "webp" {
                image_webp::WebPDecoder::new(std::io::Cursor::new(&bytes))
                    .unwrap()
                    .dimensions()
            } else {
                let img = image::load_from_memory(&bytes).unwrap();
                (img.width(), img.height())
            };
            assert_eq!((w, h), (60, 40), "{ext}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_size_fields_keep_the_aspect_ratio_and_sizes_read_well() {
        assert_eq!(human_size(1500), "2 KB");
        assert_eq!(human_size(3 * 1024 * 1024 + 300_000), "3.3 MB");
        let mut app = app_with(300, 200);
        app.open_export_as();
        let ctx = crate::a11y_tests::ctx();
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.export_as.is_some(), "still open");
        assert_eq!(missing, Vec::<String>::new());
    }
}
