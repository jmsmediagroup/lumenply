//! Tests for the Camera Raw Filter (`Filter::Develop`, run by
//! [`crate::develop::develop_in`]) in every place a filter renders: live
//! filter layers tile by tile, smart filters chunk by chunk, destructive
//! filters in one pass. All three must agree, so the reach (`Filter::pad`)
//! has to cover every local control.

#![cfg(test)]

use crate::filters::{apply_filter_in_canvas, filter_raster, Filter};
use lumenply_doc::{Develop, Document, SmartFilter, SmartFilters};
use lumenply_tiles::{Raster, Rect, Rgba, TileStore};

/// Checks, stripes and a ramp: edges across tile seams and near the border.
fn pattern(w: u32, h: u32) -> Raster {
    let mut base = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 37 + y * 11) % 97) as f32 / 97.0;
            let edge = if (x / 40 + y / 30) % 2 == 0 { 0.9 } else { 0.05 };
            base.set(
                x,
                y,
                Rgba::new(v * 0.5 + edge * 0.5, edge * 0.8, 1.0 - v * 0.7, 1.0),
            );
        }
    }
    base
}

/// Every local control on, plus some global ones.
fn busy(frame: [i32; 4]) -> Filter {
    Filter::Develop {
        settings: Develop {
            temperature: 12.0,
            exposure: 0.3,
            contrast: 20.0,
            highlights: -60.0,
            shadows: 70.0,
            texture: 40.0,
            clarity: 50.0,
            dehaze: 30.0,
            vibrance: 25.0,
            vignette: -50.0,
            vignette_midpoint: 30.0,
            ..Develop::NEUTRAL
        },
        frame,
    }
}

fn worst(a: &Raster, b: &Raster, (bx, by): (u32, u32)) -> f32 {
    let mut d = 0.0f32;
    for y in 0..a.height {
        for x in 0..a.width {
            let (p, q) = (a.get(x, y), b.get(x + bx, y + by));
            d = d
                .max((p.r - q.r).abs())
                .max((p.g - q.g).abs())
                .max((p.b - q.b).abs())
                .max((p.a - q.a).abs());
        }
    }
    d
}

#[test]
fn the_reach_covers_every_local_control() {
    // 600 px frame: tone radius 12, clarity 6, texture 2 → 3 · 12.
    assert_eq!(busy([0, 0, 600, 300]).pad(), 36);
    let only_texture = Filter::Develop {
        settings: Develop {
            texture: 10.0,
            ..Develop::NEUTRAL
        },
        frame: [0, 0, 600, 300],
    };
    assert_eq!(only_texture.pad(), 6);
    let global = Filter::Develop {
        settings: Develop {
            exposure: 1.0,
            vignette: -30.0,
            ..Develop::NEUTRAL
        },
        frame: [0, 0, 600, 300],
    };
    assert_eq!(
        global.pad(),
        0,
        "global controls and the vignette read no neighbours"
    );
    assert_eq!(busy([0, 0, 600, 300]).name(), "Camera Raw Filter");
}

/// A live filter layer renders tile by tile from padded pieces; it must
/// match one pass over the whole, edge-repeated canvas.
#[test]
fn camera_raw_filter_renders_seamlessly_across_tiles() {
    let (w, h) = (600u32, 300u32);
    let base = pattern(w, h);
    let mut doc = Document::new(w, h);
    let id = doc.add_pixel_layer("base");
    *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&base, 0, 0);
    let f = busy([0, 0, w as i32, h as i32]);
    doc.add_filter(f.clone());
    let tiled = crate::composite_raster(&doc);
    let p = f.pad();
    let mut padded = Raster::new(w + 2 * p as u32, h + 2 * p as u32);
    for y in 0..padded.height as i32 {
        for x in 0..padded.width as i32 {
            let sx = (x - p).clamp(0, w as i32 - 1) as u32;
            let sy = (y - p).clamp(0, h as i32 - 1) as u32;
            padded.set(x as u32, y as u32, base.get(sx, sy));
        }
    }
    let whole = filter_raster(&padded, &f, (-p, -p));
    let d = worst(&tiled, &whole, (p as u32, p as u32));
    assert!(d < 1e-4, "tiles differ from the whole by {d}");
    // And it did something.
    let changed = worst(&tiled, &base, (0, 0));
    assert!(changed > 0.1, "{changed}");
}

/// A smart filter renders in chunks of tiles; over a canvas wider than a
/// chunk it must equal the destructive filter, and so the bake.
#[test]
fn camera_raw_smart_filter_equals_the_destructive_filter() {
    let (w, h) = (1300u32, 400u32);
    let canvas = Rect::new(0, 0, w, h);
    let src = TileStore::from_raster(&pattern(w, h), 0, 0);
    let f = busy([0, 0, w as i32, h as i32]);
    let destructive = apply_filter_in_canvas(&src, &f, canvas);
    let sf = SmartFilters {
        filters: vec![SmartFilter::new(f)],
        ..SmartFilters::default()
    };
    let smart = crate::smart_filters::bake(&src, &sf, canvas, true);
    let (a, b) = (destructive.to_raster(canvas), smart.to_raster(canvas));
    let d = worst(&a, &b, (0, 0));
    assert!(d < 1e-4, "smart filter differs from the destructive one by {d}");
}

/// A layer that covers part of the canvas: transparent pixels neither
/// darken the local controls' neighbourhood nor gain colour.
#[test]
fn transparency_stays_out_of_the_local_controls() {
    let mut r = Raster::new(200, 100);
    for y in 0..100 {
        for x in 50..150 {
            r.set(x, y, Rgba::new(0.3, 0.3, 0.3, 1.0));
        }
    }
    let src = TileStore::from_raster(&r, 0, 0);
    let f = Filter::Develop {
        settings: Develop {
            shadows: 100.0,
            clarity: 100.0,
            ..Develop::NEUTRAL
        },
        frame: [0, 0, 200, 100],
    };
    let out = apply_filter_in_canvas(&src, &f, Rect::new(0, 0, 200, 100));
    // A flat 0.3 patch: clarity sees no detail, even at its border with
    // the transparent area, and 0.3 is too bright for the shadows slider.
    for x in [50, 51, 100, 149] {
        let p = out.get_pixel(x, 50);
        assert!(
            (p.r - 0.3).abs() < 1e-4 && (p.a - 1.0).abs() < 1e-6,
            "x {x}: {p:?}"
        );
    }
    assert_eq!(out.get_pixel(20, 50), Rgba::TRANSPARENT);
}

#[test]
fn documents_with_a_camera_raw_filter_composite_on_the_cpu() {
    let mut doc = Document::new(64, 64);
    doc.add_pixel_layer("base");
    doc.add_filter(busy([0, 0, 64, 64]));
    assert!(!crate::GpuCompositor::supports(&doc));
}

/// Run with `--ignored --nocapture`: develop timings on a 4000×3000 layer.
#[test]
#[ignore]
fn timing_on_a_12_megapixel_layer() {
    let (w, h) = (4000u32, 3000u32);
    let src = TileStore::from_raster(&pattern(w, h), 0, 0);
    let canvas = Rect::new(0, 0, w, h);
    let frame = [0, 0, w as i32, h as i32];
    for (name, d) in [
        (
            "exposure + vibrance",
            Develop {
                exposure: 0.5,
                vibrance: 30.0,
                ..Develop::NEUTRAL
            },
        ),
        (
            "shadows + highlights",
            Develop {
                shadows: 50.0,
                highlights: -40.0,
                ..Develop::NEUTRAL
            },
        ),
        ("every control", {
            let Filter::Develop { settings, .. } = busy(frame) else {
                unreachable!()
            };
            settings
        }),
    ] {
        let f = Filter::Develop { settings: d, frame };
        let t = std::time::Instant::now();
        let _ = apply_filter_in_canvas(&src, &f, canvas);
        println!("{name}: destructive {:?} (pad {})", t.elapsed(), f.pad());
        let sf = SmartFilters {
            filters: vec![SmartFilter::new(f)],
            ..SmartFilters::default()
        };
        let t = std::time::Instant::now();
        let _ = crate::smart_filters::bake(&src, &sf, canvas, false);
        println!("{name}: smart filter {:?}", t.elapsed());
    }
}
