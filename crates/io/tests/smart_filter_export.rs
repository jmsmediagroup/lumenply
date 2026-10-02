//! Smart filters through PSD and OpenRaster: both bake the filtered pixels
//! (PSD smart filters are out of scope, ADR 0011) and say so in a warning.

use std::path::PathBuf;

use lumenply_doc::{Document, Filter, SmartFilter};
use lumenply_tiles::{Raster, Rgba, TileStore};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-sf-{}-{name}", std::process::id()))
}

/// 64×32: one layer, white left of x = 32 and black from there, under a
/// 5-px box blur smart filter.
fn blurred_step() -> Document {
    let mut doc = Document::new(64, 32);
    let id = doc.add_pixel_layer("Step");
    let mut r = Raster::new(64, 32);
    for y in 0..32 {
        for x in 0..64 {
            let v = if x < 32 { 1.0 } else { 0.0 };
            r.set(x, y, Rgba::new(v, v, v, 1.0));
        }
    }
    let l = doc.layer_mut(id).unwrap();
    *l.pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
    l.smart_filters
        .filters
        .push(SmartFilter::new(Filter::BoxBlur { radius: 2.0 }));
    lumenply_render::fill::refresh_stale(&mut doc);
    doc
}

#[test]
fn psd_export_bakes_smart_filters_with_a_warning() {
    let doc = blurred_step();
    let path = temp("bake.psd");
    let rep = lumenply_io::psd::save(&path, &doc).unwrap();
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.contains("smart filters were baked")),
        "{:?}",
        rep.warnings
    );
    let back = lumenply_io::psd::load(&path).unwrap().value;
    std::fs::remove_file(&path).ok();
    let l = &back.layers()[0];
    assert!(l.smart_filters.is_empty());
    // 0.4 linear survives 8-bit sRGB storage within half a level.
    let p = l.pixels().unwrap().get_pixel(32, 16);
    assert!((p.r - 0.4).abs() < 0.004, "{p:?}");
    assert_eq!(l.pixels().unwrap().get_pixel(5, 16).r, 1.0);
}

#[test]
fn ora_export_bakes_smart_filters_with_a_warning() {
    let doc = blurred_step();
    let path = temp("bake.ora");
    let rep = lumenply_io::ora::save(&path, &doc).unwrap();
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.contains("smart filters were baked")),
        "{:?}",
        rep.warnings
    );
    let back = lumenply_io::ora::load(&path).unwrap().value;
    std::fs::remove_file(&path).ok();
    let p = back.layers()[0].pixels().unwrap().get_pixel(33, 16);
    // One white pixel of five: 0.2 linear, within 8-bit rounding.
    assert!((p.r - 0.2).abs() < 0.004, "{p:?}");
}
