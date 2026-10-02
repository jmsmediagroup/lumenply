//! The welcome screen and the no-document state.
//!
//! With nothing open (a plain launch, or after the last tab closes) the app
//! shows a start screen — New, Open, the demo, recent files, a drop hint —
//! instead of an empty canvas. `App::no_doc` marks the state: the live
//! `editor` is then an empty 1×1 placeholder that nothing may edit (`run`
//! refuses), and the tool rail, docks, options bar and history are hidden.

use std::path::Path;
use std::time::SystemTime;

use super::*;

/// What the command line asked for at launch.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LaunchArgs {
    pub demo: bool,
    /// Documents to open, each in its own tab, in order.
    pub files: Vec<String>,
    /// Images to place as layers into the document that ends up live.
    pub places: Vec<String>,
}

/// Flags that take the next argument as their value.
const VALUE_FLAGS: &[&str] = &["--screenshot", "--screenshot-do", "--window-size", "--place"];

/// `lumenply-app [--demo] [file ...] [--place image ...]` plus the debug
/// flags. Flag values are never mistaken for files to open.
pub(crate) fn parse_launch_args(args: &[String]) -> LaunchArgs {
    let mut out = LaunchArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--demo" => out.demo = true,
            "--place" => {
                if let Some(p) = args.get(i + 1) {
                    out.places.push(p.clone());
                }
                i += 1;
            }
            f if VALUE_FLAGS.contains(&f) => i += 1,
            f if f.starts_with("--") => {} // unknown flag: ignore
            p => out.files.push(p.to_string()),
        }
        i += 1;
    }
    out
}

/// "just now", "5 min ago", "yesterday", "3 months ago" for an age in seconds.
pub(crate) fn relative_age(secs: u64) -> String {
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match secs {
        s if s < MIN => "just now".into(),
        s if s < HOUR => format!("{} min ago", s / MIN),
        s if s < DAY => format!("{} h ago", s / HOUR),
        s if s < 2 * DAY => "yesterday".into(),
        s if s < 30 * DAY => plural(s / DAY, "day"),
        s if s < 365 * DAY => plural(s / (30 * DAY), "month"),
        s => plural(s / (365 * DAY), "year"),
    }
}

/// The short type label shown on a recent-file row.
pub(crate) fn kind_label(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "lumen" | "nge" => "LUMEN".into(),
        "jpeg" => "JPG".into(),
        "tiff" => "TIF".into(),
        "" => "FILE".into(),
        e => e.to_ascii_uppercase(),
    }
}

/// A path's folder for display, with the home directory shortened to `~`.
pub(crate) fn display_dir(path: &str, home: Option<&Path>) -> String {
    let dir = Path::new(path).parent().unwrap_or(Path::new(""));
    if let Some(rest) = home.and_then(|h| dir.strip_prefix(h).ok()) {
        let rest = rest.to_string_lossy();
        return if rest.is_empty() {
            "~".into()
        } else {
            format!("~{}{rest}", std::path::MAIN_SEPARATOR)
        };
    }
    dir.to_string_lossy().into_owned()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Accent hover shade for the primary button.
const ACCENT_HOVER: Color32 = Color32::from_rgb(0xFF, 0xC4, 0x6E);
/// Width of the left (start actions) column.
const LEFT_W: f32 = 300.0;
const COL_GAP: f32 = 36.0;

#[derive(Clone, Copy)]
enum Glyph {
    New,
    Open,
}

impl App {
    /// Leave every document behind and show the welcome screen. Callers
    /// have already dealt with unsaved changes.
    pub(crate) fn show_welcome(&mut self) {
        self.cancel_interaction();
        self.editor = Editor::new(Document::new(1, 1));
        self.path = None;
        self.saved_rev = self.editor.history().len();
        self.hist_thumbs.clear();
        self.below = lumenply_render::BelowCache::new();
        self.active = None;
        self.selected.clear();
        self.editing_mask = false;
        self.quick_mask = false;
        self.renaming = None;
        self.layer_drag = None;
        self.curve_drag = None;
        self.palette = None;
        self.canvas_tex = None;
        self.overlay_tex = None;
        self.last_flat = None;
        self.thumbs.clear();
        self.mask_thumbs.clear();
        self.sel_points.clear();
        self.dirty = false;
        self.dirty_rect = None;
        self.cur_tab = 0;
        self.no_doc = true;
        // Messages about the closed document no longer apply.
        self.status.clear();
        // With nothing open there is nothing to back up: a leftover backup
        // could only be of work the user just chose to discard (a save
        // already removed it). Keep it while the Recover prompt offers it.
        if !matches!(self.dialog, Some(Dialog::Recover)) {
            session::remove_autosave();
        }
        // Another window may have opened files since this one started.
        self.recent = session::load_recent();
    }

    /// Crash recovery: open the autosave backup in a new tab (from the
    /// welcome screen it becomes the first one), marked unsaved, with the
    /// path it came from so Save goes back to the original file.
    pub(crate) fn recover_autosave(&mut self) {
        let Some(file) = session::autosave_file() else {
            return;
        };
        let source = session::autosave_source();
        match project::load(&file) {
            Ok(doc) => {
                let had_path = source.is_some();
                self.open_in_new_tab(Editor::new(doc), source);
                if !had_path {
                    self.untitled = "Recovered".into();
                }
                // Recovered work is unsaved by definition.
                self.saved_rev = usize::MAX;
                self.status = "Recovered the autosaved document".into();
            }
            Err(e) => self.status = format!("Could not recover the backup: {e}"),
        }
    }

    /// Open the photo demo in a new tab.
    pub(crate) fn open_demo(&mut self) {
        match demo::build() {
            Ok(ed) => {
                let first = demo::initial_layer(ed.doc());
                self.open_in_new_tab(ed, None);
                self.set_active(first);
                self.fix_active();
                self.untitled = demo::TITLE.into();
                self.status = "Opened the demo: every edit is a layer you can change or hide".into();
            }
            Err(e) => self.status = format!("Could not open the demo: {e}"),
        }
    }

    /// The whole window while no document is open (below the menu bar).
    pub(crate) fn welcome(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("welcome-status")
            .exact_height(26.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Errors get a banner in the page itself (welcome_error).
                    let quiet = self.status.is_empty()
                        || self.status == "Ready"
                        || self.status.starts_with("Could not");
                    let msg = if quiet {
                        "No document open"
                    } else {
                        self.status.as_str()
                    };
                    ui.label(RichText::new(msg).size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("Lumenply {}", env!("CARGO_PKG_VERSION")))
                                .monospace()
                                .size(11.0)
                                .color(MUTED),
                        );
                    });
                });
            });

        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(GROUND))
            .show(ctx, |ui| {
                // Centre the content vertically using last frame's height.
                let height_id = egui::Id::new("welcome-content-height");
                let last_h = ctx.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(560.0);
                let view = ui.available_size();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let w = (view.x - 64.0).clamp(560.0, 980.0);
                        let top = ((view.y - last_h) / 2.0).max(28.0);
                        ui.add_space(top);
                        let content_top = ui.cursor().top();
                        let left_id = egui::Id::new("welcome-left-height");
                        let left_h = ctx.data(|d| d.get_temp::<f32>(left_id)).unwrap_or(440.0);
                        ui.horizontal(|ui| {
                            ui.add_space(((view.x - w) / 2.0).max(0.0));
                            ui.vertical(|ui| {
                                ui.set_width(w);
                                self.welcome_header(ui);
                                ui.add_space(26.0);
                                self.welcome_error(ui);
                                ui.horizontal_top(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    let left = ui.vertical(|ui| {
                                        ui.set_width(LEFT_W);
                                        self.welcome_actions(ui);
                                    });
                                    let lh = left.response.rect.height();
                                    if (lh - left_h).abs() > 0.5 {
                                        ctx.data_mut(|d| d.insert_temp(left_id, lh));
                                        ctx.request_repaint();
                                    }
                                    ui.add_space(COL_GAP);
                                    ui.vertical(|ui| {
                                        ui.set_width(w - LEFT_W - COL_GAP);
                                        self.welcome_recent(ui, left_h);
                                    });
                                });
                                ui.add_space(26.0);
                                drop_zone(ui, hovering);
                            });
                        });
                        let h = ui.cursor().top() - content_top;
                        ui.add_space(28.0);
                        if (h - last_h).abs() > 0.5 {
                            ctx.data_mut(|d| d.insert_temp(height_id, h));
                            ctx.request_repaint();
                        }
                    });
            });
    }

    fn welcome_header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(58.0), Sense::hover());
            ui.painter().rect_filled(r, 15.0, PANEL);
            ui.painter().rect_stroke(r, 15.0, Stroke::new(1.0, LINE));
            brand::paint_mark(ui.painter(), r.shrink(10.0), TEXT, ACCENT, PANEL);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(3.0);
                ui.label(
                    RichText::new("Lumenply")
                        .family(egui::FontFamily::Name("semibold".into()))
                        .size(28.0)
                        .color(TEXT),
                );
                ui.label(
                    RichText::new("Free, non-destructive photo editing")
                        .size(14.0)
                        .color(MUTED),
                );
            });
        });
    }

    /// A failed open (from here, the command line or a drop) shown where
    /// the user is looking, not only in the footer. × dismisses it.
    fn welcome_error(&mut self, ui: &mut egui::Ui) {
        if !self.status.starts_with("Could not") {
            return;
        }
        let w = ui.available_width();
        let font = FontId::proportional(13.0);
        let galley = ui.painter().layout(self.status.clone(), font, TEXT, w - 84.0);
        let h = (galley.size().y + 22.0).max(42.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 10.0, ACCENT_TINT);
        p.rect_stroke(rect, 10.0, Stroke::new(1.0, ACCENT.gamma_multiply(0.55)));
        let dot = egui::pos2(rect.left() + 22.0, rect.center().y);
        p.circle_filled(dot, 9.0, ACCENT);
        p.text(
            dot,
            Align2::CENTER_CENTER,
            "!",
            FontId::new(13.0, semibold()),
            ACCENT_INK,
        );
        p.galley(
            egui::pos2(rect.left() + 42.0, rect.center().y - galley.size().y / 2.0),
            galley,
            TEXT,
        );
        let x_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 20.0, rect.center().y),
            Vec2::splat(22.0),
        );
        let x = ui.interact(x_rect, ui.id().with("welcome-error-x"), Sense::click());
        ui.painter().text(
            x_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(16.0),
            if x.hovered() { TEXT } else { MUTED },
        );
        if x.on_hover_text("Dismiss").clicked() {
            self.status.clear();
        }
        ui.add_space(18.0);
    }

    fn welcome_actions(&mut self, ui: &mut egui::Ui) {
        section_label(ui, "START", ui.available_width());
        ui.add_space(10.0);
        if action_button(
            ui,
            Glyph::New,
            "New image…",
            "A blank canvas at any size",
            None,
            true,
        )
        .clicked()
        {
            self.dialog = Some(Dialog::New(1920, 1080));
        }
        ui.add_space(10.0);
        let open_hint = session::chord_label(ui.ctx(), &self.prefs, "open");
        if action_button(
            ui,
            Glyph::Open,
            "Open…",
            "Projects, PSD, OpenRaster or images",
            Some(&open_hint),
            false,
        )
        .clicked()
        {
            self.pick_open();
        }
        ui.add_space(22.0);
        section_label(ui, "TRY IT", ui.available_width());
        ui.add_space(10.0);
        if self.demo_card(ui).clicked() {
            self.open_demo();
        }
    }

    /// The demo as a clickable card: the photo with a caption below.
    fn demo_card(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if self.start_thumb.is_none() {
            if let Ok(img) = image::load_from_memory_with_format(demo::PHOTO_JPG, image::ImageFormat::Jpeg) {
                let t = img.thumbnail(640, 640).to_rgba8();
                let (w, h) = t.dimensions();
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], t.as_raw());
                self.start_thumb = Some(ui.ctx().load_texture(
                    "welcome-demo",
                    ci,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        let img_h = (LEFT_W * 1205.0 / 1800.0).round();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(LEFT_W, img_h + 58.0), Sense::click());
        let p = ui.painter();
        let hot = resp.hovered();
        p.rect_filled(rect, 10.0, if hot { RAISED } else { PANEL });
        let img_rect = egui::Rect::from_min_size(rect.min, egui::vec2(LEFT_W, img_h));
        if let Some(tex) = &self.start_thumb {
            egui::Image::new((tex.id(), img_rect.size()))
                .rounding(egui::Rounding {
                    nw: 10.0,
                    ne: 10.0,
                    sw: 0.0,
                    se: 0.0,
                })
                .paint_at(ui, img_rect);
        }
        p.rect_stroke(rect, 10.0, Stroke::new(1.0, if hot { ACCENT } else { LINE }));
        let text_x = rect.left() + 14.0;
        p.text(
            egui::pos2(text_x, img_rect.bottom() + 12.0),
            Align2::LEFT_TOP,
            "Open the demo photo",
            FontId::new(14.0, egui::FontFamily::Name("semibold".into())),
            TEXT,
        );
        p.text(
            egui::pos2(text_x, img_rect.bottom() + 33.0),
            Align2::LEFT_TOP,
            "Adjustment layers, a mask and a title",
            FontId::proportional(12.0),
            MUTED,
        );
        if hot {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp.on_hover_text("Aoraki at sunrise — photo by Aleks Dahlberg (CC0)")
    }

    /// Recent files, newest first. `column_h` is the left column's height,
    /// which the empty state matches so the two columns end together.
    fn welcome_recent(&mut self, ui: &mut egui::Ui, column_h: f32) {
        let width = ui.available_width();
        let head = section_label(ui, "RECENT", width);
        if !self.recent.is_empty() {
            let clear_rect = egui::Rect::from_min_max(
                egui::pos2(head.right() - 80.0, head.top() - 4.0),
                egui::pos2(head.right(), head.bottom() + 4.0),
            );
            let clear = ui.interact(clear_rect, ui.id().with("welcome-clear-recent"), Sense::click());
            ui.painter().text(
                egui::pos2(head.right() - 2.0, head.center().y),
                Align2::RIGHT_CENTER,
                "Clear list",
                FontId::proportional(11.5),
                if clear.hovered() { TEXT } else { MUTED },
            );
            if clear.on_hover_text("Forget the recent files").clicked() {
                self.recent = session::clear_recent();
            }
        }
        ui.add_space(10.0);
        if self.recent.is_empty() {
            let h = (column_h - ui.cursor().top() + head.top()).max(150.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(width, h), Sense::hover());
            let p = ui.painter();
            p.rect_filled(r, 10.0, PANEL);
            // A stack of two sheets as a quiet illustration.
            let c = r.center() - egui::vec2(0.0, 34.0);
            for (d, ink) in [(7.0, MUTED.gamma_multiply(0.45)), (0.0, MUTED)] {
                let sheet = egui::Rect::from_center_size(c + egui::vec2(d, -d), egui::vec2(26.0, 32.0));
                p.rect_filled(sheet, 4.0, PANEL);
                p.rect_stroke(sheet, 4.0, Stroke::new(1.5, ink));
            }
            for (i, len) in [14.0, 14.0, 9.0].into_iter().enumerate() {
                let y = c.y - 6.0 + i as f32 * 6.0;
                let x = c.x - 7.0;
                p.line_segment(
                    [egui::pos2(x, y), egui::pos2(x + len, y)],
                    Stroke::new(1.5, MUTED.gamma_multiply(0.7)),
                );
            }
            p.text(
                r.center() + egui::vec2(0.0, 10.0),
                Align2::CENTER_CENTER,
                "No recent files yet",
                FontId::new(14.0, semibold()),
                TEXT,
            );
            p.text(
                r.center() + egui::vec2(0.0, 32.0),
                Align2::CENTER_CENTER,
                "Projects and images you open or save will be listed here",
                FontId::proportional(12.0),
                MUTED,
            );
            return;
        }
        let home = home_dir();
        let now = SystemTime::now();
        let mut open: Option<String> = None;
        let mut remove: Option<String> = None;
        let recent = self.recent.clone();
        ui.spacing_mut().item_spacing.y = 2.0;
        for path in &recent {
            let meta = std::fs::metadata(path).ok();
            let age = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .map(|d| relative_age(d.as_secs()));
            let missing = meta.is_none();
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 46.0), Sense::click());
            let hot = resp.hovered();
            let p = ui.painter();
            if hot {
                p.rect_filled(rect, 8.0, RAISED);
            }
            // Type chip.
            let kind = kind_label(path);
            let (chip_fill, chip_ink) = match kind.as_str() {
                "LUMEN" => (ACCENT_TINT, ACCENT),
                "PSD" | "PSB" | "ORA" => (Color32::from_rgb(0x26, 0x30, 0x3C), LIVE_FILTER),
                _ => (if hot { PANEL } else { RAISED }, MUTED),
            };
            let chip = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 10.0, rect.center().y - 10.0),
                egui::vec2(48.0, 20.0),
            );
            p.rect_filled(chip, 5.0, chip_fill);
            p.text(
                chip.center(),
                Align2::CENTER_CENTER,
                &kind,
                FontId::monospace(10.5),
                chip_ink,
            );

            // Right column: age or "Not found" (plus a remove ×).
            let right = rect.right() - 12.0;
            let (status, status_color) = match &age {
                Some(a) => (a.clone(), MUTED),
                None => ("Not found".to_string(), ACCENT),
            };
            let status_w = if missing { 96.0 } else { 90.0 };
            let x_rect =
                egui::Rect::from_center_size(egui::pos2(right - 8.0, rect.center().y), Vec2::splat(20.0));
            let status_right = if missing { x_rect.left() - 6.0 } else { right };
            p.text(
                egui::pos2(status_right, rect.center().y),
                Align2::RIGHT_CENTER,
                &status,
                FontId::monospace(11.0),
                status_color,
            );

            // Name over folder, truncated to fit.
            let text_x = chip.right() + 12.0;
            let text_w = (status_right - status_w - text_x).max(40.0);
            let name_galley = p.layout_job(one_line(
                &file_name(path),
                FontId::proportional(13.5),
                if missing { MUTED } else { TEXT },
                text_w,
            ));
            p.galley(egui::pos2(text_x, rect.top() + 6.0), name_galley, TEXT);
            // Folders lose their middle, not their end: the last folder
            // is usually the one that identifies the file.
            let dir_font = FontId::proportional(11.5);
            let dir = fit_middle(p, &display_dir(path, home.as_deref()), &dir_font, text_w);
            p.text(
                egui::pos2(text_x, rect.top() + 26.0),
                Align2::LEFT_TOP,
                dir,
                dir_font,
                MUTED,
            );

            if missing {
                let x_hot = ui.rect_contains_pointer(x_rect);
                p.text(
                    x_rect.center(),
                    Align2::CENTER_CENTER,
                    "×",
                    FontId::proportional(15.0),
                    if x_hot { TEXT } else { MUTED },
                );
                if resp.clicked() && x_hot {
                    remove = Some(path.clone());
                }
            }
            if hot {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let resp = resp.on_hover_text(path.as_str());
            if resp.clicked() && remove.is_none() {
                if missing {
                    self.status = format!(
                        "Could not open {}: the file was moved or deleted",
                        file_name(path)
                    );
                } else {
                    open = Some(path.clone());
                }
            }
            resp.context_menu(|ui| {
                if ui.add_enabled(!missing, egui::Button::new("Open")).clicked() {
                    open = Some(path.clone());
                    ui.close_menu();
                }
                if ui.button("Remove from list").clicked() {
                    remove = Some(path.clone());
                    ui.close_menu();
                }
            });
        }
        if let Some(p) = remove {
            self.recent = session::remove_recent(&p);
        }
        if let Some(p) = open {
            self.open_path(&p);
        }
    }
}

/// A single truncated line of text.
fn one_line(text: &str, font: FontId, color: Color32, max_w: f32) -> egui::text::LayoutJob {
    let mut job =
        egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat::simple(font, color));
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
    job
}

/// `text` cut to `keep` characters by removing its middle: a third of the
/// budget for the head, the rest for the tail, joined by an ellipsis.
pub(crate) fn elide_middle(text: &str, keep: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= keep {
        return text.to_string();
    }
    let budget = keep.saturating_sub(1); // room for the ellipsis
    let head = budget / 3;
    let tail = budget - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// The longest middle-elided form of `text` that fits `max_w`.
fn fit_middle(p: &egui::Painter, text: &str, font: &FontId, max_w: f32) -> String {
    let width = |s: &str| p.layout_no_wrap(s.to_string(), font.clone(), MUTED).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let (mut lo, mut hi) = (1, text.chars().count());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&elide_middle(text, mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    elide_middle(text, lo)
}

fn semibold() -> egui::FontFamily {
    egui::FontFamily::Name("semibold".into())
}

/// A small caps section heading on a fixed-height row; returns the row.
fn section_label(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Rect {
    let (r, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), Sense::hover());
    ui.painter().text(
        r.left_center(),
        Align2::LEFT_CENTER,
        text,
        FontId::new(11.0, semibold()),
        MUTED,
    );
    r
}

/// A large start-screen button: glyph, title, one-line description and an
/// optional shortcut hint. The primary one wears the accent.
fn action_button(
    ui: &mut egui::Ui,
    glyph: Glyph,
    title: &str,
    sub: &str,
    hint: Option<&str>,
    primary: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(LEFT_W, 56.0), Sense::click());
    let hot = resp.hovered();
    let p = ui.painter();
    let (fill, ink, sub_ink) = if primary {
        (
            if hot { ACCENT_HOVER } else { ACCENT },
            ACCENT_INK,
            Color32::from_rgb(0x5A, 0x42, 0x14),
        )
    } else {
        (if hot { RAISED } else { PANEL }, TEXT, MUTED)
    };
    p.rect_filled(rect, 10.0, fill);
    if !primary {
        p.rect_stroke(rect, 10.0, Stroke::new(1.0, LINE));
    }
    let icon =
        egui::Rect::from_center_size(egui::pos2(rect.left() + 28.0, rect.center().y), Vec2::splat(18.0));
    paint_glyph(p, icon, glyph, ink);
    let x = rect.left() + 52.0;
    p.text(
        egui::pos2(x, rect.top() + 10.0),
        Align2::LEFT_TOP,
        title,
        FontId::new(14.5, egui::FontFamily::Name("semibold".into())),
        ink,
    );
    p.text(
        egui::pos2(x, rect.top() + 31.0),
        Align2::LEFT_TOP,
        sub,
        FontId::proportional(12.0),
        sub_ink,
    );
    if let Some(h) = hint {
        p.text(
            egui::pos2(rect.right() - 14.0, rect.top() + 18.0),
            Align2::RIGHT_CENTER,
            h,
            FontId::monospace(11.0),
            sub_ink,
        );
    }
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn paint_glyph(p: &egui::Painter, r: egui::Rect, glyph: Glyph, c: Color32) {
    let s = Stroke::new(1.8, c);
    match glyph {
        Glyph::New => {
            // A page with a plus.
            let page = egui::Rect::from_center_size(r.center(), egui::vec2(r.width() * 0.8, r.height()));
            p.rect_stroke(page, 2.5, s);
            let m = page.center();
            p.line_segment([m - egui::vec2(4.0, 0.0), m + egui::vec2(4.0, 0.0)], s);
            p.line_segment([m - egui::vec2(0.0, 4.0), m + egui::vec2(0.0, 4.0)], s);
        }
        Glyph::Open => {
            // A folder.
            let (l, t, rt, b) = (r.left(), r.top() + 2.0, r.right(), r.bottom() - 1.0);
            let pts = vec![
                egui::pos2(l, b),
                egui::pos2(l, t),
                egui::pos2(l + 6.0, t),
                egui::pos2(l + 8.5, t + 3.0),
                egui::pos2(rt, t + 3.0),
                egui::pos2(rt, b),
            ];
            p.add(Shape::closed_line(pts, s));
            p.line_segment([egui::pos2(l, t + 6.5), egui::pos2(rt, t + 6.5)], s);
        }
    }
}

/// The drop hint: a dashed well across the bottom, lit while files hover.
fn drop_zone(ui: &mut egui::Ui, hovering: bool) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 66.0), Sense::hover());
    let p = ui.painter();
    let ink = if hovering { ACCENT } else { LINE };
    if hovering {
        p.rect_filled(rect, 10.0, ACCENT_TINT);
    }
    let r = rect.shrink(0.5);
    let corners = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    p.extend(Shape::dashed_line(&corners, Stroke::new(1.2, ink), 6.0, 5.0));
    // Tray-with-arrow glyph.
    let g = egui::Rect::from_center_size(egui::pos2(rect.left() + 34.0, rect.center().y), Vec2::splat(20.0));
    let gs = Stroke::new(1.6, if hovering { ACCENT } else { MUTED });
    p.line_segment([g.center_top(), g.center() + egui::vec2(0.0, 3.0)], gs);
    p.line_segment(
        [
            g.center() + egui::vec2(-4.5, -1.5),
            g.center() + egui::vec2(0.0, 3.0),
        ],
        gs,
    );
    p.line_segment(
        [
            g.center() + egui::vec2(4.5, -1.5),
            g.center() + egui::vec2(0.0, 3.0),
        ],
        gs,
    );
    p.add(Shape::line(
        vec![
            g.left_center() + egui::vec2(0.0, 2.0),
            g.left_bottom(),
            g.right_bottom(),
            g.right_center() + egui::vec2(0.0, 2.0),
        ],
        gs,
    ));
    let x = rect.left() + 60.0;
    p.text(
        egui::pos2(x, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        if hovering {
            "Release to open"
        } else {
            "Drop a photo, PSD or project anywhere in this window to open it"
        },
        FontId::proportional(13.5),
        if hovering { TEXT } else { MUTED },
    );
    p.text(
        egui::pos2(x, rect.center().y + 11.0),
        Align2::LEFT_CENTER,
        "LUMEN · PSD · ORA · PNG · JPEG · TIFF · WEBP · EXR",
        FontId::monospace(10.5),
        MUTED,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flag_values_are_not_opened_as_files() {
        // The old parser opened "--screenshot" itself as a file.
        let a = parse_launch_args(&args(&[
            "--screenshot",
            "/tmp/ui.png",
            "--window-size",
            "900x600",
        ]));
        assert_eq!(a, LaunchArgs::default());
        let a = parse_launch_args(&args(&[
            "--screenshot-do",
            "fit,start:welcome",
            "photo.jpg",
            "--place",
            "logo.png",
            "poster.psd",
        ]));
        assert_eq!(a.files, ["photo.jpg", "poster.psd"]);
        assert_eq!(a.places, ["logo.png"]);
        assert!(!a.demo);
    }

    #[test]
    fn demo_flag_anywhere() {
        let a = parse_launch_args(&args(&["--screenshot", "x.png", "--demo"]));
        assert!(a.demo);
        assert!(a.files.is_empty());
        // A trailing value flag with no value is ignored, not a crash.
        let a = parse_launch_args(&args(&["--demo", "--place"]));
        assert!(a.demo && a.places.is_empty());
    }

    /// App-level tests share the (test-only) data dir and its autosave
    /// file: run them one at a time.
    static APP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn app_lock() -> std::sync::MutexGuard<'static, ()> {
        APP_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_crash_backup_is_offered_and_recovers_into_a_tab() {
        let _g = app_lock();
        let dir = session::data_dir().unwrap();
        let file = session::autosave_file().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        project::save(&file, blank(40, 30).doc()).unwrap();
        std::fs::write(session::autosave_source_file().unwrap(), "/work/poster.lumen").unwrap();

        let mut app = App::launch(&[]);
        assert!(app.no_doc, "the prompt shows over the welcome screen");
        assert!(matches!(app.dialog, Some(Dialog::Recover)));
        app.recover_autosave();
        assert!(!app.no_doc);
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (40, 30));
        assert_eq!(app.path, Some(PathBuf::from("/work/poster.lumen")));
        assert!(app.any_unsaved(), "recovered work counts as unsaved");
        assert_eq!(app.tab_infos().len(), 1);
        session::remove_autosave();
        assert!(!file.exists());
    }

    #[test]
    fn a_plain_launch_is_the_welcome_screen_and_edits_nothing() {
        let _g = app_lock();
        let mut app = App::launch(&[]);
        assert!(app.no_doc);
        assert!(app.tab_infos().is_empty(), "no tabs on the welcome screen");
        // Commands are refused: the placeholder never gains history, so
        // quitting never asks about unsaved changes.
        app.run(&SetSelection {
            selection: Some(Selection::all()),
        });
        app.run(&AddPixelLayer::new("Stray"));
        assert_eq!(app.editor.history().len(), 0);
        assert_eq!(app.editor.doc().layer_count(), 0);
        assert!(app.editor.doc().selection.is_none());
        assert!(!app.any_unsaved());
    }

    #[test]
    fn closing_the_last_tab_returns_to_the_welcome_screen() {
        let _g = app_lock();
        let mut app = App::launch(&args(&["--demo"]));
        assert!(!app.no_doc);
        assert_eq!(app.tab_infos(), [("Aoraki demo".to_string(), false)]);
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (1800, 1205));
        let active = app.active_layer().map(|l| l.name.clone());
        assert_eq!(active.as_deref(), Some("Lift shadows"));

        app.open_in_new_tab(blank(64, 48), None);
        assert_eq!(app.tab_infos().len(), 2);
        app.run(&AddPixelLayer::new("Sketch"));
        assert!(app.any_unsaved());
        app.force_close_tab(app.cur_tab);
        assert!(!app.no_doc, "one tab left");
        assert_eq!(app.editor.doc().width, 1800);
        app.force_close_tab(app.cur_tab);
        assert!(app.no_doc);
        assert!(app.tabs.is_empty());
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (1, 1));
        assert!(app.active.is_none() && !app.any_unsaved());

        // Anything opened from here becomes the first tab again.
        app.open_in_new_tab(blank(32, 32), None);
        assert!(!app.no_doc);
        assert_eq!(app.tab_infos().len(), 1);
        assert_eq!(app.cur_tab, 0);
    }

    #[test]
    fn placing_with_nothing_open_opens_the_image() {
        let dir = std::env::temp_dir().join("lumenply-start-place");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("dot.png");
        image::RgbaImage::from_pixel(5, 3, image::Rgba([200, 10, 10, 255]))
            .save(&png)
            .unwrap();
        // (Under test, session::data_dir is a temp dir, so the recent list
        // this open writes is not the user's.)
        let _g = app_lock();
        let app = App::launch(&args(&["--place", &png.to_string_lossy()]));
        assert!(!app.no_doc);
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (5, 3));
        assert_eq!(
            app.editor.history().len(),
            0,
            "an opened image is not an undo step"
        );
        assert_eq!(app.tab_infos(), [("dot.png".to_string(), false)]);
    }

    #[test]
    fn a_file_that_fails_to_open_leaves_its_error_on_the_welcome_screen() {
        let missing = std::env::temp_dir()
            .join("lumenply-no-such-dir")
            .join("gone.lumen");
        let _g = app_lock();
        let app = App::launch(&args(&[&missing.to_string_lossy()]));
        assert!(app.no_doc);
        assert!(app.status.starts_with("Could not open"), "{}", app.status);
        assert!(!app.recent.iter().any(|r| r.ends_with("gone.lumen")));
    }

    #[test]
    fn long_folders_lose_their_middle() {
        let abc = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(elide_middle(abc, 26), abc);
        assert_eq!(elide_middle(abc, 40), abc);
        assert_eq!(elide_middle(abc, 10), "abc…uvwxyz");
        assert_eq!(elide_middle(abc, 10).chars().count(), 10);
        assert_eq!(elide_middle(abc, 1), "…");
        assert_eq!(
            elide_middle("~/Pictures/2026/Trips/New Zealand", 20),
            "~/Pict…s/New Zealand"
        );
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(relative_age(0), "just now");
        assert_eq!(relative_age(59), "just now");
        assert_eq!(relative_age(60), "1 min ago");
        assert_eq!(relative_age(3599), "59 min ago");
        assert_eq!(relative_age(3600), "1 h ago");
        assert_eq!(relative_age(86_399), "23 h ago");
        assert_eq!(relative_age(86_400), "yesterday");
        assert_eq!(relative_age(2 * 86_400), "2 days ago");
        assert_eq!(relative_age(29 * 86_400), "29 days ago");
        assert_eq!(relative_age(30 * 86_400), "1 month ago");
        assert_eq!(relative_age(200 * 86_400), "6 months ago");
        assert_eq!(relative_age(365 * 86_400), "1 year ago");
        assert_eq!(relative_age(3 * 365 * 86_400), "3 years ago");
    }

    #[test]
    fn kind_labels_and_folders() {
        assert_eq!(kind_label("/a/summit.lumen"), "LUMEN");
        assert_eq!(kind_label("/a/old.NGE"), "LUMEN");
        assert_eq!(kind_label("/a/poster.psd"), "PSD");
        assert_eq!(kind_label("/a/IMG_1.jpeg"), "JPG");
        assert_eq!(kind_label("/a/scan.TIFF"), "TIF");
        assert_eq!(kind_label("/a/README"), "FILE");
        let home = PathBuf::from("/Users/me");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            display_dir("/Users/me/Pictures/trip/a.jpg", Some(&home)),
            format!("~{sep}Pictures/trip")
        );
        assert_eq!(display_dir("/Users/me/a.jpg", Some(&home)), "~");
        assert_eq!(display_dir("/Volumes/Card/a.jpg", Some(&home)), "/Volumes/Card");
    }
}
