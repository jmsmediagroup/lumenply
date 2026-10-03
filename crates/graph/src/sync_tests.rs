//! Syncing documents into graph versions and projecting them back, the
//! `translate` op, and the history's version operations.

use std::sync::Arc;

use lumenply_doc::{Document, Layer, LayerContent};
use lumenply_tiles::{Raster, Rgba, TileStore};

use crate::sync::{content_edit, Base, ContentEdit};
use crate::tests::{assert_same, busy_document};
use crate::*;

fn same_pixels(a: &TileStore, b: &TileStore) -> bool {
    let mut ca: Vec<_> = a.coords().map(|c| (c.x, c.y)).collect();
    let mut cb: Vec<_> = b.coords().map(|c| (c.x, c.y)).collect();
    ca.sort();
    cb.sort();
    ca == cb && a.coords().all(|c| a.tile(c) == b.tile(c))
}

fn assert_same_layers(a: &[Layer], b: &[Layer]) {
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b) {
        assert_eq!(
            (x.id, &x.name, x.visible, x.opacity),
            (y.id, &y.name, y.visible, y.opacity)
        );
        assert_eq!(
            (x.blend, x.clip, x.pass_through),
            (y.blend, y.clip, y.pass_through)
        );
        assert_eq!(x.effects, y.effects);
        assert_eq!(x.mask.is_some(), y.mask.is_some(), "{}", x.name);
        if let (Some(p), Some(q)) = (&x.mask, &y.mask) {
            assert!(same_pixels(&p.tiles, &q.tiles), "{}: mask", x.name);
        }
        match (&x.content, &y.content) {
            (LayerContent::Group(p), LayerContent::Group(q)) => assert_same_layers(p, q),
            (LayerContent::Pixel(p), LayerContent::Pixel(q)) => assert!(same_pixels(p, q), "{}", x.name),
            (p, q) => assert_eq!(std::mem::discriminant(p), std::mem::discriminant(q)),
        }
    }
}

#[test]
fn a_synced_graph_is_the_lowered_graph_and_projects_back() {
    let doc = busy_document();
    let r = Renderer::new();
    let (mut b1, mut b2) = (BlobStore::new(), BlobStore::new());
    let lowered = lower(&doc, &mut b1, &r.hasher).graph;
    let (synced, state) = sync(None, &doc, &mut b2, &r.hasher);
    synced.validate().unwrap();
    // Same structure: the output's content key covers every op and edge.
    let key = |g: &Graph| r.keys(g, g.output.unwrap())[&g.output.unwrap()];
    assert_eq!(key(&synced), key(&lowered));
    assert_same(
        &r.render_canvas(&synced, &b2),
        &lumenply_render::composite(&doc),
        doc.canvas(),
    );
    // Every layer node is tagged with its layer; the stack shares one
    // empty node at the bottom.
    let tagged: Vec<u64> = synced.nodes().filter_map(|(_, n)| n.layer).collect();
    // Eleven layers composite; the clip chain of three is one node.
    assert_eq!(tagged.len(), 11, "{tagged:?}");
    assert_eq!(synced.nodes().filter(|(_, n)| n.op == Op::Empty).count(), 1);
    // Pixel layers' pixels are in the graph, not in the records.
    let red = state.layer(doc.layers()[1].id).unwrap();
    assert!(red.pixels_in_graph);
    assert!(red.layer.pixels().unwrap().is_empty());
    // The projection is the document.
    let back = project(&synced, &state, &b2, &r);
    assert_same_layers(back.layers(), doc.layers());
    assert_eq!(back.next_id(), doc.next_id());
}

#[test]
fn syncing_an_unchanged_document_changes_nothing() {
    let doc = busy_document();
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let (g1, s1) = sync(None, &doc, &mut blobs, &r.hasher);
    let base = Base {
        graph: &g1,
        state: &s1,
        doc: &doc,
    };
    let (g2, _) = sync(Some(base), &doc, &mut blobs, &r.hasher);
    assert_eq!(g2.len(), g1.len());
    for (id, _) in g1.nodes() {
        assert!(
            Arc::ptr_eq(g1.node_arc(id).unwrap(), g2.node_arc(id).unwrap()),
            "{id}"
        );
    }
}

#[test]
fn translate_moves_pixels_exactly_like_the_tile_store() {
    let mut src = Raster::new(300, 200);
    for y in 0..200 {
        for x in 0..300 {
            src.set(
                x,
                y,
                Rgba::from_straight(x as f32 / 300.0, y as f32 / 200.0, 0.5, 1.0),
            );
        }
    }
    let mut doc = Document::new(800, 600);
    let id = doc.alloc_id();
    let mut layer = Layer::pixel(id, "L");
    let mut store = TileStore::from_raster(&src, 30, 40);
    store.compact();
    *layer.pixels_mut().unwrap() = store.clone();
    doc.add_layer(layer);
    let r = Renderer::new();
    let mut blobs = BlobStore::new();
    let (graph, state) = sync(None, &doc, &mut blobs, &r.hasher);
    for (dx, dy) in [(1, 0), (-37, 255), (256, -512), (-1000, 3)] {
        let base = Base {
            graph: &graph,
            state: &state,
            doc: &doc,
        };
        let edit = ContentEdit::new(id, Op::Translate { dx, dy });
        let e = content_edit(base, &edit, false, &mut blobs, &r).unwrap();
        let ours = e.doc.layer(id).unwrap().pixels().unwrap();
        let mut theirs = store.translated(dx, dy);
        theirs.compact();
        assert!(same_pixels(ours, &theirs), "({dx}, {dy})");
        assert!(ours.coords().all(|c| ours.tile(c).unwrap().is_compact()));
        // The extent bounds the moved pixels.
        let top = e.state.layer(id).unwrap().content.unwrap();
        let ext = r.extent(&e.graph, &blobs, top).unwrap();
        let b = theirs.content_bounds().unwrap();
        assert_eq!(ext.intersect(&b), b);
    }
}

#[test]
fn history_versions_carry_payloads_squash_and_roll_back() {
    let mut h: History<u32> = History::with_payload(Graph::new(1, 1), 0);
    h.limit = usize::MAX;
    for i in 1..=5 {
        h.commit_with(Graph::new(i, 1), format!("step {i}"), i);
    }
    assert_eq!(h.cursor(), 5);
    // Merge the newest three into one step: versions 3 and 4 go.
    let dropped = h.squash(3, "three");
    assert_eq!(dropped.iter().map(|v| v.payload).collect::<Vec<_>>(), vec![3, 4]);
    let (labels, cursor) = h.labels();
    assert_eq!((labels, cursor), (vec!["Open", "step 1", "step 2", "three"], 3));
    assert_eq!((h.current().width, h.current_version().payload), (5, 5));
    // Roll back one step: no redo is left.
    let gone = h.rollback(1);
    assert_eq!(gone.len(), 1);
    assert_eq!(
        (h.cursor(), h.can_redo(), h.current_version().payload),
        (2, false, 2)
    );
    assert!(h.rollback(0).is_empty());
    // The oldest goes, never the current one.
    assert_eq!(h.drop_oldest().map(|v| v.payload), Some(0));
    assert_eq!(h.drop_oldest().map(|v| v.payload), Some(1));
    assert!(h.drop_oldest().is_none());
    assert_eq!(h.labels(), (vec!["step 2"], 0));
    // A limit of usize::MAX never trims (no overflow).
    h.commit_with(Graph::new(9, 9), "more", 9);
    assert_eq!(h.versions().len(), 2);
}
