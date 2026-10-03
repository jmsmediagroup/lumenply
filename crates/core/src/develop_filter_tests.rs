//! The Camera Raw Filter (`Filter::Develop`) through the existing filter
//! commands: destructive (`ApplyFilter`), smart (`AddSmartFilter`,
//! `SetSmartFilter`) and live (`AddFilterLayer`, `SetFilter`). Each is one
//! undo step with numeric results.

use lumenply_doc::{Develop, Document, Filter, LayerId, Selection, SmartFilter};
use lumenply_tiles::{Raster, Rect, Rgba, TileStore};

use crate::commands::{AddFilterLayer, ApplyFilter, SetFilter, SetSelection};
use crate::smart_filter_cmds::{AddSmartFilter, SetSmartFilter};
use crate::Editor;

/// A 200×100 document: one grey (0.2) pixel layer.
fn grey() -> (Editor, LayerId) {
    let mut doc = Document::new(200, 100);
    let id = doc.add_pixel_layer("Photo");
    let r = Raster::filled(200, 100, Rgba::new(0.2, 0.2, 0.2, 1.0));
    *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
    (Editor::new(doc), id)
}

fn develop(d: Develop) -> Filter {
    Filter::Develop {
        settings: d,
        frame: [0, 0, 200, 100],
    }
}

fn ev(e: f32) -> Develop {
    Develop {
        exposure: e,
        ..Develop::NEUTRAL
    }
}

fn shown(ed: &Editor, id: LayerId, x: i32, y: i32) -> f32 {
    ed.doc()
        .layer(id)
        .unwrap()
        .raster_store()
        .unwrap()
        .get_pixel(x, y)
        .r
}

#[test]
fn destructive_camera_raw_filter_is_one_step_and_respects_the_selection() {
    let (mut ed, id) = grey();
    ed.execute(&SetSelection {
        selection: Some(Selection::rect(Rect::new(0, 0, 100, 100))),
    })
    .unwrap();
    let steps = ed.history().len();
    ed.execute(&ApplyFilter {
        layer: id,
        filter: develop(ev(1.0)),
    })
    .unwrap();
    assert_eq!(ed.history().len(), steps + 1);
    assert_eq!(ed.history().last().copied(), Some("Camera Raw Filter"));
    assert!((shown(&ed, id, 50, 50) - 0.4).abs() < 1e-3, "inside doubled");
    assert!((shown(&ed, id, 150, 50) - 0.2).abs() < 1e-3, "outside untouched");
    ed.undo();
    assert!((shown(&ed, id, 50, 50) - 0.2).abs() < 1e-3);
}

#[test]
fn camera_raw_smart_filter_edits_in_place() {
    let (mut ed, id) = grey();
    ed.execute(&AddSmartFilter::new(id, develop(ev(1.0)))).unwrap();
    assert!((shown(&ed, id, 50, 50) - 0.4).abs() < 1e-3);
    assert!(
        (ed.doc()
            .layer(id)
            .unwrap()
            .content_store()
            .unwrap()
            .get_pixel(50, 50)
            .r
            - 0.2)
            .abs()
            < 1e-3,
        "the pixels stay"
    );
    let steps = ed.history().len();
    let shadows = Develop {
        shadows: 50.0,
        ..ev(-1.0)
    };
    ed.execute(&SetSmartFilter {
        layer: id,
        index: 0,
        filter: SmartFilter::new(develop(shadows)),
    })
    .unwrap();
    assert_eq!(ed.history().len(), steps + 1, "one step");
    // Flat 0.1 (−1 EV): the local base is log2(0.1) = −3.32 stops, so the
    // dark weight is (3.32 − 2) / 4 = 0.33 and Shadows +50 lifts
    // 0.5 · 2 · 0.33 = 0.33 stops: 0.1 · 2^0.33 = 0.1257.
    let v = shown(&ed, id, 100, 50);
    assert!((v - 0.1257).abs() < 2e-3, "{v}");
    ed.undo();
    assert!((shown(&ed, id, 100, 50) - 0.4).abs() < 1e-3);
}

#[test]
fn live_camera_raw_layer_develops_what_is_below() {
    let (mut ed, _) = grey();
    let live = ed.doc().next_id();
    ed.execute(&AddFilterLayer::new(develop(ev(1.0)))).unwrap();
    let p = lumenply_render::composite_raster(ed.doc()).get(100, 50);
    assert!((p.r - 0.4).abs() < 1e-3, "{p:?}");
    ed.execute(&SetFilter {
        layer: live,
        filter: develop(ev(2.0)),
    })
    .unwrap();
    let p = lumenply_render::composite_raster(ed.doc()).get(100, 50);
    assert!((p.r - 0.8).abs() < 1e-3, "{p:?}");
}

/// Run with `--ignored --nocapture`: the editor's cost around the filter
/// on a demo-sized (1800×1205) layer.
#[test]
#[ignore]
fn timing_through_the_editor() {
    let (w, h) = (1800u32, 1205u32);
    let mut doc = Document::new(w, h);
    let id = doc.add_pixel_layer("Photo");
    let mut r = Raster::new(w, h);
    for (i, p) in r.pixels.iter_mut().enumerate() {
        let v = (i % 251) as f32 / 251.0;
        *p = Rgba::new(v, v * 0.7, 0.3, 1.0);
    }
    *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
    let mut ed = Editor::new(doc);
    let f = Filter::Develop {
        settings: Develop {
            clarity: 100.0,
            texture: 60.0,
            dehaze: 40.0,
            ..Develop::NEUTRAL
        },
        frame: [0, 0, w as i32, h as i32],
    };
    let canvas = ed.doc().canvas();
    let store = ed.doc().layer(id).unwrap().pixels().unwrap().clone();
    let t = std::time::Instant::now();
    let _ = lumenply_render::apply_filter_in_canvas(&store, &f, canvas);
    println!("filter alone {:?}", t.elapsed());
    let t = std::time::Instant::now();
    ed.execute(&ApplyFilter { layer: id, filter: f }).unwrap();
    println!("through the editor {:?}", t.elapsed());
}
