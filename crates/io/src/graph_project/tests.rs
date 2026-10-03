use super::*;
use lumenply_doc::{Adjustment, BlendMode, Filter, Layer, LayerContent, Mask, ShadowFx};
use lumenply_graph::{Op, Renderer};
use lumenply_tiles::{Rect, Rgba, TileStore};

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("lumenply-graph-project-test");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

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

/// The busy document of `lumenply-graph`'s tests (blend modes, opacity,
/// fill, masks, effects, adjustment and filter layers, isolated and
/// pass-through groups, a clip chain), with the background and the "Red"
/// layer at rest in 16-bit tiles so both tile formats are saved, plus a
/// saved selection, a guide and a resolution for the meta.
fn busy_document() -> Document {
    let mut doc = Document::new(300, 280);
    let canvas = doc.canvas();
    let mut bg = painted(&mut doc, "Background", canvas, [0.9, 0.85, 0.7, 1.0]);
    if let LayerContent::Pixel(s) = &mut bg.content {
        s.compact();
    }
    doc.add_layer(bg);

    let mut red = painted(&mut doc, "Red", Rect::new(20, 30, 200, 120), [0.8, 0.1, 0.1, 0.9]);
    red.blend = BlendMode::Multiply;
    red.opacity = 0.6;
    red.mask = Some(half_mask(Rect::new(20, 30, 200, 120)));
    if let LayerContent::Pixel(s) = &mut red.content {
        s.compact();
    }
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

    doc.saved_selections.push(lumenply_doc::SavedSelection {
        name: "Sky".into(),
        mask: half_mask(Rect::new(0, 0, 300, 90)),
    });
    doc.guides = vec![lumenply_doc::Guide::vertical(150.0)];
    doc.resolution = 300.0;
    doc
}

/// Same tiles, bit for bit and in the same storage format.
fn assert_same_bits(a: &TileStore, b: &TileStore, canvas: Rect) {
    for c in canvas.tiles() {
        match (a.tile(c), b.tile(c)) {
            (None, None) => {}
            (Some(p), Some(q)) => assert!(p.raw_bytes() == q.raw_bytes(), "tile {c:?} differs"),
            _ => panic!("tile {c:?} is present in only one render"),
        }
    }
}

/// Rewrite the archive at `path`, passing every entry through `f`
/// (decompressed; `None` drops the entry).
fn rewrite(path: &Path, f: impl Fn(&str, Vec<u8>) -> Option<Vec<u8>>) {
    let mut entries = Vec::new();
    {
        let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
        for i in 0..zip.len() {
            let mut e = zip.by_index(i).unwrap();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            entries.push((e.name().to_string(), buf));
        }
    }
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let opts = FileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in entries {
        if let Some(data) = f(&name, data) {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&data).unwrap();
        }
    }
    zip.finish().unwrap();
}

/// The blob holding a pixel layer's content.
fn blob_of(graph: &Graph, layer: &str) -> BlobId {
    let (_, layer_node) = graph
        .nodes()
        .find(|(_, n)| n.name.as_deref() == Some(layer))
        .unwrap();
    let content = graph.node(layer_node.input(1).unwrap()).unwrap();
    match &content.op {
        Op::Image { blob } => *blob,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_graph_project_round_trips_exactly() {
    let doc = busy_document();
    let mut p = document_to_graph(&doc, 1);
    // A blob nothing refers to stays out of the file.
    let mut orphan = TileStore::new();
    orphan.set_pixel(0, 0, Rgba::WHITE);
    p.blobs.insert(&TileHasher::default(), orphan);
    assert_eq!(p.blobs.len(), 12);

    let path = temp("round-trip.lumen");
    let stats = save_graph_project(&path, &p.graph, &p.blobs, &p.meta, None).unwrap();
    assert_eq!(
        (
            stats.blobs,
            stats.blob_tiles,
            stats.hint_tiles,
            stats.dropped_blobs
        ),
        (11, 21, 0, 1),
        "ten layer and mask blobs (20 tiles) plus the saved selection (1); the orphan dropped"
    );
    assert_eq!(project_version(&path).unwrap(), 3);
    assert!(is_graph_project(&path));

    let back = load_graph_project(&path).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.source_version, 3);
    assert_eq!(
        back.graph.to_json(),
        p.graph.to_json(),
        "graph.json is the graph exactly"
    );
    assert_eq!(back.graph, p.graph);
    assert_eq!(back.meta, p.meta);
    assert_eq!(back.meta["resolution"], 300.0);
    assert_eq!(back.meta["guides"][0]["pos"], 150.0);

    // The same blobs, under the same hashes, with the same content.
    let ids = |b: &BlobStore| b.ids().copied().collect::<BTreeSet<_>>();
    let refs = referenced_blobs(&p.graph, &p.meta);
    let mut expected = ids(&p.blobs);
    expected.retain(|id| refs.contains(id));
    assert_eq!(ids(&back.blobs), expected);
    assert_eq!(back.blobs.len(), 11);
    let hasher = TileHasher::default();
    for id in back.blobs.ids() {
        assert_eq!(hasher.store(back.blobs.get(id).unwrap()), *id);
    }
    // Tiles kept their storage format: the 16-bit background, f32 elsewhere.
    let bg = back.blobs.get(&blob_of(&back.graph, "Background")).unwrap();
    assert_eq!(bg.len(), 4);
    assert!(bg.coords().all(|c| bg.tile(c).unwrap().is_compact()));
    let shadowed = back.blobs.get(&blob_of(&back.graph, "Shadowed")).unwrap();
    assert_eq!(shadowed.len(), 1);
    assert!(shadowed.coords().all(|c| !shadowed.tile(c).unwrap().is_compact()));
    // The saved selection's pixels came along through the meta.
    let sky: Hash = serde_json::from_value(back.meta["channels"][0]["blob"].clone()).unwrap();
    assert_eq!(back.blobs.get(&sky).unwrap().len(), 1);
    assert_eq!(back.blobs.get(&sky).unwrap().get_pixel(149, 89).a, 1.0);

    // Bit-identical rendering, and the same as the layer tree.
    let ours = Renderer::new().render_canvas(&back.graph, &back.blobs);
    let before = Renderer::new().render_canvas(&p.graph, &p.blobs);
    assert_same_bits(&ours, &before, doc.canvas());
    let tree = lumenply_render::composite(&doc);
    for (x, y) in [(10, 10), (60, 60), (195, 175), (230, 40), (299, 279)] {
        let (a, b) = (ours.get_pixel(x, y), tree.get_pixel(x, y));
        assert!((a.r - b.r).abs() < 1e-6 && (a.a - b.a).abs() < 1e-6, "({x}, {y})");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn render_hints_serve_the_first_render_until_the_graph_changes() {
    let doc = busy_document();
    let p = document_to_graph(&doc, 1);
    let r = Renderer::new();
    let out = p.graph.output.unwrap();
    let hints = RenderHints::capture(&r, &p.graph, &p.blobs, &[out]);
    assert_eq!(hints.len(), 4, "2×2 canvas tiles");
    let path = temp("hints.lumen");
    let stats = save_graph_project(&path, &p.graph, &p.blobs, &p.meta, Some(&hints)).unwrap();
    assert_eq!(stats.hint_tiles, 4);

    let back = load_graph_project(&path).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.hints.len(), 4);
    let fresh = Renderer::new();
    assert_eq!(back.hints.seed(&fresh.cache), 4);
    let seeded = fresh.render_canvas(&back.graph, &back.blobs);
    let s = fresh.cache.stats();
    assert_eq!(
        (s.hits, s.misses),
        (4, 0),
        "every output tile from the hints, nothing computed"
    );
    let reference = Renderer::new().render_canvas(&p.graph, &p.blobs);
    assert_same_bits(&seeded, &reference, doc.canvas());

    // A changed graph has another output key: the hints go unused and the
    // render is the changed one, computed in full.
    let mut changed = back.graph.clone();
    changed
        .update(out, |n| {
            if let Op::FilterLayer { opacity, .. } = &mut n.op {
                *opacity = 0.2;
            }
        })
        .unwrap();
    let hinted = Renderer::new();
    assert_eq!(back.hints.seed(&hinted.cache), 4);
    let a = hinted.render_canvas(&changed, &back.blobs);
    let plain = Renderer::new();
    let b = plain.render_canvas(&changed, &back.blobs);
    assert_same_bits(&a, &b, doc.canvas());
    // Every tile the changed graph needs is computed, hints or not.
    let (sa, sb) = (hinted.cache.stats(), plain.cache.stats());
    assert_eq!(sa.misses, sb.misses, "the hints spared no work");
    assert!(sa.misses > 4);
    assert!(a.get_pixel(60, 60) != seeded.get_pixel(60, 60), "the edit shows");

    // Hints for content no longer in the graph are not saved.
    let path2 = temp("hints-stale.lumen");
    let stats = save_graph_project(&path2, &changed, &back.blobs, &back.meta, Some(&hints)).unwrap();
    assert_eq!(stats.hint_tiles, 0);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

#[test]
fn a_damaged_blob_or_hint_is_dropped_with_a_warning_and_the_rest_loads() {
    let doc = busy_document();
    let p = document_to_graph(&doc, 1);
    let r = Renderer::new();
    let hints = RenderHints::capture(&r, &p.graph, &p.blobs, &[p.graph.output.unwrap()]);
    let path = temp("damaged.lumen");
    save_graph_project(&path, &p.graph, &p.blobs, &p.meta, Some(&hints)).unwrap();
    let inner = blob_of(&p.graph, "Inner");
    // One byte of the "Inner" layer's only tile changes (the zip entry
    // itself stays valid), one hint tile is cut short, and meta.json stops
    // being JSON.
    rewrite(&path, |name, mut data| {
        if name == format!("blobs/{inner}/0_0") {
            data[(40 * 256 + 230) * 16 + 1] ^= 0x40;
        } else if name.starts_with("cache/") && name.ends_with("/1_1") {
            data.truncate(1000);
        } else if name == "meta.json" {
            data = b"{ not json".to_vec();
        }
        Some(data)
    });
    let back = load_graph_project(&path).unwrap();
    assert_eq!(back.warnings.len(), 3, "{:#?}", back.warnings);
    assert!(back.warnings.iter().any(|w| w.contains("meta.json")));
    let blob_warning = back
        .warnings
        .iter()
        .find(|w| w.contains(&inner.to_hex()))
        .unwrap();
    assert!(blob_warning.contains("(in Inner)"), "{blob_warning}");
    assert!(back.warnings.iter().any(|w| w.starts_with("render hints")));
    assert_eq!(back.meta, serde_json::json!({}));
    assert_eq!(back.blobs.len(), p.blobs.len() - 1);
    assert!(!back.blobs.contains(&inner));
    assert!(back.hints.is_empty(), "the damaged key's hints all went");

    // Everything but the lost layer renders as before.
    let ours = Renderer::new().render_canvas(&back.graph, &back.blobs);
    let before = Renderer::new().render_canvas(&p.graph, &p.blobs);
    assert_eq!(ours.get_pixel(20, 270), before.get_pixel(20, 270));
    assert_eq!(ours.get_pixel(60, 60), before.get_pixel(60, 60));
    assert!(
        ours.get_pixel(230, 40) != before.get_pixel(230, 40),
        "Inner is gone"
    );

    // A missing tile entry loses its blob the same way.
    save_graph_project(&path, &p.graph, &p.blobs, &p.meta, None).unwrap();
    rewrite(&path, |name, data| {
        (name != format!("blobs/{inner}/0_0")).then_some(data)
    });
    let back = load_graph_project(&path).unwrap();
    assert_eq!(back.warnings.len(), 1, "{:#?}", back.warnings);
    assert!(back.warnings[0].contains("missing entry"), "{}", back.warnings[0]);
    assert_eq!(back.blobs.len(), p.blobs.len() - 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn layer_tree_projects_load_as_graphs_and_versions_are_told_apart() {
    let doc = busy_document();
    let v1 = temp("tree.lumen");
    project::save(&v1, &doc).unwrap();
    assert_eq!(project_version(&v1).unwrap(), 1);
    assert!(!is_graph_project(&v1));
    assert!(matches!(
        load_graph_project(&v1),
        Err(ProjectError::NotAProject(_))
    ));

    let g = load_any_as_graph(&v1).unwrap();
    assert_eq!(g.source_version, 1);
    assert!(g.warnings.is_empty());
    assert_eq!((g.graph.width, g.graph.height), (300, 280));
    assert_eq!(g.meta["resolution"], 300.0);
    assert_eq!(g.meta["channels"][0]["name"], "Sky");
    let ours = Renderer::new().render_canvas(&g.graph, &g.blobs);
    let tree = lumenply_render::composite(&project::load(&v1).unwrap());
    let mut worst = 0f32;
    for y in 0..280 {
        for x in 0..300 {
            let (a, b) = (ours.get_pixel(x, y), tree.get_pixel(x, y));
            for (u, v) in [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)] {
                worst = worst.max((u - v).abs());
            }
        }
    }
    assert!(worst <= 1e-6, "{worst}");

    // Saved as format 3 it loads through either function; the layer-tree
    // loader refuses it rather than misreading it.
    let v3 = temp("tree-as-graph.lumen");
    save_graph_project(&v3, &g.graph, &g.blobs, &g.meta, None).unwrap();
    assert_eq!(load_any_as_graph(&v3).unwrap().graph, g.graph);
    assert!(project::load(&v3).is_err());

    // A newer format is refused by name.
    rewrite(&v3, |name, data| {
        Some(if name == "manifest.json" {
            String::from_utf8(data)
                .unwrap()
                .replace("\"version\": 3", "\"version\": 4")
                .into_bytes()
        } else {
            data
        })
    });
    assert!(matches!(
        load_graph_project(&v3),
        Err(ProjectError::GraphTooNew(4))
    ));
    let _ = std::fs::remove_file(&v1);
    let _ = std::fs::remove_file(&v3);
}

#[test]
fn saves_are_atomic_and_refuse_broken_graphs() {
    let doc = busy_document();
    let p = document_to_graph(&doc, 1);
    let path = temp("atomic.lumen");
    save_graph_project(&path, &p.graph, &p.blobs, &p.meta, None).unwrap();
    let before = std::fs::read(&path).unwrap();

    // A cycle can't be saved (it couldn't be loaded back).
    let mut broken = p.graph.clone();
    let out = broken.output.unwrap();
    let first = broken.nodes().next().unwrap().0;
    broken
        .update(first, |n| {
            n.op = Op::Layer {
                props: Default::default(),
            };
            n.inputs = vec![Some(out)];
        })
        .unwrap();
    assert!(matches!(
        save_graph_project(&path, &broken, &p.blobs, &p.meta, None),
        Err(ProjectError::Graph(_))
    ));
    // Nor into a folder that doesn't exist; the old file is untouched and
    // no temporary file is left behind.
    let gone = temp("no-such-dir").join("x.lumen");
    assert!(save_graph_project(&gone, &p.graph, &p.blobs, &p.meta, None).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    assert!(!std::path::PathBuf::from(tmp).exists());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn pattern_overlays_keep_their_pattern_pixels() {
    // A 3×2 pattern: red, green, blue over half-transparent grey.
    let mut image = lumenply_tiles::Raster::new(3, 2);
    for (i, c) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        .iter()
        .enumerate()
    {
        image.set(i as u32, 0, Rgba::from_straight(c[0], c[1], c[2], 1.0));
        image.set(i as u32, 1, Rgba::from_straight(0.5, 0.5, 0.5, 0.5));
    }
    let pattern = lumenply_doc::Pattern::new("pat-1", "Stripes", image.clone());
    let mut doc = Document::new(64, 48);
    let mut l = painted(
        &mut doc,
        "Patterned",
        Rect::new(8, 8, 40, 30),
        [0.2, 0.2, 0.2, 1.0],
    );
    l.effects.pattern_overlay = Some(lumenply_doc::PatternOverlayFx {
        pattern: lumenply_doc::PatternRef {
            id: "pat-1".into(),
            name: "Stripes".into(),
            image: None,
        },
        scale: 1.0,
        opacity: 0.8,
        blend: BlendMode::Normal,
        offset: [0.0, 0.0],
        angle: 0.0,
        link: true,
    });
    doc.add_layer(l);
    doc.patterns.push(pattern);
    assert!(doc.resolve_patterns());

    let p = document_to_graph(&doc, 1);
    let path = temp("pattern.lumen");
    let stats = save_graph_project(&path, &p.graph, &p.blobs, &p.meta, None).unwrap();
    assert_eq!(
        (stats.blobs, stats.blob_tiles),
        (2, 2),
        "the layer and the pattern"
    );
    let back = load_graph_project(&path).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(
        back.blobs.len(),
        1,
        "the pattern's pixels live on the overlay again"
    );
    let (_, node) = back
        .graph
        .nodes()
        .find(|(_, n)| n.name.as_deref() == Some("Patterned"))
        .unwrap();
    let Op::Layer { props } = &node.op else {
        panic!("{:?}", node.op)
    };
    let got = props
        .effects
        .pattern_overlay
        .as_ref()
        .unwrap()
        .pattern
        .image
        .as_ref()
        .unwrap();
    assert_eq!(**got, image);
    assert_eq!(got.get(1, 0), Rgba::from_straight(0.0, 1.0, 0.0, 1.0));

    let ours = Renderer::new().render_canvas(&back.graph, &back.blobs);
    let before = Renderer::new().render_canvas(&p.graph, &p.blobs);
    assert_same_bits(&ours, &before, doc.canvas());
    // The overlay tiles the pattern from the canvas origin at 80% over the
    // layer's 0.2 grey: x = 9 is its red column, x = 10 its green one.
    let close = |p: Rgba, q: [f32; 4]| {
        [(p.r, q[0]), (p.g, q[1]), (p.b, q[2]), (p.a, q[3])]
            .iter()
            .all(|(a, b)| (a - b).abs() < 1e-5)
    };
    assert!(
        close(ours.get_pixel(9, 8), [0.84, 0.04, 0.04, 1.0]),
        "{:?}",
        ours.get_pixel(9, 8)
    );
    assert!(
        close(ours.get_pixel(10, 8), [0.04, 0.84, 0.04, 1.0]),
        "{:?}",
        ours.get_pixel(10, 8)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_pattern_fill_keeps_the_blob_its_op_names() {
    // A fill op names its pattern's pixels by blob id in its own
    // parameters (`pattern_pixels`), not through an `image` node.
    let mut image = lumenply_tiles::Raster::new(2, 2);
    image.set(0, 0, Rgba::from_straight(1.0, 0.5, 0.0, 1.0));
    image.set(1, 1, Rgba::from_straight(0.0, 0.5, 1.0, 1.0));
    let mut doc = Document::new(40, 30);
    let fill = lumenply_doc::Fill::Pattern {
        pattern: lumenply_doc::PatternRef {
            id: "pat-2".into(),
            name: "Checks".into(),
            image: None,
        },
        scale: 1.0,
        offset: [0.0, 0.0],
        angle: 0.0,
    };
    let id = doc.alloc_id();
    doc.add_layer(Layer::with_content(
        id,
        "Pattern Fill",
        LayerContent::Fill(lumenply_doc::FillLayer::new(fill)),
    ));
    doc.patterns
        .push(lumenply_doc::Pattern::new("pat-2", "Checks", image.clone()));
    lumenply_render::fill::refresh_stale(&mut doc);

    let p = document_to_graph(&doc, 1);
    let (_, node) = p.graph.nodes().find(|(_, n)| n.op.type_name() == "fill").unwrap();
    let Op::Fill {
        pattern_pixels: Some(pixels),
        ..
    } = &node.op
    else {
        panic!("{:?}", node.op)
    };
    assert_eq!(pixels.size, [2, 2]);
    assert_eq!(referenced_blobs(&p.graph, &p.meta), BTreeSet::from([pixels.blob]));

    let path = temp("pattern-fill.lumen");
    let stats = save_graph_project(&path, &p.graph, &p.blobs, &p.meta, None).unwrap();
    assert_eq!((stats.blobs, stats.blob_tiles, stats.dropped_blobs), (1, 1, 0));
    let back = load_graph_project(&path).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(pixels.image(&back.blobs).unwrap(), image);
    let ours = Renderer::new().render_canvas(&back.graph, &back.blobs);
    let before = Renderer::new().render_canvas(&p.graph, &p.blobs);
    assert_same_bits(&ours, &before, doc.canvas());
    // The fill rests in 16-bit tiles: 0.5 is stored as 32768/65535.
    assert_eq!(ours.get_pixel(0, 0), Rgba::new(1.0, 32768.0 / 65535.0, 0.0, 1.0));
    let _ = std::fs::remove_file(&path);
}
