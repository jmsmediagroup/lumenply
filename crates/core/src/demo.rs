//! A ready-made demo document used by the CLI and the desktop app: paper,
//! a feathered glow, a pressure-sensitive ink stroke, a masked hue shift and
//! a curves layer. It exercises most of the engine in one go.

use nge_doc::{Adjustment, Document, Mask, Selection};
use nge_tiles::{Raster, Rect, Rgba};

use crate::commands::{
    AddAdjustmentLayer, AddPixelLayer, Brush, BrushMode, FeatherSelection, Fill, PaintStroke, SetMask,
    SetSelection, StrokePoint,
};
use crate::{EditResult, Editor};

pub fn build(width: u32, height: u32) -> EditResult<Editor> {
    let mut ed = Editor::new(Document::new(width, height));
    let paper = Raster::filled(width, height, Rgba::from_straight(0.95, 0.93, 0.88, 1.0));
    ed.execute(&AddPixelLayer::from_raster("Paper", paper, 0, 0))?;

    // A soft glow: select an ellipse, feather it, fill a layer, deselect.
    ed.execute(&AddPixelLayer::new("Glow"))?;
    let glow = ed.doc().layers()[1].id;
    let (w, h) = (width as i32, height as i32);
    ed.execute(&SetSelection {
        selection: Some(Selection::ellipse(Rect::new(
            w / 4,
            h / 4,
            (w / 2) as u32,
            (h / 2) as u32,
        ))),
    })?;
    ed.execute(&FeatherSelection {
        radius: width as f32 * 0.04,
    })?;
    ed.execute(&Fill {
        layer: glow,
        color: [1.0, 0.85, 0.3, 0.8],
    })?;
    ed.execute(&SetSelection { selection: None })?;

    ed.execute(&AddPixelLayer::new("Ink"))?;
    let ink = ed.doc().layers()[2].id;

    // A sine wave whose pressure swells in the middle.
    let n = 200;
    let points: Vec<StrokePoint> = (0..=n)
        .map(|i| {
            let f = i as f32 / n as f32;
            let x = width as f32 * (0.08 + 0.84 * f);
            let y = height as f32 * (0.5 + 0.3 * (f * std::f32::consts::TAU * 1.5).sin());
            StrokePoint::new(x, y, (f * std::f32::consts::PI).sin().max(0.05))
        })
        .collect();
    ed.execute(&PaintStroke {
        layer: ink,
        brush: Brush {
            radius: width as f32 * 0.02,
            hardness: 0.6,
            color: [0.05, 0.1, 0.4, 1.0],
            spacing: 0.15,
            jitter: 0.0,
            mode: BrushMode::Paint,
        },
        points,
    })?;

    // A hue-rotating adjustment, masked by a left-to-right gradient, so the
    // ink shifts from blue to rust while staying fully editable.
    ed.execute(&AddAdjustmentLayer::new(Adjustment::HueSaturation {
        hue: 150.0,
        saturation: 0.3,
        lightness: 0.0,
    }))?;
    let adj = ed.doc().layers()[3].id;
    let mut mask = Mask::hide_all();
    nge_render::gradient_mask(&mut mask, Rect::new(0, 0, width, height));
    ed.execute(&SetMask {
        layer: adj,
        mask: Some(mask),
    })?;

    // A gentle S-curve on top of everything, for contrast.
    ed.execute(&AddAdjustmentLayer::new(Adjustment::Curves {
        points: vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]],
    }))?;
    Ok(ed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_builds_the_expected_stack() {
        let ed = build(300, 200).unwrap();
        let names: Vec<&str> = ed.doc().layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Paper", "Glow", "Ink", "Hue/Saturation", "Curves"]);
        assert!(ed.doc().selection.is_none());
        assert_eq!(ed.history().len(), 11);
    }
}
