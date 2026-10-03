//! The GPU executor against the CPU evaluator, the reference: every
//! kernel with its parameters, the fallback path, caching and uploads.
//! Machines without a GPU adapter skip with a notice.

use lumenply_doc::{
    Adjustment, BlendMode, Document, Filter, Layer, LayerContent, LevelsChannel, Mask, ShadowFx, StrokeFx,
};
use lumenply_tiles::{Raster, Rect, Rgba, TileCoord, TileStore};

use super::*;
use crate::tests::{busy_document, half_mask, painted};
use crate::{lower, History, LayerProps};

fn gpu() -> Option<GpuRenderer> {
    let g = GpuRenderer::new();
    if g.is_none() {
        eprintln!("no GPU adapter available; skipping GPU equality test");
    }
    g
}

fn lowered(doc: &Document) -> (Graph, BlobStore) {
    let mut blobs = BlobStore::new();
    let g = lower(doc, &mut blobs, &Renderer::new().hasher).graph;
    (g, blobs)
}

/// Largest per-channel difference over `area`.
fn worst(a: &TileStore, b: &TileStore, area: Rect) -> (f32, (i32, i32)) {
    let mut w = (0.0f32, (0, 0));
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            for (u, v) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
                if (u - v).abs() > w.0 {
                    w = ((u - v).abs(), (x, y));
                }
            }
        }
    }
    w
}

/// Render the canvas with a fresh CPU renderer and on the GPU (read back)
/// and require the same pixels within 1e-4.
fn assert_gpu_matches(gpu: &mut GpuRenderer, graph: &Graph, blobs: &BlobStore, what: &str) {
    let cpu = Renderer::new().render_canvas(graph, blobs);
    let ours = gpu.render_canvas_to_store(graph, blobs);
    let (d, at) = worst(&cpu, &ours, graph.canvas());
    assert!(
        d <= 1e-4,
        "{what}: GPU differs from CPU by {d} at {at:?} (cpu {:?}, gpu {:?})",
        cpu.get_pixel(at.0, at.1),
        ours.get_pixel(at.0, at.1)
    );
}

/// Store every layer's pixels and masks compactly (16-bit), as the editor
/// does after each command, so the 16-bit upload path is exercised.
fn compact(layers: &mut [Layer]) {
    for l in layers {
        if let Some(px) = l.pixels_mut() {
            px.compact();
        }
        if let Some(m) = &mut l.mask {
            m.tiles.compact();
        }
        if let Some(c) = l.children_mut() {
            compact(c);
        }
    }
}

/// A horizontal hue sweep with a vertical ramp over the whole canvas;
/// irrational steps keep the discontinuous modes (Hard Mix, Darker
/// Color...) away from exact ties, where float rounding could pick either
/// side on either device.
fn sweep(doc: &mut Document) -> Layer {
    let (w, h) = (doc.width, doc.height);
    let mut r = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 * 0.618_034 + 0.013) % 1.0;
            let u = (y as f32 * 0.414_213 + 0.021) % 1.0;
            r.set(x, y, Rgba::from_straight(t, u, 1.0 - 0.6 * t, 0.97));
        }
    }
    let mut l = Layer::pixel(doc.alloc_id(), "sweep");
    *l.pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
    l
}

/// A layer of soft-edged colour bands over `area`.
fn bands(doc: &mut Document, area: Rect, alpha: f32) -> Layer {
    let mut r = Raster::new(area.w, area.h);
    for y in 0..area.h {
        for x in 0..area.w {
            let a = alpha * (0.35 + 0.65 * ((x + 2 * y) % 37) as f32 / 36.0);
            r.set(
                x,
                y,
                Rgba::from_straight(0.85, 0.3 + 0.002 * y as f32, 0.1 + 0.003 * x as f32, a),
            );
        }
    }
    let mut l = Layer::pixel(doc.alloc_id(), "bands");
    *l.pixels_mut().unwrap() = TileStore::from_raster(&r, area.x, area.y);
    l
}

/// Every blend mode the GPU runs (all but Dissolve) over a sweep, with
/// masks of every kind (painted, hidden by default, partial default),
/// fractional opacity and fill opacity. Nothing falls back to the CPU.
#[test]
fn every_blend_mode_with_masks_opacity_and_fill_matches_the_cpu() {
    let Some(mut gpu) = gpu() else { return };
    let modes: Vec<BlendMode> = BlendMode::ALL
        .into_iter()
        .filter(|m| *m != BlendMode::Dissolve)
        .collect();
    let mut doc = Document::new(30 * modes.len() as u32 + 40, 300);
    let bg = sweep(&mut doc);
    doc.add_layer(bg);
    for (i, mode) in modes.iter().enumerate() {
        let area = Rect::new(i as i32 * 30, 10 + (i as i32 % 3) * 20, 64, 250);
        let mut l = bands(&mut doc, area, 0.8);
        l.blend = *mode;
        l.opacity = 0.85;
        l.fill_opacity = if i % 3 == 1 { 0.7 } else { 1.0 };
        l.mask = match i % 4 {
            0 => {
                let mut m = Mask::reveal_all();
                lumenply_render::gradient_mask(&mut m, area);
                Some(m)
            }
            1 => Some(half_mask(area)),
            2 => Some(Mask {
                default: 0.55,
                ..Mask::hide_all()
            }),
            _ => None,
        };
        doc.add_layer(l);
    }
    let (g, blobs) = lowered(&doc);
    assert_gpu_matches(&mut gpu, &g, &blobs, "every GPU blend mode (f32 tiles)");
    let s = gpu.stats();
    assert_eq!(s.cpu_tiles(), 0, "nothing falls back: {s:?}");
    assert!(s.dispatches > 0);

    compact(doc.layers_mut());
    let (g, blobs) = lowered(&doc);
    let mut fresh = GpuRenderer::new().unwrap();
    assert_gpu_matches(&mut fresh, &g, &blobs, "every GPU blend mode (16-bit tiles)");
    assert_eq!(fresh.stats().cpu_tiles(), 0);
}

/// Adjustments that compile to tables, one table or one per channel, in
/// Normal and other blend modes, at partial opacity under masks; a
/// hidden one changes nothing; three overlap at the right. All on the GPU.
///
/// Each gets its own strip (a mask) so a step-shaped table such as
/// Posterize sees the image itself: downstream of other layers it would
/// magnify their last-bit GPU/CPU differences (about 1e-7) by its slope,
/// a few hundred at a step, whichever device ran it.
#[test]
fn table_adjustments_in_any_mode_match_the_cpu() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(420, 300);
    let bg = sweep(&mut doc);
    doc.add_layer(bg);
    let red = LevelsChannel {
        in_black: 0.1,
        gamma: 1.4,
        ..LevelsChannel::default()
    };
    let adjustments = [
        (
            Adjustment::Levels {
                in_black: 0.1,
                in_white: 0.9,
                gamma: 1.3,
                out_black: 0.05,
                out_white: 1.0,
                channels: Default::default(),
            },
            BlendMode::Normal,
        ),
        (
            Adjustment::Levels {
                in_black: 0.0,
                in_white: 0.95,
                gamma: 0.9,
                out_black: 0.0,
                out_white: 1.0,
                channels: [red, LevelsChannel::default(), LevelsChannel::default()],
            },
            BlendMode::Multiply,
        ),
        (
            Adjustment::Curves {
                points: vec![[0.0, 0.0], [0.3, 0.2], [0.7, 0.85], [1.0, 1.0]],
                channels: Default::default(),
            },
            BlendMode::Normal,
        ),
        (
            Adjustment::Curves {
                points: vec![[0.0, 0.05], [1.0, 0.95]],
                channels: [vec![], vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]], vec![]],
            },
            BlendMode::Luminosity,
        ),
        (Adjustment::Invert, BlendMode::Color),
        (
            Adjustment::BrightnessContrast {
                brightness: 0.1,
                contrast: 0.3,
            },
            BlendMode::Screen,
        ),
        (Adjustment::Posterize { levels: 5 }, BlendMode::Normal),
        (Adjustment::Invert, BlendMode::Hue),
    ];
    for (i, (adj, mode)) in adjustments.into_iter().enumerate() {
        let strip = Rect::new(i as i32 * 45, 0, 45, 300);
        let mut m = Mask::hide_all();
        if i % 2 == 0 {
            lumenply_render::gradient_mask(&mut m, strip);
        } else {
            m.fill_rect(strip, 1.0);
        }
        if i < 3 {
            m.fill_rect(Rect::new(360, 0, 60, 300), 1.0);
        }
        let mut l = Layer::adjustment(doc.alloc_id(), adj);
        l.blend = mode;
        l.opacity = [1.0, 0.6, 0.85][i % 3];
        l.mask = Some(m);
        doc.add_layer(l);
    }
    let mut hidden = Layer::adjustment(doc.alloc_id(), Adjustment::Invert);
    hidden.visible = false;
    doc.add_layer(hidden);
    let (g, blobs) = lowered(&doc);
    assert_gpu_matches(&mut gpu, &g, &blobs, "table adjustments");
    assert_eq!(gpu.stats().cpu_tiles(), 0, "{:?}", gpu.stats());
}

/// Pass-through groups at partial strength and with a mask mix on the
/// GPU; isolated groups composite as layers.
#[test]
fn pass_through_and_isolated_groups_match_the_cpu() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(520, 300);
    let bg = sweep(&mut doc);
    doc.add_layer(bg);

    let mut pass = Layer::group(doc.alloc_id(), "Pass");
    pass.pass_through = true;
    pass.opacity = 0.6;
    pass.mask = Some(half_mask(Rect::new(100, 0, 400, 300)));
    let mut px = bands(&mut doc, Rect::new(50, 40, 300, 200), 0.9);
    px.blend = BlendMode::Multiply;
    let mut inv = Layer::adjustment(doc.alloc_id(), Adjustment::Invert);
    inv.opacity = 0.7;
    let mut iso = Layer::group(doc.alloc_id(), "Isolated");
    iso.blend = BlendMode::Screen;
    iso.opacity = 0.8;
    let inner = bands(&mut doc, Rect::new(260, 100, 200, 150), 0.7);
    iso.children_mut().unwrap().push(inner);
    pass.children_mut().unwrap().extend([px, inv, iso]);
    doc.add_layer(pass);

    // Full strength without a mask: the group's own result passes up.
    let mut full = Layer::group(doc.alloc_id(), "Full");
    full.pass_through = true;
    let mut lift = Layer::adjustment(
        doc.alloc_id(),
        Adjustment::BrightnessContrast {
            brightness: 0.05,
            contrast: 0.2,
        },
    );
    lift.mask = Some(half_mask(Rect::new(0, 0, 520, 300)));
    full.children_mut().unwrap().push(lift);
    doc.add_layer(full);

    let (g, blobs) = lowered(&doc);
    assert!(
        g.nodes().any(|(_, n)| matches!(n.op, Op::PassThrough { .. })),
        "the lowering made pass-through nodes"
    );
    assert_gpu_matches(&mut gpu, &g, &blobs, "groups");
    assert_eq!(gpu.stats().cpu_tiles(), 0, "{:?}", gpu.stats());
}

/// The document the lowering tests use (effects, clip chain, filter,
/// pass-through and isolated groups, a hidden layer) renders the same.
#[test]
fn the_busy_document_matches_the_cpu() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = busy_document();
    let (g, blobs) = lowered(&doc);
    assert_gpu_matches(&mut gpu, &g, &blobs, "busy document");
    compact(doc.layers_mut());
    let (g, blobs) = lowered(&doc);
    assert_gpu_matches(&mut gpu, &g, &blobs, "busy document, compact");
}

/// Nodes without a kernel (a drop shadow, Dissolve, a live filter, a clip
/// chain, per-pixel and gradient-map adjustments, a styled group) run on
/// the CPU between GPU layers; the stack below each is computed on the GPU
/// and read back, not recomputed by the CPU.
#[test]
fn cpu_fallback_nodes_between_gpu_layers_match_the_cpu() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(600, 520);
    let bg = sweep(&mut doc);
    doc.add_layer(bg);
    for i in 0..5 {
        let mut l = bands(&mut doc, Rect::new(40 * i, 30 * i, 300, 260), 0.8);
        l.blend = [BlendMode::Multiply, BlendMode::Overlay, BlendMode::Normal][i as usize % 3];
        l.opacity = 0.9;
        doc.add_layer(l);
    }
    let mut shadowed = painted(
        &mut doc,
        "Shadowed",
        Rect::new(150, 140, 200, 170),
        [0.1, 0.4, 0.9, 1.0],
    );
    shadowed.effects.drop_shadow = Some(ShadowFx {
        dx: 6.0,
        dy: 8.0,
        blur: 5.0,
        opacity: 0.7,
        ..ShadowFx::default()
    });
    shadowed.fill_opacity = 0.5;
    doc.add_layer(shadowed);
    let mut top = bands(&mut doc, Rect::new(0, 200, 600, 120), 0.6);
    top.blend = BlendMode::SoftLight;
    doc.add_layer(top);
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &Renderer::new().hasher);
    let g = low.graph;
    assert_gpu_matches(&mut gpu, &g, &blobs, "drop shadow between GPU layers");
    let s = gpu.stats();
    let canvas_tiles = doc.canvas().tiles();
    let tiles = canvas_tiles.len() as u64;
    assert_eq!(s.fallback_tiles.get("layer"), Some(&tiles), "{s:?}");
    assert_eq!(s.cpu_tiles(), tiles, "only the styled layer runs on the CPU");
    // The CPU evaluator got the styled layer's backdrop from the GPU (read
    // back into its cache) and never computed the layers below that.
    let keys = gpu.cpu.keys(&g, g.output.unwrap());
    let layers = doc.layers();
    let backdrop = keys[&low.layer_nodes[&layers[5].id]];
    let below = keys[&low.layer_nodes[&layers[4].id]];
    for c in &canvas_tiles {
        assert!(
            gpu.cpu.cache.contains(backdrop, *c),
            "backdrop read back at {c:?}"
        );
        assert!(
            !gpu.cpu.cache.contains(below, *c),
            "the CPU recomputed the stack at {c:?}"
        );
    }
    // The backdrop, and the output for the readback API.
    assert_eq!(s.read_back_tiles, 2 * tiles, "{s:?}");

    // Every other fallback, mixed with GPU nodes.
    let mut d = Document::new(600, 520);
    let bg = sweep(&mut d);
    d.add_layer(bg);
    let mut dis = bands(&mut d, Rect::new(30, 30, 400, 300), 0.7);
    dis.blend = BlendMode::Dissolve;
    dis.opacity = 0.8;
    d.add_layer(dis);
    let mid = bands(&mut d, Rect::new(200, 100, 300, 300), 0.5);
    d.add_layer(mid);
    // A clip chain: base plus a clipped pixel layer.
    let base = painted(&mut d, "Base", Rect::new(60, 260, 260, 200), [0.2, 0.7, 0.3, 1.0]);
    d.add_layer(base);
    let mut member = bands(&mut d, Rect::new(0, 300, 600, 80), 1.0);
    member.clip = true;
    member.blend = BlendMode::Screen;
    d.add_layer(member);
    // A styled isolated group: its content (a sub-stack) is computed on
    // the GPU and read back for the CPU's stroke.
    let mut grp = Layer::group(d.alloc_id(), "Styled");
    grp.effects.stroke = Some(StrokeFx {
        size: 4.0,
        color: [0.9, 0.1, 0.1],
        ..StrokeFx::default()
    });
    let g1 = bands(&mut d, Rect::new(300, 20, 200, 180), 0.9);
    let mut g2 = bands(&mut d, Rect::new(350, 80, 200, 180), 0.9);
    g2.blend = BlendMode::Difference;
    grp.children_mut().unwrap().extend([g1, g2]);
    d.add_layer(grp);
    let mut vib = Layer::adjustment(
        d.alloc_id(),
        Adjustment::Vibrance {
            vibrance: 0.5,
            saturation: 0.2,
        },
    );
    vib.mask = Some(half_mask(Rect::new(0, 0, 600, 520)));
    d.add_layer(vib);
    let gmap = Layer::adjustment(d.alloc_id(), Adjustment::gradient_map_default());
    d.add_layer(gmap);
    let mut lut_dissolve = Layer::adjustment(d.alloc_id(), Adjustment::Invert);
    lut_dissolve.blend = BlendMode::Dissolve;
    lut_dissolve.opacity = 0.3;
    d.add_layer(lut_dissolve);
    let after = bands(&mut d, Rect::new(100, 0, 300, 520), 0.4);
    d.add_layer(after);
    let mut blur = Layer::filter(d.alloc_id(), Filter::GaussianBlur { radius: 3.0 });
    blur.opacity = 0.8;
    d.add_layer(blur);
    let top = bands(&mut d, Rect::new(0, 0, 250, 250), 0.5);
    d.add_layer(top);
    compact(d.layers_mut());
    let (g, blobs) = lowered(&d);
    gpu.reset_stats();
    assert_gpu_matches(&mut gpu, &g, &blobs, "every fallback kind");
    let s = gpu.stats();
    for op in ["layer", "clip-group", "adjustment", "filter-layer"] {
        assert!(s.fallback_tiles.contains_key(op), "{op} fell back: {s:?}");
    }
    assert!(s.dispatches > 0, "and the rest ran on the GPU: {s:?}");
}

/// An unchanged graph renders from the cache; an edit near the top
/// dispatches only the edited node's tiles and what is above it, uploads
/// nothing; undo finds the old tiles still resident.
#[test]
fn renders_reuse_resident_tiles_across_edits_and_undo() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(600, 520);
    let bg = sweep(&mut doc);
    doc.add_layer(bg);
    for i in 0..6 {
        let mut l = bands(&mut doc, Rect::new(30 * i, 20 * i, 500, 400), 0.7);
        l.blend = BlendMode::ALL[(i * 5) as usize % 9 + 1];
        doc.add_layer(l);
    }
    compact(doc.layers_mut());
    let canvas = doc.canvas();
    let tiles = canvas.tiles().len() as u64;
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &r.hasher);
    let mut history = History::new(low.graph.clone());

    let first = gpu.render_gpu(history.current(), &blobs, canvas);
    let cold = gpu.stats();
    assert!(cold.dispatches > 0 && cold.uploaded_tiles > 0);
    let cold_store = gpu.read_back(&first);
    let (d, _) = worst(&cold_store, &r.render_canvas(history.current(), &blobs), canvas);
    assert!(d <= 1e-4, "cold render differs by {d}");

    gpu.reset_stats();
    let again = gpu.render_gpu(history.current(), &blobs, canvas);
    let warm = gpu.stats();
    assert_eq!((warm.dispatches, warm.uploaded_tiles), (0, 0), "{warm:?}");
    assert_eq!(warm.hits, tiles, "one hit per output tile and nothing below");
    assert_eq!(again.tiles.len(), first.tiles.len());

    // Edit the second layer from the top: it and the top layer recompute.
    let edited_layer = doc.layers()[doc.layers().len() - 2].id;
    let node = low.layer_nodes[&edited_layer];
    let mut g = history.current().clone();
    g.update(node, |n| {
        if let Op::Layer { props } = &mut n.op {
            props.opacity = 0.35;
        }
    })
    .unwrap();
    history.commit(g, "Opacity");
    gpu.reset_stats();
    gpu.render_gpu(history.current(), &blobs, canvas);
    let edit = gpu.stats();
    assert_eq!(
        edit.uploaded_tiles, 0,
        "every blob tile is still resident: {edit:?}"
    );
    assert!(edit.dispatches <= 2 * tiles, "two nodes at most: {edit:?}");
    assert!(edit.dispatches > 0);
    assert_gpu_matches(&mut gpu, history.current(), &blobs, "after the edit");

    // Undo: the previous version's tiles are all still cached.
    assert!(history.undo());
    gpu.reset_stats();
    let undone = gpu.render_gpu(history.current(), &blobs, canvas);
    assert_eq!(gpu.stats().dispatches, 0, "undo recomputes nothing");
    let (d, _) = worst(&gpu.read_back(&undone), &cold_store, canvas);
    assert_eq!(d, 0.0, "undo shows the very same tiles");
}

/// Blob tiles upload once per content: two layers with the same pixels
/// share one upload, and a new version of a layer that shares all but one
/// tile with the old uploads only that tile.
#[test]
fn blob_tiles_upload_once_per_content() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(512, 512);
    let mut a = painted(&mut doc, "A", Rect::new(0, 0, 512, 512), [0.2, 0.5, 0.7, 0.8]);
    compact(std::slice::from_mut(&mut a));
    let mut b = a.clone();
    b.id = doc.alloc_id();
    b.blend = BlendMode::Multiply;
    doc.add_layer(a);
    doc.add_layer(b);
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &r.hasher);
    assert_eq!(blobs.len(), 1);
    assert_gpu_matches(&mut gpu, &low.graph, &blobs, "shared blob");
    assert_eq!(
        gpu.stats().uploaded_tiles,
        4,
        "four tiles, uploaded once for both layers"
    );
    assert_eq!(gpu.stats().uploaded_bytes, 4 * 256 * 256 * 8, "as 16-bit texels");

    // Paint one pixel of B: a new blob sharing three tiles with the old.
    let bid = doc.layers()[1].id;
    let LayerContent::Pixel(store) = &doc.layers()[1].content else {
        unreachable!()
    };
    let mut painted_store = store.clone();
    painted_store.set_pixel(300, 300, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
    let blob = blobs.insert(&r.hasher, painted_store);
    let mut g = low.graph.clone();
    let content = g.node(low.layer_nodes[&bid]).unwrap().input(1).unwrap();
    g.update(content, |n| n.op = Op::Image { blob }).unwrap();
    gpu.reset_stats();
    assert_gpu_matches(&mut gpu, &g, &blobs, "repainted layer");
    assert_eq!(gpu.stats().uploaded_tiles, 1, "only the painted tile uploads");
}

/// The resident result reads back to the same pixels as the readback API;
/// transparent tiles are left out and a solid tile stays a colour.
#[test]
fn resident_images_read_back_like_render_to_store() {
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(700, 300);
    let l = bands(&mut doc, Rect::new(10, 10, 200, 200), 0.9);
    doc.add_layer(l);
    let (g, blobs) = lowered(&doc);
    let img = gpu.render_gpu(&g, &blobs, g.canvas());
    let coords: Vec<TileCoord> = img.tiles.iter().map(|(c, _)| *c).collect();
    assert_eq!(
        coords,
        vec![TileCoord::new(0, 0)],
        "transparent tiles are left out"
    );
    let back = gpu.read_back(&img);
    let store = gpu.render_canvas_to_store(&g, &blobs);
    assert_eq!(worst(&back, &store, g.canvas()).0, 0.0);

    let mut mg = Graph::new(300, 300);
    let m = mg.add(Node::new(
        Op::Mask {
            blob: None,
            default: 0.25,
        },
        vec![],
    ));
    let img = gpu.render_node_gpu(&mg, &BlobStore::new(), m, Rect::new(0, 0, 10, 10));
    assert!(matches!(
        img.tiles[..],
        [(_, GpuTile::Solid([0.25, 0.25, 0.25, 0.25]))]
    ));
    let back = gpu.read_back(&img);
    assert_eq!(back.get_pixel(5, 5), Rgba::new(0.25, 0.25, 0.25, 0.25));
}

/// A budget far below the working set evicts as it goes and still
/// renders exactly, again and again.
#[test]
fn a_tiny_gpu_budget_still_renders_exactly() {
    let Some(mut gpu) = gpu() else { return };
    gpu.set_budget(3 << 20);
    let doc = busy_document();
    let (g, blobs) = lowered(&doc);
    assert_gpu_matches(&mut gpu, &g, &blobs, "tiny budget");
    assert!(gpu.stats().cached_bytes <= 3 << 20, "{:?}", gpu.stats());
    gpu.clear();
    let img = gpu.render_gpu(&g, &blobs, g.canvas());
    let back = gpu.read_back(&img);
    let (d, _) = worst(&back, &Renderer::new().render_canvas(&g, &blobs), g.canvas());
    assert!(d <= 1e-4, "{d}");
}

/// Layer content computed in one piece (text, fills, a shape, a smart
/// object through a transform and smart filters) has no kernel: the CPU
/// makes each once and its tiles are uploaded for the GPU layers above.
#[test]
fn content_ops_run_on_the_cpu_under_gpu_layers() {
    let Some(mut gpu) = gpu() else { return };
    let doc = crate::tests_content::every_content_op();
    let (g, blobs) = lowered(&doc);
    assert_gpu_matches(&mut gpu, &g, &blobs, "every content op");
    let s = gpu.stats();
    for op in ["text", "fill", "shape", "smart-filter"] {
        assert!(s.fallback_tiles.contains_key(op), "{op} ran on the CPU: {s:?}");
    }
    assert!(s.dispatches > 0, "the layers above ran on the GPU: {s:?}");
    assert_eq!(gpu.cpu.cache.stats().duplicates, 0, "no tile computed twice");
}

/// eframe keeps paint-callback resources in a `Send + Sync` map, and the
/// images are handed to its render thread.
#[test]
fn the_renderer_and_its_images_can_live_in_eframes_callback_resources() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<GpuRenderer>();
    send_sync::<GpuImage>();
}

/// Brush strokes (paint, smudge, blur) have no kernel: the CPU walks the
/// chain and its tiles are uploaded for the GPU layer above. A changed
/// stroke uploads only the tiles that changed.
#[test]
fn stroke_chains_run_on_the_cpu_under_gpu_layers() {
    use lumenply_render::paint::{Brush, BrushMode, StrokePoint};
    let Some(mut gpu) = gpu() else { return };
    let mut doc = Document::new(768, 512);
    let bg = sweep(&mut doc);
    let LayerContent::Pixel(backdrop) = bg.content else {
        unreachable!()
    };
    let mut stripes = TileStore::new();
    for y in 0..512 {
        for x in 0..768 {
            let v = if (x / 7 + y / 11) % 2 == 0 { 0.9 } else { 0.1 };
            stripes.set_pixel(x, y, Rgba::from_straight(v, 0.5, 1.0 - v, 0.8));
        }
    }
    let line = |x0: f32, y0: f32, x1: f32, y1: f32| -> Vec<StrokePoint> {
        (0..6)
            .map(|i| {
                let t = i as f32 / 5.0;
                StrokePoint::new(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, 1.0)
            })
            .collect()
    };
    let brush = |mode: BrushMode| Brush {
        radius: 9.0,
        color: [0.2, 0.6, 0.9, 0.8],
        mode,
        ..Brush::default()
    };
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let mut g = Graph::new(768, 512);
    let back = g.add(Node::new(
        Op::Image {
            blob: blobs.insert(&r.hasher, backdrop),
        },
        vec![],
    ));
    let mut content = g.add(Node::new(
        Op::Image {
            blob: blobs.insert(&r.hasher, stripes),
        },
        vec![],
    ));
    let strokes = [
        (BrushMode::Paint, line(60.0, 60.0, 160.0, 90.0)),
        (BrushMode::Smudge, line(150.0, 100.0, 330.0, 120.0)),
        (BrushMode::Blur, line(550.0, 350.0, 710.0, 360.0)),
    ];
    let mut ids = Vec::new();
    for (mode, points) in &strokes {
        content = g.add(Node::new(
            Op::stroke(&brush(*mode), points.clone(), &mut blobs),
            vec![Some(content), None],
        ));
        ids.push(content);
    }
    let layer = g.add(Node::new(
        Op::Layer {
            props: LayerProps {
                blend: BlendMode::Multiply,
                opacity: 0.8,
                ..LayerProps::default()
            },
        },
        vec![Some(back), Some(content), None],
    ));
    g.output = Some(layer);
    assert_gpu_matches(&mut gpu, &g, &blobs, "strokes");
    let s = gpu.stats();
    assert!(s.fallback_tiles.contains_key("stroke"), "{s:?}");
    assert!(s.dispatches > 0, "{s:?}");

    // Move the paint stroke: only the tiles strokes change upload again.
    g.update(ids[0], |n| {
        n.op = Op::stroke(
            &brush(BrushMode::Paint),
            line(70.0, 300.0, 200.0, 330.0),
            &mut blobs,
        )
    })
    .unwrap();
    gpu.reset_stats();
    assert_gpu_matches(&mut gpu, &g, &blobs, "an edited stroke");
    let tiles = g.canvas().tiles().len() as u64;
    let up = gpu.stats().uploaded_tiles;
    assert!(up > 0 && up < tiles, "{up} of {tiles} tiles uploaded");
}
