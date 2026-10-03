//! The editor's document as graph versions (ADR 0025, stage 3).
//!
//! A version is a [`Graph`] plus a [`DocState`]: everything the document
//! holds besides what the graph does. That is the selection, paths,
//! guides, saved selections, patterns, resolution and float mode, and for
//! each layer its settings and UI state (name, locks, collapsed, clip,
//! ids) together with the nodes that carry it.
//!
//! - [`sync`] turns a document into the next version. It lowers the
//!   document as [`crate::lower`] does, but every layer whose content, mask
//!   and settings didn't change keeps the previous version's nodes (same
//!   ids, same `Arc`s), so their content keys stay the same and the render
//!   cache keeps hitting. Changed pixels become a new `image` blob; since
//!   tile hashes are memoised by tile, only the tiles that changed are
//!   hashed.
//! - [`project`] turns a version back into the document: settings from the
//!   records, a pixel layer's pixels from its content node in the graph.
//! - [`content_edit`] applies a command's [`ContentEdit`] natively: the op
//!   is appended to the layer's content chain and the layer's pixels are
//!   projected from the graph.
//!
//! The structure of the lowering (clip chains, pass-through groups) mirrors
//! `lower.rs` exactly; `a_synced_graph_is_the_lowered_graph` checks that the
//! two produce the same content key for the output.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lumenply_doc::{
    Document, Guide, Layer, LayerContent, LayerId, Mask, NamedPath, Pattern, SavedSelection, Selection,
    VectorPath,
};
use lumenply_tiles::{Tile, TileStore};

use crate::blob::{BlobId, BlobStore, TileHasher};
use crate::eval::Renderer;
use crate::lower::Step;
use crate::model::{Graph, Node, NodeId};
use crate::ops::{ClipMember, LayerProps, Op};

/// What a graph version holds besides its graph: the document state that
/// isn't pixels. Pixel data in it (the selection, saved selections, a few
/// kinds of layer content, below) is shared copy-on-write with the
/// document, as undo snapshots always were.
///
/// Everything here is plain data meant for the project file's
/// `meta.json`: node ids serialise as `"n12"`, and the tile stores it holds
/// can be written as blobs.
#[derive(Clone, Debug)]
pub struct DocState {
    pub selection: Option<Selection>,
    pub work_path: Option<VectorPath>,
    pub saved_paths: Vec<NamedPath>,
    pub float_mode: bool,
    pub guides: Vec<Guide>,
    pub saved_selections: Vec<SavedSelection>,
    pub last_selection: Option<Selection>,
    pub patterns: Vec<Pattern>,
    pub resolution: f32,
    /// The id the next new layer gets.
    pub next_id: LayerId,
    /// The transparent node at the bottom of the layer stack (and of every
    /// isolated group's).
    pub empty: NodeId,
    /// The layer tree, bottom to top.
    pub layers: Vec<LayerRecord>,
}

/// One layer of a version: its settings and where its pixels are.
#[derive(Clone, Debug)]
pub struct LayerRecord {
    /// The layer without what the graph holds for it: a pixel layer whose
    /// pixels are `content`'s output keeps an empty store here, and a
    /// group's children are in `children` (its own list is left empty).
    /// Everything else (mask, smart filters, text, smart object source)
    /// stays here.
    pub layer: Layer,
    /// The node compositing the layer; for a member of a clip chain, the
    /// chain's node.
    pub node: NodeId,
    /// The node producing the layer's own pixels (for a group, the top of
    /// its children's stack); `None` for adjustment, filter and
    /// pass-through group layers.
    pub content: Option<NodeId>,
    /// The layer's mask node, when it has an enabled mask.
    pub mask: Option<NodeId>,
    /// The layer's pixels are `content`'s output (a pixel layer without
    /// smart filters); otherwise `layer` keeps them.
    pub pixels_in_graph: bool,
    pub children: Vec<LayerRecord>,
}

impl DocState {
    fn of(doc: &Document, empty: NodeId, layers: Vec<LayerRecord>) -> Self {
        DocState {
            selection: doc.selection.clone(),
            work_path: doc.work_path.clone(),
            saved_paths: doc.saved_paths.clone(),
            float_mode: doc.float_mode,
            guides: doc.guides.clone(),
            saved_selections: doc.saved_selections.clone(),
            last_selection: doc.last_selection.clone(),
            patterns: doc.patterns.clone(),
            resolution: doc.resolution,
            next_id: doc.next_id(),
            empty,
            layers,
        }
    }

    /// The record of layer `id`, anywhere in the tree.
    pub fn layer(&self, id: LayerId) -> Option<&LayerRecord> {
        fn find(list: &[LayerRecord], id: LayerId) -> Option<&LayerRecord> {
            list.iter().find_map(|r| {
                if r.layer.id == id {
                    Some(r)
                } else {
                    find(&r.children, id)
                }
            })
        }
        find(&self.layers, id)
    }

    fn layer_mut(&mut self, id: LayerId) -> Option<&mut LayerRecord> {
        fn find(list: &mut [LayerRecord], id: LayerId) -> Option<&mut LayerRecord> {
            for r in list {
                if r.layer.id == id {
                    return Some(r);
                }
                if let Some(found) = find(&mut r.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&mut self.layers, id)
    }

    /// Visit every record depth-first, bottom to top.
    pub fn for_each_layer<'a>(&'a self, mut f: impl FnMut(&'a LayerRecord)) {
        fn walk<'a>(list: &'a [LayerRecord], f: &mut impl FnMut(&'a LayerRecord)) {
            for r in list {
                f(r);
                walk(&r.children, f);
            }
        }
        walk(&self.layers, &mut f);
    }
}

/// A version and the document it projects to: what [`sync`] may reuse.
#[derive(Clone, Copy)]
pub struct Base<'a> {
    pub graph: &'a Graph,
    pub state: &'a DocState,
    pub doc: &'a Document,
}

/// Same tiles, by allocation: an edit replaces exactly the tiles it touches.
pub fn same_store(a: &TileStore, b: &TileStore) -> bool {
    a.len() == b.len()
        && a.coords()
            .all(|c| matches!((a.tile_arc(c), b.tile_arc(c)), (Some(x), Some(y)) if Arc::ptr_eq(x, y)))
}

fn same_mask(a: &Mask, b: &Mask) -> bool {
    a.enabled == b.enabled && a.default == b.default && same_store(&a.tiles, &b.tiles)
}

/// A pixel layer whose pixels are what it composites (no smart filters in
/// between): the kind of layer whose pixels the graph holds.
fn plain_pixels(layer: &Layer) -> Option<&TileStore> {
    match &layer.content {
        LayerContent::Pixel(s) if layer.smart_filters.filtered().is_none() => Some(s),
        _ => None,
    }
}

fn is_filter(l: &Layer) -> bool {
    matches!(l.content, LayerContent::Filter(_))
}

/// The layer for a record: everything but the pixels the graph holds.
fn strip(layer: &Layer, pixels_in_graph: bool) -> Layer {
    let content = match &layer.content {
        LayerContent::Pixel(_) if pixels_in_graph => LayerContent::Pixel(TileStore::new()),
        LayerContent::Group(_) => LayerContent::Group(Vec::new()),
        c => c.clone(),
    };
    Layer {
        id: layer.id,
        name: layer.name.clone(),
        visible: layer.visible,
        opacity: layer.opacity,
        blend: layer.blend,
        effects: layer.effects.clone(),
        fill_opacity: layer.fill_opacity,
        clip: layer.clip,
        pass_through: layer.pass_through,
        mask: layer.mask.clone(),
        content,
        collapsed: layer.collapsed,
        locks: layer.locks,
        smart_filters: layer.smart_filters.clone(),
    }
}

/// What the previous version had for one layer.
struct Prev<'a> {
    layer: &'a Layer,
    record: &'a LayerRecord,
}

fn index<'a>(base: Base<'a>) -> HashMap<LayerId, Prev<'a>> {
    let mut records: HashMap<LayerId, &'a LayerRecord> = HashMap::new();
    fn walk<'a>(list: &'a [LayerRecord], out: &mut HashMap<LayerId, &'a LayerRecord>) {
        for r in list {
            out.insert(r.layer.id, r);
            walk(&r.children, out);
        }
    }
    walk(&base.state.layers, &mut records);
    let mut out = HashMap::with_capacity(records.len());
    fn join<'a>(
        list: &'a [Layer],
        records: &HashMap<LayerId, &'a LayerRecord>,
        out: &mut HashMap<LayerId, Prev<'a>>,
    ) {
        for l in list {
            if let Some(record) = records.get(&l.id) {
                out.insert(l.id, Prev { layer: l, record });
            }
            if let Some(c) = l.children() {
                join(c, records, out);
            }
        }
    }
    join(base.doc.layers(), &records, &mut out);
    out
}

/// Turn `doc` into a graph version, reusing `base`'s nodes for every layer
/// that didn't change (see the module docs). Without a base this is a
/// fresh lowering, with [`DocState`] records.
pub fn sync(
    base: Option<Base>,
    doc: &Document,
    blobs: &mut BlobStore,
    hasher: &TileHasher,
) -> (Graph, DocState) {
    let mut graph = Graph::new(doc.width, doc.height);
    if let Some(b) = base {
        graph.continue_ids(b.graph);
    }
    let mut sx = Sx {
        graph,
        base,
        prev: base.map(index).unwrap_or_default(),
        blobs,
        hasher,
        float: doc.float_mode,
        empty: NodeId(0),
    };
    sx.empty = sx.bottom();
    let (out, layers) = sx.stack(doc.layers(), sx.empty);
    sx.graph.output = Some(out);
    let empty = sx.empty;
    (sx.graph, DocState::of(doc, empty, layers))
}

struct Sx<'a> {
    graph: Graph,
    base: Option<Base<'a>>,
    prev: HashMap<LayerId, Prev<'a>>,
    blobs: &'a mut BlobStore,
    hasher: &'a TileHasher,
    float: bool,
    empty: NodeId,
}

impl Sx<'_> {
    /// Lower one part of the document as [`crate::lower`] does.
    fn lowering(&mut self) -> Step<'_> {
        Step {
            graph: &mut self.graph,
            blobs: self.blobs,
            hasher: self.hasher,
            float: self.float,
        }
    }

    /// Copy `id` and everything it depends on from the base graph, keeping
    /// ids and `Arc`s. False when the base doesn't have it.
    fn carry(&mut self, id: NodeId) -> bool {
        let Some(b) = self.base else { return false };
        if !b.graph.contains(id) {
            return false;
        }
        for n in b.graph.upstream(id) {
            if !self.graph.contains(n) {
                match b.graph.node_arc(n) {
                    Some(node) => self.graph.insert_arc(n, node.clone()),
                    None => return false,
                }
            }
        }
        true
    }

    fn bottom(&mut self) -> NodeId {
        if let Some(b) = self.base {
            let id = b.state.empty;
            if b.graph.node(id).is_some_and(|n| n.op == Op::Empty) && self.carry(id) {
                return id;
            }
        }
        self.graph.add(Node::new(Op::Empty, vec![]))
    }

    /// Add the node compositing `layer`, under the id it had before (its
    /// `Arc` too, when nothing about it changed).
    fn place(&mut self, layer: LayerId, node: Node) -> NodeId {
        if let (Some(b), Some(p)) = (self.base, self.prev.get(&layer)) {
            let id = p.record.node;
            if !self.graph.contains(id) {
                if let Some(old) = b.graph.node_arc(id) {
                    let arc = if **old == node {
                        old.clone()
                    } else {
                        Arc::new(node)
                    };
                    self.graph.insert_arc(id, arc);
                    return id;
                }
            }
        }
        self.graph.add(node)
    }

    /// Composite `layers` (bottom to top) onto `backdrop`; returns the top
    /// and the layers' records. Mirrors `lower.rs`.
    fn stack(&mut self, layers: &[Layer], backdrop: NodeId) -> (NodeId, Vec<LayerRecord>) {
        let mut records = Vec::with_capacity(layers.len());
        let mut below = backdrop;
        let mut idx = 0;
        while idx < layers.len() {
            let layer = &layers[idx];
            let mut end = idx + 1;
            while end < layers.len() && layers[end].clip && !is_filter(&layers[end]) {
                end += 1;
            }
            let baseable = !layer.clip
                && matches!(
                    layer.content,
                    LayerContent::Pixel(_)
                        | LayerContent::Text(_)
                        | LayerContent::Smart(_)
                        | LayerContent::Fill(_)
                        | LayerContent::Shape(_)
                        | LayerContent::Group(_)
                );
            if end > idx + 1 && baseable {
                below = self.clip_group(&layers[idx..end], below, &mut records);
                idx = end;
            } else {
                below = self.layer(layer, below, &mut records);
                idx += 1;
            }
        }
        (below, records)
    }

    fn mask(&mut self, layer: &Layer) -> Option<NodeId> {
        let m = layer.mask.as_ref().filter(|m| m.enabled)?;
        if let Some(p) = self.prev.get(&layer.id) {
            if let (Some(id), Some(old)) = (p.record.mask, p.layer.mask.as_ref()) {
                if same_mask(old, m) && self.carry(id) {
                    return Some(id);
                }
            }
        }
        self.lowering().mask(layer)
    }

    /// The previous version's content nodes for `layer`, if its pixels are
    /// the very tiles that version projected to.
    fn reusable_content(&mut self, layer: &Layer) -> Option<(NodeId, bool)> {
        let p = self.prev.get(&layer.id)?;
        let id = p.record.content?;
        if std::mem::discriminant(&p.layer.content) != std::mem::discriminant(&layer.content) {
            return None;
        }
        let same = match (p.layer.raster_store(), layer.raster_store()) {
            (None, None) => true,
            (Some(a), Some(b)) => same_store(a, b),
            _ => false,
        };
        let in_graph = p.record.pixels_in_graph;
        (same && self.carry(id)).then_some((id, in_graph))
    }

    /// Whether `node` outputs exactly `store` (an `image` of those pixels).
    fn holds(&self, node: NodeId, store: &TileStore) -> bool {
        match self.graph.node(node).map(|n| &n.op) {
            Some(Op::Empty) => store.is_empty(),
            Some(Op::Image { blob }) => {
                self.blobs.get(blob).is_some_and(|b| same_store(b, store))
                    || self.hasher.store(store) == *blob
            }
            _ => false,
        }
    }

    /// The node producing a layer's own pixels, the records of a group's
    /// children, and whether the graph holds the layer's pixels.
    fn content(&mut self, layer: &Layer) -> (NodeId, Vec<LayerRecord>, bool) {
        if let LayerContent::Group(children) = &layer.content {
            let (top, records) = self.stack(children, self.empty);
            return (top, records, false);
        }
        let plain = plain_pixels(layer);
        if let Some((id, was_in_graph)) = self.reusable_content(layer) {
            let in_graph = plain.is_some_and(|s| was_in_graph || self.holds(id, s));
            return (id, Vec::new(), in_graph);
        }
        let id = self.lowering().content(layer);
        let in_graph = plain.is_some_and(|s| self.holds(id, s));
        (id, Vec::new(), in_graph)
    }

    fn layer(&mut self, layer: &Layer, below: NodeId, out: &mut Vec<LayerRecord>) -> NodeId {
        let mask = self.mask(layer);
        let mut content = None;
        let mut children = Vec::new();
        let mut in_graph = false;
        let node = match &layer.content {
            LayerContent::Adjustment(adj) => Node::new(
                Op::Adjustment {
                    adjustment: adj.clone(),
                    visible: layer.visible,
                    opacity: layer.opacity,
                    blend: layer.blend,
                },
                vec![Some(below), mask],
            ),
            LayerContent::Filter(f) => Node::new(
                Op::FilterLayer {
                    filter: f.clone(),
                    visible: layer.visible,
                    opacity: layer.opacity,
                },
                vec![Some(below), mask],
            ),
            LayerContent::Group(ch)
                if layer.pass_through
                    && layer.effects.is_empty()
                    && !ch.iter().any(|c| c.visible && c.opacity > 0.0 && is_filter(c)) =>
            {
                let (after, records) = self.stack(ch, below);
                children = records;
                Node::new(
                    Op::PassThrough {
                        visible: layer.visible,
                        opacity: layer.opacity * layer.fill_opacity,
                    },
                    vec![Some(below), Some(after), mask],
                )
            }
            _ => {
                let (c, records, g) = self.content(layer);
                content = Some(c);
                children = records;
                in_graph = g;
                Node::new(
                    Op::Layer {
                        props: LayerProps::of(layer),
                    },
                    vec![Some(below), Some(c), mask],
                )
            }
        };
        let id = self.place(layer.id, node.named(layer.name.clone()).for_layer(layer.id));
        out.push(LayerRecord {
            layer: strip(layer, in_graph),
            node: id,
            content,
            mask,
            pixels_in_graph: in_graph,
            children,
        });
        id
    }

    fn clip_group(&mut self, chain: &[Layer], below: NodeId, out: &mut Vec<LayerRecord>) -> NodeId {
        let base = &chain[0];
        let (base_content, base_children, base_in_graph) = self.content(base);
        let base_mask = self.mask(base);
        let mut inputs = vec![Some(below), Some(base_content), base_mask];
        let mut members = Vec::new();
        // (content, mask, children, pixels in graph) per member.
        let mut parts = Vec::new();
        for m in &chain[1..] {
            let mask = self.mask(m);
            match &m.content {
                LayerContent::Adjustment(adj) => {
                    members.push(ClipMember::Adjustment {
                        adjustment: adj.clone(),
                        visible: m.visible,
                        opacity: m.opacity,
                        blend: m.blend,
                    });
                    inputs.push(None);
                    parts.push((None, mask, Vec::new(), false));
                }
                _ => {
                    members.push(ClipMember::Layer {
                        props: Box::new(LayerProps::of(m)),
                    });
                    let (c, children, g) = self.content(m);
                    inputs.push(Some(c));
                    parts.push((Some(c), mask, children, g));
                }
            }
            inputs.push(mask);
        }
        let node = Node::new(
            Op::ClipGroup {
                base: LayerProps::of(base),
                members,
            },
            inputs,
        )
        .named(base.name.clone())
        .for_layer(base.id);
        let id = self.place(base.id, node);
        out.push(LayerRecord {
            layer: strip(base, base_in_graph),
            node: id,
            content: Some(base_content),
            mask: base_mask,
            pixels_in_graph: base_in_graph,
            children: base_children,
        });
        for (m, (content, mask, children, g)) in chain[1..].iter().zip(parts) {
            out.push(LayerRecord {
                layer: strip(m, g),
                node: id,
                content,
                mask,
                pixels_in_graph: g,
                children,
            });
        }
        id
    }
}

/// A layer's pixels from its content node. New tiles an op computed are
/// stored at rest (16-bit) as a command's would be, unless the document
/// is in float mode; tiles passed through from a blob stay as they are.
fn content_pixels(
    graph: &Graph,
    blobs: &BlobStore,
    renderer: &Renderer,
    node: NodeId,
    float: bool,
) -> TileStore {
    match graph.node(node).map(|n| &n.op) {
        None | Some(Op::Empty) => TileStore::new(),
        Some(Op::Image { blob }) => blobs.get(blob).map(|s| (**s).clone()).unwrap_or_default(),
        Some(_) => {
            let mut out = renderer.render_all(graph, blobs, node);
            if !float {
                let mut sources: HashSet<*const Tile> = HashSet::new();
                for id in graph.upstream(node) {
                    for b in graph.node(id).into_iter().flat_map(|n| n.op.blob_refs()) {
                        if let Some(s) = blobs.get(b) {
                            sources.extend(s.coords().filter_map(|c| s.tile_arc(c)).map(Arc::as_ptr));
                        }
                    }
                }
                let coords: Vec<_> = out.coords().collect();
                for c in coords {
                    let t = out.tile_arc(c).expect("listed coordinate");
                    if !t.is_compact() && !sources.contains(&Arc::as_ptr(t)) {
                        let mut copy = (**t).clone();
                        copy.compact();
                        out.insert(c, Arc::new(copy));
                    }
                }
            }
            out
        }
    }
}

impl LayerRecord {
    fn to_layer(&self, graph: &Graph, blobs: &BlobStore, renderer: &Renderer, float: bool) -> Layer {
        let mut l = self.layer.clone();
        match &mut l.content {
            LayerContent::Pixel(s) if self.pixels_in_graph => {
                if let Some(c) = self.content {
                    *s = content_pixels(graph, blobs, renderer, c, float);
                }
            }
            LayerContent::Group(children) => {
                *children = self
                    .children
                    .iter()
                    .map(|r| r.to_layer(graph, blobs, renderer, float))
                    .collect();
            }
            _ => {}
        }
        l
    }
}

/// The document a version stands for.
pub fn project(graph: &Graph, state: &DocState, blobs: &BlobStore, renderer: &Renderer) -> Document {
    let layers = state
        .layers
        .iter()
        .map(|r| r.to_layer(graph, blobs, renderer, state.float_mode))
        .collect();
    let mut doc = Document::from_parts(graph.width, graph.height, layers, state.next_id);
    doc.selection = state.selection.clone();
    doc.work_path = state.work_path.clone();
    doc.saved_paths = state.saved_paths.clone();
    doc.float_mode = state.float_mode;
    doc.guides = state.guides.clone();
    doc.saved_selections = state.saved_selections.clone();
    doc.last_selection = state.last_selection.clone();
    doc.patterns = state.patterns.clone();
    doc.resolution = state.resolution;
    doc
}

/// A pixel operation a command applies to one layer's content instead of
/// producing new pixels (ADR 0025, stage 3): the editor appends `op` to the
/// layer's content chain and projects the layer's pixels from the graph.
#[derive(Clone, Debug)]
pub struct ContentEdit {
    /// The pixel layer whose content the op transforms.
    pub layer: LayerId,
    /// The op. Its first input port receives the layer's current content.
    pub op: Op,
    /// Inputs for the op's further ports, in order: pixels it reads (a
    /// selection to clip to, say), stored as blobs.
    pub inputs: Vec<EditInput>,
}

/// Pixel data feeding one of a [`ContentEdit`]'s extra ports.
#[derive(Clone, Debug)]
pub enum EditInput {
    /// Pixels, as an `image` node.
    Image(TileStore),
    /// Coverage (alpha, `default` outside its tiles), as a `mask` node.
    Mask(Mask),
}

impl ContentEdit {
    pub fn new(layer: LayerId, op: Op) -> Self {
        ContentEdit {
            layer,
            op,
            inputs: Vec::new(),
        }
    }

    pub fn with_input(mut self, input: EditInput) -> Self {
        self.inputs.push(input);
        self
    }
}

/// A version with a [`ContentEdit`] applied and the document it projects to.
pub struct Edited {
    pub graph: Graph,
    pub state: DocState,
    pub doc: Document,
}

impl Edited {
    pub fn base(&self) -> Base<'_> {
        Base {
            graph: &self.graph,
            state: &self.state,
            doc: &self.doc,
        }
    }
}

/// Merge `op` into the op on top of a content chain when the two make one
/// exact op (consecutive whole-pixel moves).
fn fold(top: &Op, op: &Op) -> Option<Op> {
    match (top, op) {
        (Op::Translate { dx: a, dy: b }, Op::Translate { dx: c, dy: d }) => Some(Op::Translate {
            dx: a.checked_add(*c)?,
            dy: b.checked_add(*d)?,
        }),
        _ => None,
    }
}

/// Apply `edit` to `base` natively. `None` when the graph doesn't hold the
/// layer's pixels (no such layer, not a pixel layer, smart filters): the
/// command then runs on the document instead. With `fold`, an op that
/// merges exactly with the one on top of the layer's chain replaces it
/// rather than stacking on it (a coalescing run of nudges stays one node).
pub fn content_edit(
    base: Base,
    edit: &ContentEdit,
    fold_into_top: bool,
    blobs: &mut BlobStore,
    renderer: &Renderer,
) -> Option<Edited> {
    let record = base.state.layer(edit.layer)?;
    if !record.pixels_in_graph {
        return None;
    }
    let content = record.content?;
    let mut graph = base.graph.clone();
    let folded = match graph.node(content) {
        Some(top) if fold_into_top && edit.inputs.is_empty() && top.inputs.len() == 1 => {
            fold(&top.op, &edit.op).map(|op| Node::new(op, top.inputs.clone()))
        }
        _ => None,
    };
    let top = match folded {
        Some(node) => graph.add(node),
        None => {
            let mut inputs = vec![Some(content)];
            for i in &edit.inputs {
                let op = match i {
                    EditInput::Image(s) if s.is_empty() => Op::Empty,
                    EditInput::Image(s) => Op::Image {
                        blob: blobs.insert(&renderer.hasher, s.clone()),
                    },
                    EditInput::Mask(m) => Op::Mask {
                        blob: (!m.tiles.is_empty()).then(|| blobs.insert(&renderer.hasher, m.tiles.clone())),
                        default: m.default,
                    },
                };
                inputs.push(Some(graph.add(Node::new(op, vec![]))));
            }
            graph.add(Node::new(edit.op.clone(), inputs))
        }
    };
    // Point the compositing node's content port (never the backdrop) at the
    // new top of the chain.
    let mut rewired = 0;
    graph
        .update(record.node, |n| {
            for slot in n.inputs.iter_mut().skip(1) {
                if *slot == Some(content) {
                    *slot = Some(top);
                    rewired += 1;
                }
            }
        })
        .ok()?;
    if rewired != 1 {
        return None;
    }
    graph.collect_garbage();
    let mut state = base.state.clone();
    state.layer_mut(edit.layer)?.content = Some(top);
    let pixels = content_pixels(&graph, blobs, renderer, top, state.float_mode);
    let mut doc = base.doc.clone();
    *doc.layer_mut(edit.layer)?.pixels_mut()? = pixels;
    Some(Edited { graph, state, doc })
}

impl Op {
    /// The blobs this op names. The match lists every op on purpose:
    /// dropping unused blobs relies on it, so a new op must say here which
    /// blobs it reads.
    pub fn blob_refs(&self) -> impl Iterator<Item = &BlobId> {
        let blob = match self {
            Op::Image { blob } => Some(blob),
            Op::Mask { blob, .. } => blob.as_ref(),
            Op::Fill { pattern_pixels, .. } | Op::Shape { pattern_pixels, .. } => {
                pattern_pixels.as_ref().map(|p| &p.blob)
            }
            Op::Empty
            | Op::Layer { .. }
            | Op::Adjustment { .. }
            | Op::FilterLayer { .. }
            | Op::PassThrough { .. }
            | Op::ClipGroup { .. }
            | Op::Text { .. }
            | Op::Transform { .. }
            | Op::SmartFilter { .. }
            | Op::Translate { .. }
            | Op::Compact => None,
        };
        blob.into_iter()
    }
}

/// Every blob `graph` names.
pub fn graph_blobs(graph: &Graph) -> HashSet<BlobId> {
    graph
        .nodes()
        .flat_map(|(_, n)| n.op.blob_refs().copied())
        .collect()
}

/// Drop the blobs none of `graphs` names.
pub fn collect_blobs<'a>(blobs: &mut BlobStore, graphs: impl IntoIterator<Item = &'a Graph>) {
    let mut keep = HashSet::new();
    for g in graphs {
        keep.extend(graph_blobs(g));
    }
    blobs.retain(&keep);
}

/// Tile stores a version's records keep outside the graph, per layer and
/// kind: content the graph doesn't hold, a smart object's source, smart
/// filter caches, masks.
fn record_stores(state: &DocState) -> HashMap<(LayerId, u8), &TileStore> {
    let mut out = HashMap::new();
    state.for_each_layer(|r| {
        let l = &r.layer;
        if let Some(s) = l.content_store().filter(|s| !s.is_empty()) {
            out.insert((l.id, 0), s);
        }
        if let LayerContent::Smart(sm) = &l.content {
            out.insert((l.id, 1), &sm.source);
        }
        if let Some(c) = &l.smart_filters.cache {
            out.insert((l.id, 2), &c.store);
        }
        if let Some(m) = &l.mask {
            out.insert((l.id, 3), &m.tiles);
        }
    });
    out
}

/// Estimated bytes of pixel data version `a` keeps alive that version `b`
/// doesn't: the tiles of blobs only `a` names, and of record stores that
/// changed from `a` to `b`, less any tile `b` holds too. This is what
/// dropping `a` from a history whose next version is `b` frees. A shared
/// tile counts once.
pub fn released_bytes(a: (&Graph, &DocState), b: (&Graph, &DocState), blobs: &BlobStore) -> usize {
    let (a_blobs, b_blobs) = (graph_blobs(a.0), graph_blobs(b.0));
    let (a_stores, b_stores) = (record_stores(a.1), record_stores(b.1));
    let tiles =
        |s: &TileStore| -> Vec<Arc<Tile>> { s.coords().filter_map(|c| s.tile_arc(c).cloned()).collect() };
    let mut kept: HashSet<*const Tile> = HashSet::new();
    for id in b_blobs.difference(&a_blobs) {
        if let Some(s) = blobs.get(id) {
            kept.extend(tiles(s).iter().map(Arc::as_ptr));
        }
    }
    for (k, s) in &b_stores {
        if a_stores.get(k).is_none_or(|o| !same_store(o, s)) {
            kept.extend(tiles(s).iter().map(Arc::as_ptr));
        }
    }
    let mut seen = HashSet::new();
    let mut total = 0;
    let mut count = |s: &TileStore| {
        for t in tiles(s) {
            let p = Arc::as_ptr(&t);
            if !kept.contains(&p) && seen.insert(p) {
                total += t.byte_size();
            }
        }
    };
    for id in a_blobs.difference(&b_blobs) {
        if let Some(s) = blobs.get(id) {
            count(s);
        }
    }
    for (k, s) in &a_stores {
        if b_stores.get(k).is_none_or(|n| !same_store(s, n)) {
            count(s);
        }
    }
    total
}
