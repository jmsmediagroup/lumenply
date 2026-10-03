//! Patterns through the file formats (ADR 0020): `.lumen` keeps the used
//! patterns as 16-bit PNGs; PSD writes them in the global `Patt` block and
//! references them from `PtFl` fills and the `patternFill` effect.

use std::path::PathBuf;

use lumenply_doc::{BlendMode, Document, Fill, Layer, LayerContent, Pattern, PatternOverlayFx};
use lumenply_tiles::{Raster, Rgba};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-patterns-{}-{name}", std::process::id()))
}

/// 3×2 pattern: red, green, blue over white, black, half-transparent grey.
fn rgb_pattern() -> Pattern {
    let mut r = Raster::new(3, 2);
    r.set(0, 0, Rgba::new(1.0, 0.0, 0.0, 1.0));
    r.set(1, 0, Rgba::new(0.0, 1.0, 0.0, 1.0));
    r.set(2, 0, Rgba::new(0.0, 0.0, 1.0, 1.0));
    r.set(0, 1, Rgba::WHITE);
    r.set(1, 1, Rgba::BLACK);
    r.set(2, 1, Rgba::from_straight(0.5, 0.5, 0.5, 0.5));
    Pattern::new("8f632baf-688d-1177-b2e5-b715d4e2a635", "RGB Test", r)
}

/// A pattern fill layer (offset, 100%) plus a pixel layer with a Pattern
/// Overlay at 50% Multiply, and an unused pattern that is not saved.
fn pattern_doc() -> Document {
    let mut doc = Document::new(40, 30);
    let p = rgb_pattern();
    doc.patterns.push(p.clone());
    doc.patterns
        .push(Pattern::new("unused", "Unused", Raster::new(2, 2)));
    let id = doc.alloc_id();
    let mut fill = Layer::fill(
        id,
        Fill::Pattern {
            pattern: p.reference(),
            scale: 1.0,
            offset: [1.0, 0.0],
            angle: 0.0,
        },
    );
    fill.name = "Tiles".into();
    doc.add_layer(fill);
    let id = doc.alloc_id();
    let mut px = Layer::pixel(id, "Box");
    for y in 5..15 {
        for x in 5..20 {
            px.pixels_mut().unwrap().set_pixel(x, y, Rgba::WHITE);
        }
    }
    let mut fx = PatternOverlayFx::new(p.reference());
    fx.opacity = 0.5;
    fx.scale = 2.0;
    fx.blend = BlendMode::Multiply;
    px.effects.pattern_overlay = Some(fx);
    doc.add_layer(px);
    lumenply_render::fill::refresh_stale(&mut doc);
    doc
}

fn close(a: Rgba, b: Rgba, tol: f32) -> bool {
    (a.r - b.r).abs() <= tol
        && (a.g - b.g).abs() <= tol
        && (a.b - b.b).abs() <= tol
        && (a.a - b.a).abs() <= tol
}

#[test]
fn projects_keep_used_patterns_and_their_references() {
    let doc = pattern_doc();
    let path = temp("roundtrip.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let back = lumenply_io::project::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(back.patterns.len(), 1, "only the used pattern is saved");
    let p = &back.patterns[0];
    assert_eq!(
        (p.id.as_str(), p.name.as_str()),
        ("8f632baf-688d-1177-b2e5-b715d4e2a635", "RGB Test")
    );
    for (a, b) in rgb_pattern().image.pixels.iter().zip(&p.image.pixels) {
        assert!(close(*a, *b, 1e-4), "16-bit: {a:?} vs {b:?}");
    }
    let l = &back.layers()[0];
    let LayerContent::Fill(f) = &l.content else {
        panic!("fill layer")
    };
    assert!(matches!(&f.fill, Fill::Pattern { offset, .. } if *offset == [1.0, 0.0]));
    // The cache rendered from the loaded pattern: canvas x 1 is the
    // pattern's first column (red), x 0 its last (blue).
    let cache = f.cache.as_ref().unwrap();
    assert!(close(cache.get_pixel(1, 0), Rgba::new(1.0, 0.0, 0.0, 1.0), 1e-3));
    assert!(close(cache.get_pixel(0, 0), Rgba::new(0.0, 0.0, 1.0, 1.0), 1e-3));
    let po = back.layers()[1].effects.pattern_overlay.as_ref().unwrap();
    assert_eq!((po.scale, po.opacity, po.blend), (2.0, 0.5, BlendMode::Multiply));
    assert!(po.pattern.image.is_some(), "resolved on load");
    let a = lumenply_render::composite_raster(&doc);
    let b = lumenply_render::composite_raster(&back);
    for (x, y) in [(0, 0), (4, 1), (10, 10), (19, 14)] {
        assert!(
            close(a.get(x, y), b.get(x, y), 2e-3),
            "({x},{y}) {:?} {:?}",
            a.get(x, y),
            b.get(x, y)
        );
    }
}

#[test]
fn psd_carries_patterns_fills_and_overlays_both_ways() {
    let doc = pattern_doc();
    let path = temp("roundtrip.psd");
    lumenply_io::psd::save(&path, &doc).unwrap();
    let back = lumenply_io::psd::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        !back.warnings.iter().any(|w| w.contains("pattern")),
        "{:?}",
        back.warnings
    );
    let back = back.value;
    assert_eq!(back.patterns.len(), 1);
    let p = &back.patterns[0];
    assert_eq!(p.name, "RGB Test");
    assert_eq!((p.width(), p.height()), (3, 2));
    // 8-bit in PSD: the half-transparent grey keeps its alpha within a step.
    let t = p.image.get(2, 1);
    assert!((t.a - 0.502).abs() < 3e-3, "{t:?}");
    assert_eq!(back.layers()[0].name, "Tiles");
    let LayerContent::Fill(f) = &back.layers()[0].content else {
        panic!("the pattern fill came back as another kind")
    };
    let Fill::Pattern {
        pattern,
        scale,
        offset,
        ..
    } = &f.fill
    else {
        panic!("not a pattern fill")
    };
    assert_eq!(
        (pattern.id.as_str(), *scale, *offset),
        (p.id.as_str(), 1.0, [1.0, 0.0])
    );
    let po = back.layers()[1].effects.pattern_overlay.as_ref().unwrap();
    assert_eq!((po.scale, po.opacity, po.blend), (2.0, 0.5, BlendMode::Multiply));
    let a = lumenply_render::composite_raster(&doc);
    let b = lumenply_render::composite_raster(&back);
    for (x, y) in [(0, 0), (4, 1), (10, 10), (19, 14)] {
        assert!(
            close(a.get(x, y), b.get(x, y), 1e-2),
            "({x},{y}) {:?} {:?}",
            a.get(x, y),
            b.get(x, y)
        );
    }
}
