use std::sync::Arc;

use lumenply_doc::{Mask, Selection};
use lumenply_render::brush_tip::builtin_tip;
use lumenply_render::paint::{paint_stroke, stroke_bounds, Brush, BrushDynamics, BrushMode, StrokePoint};
use lumenply_tiles::{Rect, Rgba, Tile, TileCoord, TileStore};

use crate::*;

/// A canvas-sized grey image: one tile shared by every coordinate.
fn grey(canvas: Rect) -> TileStore {
    let tile = Arc::new(Tile::filled(Rgba::from_straight(0.5, 0.45, 0.4, 1.0)));
    let mut s = TileStore::new();
    for c in canvas.tiles() {
        s.insert(c, tile.clone());
    }
    s
}

fn line(x0: f32, y0: f32, x1: f32, y1: f32, n: usize) -> Vec<StrokePoint> {
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            StrokePoint::new(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, 1.0)
        })
        .collect()
}

/// Bit-for-bit equality over `area`; returns how many pixels differ from
/// `before` (to show something was painted).
fn assert_bits(a: &TileStore, b: &TileStore, area: Rect, before: &TileStore) -> usize {
    let mut changed = 0;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            let bits = |p: Rgba| [p.r.to_bits(), p.g.to_bits(), p.b.to_bits(), p.a.to_bits()];
            assert_eq!(bits(p), bits(q), "differs at ({x}, {y}): {p:?} vs {q:?}");
            changed += (p != before.get_pixel(x, y)) as usize;
        }
    }
    changed
}

/// A graph: an image, then `strokes` one on another, the last one output.
fn chain(
    canvas: Rect,
    image: TileStore,
    strokes: &[(Brush, Vec<StrokePoint>)],
    blobs: &mut BlobStore,
    r: &Renderer,
) -> (Graph, Vec<NodeId>) {
    let mut g = Graph::new(canvas.w, canvas.h);
    let mut below = g.add(Node::new(
        Op::Image {
            blob: blobs.insert(&r.hasher, image),
        },
        vec![],
    ));
    let mut ids = Vec::new();
    for (brush, points) in strokes {
        below = g.add(Node::new(
            Op::stroke(brush, points.clone(), blobs),
            vec![Some(below), None],
        ));
        ids.push(below);
    }
    g.output = Some(below);
    (g, ids)
}

/// The reference: the same strokes painted one after another into a store.
fn painted(canvas: Rect, image: &TileStore, strokes: &[(Brush, Vec<StrokePoint>)]) -> TileStore {
    let mut s = image.clone();
    for (brush, points) in strokes {
        paint_stroke(&mut s, brush, points, canvas, None).unwrap();
    }
    s
}

#[test]
fn a_stroke_node_round_trips_through_json_with_its_tip_as_a_blob() {
    let canvas = Rect::new(0, 0, 300, 200);
    let chalk = builtin_tip("Chalk").unwrap();
    let brush = Brush {
        radius: 12.0,
        color: [0.9, 0.2, 0.1, 0.8],
        jitter: 0.5,
        tip: Some(chalk.clone()),
        angle: 30.0,
        dynamics: BrushDynamics {
            size_jitter: 0.4,
            count: 2,
            ..BrushDynamics::default()
        },
        ..Brush::default()
    };
    let points = vec![
        StrokePoint::new(20.0, 30.0, 1.0),
        StrokePoint::new(150.5, 90.25, 0.5).with_opacity(0.75),
        StrokePoint::new(280.0, 170.0, 0.8),
    ];
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let (mut g, ids) = chain(canvas, grey(canvas), &[(brush.clone(), points)], &mut blobs, &r);
    // A selection port: an ellipse's coverage.
    let sel = Selection::ellipse(Rect::new(40, 20, 220, 160));
    let mask = g.add(Node::new(
        Op::Mask {
            blob: Some(blobs.insert(&r.hasher, sel.coverage.tiles.clone())),
            default: 0.0,
        },
        vec![],
    ));
    g.update(ids[0], |n| n.inputs[1] = Some(mask)).unwrap();

    let json = g.to_json();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let node = &v["nodes"][ids[0].to_string()];
    println!("{}", serde_json::to_string_pretty(node).unwrap());
    assert_eq!(node["type"], "stroke");
    let tip_id = blob::tip_hash(&chalk);
    assert_eq!(
        node["brush"]["tip"],
        tip_id.to_hex(),
        "the tip is a blob reference"
    );
    assert_eq!(tip_id.to_hex().len(), 64);
    assert_eq!(node["brush"]["radius"], 12.0);
    assert_eq!(node["brush"]["angle"], 30.0);
    assert_eq!(node["brush"]["dynamics"]["count"], 2);
    assert!(node["brush"].get("mode").is_none(), "paint is the default");
    assert!(node["brush"].get("roundness").is_none(), "1 is the default");
    assert_eq!(node["points"][0], serde_json::json!([20.0, 30.0, 1.0]));
    assert_eq!(
        node["points"][1],
        serde_json::json!([150.5, 90.25, 0.5, 0.75]),
        "opacity only when not 1"
    );
    assert_eq!(node["inputs"].as_array().unwrap().len(), 2, "pixels, selection");
    // The tip blob holds the exact coverage, once.
    assert_eq!(blobs.tip_ids().count(), 1);
    assert_eq!(
        blobs.tip(&tip_id).unwrap().coverage_values(),
        chalk.coverage_values()
    );

    let back = Graph::from_json(&json).unwrap();
    assert_eq!(back, g);
    let r2 = Renderer::new();
    let (a, b) = (r.render_canvas(&g, &blobs), r2.render_canvas(&back, &blobs));
    let before = grey(canvas);
    let changed = assert_bits(&a, &b, canvas, &before);
    // And both match painting the stroke directly with the selection.
    let mut want = grey(canvas);
    let pts = match &back.node(ids[0]).unwrap().op {
        Op::Stroke { points, .. } => points.clone(),
        _ => unreachable!(),
    };
    paint_stroke(&mut want, &brush, &pts, canvas, Some(&sel)).unwrap();
    assert_eq!(assert_bits(&a, &want, canvas, &before), changed);
    assert!(changed > 1000, "{changed} pixels painted");
}

#[test]
fn equal_tips_are_one_blob_whatever_their_name() {
    let chalk = builtin_tip("Chalk").unwrap();
    let renamed = Arc::new(
        lumenply_render::paint::BrushTip::new("Other", 64, 64, chalk.coverage_values().to_vec()).unwrap(),
    );
    let mut blobs = BlobStore::new();
    let a = blobs.insert_tip(chalk.clone());
    let b = blobs.insert_tip(renamed);
    assert_eq!(a, b);
    assert_eq!(blobs.tip_ids().count(), 1);
    // One coverage value different is another tip.
    let mut cov = chalk.coverage_values().to_vec();
    cov[2080] = if cov[2080] < 0.5 {
        cov[2080] + 0.25
    } else {
        cov[2080] - 0.25
    };
    let nudged = Arc::new(lumenply_render::paint::BrushTip::new("Chalk", 64, 64, cov).unwrap());
    assert_ne!(blobs.insert_tip(nudged), a);
    assert_eq!(blobs.tip_ids().count(), 2);
    // Pixel blobs and tips are kept apart; retain drops unused tips.
    assert!(blobs.is_empty() && blobs.get(&a).is_none());
    blobs.retain(&std::collections::HashSet::from([a]));
    assert_eq!(blobs.tip_ids().collect::<Vec<_>>(), vec![&a]);
}

#[test]
fn tiles_a_stroke_misses_are_its_input_tiles_shared() {
    let canvas = Rect::new(0, 0, 1024, 512);
    let image = grey(canvas);
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let strokes: Vec<(Brush, Vec<StrokePoint>)> = (0..50)
        .map(|i| (Brush::default(), line(40.0, 40.0 + i as f32, 200.0, 60.0, 4)))
        .collect();
    let (g, _) = chain(canvas, image.clone(), &strokes, &mut blobs, &r);
    let out = r.render_canvas(&g, &blobs);
    // Every stroke stays in tile (0, 0); everywhere else the output is the
    // image's own tile, through 50 strokes, and nothing was cached for it.
    for c in canvas.tiles() {
        let same = Arc::ptr_eq(out.tile_arc(c).unwrap(), image.tile_arc(c).unwrap());
        assert_eq!(same, c != TileCoord::new(0, 0), "{c:?}");
    }
    let s = r.cache.stats();
    // 50 strokes × tile (0, 0), and the image's 8 tiles.
    assert_eq!((s.misses, s.tiles), (58, 58));
    assert_bits(&out, &painted(canvas, &image, &strokes), canvas, &image);
}

/// Stroke `k` (1-based) of the 200: inside tile `(k − 1) mod 16` of a 4×4
/// grid, except stroke 150, which runs from tile (1, 1) into (2, 1).
fn stroke(k: usize) -> (Brush, Vec<StrokePoint>) {
    let brush = Brush {
        radius: 10.0,
        color: [k as f32 / 200.0, 0.3, 1.0 - k as f32 / 200.0, 1.0],
        ..Brush::default()
    };
    if k == 150 {
        return (brush, line(300.0, 400.0, 620.0, 400.0, 6));
    }
    let i = (k - 1) % 16;
    let (cx, cy) = ((i % 4) as f32 * 256.0 + 128.0, (i / 4) as f32 * 256.0 + 128.0);
    let dy = ((k - 1) / 16) as f32 * 4.0 - 24.0;
    (brush, line(cx - 40.0, cy + dy, cx + 40.0, cy + dy, 5))
}

#[test]
fn editing_stroke_150_of_200_rerenders_only_what_it_overlaps() {
    let canvas = Rect::new(0, 0, 1024, 1024);
    let image = grey(canvas);
    let strokes: Vec<_> = (1..=200).map(stroke).collect();
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let (mut g, ids) = chain(canvas, image.clone(), &strokes, &mut blobs, &r);
    let top = *ids.last().unwrap();
    // The layer compositing the painted pixels is the output.
    let empty = g.add(Node::new(Op::Empty, vec![]));
    let layer = g.add(Node::new(
        Op::Layer {
            props: LayerProps::default(),
        },
        vec![Some(empty), Some(top), None],
    ));
    g.output = Some(layer);
    let mut history = History::new(g.clone());

    // Cold: every stroke paints its tiles once (199 strokes in one tile,
    // stroke 150 in two), then 16 tiles each for the image, the layer's
    // empty backdrop and the layer.
    let first = r.render_canvas(&g, &blobs);
    let cold = r.cache.stats().misses;
    assert_eq!(cold, 201 + 16 + 16 + 16);
    assert_eq!(r.wholes.stats().2, 0);

    // Recolour stroke 150.
    let n150 = ids[149];
    let before_keys = r.keys(&g, layer);
    g.update(n150, |n| {
        if let Op::Stroke { brush, .. } = &mut n.op {
            brush.color = [1.0, 0.9, 0.0, 1.0];
        }
    })
    .unwrap();
    history.commit(g.clone(), "Recolour");
    // Content keys change for stroke 150 and what is downstream of it
    // (strokes 151–200 and the layer), and nothing else.
    let after_keys = r.keys(&g, layer);
    let downstream = g.downstream(n150);
    assert_eq!(downstream.len(), 52);
    for (id, k) in &after_keys {
        assert_eq!(before_keys[id] != *k, downstream.contains(id), "{id}");
    }

    // The painted pixels: only tiles under strokes 150–200 that overlap
    // stroke 150's bounds are painted again.
    let b150 = stroke_bounds(&strokes[149].0, &strokes[149].1, canvas).tiles();
    assert_eq!(b150, vec![TileCoord::new(1, 1), TileCoord::new(2, 1)]);
    let overlapping: usize = (150..=200)
        .map(|k| {
            let (b, p) = &strokes[k - 1];
            stroke_bounds(b, p, canvas)
                .tiles()
                .iter()
                .filter(|t| b150.contains(t))
                .count()
        })
        .sum();
    // Tile (1, 1): strokes 150, 166, 182, 198; tile (2, 1): 150, 151,
    // 167, 183, 199.
    assert_eq!(overlapping, 9);
    let s0 = r.cache.stats().misses;
    let painted_top = r.render_node(&g, &blobs, top, canvas);
    assert_eq!(r.cache.stats().misses - s0, overlapping as u64);
    // The layer above composites all 16 tiles again; no stroke repaints.
    let s1 = r.cache.stats().misses;
    let edited = r.render_canvas(&g, &blobs);
    assert_eq!(r.cache.stats().misses - s1, 16);

    let mut recoloured = strokes.clone();
    recoloured[149].0.color = [1.0, 0.9, 0.0, 1.0];
    let want = painted(canvas, &image, &recoloured);
    assert_bits(&painted_top, &want, canvas, &image);
    assert_bits(&edited, &want, canvas, &image);
    assert_eq!(
        edited.get_pixel(450, 400),
        Rgba::from_straight(1.0, 0.9, 0.0, 1.0)
    );

    // Undo: the old tiles are all still cached.
    assert!(history.undo());
    let s2 = r.cache.stats().misses;
    let undone = r.render_canvas(history.current(), &blobs);
    assert_eq!(r.cache.stats().misses, s2, "undo repaints nothing");
    assert_bits(&undone, &first, canvas, &image);
    assert_bits(&first, &painted(canvas, &image, &strokes), canvas, &image);
}

#[test]
fn smudge_and_blur_paint_their_region_once_and_only_when_what_they_read_changes() {
    let canvas = Rect::new(0, 0, 768, 512);
    // Stripes, so smudging and blurring have edges to work on.
    let mut image = TileStore::new();
    for y in 0..512 {
        for x in 0..768 {
            let v = if (x / 7 + y / 11) % 2 == 0 { 0.9 } else { 0.1 };
            image.set_pixel(x, y, Rgba::from_straight(v, 0.5, 1.0 - v, 1.0));
        }
    }
    let mode = |mode: BrushMode, strength: f32| Brush {
        radius: 9.0,
        color: [0.2, 0.6, 0.9, strength],
        mode,
        ..Brush::default()
    };
    let strokes = vec![
        // Tile (0, 0); a smudge from (0, 0) across into (1, 0).
        (mode(BrushMode::Paint, 1.0), line(60.0, 60.0, 160.0, 90.0, 4)),
        (mode(BrushMode::Smudge, 0.8), line(150.0, 100.0, 330.0, 120.0, 6)),
        // Tile (2, 1), far away: a paint stroke, then a blur over it.
        (mode(BrushMode::Paint, 1.0), line(560.0, 330.0, 700.0, 380.0, 4)),
        (mode(BrushMode::Blur, 1.0), line(550.0, 350.0, 710.0, 360.0, 5)),
        (mode(BrushMode::Sharpen, 1.0), line(40.0, 180.0, 120.0, 200.0, 3)),
    ];
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let (mut g, ids) = chain(canvas, image.clone(), &strokes, &mut blobs, &r);
    let out = r.render_canvas(&g, &blobs);
    // Each region stroke is painted once although the smudge spans two
    // tiles that are rendered in parallel.
    assert_eq!(r.wholes.stats().2, 3);
    assert_bits(&out, &painted(canvas, &image, &strokes), canvas, &image);

    // Recolour the far paint stroke: the blur over it reads what changed
    // and is painted again; the smudge and the sharpen read nothing it
    // changed and are not.
    g.update(ids[2], |n| {
        if let Op::Stroke { brush, .. } = &mut n.op {
            brush.color = [0.9, 0.1, 0.1, 1.0];
        }
    })
    .unwrap();
    let m0 = r.cache.stats().misses;
    let edited = r.render_canvas(&g, &blobs);
    assert_eq!(r.wholes.stats().2, 4);
    // The paint stroke's tile and the blur's tile.
    assert_eq!(r.cache.stats().misses - m0, 2);
    let mut want = strokes.clone();
    want[2].0.color = [0.9, 0.1, 0.1, 1.0];
    let want = painted(canvas, &image, &want);
    assert_bits(&edited, &want, canvas, &image);

    // A planned render (node by node) paints the same.
    let mut planned = Renderer::new();
    planned.plan = true;
    assert_bits(&planned.render_canvas(&g, &blobs), &want, canvas, &image);
    assert_eq!(planned.wholes.stats().2, 3);
}

#[test]
fn history_and_empty_strokes_change_nothing_and_a_selection_limits_one() {
    let canvas = Rect::new(0, 0, 256, 256);
    let image = grey(canvas);
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let history = Brush {
        mode: BrushMode::History,
        ..Brush::default()
    };
    let strokes = vec![
        (history, line(10.0, 10.0, 200.0, 200.0, 3)),
        (Brush::default(), Vec::new()),
    ];
    let (g, _) = chain(canvas, image.clone(), &strokes, &mut blobs, &r);
    let out = r.render_canvas(&g, &blobs);
    let c = TileCoord::new(0, 0);
    assert!(Arc::ptr_eq(out.tile_arc(c).unwrap(), image.tile_arc(c).unwrap()));
    assert_eq!(r.render_node(&g, &blobs, g.output.unwrap(), canvas).len(), 1);

    // A selection of the left half: a stroke across it paints only there.
    let (mut g, ids) = chain(
        canvas,
        image.clone(),
        &[(Brush::default(), line(20.0, 100.0, 230.0, 100.0, 3))],
        &mut blobs,
        &r,
    );
    let sel = Selection::rect(Rect::new(0, 0, 128, 256));
    let mask = g.add(Node::new(
        Op::Mask {
            blob: Some(blobs.insert(&r.hasher, sel.coverage.tiles.clone())),
            default: 0.0,
        },
        vec![],
    ));
    g.update(ids[0], |n| n.inputs[1] = Some(mask)).unwrap();
    let out = r.render_canvas(&g, &blobs);
    assert_eq!(out.get_pixel(100, 100), Rgba::new(0.0, 0.0, 0.0, 1.0));
    assert_eq!(out.get_pixel(150, 100), image.get_pixel(150, 100));
    // An inverted selection (default 1 outside its tiles) the other way.
    let mut inv = sel.clone();
    inv.invert();
    let inverted = g.add(Node::new(
        Op::Mask {
            blob: Some(blobs.insert(&r.hasher, inv.coverage.tiles.clone())),
            default: inv.coverage.default,
        },
        vec![],
    ));
    g.update(ids[0], |n| n.inputs[1] = Some(inverted)).unwrap();
    let out = r.render_canvas(&g, &blobs);
    let mut want = image.clone();
    paint_stroke(
        &mut want,
        &Brush::default(),
        &line(20.0, 100.0, 230.0, 100.0, 3),
        canvas,
        Some(&Selection {
            coverage: Mask {
                tiles: inv.coverage.tiles.clone(),
                default: inv.coverage.default,
                enabled: true,
            },
        }),
    )
    .unwrap();
    assert_bits(&out, &want, canvas, &image);
    assert_eq!(out.get_pixel(150, 100), Rgba::new(0.0, 0.0, 0.0, 1.0));
    assert_eq!(out.get_pixel(100, 100), image.get_pixel(100, 100));
}
