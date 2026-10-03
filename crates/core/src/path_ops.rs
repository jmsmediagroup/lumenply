//! The Paths panel's commands: rename and duplicate saved paths, run a
//! work-path command (fill, stroke, selection) on a saved path without
//! touching the work path, and Photoshop's "Make work path from
//! selection" (trace the selection's 50% edge into corner-node polygons).

use std::collections::HashMap;

use crate::{Command, EditError, EditResult};
use lumenply_doc::{Document, NamedPath, PathNode, SubPath, VectorPath};
use lumenply_tiles::Rect;

/// `base`, or `base 2`, `base 3`... — the first name not in `taken`.
fn unique_path_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken.contains(&n.as_str()))
        .expect("unbounded")
}

/// Rename saved path `index` (trimmed, unique among the other paths).
pub struct RenameSavedPath {
    pub index: usize,
    pub name: String,
}

impl Command for RenameSavedPath {
    fn label(&self) -> String {
        "Rename path".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.index >= doc.saved_paths.len() {
            return Err(EditError::Invalid(format!("no saved path #{}", self.index)));
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err(EditError::Invalid("the path needs a name".into()));
        }
        let others: Vec<&str> = doc
            .saved_paths
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.index)
            .map(|(_, p)| p.name.as_str())
            .collect();
        let name = unique_path_name(name, &others);
        doc.saved_paths[self.index].name = name;
        Ok(())
    }
}

/// Copy saved path `index` to a new path right after it, named
/// "<name> copy" (numbered when taken).
pub struct DuplicateSavedPath {
    pub index: usize,
}

impl Command for DuplicateSavedPath {
    fn label(&self) -> String {
        "Duplicate path".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let src = doc
            .saved_paths
            .get(self.index)
            .ok_or_else(|| EditError::Invalid(format!("no saved path #{}", self.index)))?;
        let taken: Vec<&str> = doc.saved_paths.iter().map(|p| p.name.as_str()).collect();
        let copy = NamedPath {
            name: unique_path_name(&format!("{} copy", src.name), &taken),
            path: src.path.clone(),
        };
        doc.saved_paths.insert(self.index + 1, copy);
        Ok(())
    }
}

/// Run a work-path command (FillPath, StrokeWorkPath, PathToSelection...)
/// on saved path `index` instead: the path stands in as the work path for
/// the duration of the command and the real work path comes back after,
/// so the Paths panel can fill or stroke any path in one undo step.
pub struct OnSavedPath<C: Command> {
    pub index: usize,
    pub inner: C,
}

impl<C: Command> Command for OnSavedPath<C> {
    fn label(&self) -> String {
        self.inner.label()
    }

    fn target_layer(&self) -> Option<lumenply_doc::LayerId> {
        self.inner.target_layer()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        // The inner command would measure the work path; anything is safe.
        None
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let path = doc
            .saved_paths
            .get(self.index)
            .map(|p| p.path.clone())
            .ok_or_else(|| EditError::Invalid(format!("no saved path #{}", self.index)))?;
        let work = doc.work_path.replace(path);
        let out = self.inner.apply(doc);
        doc.work_path = work;
        out
    }
}

/// Photoshop's "Make work path from selection": the selection's 50%
/// coverage edge becomes the work path, one closed corner-node subpath per
/// outline (holes included; paths fill even-odd), simplified so no point
/// strays more than `tolerance` pixels from the traced edge.
pub struct SelectionToWorkPath {
    pub tolerance: f32,
}

impl Command for SelectionToWorkPath {
    fn label(&self) -> String {
        "Make work path".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .selection
            .as_ref()
            .ok_or_else(|| EditError::Invalid("make a selection first".into()))?;
        let area = sel.bounds_within(doc.canvas());
        if area.is_empty() {
            return Err(EditError::Invalid("the selection is empty".into()));
        }
        let (w, h) = (area.w as usize, area.h as usize);
        let mut inside = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                inside[y * w + x] = sel.value(area.x + x as i32, area.y + y as i32) >= 0.5;
            }
        }
        let loops = trace_outlines(&inside, w, h);
        let subpaths: Vec<SubPath> = loops
            .into_iter()
            .map(|lp| simplify_closed(&lp, self.tolerance.max(0.0)))
            .filter(|lp| lp.len() >= 3)
            .map(|lp| SubPath {
                nodes: lp
                    .into_iter()
                    .map(|(x, y)| PathNode::corner((x + area.x) as f32, (y + area.y) as f32))
                    .collect(),
                closed: true,
            })
            .collect();
        if subpaths.is_empty() {
            return Err(EditError::Invalid("the selection has no edge to trace".into()));
        }
        doc.work_path = Some(VectorPath { subpaths });
        Ok(())
    }
}

/// The outlines of the `true` cells of a `w`×`h` grid, as closed loops of
/// pixel-corner vertices: outer outlines clockwise (y down), holes
/// counter-clockwise. Only corners are kept (straight runs collapse).
fn trace_outlines(inside: &[bool], w: usize, h: usize) -> Vec<Vec<(i32, i32)>> {
    let at = |x: i32, y: i32| {
        x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && inside[y as usize * w + x as usize]
    };
    // Directed boundary edges, inside on the right going clockwise.
    let mut edges: Vec<((i32, i32), (i32, i32))> = Vec::new();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if !at(x, y) {
                continue;
            }
            if !at(x, y - 1) {
                edges.push(((x, y), (x + 1, y)));
            }
            if !at(x + 1, y) {
                edges.push(((x + 1, y), (x + 1, y + 1)));
            }
            if !at(x, y + 1) {
                edges.push(((x + 1, y + 1), (x, y + 1)));
            }
            if !at(x - 1, y) {
                edges.push(((x, y + 1), (x, y)));
            }
        }
    }
    let mut from: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        from.entry(e.0).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut loops = Vec::new();
    for start in 0..edges.len() {
        if used[start] {
            continue;
        }
        let mut pts: Vec<(i32, i32)> = Vec::new();
        let mut cur = start;
        loop {
            used[cur] = true;
            let (a, b) = edges[cur];
            pts.push(a);
            let dir = (b.0 - a.0, b.1 - a.1);
            // Where two outlines touch at a corner, turn right (stay with
            // this outline's own pixel) so diagonal neighbours stay apart.
            let next = from.get(&b).and_then(|c| {
                let free: Vec<usize> = c.iter().copied().filter(|&i| !used[i]).collect();
                let right = (-dir.1, dir.0);
                free.iter()
                    .copied()
                    .find(|&i| {
                        let (p, q) = edges[i];
                        (q.0 - p.0, q.1 - p.1) == right
                    })
                    .or_else(|| free.first().copied())
            });
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        // Keep corners only.
        let n = pts.len();
        let corners: Vec<(i32, i32)> = (0..n)
            .filter(|&i| {
                let p = pts[(i + n - 1) % n];
                let c = pts[i];
                let q = pts[(i + 1) % n];
                (c.0 - p.0) * (q.1 - c.1) - (c.1 - p.1) * (q.0 - c.0) != 0
            })
            .map(|i| pts[i])
            .collect();
        if corners.len() >= 3 {
            loops.push(corners);
        }
    }
    loops
}

/// Douglas–Peucker on a closed polygon: split at the vertex farthest from
/// the first one and simplify both halves.
fn simplify_closed(pts: &[(i32, i32)], tol: f32) -> Vec<(i32, i32)> {
    if tol <= 0.0 || pts.len() <= 4 {
        return pts.to_vec();
    }
    let d2 = |a: (i32, i32), b: (i32, i32)| ((a.0 - b.0).pow(2) + (a.1 - b.1).pow(2)) as f32;
    let far = (1..pts.len())
        .max_by(|&i, &j| d2(pts[0], pts[i]).total_cmp(&d2(pts[0], pts[j])))
        .unwrap_or(1);
    let mut first: Vec<(i32, i32)> = pts[..=far].to_vec();
    let mut second: Vec<(i32, i32)> = pts[far..].to_vec();
    second.push(pts[0]);
    first = douglas_peucker(&first, tol);
    second = douglas_peucker(&second, tol);
    first.pop();
    second.pop();
    first.extend(second);
    first
}

fn douglas_peucker(pts: &[(i32, i32)], tol: f32) -> Vec<(i32, i32)> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (dx, dy) = ((b.0 - a.0) as f32, (b.1 - a.1) as f32);
    let len = (dx * dx + dy * dy).sqrt();
    let dist = |p: (i32, i32)| {
        let (px, py) = ((p.0 - a.0) as f32, (p.1 - a.1) as f32);
        if len < 1e-6 {
            (px * px + py * py).sqrt()
        } else {
            (px * dy - py * dx).abs() / len
        }
    };
    let (idx, worst) = (1..pts.len() - 1)
        .map(|i| (i, dist(pts[i])))
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .expect("at least three points");
    if worst <= tol {
        return vec![a, b];
    }
    let mut left = douglas_peucker(&pts[..=idx], tol);
    let right = douglas_peucker(&pts[idx..], tol);
    left.pop();
    left.extend(right);
    left
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{FillPath, PathToSelection, SaveWorkPath, SetSelection, SetWorkPath};
    use crate::Editor;
    use lumenply_doc::{CombineOp, Selection};

    fn square(x: f32, y: f32, s: f32) -> VectorPath {
        VectorPath {
            subpaths: vec![SubPath {
                nodes: vec![
                    PathNode::corner(x, y),
                    PathNode::corner(x + s, y),
                    PathNode::corner(x + s, y + s),
                    PathNode::corner(x, y + s),
                ],
                closed: true,
            }],
        }
    }

    fn with_paths() -> Editor {
        let mut ed = Editor::new(Document::new(64, 64));
        ed.execute(&SetWorkPath {
            path: Some(square(4.0, 4.0, 10.0)),
        })
        .unwrap();
        ed.execute(&SaveWorkPath { name: "Box".into() }).unwrap();
        ed.execute(&SetWorkPath {
            path: Some(square(30.0, 30.0, 20.0)),
        })
        .unwrap();
        ed.execute(&SaveWorkPath { name: "Big".into() }).unwrap();
        ed
    }

    fn names(ed: &Editor) -> Vec<String> {
        ed.doc().saved_paths.iter().map(|p| p.name.clone()).collect()
    }

    #[test]
    fn paths_rename_and_duplicate_with_unique_names() {
        let mut ed = with_paths();
        ed.execute(&RenameSavedPath {
            index: 0,
            name: " Logo ".into(),
        })
        .unwrap();
        assert_eq!(names(&ed), ["Logo", "Big"]);
        ed.execute(&RenameSavedPath {
            index: 0,
            name: "Big".into(),
        })
        .unwrap();
        assert_eq!(names(&ed), ["Big 2", "Big"]);
        assert!(ed
            .execute(&RenameSavedPath {
                index: 0,
                name: "".into()
            })
            .is_err());
        ed.execute(&DuplicateSavedPath { index: 1 }).unwrap();
        ed.execute(&DuplicateSavedPath { index: 1 }).unwrap();
        assert_eq!(names(&ed), ["Big 2", "Big", "Big copy 2", "Big copy"]);
        assert_eq!(ed.doc().saved_paths[2].path, square(30.0, 30.0, 20.0));
        ed.undo();
        ed.undo();
        assert_eq!(names(&ed), ["Big 2", "Big"]);
        assert!(ed.execute(&DuplicateSavedPath { index: 5 }).is_err());
    }

    #[test]
    fn a_saved_path_fills_and_selects_without_touching_the_work_path() {
        let mut ed = with_paths();
        let paper = lumenply_tiles::Raster::filled(64, 64, lumenply_tiles::Rgba::WHITE);
        ed.execute(&crate::commands::AddPixelLayer::from_raster("Bg", paper, 0, 0))
            .unwrap();
        let layer = ed.doc().layers()[0].id;
        ed.execute(&OnSavedPath {
            index: 0,
            inner: FillPath {
                layer,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        })
        .unwrap();
        assert_eq!(ed.history().last().copied(), Some("Fill path"));
        let px = |x, y| ed.doc().layers()[0].pixels().unwrap().get_pixel(x, y);
        // Inside the 10 px box at (4, 4): red; inside the work path: white.
        assert_eq!((px(8, 8).r, px(8, 8).g), (1.0, 0.0));
        assert_eq!((px(40, 40).r, px(40, 40).g), (1.0, 1.0));
        assert_eq!(ed.doc().work_path, Some(square(30.0, 30.0, 20.0)));

        ed.execute(&OnSavedPath {
            index: 0,
            inner: PathToSelection {
                op: CombineOp::Replace,
            },
        })
        .unwrap();
        let sel = ed.doc().selection.as_ref().unwrap();
        assert_eq!((sel.value(8, 8), sel.value(40, 40)), (1.0, 0.0));
        // The 50% edge sits exactly on the box (x 4..14).
        let on = |x: i32| sel.value(x, 8) >= 0.5;
        assert_eq!((on(3), on(4), on(13), on(14)), (false, true, true, false));
        assert_eq!(ed.doc().work_path, Some(square(30.0, 30.0, 20.0)));
        assert!(ed
            .execute(&OnSavedPath {
                index: 7,
                inner: PathToSelection {
                    op: CombineOp::Replace
                },
            })
            .is_err());
    }

    #[test]
    fn a_rectangular_selection_traces_to_its_four_corners() {
        let mut ed = Editor::new(Document::new(64, 48));
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 6, 20, 12))),
        })
        .unwrap();
        ed.execute(&SelectionToWorkPath { tolerance: 2.0 }).unwrap();
        let path = ed.doc().work_path.clone().unwrap();
        assert_eq!(path.subpaths.len(), 1);
        let sp = &path.subpaths[0];
        assert!(sp.closed);
        let pts: Vec<(f32, f32)> = sp.nodes.iter().map(|n| n.point).collect();
        assert_eq!(pts, [(10.0, 6.0), (30.0, 6.0), (30.0, 18.0), (10.0, 18.0)]);
        // Back to a selection: the same box.
        ed.execute(&SetSelection { selection: None }).unwrap();
        ed.execute(&PathToSelection {
            op: CombineOp::Replace,
        })
        .unwrap();
        let sel = ed.doc().selection.as_ref().unwrap();
        let on = |x: i32, y: i32| sel.value(x, y) >= 0.5;
        assert_eq!(
            (on(9, 10), on(10, 10), on(29, 10), on(30, 10)),
            (false, true, true, false)
        );
        assert_eq!(
            (on(20, 5), on(20, 6), on(20, 17), on(20, 18)),
            (false, true, true, false)
        );
    }

    #[test]
    fn a_ring_traces_to_an_outline_and_a_hole() {
        let mut ed = Editor::new(Document::new(64, 64));
        let mut ring = Selection::rect(Rect::new(8, 8, 40, 40));
        ring.combine(&Selection::rect(Rect::new(20, 20, 16, 16)), CombineOp::Subtract);
        ed.execute(&SetSelection {
            selection: Some(ring),
        })
        .unwrap();
        ed.execute(&SelectionToWorkPath { tolerance: 0.0 }).unwrap();
        let path = ed.doc().work_path.clone().unwrap();
        let mut boxes: Vec<(f32, f32, f32, f32)> = path
            .subpaths
            .iter()
            .map(|sp| {
                let xs = sp.nodes.iter().map(|n| n.point.0);
                let ys = sp.nodes.iter().map(|n| n.point.1);
                (
                    xs.clone().fold(f32::MAX, f32::min),
                    ys.clone().fold(f32::MAX, f32::min),
                    xs.fold(f32::MIN, f32::max),
                    ys.fold(f32::MIN, f32::max),
                )
            })
            .collect();
        boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(boxes, [(8.0, 8.0, 48.0, 48.0), (20.0, 20.0, 36.0, 36.0)]);
        assert!(path.subpaths.iter().all(|s| s.nodes.len() == 4));
        // Even-odd: the hole stays a hole.
        ed.execute(&SetSelection { selection: None }).unwrap();
        ed.execute(&PathToSelection {
            op: CombineOp::Replace,
        })
        .unwrap();
        let sel = ed.doc().selection.as_ref().unwrap();
        assert_eq!((sel.value(10, 10), sel.value(28, 28)), (1.0, 0.0));
    }

    #[test]
    fn a_staircase_simplifies_within_the_tolerance() {
        // A 45° staircase (a right triangle of pixels) collapses to three
        // corners at tolerance 2 but keeps every step at tolerance 0.
        let n = 12usize;
        let inside: Vec<bool> = (0..n * n).map(|i| (i % n) <= (i / n)).collect();
        let loops = trace_outlines(&inside, n, n);
        assert_eq!(loops.len(), 1);
        assert_eq!(
            loops[0].len(),
            2 * n + 2,
            "every step is a corner pair, plus two right angles"
        );
        let simple = simplify_closed(&loops[0], 2.0);
        assert_eq!(simple.len(), 3, "{simple:?}");
        let mut s = simple.clone();
        s.sort_unstable();
        assert_eq!(s, [(0, 0), (0, 12), (12, 12)]);
        assert!(SelectionToWorkPath { tolerance: 1.0 }
            .apply(&mut Document::new(4, 4))
            .is_err());
    }
}
