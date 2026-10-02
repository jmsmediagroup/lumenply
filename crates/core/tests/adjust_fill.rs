//! Gradient Map, Channel Mixer, Photo Filter and Selective Color layers
//! through the editor: added and edited by commands, composited with exact
//! expected colours, undone.

use lumenply_core::commands::{AddAdjustmentLayer, AddPixelLayer, SetAdjustment};
use lumenply_core::Editor;
use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::{Adjustment, Document, Gradient, LayerContent, LayerId};
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

#[test]
fn gradient_map_layer_maps_mid_grey_halfway_along_the_ramp() {
    let mut ed = editor_with([0.5; 3]);
    ed.execute(&AddAdjustmentLayer::new(Adjustment::GradientMap {
        gradient: Gradient::two([0.0; 3], [1.0, 0.0, 0.0]),
        reverse: false,
    }))
    .unwrap();
    // Black → red at gamma luminance 0.5: sRGB (0.5, 0, 0).
    let p = pixel(&ed);
    assert!(close(p[0], 0.5) && close(p[1], 0.0) && close(p[2], 0.0), "{p:?}");
    let id = top_id(&ed);
    assert_eq!(ed.doc().layer(id).unwrap().name, "Gradient Map");

    // Editing the ramp (white → blue) recolours; undo restores.
    ed.execute(&SetAdjustment {
        layer: id,
        adjustment: Adjustment::GradientMap {
            gradient: Gradient::two([1.0; 3], [0.0, 0.0, 1.0]),
            reverse: false,
        },
    })
    .unwrap();
    let p = pixel(&ed);
    assert!(close(p[0], 0.5) && close(p[1], 0.5) && close(p[2], 1.0), "{p:?}");
    ed.undo();
    assert!(close(pixel(&ed)[0], 0.5) && close(pixel(&ed)[2], 0.0));
    ed.undo();
    assert!(pixel(&ed).iter().all(|v| close(*v, 0.5)), "back to grey");
}

#[test]
fn mixer_filter_and_selective_layers_composite_exact_colours() {
    // Channel Mixer, monochrome 50/50/0 + 10% on (0.2, 0.6, 0.9) → 0.5.
    let mut ed = editor_with([0.2, 0.6, 0.9]);
    ed.execute(&AddAdjustmentLayer::new(Adjustment::ChannelMixer {
        red: [1.0, 0.0, 0.0, 0.0],
        green: [0.0, 1.0, 0.0, 0.0],
        blue: [0.0, 0.0, 1.0, 0.0],
        monochrome: true,
        gray: [0.5, 0.5, 0.0, 0.1],
    }))
    .unwrap();
    let p = pixel(&ed);
    assert!(p.iter().all(|v| close(*v, 0.5)), "{p:?}");

    // Photo Filter sRGB (1, 0.5, 0) at 50% on mid grey, luminosity kept.
    let mut ed = editor_with([0.5; 3]);
    ed.execute(&AddAdjustmentLayer::new(Adjustment::PhotoFilter {
        color: [1.0, srgb_decode(0.5), 0.0],
        density: 0.5,
        preserve_luminosity: true,
    }))
    .unwrap();
    let p = pixel(&ed);
    assert!(
        close(p[0], 0.60745) && close(p[1], 0.48245) && close(p[2], 0.35745),
        "{p:?}"
    );

    // Selective Color: Neutrals +50% black, absolute, halves mid grey.
    let mut ed = editor_with([0.5; 3]);
    let mut colors = [[0.0; 4]; 9];
    colors[7] = [0.0, 0.0, 0.0, 0.5];
    ed.execute(&AddAdjustmentLayer::new(Adjustment::SelectiveColor {
        colors,
        absolute: true,
    }))
    .unwrap();
    let p = pixel(&ed);
    assert!(p.iter().all(|v| close(*v, 0.25)), "{p:?}");
    // At 50% layer opacity the change is half as strong... in gamma terms
    // the mix runs on linear values, so check the linear midpoint.
    let id = top_id(&ed);
    ed.execute(&lumenply_core::commands::SetOpacity {
        layer: id,
        opacity: 0.5,
    })
    .unwrap();
    let lin = lumenply_render::composite(ed.doc()).get_pixel(1, 1).r;
    assert!(close(lin, (srgb_decode(0.5) + srgb_decode(0.25)) / 2.0), "{lin}");
    assert!(matches!(
        ed.doc().layer(id).unwrap().content,
        LayerContent::Adjustment(Adjustment::SelectiveColor { .. })
    ));
}
