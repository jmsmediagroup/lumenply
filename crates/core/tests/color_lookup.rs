//! Color Lookup layers through the editor: added and edited by commands,
//! composited with exact expected colours (gamma-encoded), mixed by
//! opacity in linear light like every adjustment, undone.

use std::sync::Arc;

use lumenply_core::commands::{AddAdjustmentLayer, AddPixelLayer, SetAdjustment, SetOpacity};
use lumenply_core::Editor;
use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::lut::looks::Look;
use lumenply_doc::{Adjustment, Document, LayerContent, LayerId, Lut3D};
use lumenply_tiles::{Raster, Rgba};

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 3e-3
}

/// A 4×4 document of one opaque sRGB colour.
fn editor_with(srgb: [f32; 3]) -> Editor {
    let mut ed = Editor::new(Document::new(4, 4));
    let [r, g, b] = srgb.map(srgb_decode);
    let raster = Raster::filled(4, 4, Rgba::new(r, g, b, 1.0));
    ed.execute(&AddPixelLayer::from_raster("Background", raster, 0, 0))
        .unwrap();
    ed
}

/// The composite at (1, 1), gamma-encoded.
fn pixel(ed: &Editor) -> [f32; 3] {
    let p = lumenply_render::composite(ed.doc()).get_pixel(1, 1);
    let [r, g, b, _] = p.to_straight();
    [r, g, b].map(srgb_encode)
}

fn top_id(ed: &Editor) -> LayerId {
    ed.doc().layers().last().unwrap().id
}

fn lookup(lut: Lut3D, name: &str) -> Adjustment {
    Adjustment::ColorLookup {
        lut: Arc::new(lut),
        name: name.into(),
    }
}

#[test]
fn a_swap_lookup_swaps_and_mixes_by_opacity_in_linear_light() {
    let mut ed = editor_with([0.8, 0.4, 0.2]);
    ed.execute(&AddAdjustmentLayer::new(lookup(
        Lut3D::from_fn(17, |[r, g, b]| [b, g, r]),
        "Swap",
    )))
    .unwrap();
    let id = top_id(&ed);
    assert_eq!(ed.doc().layer(id).unwrap().name, "Color Lookup");
    let p = pixel(&ed);
    assert!(close(p[0], 0.2) && close(p[1], 0.4) && close(p[2], 0.8), "{p:?}");
    // 50%: halfway in linear light, sRGB (0.59993, 0.4, 0.59993).
    ed.execute(&SetOpacity {
        layer: id,
        opacity: 0.5,
    })
    .unwrap();
    let p = pixel(&ed);
    assert!(
        close(p[0], 0.59993) && close(p[1], 0.4) && close(p[2], 0.59993),
        "{p:?}"
    );
}

#[test]
fn choosing_a_look_recolours_and_undo_restores() {
    let mut ed = editor_with([0.5; 3]);
    ed.execute(&AddAdjustmentLayer::new(Adjustment::color_lookup_default()))
        .unwrap();
    let id = top_id(&ed);
    // The fresh lookup is the identity.
    let p = pixel(&ed);
    assert!(p.iter().all(|v| close(*v, 0.5)), "{p:?}");
    // Warm on mid grey lands on its formula's lattice value:
    // (0.537068, 0.510418, 0.457485).
    ed.execute(&SetAdjustment {
        layer: id,
        adjustment: lookup(Look::Warm.table(), "Warm"),
    })
    .unwrap();
    let p = pixel(&ed);
    assert!(
        close(p[0], 0.537068) && close(p[1], 0.510418) && close(p[2], 0.457485),
        "{p:?}"
    );
    // Monochrome Contrast greys it: S(0.5, 0.6) = 0.5.
    ed.execute(&SetAdjustment {
        layer: id,
        adjustment: lookup(Look::MonochromeContrast.table(), "Monochrome Contrast"),
    })
    .unwrap();
    assert!(pixel(&ed).iter().all(|v| close(*v, 0.5)));
    ed.undo();
    let p = pixel(&ed);
    assert!(close(p[0], 0.537068), "undo brings Warm back: {p:?}");
    let LayerContent::Adjustment(Adjustment::ColorLookup { name, .. }) = &ed.doc().layer(id).unwrap().content
    else {
        panic!("still a Color Lookup layer");
    };
    assert_eq!(name, "Warm");
    ed.undo();
    assert!(pixel(&ed).iter().all(|v| close(*v, 0.5)));
}
