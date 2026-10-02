//! Text tool: what a canvas click does, the Text options bar, the
//! Properties text section and the searchable font picker. Typing on the
//! canvas itself lives in text_edit.rs.

use lumenply_render::text::{font_display_name, font_is_available, BUNDLED_FAMILY};

use super::*;
use crate::text_edit::EditStart;

/// Missing fonts and other warnings: the quick-mask red.
const WARN: Color32 = Color32::from_rgb(0xE8, 0x5D, 0x5D);

/// Height of every control in the Text options bar and its rows.
const ROW_H: f32 = 24.0;

/// What a Text-tool click on the canvas does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextClick {
    /// Edit the text layer whose glyphs (or paragraph box) were clicked,
    /// caret at the click.
    Edit(LayerId),
    /// Start new text at the click.
    New,
}

/// Decide a Text-tool click at document position (x, y). `force_new` is
/// Shift held or the "New text" button armed: a deliberate new layer even
/// over existing text. Otherwise a click on any visible text layer edits
/// it, and a click anywhere else starts new text (which disappears again
/// if nothing is typed).
pub(crate) fn text_click_action(doc: &Document, x: f32, y: f32, force_new: bool) -> TextClick {
    if force_new {
        return TextClick::New;
    }
    match text_layer_at(doc.layers(), x, y) {
        Some(id) => TextClick::Edit(id),
        None => TextClick::New,
    }
}

impl App {
    /// A Text-tool click at document position (x, y): edit the text there
    /// with the caret at the click, or start new point text.
    pub(crate) fn text_click(&mut self, ctx: &egui::Context, x: f32, y: f32, shift: bool) {
        self.commit_text_edit();
        self.tool = Tool::Text;
        let force_new = shift || std::mem::take(&mut self.text_new_armed);
        match text_click_action(self.editor.doc(), x, y, force_new) {
            TextClick::Edit(id) => self.begin_text_edit(ctx, id, EditStart::At(x, y)),
            TextClick::New => self.begin_new_text(ctx, x, y, None),
        }
    }

    /// Copy the active text layer's style into the new-text defaults.
    pub(crate) fn adopt_active_text_style(&mut self) {
        if let Some(t) = self.active_text() {
            self.text_size = t.size;
            self.text_bold = t.bold;
            self.text_italic = t.italic;
            self.text_font = t.font;
            self.text_align = t.align;
        }
    }

    /// After a document opens: name any fonts it uses that this machine
    /// lacks (those layers render in DejaVu Sans until they are installed).
    pub(crate) fn note_missing_fonts(&mut self) {
        let missing = lumenply_render::text::missing_fonts(self.editor.doc().layers());
        if missing.is_empty() {
            return;
        }
        let names: Vec<String> = missing.iter().map(|f| font_display_name(f)).collect();
        let plural = if names.len() == 1 { "" } else { "s" };
        let notice = format!(
            "Missing font{plural}: {} (shown in {BUNDLED_FAMILY})",
            names.join(", ")
        );
        self.status = if self.status.is_empty() || self.status == "Ready" {
            notice
        } else {
            format!("{} · {notice}", self.status)
        };
    }

    /// The Text tool's options bar: edits the active text layer, or sets
    /// up the next new text.
    pub(crate) fn text_options_bar(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let editing = if self.text_new_armed {
            None
        } else {
            self.active.zip(self.active_text())
        };
        let hint_full;
        let hint_short;
        if let Some((id, layer_t)) = editing {
            let before = layer_t.clone();
            let (shown, sel) = self.text_controls_view(id, &layer_t);
            let mut t = shown.clone();
            let mut finished = false;
            // On a narrow window Tracking stays in Properties so the hint
            // still fits.
            let narrow = ui.available_width() < 1100.0;
            finished |= font_picker(ui, "bar", &mut t.font, 170.0);
            finished |= style_toggles(ui, &mut t.bold, &mut t.italic, Some(&mut t.all_caps));
            bar_separator(ui);
            finished |= align_toggles(ui, &mut t.align, t.box_size.is_some());
            bar_separator(ui);
            muted(ui, "Size");
            finished |= edit_finished(&value_field(
                ui,
                "Font size",
                &mut t.size,
                6.0..=400.0,
                " px",
                66.0,
            ));
            // On a narrow window Leading and Tracking stay in Properties.
            if !narrow {
                muted(ui, "Leading");
                finished |= leading_field(ui, &mut t, 58.0);
                muted(ui, "Tracking");
                finished |= edit_finished(&value_field(
                    ui,
                    "Tracking",
                    &mut t.tracking,
                    -200.0..=800.0,
                    "",
                    50.0,
                ));
            }
            finished |= text_color_button(ui, &mut t.color);
            let t = crate::text_edit::route_text_edit(&before, &shown, t, sel);
            self.commit_text(id, &before, t, finished);
            bar_separator(ui);
            self.new_text_button(ui);
            if self.text_editing() {
                let r = chip(ui, false, RichText::new("Cancel"), 0.0)
                    .on_hover_text("Put the text back as it was before this edit");
                if r.clicked() {
                    self.cancel_text_edit();
                }
                let r = chip(ui, true, RichText::new("Commit"), 0.0)
                    .on_hover_text("Finish editing the text (Esc or Cmd+Enter)");
                if r.clicked() {
                    self.commit_text_edit();
                }
            }
            if self.text_editing() {
                hint_full = "Esc or Cmd+Enter commits · Cmd-drag moves the text";
                hint_short = "Esc commits";
            } else {
                hint_full = "Click text to edit it · click elsewhere for new text · drag for a text box";
                hint_short = "Drag: text box";
            }
        } else {
            font_picker(ui, "bar", &mut self.text_font, 170.0);
            style_toggles(ui, &mut self.text_bold, &mut self.text_italic, None);
            bar_separator(ui);
            align_toggles(ui, &mut self.text_align, true);
            bar_separator(ui);
            muted(ui, "Size");
            value_field(ui, "Font size", &mut self.text_size, 6.0..=400.0, " px", 66.0);
            bar_separator(ui);
            self.new_text_button(ui);
            if self.text_new_armed {
                if ui.button("Cancel").clicked() {
                    self.text_new_armed = false;
                }
                hint_full = "Click on the canvas to place the new text, or drag a text box";
                hint_short = "Click to place";
            } else {
                hint_full = "Click on the canvas to add text · drag to draw a text box";
                hint_short = "Click to add text";
            }
        }
        // The hint takes whatever room is left, shortened or dropped
        // rather than clipped.
        let font = egui::TextStyle::Body.resolve(ui.style());
        let room = ui.available_width() - 8.0;
        let fits =
            |s: &str| ui.fonts(|f| f.layout_no_wrap(s.to_string(), font.clone(), MUTED).size().x) <= room;
        let hint = [hint_full, hint_short].into_iter().find(|s| fits(s));
        if let Some(hint) = hint {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(hint).color(MUTED));
            });
        }
    }

    /// "New text": arm the tool so the next click starts a new layer in
    /// the current style, whatever lies under it.
    fn new_text_button(&mut self, ui: &mut egui::Ui) {
        let r = chip(ui, self.text_new_armed, RichText::new("New text"), 0.0).on_hover_text(
            "Start a new text layer: the next canvas click places it (Shift+click does the same)",
        );
        if r.clicked() {
            if self.text_new_armed {
                self.text_new_armed = false;
            } else {
                self.adopt_active_text_style();
                self.text_new_armed = true;
            }
        }
    }

    /// The Properties text section: content, then font, style (with
    /// alignment and colour), size and tracking as labelled rows. Compact,
    /// so it fits the Properties area without scrolling.
    pub(crate) fn text_properties(&mut self, ui: &mut egui::Ui, id: LayerId, layer_t: TextLayer) {
        let mut rasterize = false;
        ui.horizontal(|ui| {
            section_title(ui, "TEXT");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                rasterize = ui
                    .add(egui::Button::new(RichText::new("Rasterize").small().color(MUTED)).frame(false))
                    .on_hover_text("Turn this text into ordinary pixels (no longer editable as text)")
                    .clicked();
            });
        });
        let before = layer_t.clone();
        let (shown, sel) = self.text_controls_view(id, &layer_t);
        let mut t = shown.clone();
        let mut finished = false;
        let r = field_style(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut t.text)
                    .hint_text("Type your text")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            )
        });
        a11y_name(&r, "Text");
        finished |= r.lost_focus();
        row(ui, "Font", |ui| {
            let w = ui.available_width();
            finished |= font_picker(ui, "panel", &mut t.font, w);
        });
        if !font_is_available(&t.font) {
            row(ui, "", |ui| {
                ui.label(
                    RichText::new(format!("Not installed: shown in {BUNDLED_FAMILY}"))
                        .small()
                        .color(WARN),
                );
            });
        }
        row(ui, "Style", |ui| {
            finished |= style_toggles(ui, &mut t.bold, &mut t.italic, Some(&mut t.all_caps));
            ui.add_space(4.0);
            let r = chip(ui, t.kerning, RichText::new("VA").size(11.0), 28.0).on_hover_text(
                "Kerning: the font's own pair spacing and exact advances (Photoshop's Metrics)",
            );
            a11y_name(&r, "Kerning");
            if r.clicked() {
                t.kerning = !t.kerning;
                finished = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                finished |= text_color_button(ui, &mut t.color);
            });
        });
        row(ui, "Align", |ui| {
            finished |= align_toggles(ui, &mut t.align, t.box_size.is_some());
        });
        finished |= value_slider_row(ui, "Size", &mut t.size, 6.0..=400.0, " px", true);
        let mut lead = t.line_height * t.size;
        let lead_max = (t.size * 4.0).max(lead);
        if value_slider_row(ui, "Leading", &mut lead, (t.size * 0.5)..=lead_max, " px", false) {
            finished = true;
        }
        if (lead - t.line_height * t.size).abs() > 1e-3 && t.size > 0.0 {
            t.line_height = (lead / t.size).clamp(0.5, 10.0);
        }
        finished |= value_slider_row(ui, "Tracking", &mut t.tracking, -200.0..=800.0, "", false);
        finished |= value_slider_row(
            ui,
            "Baseline",
            &mut t.baseline_shift,
            -100.0..=100.0,
            " px",
            false,
        );
        let mut convert = None;
        row(ui, "Type", |ui| {
            let paragraph = t.box_size.is_some();
            if chip(ui, !paragraph, RichText::new("Point"), 0.0)
                .on_hover_text("One line per return; the anchor sits on the first baseline")
                .clicked()
                && paragraph
            {
                convert = Some(lumenply_render::text_layout::to_point(&before));
            }
            if chip(ui, paragraph, RichText::new("Paragraph"), 0.0)
                .on_hover_text("Lines wrap inside a box; drag its handles while editing")
                .clicked()
                && !paragraph
            {
                convert = Some(lumenply_render::text_layout::to_paragraph(&before));
            }
        });
        if let Some([w, h]) = t.box_size.as_mut() {
            row(ui, "Box", |ui| {
                let rw = value_field(ui, "Box width", w, 8.0..=20000.0, " px", 70.0);
                ui.label(RichText::new("\u{00D7}").color(MUTED));
                let rh = value_field(ui, "Box height", h, 8.0..=20000.0, " px", 70.0);
                finished |= edit_finished(&rw) || edit_finished(&rh);
            });
        }
        let t = match convert {
            Some(c) => {
                finished = true;
                c
            }
            None => crate::text_edit::route_text_edit(&before, &shown, t, sel),
        };
        self.commit_text(id, &before, t, finished);
        if rasterize {
            self.run(&RasterizeLayer { layer: id });
        }
    }

    /// Apply a frame's edits to a text layer, coalescing a drag or a run
    /// of keystrokes into one undo step.
    fn commit_text(&mut self, id: LayerId, before: &TextLayer, t: TextLayer, finished: bool) {
        if t != *before {
            self.run_coalescing(&SetText { layer: id, text: t }, &format!("text-{id}"));
        }
        if finished {
            self.editor.end_coalescing();
        }
    }

    /// Text-tool canvas overlay: a hairline box around the active text
    /// layer with its anchor marked, and a fainter box around other text
    /// under the cursor (a click there edits it).
    pub(crate) fn paint_text_overlay(&self, painter: &egui::Painter, resp: &egui::Response) {
        if self.paint_text_session(&resp.ctx, painter, resp) {
            return;
        }
        let origin = resp.rect.min + self.pan;
        let to_screen = |x: f32, y: f32| egui::pos2(origin.x + x * self.zoom, origin.y + y * self.zoom);
        let doc = self.editor.doc();
        let glyph_box = |id: LayerId| {
            // Paragraph text: its box, glyphs or not.
            if let Some(t) = doc.layer(id)?.text_layer().filter(|t| t.box_size.is_some()) {
                let [w, h] = t.box_size?;
                return Some(egui::Rect::from_min_max(
                    to_screen(t.x, t.y),
                    to_screen(t.x + w, t.y + h),
                ));
            }
            let b = doc.layer(id)?.raster_store()?.content_bounds()?;
            Some(egui::Rect::from_min_max(
                to_screen(b.x as f32, b.y as f32),
                to_screen(b.right() as f32, b.bottom() as f32),
            ))
        };
        let active = self
            .active
            .filter(|_| !self.text_new_armed)
            .and_then(|id| doc.layer(id))
            .filter(|l| l.visible)
            .and_then(|l| {
                l.text_layer()
                    .filter(|t| t.box_size.is_none())
                    .map(|t| (l.id, t.x, t.y))
            });
        if let Some(p) = resp.hover_pos() {
            let ox = (p.x - origin.x) / self.zoom;
            let oy = (p.y - origin.y) / self.zoom;
            if let Some(hit) = text_layer_at(doc.layers(), ox, oy) {
                if active.map(|a| a.0) != Some(hit) {
                    if let Some(r) = glyph_box(hit) {
                        painter.rect_stroke(r.expand(3.0), 2.0, Stroke::new(1.0, MUTED.gamma_multiply(0.7)));
                    }
                }
            }
        }
        let Some((id, ax, ay)) = active else {
            return;
        };
        if let Some(r) = glyph_box(id) {
            let r = r.expand(3.0);
            painter.rect_stroke(
                r.expand(1.0),
                2.0,
                Stroke::new(1.0, Color32::from_black_alpha(120)),
            );
            painter.rect_stroke(r, 2.0, Stroke::new(1.0, ACCENT));
        }
        // The anchor: where the baseline starts (or centres, or ends).
        let a = to_screen(ax, ay);
        let ink = Stroke::new(1.0, Color32::from_black_alpha(160));
        painter.line_segment([a + egui::vec2(-7.0, 1.0), a + egui::vec2(7.0, 1.0)], ink);
        painter.line_segment(
            [a + egui::vec2(-6.0, 0.0), a + egui::vec2(6.0, 0.0)],
            Stroke::new(1.5, ACCENT),
        );
        painter.rect_filled(egui::Rect::from_center_size(a, egui::vec2(5.0, 5.0)), 1.0, ACCENT);
    }
}

// ---- small widgets ----------------------------------------------------------------------

/// True when a drag released, a typed value was committed, or a click
/// changed the value: the end of one undoable edit.
fn edit_finished(r: &egui::Response) -> bool {
    r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged())
}

fn muted(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(MUTED));
}

fn bar_separator(ui: &mut egui::Ui) {
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(2.0);
}

/// A labelled Properties row: muted 70 px label, then the contents.
fn row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 18.0], egui::Label::new(RichText::new(label).color(MUTED)));
        add(ui)
    })
    .inner
}

/// Field look for text inputs: ground fill, hairline edge, an amber ring
/// while focused and a soft amber selection that keeps text legible.
fn field_style<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.extreme_bg_color = GROUND;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED.gamma_multiply(0.5));
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xFF, 0xB5, 0x47, 70);
        add(ui)
    })
    .inner
}

/// A compact numeric box: drag to scrub, click to type. Mono digits.
fn value_field(
    ui: &mut egui::Ui,
    name: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    width: f32,
) -> egui::Response {
    let speed = (v.abs() * 0.01).max(0.25);
    field_style(ui, |ui| {
        let vis = ui.visuals_mut();
        vis.widgets.inactive.weak_bg_fill = GROUND;
        vis.widgets.hovered.weak_bg_fill = GROUND;
        vis.widgets.active.weak_bg_fill = GROUND;
        ui.style_mut().drag_value_text_style = egui::TextStyle::Monospace;
        let r = ui.add_sized(
            [width, ROW_H],
            egui::DragValue::new(v)
                .range(range)
                .speed(speed)
                .custom_formatter(|n, _| {
                    // Whole numbers stay whole; fractions show one place.
                    if (n - n.round()).abs() < 0.05 {
                        format!("{n:.0}")
                    } else {
                        format!("{n:.1}")
                    }
                })
                .suffix(suffix),
        );
        a11y_name(&r, name);
        r
    })
}

/// A two-state chip: hairline when off, amber edge and tint when on.
fn chip(ui: &mut egui::Ui, on: bool, text: RichText, width: f32) -> egui::Response {
    let (fill, stroke, color) = if on {
        (ACCENT_TINT, ACCENT, ACCENT)
    } else {
        (Color32::TRANSPARENT, LINE, TEXT)
    };
    ui.add(
        egui::Button::new(text.color(color))
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .min_size(egui::vec2(width, ROW_H)),
    )
}

/// Bold and Italic toggles, side by side, plus All caps when given.
/// Returns true on a change.
fn style_toggles(ui: &mut egui::Ui, bold: &mut bool, italic: &mut bool, caps: Option<&mut bool>) -> bool {
    let mut changed = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().button_padding.x = 4.0;
        let b = RichText::new("B").family(egui::FontFamily::Name("semibold".into()));
        let r = chip(ui, *bold, b, 28.0).on_hover_text("Bold");
        a11y_name(&r, "Bold");
        if r.clicked() {
            *bold = !*bold;
            changed = true;
        }
        let r = chip(ui, *italic, RichText::new("I").italics(), 28.0)
            .on_hover_text("Italic (slanted automatically when the font has no italic)");
        a11y_name(&r, "Italic");
        if r.clicked() {
            *italic = !*italic;
            changed = true;
        }
        if let Some(caps) = caps {
            let r = chip(ui, *caps, RichText::new("TT").size(11.0), 28.0)
                .on_hover_text("All caps (the text keeps its own case underneath)");
            a11y_name(&r, "All caps");
            if r.clicked() {
                *caps = !*caps;
                changed = true;
            }
        }
    });
    changed
}

/// Left / centre / right / justify alignment as four icon chips. Justify
/// needs paragraph text; on point text it is shown but disabled.
fn align_toggles(ui: &mut egui::Ui, align: &mut TextAlign, paragraph: bool) -> bool {
    let mut changed = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().button_padding.x = 4.0;
        for (a, tip) in [
            (TextAlign::Left, "Align left"),
            (TextAlign::Center, "Align centre"),
            (TextAlign::Right, "Align right"),
            (TextAlign::Justify, "Justify (last line left)"),
        ] {
            let enabled = paragraph || a != TextAlign::Justify;
            let on = *align == a;
            let r = ui
                .add_enabled_ui(enabled, |ui| chip(ui, on, RichText::new(""), 28.0))
                .inner
                .on_hover_text(tip)
                .on_disabled_hover_text("Justify needs paragraph text: drag a box with the Text tool");
            a11y_name(&r, tip);
            // Three lines of a paragraph, ragged on the free side.
            let c = r.rect.center();
            let color = if on {
                ACCENT
            } else if enabled {
                TEXT
            } else {
                MUTED.gamma_multiply(0.5)
            };
            for (dy, w) in [(-4.0, 12.0), (0.0, 8.0), (4.0, 10.0)] {
                let (x0, x1) = match a {
                    TextAlign::Left => (c.x - 6.0, c.x - 6.0 + w),
                    TextAlign::Center => (c.x - w / 2.0, c.x + w / 2.0),
                    TextAlign::Right => (c.x + 6.0 - w, c.x + 6.0),
                    // Full lines, the last one short.
                    TextAlign::Justify if dy < 4.0 => (c.x - 6.0, c.x + 6.0),
                    TextAlign::Justify => (c.x - 6.0, c.x),
                };
                ui.painter().line_segment(
                    [egui::pos2(x0, c.y + dy), egui::pos2(x1, c.y + dy)],
                    Stroke::new(1.5, color),
                );
            }
            if r.clicked() && !on {
                *align = a;
                changed = true;
            }
        }
    });
    changed
}

/// Leading (line spacing) in pixels, stored as a multiple of the size.
fn leading_field(ui: &mut egui::Ui, t: &mut TextLayer, width: f32) -> bool {
    let mut px = t.line_height * t.size;
    let r = value_field(ui, "Leading", &mut px, 1.0..=2000.0, "", width)
        .on_hover_text("Leading: the distance from one baseline to the next, in pixels");
    if r.changed() && t.size > 0.0 {
        t.line_height = (px / t.size).clamp(0.5, 10.0);
    }
    edit_finished(&r)
}

/// The text colour swatch (stored linear, edited as sRGB).
fn text_color_button(ui: &mut egui::Ui, color: &mut [f32; 4]) -> bool {
    let mut rgb = [
        lumenply_io::linear_to_srgb(color[0]) as f32 / 255.0,
        lumenply_io::linear_to_srgb(color[1]) as f32 / 255.0,
        lumenply_io::linear_to_srgb(color[2]) as f32 / 255.0,
    ];
    let r = crate::color_picker::color_edit_button_rgb(ui, &mut rgb).on_hover_text("Text colour");
    if r.changed() {
        *color = linear_rgba(rgb, color[3]);
    }
    // The shared picker reports drags like a slider, so a drag through
    // the spectrum is one undo step that ends on release.
    r.drag_stopped() || (r.changed() && !r.dragged())
}

/// A Properties slider row like `slider_row`, optionally logarithmic.
fn value_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    log: bool,
) -> bool {
    let mut finished = false;
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 18.0], egui::Label::new(RichText::new(label).color(MUTED)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized(
                [58.0, 18.0],
                egui::Label::new(RichText::new(format!("{v:.0}{suffix}")).monospace().color(TEXT)),
            );
            ui.spacing_mut().slider_width = (ui.available_width() - 10.0).max(60.0);
            let r = ui.add(egui::Slider::new(v, range).logarithmic(log).show_value(false));
            a11y_name(&r, label);
            finished = r.drag_stopped() || (r.changed() && !r.dragged());
        });
    });
    finished
}

// ---- font picker --------------------------------------------------------------------------

fn font_popup_id(salt: &str) -> egui::Id {
    egui::Id::new("font-picker").with(salt)
}

/// Open a picker's popup as if its button were clicked (debug hooks).
pub(crate) fn open_font_picker(ctx: &egui::Context, salt: &str, query: &str) {
    let id = font_popup_id(salt);
    ctx.memory_mut(|m| m.open_popup(id));
    ctx.data_mut(|d| {
        d.insert_temp(id.with("q"), query.to_string());
        d.insert_temp(id.with("fresh"), true);
    });
}

/// Searchable font family picker. The button shows the current family
/// (the empty string is the bundled DejaVu Sans; a family this machine
/// lacks is flagged); the popup filters the installed families as you
/// type. Returns true when the choice changed.
pub(crate) fn font_picker(ui: &mut egui::Ui, salt: &str, font: &mut String, width: f32) -> bool {
    let popup_id = font_popup_id(salt);
    let q_id = popup_id.with("q");
    let fresh_id = popup_id.with("fresh");
    let available = font_is_available(font);

    // The button: a field showing the family, elided, with a chevron.
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, format!("Font {font}")));
    let open = ui.memory(|m| m.is_popup_open(popup_id));
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let edge = if open {
            ACCENT
        } else if resp.hovered() {
            MUTED.gamma_multiply(0.5)
        } else {
            LINE
        };
        p.rect(rect, 6.0, GROUND, Stroke::new(1.0, edge));
        let name = if font.is_empty() {
            format!("{BUNDLED_FAMILY} (bundled)")
        } else {
            font_display_name(font)
        };
        let mut job = egui::text::LayoutJob::default();
        let body = egui::TextStyle::Body.resolve(ui.style());
        job.append(
            &name,
            0.0,
            egui::TextFormat::simple(body.clone(), if available { TEXT } else { WARN }),
        );
        if !available {
            job.append("  missing", 0.0, egui::TextFormat::simple(body, WARN));
        }
        job.wrap = egui::text::TextWrapping::truncate_at_width(width - 34.0);
        let galley = ui.fonts(|f| f.layout_job(job));
        p.galley(
            egui::pos2(rect.left() + 9.0, rect.center().y - galley.size().y / 2.0),
            galley,
            TEXT,
        );
        // Chevron.
        let c = egui::pos2(rect.right() - 14.0, rect.center().y);
        p.line_segment(
            [c + egui::vec2(-4.0, -2.0), c + egui::vec2(0.0, 2.0)],
            Stroke::new(1.5, MUTED),
        );
        p.line_segment(
            [c + egui::vec2(0.0, 2.0), c + egui::vec2(4.0, -2.0)],
            Stroke::new(1.5, MUTED),
        );
    }
    let resp = if available {
        resp.on_hover_text("Font family: click to search the installed fonts")
    } else {
        resp.on_hover_text(format!(
            "\u{201C}{}\u{201D} is not installed on this computer, so this text is shown in \
             {BUNDLED_FAMILY}. Install the font or pick another one.",
            font_display_name(font)
        ))
    };
    if resp.clicked() {
        if !open {
            ui.data_mut(|d| {
                d.insert_temp(q_id, String::new());
                d.insert_temp(fresh_id, true);
            });
        }
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }

    let mut changed = false;
    egui::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(width.max(260.0));
            ui.set_max_width(width.max(260.0));
            let mut q: String = ui.data_mut(|d| d.get_temp(q_id)).unwrap_or_default();
            let fresh = ui.data_mut(|d| d.remove_temp::<bool>(fresh_id)).unwrap_or(false);
            let sr = field_style(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut q)
                        .hint_text("Search fonts")
                        .desired_width(f32::INFINITY),
                )
            });
            a11y_name(&sr, "Search fonts");
            if fresh {
                sr.request_focus();
            }
            ui.data_mut(|d| d.insert_temp(q_id, q.clone()));

            // (value stored in the layer, label shown)
            let families = lumenply_render::text::system_font_families();
            let needle = q.trim().to_lowercase();
            let bundled_label = format!("{BUNDLED_FAMILY} (bundled)");
            let mut entries: Vec<(&str, &str)> = Vec::with_capacity(families.len() + 1);
            if needle.is_empty() || bundled_label.to_lowercase().contains(&needle) {
                entries.push(("", bundled_label.as_str()));
            }
            entries.extend(
                families
                    .iter()
                    .filter(|f| needle.is_empty() || f.to_lowercase().contains(&needle))
                    .map(|f| (f.as_str(), f.as_str())),
            );
            let mut pick: Option<String> = None;
            if sr.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                pick = entries.first().map(|(v, _)| v.to_string());
            }

            ui.add_space(2.0);
            if entries.is_empty() {
                ui.label(
                    RichText::new(format!("No installed font matches \u{201C}{}\u{201D}", q.trim()))
                        .color(MUTED),
                );
            } else {
                let row_h = ui.spacing().interact_size.y;
                let mut area = egui::ScrollArea::vertical()
                    .id_salt(("font-list", salt, &needle))
                    .max_height(300.0)
                    .auto_shrink([false, true]);
                if fresh && needle.is_empty() {
                    // Open with the current family in view.
                    if let Some(i) = entries.iter().position(|(v, _)| *v == font.as_str()) {
                        let spacing = ui.spacing().item_spacing.y;
                        area = area.vertical_scroll_offset((i as f32 * (row_h + spacing) - 120.0).max(0.0));
                    }
                }
                let scroll_out = area.show_rows(ui, row_h, entries.len(), |ui, range| {
                    for (value, label) in &entries[range] {
                        let on = *value == font.as_str();
                        // Full-width rows, names left-aligned.
                        let r = ui
                            .with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                                ui.add(egui::SelectableLabel::new(on, *label))
                            })
                            .inner;
                        if r.clicked() {
                            pick = Some(value.to_string());
                        }
                    }
                });
                a11y_scroll(ui.ctx(), &scroll_out, "Fonts");
            }
            ui.add_space(2.0);
            let total = families.len() + 1;
            let note = if needle.is_empty() {
                format!("{total} fonts")
            } else {
                format!("{} of {total} fonts", entries.len())
            };
            ui.label(RichText::new(note).small().color(MUTED));

            if let Some(v) = pick {
                if v != *font {
                    *font = v;
                    changed = true;
                }
                ui.memory_mut(|m| m.close_popup());
            }
        },
    );
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    /// A 600×400 document: a pixel background, text "Hello" (40 px, at
    /// 100, 200) and a hidden text layer "Ghost" at (100, 320).
    fn doc() -> (Document, LayerId, LayerId, LayerId) {
        let mut ed = Editor::new(Document::new(600, 400));
        ed.execute(&AddPixelLayer::new("Background")).unwrap();
        let bg = ed.doc().layers()[0].id;
        let hello = ed.doc().next_id();
        ed.execute(&AddTextLayer {
            text: TextLayer::new("Hello", 100.0, 200.0, 40.0, BLACK),
            above: Some(bg),
        })
        .unwrap();
        let ghost = ed.doc().next_id();
        ed.execute(&AddTextLayer {
            text: TextLayer::new("Ghost", 100.0, 320.0, 40.0, BLACK),
            above: Some(hello),
        })
        .unwrap();
        ed.execute(&SetVisible {
            layer: ghost,
            visible: false,
        })
        .unwrap();
        (ed.doc().clone(), bg, hello, ghost)
    }

    #[test]
    fn a_click_on_glyphs_edits_that_text() {
        let (d, _, hello, _) = doc();
        // "Hello" at 40 px spans roughly x 100..200, y 171..200.
        assert_eq!(text_click_action(&d, 120.0, 190.0, false), TextClick::Edit(hello));
        assert_eq!(text_click_action(&d, 150.0, 185.0, false), TextClick::Edit(hello));
        // The 4 px grab margin around the glyphs still counts.
        let b = d
            .layer(hello)
            .unwrap()
            .raster_store()
            .unwrap()
            .content_bounds()
            .unwrap();
        let left = b.x as f32 - 3.0;
        assert_eq!(text_click_action(&d, left, 190.0, false), TextClick::Edit(hello));
        assert_eq!(
            text_click_action(&d, b.x as f32 - 6.0, 190.0, false),
            TextClick::New
        );
    }

    #[test]
    fn an_empty_click_starts_new_text_and_hidden_text_is_ignored() {
        let (d, _, _, _) = doc();
        // Empty canvas: new text (it vanishes again if nothing is typed).
        assert_eq!(text_click_action(&d, 450.0, 80.0, false), TextClick::New);
        // A hidden text layer does not catch clicks on its glyphs.
        assert_eq!(text_click_action(&d, 120.0, 310.0, false), TextClick::New);
    }

    #[test]
    fn a_click_inside_a_paragraph_box_edits_it_even_off_the_glyphs() {
        let (d, _, _, _) = doc();
        let mut ed = Editor::new(d);
        let id = ed.doc().next_id();
        ed.execute(&AddTextLayer {
            text: TextLayer {
                box_size: Some([200.0, 100.0]),
                ..TextLayer::new("Hi", 300.0, 20.0, 20.0, BLACK)
            },
            above: None,
        })
        .unwrap();
        // The glyphs sit in the top-left corner; the box's far corner counts.
        assert_eq!(
            text_click_action(ed.doc(), 490.0, 110.0, false),
            TextClick::Edit(id)
        );
        assert_eq!(text_click_action(ed.doc(), 510.0, 110.0, false), TextClick::New);
    }

    #[test]
    fn shift_or_new_text_always_starts_a_new_layer() {
        let (d, _, _, _) = doc();
        assert_eq!(text_click_action(&d, 450.0, 80.0, true), TextClick::New);
        assert_eq!(text_click_action(&d, 120.0, 190.0, true), TextClick::New);
    }

    #[test]
    fn moving_text_is_one_undo_step_that_keeps_the_layer_count() {
        let (d, _, hello, _) = doc();
        let mut ed = Editor::new(d);
        let layers = ed.doc().layer_count();
        let steps = ed.history().len();
        let mut t = ed.doc().layer(hello).unwrap().text_layer().unwrap().clone();
        t.x = 450.0;
        t.y = 80.0;
        ed.execute(&SetText {
            layer: hello,
            text: t,
        })
        .unwrap();
        assert_eq!(ed.doc().layer_count(), layers);
        assert_eq!(ed.history().len(), steps + 1);
        let moved = ed.doc().layer(hello).unwrap().text_layer().unwrap();
        assert_eq!((moved.x, moved.y), (450.0, 80.0));
        let b = ed
            .doc()
            .layer(hello)
            .unwrap()
            .raster_store()
            .unwrap()
            .content_bounds()
            .unwrap();
        assert!(
            (b.x - 450).abs() <= 3 && (b.bottom() - 80).abs() <= 3,
            "glyphs follow: {b:?}"
        );
        // The old spot is empty now, the new one edits the text.
        assert_eq!(text_click_action(ed.doc(), 120.0, 190.0, false), TextClick::New);
        assert_eq!(
            text_click_action(ed.doc(), 470.0, 70.0, false),
            TextClick::Edit(hello)
        );
        assert!(ed.undo().is_some());
        let back = ed.doc().layer(hello).unwrap().text_layer().unwrap();
        assert_eq!((back.x, back.y), (100.0, 200.0));
    }
}
