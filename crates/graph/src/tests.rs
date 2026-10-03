use std::sync::Arc;

use lumenply_doc::{Adjustment, BlendMode, Document, Filter, Layer, LayerContent, Mask, ShadowFx};
use lumenply_tiles::{Rect, Rgba, TileStore};

use crate::*;

/// A pixel layer painted with `color` (straight linear RGBA) over `area`.
fn painted(doc: &mut Document, name: &str, area: Rect, color: [f32; 4]) -> Layer {
    let mut l = Layer::pixel(doc.alloc_id(), name);
    let LayerContent::Pixel(store) = &mut l.content else {
        unreachable!()
    };
    let p = Rgba::from_straight(color[0], color[1], color[2], color[3]);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            store.set_pixel(x, y, p);
        }
    }
    l
}

/// A half-revealing mask: the left half of `area` shown, the rest hidden.
fn half_mask(area: Rect) -> Mask {
    let mut m = Mask::hide_all();
    for y in area.y..area.bottom() {
        for x in area.x..area.x + area.w as i32 / 2 {
            m.set_value(x, y, 1.0);
        }
    }
    m
}

/// A document using every compositing feature the lowering handles:
/// blend modes, opacity, fill, masks, effects, adjustment and filter
/// layers, isolated and pass-through groups, and a clip chain.
fn busy_document() -> Document {
    let mut doc = Document::new(300, 280);
    let canvas = doc.canvas();
    let bg = painted(&mut doc, "Background", canvas, [0.9, 0.85, 0.7, 1.0]);
    doc.add_layer(bg);

    let mut red = painted(&mut doc, "Red", Rect::new(20, 30, 200, 120), [0.8, 0.1, 0.1, 0.9]);
    red.blend = BlendMode::Multiply;
    red.opacity = 0.6;
    red.mask = Some(half_mask(Rect::new(20, 30, 200, 120)));
    doc.add_layer(red);

    let mut shadowed = painted(
        &mut doc,
        "Shadowed",
        Rect::new(150, 140, 90, 70),
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

    // A clip chain: base, a pixel member in Screen and an adjustment member.
    let base = painted(
        &mut doc,
        "Base",
        Rect::new(40, 160, 120, 90),
        [0.2, 0.7, 0.3, 1.0],
    );
    doc.add_layer(base);
    let mut member = painted(
        &mut doc,
        "Member",
        Rect::new(0, 190, 300, 40),
        [0.9, 0.9, 0.2, 1.0],
    );
    member.clip = true;
    member.blend = BlendMode::Screen;
    doc.add_layer(member);
    let mut inv = Layer::adjustment(doc.alloc_id(), Adjustment::Invert);
    inv.clip = true;
    inv.opacity = 0.4;
    doc.add_layer(inv);

    // A pass-through group with an adjustment inside, at 70%.
    let gid = doc.alloc_id();
    let mut pass = Layer::group(gid, "Pass");
    pass.pass_through = true;
    pass.opacity = 0.7;
    let mut inner = Layer::adjustment(doc.alloc_id(), Adjustment::Invert);
    inner.mask = Some(half_mask(Rect::new(0, 0, 300, 280)));
    let inner_px = painted(
        &mut doc,
        "Inner",
        Rect::new(200, 10, 60, 60),
        [0.5, 0.2, 0.8, 1.0],
    );
    if let LayerContent::Group(children) = &mut pass.content {
        children.push(inner_px);
        children.push(inner);
    }
    doc.add_layer(pass);

    // An isolated group in Overlay.
    let mut iso = Layer::group(doc.alloc_id(), "Isolated");
    iso.blend = BlendMode::Overlay;
    let a = painted(&mut doc, "A", Rect::new(100, 0, 80, 280), [0.1, 0.1, 0.6, 1.0]);
    if let LayerContent::Group(children) = &mut iso.content {
        children.push(a);
    }
    doc.add_layer(iso);

    // A hidden layer must change nothing.
    let mut hidden = painted(&mut doc, "Hidden", canvas, [0.0, 0.0, 0.0, 1.0]);
    hidden.visible = false;
    doc.add_layer(hidden);

    let mut blur = Layer::filter(doc.alloc_id(), Filter::GaussianBlur { radius: 3.0 });
    blur.opacity = 0.8;
    doc.add_layer(blur);
    doc
}

fn assert_same(a: &TileStore, b: &TileStore, canvas: Rect) {
    let mut worst = 0.0f32;
    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            for (u, v) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
                worst = worst.max((u - v).abs());
            }
        }
    }
    assert!(worst <= 1e-6, "graph and layer tree differ by {worst}");
}

#[test]
fn a_lowered_document_renders_exactly_like_the_layer_tree() {
    let doc = busy_document();
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &r.hasher);
    low.graph.validate().unwrap();
    let ours = r.render_canvas(&low.graph, &blobs);
    let reference = lumenply_render::composite(&doc);
    assert_same(&ours, &reference, doc.canvas());
    // Every layer is reachable through its node.
    for l in doc.layers() {
        assert!(low.layer_nodes.contains_key(&l.id), "{}", l.name);
    }
}

#[test]
fn graphs_round_trip_through_json_with_type_params_and_inputs() {
    let doc = busy_document();
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let g = lower(&doc, &mut blobs, &r.hasher).graph;
    let json = g.to_json();
    let back = Graph::from_json(&json).unwrap();
    assert_eq!(back, g);
    // A node reads as type + parameters + input references.
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let red = v["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["name"] == "Red")
        .unwrap();
    assert_eq!(red["type"], "layer");
    assert_eq!(red["blend"], "multiply");
    assert!((red["opacity"].as_f64().unwrap() - 0.6).abs() < 1e-6);
    assert_eq!(
        red["inputs"].as_array().unwrap().len(),
        3,
        "backdrop, content, mask"
    );
    assert!(red["inputs"][0].as_str().unwrap().starts_with('n'));
    // Same pixels after the round trip, from the same blobs.
    let r2 = Renderer::new();
    assert_same(
        &r2.render_canvas(&back, &blobs),
        &r.render_canvas(&g, &blobs),
        doc.canvas(),
    );
}

#[test]
fn editing_a_node_changes_its_key_and_everything_downstream_only() {
    let doc = busy_document();
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &r.hasher);
    let mut g = low.graph.clone();
    let out = g.output.unwrap();
    let red = low.layer_nodes[&doc.layers()[1].id];
    let before = r.keys(&g, out);
    g.update(red, |n| {
        if let Op::Layer { props } = &mut n.op {
            props.opacity = 0.3;
        }
    })
    .unwrap();
    let after = r.keys(&g, out);
    let downstream = g.downstream(red);
    for (id, k) in &after {
        assert_eq!(
            before[id] != *k,
            downstream.contains(id),
            "{id}: only nodes downstream of the edit change key"
        );
    }
    // Renaming changes nothing at all.
    let mut renamed = low.graph.clone();
    renamed.update(red, |n| n.name = Some("Crimson".into())).unwrap();
    assert_eq!(r.keys(&renamed, out), before);
}

#[test]
fn a_second_render_and_an_undo_are_served_from_the_cache() {
    let doc = busy_document();
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &r.hasher);
    let mut history = History::new(low.graph.clone());
    let first = r.render_canvas(history.current(), &blobs);
    let s1 = r.cache.stats();
    assert!(s1.misses > 0);

    // Unchanged: every output tile is a hit, nothing recomputes.
    let again = r.render_canvas(history.current(), &blobs);
    let s2 = r.cache.stats();
    assert_eq!(s2.misses, s1.misses, "no work the second time");
    assert_same(&again, &first, doc.canvas());

    // Edit the top filter: only its own tiles recompute (the layers below
    // it come from the cache).
    let mut g = history.current().clone();
    let top = g.output.unwrap();
    g.update(top, |n| {
        if let Op::FilterLayer { opacity, .. } = &mut n.op {
            *opacity = 0.2;
        }
    })
    .unwrap();
    history.commit(g, "Filter opacity");
    let edited = r.render_canvas(history.current(), &blobs);
    let s3 = r.cache.stats();
    let tiles = doc.canvas().tiles().len() as u64;
    assert_eq!(
        s3.misses - s2.misses,
        tiles,
        "one new tile per canvas tile, for the edited node"
    );
    let reference = {
        let mut d = doc.clone();
        d.layers_mut().last_mut().unwrap().opacity = 0.2;
        lumenply_render::composite(&d)
    };
    assert_same(&edited, &reference, doc.canvas());

    // Undo: the old version's tiles are still cached.
    assert!(history.undo());
    let undone = r.render_canvas(history.current(), &blobs);
    assert_eq!(r.cache.stats().misses, s3.misses, "undo recomputes nothing");
    assert_same(&undone, &first, doc.canvas());
    assert!(history.redo());
    assert_eq!(history.labels(), (vec!["Open", "Filter opacity"], 1));
}

#[test]
fn identical_pixels_are_one_blob_and_unchanged_tiles_are_hashed_once() {
    let mut doc = Document::new(512, 256);
    let a = painted(&mut doc, "A", Rect::new(0, 0, 512, 256), [0.2, 0.3, 0.4, 1.0]);
    let mut b = a.clone();
    b.id = doc.alloc_id();
    doc.add_layer(a);
    doc.add_layer(b);
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    lower(&doc, &mut blobs, &r.hasher);
    assert_eq!(blobs.len(), 1, "two layers, the same pixels, one blob");
    let LayerContent::Pixel(store) = &doc.layers()[0].content else {
        unreachable!()
    };
    let t = store.tile_arc(lumenply_tiles::TileCoord::new(0, 0)).unwrap();
    let h = r.hasher.tile(t);
    assert_eq!(h, blob::tile_hash(t));
    assert_eq!(Hash::from_hex(&h.to_hex()), Some(h));
}

#[test]
fn a_tile_being_computed_elsewhere_never_makes_a_caller_wait() {
    let cache = TileCache::default();
    let key = Hash([7; 32]);
    let c = lumenply_tiles::TileCoord::new(0, 0);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let shared = &cache;
    std::thread::scope(|s| {
        let slow = s.spawn(move || {
            shared.get_or_compute(key, c, || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Some(Arc::new(lumenply_tiles::Tile::new()))
            })
        });
        started_rx.recv().unwrap();
        // The first computation is still running: this one doesn't wait
        // for it (waiting can deadlock under rayon) but computes its own.
        let mine = cache.get_or_compute(key, c, || {
            Some(Arc::new(lumenply_tiles::Tile::filled(Rgba::WHITE)))
        });
        assert_eq!(mine.unwrap().get(0, 0), Rgba::WHITE);
        assert_eq!(cache.stats().duplicates, 1);
        release_tx.send(()).unwrap();
        slow.join().unwrap();
    });
    // The first result is the one kept.
    let kept = cache.get_or_compute(key, c, || unreachable!("cached"));
    assert_eq!(kept.unwrap().get(0, 0), Rgba::TRANSPARENT);
}

#[test]
fn a_planned_render_computes_every_tile_once() {
    let doc = busy_document();
    let mut r = Renderer::new();
    r.plan = true;
    let mut blobs = BlobStore::new();
    let g = lower(&doc, &mut blobs, &r.hasher).graph;
    let out = r.render_canvas(&g, &blobs);
    assert_eq!(
        r.cache.stats().duplicates,
        0,
        "inputs are ready before their consumers run"
    );
    assert_same(&out, &lumenply_render::composite(&doc), doc.canvas());
}

#[test]
fn concurrent_renders_of_filters_and_effects_never_hang() {
    // Many renders at once, each with a cold cache: the case that used to
    // deadlock (a thread waiting for a tile its own stack was computing).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let doc = busy_document();
        let reference = lumenply_render::composite(&doc);
        use rayon::prelude::*;
        (0..48).into_par_iter().for_each(|_| {
            let r = Renderer::new();
            let mut blobs = BlobStore::new();
            let g = lower(&doc, &mut blobs, &r.hasher).graph;
            // Ask for the canvas without planning first, from several
            // threads at once, to stress the cache directly.
            let out = r.render_node(&g, &blobs, g.output.unwrap(), doc.canvas());
            assert_same(&out, &reference, doc.canvas());
        });
        tx.send(()).unwrap();
    });
    rx.recv_timeout(std::time::Duration::from_secs(120))
        .expect("renders finished (no deadlock)");
}

#[test]
fn the_budget_evicts_least_recently_used_tiles() {
    // Room for about three f32 tiles (1 MiB each).
    let cache = TileCache::with_budget(3 << 20);
    let c = lumenply_tiles::TileCoord::new(0, 0);
    for i in 0..6u8 {
        cache.get_or_compute(Hash([i; 32]), c, || Some(Arc::new(lumenply_tiles::Tile::new())));
    }
    let s = cache.stats();
    assert!(s.bytes <= 3 << 20, "{s:?}");
    assert!(cache.contains(Hash([5; 32]), c), "the newest stays");
    assert!(!cache.contains(Hash([0; 32]), c), "the oldest went");
}

#[test]
fn cycles_dangling_inputs_and_newer_files_are_refused() {
    let mut g = Graph::new(10, 10);
    let a = g.add(Node::new(Op::Empty, vec![]));
    let b = g.add(Node::new(
        Op::Layer {
            props: LayerProps::default(),
        },
        vec![Some(a), None, None],
    ));
    g.output = Some(b);
    g.validate().unwrap();
    g.update(a, |n| {
        n.op = Op::Layer {
            props: LayerProps::default(),
        };
        n.inputs = vec![Some(b)];
    })
    .unwrap();
    assert!(matches!(g.validate(), Err(GraphError::Cycle(_))));
    let mut d = Graph::new(10, 10);
    let x = d.add(Node::new(
        Op::Layer {
            props: LayerProps::default(),
        },
        vec![Some(NodeId(99))],
    ));
    d.output = Some(x);
    assert!(matches!(d.validate(), Err(GraphError::DanglingInput { .. })));
    let newer = Graph::new(1, 1)
        .to_json()
        .replace("\"format\": 3", "\"format\": 9");
    assert_eq!(Graph::from_json(&newer), Err(GraphError::TooNew(9)));
}
