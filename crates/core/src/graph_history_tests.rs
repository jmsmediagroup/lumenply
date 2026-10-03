//! The editor's history as graph versions (ADR 0025, stage 3): node reuse
//! across edits, undo returning the identical version, memory accounting,
//! coalescing, the native `translate` path, and the graph rendering the
//! document through a long random sequence of edits.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lumenply_doc::{Adjustment, BlendMode, Document, Filter, Layer, LayerContent, LayerId, Selection};
use lumenply_graph::{lower, BlobStore, Graph, NodeId, Op, Renderer};
use lumenply_tiles::{Raster, Rect, Rgba, TileStore, TILE_PIXELS};

use crate::commands::*;
use crate::layer_ops::{DuplicateLayer, MergeDown};
use crate::{compact_storage, Command, Editor};

/// One compact (16-bit) tile.
const TILE: usize = TILE_PIXELS * 8;

fn raster(w: u32, h: u32, seed: u32) -> Raster {
    let mut r = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = |k: u32| ((x * 7 + y * 13 + seed * 31 + k * 17) % 97) as f32 / 96.0;
            r.set(x, y, Rgba::from_straight(v(0), v(1), v(2), 0.5 + v(3) / 2.0));
        }
    }
    r
}

fn dab(layer: LayerId, x: f32, y: f32, color: [f32; 4]) -> PaintStroke {
    PaintStroke {
        layer,
        brush: Brush {
            radius: 10.0,
            hardness: 1.0,
            color,
            ..Brush::default()
        },
        points: vec![StrokePoint::new(x, y, 1.0)],
    }
}

fn id_of(ed: &Editor, name: &str) -> LayerId {
    let mut found = None;
    ed.doc().for_each_layer(|l| {
        if l.name == name {
            found = Some(l.id);
        }
    });
    found.unwrap_or_else(|| panic!("no layer {name}"))
}

/// Same pixels, tile for tile (same tiles present, equal contents).
fn assert_same_store(a: &TileStore, b: &TileStore, what: &str) {
    let ca: BTreeSet<_> = a.coords().map(|c| (c.x, c.y)).collect();
    let cb: BTreeSet<_> = b.coords().map(|c| (c.x, c.y)).collect();
    assert_eq!(ca, cb, "{what}: tiles present");
    for c in a.coords() {
        assert!(
            a.tile(c).unwrap() == b.tile(c).unwrap(),
            "{what}: tile {c:?} differs"
        );
    }
}

/// The same tiles, each pixel within `tol` per channel.
fn assert_close_store(a: &TileStore, b: &TileStore, tol: f32, what: &str) {
    let ca: BTreeSet<_> = a.coords().map(|c| (c.x, c.y)).collect();
    let cb: BTreeSet<_> = b.coords().map(|c| (c.x, c.y)).collect();
    assert_eq!(ca, cb, "{what}: tiles present");
    for c in a.coords() {
        let (p, q) = (a.tile(c).unwrap().pixels(), b.tile(c).unwrap().pixels());
        let worst = p
            .iter()
            .zip(q.iter())
            .flat_map(|(u, v)| [u.r - v.r, u.g - v.g, u.b - v.b, u.a - v.a])
            .fold(0.0f32, |m, d| m.max(d.abs()));
        assert!(worst <= tol, "{what}: tile {c:?} differs by {worst}");
    }
}

fn assert_same_layers(a: &[Layer], b: &[Layer]) {
    compare_layers(a, b, 0.0);
}

/// Layer trees with the same settings and pixels; caches derived from a
/// layer's parameters (text, smart, fill and shape pixels, smart filter
/// results) may differ by `derived` per channel (0: not at all).
fn compare_layers(a: &[Layer], b: &[Layer], derived: f32) {
    let derived_store = |p: &TileStore, q: &TileStore, n: &str| {
        if derived == 0.0 {
            assert_same_store(p, q, n)
        } else {
            assert_close_store(p, q, derived, n)
        }
    };
    assert_eq!(a.len(), b.len(), "layer count");
    for (x, y) in a.iter().zip(b) {
        let n = &x.name;
        assert_eq!((x.id, &x.name), (y.id, &y.name));
        assert_eq!(
            (x.visible, x.opacity, x.blend, x.fill_opacity),
            (y.visible, y.opacity, y.blend, y.fill_opacity),
            "{n}"
        );
        assert_eq!(
            (x.clip, x.pass_through, x.collapsed, x.locks),
            (y.clip, y.pass_through, y.collapsed, y.locks),
            "{n}"
        );
        assert_eq!(x.effects, y.effects, "{n}");
        assert!(x.smart_filters == y.smart_filters, "{n}");
        match (&x.mask, &y.mask) {
            (None, None) => {}
            (Some(p), Some(q)) => {
                assert_eq!((p.default, p.enabled), (q.default, q.enabled), "{n}");
                assert_same_store(&p.tiles, &q.tiles, n);
            }
            _ => panic!("{n}: mask presence differs"),
        }
        assert_eq!(
            std::mem::discriminant(&x.content),
            std::mem::discriminant(&y.content),
            "{n}"
        );
        match (&x.smart_filters.cache, &y.smart_filters.cache) {
            (Some(p), Some(q)) => derived_store(&p.store, &q.store, n),
            (p, q) => assert_eq!(p.is_some(), q.is_some(), "{n}: smart filter cache"),
        }
        match (&x.content, &y.content) {
            (LayerContent::Group(p), LayerContent::Group(q)) => compare_layers(p, q, derived),
            (LayerContent::Adjustment(p), LayerContent::Adjustment(q)) => assert_eq!(p, q),
            (LayerContent::Filter(p), LayerContent::Filter(q)) => assert_eq!(p, q),
            (LayerContent::Pixel(p), LayerContent::Pixel(q)) => assert_same_store(p, q, n),
            (LayerContent::Text(p), LayerContent::Text(q)) => assert!(p == q, "{n}: text"),
            (LayerContent::Fill(p), LayerContent::Fill(q)) => assert_eq!(p.fill, q.fill, "{n}"),
            (LayerContent::Shape(p), LayerContent::Shape(q)) => {
                assert_eq!(
                    (&p.geometry, &p.fill, &p.stroke),
                    (&q.geometry, &q.fill, &q.stroke),
                    "{n}"
                );
                assert_eq!(p.transform, q.transform, "{n}");
            }
            (LayerContent::Smart(p), LayerContent::Smart(q)) => {
                assert_same_store(&p.source, &q.source, n);
                assert_eq!(p.transform, q.transform, "{n}");
            }
            _ => unreachable!("same kinds"),
        }
        if !matches!(x.content, LayerContent::Pixel(_) | LayerContent::Group(_)) {
            match (x.content_store(), y.content_store()) {
                (Some(p), Some(q)) => derived_store(p, q, n),
                (p, q) => assert_eq!(p.is_some(), q.is_some(), "{n}"),
            }
        }
    }
}

/// Two documents are the same: settings, layer tree, pixels and the
/// state beside them.
fn assert_same_doc(a: &Document, b: &Document) {
    assert_eq!((a.width, a.height, a.next_id()), (b.width, b.height, b.next_id()));
    assert_eq!((a.float_mode, a.resolution), (b.float_mode, b.resolution));
    assert_eq!(a.guides, b.guides);
    assert_eq!(a.work_path, b.work_path);
    assert_eq!(a.saved_paths, b.saved_paths);
    assert_eq!(a.patterns, b.patterns);
    for (p, q) in [
        (&a.selection, &b.selection),
        (&a.last_selection, &b.last_selection),
    ] {
        assert_eq!(p.is_some(), q.is_some(), "selection");
        if let (Some(p), Some(q)) = (p, q) {
            assert_same_store(&p.coverage.tiles, &q.coverage.tiles, "selection");
        }
    }
    assert_eq!(a.saved_selections.len(), b.saved_selections.len());
    assert_same_layers(a.layers(), b.layers());
}

/// The largest per-channel difference between two renders over `canvas`.
fn max_diff(a: &TileStore, b: &TileStore, canvas: Rect) -> f32 {
    let mut worst = 0.0f32;
    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            for (u, v) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
                worst = worst.max((u - v).abs());
            }
        }
    }
    worst
}

/// Nodes of `b` new since `a` (ids `a` lacks), gone (ids `b` lacks), and
/// kept under the same id but as a new `Arc`.
fn diff(a: &Graph, b: &Graph) -> (Vec<NodeId>, Vec<NodeId>, Vec<NodeId>) {
    let ids = |g: &Graph| g.nodes().map(|(id, _)| id).collect::<BTreeSet<_>>();
    let (ia, ib) = (ids(a), ids(b));
    let added = ib.difference(&ia).copied().collect();
    let removed = ia.difference(&ib).copied().collect();
    let rearced = ia
        .intersection(&ib)
        .copied()
        .filter(|id| !Arc::ptr_eq(a.node_arc(*id).unwrap(), b.node_arc(*id).unwrap()))
        .collect();
    (added, removed, rearced)
}

/// (compositing node, content node, mask node) of every layer.
fn layer_nodes(ed: &Editor) -> BTreeMap<LayerId, (NodeId, Option<NodeId>, Option<NodeId>)> {
    let mut out = BTreeMap::new();
    ed.doc_state().for_each_layer(|r| {
        out.insert(r.layer.id, (r.node, r.content, r.mask));
    });
    out
}

/// A document with a masked layer, an adjustment, a group and a clip chain.
fn layered_editor() -> Editor {
    let mut ed = Editor::new(Document::new(600, 400));
    ed.execute(&AddPixelLayer::from_raster("Bottom", raster(600, 400, 1), 0, 0))
        .unwrap();
    ed.execute(&AddPixelLayer::from_raster(
        "Middle",
        raster(300, 200, 2),
        100,
        100,
    ))
    .unwrap();
    let middle = id_of(&ed, "Middle");
    ed.execute(&AddMask { layer: middle }).unwrap();
    ed.execute(&PaintMask {
        layer: middle,
        brush: Brush {
            radius: 30.0,
            color: [0.0, 0.0, 0.0, 1.0],
            ..Brush::default()
        },
        points: vec![StrokePoint::new(150.0, 150.0, 1.0)],
    })
    .unwrap();
    ed.execute(&AddAdjustmentLayer::new(Adjustment::Invert)).unwrap();
    ed.execute(&AddPixelLayer::from_raster("Inner", raster(200, 200, 3), 300, 50))
        .unwrap();
    let inner = id_of(&ed, "Inner");
    ed.execute(&GroupLayers {
        layers: vec![inner],
        name: "Group".into(),
    })
    .unwrap();
    ed.execute(&AddPixelLayer::from_raster("Base", raster(200, 150, 4), 50, 200))
        .unwrap();
    ed.execute(&AddPixelLayer::from_raster("Clipped", raster(600, 60, 5), 0, 250))
        .unwrap();
    let clipped = id_of(&ed, "Clipped");
    ed.execute(&SetClipped {
        layer: clipped,
        clip: true,
    })
    .unwrap();
    ed.execute(&AddPixelLayer::from_raster("Top", raster(100, 100, 6), 450, 250))
        .unwrap();
    ed
}

#[test]
fn an_edit_to_one_layer_reuses_every_other_layers_nodes() {
    let mut ed = layered_editor();
    let middle = id_of(&ed, "Middle");
    let g0 = ed.graph_arc();
    let n0 = layer_nodes(&ed);
    let k0 = ed.renderer().keys(&g0, g0.output.unwrap());

    ed.execute(&dab(middle, 200.0, 200.0, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let g1 = ed.graph_arc();
    let n1 = layer_nodes(&ed);
    let k1 = ed.renderer().keys(&g1, g1.output.unwrap());

    // The stroke goes onto the layer's content chain: a `stroke` node on
    // the old pixels and a `compact` node on it are new, the compositing
    // node is re-made under its id with the new content, and every other
    // node is the very same `Arc`.
    let (added, removed, rearced) = diff(&g0, &g1);
    let (m_node, m_content, m_mask) = n0[&middle];
    let chain_top = n1[&middle].1.unwrap();
    let stroke = g1.node(chain_top).unwrap().input(0).unwrap();
    assert_eq!(g1.node(chain_top).unwrap().op, Op::Compact);
    assert!(matches!(g1.node(stroke).unwrap().op, Op::Stroke { .. }));
    assert_eq!(g1.node(stroke).unwrap().input(0), m_content);
    assert_eq!(added, vec![stroke, chain_top]);
    assert!(removed.is_empty());
    assert_eq!(rearced, vec![m_node]);
    assert_eq!(n1[&middle], (m_node, Some(chain_top), m_mask));
    assert_eq!(g1.len(), g0.len() + 2);

    // Every other layer keeps its nodes, and its pixels and mask keep
    // their content keys.
    for (id, (node, content, mask)) in &n0 {
        if *id == middle {
            continue;
        }
        assert_eq!(n1[id], (*node, *content, *mask), "layer {id}");
        for n in [*content, *mask].into_iter().flatten() {
            assert_eq!(k0[&n], k1[&n], "layer {id}: node {n}");
        }
    }
    // The mask of the stroked layer is untouched too.
    assert_eq!(k0[&m_mask.unwrap()], k1[&m_mask.unwrap()]);
    // Below the edit nothing changes key; from the edit up everything does.
    let bottom = n0[&id_of(&ed, "Bottom")].0;
    assert_eq!(k0[&bottom], k1[&bottom]);
    let top = n0[&id_of(&ed, "Top")].0;
    assert_ne!(k0[&top], k1[&top]);
    assert_ne!(k0[&m_node], k1[&m_node]);

    // A property change re-creates one node and adds or drops none.
    let top_id = id_of(&ed, "Top");
    ed.execute(&SetOpacity {
        layer: top_id,
        opacity: 0.5,
    })
    .unwrap();
    let g2 = ed.graph_arc();
    let (added, removed, rearced) = diff(&g1, &g2);
    assert_eq!((added.len(), removed.len()), (0, 0));
    assert_eq!(rearced, vec![top]);
    let k2 = ed.renderer().keys(&g2, g2.output.unwrap());
    let changed: Vec<NodeId> = k1.keys().copied().filter(|n| k1[n] != k2[n]).collect();
    assert_eq!(changed, vec![top], "only the top layer's node changes key");

    // A command that isn't a graph op (a fill) gives the layer new pixels:
    // one `image` node replaces the whole chain.
    ed.execute(&Fill {
        layer: middle,
        color: [0.1, 0.2, 0.3, 1.0],
    })
    .unwrap();
    let g3 = ed.graph_arc();
    let (added, removed, rearced) = diff(&g2, &g3);
    assert_eq!(removed, vec![m_content.unwrap(), stroke, chain_top]);
    assert_eq!(added.len(), 1);
    assert!(matches!(g3.node(added[0]).unwrap().op, Op::Image { .. }));
    assert_eq!(rearced, vec![m_node]);
}

fn text(s: &str) -> lumenply_doc::TextLayer {
    lumenply_doc::TextLayer::new(s, 40.0, 60.0, 28.0, [1.0, 1.0, 0.2, 1.0])
}

#[test]
fn editing_a_content_layer_makes_one_new_op_node_and_no_blob() {
    let mut ed = layered_editor();
    ed.execute(&AddTextLayer {
        text: text("Hello"),
        above: None,
    })
    .unwrap();
    let words = ed.doc().layers().last().unwrap().id;
    let mut add = AddFillLayer::new(lumenply_doc::Fill::Solid {
        color: [0.1, 0.5, 0.9],
    });
    add.mask_selection = false;
    ed.execute(&add).unwrap();
    let paint = ed.doc().layers().last().unwrap().id;
    ed.execute(&SetOpacity {
        layer: paint,
        opacity: 0.4,
    })
    .unwrap();

    // Retyping the text: one new `text` node (and the `compact` after it:
    // the editor keeps the re-rendered glyphs at 16 bits) replaces the old
    // pair, the layer node is re-made under the same id, and no pixels are
    // stored.
    let (g0, n0, blobs0) = (ed.graph_arc(), layer_nodes(&ed), ed.blobs().len());
    let chain = |g: &Graph, top: NodeId| -> Vec<NodeId> {
        let n = g.node(top).unwrap();
        assert_eq!(n.op, Op::Compact);
        vec![n.input(0).unwrap(), top]
    };
    let old = chain(&g0, n0[&words].1.unwrap());
    ed.execute(&SetText {
        layer: words,
        text: text("Hello, world"),
    })
    .unwrap();
    let g1 = ed.graph_arc();
    let (added, removed, rearced) = diff(&g0, &g1);
    assert_eq!(removed, old);
    let new = chain(&g1, layer_nodes(&ed)[&words].1.unwrap());
    assert_eq!(added, new);
    match &g1.node(new[0]).unwrap().op {
        Op::Text { text } => assert_eq!(text.text, "Hello, world"),
        op => panic!("{op:?}"),
    }
    assert_eq!(rearced, vec![n0[&words].0]);
    assert_eq!(ed.blobs().len(), blobs0, "no new blob");

    // Recolouring the fill layer: likewise one new `fill` node.
    ed.execute(&SetFill {
        layer: paint,
        fill: lumenply_doc::Fill::Solid {
            color: [0.9, 0.2, 0.1],
        },
    })
    .unwrap();
    let g2 = ed.graph_arc();
    let (added, removed, rearced) = diff(&g1, &g2);
    assert_eq!((added.len(), removed.len()), (1, 1));
    assert!(matches!(g2.node(added[0]).unwrap().op, Op::Fill { .. }));
    assert_eq!(rearced, vec![n0[&paint].0]);
    assert_eq!(ed.blobs().len(), blobs0);

    let r = ed.renderer().render_canvas(ed.graph(), ed.blobs());
    assert!(max_diff(&r, &lumenply_render::composite(ed.doc()), ed.doc().canvas()) <= 1e-6);
    ed.undo();
    ed.undo();
    assert!(Arc::ptr_eq(&ed.graph_arc(), &g0));
}

#[test]
fn undo_returns_the_identical_graph_version_and_document() {
    let mut ed = layered_editor();
    let base = id_of(&ed, "Base");
    let g0 = ed.graph_arc();
    let s0 = ed.doc_state() as *const _;
    let d0 = ed.doc().clone();
    ed.execute(&dab(base, 100.0, 220.0, [0.0, 1.0, 0.0, 1.0]))
        .unwrap();
    ed.execute(&SetBlendMode {
        layer: base,
        blend: BlendMode::Screen,
    })
    .unwrap();
    ed.execute(&SetSelection {
        selection: Some(Selection::rect(Rect::new(10, 10, 50, 40))),
    })
    .unwrap();
    let g3 = ed.graph_arc();
    let d3 = ed.doc().clone();

    ed.jump_to(ed.history().len() - 3);
    assert!(Arc::ptr_eq(&ed.graph_arc(), &g0), "the same version, not a copy");
    assert!(std::ptr::eq(ed.doc_state(), s0));
    assert_eq!(ed.graph().to_json(), g0.to_json());
    assert_same_doc(ed.doc(), &d0);
    assert!(ed.doc().selection.is_none());

    for _ in 0..3 {
        ed.redo().unwrap();
    }
    assert!(Arc::ptr_eq(&ed.graph_arc(), &g3));
    assert_same_doc(ed.doc(), &d3);
    assert!(ed.doc().selection.is_some());
    // Past states project to the same documents too.
    let n = ed.history().len();
    assert_same_doc(ed.state(n - 3).unwrap(), &d0);
    assert!(std::ptr::eq(ed.state(n).unwrap(), ed.doc()));
}

#[test]
fn history_memory_counts_the_pixels_each_step_replaced() {
    // 1024 × 512: eight tiles.
    let mut ed = Editor::new(Document::new(1024, 512));
    ed.execute(&AddPixelLayer::new("L")).unwrap();
    let id = ed.doc().layers()[0].id;
    ed.execute(&Fill {
        layer: id,
        color: [0.2, 0.4, 0.6, 1.0],
    })
    .unwrap();
    assert_eq!(ed.history_bytes(), 0, "an empty layer and an empty canvas");
    // Filling one tile's worth of selection: a new image sharing seven
    // tiles with the old one, which only the history now holds one of.
    let fill_tile = |ed: &mut Editor, x: i32, y: i32, color: [f32; 4]| {
        let sel = Selection::rect(Rect::new(x, y, 256, 256));
        ed.execute(&SetSelection { selection: Some(sel) }).unwrap();
        ed.execute(&Fill { layer: id, color }).unwrap();
        ed.execute(&SetSelection { selection: None }).unwrap();
    };
    fill_tile(&mut ed, 0, 0, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(ed.history_bytes(), TILE);
    // Strokes, moves and properties are graph ops on top of that image,
    // which stays: nothing is released.
    ed.execute(&dab(id, 100.0, 100.0, [0.0, 1.0, 0.0, 1.0])).unwrap();
    assert_eq!(ed.history_bytes(), TILE, "a stroke is a node, not pixels");
    ed.execute(&SetOpacity {
        layer: id,
        opacity: 0.5,
    })
    .unwrap();
    ed.execute(&MoveLayer {
        layer: id,
        dx: 256,
        dy: 0,
    })
    .unwrap();
    assert_eq!(ed.history_bytes(), TILE);
    assert_eq!(ed.blobs().len(), 2, "the fill and the refilled tile's image");
    // A fill on the moved, stroked layer replaces its chain with an image.
    // Of the image the chain started from, two tiles are gone: the one
    // the stroke painted over and the one refilled (moved to (2, 1)).
    fill_tile(&mut ed, 512, 256, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(ed.history_bytes(), 3 * TILE);
    assert_eq!(ed.blobs().len(), 3);
    // Undone steps no longer count; a new edit drops them and their blobs.
    for _ in 0..3 {
        ed.undo();
    }
    assert_eq!(ed.history_bytes(), TILE);
    ed.execute(&SetVisible {
        layer: id,
        visible: false,
    })
    .unwrap();
    assert_eq!(ed.history_bytes(), TILE);
    assert_eq!(ed.blobs().len(), 2, "the dropped fill's image went");
}

#[test]
fn a_coalesced_drag_is_one_graph_version() {
    let mut ed = layered_editor();
    let top = id_of(&ed, "Top");
    let steps = ed.history().len();
    let before = ed.graph_arc();
    let node = layer_nodes(&ed)[&top].0;
    for i in 1..=10 {
        ed.execute_coalescing(
            &SetOpacity {
                layer: top,
                opacity: 1.0 - i as f32 * 0.05,
            },
            "opacity",
        )
        .unwrap();
        assert_eq!(ed.history().len(), steps + 1, "tick {i}");
    }
    match &ed.graph().node(node).unwrap().op {
        Op::Layer { props } => assert!((props.opacity - 0.5).abs() < 1e-6),
        op => panic!("{op:?}"),
    }
    assert_eq!(ed.undo().as_deref(), Some("Set opacity"));
    assert!(Arc::ptr_eq(&ed.graph_arc(), &before));

    // Nudges in one run fold into a single `translate` node.
    ed.redo();
    for _ in 0..5 {
        ed.execute_coalescing(
            &MoveLayer {
                layer: top,
                dx: 1,
                dy: 0,
            },
            "nudge",
        )
        .unwrap();
    }
    let translates: Vec<&Op> = ed
        .graph()
        .nodes()
        .map(|(_, n)| &n.op)
        .filter(|op| matches!(op, Op::Translate { .. }))
        .collect();
    assert_eq!(translates, vec![&Op::Translate { dx: 5, dy: 0 }]);
    assert_eq!(ed.history().len(), steps + 2);
}

/// Run `cmd` both ways: through the editor (natively, via the graph) and
/// with `apply` on a copy of the document, then compare.
fn assert_native_matches_apply(ed: &mut Editor, cmd: &dyn Command, layer: LayerId) {
    let mut reference = ed.doc().clone();
    cmd.apply(&mut reference).unwrap();
    compact_storage(&mut reference);
    ed.execute(cmd).unwrap();
    let ours = ed.doc().layer(layer).unwrap().pixels().unwrap();
    let theirs = reference.layer(layer).unwrap().pixels().unwrap();
    assert_same_store(ours, theirs, "moved layer");
    for c in theirs.coords() {
        assert_eq!(
            ours.tile(c).unwrap().is_compact(),
            theirs.tile(c).unwrap().is_compact(),
            "storage of tile {c:?}"
        );
    }
    assert_same_doc(ed.doc(), &reference);
}

fn translate_nodes(g: &Graph) -> usize {
    g.nodes()
        .filter(|(_, n)| matches!(n.op, Op::Translate { .. }))
        .count()
}

#[test]
fn moving_a_layer_through_the_graph_matches_the_command() {
    for float in [false, true] {
        let mut ed = Editor::new(Document::new(700, 500));
        if float {
            ed.execute(&SetFloatMode { on: true }).unwrap();
        }
        ed.execute(&AddPixelLayer::from_raster("L", raster(330, 270, 9), 40, 30))
            .unwrap();
        let id = id_of(&ed, "L");
        let mut chain = 0;
        for (dx, dy) in [(37, -5), (256, 512), (-300, 7), (1, 0), (-256, -256)] {
            assert_native_matches_apply(&mut ed, &MoveLayer { layer: id, dx, dy }, id);
            chain += 1;
            assert_eq!(translate_nodes(ed.graph()), chain, "float {float}: ({dx}, {dy})");
            // The graph renders what the document composites.
            let r = ed.renderer().render_canvas(ed.graph(), ed.blobs());
            assert!(max_diff(&r, &lumenply_render::composite(ed.doc()), ed.doc().canvas()) <= 1e-6);
        }
        // Undo walks back down the chain, one move at a time.
        let after = ed.doc().clone();
        ed.undo();
        assert_eq!(translate_nodes(ed.graph()), chain - 1);
        ed.redo();
        assert_same_doc(ed.doc(), &after);
        // A stroke on the moved layer goes on top of the chain; a command
        // that isn't a graph op (a fill) turns the chain back into pixels.
        ed.execute(&dab(id, 300.0, 300.0, [1.0, 1.0, 0.0, 1.0])).unwrap();
        assert_eq!(translate_nodes(ed.graph()), chain);
        ed.execute(&Fill {
            layer: id,
            color: [0.5, 0.5, 0.5, 0.5],
        })
        .unwrap();
        assert_eq!(translate_nodes(ed.graph()), 0);
    }

    // A masked layer moves its mask too, which `translate` doesn't: that
    // move runs as a command, with the same result.
    let mut ed = Editor::new(Document::new(400, 300));
    ed.execute(&AddPixelLayer::from_raster("M", raster(200, 100, 3), 20, 20))
        .unwrap();
    let id = id_of(&ed, "M");
    ed.execute(&AddMask { layer: id }).unwrap();
    assert_native_matches_apply(
        &mut ed,
        &MoveLayer {
            layer: id,
            dx: 11,
            dy: 13,
        },
        id,
    );
    assert_eq!(translate_nodes(ed.graph()), 0);
}

fn stroke_nodes(g: &Graph) -> usize {
    g.nodes()
        .filter(|(_, n)| matches!(n.op, Op::Stroke { .. }))
        .count()
}

#[test]
fn painting_through_the_graph_matches_the_command() {
    let modes = [
        BrushMode::Paint,
        BrushMode::Erase,
        BrushMode::Dodge,
        BrushMode::Smudge,
        BrushMode::Blur,
    ];
    for float in [false, true] {
        for selected in [false, true] {
            let mut ed = Editor::new(Document::new(700, 500));
            if float {
                ed.execute(&SetFloatMode { on: true }).unwrap();
            }
            ed.execute(&AddPixelLayer::from_raster("L", raster(600, 420, 4), 30, 20))
                .unwrap();
            let id = id_of(&ed, "L");
            if selected {
                let sel = Selection::ellipse(Rect::new(100, 50, 400, 300));
                ed.execute(&SetSelection { selection: Some(sel) }).unwrap();
            }
            for (i, mode) in modes.into_iter().enumerate() {
                let k = i as f32;
                let stroke = PaintStroke {
                    layer: id,
                    brush: Brush {
                        radius: 12.0 + 4.0 * k,
                        hardness: 0.7,
                        color: [0.9, 0.2 * k, 0.1, 0.8],
                        mode,
                        ..Brush::default()
                    },
                    // Across the tile borders at x = 256 and y = 256.
                    points: vec![
                        StrokePoint::new(150.0 + 20.0 * k, 120.0, 1.0),
                        StrokePoint::new(300.0, 280.0 + 10.0 * k, 0.7),
                        StrokePoint::new(420.0, 200.0, 1.0),
                    ],
                };
                assert_native_matches_apply(&mut ed, &stroke, id);
                assert_eq!(stroke_nodes(ed.graph()), i + 1, "{mode:?}: on the chain");
                let r = ed.renderer().render_canvas(ed.graph(), ed.blobs());
                let d = max_diff(&r, &lumenply_render::composite(ed.doc()), ed.doc().canvas());
                assert!(d <= 1e-6, "{mode:?}: {d}");
            }
            // Undo takes one stroke off; redo puts back the same pixels.
            let after = ed.doc().clone();
            ed.undo();
            assert_eq!(stroke_nodes(ed.graph()), modes.len() - 1);
            ed.redo();
            assert_same_doc(ed.doc(), &after);
        }
    }
}

#[test]
fn a_version_round_trips_through_the_project_meta() {
    use lumenply_doc::{Guide, LayerLocks, PathNode, Pattern, SubPath, VectorPath};
    let mut ed = layered_editor();
    let (bottom, middle, base, top) = (
        id_of(&ed, "Bottom"),
        id_of(&ed, "Middle"),
        id_of(&ed, "Base"),
        id_of(&ed, "Top"),
    );
    // Strokes and a move: chains of ops on the layers' content.
    ed.execute(&dab(top, 480.0, 280.0, [1.0, 0.0, 1.0, 1.0])).unwrap();
    ed.execute(&MoveLayer {
        layer: top,
        dx: -30,
        dy: 12,
    })
    .unwrap();
    ed.execute(&dab(top, 470.0, 300.0, [0.0, 1.0, 1.0, 1.0])).unwrap();
    // Text, a fill, a smart object, smart filters, a disabled mask, locks.
    ed.execute(&AddTextLayer {
        text: text("Saved"),
        above: None,
    })
    .unwrap();
    let mut fill = AddFillLayer::new(lumenply_doc::Fill::Solid {
        color: [0.3, 0.1, 0.6],
    });
    fill.mask_selection = false;
    ed.execute(&fill).unwrap();
    let paint = ed.doc().layers().last().unwrap().id;
    ed.execute(&SetOpacity {
        layer: paint,
        opacity: 0.3,
    })
    .unwrap();
    ed.execute(&ConvertToSmartObject { layer: bottom }).unwrap();
    ed.execute(&AddSmartFilter::new(base, Filter::GaussianBlur { radius: 2.0 }))
        .unwrap();
    ed.execute(&SetMaskEnabled {
        layer: middle,
        enabled: false,
    })
    .unwrap();
    ed.execute(&crate::locks::SetLayerLocks {
        layer: base,
        locks: LayerLocks {
            position: true,
            ..LayerLocks::NONE
        },
    })
    .unwrap();
    let group = id_of(&ed, "Group");
    ed.execute(&SetCollapsed {
        layer: group,
        collapsed: true,
    })
    .unwrap();
    // Document state: a saved selection, a guide, a path, a pattern, the
    // resolution.
    let sel = Selection::rect(Rect::new(20, 30, 200, 100));
    ed.execute(&SetSelection { selection: Some(sel) }).unwrap();
    ed.execute(&crate::channels::SaveSelection { name: "Sky".into() })
        .unwrap();
    ed.execute(&SetSelection { selection: None }).unwrap();
    ed.execute(&AddGuide {
        guide: Guide::vertical(120.0),
    })
    .unwrap();
    ed.execute(&crate::resolution::SetResolution { ppi: 300.0 })
        .unwrap();
    ed.execute(&DefinePattern {
        pattern: Pattern::new("p-1", "Dots", raster(4, 4, 7)),
    })
    .unwrap();
    let path = VectorPath {
        subpaths: vec![SubPath {
            nodes: vec![PathNode::corner(10.0, 10.0), PathNode::corner(90.0, 40.0)],
            closed: false,
        }],
    };
    ed.execute(&SetWorkPath {
        path: Some(path.clone()),
    })
    .unwrap();

    let (graph, meta, blobs) = ed.graph_project();
    // Through JSON, as a file holds them, with only the blobs they name.
    let graph = Graph::from_json(&graph.to_json()).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta.to_string()).unwrap();
    let mut named: std::collections::HashSet<_> = lumenply_graph::blob_refs(&graph).into_keys().collect();
    named.extend(serde_json::from_value::<Vec<lumenply_graph::BlobId>>(meta["blobs"].clone()).unwrap());
    let mut kept = blobs.clone();
    kept.retain(&named);
    let mut opened = Editor::from_graph_project(&graph, &meta, kept).unwrap();

    // The same document: settings, layers and pixels; caches derived from
    // parameters within a 16-bit step (they render again at 32 bits, as
    // when any project opens).
    let (a, b) = (ed.doc(), opened.doc());
    assert_eq!((b.width, b.height, b.next_id()), (a.width, a.height, a.next_id()));
    assert_eq!((b.resolution, b.float_mode), (300.0, false));
    assert_eq!(b.guides, vec![Guide::vertical(120.0)]);
    assert_eq!(b.work_path, Some(path));
    assert_eq!(b.patterns, a.patterns);
    assert_eq!(b.saved_selections.len(), 1);
    assert_eq!(b.saved_selections[0].name, "Sky");
    assert_same_store(
        &b.saved_selections[0].mask.tiles,
        &a.saved_selections[0].mask.tiles,
        "Sky",
    );
    assert!(b.selection.is_none(), "the selection is not saved");
    compare_layers(a.layers(), b.layers(), 1.0 / 65535.0);
    assert!(!b.layer(middle).unwrap().mask.as_ref().unwrap().enabled);
    assert!(b.layer(base).unwrap().locks.position);
    // The strokes and the move are still nodes, there to edit.
    assert_eq!(stroke_nodes(opened.graph()), 2);
    assert_eq!(translate_nodes(opened.graph()), 1);
    // The graph renders the document, and editing goes on from there.
    let r = opened.renderer().render_canvas(opened.graph(), opened.blobs());
    assert!(max_diff(&r, &lumenply_render::composite(opened.doc()), b.canvas()) <= 1e-6);
    assert!(!opened.can_undo());
    opened
        .execute(&dab(top, 450.0, 320.0, [1.0, 1.0, 1.0, 1.0]))
        .unwrap();
    assert_eq!(stroke_nodes(opened.graph()), 3);
    opened.undo();
    assert_eq!(stroke_nodes(opened.graph()), 2);
}

/// A tiny deterministic generator for the random sequence.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 1001) as f32 / 1000.0
    }
}

fn all_ids(doc: &Document, pred: impl Fn(&Layer) -> bool) -> Vec<LayerId> {
    let mut out = Vec::new();
    doc.for_each_layer(|l| {
        if pred(l) {
            out.push(l.id);
        }
    });
    out
}

/// One random edit (or undo, redo, jump); returns what it did.
fn random_edit(ed: &mut Editor, rng: &mut Rng) -> String {
    let doc = ed.doc();
    let (w, h) = (doc.width as f32, doc.height as f32);
    let any = all_ids(doc, |_| true);
    let pixel = all_ids(doc, |l| matches!(l.content, LayerContent::Pixel(_)));
    let masked = all_ids(doc, |l| l.mask.is_some());
    let groups = all_ids(doc, |l| matches!(l.content, LayerContent::Group(_)));
    let texts = all_ids(doc, |l| matches!(l.content, LayerContent::Text(_)));
    let fills = all_ids(doc, |l| matches!(l.content, LayerContent::Fill(_)));
    let pick = |rng: &mut Rng, v: &[LayerId]| (!v.is_empty()).then(|| v[rng.below(v.len())]);
    let color = |rng: &mut Rng| [rng.unit(), rng.unit(), rng.unit(), 0.3 + rng.unit() * 0.7];
    let kind = rng.below(27);
    let cmd: Box<dyn Command> = match kind {
        0 | 1 => {
            let (rw, rh) = (20 + rng.below(300) as u32, 20 + rng.below(200) as u32);
            let (x, y) = (rng.below(500) as i32 - 50, rng.below(300) as i32 - 50);
            Box::new(AddPixelLayer::from_raster(
                "px",
                raster(rw, rh, rng.below(50) as u32),
                x,
                y,
            ))
        }
        2..=4 => match pick(rng, &pixel) {
            Some(layer) => {
                let points = (0..1 + rng.below(4))
                    .map(|_| StrokePoint::new(rng.unit() * w, rng.unit() * h, 1.0))
                    .collect();
                Box::new(PaintStroke {
                    layer,
                    brush: Brush {
                        radius: 3.0 + rng.unit() * 30.0,
                        color: color(rng),
                        ..Brush::default()
                    },
                    points,
                })
            }
            None => return "skip".into(),
        },
        5 => match pick(rng, &any) {
            Some(layer) => Box::new(SetOpacity {
                layer,
                opacity: rng.unit(),
            }),
            None => return "skip".into(),
        },
        6 => match pick(rng, &any) {
            Some(layer) => Box::new(SetBlendMode {
                layer,
                blend: BlendMode::ALL[rng.below(BlendMode::ALL.len())],
            }),
            None => return "skip".into(),
        },
        7 => match pick(rng, &any) {
            Some(layer) => Box::new(SetVisible {
                layer,
                visible: rng.below(3) > 0,
            }),
            None => return "skip".into(),
        },
        8 => match pick(rng, &any) {
            Some(layer) => Box::new(RemoveLayer { layer }),
            None => return "skip".into(),
        },
        9 => match pick(rng, &any) {
            Some(layer) => Box::new(ReorderLayer {
                layer,
                delta: if rng.below(2) == 0 { 1 } else { -1 },
            }),
            None => return "skip".into(),
        },
        10 => match pick(rng, &any) {
            Some(layer) => Box::new(GroupLayers {
                layers: vec![layer],
                name: "grp".into(),
            }),
            None => return "skip".into(),
        },
        11 => match pick(rng, &groups) {
            Some(layer) => Box::new(SetPassThrough {
                layer,
                pass_through: rng.below(2) == 0,
            }),
            None => return "skip".into(),
        },
        12 => match pick(rng, &any) {
            Some(layer) => Box::new(AddMask { layer }),
            None => return "skip".into(),
        },
        13 => match pick(rng, &masked) {
            Some(layer) => Box::new(PaintMask {
                layer,
                brush: Brush {
                    radius: 5.0 + rng.unit() * 40.0,
                    color: [rng.unit(), rng.unit(), rng.unit(), 1.0],
                    ..Brush::default()
                },
                points: vec![StrokePoint::new(rng.unit() * w, rng.unit() * h, 1.0)],
            }),
            None => return "skip".into(),
        },
        14 => match pick(rng, &masked) {
            Some(layer) => Box::new(SetMaskEnabled {
                layer,
                enabled: rng.below(2) == 0,
            }),
            None => return "skip".into(),
        },
        15 => match pick(rng, &any) {
            Some(layer) => Box::new(SetClipped {
                layer,
                clip: rng.below(3) > 0,
            }),
            None => return "skip".into(),
        },
        16 => match rng.below(3) {
            0 => Box::new(AddAdjustmentLayer::new(Adjustment::Invert)),
            1 => Box::new(AddFilterLayer::new(Filter::GaussianBlur { radius: 2.0 })),
            _ => Box::new(AddAdjustmentLayer::new(Adjustment::BrightnessContrast {
                brightness: 0.2,
                contrast: 0.1,
            })),
        },
        17 | 18 => match pick(rng, &pixel) {
            Some(layer) => {
                let (dx, dy) = match rng.below(3) {
                    0 => (256 * (rng.below(3) as i32 - 1), 256 * (rng.below(3) as i32 - 1)),
                    _ => (rng.below(120) as i32 - 60, rng.below(120) as i32 - 60),
                };
                if rng.below(3) == 0 {
                    // A run of nudges, coalesced.
                    for _ in 0..1 + rng.below(4) {
                        let _ = ed.execute_coalescing(&MoveLayer { layer, dx: 1, dy: 0 }, "nudge");
                    }
                    ed.end_coalescing();
                    return "nudges".into();
                }
                Box::new(MoveLayer { layer, dx, dy })
            }
            None => return "skip".into(),
        },
        19 => match pick(rng, &any) {
            Some(layer) => {
                // A slider drag, coalesced.
                for _ in 0..1 + rng.below(4) {
                    let opacity = rng.unit();
                    let _ = ed.execute_coalescing(&SetOpacity { layer, opacity }, "drag");
                }
                ed.end_coalescing();
                return "drag".into();
            }
            None => return "skip".into(),
        },
        20 => match rng.below(3) {
            0 => Box::new(SetSelection { selection: None }),
            _ => {
                let r = Rect::new(
                    rng.below(400) as i32,
                    rng.below(200) as i32,
                    10 + rng.below(200) as u32,
                    10 + rng.below(150) as u32,
                );
                Box::new(SetSelection {
                    selection: Some(Selection::rect(r)),
                })
            }
        },
        21 => match pick(rng, &any) {
            Some(layer) => match rng.below(2) {
                0 => Box::new(DuplicateLayer { layer }),
                _ => Box::new(MergeDown { layer }),
            },
            None => return "skip".into(),
        },
        22 => {
            ed.undo();
            return "undo".into();
        }
        24 => match pick(rng, &texts) {
            Some(layer) if rng.below(3) > 0 => Box::new(SetText {
                layer,
                text: lumenply_doc::TextLayer::new(
                    ["Hi", "Lumen", "ply", "graph"][rng.below(4)],
                    rng.unit() * w,
                    20.0 + rng.unit() * h,
                    12.0 + rng.unit() * 40.0,
                    color(rng),
                ),
            }),
            _ => Box::new(AddTextLayer {
                text: text("Text"),
                above: None,
            }),
        },
        25 => match pick(rng, &fills) {
            Some(layer) if rng.below(2) > 0 => Box::new(SetFill {
                layer,
                fill: lumenply_doc::Fill::Solid {
                    color: [rng.unit(), rng.unit(), rng.unit()],
                },
            }),
            _ => {
                let mut add = AddFillLayer::new(lumenply_doc::Fill::Solid {
                    color: [rng.unit(), rng.unit(), rng.unit()],
                });
                add.mask_selection = rng.below(2) == 0;
                Box::new(add)
            }
        },
        26 => match pick(rng, &pixel) {
            Some(layer) => match rng.below(2) {
                0 => Box::new(AddSmartFilter::new(layer, Filter::GaussianBlur { radius: 3.0 })),
                _ => Box::new(ConvertToSmartObject { layer }),
            },
            None => return "skip".into(),
        },
        _ => {
            match rng.below(3) {
                0 => {
                    ed.redo();
                }
                _ => {
                    let total = ed.history().len() + ed.redo_history().len();
                    ed.jump_to(rng.below(total + 1));
                }
            }
            return "redo/jump".into();
        }
    };
    let label = cmd.label();
    match ed.execute(cmd.as_ref()) {
        Ok(()) => label,
        Err(e) => format!("{label} refused: {e}"),
    }
}

/// The graph structure `sync` builds from scratch is the one `lower`
/// builds: the output's content key covers every op and connection.
fn assert_synced_like_lowered(doc: &Document, hasher: &lumenply_graph::TileHasher) {
    let r = Renderer::new();
    let (mut b1, mut b2) = (BlobStore::new(), BlobStore::new());
    let lowered = lower(doc, &mut b1, hasher).graph;
    let (synced, _) = lumenply_graph::sync(None, doc, &mut b2, hasher);
    let key = |g: &Graph| r.keys(g, g.output.unwrap())[&g.output.unwrap()];
    assert_eq!(key(&lowered), key(&synced));
}

#[test]
fn the_graph_renders_the_document_through_a_long_random_edit_sequence() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut ed = Editor::new(Document::new(600, 380));
    ed.execute(&AddPixelLayer::from_raster("bg", raster(600, 380, 0), 0, 0))
        .unwrap();
    let canvas = ed.doc().canvas();
    let mut done: BTreeMap<String, usize> = BTreeMap::new();
    for step in 0..400 {
        let what = random_edit(&mut ed, &mut rng);
        *done
            .entry(what.split(' ').next().unwrap_or("").to_string())
            .or_default() += 1;
        let doc = ed.doc();
        // The current version renders what the document composites...
        let ours = ed.renderer().render_canvas(ed.graph(), ed.blobs());
        let reference = lumenply_render::composite(doc);
        let d = max_diff(&ours, &reference, canvas);
        assert!(
            d <= 1e-6,
            "step {step} ({what}): graph and document differ by {d}"
        );
        // ...projects back to the document...
        let projected = lumenply_graph::project(ed.graph(), ed.doc_state(), ed.blobs(), ed.renderer());
        assert_same_doc(&projected, doc);
        // ...and has the structure a fresh lowering gives.
        assert_synced_like_lowered(doc, &ed.renderer().hasher);
        ed.graph().validate().unwrap();
    }
    // The sequence really exercised the editor.
    for k in ["undo", "nudges", "drag", "Move", "Paint", "Add", "Group"] {
        assert!(done.get(k).copied().unwrap_or(0) >= 5, "{k}: {done:?}");
    }
}

/// Median, 90th percentile and worst of some durations, as text in ms.
fn spread(mut v: Vec<std::time::Duration>) -> String {
    v.sort();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
    format!(
        "median {:.3}, p90 {:.3}, worst {:.3} ms",
        ms(v[v.len() / 2]),
        ms(v[v.len() * 9 / 10]),
        ms(*v.last().unwrap())
    )
}

/// Brush strokes and opacity changes on random layers, `rounds` of each:
/// the graph's share of each edit ([`Editor::last_sync_time`]) and the
/// whole edit, median and worst. Runs past the history limit, so dropping
/// old versions and their blobs is included.
pub(crate) fn sync_timings(ed: &mut Editor, name: &str, rounds: usize) {
    let pixel = all_ids(ed.doc(), |l| {
        matches!(l.content, LayerContent::Pixel(_)) && l.smart_filters.filters.is_empty()
    });
    let any = all_ids(ed.doc(), |_| true);
    let (w, h) = (ed.doc().width as f32, ed.doc().height as f32);
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut stroke, mut stroke_all, mut opacity, mut opacity_all) = (vec![], vec![], vec![], vec![]);
    for _ in 0..rounds {
        let layer = pixel[rng.below(pixel.len())];
        let (x, y) = (rng.unit() * (w - 100.0), rng.unit() * (h - 100.0));
        let cmd = PaintStroke {
            layer,
            brush: Brush {
                radius: 20.0,
                color: [rng.unit(), rng.unit(), rng.unit(), 1.0],
                ..Brush::default()
            },
            points: (0..8)
                .map(|i| StrokePoint::new(x + i as f32 * 8.0, y + i as f32 * 4.0, 1.0))
                .collect(),
        };
        let t = std::time::Instant::now();
        ed.execute(&cmd).unwrap();
        stroke_all.push(t.elapsed());
        stroke.push(ed.last_sync_time());

        let layer = any[rng.below(any.len())];
        let t = std::time::Instant::now();
        ed.execute(&SetOpacity {
            layer,
            opacity: 0.3 + rng.unit() * 0.7,
        })
        .unwrap();
        opacity_all.push(t.elapsed());
        opacity.push(ed.last_sync_time());
    }
    let show = |what: &str, sync: Vec<std::time::Duration>, all: Vec<std::time::Duration>| {
        eprintln!(
            "{name}: {what}: sync {}; whole edit {}",
            spread(sync),
            spread(all)
        );
    };
    show("brush stroke", stroke, stroke_all);
    show("opacity", opacity, opacity_all);
    // Of which dropping unused blobs and memoised hashes, once the history
    // is full (every edit then drops the oldest version).
    let gc: Vec<_> = (0..50)
        .map(|_| {
            let t = std::time::Instant::now();
            ed.collect_garbage();
            t.elapsed()
        })
        .collect();
    eprintln!("{name}: garbage collection alone: {}", spread(gc));
}

/// `layers` full-canvas pixel layers of distinct content, at rest.
fn big_document(layers: usize, w: u32, h: u32) -> Document {
    use rayon::prelude::*;
    let mut doc = Document::new(w, h);
    for i in 0..layers {
        let id = doc.alloc_id();
        let mut l = Layer::pixel(id, format!("Layer {i}"));
        let tiles: Vec<_> = doc
            .canvas()
            .tiles()
            .into_par_iter()
            .map(|c| {
                let mut t = lumenply_tiles::Tile::new();
                let (ox, oy) = c.origin();
                for (k, p) in t.pixels_mut().iter_mut().enumerate() {
                    let (x, y) = (ox + (k % 256) as i32, oy + (k / 256) as i32);
                    let v = ((x * 3 + y * 5 + i as i32 * 17) & 255) as f32 / 255.0;
                    *p = Rgba::from_straight(v, 1.0 - v, 0.5, 0.9);
                }
                t.compact();
                (c, Arc::new(t))
            })
            .collect();
        let store = l.pixels_mut().unwrap();
        for (c, t) in tiles {
            store.insert(c, t);
        }
        l.opacity = if i == 0 { 1.0 } else { 0.8 };
        doc.add_layer(l);
    }
    doc
}

#[test]
#[ignore = "timing: cargo test --release -p lumenply-core sync_overhead -- --ignored --nocapture"]
fn sync_overhead_per_edit() {
    let mut ed = crate::demo::build(1600, 1200).unwrap();
    sync_timings(&mut ed, "core demo 1600×1200", 150);

    let doc = big_document(30, 4000, 3000);
    let t = std::time::Instant::now();
    let mut ed = Editor::new(doc);
    eprintln!(
        "30 × 4000×3000: opening (lowering every layer, hashing every tile) {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    sync_timings(&mut ed, "30 × 4000×3000", 150);

    // A long session on one layer: its chain grows by a stroke and a
    // compact node per stroke.
    let layer = ed.doc().layers()[7].id;
    let mut last = Vec::new();
    for i in 0..1000u32 {
        let (x, y) = ((i * 37 % 3800) as f32 + 50.0, (i * 53 % 2800) as f32 + 50.0);
        ed.execute(&dab(layer, x, y, [0.2, 0.6, 0.9, 1.0])).unwrap();
        if i >= 900 {
            last.push(ed.last_sync_time());
        }
    }
    eprintln!(
        "30 × 4000×3000, strokes 900-1000 on one layer ({} nodes): {}",
        ed.graph().len(),
        spread(last)
    );
    let mut opacity = Vec::new();
    for i in 0..50 {
        ed.execute(&SetOpacity {
            layer,
            opacity: 0.5 + (i % 5) as f32 * 0.1,
        })
        .unwrap();
        opacity.push(ed.last_sync_time());
    }
    eprintln!("  then opacity: {}", spread(opacity));
    let time = |f: &mut dyn FnMut()| {
        let t = std::time::Instant::now();
        for _ in 0..20 {
            f();
        }
        t.elapsed().as_secs_f64() * 1e3 / 20.0
    };
    let g = ed.graph_arc();
    let keys = time(&mut || {
        ed.renderer().keys(&g, g.output.unwrap());
    });
    let state = ed.doc_state().clone();
    let doc = ed.doc().clone();
    let mut blobs = ed.blobs().clone();
    let sync = time(&mut || {
        let base = lumenply_graph::Base {
            graph: &g,
            state: &state,
            doc: &doc,
        };
        lumenply_graph::sync(Some(base), &doc, &mut blobs, &ed.renderer().hasher);
    });
    let refs = time(&mut || {
        ed.blob_refs.graph(&g);
    });
    let versions = ed.history.versions().len();
    eprintln!(
        "  keys {keys:.3} ms, sync {sync:.3} ms, blob refs of one version {refs:.3} ms ({versions} versions)"
    );
}
