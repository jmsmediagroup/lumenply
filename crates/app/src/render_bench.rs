//! Interactive redraw timings through the app's own paths: what a user
//! waits for between an edit and the screen (ADR 0025, stage 3b). Not a
//! correctness test, so ignored by default:
//!
//! ```text
//! cargo test --release -p lumenply-app render_bench -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `LUMENPLY_BENCH=demo` or `=big` runs one document. Every scenario runs
//! the edit through `App::run` (or a drag's `run_coalescing`, or a stroke's
//! per-frame `preview`) and then `App::refresh`, as a frame does, and
//! reports the median.

use super::*;
use std::time::{Duration, Instant};

use lumenply_tiles::{Rgba, TileStore};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// A deterministic texture: smooth gradients plus a little hash noise, so
/// no two tiles are alike.
fn texture(w: u32, h: u32, seed: u32, alpha: f32) -> Raster {
    let mut r = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let n = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ seed.wrapping_mul(83492791)) % 64;
            let f = |k: u32| ((x + 3 * y + k * 97 + seed * 31) % 1024) as f32 / 1024.0;
            r.pixels[(y * w + x) as usize] = Rgba::from_straight(
                (f(1) * 0.8 + n as f32 / 640.0).min(1.0),
                (f(2) * 0.7 + 0.1).min(1.0),
                (f(3) * 0.6 + 0.2).min(1.0),
                alpha,
            );
        }
    }
    r
}

/// Thirty layers on a 4000 × 3000 canvas: four full-canvas textures (one
/// multiply, one screen), twenty partial patches at varied opacity and
/// blend, five adjustments (one masked) and a group of three, at rest in
/// 16 bits as an edited project is. Returns the editor and a pixel layer
/// in the middle of the stack.
fn big_doc() -> (Editor, LayerId) {
    let (w, h) = (4000u32, 3000u32);
    let mut doc = Document::new(w, h);
    let mut mid = 0;
    let modes = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
    ];
    let mut n = 0u32;
    let pixel = |doc: &mut Document, name: String, r: &Raster, x: i32, y: i32| {
        let id = doc.add_pixel_layer(name);
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(r, x, y);
        id
    };
    for i in 0..4 {
        let id = pixel(&mut doc, format!("Full {i}"), &texture(w, h, i, 1.0), 0, 0);
        if i > 0 {
            let l = doc.layer_mut(id).unwrap();
            l.blend = modes[i as usize];
            l.opacity = 0.5;
        }
        n += 1;
    }
    for i in 0..20u32 {
        let (pw, ph) = (600 + (i * 97) % 900, 500 + (i * 61) % 700);
        let (x, y) = (((i * 523) % 3300) as i32, ((i * 377) % 2400) as i32);
        let id = pixel(
            &mut doc,
            format!("Patch {i}"),
            &texture(pw, ph, 10 + i, 0.9),
            x,
            y,
        );
        let l = doc.layer_mut(id).unwrap();
        l.opacity = 0.6 + (i % 4) as f32 * 0.1;
        l.blend = modes[(i % 5) as usize];
        if i == 10 {
            mid = id;
        }
        n += 1;
        if i % 4 == 3 {
            let a = doc.add_adjustment(match i % 3 {
                0 => Adjustment::BrightnessContrast {
                    brightness: 0.05,
                    contrast: 0.1,
                },
                1 => Adjustment::HueSaturation {
                    hue: 8.0,
                    saturation: 0.2,
                    lightness: 0.0,
                    colorize: false,
                },
                _ => Adjustment::Invert,
            });
            if i == 7 {
                // A checkerboard over the top-left quarter, hidden elsewhere.
                let (mw, mh) = (w / 2, h / 2);
                let mut r = Raster::new(mw, mh);
                for y in 0..mh {
                    for x in 0..mw {
                        let v = if (x / 64 + y / 64) % 2 == 0 { 1.0 } else { 0.3 };
                        r.pixels[(y * mw + x) as usize] = Rgba::new(v, v, v, v);
                    }
                }
                let mut m = lumenply_doc::Mask::hide_all();
                m.tiles = TileStore::from_raster(&r, 0, 0);
                doc.layer_mut(a).unwrap().mask = Some(m);
            }
            n += 1;
        }
    }
    let g = doc.add_group("Group");
    for i in 0..3u32 {
        let id = doc.alloc_id();
        let mut l = Layer::pixel(id, format!("In group {i}"));
        *l.pixels_mut().unwrap() =
            TileStore::from_raster(&texture(800, 700, 50 + i, 0.8), 400 + i as i32 * 900, 1800);
        doc.layer_mut(g).unwrap().children_mut().unwrap().push(l);
    }
    n += 1;
    assert!(n >= 30 - 3, "{n} top-level layers");
    lumenply_core::compact_storage(&mut doc);
    (Editor::new(doc), mid)
}

struct Bench {
    app: App,
    ctx: egui::Context,
    /// The compositing path the app used before the graph renderer: a
    /// cache of the composite below the layer being edited, driven as the
    /// app drove it. Timed on the same edits for comparison.
    below: lumenply_render::BelowCache,
    /// Scenario, edit ms, redraw ms, of which compositing through the
    /// graph, and the old path's compositing of the same area.
    rows: Vec<(String, f64, f64, f64, f64)>,
}

/// Compositing time since the last call (see `canvas::COMPOSITE_NANOS`).
fn composite_ms() -> f64 {
    crate::canvas::COMPOSITE_NANOS.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / 1e6
}

impl Bench {
    fn new(ed: Editor) -> Bench {
        let mut app = App::launch(&[]);
        app.dialog = None;
        app.last_autosave = Instant::now() + Duration::from_secs(24 * 3600);
        app.set_doc(ed, None);
        if let Some(mb) = std::env::var("LUMENPLY_BENCH_BUDGET_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            app.editor.renderer().cache.set_budget(mb << 20);
            app.editor.renderer().wholes.set_budget(mb << 20);
        }
        let ctx = egui::Context::default();
        let mut below = lumenply_render::BelowCache::new();
        let doc = app.editor.doc();
        let t = Instant::now();
        below.composite_rect(doc, doc.canvas());
        let old = ms(t.elapsed());
        composite_ms();
        let t = Instant::now();
        app.refresh(&ctx);
        let first = ms(t.elapsed());
        let first_composite = composite_ms();
        let mut b = Bench {
            app,
            ctx,
            below,
            rows: Vec::new(),
        };
        b.rows
            .push(("first full render".into(), 0.0, first, first_composite, old));
        b
    }

    /// The old path's compositing of `area` after the edit just made, the
    /// cache driven as `App::run` and `undo` drove it.
    fn old_path(&mut self, area: Rect) -> f64 {
        let doc = self.app.editor.doc();
        self.below.note_change(doc, self.app.editor.last_target_layer());
        let t = Instant::now();
        if !area.is_empty() {
            self.below.composite_rect(doc, area);
        }
        ms(t.elapsed())
    }

    /// Time `edit` then a refresh, `reps` times; record the medians.
    fn time(&mut self, name: &str, reps: usize, mut edit: impl FnMut(&mut App, usize)) {
        let (mut run, mut draw, mut comp, mut old) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..reps {
            composite_ms();
            let t = Instant::now();
            edit(&mut self.app, i);
            run.push(ms(t.elapsed()));
            if !self.app.dirty {
                continue;
            }
            let canvas = self.app.editor.doc().canvas();
            let area = self.app.dirty_rect.map_or(canvas, |r| r.intersect(&canvas));
            // Alternate which path goes first, so neither always finds the
            // other's work in the processor's caches.
            if i % 2 == 0 {
                old.push(self.old_path(area));
            }
            let before = self.app.editor.renderer().cache.stats();
            let t = Instant::now();
            self.app.refresh(&self.ctx);
            draw.push(ms(t.elapsed()));
            comp.push(composite_ms());
            if i % 2 == 1 {
                old.push(self.old_path(area));
            }
            if std::env::var("LUMENPLY_BENCH_DEBUG").is_ok() {
                let after = self.app.editor.renderer().cache.stats();
                println!(
                    "  {name} #{i}: {} hits, {} misses, {} duplicates; {} tiles, {} MB",
                    after.hits - before.hits,
                    after.misses - before.misses,
                    after.duplicates - before.duplicates,
                    after.tiles,
                    after.bytes >> 20
                );
            }
        }
        self.rows
            .push((name.into(), median(run), median(draw), median(comp), median(old)));
    }

    /// A stroke as the Brush tool draws it: a preview frame per new point
    /// (the document so far plus the stroke so far, over the area the new
    /// points reach), then the commit.
    fn stroke(&mut self, name: &str, layer: LayerId, from: (f32, f32), to: (f32, f32)) {
        self.app.active = Some(layer);
        let brush = Brush {
            radius: 40.0,
            hardness: 0.7,
            color: [0.9, 0.2, 0.1, 1.0],
            spacing: 0.12,
            ..Brush::default()
        };
        let n = 40;
        let pts: Vec<StrokePoint> = (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                StrokePoint::new(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, 1.0)
            })
            .collect();
        let (mut frames, mut comp, mut old) = (Vec::new(), Vec::new(), Vec::new());
        for i in 1..=n {
            composite_ms();
            let t = Instant::now();
            let cmd = PaintStroke {
                layer,
                brush: brush.clone(),
                points: pts[..i].to_vec(),
            };
            let mut preview = self.app.editor.doc().clone();
            cmd.apply(&mut preview).unwrap();
            let from = i.saturating_sub(3);
            let area = stroke_bounds(&brush, &pts[from..i], preview.canvas());
            self.app.preview(&self.ctx, &preview, Some(area));
            frames.push(ms(t.elapsed()));
            comp.push(composite_ms());
            // The old path: the backdrop below the active layer cached.
            self.below.note_change(&preview, Some(layer));
            let t = Instant::now();
            self.below
                .composite_rect(&preview, area.intersect(&preview.canvas()));
            old.push(ms(t.elapsed()));
        }
        self.rows.push((
            format!("{name}: preview frame"),
            0.0,
            median(frames),
            median(comp),
            median(old),
        ));
        self.time(&format!("{name}: commit"), 1, |app, _| {
            app.run(&PaintStroke {
                layer,
                brush: brush.clone(),
                points: pts.clone(),
            })
        });
    }

    /// Where a full-canvas refresh spends its time, step by step.
    fn breakdown(&mut self) {
        let canvas = self.app.editor.doc().canvas();
        let t = Instant::now();
        let store = self.app.render_view(canvas);
        let render = ms(t.elapsed());
        let t = Instant::now();
        let flat = flatten(&store, canvas);
        let raster = ms(t.elapsed());
        let t = Instant::now();
        let img = crate::soft_proof::display_image(&flat, None);
        let display = ms(t.elapsed());
        let t = Instant::now();
        upload(
            &mut self.app.canvas_tex,
            &self.ctx,
            "canvas",
            img,
            nearest_when_zoomed(),
        );
        let upload_ms = ms(t.elapsed());
        self.app.last_flat = Some(flat);
        let t = Instant::now();
        self.app.refresh_thumbs(&self.ctx, None);
        let thumbs = ms(t.elapsed());
        let t = Instant::now();
        self.app.update_histogram();
        let hist = ms(t.elapsed());
        println!(
            "full refresh steps: render {render:.1}, to raster {raster:.1}, display {display:.1}, \
             upload {upload_ms:.1}, thumbnails {thumbs:.1}, histogram {hist:.1} ms"
        );
    }

    fn report(&self, title: &str) {
        println!("\n{title}");
        println!(
            "{:<40} {:>8} {:>9} {:>11} {:>11}",
            "scenario", "edit ms", "redraw ms", "graph ms", "old ms"
        );
        for (name, run, draw, comp, old) in &self.rows {
            println!("{name:<40} {run:>8.1} {draw:>9.1} {comp:>11.1} {old:>11.1}");
        }
        let ed = &self.app.editor;
        let s = ed.renderer().cache.stats();
        println!(
            "render cache: {} tiles, {:.0} MB; wholes {:.0} MB",
            s.tiles,
            s.bytes as f64 / 1048576.0,
            ed.renderer().wholes.stats().1 as f64 / 1048576.0
        );
    }
}

/// Run every scenario on one document. `layer` is a pixel layer in the
/// stack to paint on; `slider` the layer whose opacity is dragged.
fn scenarios(title: &str, ed: Editor, layer: LayerId, slider: LayerId) {
    let mut b = Bench::new(ed);
    let (w, h) = (b.app.editor.doc().width as f32, b.app.editor.doc().height as f32);
    b.stroke(
        "paint mid-stack layer",
        layer,
        (w * 0.3, h * 0.4),
        (w * 0.6, h * 0.5),
    );
    b.time(
        "undo",
        3,
        |app, i| {
            if i % 2 == 0 {
                app.undo()
            } else {
                app.redo()
            }
        },
    );
    b.time("redo", 1, |app, _| app.redo());
    b.time("opacity drag tick", 20, |app, i| {
        app.run_coalescing(
            &SetOpacity {
                layer: slider,
                opacity: 1.0 - i as f32 * 0.03,
            },
            "bench-opacity",
        )
    });
    b.app.editor.end_coalescing();
    b.time("hide layer", 1, |app, _| {
        app.run(&SetVisible {
            layer: slider,
            visible: false,
        })
    });
    b.time("show layer", 1, |app, _| {
        app.run(&SetVisible {
            layer: slider,
            visible: true,
        })
    });
    b.time("undo (show)", 1, |app, _| app.undo());
    b.time("new layer on top", 1, |app, _| {
        app.run(&AddPixelLayer::new("Paint"))
    });
    let top = b.app.editor.doc().layers().last().unwrap().id;
    b.stroke("paint new top layer", top, (w * 0.2, h * 0.6), (w * 0.5, h * 0.3));
    b.time("undo stroke", 1, |app, _| app.undo());
    b.time("redo stroke", 1, |app, _| app.redo());
    b.breakdown();
    b.report(title);
}

#[test]
#[ignore = "timings, not a check; run with --ignored --nocapture"]
fn render_bench() {
    let which = std::env::var("LUMENPLY_BENCH").unwrap_or_default();
    if which.is_empty() || which == "demo" {
        let ed = demo::build().unwrap();
        let ids: Vec<(LayerId, String)> = ed.doc().layers().iter().map(|l| (l.id, l.name.clone())).collect();
        let bg = ids[0].0;
        let vignette = ids.iter().find(|(_, n)| n == "Vignette").unwrap().0;
        scenarios("demo document (1800 × 1205, 7 layers)", ed, bg, vignette);
    }
    if which.is_empty() || which == "big" {
        let t = Instant::now();
        let (ed, mid) = big_doc();
        println!(
            "\nbuilt the 30-layer document in {:.1} s",
            t.elapsed().as_secs_f64()
        );
        scenarios("30 layers, 4000 × 3000", ed, mid, mid);
    }
}
