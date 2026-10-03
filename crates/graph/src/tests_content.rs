//! Layer content as operations: every op renders exactly the derived cache
//! the layer tree holds, edits invalidate only downstream keys, and the
//! JSON keeps pixels in blobs.

use lumenply_doc::adjust::srgb_encode;
use lumenply_doc::{
    CustomShape, Document, Fill, Filter, Gradient, GradientOverlayFx, GradientStyle, Layer, LayerContent,
    Mask, Pattern, ShapeGeometry, ShapeLayer, ShapeStroke, SmartFilter, SmartLayer, StrokeAlign, TextLayer,
};
use lumenply_tiles::{Affine, Raster, Rect, Rgba, TileStore};

use crate::*;

fn push(doc: &mut Document, layer: impl FnOnce(lumenply_doc::LayerId) -> Layer) {
    let id = doc.alloc_id();
    doc.add_layer(layer(id));
}

/// Every derived cache brought up to date, as loaders and the editor do.
fn refreshed(mut doc: Document) -> Document {
    doc.for_each_layer_mut(|l| {
        if let LayerContent::Text(t) = &mut l.content {
            lumenply_render::text::refresh_cache(t);
        }
    });
    lumenply_render::fill::refresh_stale(&mut doc);
    doc
}

fn assert_identical(graph: &TileStore, tree: &TileStore, canvas: Rect) {
    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            let (p, q) = (graph.get_pixel(x, y), tree.get_pixel(x, y));
            assert!(
                [p.r, p.g, p.b, p.a] == [q.r, q.g, q.b, q.a],
                "({x}, {y}): graph {p:?}, layer tree {q:?}"
            );
        }
    }
}

/// Lower `doc`, render it through the graph, and require exactly the
/// layer tree's pixels. Returns the lowering and the graph's render.
fn lower_and_check(doc: &Document) -> (Lowered, BlobStore, Renderer, TileStore) {
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(doc, &mut blobs, &r.hasher);
    low.graph.validate().unwrap();
    let ours = r.render_canvas(&low.graph, &blobs);
    assert_identical(&ours, &lumenply_render::composite(doc), doc.canvas());
    (low, blobs, r, ours)
}

/// The node feeding a layer's content port.
fn content_of(low: &Lowered, layer: &Layer) -> NodeId {
    low.graph
        .node(low.layer_nodes[&layer.id])
        .unwrap()
        .input(1)
        .unwrap()
}

fn type_of(g: &Graph, id: NodeId) -> &'static str {
    g.node(id).unwrap().op.type_name()
}

fn gradient_fill() -> Fill {
    Fill::Gradient {
        gradient: Gradient::default(),
        style: GradientStyle::Linear,
        angle: 0.0,
        scale: 1.0,
        reverse: false,
        offset: [0.0, 0.0],
    }
}

/// 4×2 pattern whose red channel is the pixel index / 8.
fn ramp() -> Pattern {
    let mut r = Raster::new(4, 2);
    for y in 0..2 {
        for x in 0..4 {
            r.set(x, y, Rgba::new((y * 4 + x) as f32 / 8.0, 0.0, 0.0, 1.0));
        }
    }
    Pattern::new("ramp-id", "Ramp", r)
}

fn star() -> ShapeLayer {
    ShapeLayer::new(
        ShapeGeometry::Custom {
            rect: [50.0, 40.0, 120.0, 120.0],
            shape: CustomShape::Star,
        },
        Some(Fill::Solid {
            color: [0.2, 0.6, 1.0],
        }),
        Some(ShapeStroke {
            color: [1.0, 0.0, 0.0],
            width: 4.0,
            align: StrokeAlign::Outside,
            dash: None,
        }),
    )
}

fn source(area: Rect, color: [f32; 4]) -> TileStore {
    let mut s = TileStore::new();
    let p = Rgba::from_straight(color[0], color[1], color[2], color[3]);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            s.set_pixel(x, y, p);
        }
    }
    s
}

fn smart(id: lumenply_doc::LayerId, src: TileStore, t: Affine) -> Layer {
    let cache = lumenply_render::transform_store(&src, &t);
    Layer::with_content(
        id,
        "Smart",
        LayerContent::Smart(SmartLayer {
            source: src,
            transform: t,
            cache: Some(cache),
        }),
    )
}

#[test]
fn a_gradient_fill_is_a_fill_op_with_the_layer_trees_pixels() {
    let mut doc = Document::new(300, 200);
    let mut l = Layer::fill(doc.alloc_id(), gradient_fill());
    l.opacity = 0.8;
    doc.add_layer(l);
    let doc = refreshed(doc);
    let (low, _, r, _) = lower_and_check(&doc);
    let c = content_of(&low, &doc.layers()[0]);
    assert_eq!(type_of(&low.graph, c), "fill");
    // Black to white left to right, interpolated in sRGB: pixel x sits at
    // (x + 0.5) / 300 along the ramp.
    let blobs = BlobStore::new();
    let fill = r.render_node(&low.graph, &blobs, c, doc.canvas());
    for (x, want) in [(0, 0.5 / 300.0), (149, 149.5 / 300.0), (299, 299.5 / 300.0)] {
        let p = fill.get_pixel(x, 100);
        assert!((srgb_encode(p.r) - want).abs() < 1e-3, "x {x}: {p:?}");
        assert_eq!(p.a, 1.0);
    }
    // Exactly the canvas, in 16-bit tiles as the layer tree keeps them.
    assert_eq!(fill.bounds(), Some(Rect::new(0, 0, 512, 256)));
    assert_eq!(fill.get_pixel(300, 100).a, 0.0);
    assert!(fill.coords().all(|t| fill.tile(t).unwrap().is_compact()));
}

#[test]
fn a_pattern_fill_reads_the_pattern_from_a_blob() {
    let mut doc = Document::new(64, 32);
    let p = ramp();
    doc.patterns.push(p.clone());
    push(&mut doc, |id| Layer::fill(id, Fill::pattern(p.reference())));
    let doc = refreshed(doc);
    let (low, blobs, _, ours) = lower_and_check(&doc);
    let c = content_of(&low, &doc.layers()[0]);
    let Op::Fill {
        fill: Fill::Pattern { pattern, .. },
        pattern_pixels: Some(pp),
        float: false,
    } = &low.graph.node(c).unwrap().op
    else {
        panic!("a pattern fill op: {:?}", low.graph.node(c));
    };
    assert!(pattern.image.is_none(), "the op itself holds no pixels");
    assert_eq!(pp.size, [4, 2]);
    assert_eq!(
        pp.image(&blobs).as_ref(),
        Some(p.image.as_ref()),
        "the blob is the pattern, exactly"
    );
    // The pattern repeats every 4×2 px from the canvas origin (16-bit
    // tiles: within 1/65535).
    for (x, y, red) in [
        (0, 0, 0.0),
        (2, 0, 2.0 / 8.0),
        (5, 1, 5.0 / 8.0),
        (63, 31, 7.0 / 8.0),
    ] {
        let q = ours.get_pixel(x, y);
        assert!((q.r - red).abs() < 2e-5 && q.a == 1.0, "({x}, {y}): {q:?}");
    }
}

#[test]
fn text_is_a_text_op_unless_its_cache_is_photoshops() {
    let mut doc = Document::new(200, 100);
    let mut l = Layer::text(
        doc.alloc_id(),
        TextLayer::new("Hi", 20.0, 60.0, 40.0, [1.0, 0.2, 0.0, 1.0]),
    );
    // An effect laid out over the layer's whole bounds reads all of the
    // text op's output.
    l.effects.gradient_overlay = Some(GradientOverlayFx {
        opacity: 0.5,
        ..GradientOverlayFx::default()
    });
    doc.add_layer(l);
    let doc = refreshed(doc);
    let (low, blobs, r, _) = lower_and_check(&doc);
    let c = content_of(&low, &doc.layers()[0]);
    let Op::Text { text } = &low.graph.node(c).unwrap().op else {
        panic!("a text op");
    };
    assert_eq!(
        (text.text.as_str(), text.size, text.x, text.y),
        ("Hi", 40.0, 20.0, 60.0)
    );
    assert!(text.cache.is_none(), "the op holds no pixels");
    let glyphs = r.render_node(&low.graph, &blobs, c, doc.canvas());
    // DejaVu Sans "Hi" at 40 px on the baseline y = 60: the H's left stem
    // is solid colour, the space between the glyphs empty.
    let stem = glyphs.get_pixel(27, 45);
    assert_eq!([stem.r, stem.g, stem.b, stem.a], [1.0, 0.2, 0.0, 1.0]);
    assert_eq!(glyphs.get_pixel(70, 20).a, 0.0);
    assert_eq!(
        glyphs.content_bounds(),
        lumenply_render::text::rasterize(text).content_bounds()
    );
    let b = glyphs.content_bounds().unwrap();
    assert!(b.x >= 22 && b.x <= 26 && b.bottom() == 60 && b.y <= 31, "{b:?}");

    // The editor keeps caches at 16 bits: still ours.
    let mut compacted = doc.clone();
    if let LayerContent::Text(t) = &mut compacted.layers_mut()[0].content {
        t.cache.as_mut().unwrap().compact();
    }
    let mut b2 = BlobStore::new();
    let low2 = lower(&compacted, &mut b2, &r.hasher);
    assert_eq!(
        type_of(&low2.graph, content_of(&low2, &compacted.layers()[0])),
        "text"
    );

    // A PSD's text shows Photoshop's own pixels until it is edited: those
    // stay an image (exactly what the layer tree shows).
    let mut psd = doc.clone();
    if let LayerContent::Text(t) = &mut psd.layers_mut()[0].content {
        t.cache = Some(source(Rect::new(20, 30, 50, 30), [0.0, 0.0, 1.0, 1.0]));
    }
    let (low3, blobs3, r3, _) = lower_and_check(&psd);
    let c3 = content_of(&low3, &psd.layers()[0]);
    assert_eq!(type_of(&low3.graph, c3), "image");
    let shown = r3.render_node(&low3.graph, &blobs3, c3, psd.canvas());
    let p = shown.get_pixel(30, 40);
    assert_eq!(
        [p.r, p.g, p.b, p.a],
        [0.0, 0.0, 1.0, 1.0],
        "Photoshop's blue, not our glyphs"
    );
}

#[test]
fn a_star_with_a_stroke_is_a_shape_op() {
    let mut doc = Document::new(220, 200);
    let bg = source(doc.canvas(), [1.0, 1.0, 1.0, 1.0]);
    push(&mut doc, |id| {
        Layer::with_content(id, "Paper", LayerContent::Pixel(bg))
    });
    let mut l = Layer::shape(doc.alloc_id(), star());
    l.blend = lumenply_doc::BlendMode::Multiply;
    doc.add_layer(l);
    let doc = refreshed(doc);
    let (low, blobs, r, _) = lower_and_check(&doc);
    let c = content_of(&low, &doc.layers()[1]);
    assert_eq!(type_of(&low.graph, c), "shape");
    let shape = r.render_node(&low.graph, &blobs, c, doc.canvas());
    let px = |x, y| {
        let p: Rgba = shape.get_pixel(x, y);
        [p.r, p.g, p.b, p.a]
    };
    // The fill at the centre, the stroke 2 px outside the top point
    // (Outside strokes start at the outline), nothing past the stroke.
    let fill = px(110, 100);
    for (v, want) in fill.iter().zip([0.2, 0.6, 1.0, 1.0]) {
        assert!((v - want).abs() < 1e-4, "{fill:?}");
    }
    assert_eq!(px(110, 38), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(px(110, 32)[3], 0.0);
    assert_eq!(px(60, 50)[3], 0.0, "between the star's arms");
}

#[test]
fn a_rotated_smart_object_is_its_source_through_a_transform_op() {
    let mut doc = Document::new(160, 120);
    let src = source(Rect::new(40, 30, 80, 50), [0.9, 0.5, 0.1, 1.0]);
    let t = Affine::around(80.0, 55.0, 1.0, 1.0, 30f32.to_radians());
    let l = smart(doc.alloc_id(), src.clone(), t);
    doc.add_layer(l);
    let doc = refreshed(doc);
    let (low, blobs, r, ours) = lower_and_check(&doc);
    let c = content_of(&low, &doc.layers()[0]);
    let Op::Transform { matrix } = &low.graph.node(c).unwrap().op else {
        panic!("a transform op");
    };
    assert_eq!(*matrix, t.coeffs());
    let s = low.graph.node(c).unwrap().input(0).unwrap();
    let Op::Image { blob } = &low.graph.node(s).unwrap().op else {
        panic!("the untouched source as an image");
    };
    assert_eq!(blobs.get(blob).unwrap().len(), src.len());
    // The centre keeps the source colour exactly; the source's top-left
    // corner turned out of the rotated rectangle.
    let p = ours.get_pixel(80, 55);
    assert_eq!(p, Rgba::from_straight(0.9, 0.5, 0.1, 1.0));
    assert_eq!(ours.get_pixel(41, 31).a, 0.0);
    // One top corner turns up past the source's top edge (y = 30): the
    // pixel 2.5 px inside it lands about 12 px higher, still opaque.
    let placed = r.render_node(&low.graph, &blobs, c, doc.canvas());
    let (x, y) = [t.apply(42.5, 32.5), t.apply(117.5, 32.5)]
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!(y < 21.0, "{y}");
    let p = placed.get_pixel(x.floor() as i32, y.floor() as i32);
    for (v, want) in [(p.r, 0.9), (p.g, 0.5), (p.b, 0.1), (p.a, 1.0)] {
        assert!((v - want).abs() < 1e-6, "{p:?}");
    }
}

/// A scaled, rotated smart object with a Gaussian blur and 50% Multiply
/// noise, the right half of the stack masked off.
fn filtered_smart() -> Document {
    let mut doc = Document::new(240, 180);
    let src = source(Rect::new(60, 50, 100, 80), [0.3, 0.7, 0.4, 1.0]);
    let t = Affine::around(110.0, 90.0, 1.25, 1.25, 0.2);
    let mut l = smart(doc.alloc_id(), src, t);
    l.smart_filters
        .filters
        .push(SmartFilter::new(Filter::GaussianBlur { radius: 4.0 }));
    l.smart_filters.filters.push(SmartFilter {
        opacity: 0.5,
        blend: lumenply_doc::BlendMode::Multiply,
        ..SmartFilter::new(Filter::Noise { amount: 0.3 })
    });
    // A disabled filter changes nothing and isn't a node.
    l.smart_filters.filters.push(SmartFilter {
        enabled: false,
        ..SmartFilter::new(Filter::FindEdges)
    });
    let mut m = Mask::reveal_all();
    for y in 0..180 {
        for x in 120..240 {
            m.set_value(x, y, 0.0);
        }
    }
    l.smart_filters.mask = Some(m);
    doc.add_layer(l);
    refreshed(doc)
}

#[test]
fn a_smart_object_with_two_smart_filters_and_a_filter_mask() {
    let doc = filtered_smart();
    let (low, blobs, r, ours) = lower_and_check(&doc);
    // content → blur → noise (with the mask) → layer.
    let top = content_of(&low, &doc.layers()[0]);
    let n = low.graph.node(top).unwrap();
    let Op::SmartFilter {
        filter: Filter::Noise { amount },
        opacity,
        blend,
        float: false,
    } = &n.op
    else {
        panic!("the noise filter on top: {n:?}");
    };
    assert_eq!(
        (*amount, *opacity, *blend),
        (0.3, 0.5, lumenply_doc::BlendMode::Multiply)
    );
    assert_eq!(type_of(&low.graph, n.input(1).unwrap()), "mask");
    let blur = n.input(0).unwrap();
    let b = low.graph.node(blur).unwrap();
    assert!(matches!(b.op, Op::SmartFilter { filter: Filter::GaussianBlur { radius }, .. } if radius == 4.0));
    assert_eq!(b.input(1), None, "the mask covers the whole stack, once");
    assert_eq!(type_of(&low.graph, b.input(0).unwrap()), "transform");
    assert_eq!(b.name.as_deref(), Some("Gaussian Blur"));

    // Left of x = 120 the edge is blurred: 3 px outside the transformed
    // rectangle's left edge there is some coverage. Right of it the mask
    // shows the unfiltered object, whose edge is crisp.
    let unfiltered = r.render_node(&low.graph, &blobs, b.input(0).unwrap(), doc.canvas());
    let edge_left = (0..240).find(|&x| unfiltered.get_pixel(x, 90).a > 0.5).unwrap();
    let a = ours.get_pixel(edge_left - 3, 90).a;
    assert!(a > 0.01 && a < 0.5, "blurred edge: {a}");
    assert_eq!(unfiltered.get_pixel(edge_left - 3, 90).a, 0.0);
    for x in [150, 175, 199] {
        let (p, q) = (ours.get_pixel(x, 90), unfiltered.get_pixel(x, 90));
        for (u, v) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
            assert!((u - v).abs() < 1e-5, "masked off at {x}: {p:?} vs {q:?}");
        }
    }
    // The whole stack ran in one fused pass: the transform and the top
    // filter were computed, the blur node alone never was.
    let (results, _, computed) = r.wholes.stats();
    assert_eq!((results, computed), (2, 2));
}

#[test]
fn editing_text_or_a_fill_colour_changes_only_downstream_keys() {
    let mut doc = Document::new(200, 120);
    push(&mut doc, |id| {
        Layer::fill(
            id,
            Fill::Solid {
                color: [0.1, 0.2, 0.3],
            },
        )
    });
    push(&mut doc, |id| Layer::shape(id, star()));
    push(&mut doc, |id| {
        Layer::text(id, TextLayer::new("Key", 10.0, 80.0, 50.0, [0.0, 0.0, 0.0, 1.0]))
    });
    let doc = refreshed(doc);
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let low = lower(&doc, &mut blobs, &r.hasher);
    let out = low.graph.output.unwrap();
    let before = r.keys(&low.graph, out);
    let changed_only_downstream = |g: &Graph, edited: NodeId| {
        let after = r.keys(g, out);
        let downstream = g.downstream(edited);
        assert!(
            downstream.contains(&out),
            "the content, its layer and those above"
        );
        for (id, k) in &after {
            assert_eq!(before[id] != *k, downstream.contains(id), "{id}");
        }
    };

    let text = content_of(&low, &doc.layers()[2]);
    let mut g = low.graph.clone();
    g.update(text, |n| {
        if let Op::Text { text } = &mut n.op {
            text.text = "Lock".into();
        }
    })
    .unwrap();
    changed_only_downstream(&g, text);

    let fill = content_of(&low, &doc.layers()[0]);
    let mut g = low.graph.clone();
    g.update(fill, |n| {
        if let Op::Fill { fill, .. } = &mut n.op {
            *fill = Fill::Solid {
                color: [0.9, 0.2, 0.3],
            };
        }
    })
    .unwrap();
    changed_only_downstream(&g, fill);
    // The edit renders as the edited layer tree does, recomputing one
    // whole result: the fill's.
    r.render_canvas(&low.graph, &blobs);
    let computed = r.wholes.stats().2;
    let edited = r.render_canvas(&g, &blobs);
    assert_eq!(r.wholes.stats().2, computed + 1);
    let mut d = doc.clone();
    d.layers_mut()[0].fill_layer_mut().unwrap().fill = Fill::Solid {
        color: [0.9, 0.2, 0.3],
    };
    d.layers_mut()[0].fill_layer_mut().unwrap().cache = None;
    let d = refreshed(d);
    assert_identical(&edited, &lumenply_render::composite(&d), d.canvas());
}

/// One document with every content op.
pub(crate) fn every_content_op() -> Document {
    let mut doc = filtered_smart();
    let p = ramp();
    doc.patterns.push(p.clone());
    push(&mut doc, |id| Layer::fill(id, Fill::pattern(p.reference())));
    doc.layers_mut().last_mut().unwrap().opacity = 0.3;
    let mut sh = star();
    sh.fill = Some(Fill::pattern(p.reference()));
    push(&mut doc, |id| Layer::shape(id, sh));
    push(&mut doc, |id| Layer::fill(id, gradient_fill()));
    doc.layers_mut().last_mut().unwrap().blend = lumenply_doc::BlendMode::Overlay;
    push(&mut doc, |id| {
        Layer::text(
            id,
            TextLayer::new("JSON", 30.0, 150.0, 36.0, [0.1, 0.1, 0.8, 1.0]),
        )
    });
    refreshed(doc)
}

#[test]
fn every_content_op_round_trips_through_json_with_pattern_pixels_as_a_blob() {
    let doc = every_content_op();
    let (low, blobs, r, ours) = lower_and_check(&doc);
    let json = low.graph.to_json();
    let back = Graph::from_json(&json).unwrap();
    assert_eq!(back, low.graph);
    assert_identical(&Renderer::new().render_canvas(&back, &blobs), &ours, doc.canvas());

    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let nodes: Vec<&serde_json::Value> = v["nodes"].as_object().unwrap().values().collect();
    let of = |t: &str| {
        nodes
            .iter()
            .filter(|n| n["type"] == t)
            .copied()
            .collect::<Vec<_>>()
    };
    let mut types: Vec<&str> = nodes.iter().map(|n| n["type"].as_str().unwrap()).collect();
    types.sort();
    types.dedup();
    for t in ["text", "fill", "shape", "transform", "smart-filter"] {
        assert!(types.contains(&t), "{t} in {types:?}");
    }
    let text = of("text")[0];
    assert_eq!(text["text"], "JSON");
    assert_eq!(text["size"], 36.0);
    assert!(text.get("cache").is_none());
    let transform = of("transform")[0];
    assert_eq!(transform["matrix"].as_array().unwrap().len(), 6);
    assert_eq!(transform["inputs"].as_array().unwrap().len(), 1);
    let filters = of("smart-filter");
    assert_eq!(filters.len(), 2);
    assert!(filters
        .iter()
        .any(|f| f["filter"]["type"] == "gaussian-blur" && f["filter"]["radius"] == 4.0));
    assert!(filters.iter().any(|f| f["blend"] == "multiply"
        && f["opacity"] == 0.5
        && f["inputs"].as_array().unwrap().len() == 2));
    let shape = of("shape")[0];
    assert_eq!(shape["geometry"]["type"], "custom");
    assert_eq!(shape["stroke"]["align"], "outside");

    // Pattern pixels: one blob, named by hash from the fill and the shape;
    // the fill's own reference keeps only the pattern's id and name.
    let fills = of("fill");
    let pattern_fill = fills.iter().find(|f| f["fill"]["type"] == "pattern").unwrap();
    assert_eq!(
        pattern_fill["fill"]["pattern"],
        serde_json::json!({"id": "ramp-id", "name": "Ramp"})
    );
    let pp = &pattern_fill["pattern_pixels"];
    assert_eq!(pp["size"], serde_json::json!([4, 2]));
    let hash = Hash::from_hex(pp["blob"].as_str().unwrap()).unwrap();
    assert_eq!(shape["pattern_pixels"]["blob"], pp["blob"]);
    let stored = blobs.get(&hash).unwrap();
    assert_eq!(
        stored.to_raster(Rect::new(0, 0, 4, 2)),
        *ramp().image,
        "the blob holds the pattern exactly"
    );
    let gradient = fills.iter().find(|f| f["fill"]["type"] == "gradient").unwrap();
    assert!(gradient.get("pattern_pixels").is_none());
    // The renderer made each whole result once: the transform, the fused
    // filter run, two fills, the shape and the text.
    assert_eq!(r.wholes.stats().2, 6);
}

#[test]
fn one_renderer_keeps_the_same_fill_on_two_canvases_apart() {
    let solid = Fill::Solid {
        color: [0.4, 0.5, 0.6],
    };
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let mut seen = Vec::new();
    for (w, h) in [(300, 100), (100, 300)] {
        let mut doc = Document::new(w, h);
        push(&mut doc, |id| Layer::fill(id, solid.clone()));
        let doc = refreshed(doc);
        let low = lower(&doc, &mut blobs, &r.hasher);
        let ours = r.render_canvas(&low.graph, &blobs);
        assert_identical(&ours, &lumenply_render::composite(&doc), doc.canvas());
        // A fill covers exactly its own canvas.
        assert_eq!(ours.get_pixel(w as i32 - 1, h as i32 - 1).a, 1.0);
        assert_eq!(ours.get_pixel(w as i32, 0).a, 0.0);
        let c = content_of(&low, &doc.layers()[0]);
        seen.push((low.graph.node(c).unwrap().op.clone(), r.keys(&low.graph, c)[&c]));
    }
    assert_eq!(seen[0].0, seen[1].0, "the same op");
    assert_ne!(seen[0].1, seen[1].1, "on another canvas, another key");
}

#[test]
fn hidden_layers_content_is_never_computed() {
    let mut doc = Document::new(100, 100);
    let mut l = Layer::text(
        doc.alloc_id(),
        TextLayer::new("Hidden", 0.0, 50.0, 30.0, [0.0, 0.0, 0.0, 1.0]),
    );
    l.visible = false;
    doc.add_layer(l);
    push(&mut doc, |id| {
        Layer::fill(
            id,
            Fill::Solid {
                color: [0.5, 0.5, 0.5],
            },
        )
    });
    let doc = refreshed(doc);
    let (low, ..) = lower_and_check(&doc);
    assert_eq!(type_of(&low.graph, content_of(&low, &doc.layers()[0])), "text");
    let r = Renderer::new();
    let blobs = BlobStore::new();
    r.render_canvas(&low.graph, &blobs);
    assert_eq!(r.wholes.stats().2, 1, "the fill only");
}
