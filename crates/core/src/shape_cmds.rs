//! Shape layer commands: add a shape (the shape tool), edit its geometry,
//! fill and stroke, and convert between shapes and the pen's work path.
//! Re-exported from [`crate::commands`].

use lumenply_doc::{Document, Fill, Layer, LayerContent, LayerId, ShapeGeometry, ShapeLayer, ShapeStroke};
use lumenply_tiles::Rect;

use crate::{Command, EditError, EditResult};

/// The canvas area a shape's pixels can cover: its outline's bounds grown
/// by the stroke's outward reach and a pixel of anti-aliasing.
pub fn shape_area(shape: &ShapeLayer, canvas: Rect) -> Rect {
    let Some([x0, y0, x1, y1]) = shape.bounds() else {
        return Rect::default();
    };
    // The 1e-3 keeps flattening's last-bit wobble from adding a pixel.
    let grow = shape.stroke.map_or(0.0, |s| s.reach_outside()) + 2.0 - 1e-3;
    let (ax, ay) = (
        (x0 - grow).floor().max(-1e6) as i32,
        (y0 - grow).floor().max(-1e6) as i32,
    );
    let (bx, by) = (
        (x1 + grow).ceil().min(1e6) as i32,
        (y1 + grow).ceil().min(1e6) as i32,
    );
    if bx <= ax || by <= ay {
        return Rect::default();
    }
    Rect::new(ax, ay, (bx - ax) as u32, (by - ay) as u32).intersect(&canvas)
}

fn check(shape: &ShapeLayer) -> EditResult {
    if !shape.geometry.is_finite() || !shape.transform.coeffs().iter().all(|v| v.is_finite()) {
        return Err(EditError::Invalid("the shape's numbers must be finite".into()));
    }
    if shape.transform.inverse().is_none() {
        return Err(EditError::Invalid(
            "the shape's transform is not invertible".into(),
        ));
    }
    Ok(())
}

/// Insert `layer` just above `above` (in its sibling list), else on top.
fn insert(doc: &mut Document, layer: Layer, above: Option<LayerId>) {
    match above.and_then(|a| doc.siblings_mut(a).map(|list| (a, list))) {
        Some((a, list)) => {
            let i = list.iter().position(|l| l.id == a).expect("in siblings");
            list.insert(i + 1, layer);
        }
        None => {
            doc.add_layer(layer);
        }
    }
}

/// Add a shape layer (what a shape-tool drag commits) above `above`, or on
/// top of the stack. Its name follows the geometry ("Ellipse", "Star"...)
/// unless `name` is given.
pub struct AddShapeLayer {
    pub shape: ShapeLayer,
    pub name: Option<String>,
    pub above: Option<LayerId>,
}

impl AddShapeLayer {
    pub fn new(shape: ShapeLayer) -> Self {
        AddShapeLayer {
            shape,
            name: None,
            above: None,
        }
    }
}

impl Command for AddShapeLayer {
    fn label(&self) -> String {
        format!("New {} shape", self.shape.geometry.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        check(&self.shape)?;
        let id = doc.alloc_id();
        let mut shape = self.shape.clone();
        lumenply_render::shape::refresh_cache(&mut shape, doc.width, doc.height, doc.float_mode);
        let mut layer = Layer::shape(id, shape);
        if let Some(n) = &self.name {
            layer.name = n.clone();
        }
        insert(doc, layer, self.above);
        Ok(())
    }
}

/// Replace a shape layer's geometry, transform, fill and stroke
/// (re-renders its pixels). Properties edits go through this.
pub struct SetShape {
    pub layer: LayerId,
    pub shape: ShapeLayer,
}

impl Command for SetShape {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        format!("Edit {} shape", self.shape.geometry.name())
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let canvas = doc.canvas();
        let old = doc
            .layer(self.layer)?
            .shape_layer()
            .map(|s| shape_area(s, canvas))?;
        Some(old.union(&shape_area(&self.shape, canvas)).intersect(&canvas))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        check(&self.shape)?;
        let (w, h, float) = (doc.width, doc.height, doc.float_mode);
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let old = l
            .shape_layer()
            .ok_or_else(|| EditError::Invalid(format!("layer {} is not a shape layer", self.layer)))?
            .geometry
            .name();
        // Kind changes rename a layer that still carries the default name.
        if old != self.shape.geometry.name() && l.name == old {
            l.name = self.shape.geometry.name().to_string();
        }
        let s = l.shape_layer_mut().expect("checked above");
        s.geometry = self.shape.geometry.clone();
        s.transform = self.shape.transform;
        s.fill = self.shape.fill.clone();
        s.stroke = self.shape.stroke;
        lumenply_render::shape::refresh_cache(s, w, h, float);
        Ok(())
    }
}

/// Layer ▸ New shape from the pen's work path: a shape layer whose outline
/// is the path (even-odd, so inner subpaths cut holes). The work path
/// stays, as in Photoshop.
pub struct ShapeFromWorkPath {
    pub fill: Option<Fill>,
    pub stroke: Option<ShapeStroke>,
    pub above: Option<LayerId>,
}

impl Command for ShapeFromWorkPath {
    fn label(&self) -> String {
        "Shape from path".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let path = doc
            .work_path
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| EditError::Invalid("there is no path; draw one with the pen first".into()))?;
        AddShapeLayer {
            shape: ShapeLayer::new(ShapeGeometry::Path { path }, self.fill.clone(), self.stroke),
            name: None,
            above: self.above,
        }
        .apply(doc)
    }
}

/// Make the shape's outline the work path (to edit it with the pen, or
/// select or stroke it).
pub struct ShapeToWorkPath {
    pub layer: LayerId,
}

impl Command for ShapeToWorkPath {
    fn label(&self) -> String {
        "Make work path".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default()) // paths are an overlay, not pixels
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let LayerContent::Shape(s) = &l.content else {
            return Err(EditError::Invalid(format!(
                "layer {} is not a shape layer",
                self.layer
            )));
        };
        doc.work_path = Some(s.outline());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{
        AddPixelLayer, FlipLayer, MoveLayer, RasterizeLayer, RotateImage, SetWorkPath, TransformLayer,
    };
    use crate::Editor;
    use lumenply_doc::shape::{ShapeGeometry, StrokeAlign};
    use lumenply_doc::{PathNode, SubPath, VectorPath};
    use lumenply_tiles::Affine;

    fn red_rect(x: f32, y: f32, w: f32, h: f32) -> ShapeLayer {
        ShapeLayer::new(
            ShapeGeometry::Rectangle {
                rect: [x, y, w, h],
                radius: 0.0,
            },
            Some(Fill::Solid {
                color: [1.0, 0.0, 0.0],
            }),
            None,
        )
    }

    /// Alpha to 1/1000 (tiles rest as 16-bit, so 0.5 reads 0.500008).
    fn alpha(ed: &Editor, id: LayerId, x: i32, y: i32) -> f32 {
        let a = ed
            .doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(x, y)
            .a;
        (a * 1000.0).round() / 1000.0
    }

    #[test]
    fn adding_a_shape_renders_it_and_undoes_in_one_step() {
        let mut ed = Editor::new(Document::new(200, 100));
        ed.execute(&AddPixelLayer::new("Background")).unwrap();
        let bg = ed.doc().layers()[0].id;
        ed.execute(&AddPixelLayer::new("Top")).unwrap();
        ed.execute(&AddShapeLayer {
            above: Some(bg),
            ..AddShapeLayer::new(red_rect(10.0, 10.0, 50.0, 30.0))
        })
        .unwrap();
        // Inserted just above the background, named after its geometry.
        let l = &ed.doc().layers()[1];
        assert_eq!(l.name, "Rectangle");
        let id = l.id;
        assert_eq!(alpha(&ed, id, 10, 10), 1.0);
        assert_eq!(alpha(&ed, id, 59, 39), 1.0);
        assert_eq!(alpha(&ed, id, 60, 39), 0.0);
        let p = ed
            .doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(30, 20);
        assert!((p.r - 1.0).abs() < 1e-4 && p.g == 0.0, "{p:?}");
        assert_eq!(ed.history().last().copied(), Some("New Rectangle shape"));
        ed.undo();
        assert_eq!(ed.doc().layer_count(), 2);
        // Non-finite geometry is refused.
        assert!(ed
            .execute(&AddShapeLayer::new(red_rect(f32::NAN, 0.0, 1.0, 1.0)))
            .is_err());
    }

    #[test]
    fn set_shape_edits_fill_stroke_and_corners_live() {
        let mut ed = Editor::new(Document::new(100, 100));
        ed.execute(&AddShapeLayer::new(red_rect(20.0, 20.0, 40.0, 40.0)))
            .unwrap();
        let id = ed.doc().layers()[0].id;
        let mut s = ed.doc().layer(id).unwrap().shape_layer().unwrap().clone();
        s.geometry = ShapeGeometry::Rectangle {
            rect: [20.0, 20.0, 40.0, 40.0],
            radius: 10.0,
        };
        s.stroke = Some(ShapeStroke {
            color: [0.0, 0.0, 1.0],
            width: 2.0,
            align: StrokeAlign::Outside,
            dash: None,
        });
        let cmd = SetShape { layer: id, shape: s };
        // Affected: the box grown by the outside stroke (2 px) and 2 px AA.
        assert_eq!(cmd.affected(ed.doc()), Some(Rect::new(16, 16, 48, 48)));
        ed.execute(&cmd).unwrap();
        let l = ed.doc().layer(id).unwrap();
        assert_eq!(l.name, "Rounded Rectangle", "default name follows the kind");
        // The rounded corner leaves the box corner empty; the outside
        // stroke rings the edge in blue.
        assert_eq!(alpha(&ed, id, 20, 20), 0.0);
        let edge = l.raster_store().unwrap().get_pixel(18, 40);
        assert_eq!((edge.r, edge.b, edge.a), (0.0, 1.0, 1.0));
        assert_eq!(alpha(&ed, id, 17, 40), 0.0);
        ed.undo();
        assert_eq!(alpha(&ed, id, 20, 20), 1.0);
        assert_eq!(ed.doc().layer(id).unwrap().name, "Rectangle");
        // Only shape layers take it.
        ed.execute(&AddPixelLayer::new("px")).unwrap();
        let px = ed.doc().layers()[1].id;
        assert!(ed
            .execute(&SetShape {
                layer: px,
                shape: red_rect(0.0, 0.0, 1.0, 1.0)
            })
            .is_err());
    }

    #[test]
    fn transforms_stay_vector_and_crisp() {
        let mut ed = Editor::new(Document::new(256, 256));
        ed.execute(&AddShapeLayer::new(red_rect(10.0, 10.0, 20.0, 20.0)))
            .unwrap();
        let id = ed.doc().layers()[0].id;
        // Scale 3× twice about the origin: a pixel layer would blur twice;
        // the shape is redrawn from its outline, edges still one pixel.
        for _ in 0..2 {
            ed.execute(&TransformLayer {
                layer: id,
                transform: Affine::scale(1.5, 1.5),
            })
            .unwrap();
        }
        // 10..30 × 2.25 = 22.5..67.5.
        assert_eq!(alpha(&ed, id, 23, 40), 1.0);
        assert_eq!(alpha(&ed, id, 22, 40), 0.5);
        assert_eq!(alpha(&ed, id, 67, 40), 0.5);
        assert_eq!(alpha(&ed, id, 68, 40), 0.0);
        let s = ed.doc().layer(id).unwrap().shape_layer().unwrap();
        assert_eq!(s.transform.coeffs(), [2.25, 0.0, 0.0, 2.25, 0.0, 0.0]);
        // Moving composes a translation.
        ed.execute(&MoveLayer {
            layer: id,
            dx: 5,
            dy: -3,
        })
        .unwrap();
        assert_eq!(alpha(&ed, id, 27, 40), 0.5);
        assert_eq!(alpha(&ed, id, 28, 40), 1.0);
        assert_eq!(alpha(&ed, id, 72, 40), 0.5);
        // Flip mirrors about the shape's box: still exact.
        ed.execute(&FlipLayer {
            layer: id,
            horizontal: true,
        })
        .unwrap();
        assert_eq!(alpha(&ed, id, 72, 40), 0.5);
        assert_eq!(alpha(&ed, id, 28, 40), 1.0);
        // Rotating the image carries the shape along as a vector.
        ed.execute(&RotateImage { quarter_turns: 1 }).unwrap();
        // (x, y) → (h − y, x): y 19.5..64.5 at x 27.5..72.5 → x 191.5..236.5.
        assert_eq!(alpha(&ed, id, 200, 40), 1.0);
        assert_eq!(alpha(&ed, id, 191, 40), 0.5);
    }

    #[test]
    fn rasterize_and_path_conversions() {
        let mut ed = Editor::new(Document::new(100, 100));
        let square = VectorPath {
            subpaths: vec![
                SubPath {
                    nodes: vec![
                        PathNode::corner(10.0, 10.0),
                        PathNode::corner(90.0, 10.0),
                        PathNode::corner(90.0, 90.0),
                        PathNode::corner(10.0, 90.0),
                    ],
                    closed: true,
                },
                // A hole: even-odd across subpaths.
                SubPath {
                    nodes: vec![
                        PathNode::corner(40.0, 40.0),
                        PathNode::corner(60.0, 40.0),
                        PathNode::corner(60.0, 60.0),
                        PathNode::corner(40.0, 60.0),
                    ],
                    closed: true,
                },
            ],
        };
        let no_path = ed.execute(&ShapeFromWorkPath {
            fill: Some(Fill::Solid {
                color: [0.0, 1.0, 0.0],
            }),
            stroke: None,
            above: None,
        });
        assert!(no_path.is_err());
        ed.execute(&SetWorkPath {
            path: Some(square.clone()),
        })
        .unwrap();
        ed.execute(&ShapeFromWorkPath {
            fill: Some(Fill::Solid {
                color: [0.0, 1.0, 0.0],
            }),
            stroke: None,
            above: None,
        })
        .unwrap();
        let id = ed.doc().layers()[0].id;
        assert_eq!(ed.doc().layers()[0].name, "Shape");
        assert_eq!(alpha(&ed, id, 20, 20), 1.0);
        assert_eq!(alpha(&ed, id, 50, 50), 0.0, "hole");
        // Back to a work path: the same outline.
        ed.execute(&SetWorkPath { path: None }).unwrap();
        ed.execute(&ShapeToWorkPath { layer: id }).unwrap();
        assert_eq!(ed.doc().work_path.as_ref(), Some(&square));
        // Rasterize keeps the pixels, drops the vector.
        ed.execute(&RasterizeLayer { layer: id }).unwrap();
        let l = ed.doc().layer(id).unwrap();
        assert!(l.pixels().is_some() && l.shape_layer().is_none());
        assert_eq!(l.pixels().unwrap().get_pixel(20, 20).a, 1.0);
        assert_eq!(l.pixels().unwrap().get_pixel(50, 50).a, 0.0);
    }
}
