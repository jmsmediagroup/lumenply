use super::*;

/// How long a status message stays up while nothing else happens.
const MESSAGE_SECS: f64 = 10.0;

/// The live document's version: tab, history position and edit graph.
/// Any edit, undo, redo, history jump or tab switch changes it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct DocVersion {
    tab: u64,
    steps: usize,
    redo: usize,
    graph: usize,
}

/// Which message `self.status` holds: its buffer, length and words.
/// Assigning a new `String` allocates while the old one still lives, so a
/// newly set message is told apart from the last one even when it repeats
/// its words (two undos of two "Paint stroke" steps).
fn message_id(s: &str) -> (usize, usize, u64) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    (s.as_ptr() as usize, s.len(), h.finish())
}

/// When the message on show was first seen: (message, the document
/// version it describes, the time, whether it was seen mid-frame and the
/// version must be read again once that frame's edits are done).
type Seen = ((usize, usize, u64), DocVersion, f64, bool);

fn seen_id() -> egui::Id {
    egui::Id::new("status-message")
}

impl App {
    fn doc_version(&self) -> DocVersion {
        DocVersion {
            tab: self.doc_key,
            steps: self.editor.history().len(),
            redo: self.editor.redo_history().len(),
            graph: self.editor.graph() as *const _ as usize,
        }
    }

    /// Start of a frame: a message first seen during the last frame
    /// describes that frame's outcome, so it takes the document version
    /// the frame ended with (it may have been set before the frame's edit).
    pub(crate) fn settle_status(&self, ctx: &egui::Context) {
        let doc = self.doc_version();
        ctx.data_mut(|d| {
            if let Some(seen) = d.get_temp_mut_or_default::<Option<Seen>>(seen_id()) {
                if seen.3 {
                    seen.1 = doc;
                    seen.3 = false;
                }
            }
        });
    }

    /// The status message while it is news, the one rule for every
    /// message in the app: it shows until the document moves on (an
    /// edit, undo, redo, history jump or tab switch) or for
    /// [`MESSAGE_SECS`], whichever comes first; a message set anew starts
    /// over, even with the same words. Code sets a message by assigning
    /// `self.status`; nothing else is needed.
    pub(crate) fn status_message(&self, ctx: &egui::Context) -> String {
        let now = ctx.input(|i| i.time);
        let doc = self.doc_version();
        let msg = message_id(&self.status);
        let seen: Option<Seen> = ctx.data(|d| d.get_temp(seen_id())).flatten();
        let (shown_at, since) = match seen {
            Some((m, d, t, _)) if m == msg => (d, t),
            _ => {
                let fresh: Option<Seen> = Some((msg, doc, now, true));
                ctx.data_mut(|d| d.insert_temp(seen_id(), fresh));
                (doc, now)
            }
        };
        let left = MESSAGE_SECS - (now - since);
        if shown_at != doc || left <= 0.0 {
            return String::new();
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        self.status.clone()
    }

    /// The status bar: document facts on the left (numbers in mono), the
    /// last message on the right, elided rather than run under the facts.
    pub(crate) fn status_bar(&mut self, ctx: &egui::Context) {
        // Pixel-tight, rescanned only when the document changed (the
        // sparse tiles alone would round it up to whole 256 px tiles).
        let sel_rect = self.info_selection(ctx);
        let message = self.status_message(ctx);
        egui::TopBottomPanel::bottom("status")
            .exact_height(28.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let doc = self.editor.doc();
                    let mono = |s: String| RichText::new(s).monospace().color(TEXT);
                    ui.label(mono(format!("{} × {} px", doc.width, doc.height)))
                        .on_hover_text("Canvas size");
                    // Print resolution, the print size on hover.
                    let ppi = crate::image_size_ui::fmt_ppi(doc.resolution);
                    ui.label(RichText::new(format!("{ppi} ppi")).color(MUTED))
                        .on_hover_text(format!(
                            "Prints at {} ({})",
                            crate::image_size_ui::print_size_text(doc.width, doc.height, doc.resolution),
                            crate::image_size_ui::print_size_cm(doc.width, doc.height, doc.resolution)
                        ));
                    ui.separator();
                    let at = match self.cursor_doc {
                        Some((x, y)) => format!("x {x:<5} y {y:<5}"),
                        None => "x –     y –    ".to_string(),
                    };
                    ui.label(mono(at)).on_hover_text("Pointer position on the canvas");
                    // Photoshop's Info readout: the composite's colour under
                    // the pointer, as 8-bit sRGB.
                    let under = self
                        .cursor_doc
                        .zip(self.last_flat.as_ref())
                        .and_then(|((x, y), f)| {
                            crate::color_picker::sample_srgb(f, x as f32 + 0.5, y as f32 + 0.5)
                        });
                    if let Some(c) = under {
                        let [r, g, b] = c.map(|v| (v * 255.0).round() as u8);
                        let (sw, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                        ui.painter().rect_filled(sw, 2.0, Color32::from_rgb(r, g, b));
                        ui.painter().rect_stroke(sw, 2.0, Stroke::new(1.0, LINE));
                        ui.label(mono(format!("R {r:<3} G {g:<3} B {b:<3}")))
                            .on_hover_text(format!("Colour under the pointer: #{r:02X}{g:02X}{b:02X}"));
                    }
                    ui.separator();
                    match doc.selection.as_ref().map(|_| sel_rect.unwrap_or_default()) {
                        Some(b) => {
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
                    if self.proof_colors || self.gamut_warning {
                        ui.separator();
                        ui.label(RichText::new(crate::soft_proof::PROOF_NOTE).color(ACCENT))
                            .on_hover_text("View ▸ Proof colors: the canvas shows how the image prints");
                    }
                    if self.editing_mask {
                        ui.separator();
                        ui.label(RichText::new("Editing mask").color(ACCENT))
                            .on_hover_text("Painting edits the layer mask: white reveals, black hides");
                    }
                    if self.prefs.show_render_cache {
                        ui.separator();
                        ui.label(mono(render_cache_label(&self.editor)))
                            .on_hover_text(format!(
                                "Rendered tiles kept for reuse (Preferences ▸ Render cache: {} MB)",
                                self.prefs.render_cache_mb
                            ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(message).color(MUTED)).truncate());
                    });
                });
            });
    }
}

/// The render cache's memory use, as the status bar shows it: "Cache 312
/// MB" (tiles and whole-layer results together).
pub(crate) fn render_cache_label(editor: &Editor) -> String {
    let mb = crate::session::render_cache_bytes(editor) as f64 / (1u64 << 20) as f64;
    format!("Cache {mb:.0} MB")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame at virtual time `t`; what the status bar shows.
    fn frame(app: &mut App, ctx: &egui::Context, t: f64) -> String {
        let raw = egui::RawInput {
            time: Some(t),
            ..Default::default()
        };
        let mut shown = String::new();
        let _ = ctx.run(raw, |ctx| {
            app.frame(ctx);
            shown = app.status_message(ctx);
        });
        shown
    }

    #[test]
    fn a_message_shows_until_the_document_moves_on() {
        let mut app = App::launch(&["--demo".to_string()]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        frame(&mut app, &ctx, 0.0);
        assert!(frame(&mut app, &ctx, 0.1).starts_with("Opened the demo"));

        // The next edit leaves the opening message out of date.
        app.add_pixel_layer();
        assert_eq!(frame(&mut app, &ctx, 0.2), "");

        // Undo says what it undid; adding a layer after that clears it.
        app.undo();
        let undid = frame(&mut app, &ctx, 0.3);
        assert!(undid.starts_with("Undid Add layer"), "{undid}");
        assert_eq!(frame(&mut app, &ctx, 0.4), undid);
        app.add_pixel_layer();
        assert_eq!(frame(&mut app, &ctx, 0.5), "");

        // A message set together with its edit describes that edit.
        app.add_pixel_layer();
        app.status = "Added a layer".into();
        frame(&mut app, &ctx, 0.6);
        assert_eq!(frame(&mut app, &ctx, 0.7), "Added a layer");

        // The same words again after another edit are a new message.
        app.add_pixel_layer();
        app.status = "Added a layer".into();
        frame(&mut app, &ctx, 0.8);
        assert_eq!(frame(&mut app, &ctx, 0.9), "Added a layer");

        // Two undos of steps with the same label: the second one's
        // "Undid Add layer …" shows too.
        app.undo();
        let first = frame(&mut app, &ctx, 1.0);
        app.undo();
        let second = frame(&mut app, &ctx, 1.1);
        assert!(first.starts_with("Undid Add layer"), "{first}");
        assert!(second.starts_with("Undid Add layer"), "{second}");

        // A view change is not an edit: the message stays...
        app.view_cmd = Some(ViewCmd::ZoomIn);
        frame(&mut app, &ctx, 1.2);
        assert_eq!(frame(&mut app, &ctx, 1.3), second);
        // ...for ten seconds.
        assert_eq!(frame(&mut app, &ctx, 11.0), second);
        assert_eq!(frame(&mut app, &ctx, 11.2), "");
    }
}
