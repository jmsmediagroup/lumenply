//! Menu actions for everyday Photoshop edits: Layer via Cut, Reselect,
//! Edit ▸ Stroke, Bring to Front / Send to Back, Image ▸ Duplicate and
//! loading a layer's pixels as a selection.

use super::*;
use crate::dialogs::note;
use lumenply_core::everyday::{LayerViaCut, Reselect, SelectLayerPixels, StrokeLocation, StrokeSelection};

impl App {
    /// Why an everyday action can't run (`Some(None)` = it can), or
    /// `None` when `id` isn't one of them.
    pub(crate) fn everyday_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        let doc = self.editor.doc();
        let selection = doc.selection.is_some();
        Some(match id {
            "layer-via-cut" if !self.active_is_pixel() => Some("Select a pixel layer first"),
            "layer-via-cut" => None,
            "reselect" if doc.last_selection.is_none() => Some("Nothing has been deselected yet"),
            "reselect" => None,
            "stroke-selection" if !selection => Some("Make a selection first"),
            "stroke-selection" if !self.active_is_pixel() => Some("Select a pixel layer first"),
            "stroke-selection" => None,
            "select-layer-pixels" if self.active_layer().and_then(|l| l.raster_store()).is_none() => {
                Some("Select a layer with pixels first")
            }
            "select-layer-pixels" => None,
            "layer-front" | "layer-back" if self.active_layer().is_none() => Some("Select a layer first"),
            "layer-front" | "layer-back" | "duplicate-doc" => None,
            "new-snapshot" => None,
            "fill-bg" if !self.active_is_pixel() => Some("Select a pixel layer first"),
            "fill-bg" => None,
            _ => return None,
        })
    }

    /// Run an everyday action; false when `id` isn't one of them.
    pub(crate) fn run_everyday_action(&mut self, id: &str) -> bool {
        match id {
            "layer-via-cut" => {
                if let Some(layer) = self.active {
                    let next = self.editor.doc().next_id();
                    self.run(&LayerViaCut {
                        layer,
                        name: "Layer via cut".into(),
                    });
                    if self.editor.doc().layer(next).is_some() {
                        self.set_active(Some(next));
                    }
                }
            }
            "reselect" => self.run(&Reselect),
            "new-snapshot" => self.new_snapshot(None),
            "fill-bg" => {
                if let Some(layer) = self.active {
                    let color = linear_rgba(self.bg_rgb, 1.0);
                    self.run(&lumenply_core::commands::Fill { layer, color });
                }
            }
            "stroke-selection" => self.dialog = Some(Dialog::Stroke(3.0, StrokeLocation::Center, 100.0)),
            "select-layer-pixels" => {
                if let Some(layer) = self.active {
                    self.load_layer_pixels(layer, CombineOp::Replace);
                }
            }
            "layer-front" | "layer-back" => self.layer_to_end(id == "layer-front"),
            "duplicate-doc" => {
                let doc = self.editor.doc().clone();
                let title = self
                    .path
                    .as_ref()
                    .map(|p| file_name(&p.to_string_lossy()))
                    .unwrap_or_else(|| self.untitled.clone());
                let name = format!("{title} copy");
                self.open_in_new_tab(Editor::new(doc), None);
                self.untitled = name;
                // A duplicate is new work, never saved anywhere yet.
                self.saved_rev = usize::MAX;
                self.status = "Duplicated the document".into();
            }
            _ => return false,
        }
        true
    }

    /// Fixed (not rebindable) keys of the everyday actions.
    pub(crate) fn everyday_action_keys(&self, ctx: &egui::Context, id: &str) -> Option<String> {
        use egui::Modifiers as M;
        let both = M::COMMAND | M::SHIFT;
        let (m, k) = match id {
            "layer-front" => (both, Key::CloseBracket),
            "fill-bg" => (M::COMMAND, Key::Backspace),
            "layer-back" => (both, Key::OpenBracket),
            _ => return None,
        };
        Some(shortcut_text(ctx, m, k))
    }

    /// Arrow keys nudge by a pixel (Shift: ten): the active layer with the
    /// Move tool, the selection outline with a selection tool. A burst of
    /// nudges is one undo step.
    pub(crate) fn nudge_keys(&mut self, ctx: &egui::Context) {
        use egui::Modifiers as M;
        // An open text session uses the arrows for its caret.
        if self.text_editing() {
            return;
        }
        let (mut dx, mut dy) = (0, 0);
        ctx.input_mut(|i| {
            for (key, (x, y)) in [
                (Key::ArrowLeft, (-1, 0)),
                (Key::ArrowRight, (1, 0)),
                (Key::ArrowUp, (0, -1)),
                (Key::ArrowDown, (0, 1)),
            ] {
                // Shift first: `consume_key` ignores an extra Shift, so a
                // plain-arrow match would eat Shift+arrow as a 1 px nudge.
                for (m, step) in [(M::SHIFT, 10), (M::NONE, 1)] {
                    while i.consume_key(m, key) {
                        dx += x * step;
                        dy += y * step;
                    }
                }
            }
        });
        if (dx, dy) != (0, 0) {
            self.nudge(dx, dy);
        }
    }

    /// Move the active layer (Move tool) or the selection outline
    /// (selection tools) by whole pixels.
    pub(crate) fn nudge(&mut self, dx: i32, dy: i32) {
        let selecting = matches!(
            self.tool,
            Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::PolyLasso | Tool::Wand
        );
        if self.tool == Tool::Move {
            let Some(layer) = self.active else { return };
            if let Some(why) = self.lock_block(layer_actions::LockNeed::Move) {
                self.status = why.into();
                return;
            }
            self.run_coalescing(&MoveLayer { layer, dx, dy }, &format!("nudge-{layer}"));
        } else if selecting {
            let Some(sel) = self.editor.doc().selection.as_ref() else {
                return;
            };
            let m = sel.to_mask();
            if m.default > 0.0 {
                return; // everything is selected: nothing to move
            }
            let moved = Selection::from_mask(&lumenply_doc::Mask {
                tiles: m.tiles.translated(dx, dy),
                ..m
            });
            self.run_coalescing(
                &SetSelection {
                    selection: Some(moved),
                },
                "nudge-selection",
            );
        }
    }

    /// Cmd-click on a layer thumbnail: its pixels as the selection (Shift
    /// adds, Alt subtracts, both intersect).
    pub(crate) fn load_layer_pixels(&mut self, layer: LayerId, op: CombineOp) {
        self.run(&SelectLayerPixels { layer, op });
    }

    /// Alt-click on an eye: show only this layer; Alt-clicking it again
    /// brings back what was visible before.
    pub(crate) fn solo_layer(&mut self, layer: LayerId) {
        use lumenply_core::everyday::SetVisibilities;
        let doc = self.editor.doc();
        let soloed = SetVisibilities::solo(doc, layer);
        if let Some((key, id, before)) = self.solo.take() {
            if key == self.doc_key && id == layer && soloed.matches(doc) {
                self.run(&before);
                self.status = "Showing the other layers again".into();
                return;
            }
        }
        self.solo = Some((self.doc_key, layer, SetVisibilities::snapshot(doc)));
        self.run(&soloed);
        self.status = "Showing this layer only (Alt-click its eye again to undo)".into();
    }

    /// Bring to Front / Send to Back among the layer's siblings.
    fn layer_to_end(&mut self, front: bool) {
        let Some(layer) = self.active else { return };
        let doc = self.editor.doc();
        fn find(list: &[Layer], id: LayerId, parent: Option<LayerId>) -> Option<(Option<LayerId>, usize)> {
            if list.iter().any(|l| l.id == id) {
                return Some((parent, list.len()));
            }
            list.iter()
                .filter_map(|l| l.children().map(|c| (l.id, c)))
                .find_map(|(gid, c)| find(c, id, Some(gid)))
        }
        let Some((parent, n)) = find(doc.layers(), layer, None) else {
            return;
        };
        let index = if front { n - 1 } else { 0 };
        self.run(&RelocateLayer { layer, parent, index });
    }

    /// The Stroke dialog's body: width, location and opacity; the colour is
    /// the foreground colour.
    pub(crate) fn stroke_dialog_ui(
        ui: &mut egui::Ui,
        width: &mut f32,
        location: &mut StrokeLocation,
        opacity: &mut f32,
    ) {
        let r = field_row(
            ui,
            "Width",
            egui::DragValue::new(width)
                .range(1.0..=250.0)
                .max_decimals(1)
                .suffix(" px"),
        );
        a11y_name(&r, "Stroke width");
        ui.horizontal(|ui| {
            row_label(ui, "Location", LABEL_W);
            segmented(
                ui,
                location,
                &[
                    (StrokeLocation::Inside, "Inside"),
                    (StrokeLocation::Center, "Center"),
                    (StrokeLocation::Outside, "Outside"),
                ],
            );
        });
        let r = field_row(
            ui,
            "Opacity",
            egui::DragValue::new(opacity)
                .range(1.0..=100.0)
                .max_decimals(0)
                .suffix("%"),
        );
        a11y_name(&r, "Stroke opacity");
        note(
            ui,
            "Strokes the selection's edge in the foreground colour, on the active layer.",
        );
    }

    /// Edit ▸ Stroke, confirmed.
    pub(crate) fn stroke_selection(&mut self, width: f32, location: StrokeLocation, opacity: f32) {
        let Some(layer) = self.active else { return };
        let [r, g, b, _] = linear_rgba(self.brush_rgb, 1.0);
        self.run(&StrokeSelection {
            layer,
            width,
            color: [r, g, b],
            opacity: opacity / 100.0,
            location,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::{Rect, Rgba};

    fn app_with_square() -> (App, LayerId) {
        let mut doc = Document::new(40, 40);
        doc.add_pixel_layer("below");
        let id = doc.add_pixel_layer("photo");
        for y in 10..30 {
            for x in 10..30 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        (app, id)
    }

    #[test]
    fn everyday_actions_run_from_the_registry() {
        let (mut app, id) = app_with_square();
        // Layer via cut makes the new layer active.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 10, 10, 20))),
        });
        assert_eq!(app.action_block("layer-via-cut"), None);
        app.run_menu_action("layer-via-cut");
        assert_ne!(app.active, Some(id));
        assert_eq!(app.editor.doc().layers().len(), 3);
        // Deselect, then Reselect brings the rectangle back.
        app.run_menu_action("deselect");
        assert!(app.editor.doc().selection.is_none());
        app.run_menu_action("reselect");
        assert_eq!(app.editor.doc().selection.as_ref().unwrap().value(12, 12), 1.0);
        // Stroke through the dialog, in the foreground colour.
        app.set_active(Some(id));
        app.brush_rgb = [0.0, 0.0, 1.0];
        app.run_menu_action("stroke-selection");
        assert!(matches!(app.dialog, Some(Dialog::Stroke(..))));
        app.dialog = None;
        app.stroke_selection(2.0, StrokeLocation::Outside, 100.0);
        let p = app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(9, 15);
        assert!(p.b > 0.99 && p.a > 0.99 && p.r < 0.01, "{p:?}");
        // Send to back, then bring to front.
        app.run_menu_action("layer-back");
        assert_eq!(app.editor.doc().layers()[0].id, id);
        app.run_menu_action("layer-front");
        assert_eq!(app.editor.doc().layers().last().unwrap().id, id);
        // Load the layer's pixels as the selection.
        app.run_menu_action("deselect");
        app.run_menu_action("select-layer-pixels");
        let s = app.editor.doc().selection.clone().unwrap();
        assert_eq!((s.value(25, 25), s.value(5, 5)), (1.0, 0.0));
        // Cmd+Backspace's action: fill with the background colour.
        app.bg_rgb = [1.0, 1.0, 0.0];
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 4, 4))),
        });
        app.run_menu_action("fill-bg");
        let p = app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(1, 1);
        assert!(p.r > 0.99 && p.g > 0.99 && p.b < 0.01 && p.a == 1.0, "{p:?}");
        // Alt-click on an eye solos the layer; again restores the rest.
        let others: Vec<LayerId> = app
            .editor
            .doc()
            .layers()
            .iter()
            .map(|l| l.id)
            .filter(|i| *i != id)
            .collect();
        app.run(&lumenply_core::commands::SetVisible {
            layer: others[0],
            visible: false,
        });
        app.solo_layer(id);
        assert!(app
            .editor
            .doc()
            .layers()
            .iter()
            .all(|l| l.visible == (l.id == id)));
        app.solo_layer(id);
        assert!(
            !app.editor.doc().layer(others[0]).unwrap().visible,
            "the earlier hidden one stays hidden"
        );
        assert!(app.editor.doc().layer(others[1]).unwrap().visible);
        // Export ▸ PDF writes a one-page PDF (opaque: JPEG inside).
        let pdf = std::env::temp_dir().join(format!("lumenply-test-{}.pdf", std::process::id()));
        app.export_pdf(&pdf.to_string_lossy());
        let bytes = std::fs::read(&pdf).unwrap();
        let _ = std::fs::remove_file(&pdf);
        assert!(bytes.starts_with(b"%PDF-1.4"), "{}", app.status);
        // The Layers filter keeps matches and their groups.
        app.layer_filter = "PHOTO".into();
        let names: Vec<String> = app.layer_rows().iter().map(|r| r.name().to_string()).collect();
        assert_eq!(names, ["photo"]);
        app.layer_filter.clear();
        assert_eq!(app.layer_rows().len(), 3);
        // A snapshot brings the document back in one step.
        app.run_menu_action("new-snapshot");
        let before = app.editor.doc().layers().len();
        app.run_menu_action("new-layer");
        assert_eq!(app.editor.doc().layers().len(), before + 1);
        let (_, name, kept) = app.snapshots.last().cloned().unwrap();
        app.run(&lumenply_core::everyday::RestoreSnapshot { doc: kept, name });
        assert_eq!(app.editor.doc().layers().len(), before);
        // Duplicate the document into a new, unsaved tab.
        let tabs = app.tab_infos().len();
        app.run_menu_action("duplicate-doc");
        assert_eq!(app.tab_infos().len(), tabs + 1);
        assert_eq!(app.editor.doc().layers().len(), 3);
        assert!(app.any_unsaved());
    }
}

#[cfg(test)]
mod nudge_tests {
    use super::*;
    use lumenply_tiles::{Rect, Rgba};

    #[test]
    fn arrow_nudges_move_the_layer_or_the_selection_in_one_step() {
        let mut doc = Document::new(40, 40);
        let id = doc.add_pixel_layer("dot");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(5, 5, Rgba::new(1.0, 0.0, 0.0, 1.0));
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        app.tool = Tool::Move;
        let steps = app.editor.history().len();
        app.nudge(1, 0);
        app.nudge(10, 0);
        app.nudge(0, -1);
        let px = app.editor.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.get_pixel(16, 4).a, 1.0);
        assert_eq!(px.get_pixel(5, 5).a, 0.0);
        assert_eq!(app.editor.history().len(), steps + 1, "one undo step");
        // Keys: Shift+arrow is ten pixels, a plain arrow one.
        let ctx = egui::Context::default();
        let key = |key: egui::Key, shift: bool| {
            let modifiers = if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            };
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                modifiers,
                ..Default::default()
            }
        };
        let _ = ctx.run(key(egui::Key::ArrowDown, true), |ctx| app.nudge_keys(ctx));
        let _ = ctx.run(key(egui::Key::ArrowLeft, false), |ctx| app.nudge_keys(ctx));
        let px = app.editor.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.get_pixel(15, 14).a, 1.0, "Shift+Down 10 px, Left 1 px");
        assert_eq!(px.get_pixel(16, 4).a, 0.0);
        // A selection tool nudges the outline instead.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(2, 2, 4, 4))),
        });
        app.tool = Tool::RectSelect;
        app.nudge(3, 1);
        let s = app.editor.doc().selection.clone().unwrap();
        assert_eq!((s.value(5, 3), s.value(4, 2), s.value(8, 6)), (1.0, 0.0, 1.0));
        assert_eq!(
            app.editor
                .doc()
                .layer(id)
                .unwrap()
                .pixels()
                .unwrap()
                .get_pixel(15, 14)
                .a,
            1.0
        );
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use lumenply_tiles::Rgba;

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>, alt: bool) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            modifiers: if alt {
                egui::Modifiers::ALT
            } else {
                egui::Modifiers::NONE
            },
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    #[test]
    fn alt_click_with_the_brush_picks_the_colour_and_paints_nothing() {
        let mut doc = Document::new(40, 40);
        let id = doc.add_pixel_layer("red");
        for y in 0..40 {
            for x in 0..40 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        app.tool = Tool::Brush;
        app.brush_rgb = [0.0, 0.0, 1.0];
        let ctx = crate::a11y_tests::ctx();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], false);
        }
        let p = egui::pos2(620.0, 450.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(p)], true);
        assert!(app.cursor_doc.is_some(), "the point is over the document");
        let steps = app.editor.history().len();
        for pressed in [true, false] {
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::ALT,
                }],
                true,
            );
        }
        assert_eq!(app.brush_rgb, [1.0, 0.0, 0.0], "{}", app.status);
        assert_eq!(app.editor.history().len(), steps, "no stroke");
    }

    fn click(app: &mut App, ctx: &egui::Context, p: Pos2, m: egui::Modifiers) {
        let raw = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            modifiers: m,
            ..Default::default()
        };
        let _ = ctx.run(raw(vec![egui::Event::PointerMoved(p)]), |ctx| app.frame(ctx));
        for pressed in [true, false] {
            let ev = egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: m,
            };
            let _ = ctx.run(raw(vec![ev]), |ctx| app.frame(ctx));
        }
    }

    #[test]
    fn the_zoom_mode_zooms_in_at_the_pointer_and_out_with_alt() {
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(Document::new(400, 300)), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.tool = Tool::Hand;
        app.hand_zoom = true;
        let ctx = crate::a11y_tests::ctx();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], false);
        }
        let p = egui::pos2(600.0, 420.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(p)], false);
        let (z0, under) = (app.zoom, app.cursor_doc.unwrap());
        click(&mut app, &ctx, p, egui::Modifiers::NONE);
        assert!((app.zoom - 2.0 * z0).abs() < 1e-4, "{} vs {}", app.zoom, z0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(p)], false);
        assert_eq!(
            app.cursor_doc.unwrap(),
            under,
            "the point under the pointer stays put"
        );
        click(&mut app, &ctx, p, egui::Modifiers::ALT);
        assert!((app.zoom - z0).abs() < 1e-4);
    }

    #[test]
    fn shift_dragging_a_transform_corner_scales_the_axes_freely() {
        let mut doc = Document::new(200, 120);
        let id = doc.add_pixel_layer("box");
        for y in 40..80 {
            for x in 60..140 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(0.0, 1.0, 0.0, 1.0),
                );
            }
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        let ctx = crate::a11y_tests::ctx();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], false);
        }
        app.begin_free_transform();
        frame(&mut app, &ctx, vec![], false);
        // Calibrate document → screen with a probe.
        let probe = egui::pos2(620.0, 450.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(probe)], false);
        let (qx, qy) = app.cursor_doc.unwrap();
        let z = app.zoom;
        let at = |x: f32, y: f32| {
            egui::pos2(
                probe.x + (x - (qx as f32 + 0.5)) * z,
                probe.y + (y - (qy as f32 + 0.5)) * z,
            )
        };
        // Drag the bottom-right corner (140, 80) right only, with Shift.
        let (from, to) = (at(140.0, 80.0), at(180.0, 80.0));
        let shift = egui::Modifiers::SHIFT;
        let raw = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            modifiers: shift,
            ..Default::default()
        };
        let press = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: shift,
        };
        let _ = ctx.run(raw(vec![egui::Event::PointerMoved(from)]), |c| app.frame(c));
        let _ = ctx.run(raw(vec![press(true, from)]), |c| app.frame(c));
        for k in 1..=6 {
            let t = k as f32 / 6.0;
            let _ = ctx.run(
                raw(vec![egui::Event::PointerMoved(from + (to - from) * t)]),
                |c| app.frame(c),
            );
        }
        let x = app.xform.as_ref().expect("transforming");
        // Width 80 → 120 from the opposite corner (the left edge stays at
        // 60): right edge 180, so sx = 1.5; height unchanged.
        assert!(
            (x.sx - 1.5).abs() < 0.05 && (x.sy - 1.0).abs() < 0.05,
            "{} {}",
            x.sx,
            x.sy
        );
        let left = x.corners()[0];
        assert!(
            (left.0 - 60.0).abs() < 0.5 && (left.1 - 40.0).abs() < 0.5,
            "{left:?}"
        );
    }

    #[test]
    fn shift_brackets_step_the_hardness_and_plain_brackets_the_size() {
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(Document::new(40, 40)), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.tool = Tool::Brush;
        app.brush.hardness = 0.5;
        app.brush.radius = 10.0;
        let ctx = crate::a11y_tests::ctx();
        let key = |m: egui::Modifiers| egui::Event::Key {
            key: Key::CloseBracket,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        let raw = |events, m| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            modifiers: m,
            ..Default::default()
        };
        let shift = egui::Modifiers::SHIFT;
        let _ = ctx.run(raw(vec![key(shift)], shift), |c| app.frame(c));
        assert_eq!((app.brush.hardness, app.brush.radius), (0.75, 10.0));
        let none = egui::Modifiers::NONE;
        let _ = ctx.run(raw(vec![key(none)], none), |c| app.frame(c));
        assert_eq!((app.brush.hardness, app.brush.radius), (0.75, 12.5));
    }

    #[test]
    fn shift_click_paints_a_straight_line_from_the_last_stroke() {
        let mut doc = Document::new(200, 120);
        let id = doc.add_pixel_layer("ink");
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        app.tool = Tool::Brush;
        app.brush.radius = 2.0;
        let ctx = crate::a11y_tests::ctx();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], false);
        }
        let (a, b) = (egui::pos2(560.0, 450.0), egui::pos2(680.0, 450.0));
        click(&mut app, &ctx, a, egui::Modifiers::NONE);
        let (ax, ay) = app.cursor_doc.unwrap();
        click(&mut app, &ctx, b, egui::Modifiers::SHIFT);
        let (bx, _) = app.cursor_doc.unwrap();
        assert!(bx > ax + 10, "the clicks are apart: {ax} {bx}");
        let px = app.editor.doc().layer(id).unwrap().pixels().unwrap();
        for x in [ax, (ax + bx) / 2, bx] {
            assert!(px.get_pixel(x, ay).a > 0.5, "painted at x = {x}");
        }
        assert_eq!(px.get_pixel((ax + bx) / 2, ay + 8).a, 0.0, "a line, not a band");
    }
}
