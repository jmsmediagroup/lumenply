//! Gradient Map, Channel Mixer, Photo Filter and Selective Color layers
//! through the editor: added and edited by commands, composited with exact
//! expected colours, undone.

use lumenply_core::commands::{
    AddAdjustmentLayer, AddFillLayer, AddPixelLayer, RasterizeLayer, ResizeCanvas, SetAdjustment,
    SetBlendMode, SetClipped, SetFill, SetOpacity, SetSelection,
};
use lumenply_core::Editor;
use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::{
    Adjustment, BlendMode, Document, Fill, Gradient, GradientStyle, LayerContent, LayerId, Selection,
};
use lumenply_tiles::{Raster, Rect, Rgba};

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

// ---- Fill layers -----------------------------------------------------------------

fn same(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// Straight gamma colour and alpha of the composite at `(x, y)`.
fn at(ed: &Editor, x: i32, y: i32) -> [f32; 4] {
    let [r, g, b, a] = lumenply_render::composite(ed.doc()).get_pixel(x, y).to_straight();
    [srgb_encode(r), srgb_encode(g), srgb_encode(b), a]
}

#[test]
fn solid_fill_covers_the_canvas_and_takes_the_selection_as_mask() {
    let mut ed = editor_with([0.5; 3]);
    ed.execute(&SetSelection {
        selection: Some(Selection::rect(Rect::new(0, 0, 2, 4))),
    })
    .unwrap();
    let red = Fill::Solid {
        color: [1.0, 0.0, 0.0],
    };
    ed.execute(&AddFillLayer::new(red)).unwrap();
    let id = top_id(&ed);
    let l = ed.doc().layer(id).unwrap();
    assert_eq!(l.name, "Color Fill");
    assert!(l.mask.is_some(), "the selection became the mask");
    // Inside the selection: red; outside: the grey below.
    assert!(same(at(&ed, 1, 1), [1.0, 0.0, 0.0, 1.0]), "{:?}", at(&ed, 1, 1));
    let out = at(&ed, 3, 1);
    assert!(close(out[0], 0.5) && close(out[1], 0.5), "{out:?}");

    // Multiply at 50%: red × grey is (grey, 0, 0) in linear light, mixed
    // halfway with the grey itself.
    ed.execute(&SetBlendMode {
        layer: id,
        blend: BlendMode::Multiply,
    })
    .unwrap();
    ed.execute(&SetOpacity {
        layer: id,
        opacity: 0.5,
    })
    .unwrap();
    let grey = srgb_decode(0.5);
    let p = lumenply_render::composite(ed.doc()).get_pixel(1, 1);
    assert!(close(p.r, grey) && close(p.g, grey / 2.0), "{p:?}");

    // Editing the colour re-renders; undo brings the red back.
    ed.execute(&SetFill {
        layer: id,
        fill: Fill::Solid {
            color: [0.0, 0.0, 1.0],
        },
    })
    .unwrap();
    let p = lumenply_render::composite(ed.doc()).get_pixel(1, 1);
    assert!(close(p.b, grey) && close(p.r, grey / 2.0), "{p:?}");
    ed.undo();
    let p = lumenply_render::composite(ed.doc()).get_pixel(1, 1);
    assert!(close(p.r, grey) && close(p.b, grey / 2.0), "{p:?}");
}

#[test]
fn gradient_fill_spans_the_canvas_and_follows_canvas_changes() {
    let mut ed = Editor::new(Document::new(100, 10));
    ed.execute(&AddFillLayer::new(Fill::Gradient {
        gradient: Gradient::default(),
        style: GradientStyle::Linear,
        angle: 0.0,
        scale: 1.0,
        reverse: false,
        offset: [0.0, 0.0],
    }))
    .unwrap();
    // Black on the left, white on the right, sRGB 0.495 at x = 49.
    assert!(at(&ed, 0, 5)[0] < 0.01);
    assert!(close(at(&ed, 49, 5)[0], 0.495), "{:?}", at(&ed, 49, 5));
    assert!(at(&ed, 99, 5)[0] > 0.99);
    // A wider canvas re-spans the ramp: x = 99 is now the middle.
    ed.execute(&ResizeCanvas {
        width: 200,
        height: 10,
        anchor: (0.0, 0.0),
    })
    .unwrap();
    let f = ed.doc().layers()[0].fill_layer().unwrap();
    assert_eq!(f.cache_canvas, (200, 10));
    assert!(close(at(&ed, 99, 5)[0], 0.4975), "{:?}", at(&ed, 99, 5));
    assert!(at(&ed, 199, 5)[0] > 0.99);

    // Rasterizing keeps exactly the rendered pixels.
    let before = lumenply_render::composite(ed.doc());
    let id = ed.doc().layers()[0].id;
    ed.execute(&RasterizeLayer { layer: id }).unwrap();
    assert!(ed.doc().layers()[0].pixels().is_some());
    let after = lumenply_render::composite(ed.doc());
    for x in [0, 50, 120, 199] {
        assert_eq!(before.get_pixel(x, 3), after.get_pixel(x, 3), "x = {x}");
    }
}

#[test]
fn moving_a_gradient_fill_shifts_its_centre_and_its_mask() {
    let mut ed = Editor::new(Document::new(100, 10));
    ed.execute(&SetSelection {
        selection: Some(Selection::rect(Rect::new(0, 0, 20, 10))),
    })
    .unwrap();
    ed.execute(&AddFillLayer::new(Fill::Gradient {
        gradient: Gradient::default(),
        style: GradientStyle::Linear,
        angle: 0.0,
        scale: 1.0,
        reverse: false,
        offset: [0.0, 0.0],
    }))
    .unwrap();
    let id = top_id(&ed);
    ed.execute(&lumenply_core::commands::MoveLayer {
        layer: id,
        dx: 50,
        dy: 0,
    })
    .unwrap();
    let l = ed.doc().layer(id).unwrap();
    assert!(matches!(
        l.fill_layer().unwrap().fill,
        Fill::Gradient { offset: [o, 0.0], .. } if (o - 0.5).abs() < 1e-6
    ));
    // The ramp's centre moved to x = 100: x = 99 is just below mid grey.
    let p = l.raster_store().unwrap().get_pixel(99, 5);
    assert!(close(srgb_encode(p.r), 0.495), "{p:?}");
    // The mask moved with it: 50..70 shows, 0..20 no longer does.
    let m = l.mask.as_ref().unwrap();
    assert_eq!((m.value(10, 5), m.value(60, 5)), (0.0, 1.0));
    // Scaling a fill asks for a rasterize instead.
    let scale = lumenply_core::commands::TransformLayer {
        layer: id,
        transform: lumenply_tiles::Affine::scale(2.0, 2.0),
    };
    assert!(ed.execute(&scale).is_err());
}

#[test]
fn a_solid_fill_costs_one_tile_of_history() {
    // 1024² is 16 tiles, all interior: one shared 16-bit tile of 512 KiB.
    let mut ed = Editor::new(Document::new(1024, 1024));
    let mut add = AddFillLayer::new(Fill::Solid {
        color: [1.0, 0.0, 0.0],
    });
    add.mask_selection = false;
    ed.execute(&add).unwrap();
    assert_eq!(ed.history_bytes(), 0, "nothing was replaced");
    let id = top_id(&ed);
    ed.execute(&SetFill {
        layer: id,
        fill: Fill::Solid {
            color: [0.0, 0.0, 1.0],
        },
    })
    .unwrap();
    assert_eq!(ed.history_bytes(), 256 * 256 * 4 * 2, "the old colour's one tile");
}

#[test]
fn fill_clipped_to_a_layer_paints_only_inside_it() {
    // Base: opaque pixels on the left half only.
    let mut ed = Editor::new(Document::new(4, 4));
    let raster = Raster::filled(2, 4, Rgba::new(1.0, 1.0, 1.0, 1.0));
    ed.execute(&AddPixelLayer::from_raster("Base", raster, 0, 0))
        .unwrap();
    let mut add = AddFillLayer::new(Fill::Solid {
        color: [0.0, 1.0, 0.0],
    });
    add.mask_selection = false;
    ed.execute(&add).unwrap();
    let id = top_id(&ed);
    ed.execute(&SetClipped {
        layer: id,
        clip: true,
    })
    .unwrap();
    assert!(same(at(&ed, 1, 1), [0.0, 1.0, 0.0, 1.0]), "{:?}", at(&ed, 1, 1));
    assert_eq!(at(&ed, 3, 1)[3], 0.0, "nothing outside the base");
}
