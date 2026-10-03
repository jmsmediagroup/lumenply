//! The Paths panel: the work path and the saved paths with outline
//! thumbnails. Clicking one shows it on the canvas (display only); the
//! footer fills or strokes it, loads it as a selection, makes a work path
//! from the selection, saves the work path, duplicates or deletes.

use super::*;
use crate::channels_panel::{icon_load_selection, icon_trash, load_op};
use crate::layers::IconButton;
use crate::panels::{panel_row, rename_field, PathRow, ROW_H};
use lumenply_core::path_ops::{DuplicateSavedPath, OnSavedPath, RenameSavedPath};

/// What a click in the Paths panel asks for.
enum PathAct {
    Pick(PathRow),
    Fill(PathRow),
    Stroke(PathRow),
    Select(PathRow, CombineOp),
    MakeWork,
    Save,
    UseAsWork(usize),
    Duplicate(usize),
    Rename(usize),
    Delete(PathRow),
}

/// The flattened outline of `path`, at most `max` points per subpath.
fn thumb_lines(path: &lumenply_doc::VectorPath, max: usize) -> Vec<Vec<(f32, f32)>> {
    path.flatten()
        .into_iter()
        .map(|(pts, _)| {
            let step = pts.len().div_ceil(max.max(2)).max(1);
            let mut out: Vec<(f32, f32)> = pts.iter().step_by(step).copied().collect();
            if let Some(last) = pts.last() {
                if out.last() != Some(last) {
                    out.push(*last);
                }
            }
            out
        })
        .collect()
}

/// "Path N" with the first N not already taken.
fn next_path_name(doc: &Document) -> String {
    (1..)
        .map(|i| format!("Path {i}"))
        .find(|n| !doc.saved_paths.iter().any(|p| &p.name == n))
        .expect("unbounded")
}

impl App {
    /// Draw the path picked in the Paths panel over the canvas (the Pen
    /// draws the work path itself, nodes and all, while it is up).
    pub(crate) fn paint_selected_path(&self, painter: &egui::Painter, rect: egui::Rect) {
        let doc = self.editor.doc();
        let path = match self.panels.path_sel {
            Some(PathRow::Saved(i)) => doc.saved_paths.get(i).map(|p| &p.path),
            Some(PathRow::Work) if self.tool != Tool::Pen => doc.work_path.as_ref(),
            _ => None,
        };
        let Some(path) = path else { return };
        let origin = rect.min + self.pan;
        let zoom = self.zoom;
        for (pts, _) in path.flatten() {
            let line: Vec<Pos2> = pts
                .iter()
                .map(|&(x, y)| egui::pos2(origin.x + x * zoom, origin.y + y * zoom))
                .collect();
            painter.add(Shape::line(
                line.clone(),
                Stroke::new(2.4, Color32::from_black_alpha(150)),
            ));
            painter.add(Shape::line(line, Stroke::new(1.2, ACCENT)));
        }
    }

    /// Run a work-path command on a Paths panel row: directly for the
    /// work path, through `OnSavedPath` for a saved one.
    fn run_on_path<C: Command>(&mut self, row: PathRow, cmd: C) {
        match row {
            PathRow::Work => self.run(&cmd),
            PathRow::Saved(index) => self.run(&OnSavedPath { index, inner: cmd }),
        }
    }

    pub(crate) fn paths_ui(&mut self, ui: &mut egui::Ui) {
        let doc = self.editor.doc();
        let has_work = doc.work_path.is_some();
        let names: Vec<String> = doc.saved_paths.iter().map(|p| p.name.clone()).collect();
        let canvas = doc.canvas();
        let has_selection = doc.selection.is_some();
        let sel = self.panels.path_sel;
        let target = sel.or(has_work.then_some(PathRow::Work));
        let on_pixels = self.active_is_pixel();
        let mods = ui.input(|i| i.modifiers);
        let mut act: Option<PathAct> = None;
        let mut renaming = self.panels.path_renaming.take();
        let mut rename_done: Option<Result<(usize, String), ()>> = None;
        let rows: Vec<PathRow> = has_work
            .then_some(PathRow::Work)
            .into_iter()
            .chain((0..names.len()).map(PathRow::Saved))
            .collect();
        let scroll = egui::ScrollArea::vertical()
            .id_salt("paths")
            .max_height((ui.available_height() - layers::FOOTER_H).max(ROW_H))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if rows.is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new("No paths yet").color(TEXT));
                    ui.label(
                        RichText::new("Draw one with the Pen (P), or make one from a selection below.")
                            .small()
                            .color(MUTED),
                    );
                }
                for row in &rows {
                    let (name, path) = match *row {
                        PathRow::Work => ("Work Path".to_string(), self.editor.doc().work_path.as_ref()),
                        PathRow::Saved(i) => (
                            names[i].clone(),
                            self.editor.doc().saved_paths.get(i).map(|p| &p.path),
                        ),
                    };
                    let active = sel == Some(*row);
                    let (rect, resp) = panel_row(ui, active, false, &format!("Path {name}"));
                    let p = ui.painter();
                    let cy = rect.center().y;
                    // Thumbnail: the canvas in white, the outline in ink.
                    let t = egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + 8.0, cy - 15.0),
                        egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                    );
                    p.rect_filled(t, 2.0, Color32::from_gray(70));
                    let s = (t.width() / canvas.w.max(1) as f32).min(t.height() / canvas.h.max(1) as f32);
                    let page = egui::Rect::from_center_size(
                        t.center(),
                        egui::vec2(canvas.w as f32 * s, canvas.h as f32 * s),
                    );
                    p.rect_filled(page, 0.0, Color32::from_gray(235));
                    if let Some(path) = path {
                        for line in thumb_lines(path, 160) {
                            let pts: Vec<Pos2> = line
                                .iter()
                                .map(|&(x, y)| page.min + egui::vec2(x, y) * s)
                                .collect();
                            p.add(Shape::line(pts, Stroke::new(1.2, Color32::from_gray(30))));
                        }
                    }
                    p.rect_stroke(t, 2.0, Stroke::new(1.0, LINE));
                    let x = t.max.x + 8.0;
                    if let (PathRow::Saved(i), Some((ri, text))) = (*row, renaming.as_mut()) {
                        if *ri == i {
                            let r = egui::Rect::from_min_max(
                                egui::pos2(x, cy - 11.0),
                                egui::pos2(rect.max.x - 6.0, cy + 11.0),
                            );
                            if let Some(done) = rename_field(ui, r, text, "Path name") {
                                rename_done = Some(done.map(|n| (i, n)));
                            }
                            continue;
                        }
                    }
                    let font = FontId::proportional(14.0);
                    let galley = if *row == PathRow::Work {
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            &name,
                            0.0,
                            egui::TextFormat {
                                font_id: font,
                                color: TEXT,
                                italics: true,
                                ..Default::default()
                            },
                        );
                        ui.fonts(|f| f.layout_job(job))
                    } else {
                        elided(ui, &name, font, TEXT, rect.max.x - 8.0 - x).0
                    };
                    p.galley(egui::pos2(x, cy - galley.size().y / 2.0), galley, TEXT);
                    let tip = match row {
                        PathRow::Work => "The work path · double-click to save it under a name",
                        PathRow::Saved(_) => {
                            "Click to show it · double-click to rename · Cmd-click to load as selection"
                        }
                    };
                    let resp = resp.on_hover_text(tip);
                    let row = *row;
                    context_menu(&resp, |ui| {
                        if menu_item_if(ui, on_pixels, "Fill path", "") {
                            act = Some(PathAct::Fill(row));
                        }
                        if menu_item_if(ui, on_pixels, "Stroke path", "") {
                            act = Some(PathAct::Stroke(row));
                        }
                        if menu_item(ui, "Make selection", "") {
                            act = Some(PathAct::Select(row, CombineOp::Replace));
                        }
                        menu_separator(ui);
                        match row {
                            PathRow::Work => {
                                if menu_item(ui, "Save path", "") {
                                    act = Some(PathAct::Save);
                                }
                            }
                            PathRow::Saved(i) => {
                                if menu_item(ui, "Make work path", "") {
                                    act = Some(PathAct::UseAsWork(i));
                                }
                                if menu_item(ui, "Duplicate path", "") {
                                    act = Some(PathAct::Duplicate(i));
                                }
                                if menu_item(ui, "Rename", "") {
                                    act = Some(PathAct::Rename(i));
                                }
                            }
                        }
                        if menu_item(ui, "Delete path", "") {
                            act = Some(PathAct::Delete(row));
                        }
                    });
                    if resp.double_clicked() {
                        act = Some(match row {
                            PathRow::Work => PathAct::Save,
                            PathRow::Saved(i) => PathAct::Rename(i),
                        });
                    } else if resp.clicked() {
                        act = Some(if mods.command {
                            PathAct::Select(row, load_op(mods))
                        } else {
                            PathAct::Pick(row)
                        });
                    }
                }
            });
        a11y_scroll(ui.ctx(), &scroll, "Paths");

        let need_pixels = "Select a pixel layer to paint the path onto";
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let fill_tip = if on_pixels || target.is_none() {
                "Fill path with the brush colour"
            } else {
                need_pixels
            };
            if ui
                .add_enabled(
                    target.is_some() && on_pixels,
                    IconButton::new(icon_fill, fill_tip),
                )
                .clicked()
            {
                act = target.map(PathAct::Fill);
            }
            let stroke_tip = if on_pixels || target.is_none() {
                "Stroke path with the brush"
            } else {
                need_pixels
            };
            if ui
                .add_enabled(
                    target.is_some() && on_pixels,
                    IconButton::new(icon_stroke, stroke_tip),
                )
                .clicked()
            {
                act = target.map(PathAct::Stroke);
            }
            if ui
                .add_enabled(
                    target.is_some(),
                    IconButton::new(icon_load_selection, "Load path as selection"),
                )
                .clicked()
            {
                act = target.map(|t| PathAct::Select(t, load_op(mods)));
            }
            if ui
                .add_enabled(
                    has_selection,
                    IconButton::new(icon_work_path, "Make work path from selection"),
                )
                .clicked()
            {
                act = Some(PathAct::MakeWork);
            }
            if ui
                .add_enabled(
                    has_work,
                    IconButton::new(icon_new_path, "Save the work path as a new path"),
                )
                .clicked()
            {
                act = Some(PathAct::Save);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(target.is_some(), IconButton::new(icon_trash, "Delete path"))
                    .clicked()
                {
                    act = target.map(PathAct::Delete);
                }
            });
        });

        match rename_done {
            Some(Ok((index, name))) => {
                renaming = None;
                let old = self.editor.doc().saved_paths.get(index).map(|p| p.name.clone());
                if old.as_deref() != Some(name.trim()) {
                    self.run(&RenameSavedPath { index, name });
                }
            }
            Some(Err(())) => renaming = None,
            None => {}
        }
        self.panels.path_renaming = renaming;
        let Some(act) = act else { return };
        match act {
            PathAct::Pick(row) => {
                self.panels.path_sel = if sel == Some(row) { None } else { Some(row) };
            }
            PathAct::Fill(row) => {
                if let Some(layer) = self.active {
                    let color = self.make_brush().color;
                    self.run_on_path(row, FillPath { layer, color });
                }
            }
            PathAct::Stroke(row) => {
                if let Some(layer) = self.active {
                    let mut brush = self.make_brush();
                    brush.mode = BrushMode::Paint;
                    self.run_on_path(row, StrokeWorkPath { layer, brush });
                }
            }
            PathAct::Select(row, op) => self.run_on_path(row, PathToSelection { op }),
            PathAct::MakeWork => self.run_menu_action("make-work-path"),
            PathAct::Save => {
                let name = next_path_name(self.editor.doc());
                self.run(&SaveWorkPath { name });
                let n = self.editor.doc().saved_paths.len();
                if n > 0 {
                    self.panels.path_sel = Some(PathRow::Saved(n - 1));
                }
            }
            PathAct::UseAsWork(index) => {
                self.pen_open = false;
                self.pen_sel = None;
                self.run(&UseSavedPath { index });
                self.panels.path_sel = Some(PathRow::Work);
            }
            PathAct::Duplicate(index) => {
                self.run(&DuplicateSavedPath { index });
                self.panels.path_sel = Some(PathRow::Saved(index + 1));
            }
            PathAct::Rename(i) => {
                self.panels.path_renaming = names.get(i).map(|n| (i, n.clone()));
            }
            PathAct::Delete(PathRow::Work) => {
                self.pen_open = false;
                self.pen_sel = None;
                self.run(&SetWorkPath { path: None });
                self.panels.path_sel = None;
            }
            PathAct::Delete(PathRow::Saved(index)) => {
                self.run(&DeleteSavedPath { index });
                self.panels.path_sel = None;
            }
        }
    }
}

/// A filled disc: fill the path.
fn icon_fill(p: &egui::Painter, r: egui::Rect, c: Color32) {
    p.circle_filled(r.center(), r.width() * 0.42, c);
}

/// A ring: stroke the path.
fn icon_stroke(p: &egui::Painter, r: egui::Rect, c: Color32) {
    p.circle_stroke(r.center(), r.width() * 0.4, Stroke::new(2.0, c));
}

/// A dotted square with path nodes on its corners: make a work path.
fn icon_work_path(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let b = r.shrink(1.5);
    let corners = [b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()];
    for i in 0..4 {
        let (a, z) = (corners[i], corners[(i + 1) % 4]);
        for k in 0..3 {
            let t0 = (k as f32 * 2.0 + 1.0) / 7.0;
            let t1 = (k as f32 * 2.0 + 2.0) / 7.0;
            p.line_segment([a + (z - a) * t0, a + (z - a) * t1], Stroke::new(1.4, c));
        }
    }
    for q in corners {
        p.rect_filled(egui::Rect::from_center_size(q, Vec2::splat(3.5)), 0.5, c);
    }
}

/// A page with a plus: a new (saved) path.
fn icon_new_path(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    p.rect_stroke(r.shrink(1.0), 2.0, s);
    let m = r.center();
    let h = r.width() * 0.22;
    p.line_segment([m - egui::vec2(h, 0.0), m + egui::vec2(h, 0.0)], s);
    p.line_segment([m - egui::vec2(0.0, h), m + egui::vec2(0.0, h)], s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_outlines_are_decimated_but_keep_their_ends() {
        let path = lumenply_doc::VectorPath {
            subpaths: vec![lumenply_doc::SubPath {
                nodes: vec![
                    lumenply_doc::PathNode::corner(0.0, 0.0),
                    lumenply_doc::PathNode::corner(600.0, 0.0),
                ],
                closed: false,
            }],
        };
        let full = path.flatten()[0].0.len();
        assert_eq!(full, 601, "one point per pixel");
        let lines = thumb_lines(&path, 100);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 87, "every 7th point plus the end");
        assert_eq!(lines[0].first(), Some(&(0.0, 0.0)));
        assert_eq!(lines[0].last(), Some(&(600.0, 0.0)));
    }

    /// The work path's anchors per subpath, rounded to whole pixels, and
    /// whether each is closed.
    fn anchors(app: &App) -> Vec<(Vec<(f32, f32)>, bool)> {
        let Some(p) = &app.editor.doc().work_path else {
            return Vec::new();
        };
        p.subpaths
            .iter()
            .map(|sp| {
                let pts = sp
                    .nodes
                    .iter()
                    .map(|n| (n.point.0.round(), n.point.1.round()))
                    .collect();
                (pts, sp.closed)
            })
            .collect()
    }

    #[test]
    fn every_pen_click_is_its_own_undo_step_and_esc_keeps_the_path() {
        use crate::shape_tool::test_frames::{app_with, click_at, doc_point, key};
        let (mut app, ctx) = app_with(400, 300, Tool::Pen);
        let steps = app.editor.history().len();
        let pen_click = |app: &mut App, x: f32, y: f32| {
            let p = doc_point(app, &ctx, x, y);
            click_at(app, &ctx, p, egui::Modifiers::NONE);
        };
        pen_click(&mut app, 50.0, 50.0);
        pen_click(&mut app, 200.0, 50.0);
        pen_click(&mut app, 200.0, 200.0);
        assert_eq!(
            anchors(&app),
            vec![(vec![(50.0, 50.0), (200.0, 50.0), (200.0, 200.0)], false)]
        );
        assert_eq!(app.editor.history().len(), steps + 3, "one step per anchor");
        // Undo takes back the last anchor only.
        assert!(app.editor.undo().is_some());
        assert_eq!(anchors(&app), vec![(vec![(50.0, 50.0), (200.0, 50.0)], false)]);
        // Drawing goes on from there; Esc then finishes the path and keeps it.
        pen_click(&mut app, 120.0, 250.0);
        key(&mut app, &ctx, Key::Escape, egui::Modifiers::NONE);
        assert!(!app.pen_open);
        assert_eq!(
            anchors(&app),
            vec![(vec![(50.0, 50.0), (200.0, 50.0), (120.0, 250.0)], false)]
        );
        // The next click starts a second subpath.
        pen_click(&mut app, 300.0, 100.0);
        assert_eq!(anchors(&app).len(), 2);
        assert_eq!(anchors(&app)[1], (vec![(300.0, 100.0)], false));
    }

    #[test]
    fn the_paths_panel_saves_fills_and_names_every_control() {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(blank(40, 30), None);
        let ctx = crate::a11y_tests::ctx();
        app.prefs.panels.tab = panels::DockTab::Paths;
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(4, 4, 10, 10))),
        });
        app.run_menu_action("make-work-path");
        assert_eq!(next_path_name(app.editor.doc()), "Path 1");
        app.run(&SaveWorkPath {
            name: "Path 1".into(),
        });
        assert_eq!(next_path_name(app.editor.doc()), "Path 2");
        app.panels.path_sel = Some(PathRow::Saved(0));
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
        // Fill the saved path in red on the background.
        app.brush_rgb = [1.0, 0.0, 0.0];
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        let color = app.make_brush().color;
        app.run_on_path(PathRow::Saved(0), FillPath { layer: bg, color });
        let px = app.editor.doc().layers()[0].pixels().unwrap().get_pixel(8, 8);
        assert_eq!((px.r, px.g, px.b), (1.0, 0.0, 0.0));
        let px = app.editor.doc().layers()[0].pixels().unwrap().get_pixel(20, 20);
        assert_eq!((px.r, px.g, px.b), (1.0, 1.0, 1.0));
        app.panels.path_renaming = Some((0, "Path 1".into()));
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
    }
}
