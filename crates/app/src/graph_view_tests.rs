//! The canvas renders through the edit graph (ADR 0025, stage 3b): after
//! every edit the pixels on screen must be what the reference compositor
//! makes of the document, and a preview's composite must be the reference
//! composite of the previewed document.

use super::*;
use lumenply_doc::{LayerEffects, Mask, ShadowFx};
use lumenply_tiles::{Rgba, TileStore};

/// A deterministic texture, so no two tiles are alike.
fn texture(w: u32, h: u32, seed: u32, alpha: f32) -> Raster {
    let mut r = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let f = |k: u32| ((x * 3 + y * 5 + k * 61 + seed * 17) % 256) as f32 / 255.0;
            r.pixels[(y * w + x) as usize] = Rgba::from_straight(f(1), f(2) * 0.8, f(3) * 0.6 + 0.2, alpha);
        }
    }
    r
}

/// A 640 × 480 document with most of what the compositor knows: a masked
/// multiply layer, an adjustment, a group holding a clip chain, a live
/// filter, and a plain layer on top. Returns the app and the layer ids by
/// name.
fn busy_app() -> (App, HashMap<&'static str, LayerId>) {
    let (w, h) = (640, 480);
    let mut doc = Document::new(w, h);
    let mut ids = HashMap::new();
    let pixel = |doc: &mut Document, name: &'static str, r: &Raster, x: i32, y: i32| {
        let id = doc.add_pixel_layer(name);
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(r, x, y);
        id
    };
    ids.insert("bg", pixel(&mut doc, "bg", &texture(w, h, 1, 1.0), 0, 0));
    let mul = pixel(&mut doc, "mul", &texture(400, 300, 2, 0.8), 100, 60);
    ids.insert("mul", mul);
    {
        let l = doc.layer_mut(mul).unwrap();
        l.blend = BlendMode::Multiply;
        l.opacity = 0.7;
        let mut m = Mask::reveal_all();
        lumenply_render::gradient_mask(&mut m, Rect::new(100, 60, 400, 300));
        l.mask = Some(m);
    }
    let adj = doc.add_adjustment(Adjustment::HueSaturation {
        hue: 12.0,
        saturation: 0.3,
        lightness: 0.0,
        colorize: false,
    });
    ids.insert("adj", adj);
    let g = doc.add_group("group");
    ids.insert("group", g);
    for (i, (name, clip)) in [("base", false), ("clipped", true)].into_iter().enumerate() {
        let id = doc.alloc_id();
        let mut l = Layer::pixel(id, name);
        *l.pixels_mut().unwrap() = TileStore::from_raster(&texture(260, 200, 3 + i as u32, 0.9), 300, 200);
        l.clip = clip;
        l.blend = if clip {
            BlendMode::Screen
        } else {
            BlendMode::Normal
        };
        doc.layer_mut(g).unwrap().children_mut().unwrap().push(l);
        ids.insert(name, id);
    }
    ids.insert("top", pixel(&mut doc, "top", &texture(120, 90, 9, 1.0), 20, 380));
    lumenply_core::compact_storage(&mut doc);
    let mut app = crate::a11y_tests::launch(&[]);
    app.open_in_new_tab(Editor::new(doc), None);
    app.dialog = None;
    (app, ids)
}

/// Redraw what the last edit marked, then compare the whole canvas with
/// the reference composite of the document, and the histogram with the
/// canvas's.
fn check(app: &mut App, ctx: &egui::Context, what: &str) {
    if app.dirty {
        app.refresh(ctx);
    }
    let reference = lumenply_render::composite_raster(app.editor.doc());
    let shown = app.last_flat.as_ref().expect("a composite is shown");
    assert_eq!((shown.width, shown.height), (reference.width, reference.height));
    let mut worst = 0f32;
    for (p, q) in shown.pixels.iter().zip(&reference.pixels) {
        for (a, b) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
            worst = worst.max((a - b).abs());
        }
    }
    assert!(
        worst == 0.0,
        "{what}: the canvas differs from the reference by {worst}"
    );
    assert_eq!(
        app.histogram,
        histogram::luminance_histogram(shown),
        "{what}: the histogram follows the canvas"
    );
}

fn stroke(layer: LayerId, mode: BrushMode, from: (f32, f32), to: (f32, f32)) -> PaintStroke {
    PaintStroke {
        layer,
        brush: Brush {
            radius: 18.0,
            hardness: 0.6,
            color: [0.9, 0.3, 0.1, 1.0],
            spacing: 0.15,
            mode,
            ..Brush::default()
        },
        points: (0..12)
            .map(|i| {
                let t = i as f32 / 11.0;
                StrokePoint::new(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, 1.0)
            })
            .collect(),
    }
}

#[test]
fn every_edit_shows_what_the_reference_compositor_makes() {
    let (mut app, ids) = busy_app();
    let ctx = crate::a11y_tests::ctx();
    check(&mut app, &ctx, "opened");

    app.run(&stroke(
        ids["mul"],
        BrushMode::Paint,
        (120.0, 80.0),
        (380.0, 300.0),
    ));
    check(&mut app, &ctx, "paint on the masked multiply layer");
    app.run(&stroke(ids["bg"], BrushMode::Erase, (30.0, 30.0), (200.0, 150.0)));
    check(&mut app, &ctx, "erase the background");
    for (i, o) in [0.8, 0.5, 0.2].into_iter().enumerate() {
        app.run_coalescing(
            &SetOpacity {
                layer: ids["adj"],
                opacity: o,
            },
            "opacity",
        );
        check(&mut app, &ctx, &format!("opacity drag tick {i}"));
    }
    app.editor.end_coalescing();
    app.run(&SetVisible {
        layer: ids["mul"],
        visible: false,
    });
    check(&mut app, &ctx, "hide");
    app.undo();
    check(&mut app, &ctx, "undo the hide");
    app.undo();
    check(&mut app, &ctx, "undo the opacity drag");
    app.redo();
    check(&mut app, &ctx, "redo the opacity drag");
    app.run(&MoveLayer {
        layer: ids["clipped"],
        dx: -40,
        dy: 25,
    });
    check(&mut app, &ctx, "move a clipped layer inside the group");
    app.run(&stroke(
        ids["base"],
        BrushMode::Dodge,
        (310.0, 210.0),
        (540.0, 380.0),
    ));
    check(&mut app, &ctx, "dodge the clip base");
    app.run(&SetBlendMode {
        layer: ids["group"],
        blend: BlendMode::Overlay,
    });
    check(&mut app, &ctx, "group blend mode");
    app.run(&AddFilterLayer::new(Filter::BoxBlur { radius: 3.0 }));
    check(&mut app, &ctx, "add a live blur on top");
    app.run(&AddPixelLayer::new("paint"));
    let paint = app.editor.doc().layers().last().unwrap().id;
    app.run(&stroke(paint, BrushMode::Paint, (50.0, 400.0), (600.0, 40.0)));
    check(&mut app, &ctx, "paint a new layer over the filter");
    app.run(&SetLayerEffects {
        layer: paint,
        effects: LayerEffects {
            drop_shadow: Some(ShadowFx {
                dx: 4.0,
                dy: 6.0,
                blur: 8.0,
                opacity: 0.6,
                ..ShadowFx::default()
            }),
            ..LayerEffects::default()
        },
    });
    check(&mut app, &ctx, "drop shadow on the painted layer");
    app.editor.jump_to(2);
    app.mark(app.editor.last_affected());
    check(&mut app, &ctx, "jump back in history");
    app.editor.jump_to(usize::MAX);
    app.mark(app.editor.last_affected());
    check(&mut app, &ctx, "jump forward again");
    // A full redraw of an unchanged version comes from the cache, and is
    // still the reference.
    app.mark(None);
    check(&mut app, &ctx, "redraw");
}

#[test]
fn previews_composite_the_previewed_document() {
    let (mut app, ids) = busy_app();
    let ctx = crate::a11y_tests::ctx();
    check(&mut app, &ctx, "opened");
    let canvas = app.editor.doc().canvas();
    let compare = |app: &App, doc: &Document, what: &str| {
        let ours = flatten(&app.preview_composite(doc, canvas), canvas);
        let reference = lumenply_render::composite_raster(doc);
        let worst = ours
            .pixels
            .iter()
            .zip(&reference.pixels)
            .flat_map(|(p, q)| {
                [
                    (p.r - q.r).abs(),
                    (p.g - q.g).abs(),
                    (p.b - q.b).abs(),
                    (p.a - q.a).abs(),
                ]
            })
            .fold(0f32, f32::max);
        assert!(
            worst == 0.0,
            "{what}: the preview differs from the reference by {worst}"
        );
    };
    // A stroke in progress on each kind of layer: mid-stack, the bottom
    // (nothing below), inside a group (split at the group), and with a
    // live filter above (the reference path).
    for (name, what) in [
        ("mul", "mid-stack"),
        ("bg", "bottom layer"),
        ("clipped", "clipped layer in a group"),
    ] {
        app.active = Some(ids[name]);
        let mut doc = app.editor.doc().clone();
        stroke(ids[name], BrushMode::Paint, (100.0, 100.0), (500.0, 400.0))
            .apply(&mut doc)
            .unwrap();
        compare(&app, &doc, what);
    }
    app.run(&AddFilterLayer::new(Filter::BoxBlur { radius: 2.0 }));
    app.refresh(&ctx);
    app.active = Some(ids["mul"]);
    let mut doc = app.editor.doc().clone();
    stroke(ids["mul"], BrushMode::Paint, (100.0, 100.0), (500.0, 400.0))
        .apply(&mut doc)
        .unwrap();
    compare(&app, &doc, "a live filter above");
    // A move preview, as the Move tool shows it.
    app.active = Some(ids["top"]);
    let mut doc = app.editor.doc().clone();
    MoveLayer {
        layer: ids["top"],
        dx: 200,
        dy: -150,
    }
    .apply(&mut doc)
    .unwrap();
    compare(&app, &doc, "a move");
}
