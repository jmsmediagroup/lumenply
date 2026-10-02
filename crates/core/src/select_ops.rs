//! Select > Modify (expand, contract, border, smooth) and Select > Grow /
//! Similar. Re-exported from [`crate::commands`].

use lumenply_doc::selection_ops::EdgeOp;
use lumenply_doc::{Document, Selection};
use lumenply_tiles::{Raster, Rect};

use crate::commands::SampleSource;
use crate::{Command, EditError, EditResult};

/// Reshape the active selection's edge (Select > Modify).
pub struct ModifySelectionEdge {
    pub op: EdgeOp,
}

impl Command for ModifySelectionEdge {
    fn label(&self) -> String {
        format!("{} selection {:.0} px", self.op.name(), self.op.amount())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let s = doc
            .selection
            .as_mut()
            .ok_or_else(|| EditError::Invalid("nothing is selected".into()))?;
        s.modify_edge(self.op, canvas);
        if s.is_empty() || (s.coverage.default <= 0.0 && s.tight_bounds(canvas).is_empty()) {
            doc.selection = None;
        }
        Ok(())
    }
}

/// Select > Grow (`contiguous`) and Select > Similar: add the pixels whose
/// colour lies inside the selection's colour range widened by the magic
/// wand `tolerance` — touching the selection for Grow, anywhere for
/// Similar. Repeating it keeps widening the range, as in Photoshop.
pub struct GrowSelection {
    pub tolerance: f32,
    pub contiguous: bool,
    pub sample: SampleSource,
}

impl Command for GrowSelection {
    fn label(&self) -> String {
        if self.contiguous {
            "Grow selection"
        } else {
            "Select similar"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let Some(sel) = doc.selection.clone() else {
            return Err(EditError::Invalid("nothing is selected".into()));
        };
        let img = sample_raster(doc, self.sample, canvas)?;
        let cov = sel.coverage.to_dense(canvas);
        let grown = grow_region(&img, &cov, self.tolerance, self.contiguous);
        let merged: Vec<f32> = cov
            .iter()
            .zip(&grown)
            .map(|(&c, &g)| if g { 1.0 } else { c })
            .collect();
        let mut out = sel;
        out.coverage.set_dense(canvas, &merged);
        doc.selection = Some(out).filter(|s: &Selection| !s.is_empty());
        Ok(())
    }
}

/// The pixels a flood-based tool compares, as a canvas-sized raster.
fn sample_raster(doc: &Document, source: SampleSource, canvas: Rect) -> Result<Raster, EditError> {
    match source {
        SampleSource::Merged => Ok(lumenply_render::composite_raster(doc)),
        SampleSource::Layer(id) => Ok(doc
            .layer(id)
            .ok_or(EditError::NoLayer(id))?
            .pixels()
            .ok_or(EditError::NotPixel(id))?
            .to_raster(canvas)),
    }
}

/// The per-channel colour range (straight RGBA) of the selected pixels,
/// 0.5th to 99.5th percentile so a few stray pixels at a soft edge don't
/// blow it open.
fn colour_range(img: &Raster, cov: &[f32]) -> Option<[(f32, f32); 4]> {
    const BINS: usize = 1024;
    let mut hist = vec![[0u32; BINS]; 4];
    let mut n = 0u64;
    for (p, &c) in img.pixels.iter().zip(cov) {
        if c < 0.5 {
            continue;
        }
        n += 1;
        for (ch, v) in p.to_straight().into_iter().enumerate() {
            hist[ch][((v.clamp(0.0, 1.0) * (BINS - 1) as f32).round()) as usize] += 1;
        }
    }
    if n == 0 {
        return None;
    }
    let cut = (n as f64 * 0.005).floor() as u64;
    let mut out = [(0.0, 0.0); 4];
    for (ch, h) in hist.iter().enumerate() {
        let mut acc = 0u64;
        let lo = h
            .iter()
            .position(|&k| {
                acc += k as u64;
                acc > cut
            })
            .unwrap_or(0);
        acc = 0;
        let hi = BINS
            - 1
            - h.iter()
                .rev()
                .position(|&k| {
                    acc += k as u64;
                    acc > cut
                })
                .unwrap_or(0);
        out[ch] = (lo as f32 / (BINS - 1) as f32, hi as f32 / (BINS - 1) as f32);
    }
    Some(out)
}

/// Which pixels Grow (`contiguous`) or Similar adds.
fn grow_region(img: &Raster, cov: &[f32], tolerance: f32, contiguous: bool) -> Vec<bool> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = vec![false; w * h];
    let Some(range) = colour_range(img, cov) else {
        return out;
    };
    let tol = tolerance.clamp(0.0, 1.0) + 1e-4;
    let transparent_selected = range[3].0 <= 0.001;
    let matches = |i: usize| {
        let c = img.pixels[i].to_straight();
        if transparent_selected && c[3] <= 0.001 {
            return true;
        }
        (0..4).all(|k| c[k] >= range[k].0 - tol && c[k] <= range[k].1 + tol)
    };
    if !contiguous {
        for (i, o) in out.iter_mut().enumerate() {
            *o = cov[i] < 1.0 && matches(i);
        }
        return out;
    }
    // Breadth-first from every selected pixel through matching neighbours.
    let mut seen = vec![false; w * h];
    let mut queue: std::collections::VecDeque<usize> = (0..w * h).filter(|&i| cov[i] >= 0.5).collect();
    for &i in &queue {
        seen[i] = true;
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        let mut visit = |j: usize| {
            if !seen[j] {
                seen[j] = true;
                if matches(j) {
                    out[j] = true;
                    queue.push_back(j);
                }
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::AddPixelLayer;
    use crate::Editor;
    use lumenply_tiles::Rgba;

    /// 60×20: x 0..10 grey 0.20, x 10..20 grey 0.25, x 20..40 grey 0.80,
    /// x 40..45 grey 0.20 again (cut off from the first patch), rest 0.8.
    fn bands() -> (Editor, u64) {
        let mut r = Raster::new(60, 20);
        for y in 0..20 {
            for x in 0..60 {
                let v = match x {
                    0..10 => 0.20,
                    10..20 => 0.25,
                    40..45 => 0.20,
                    _ => 0.80,
                };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let mut ed = Editor::new(Document::new(60, 20));
        ed.execute(&AddPixelLayer::from_raster("bands", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        (ed, id)
    }

    fn selected_columns(ed: &Editor) -> Vec<i32> {
        let s = ed.doc().selection.as_ref().unwrap();
        (0..60).filter(|&x| s.value(x, 10) >= 0.5).collect()
    }

    #[test]
    fn grow_adds_touching_pixels_within_tolerance_and_similar_adds_all() {
        let (mut ed, id) = bands();
        ed.execute(&crate::commands::SetSelection {
            selection: Some(Selection::rect(Rect::new(2, 2, 4, 4))),
        })
        .unwrap();
        let grow = GrowSelection {
            tolerance: 0.1,
            contiguous: true,
            sample: SampleSource::Layer(id),
        };
        ed.execute(&grow).unwrap();
        // Range [0.2, 0.2] ± 0.1 takes the 0.25 band, not the 0.8 one, and
        // not the far 0.2 patch it can't reach.
        assert_eq!(selected_columns(&ed), (0..20).collect::<Vec<_>>());
        assert_eq!(ed.doc().selection.as_ref().unwrap().value(0, 0), 1.0);
        assert_eq!(ed.undo().as_deref(), Some("Grow selection"));

        ed.execute(&GrowSelection {
            contiguous: false,
            ..grow
        })
        .unwrap();
        let want: Vec<i32> = (0..20).chain(40..45).collect();
        assert_eq!(selected_columns(&ed), want);

        // A tolerance too tight for the next band only fills the first.
        ed.undo();
        ed.execute(&GrowSelection {
            tolerance: 0.02,
            contiguous: true,
            sample: SampleSource::Merged,
        })
        .unwrap();
        assert_eq!(selected_columns(&ed), (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn modify_edge_is_one_undo_step_and_needs_a_selection() {
        let (mut ed, _) = bands();
        let expand = ModifySelectionEdge {
            op: EdgeOp::Expand(2.0),
        };
        assert!(ed.execute(&expand).is_err(), "nothing selected");
        ed.execute(&crate::commands::SetSelection {
            selection: Some(Selection::rect(Rect::new(20, 5, 10, 10))),
        })
        .unwrap();
        ed.execute(&expand).unwrap();
        assert_eq!(selected_columns(&ed), (18..32).collect::<Vec<_>>());
        assert_eq!(ed.history().last().copied(), Some("Expand selection 2 px"));
        ed.execute(&ModifySelectionEdge {
            op: EdgeOp::Contract(20.0),
        })
        .unwrap();
        assert!(ed.doc().selection.is_none(), "contracted away entirely");
        ed.undo();
        ed.undo();
        assert_eq!(selected_columns(&ed), (20..30).collect::<Vec<_>>());
    }
}
