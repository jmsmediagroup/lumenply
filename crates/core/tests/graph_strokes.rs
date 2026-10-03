//! A brush stroke replayed by the edit graph (ADR 0025, stage 2) paints
//! exactly what `PaintStroke` paints into the layer: every mode, with and
//! without a selection, with the round tip and with a sampled tip, scatter
//! and dynamics, through a JSON round trip of the graph.

use lumenply_core::brush_tip::{builtin_tip, BrushDynamics};
use lumenply_core::commands::{Brush, BrushMode, PaintStroke, StrokePoint};
use lumenply_core::Command;
use lumenply_doc::{Document, LayerId, Selection};
use lumenply_graph::{BlobStore, Graph, Node, Op, Renderer};
use lumenply_tiles::{Rect, Rgba, TileStore};

const MODES: [BrushMode; 9] = [
    BrushMode::Paint,
    BrushMode::Erase,
    BrushMode::Dodge,
    BrushMode::Burn,
    BrushMode::Smudge,
    BrushMode::Saturate,
    BrushMode::Desaturate,
    BrushMode::Blur,
    BrushMode::Sharpen,
];

/// 600×400 with a layer reaching 100 px past the right edge: a colour
/// gradient with a transparent hole and a half-transparent band, at rest
/// in 16-bit tiles as after any command.
fn document() -> (Document, LayerId) {
    let mut doc = Document::new(600, 400);
    let id = doc.add_pixel_layer("Paint");
    let store = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
    for y in 0..400 {
        for x in 0..700 {
            let (fx, fy) = (x as f32, y as f32);
            if (fx - 300.0).hypot(fy - 200.0) < 50.0 {
                continue;
            }
            let a = if (300..340).contains(&y) { 0.4 } else { 1.0 };
            let b = 0.5 + 0.4 * (fx * 0.05).sin();
            store.set_pixel(x, y, Rgba::from_straight(fx / 700.0, fy / 400.0, b, a));
        }
    }
    store.compact();
    (doc, id)
}

fn paths() -> [Vec<StrokePoint>; 2] {
    [
        // Across tile borders, through the hole and the band.
        vec![
            StrokePoint::new(30.0, 50.0, 0.4).with_opacity(0.9),
            StrokePoint::new(250.0, 140.0, 1.0),
            StrokePoint::new(270.0, 300.0, 0.8).with_opacity(0.6),
            StrokePoint::new(520.0, 260.0, 1.0),
            StrokePoint::new(590.0, 380.0, 0.5),
        ],
        // Off the right edge, where only the layer goes on.
        vec![
            StrokePoint::new(500.0, 30.0, 1.0),
            StrokePoint::new(560.0, 70.0, 1.0),
            StrokePoint::new(650.0, 120.0, 0.9),
        ],
    ]
}

fn brushes(mode: BrushMode) -> [Brush; 2] {
    [
        Brush {
            radius: 14.0,
            hardness: 0.6,
            color: [0.8, 0.3, 0.1, 0.7],
            spacing: 0.15,
            mode,
            ..Brush::default()
        },
        Brush {
            radius: 18.0,
            color: [0.2, 0.4, 0.9, 0.8],
            jitter: 1.2,
            mode,
            tip: builtin_tip("Chalk"),
            angle: 25.0,
            roundness: 0.7,
            dynamics: BrushDynamics {
                size_jitter: 0.5,
                angle_jitter: 0.3,
                follow_direction: true,
                roundness_jitter: 0.4,
                flip_x_jitter: true,
                count: 2,
                count_jitter: 0.5,
                opacity_jitter: 0.4,
                flow_jitter: 0.3,
                texture_depth: 0.35,
                fg_bg_jitter: 0.4,
                hue_jitter: 0.3,
                brightness_jitter: 0.2,
                background: [0.1, 0.9, 0.2],
                ..BrushDynamics::default()
            },
            ..Brush::default()
        },
    ]
}

fn selections() -> [Option<Selection>; 3] {
    let ellipse = Selection::ellipse(Rect::new(120, 60, 380, 280));
    let mut inverted = ellipse.clone();
    inverted.invert();
    [None, Some(ellipse), Some(inverted)]
}

/// The layer's pixels after the stroke, evaluated as
/// `image(layer before) → stroke` (selection through a mask node).
fn through_graph(
    doc: &Document,
    layer: LayerId,
    brush: &Brush,
    points: &[StrokePoint],
    area: Rect,
) -> TileStore {
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let mut g = Graph::new(doc.width, doc.height);
    let before = doc.layer(layer).unwrap().pixels().unwrap().clone();
    let image = g.add(Node::new(
        Op::Image {
            blob: blobs.insert(&r.hasher, before),
        },
        vec![],
    ));
    let sel = doc.selection.as_ref().map(|s| {
        let tiles = &s.coverage.tiles;
        g.add(Node::new(
            Op::Mask {
                blob: (!tiles.is_empty()).then(|| blobs.insert(&r.hasher, tiles.clone())),
                default: s.coverage.default,
            },
            vec![],
        ))
    });
    let stroke = g.add(Node::new(
        Op::stroke(brush, points.to_vec(), &mut blobs),
        vec![Some(image), sel],
    ));
    g.output = Some(stroke);
    // What is saved is what paints.
    let g = Graph::from_json(&g.to_json()).unwrap();
    r.render_node(&g, &blobs, stroke, area)
}

#[test]
fn every_brush_mode_replays_bit_identically_from_the_graph() {
    let (base, layer) = document();
    // The canvas's tiles, which hold the layer's part past the edge too.
    let area = Rect::new(0, 0, 768, 512);
    let mut cases = 0;
    let mut differing = 0;
    for mode in MODES {
        let mut changed_by_mode = 0;
        for brush in brushes(mode) {
            for sel in selections() {
                for points in paths() {
                    let mut doc = base.clone();
                    doc.selection = sel.clone();
                    let want_before = doc.layer(layer).unwrap().pixels().unwrap().clone();
                    let ours = through_graph(&doc, layer, &brush, &points, area);
                    PaintStroke {
                        layer,
                        brush: brush.clone(),
                        points,
                    }
                    .apply(&mut doc)
                    .unwrap();
                    let want = doc.layer(layer).unwrap().pixels().unwrap();
                    for y in area.y..area.bottom() {
                        for x in area.x..area.right() {
                            let (p, q) = (ours.get_pixel(x, y), want.get_pixel(x, y));
                            let bits = |p: Rgba| [p.r, p.g, p.b, p.a].map(f32::to_bits);
                            if bits(p) != bits(q) {
                                differing += 1;
                                if differing < 5 {
                                    eprintln!("{mode:?} at ({x}, {y}): graph {p:?}, PaintStroke {q:?}");
                                }
                            }
                            changed_by_mode += (q != want_before.get_pixel(x, y)) as usize;
                        }
                    }
                    cases += 1;
                }
            }
        }
        assert!(
            changed_by_mode > 5000,
            "{mode:?} changed only {changed_by_mode} pixels"
        );
    }
    assert_eq!(cases, 9 * 2 * 3 * 2);
    assert_eq!(differing, 0, "pixels where the graph and PaintStroke differ");
}

#[test]
fn a_stroke_in_history_mode_leaves_the_graph_input_as_it_is() {
    let (doc, layer) = document();
    let brush = Brush {
        mode: BrushMode::History,
        ..Brush::default()
    };
    let area = Rect::new(0, 0, 768, 512);
    let ours = through_graph(&doc, layer, &brush, &paths()[0], area);
    let before = doc.layer(layer).unwrap().pixels().unwrap();
    for c in area.tiles() {
        assert!(std::sync::Arc::ptr_eq(
            ours.tile_arc(c).unwrap(),
            before.tile_arc(c).unwrap()
        ));
    }
    // PaintStroke refuses it outright.
    let mut d = doc.clone();
    let err = PaintStroke {
        layer,
        brush,
        points: paths()[0].clone(),
    }
    .apply(&mut d)
    .unwrap_err();
    assert_eq!(err.to_string(), "the history brush needs a source state");
}
