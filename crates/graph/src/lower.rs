//! Lowering a layer-tree [`Document`] to a graph. Each layer becomes a node
//! that composites its content onto the node below it, so the stack reads
//! bottom to top as a chain; groups become sub-chains. The choices mirror
//! `lumenply_render::render_tile_over` exactly (clip chains, when a group
//! passes through, what a hidden layer skips), so the graph renders the
//! same pixels as the document.
//!
//! A layer's own content is an operation where one exists (text, fill,
//! shape, a smart object's transform, smart filters; see `lower/content.rs`);
//! painted pixels are an `image` blob.

use std::collections::HashMap;

use lumenply_doc::{Document, Layer, LayerContent, LayerId};

use crate::blob::{BlobStore, TileHasher};
use crate::model::{Graph, Node, NodeId};
use crate::ops::{ClipMember, LayerProps, Op};

mod content;

/// A lowered document and where each layer ended up.
pub struct Lowered {
    pub graph: Graph,
    /// The node that composites each layer (for a clip chain, every
    /// member maps to the chain's node).
    pub layer_nodes: HashMap<LayerId, NodeId>,
}

pub fn lower(doc: &Document, blobs: &mut BlobStore, hasher: &TileHasher) -> Lowered {
    let mut cx = Cx {
        graph: Graph::new(doc.width, doc.height),
        blobs,
        hasher,
        layer_nodes: HashMap::new(),
        float: doc.float_mode,
    };
    let empty = cx.graph.add(Node::new(Op::Empty, vec![]));
    let out = cx.stack(doc.layers(), empty);
    cx.graph.output = Some(out);
    Lowered {
        graph: cx.graph,
        layer_nodes: cx.layer_nodes,
    }
}

struct Cx<'a> {
    graph: Graph,
    blobs: &'a mut BlobStore,
    hasher: &'a TileHasher,
    layer_nodes: HashMap<LayerId, NodeId>,
    /// The document's 32-bit mode: content ops keep float tiles.
    float: bool,
}

fn is_filter(l: &Layer) -> bool {
    matches!(l.content, LayerContent::Filter(_))
}

impl Cx<'_> {
    /// Composite `layers` (bottom to top) onto `backdrop`; returns the top.
    fn stack(&mut self, layers: &[Layer], backdrop: NodeId) -> NodeId {
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
                below = self.clip_group(&layers[idx..end], below);
                idx = end;
            } else {
                below = self.layer(layer, below);
                idx += 1;
            }
        }
        below
    }

    fn mask(&mut self, layer: &Layer) -> Option<NodeId> {
        let m = layer.mask.as_ref().filter(|m| m.enabled)?;
        let blob = (!m.tiles.is_empty()).then(|| self.blobs.insert(self.hasher, m.tiles.clone()));
        Some(self.graph.add(Node::new(
            Op::Mask {
                blob,
                default: m.default,
            },
            vec![],
        )))
    }

    /// The node producing a layer's own pixels, before mask and blending.
    fn content(&mut self, layer: &Layer) -> NodeId {
        if let LayerContent::Group(children) = &layer.content {
            let empty = self.graph.add(Node::new(Op::Empty, vec![]));
            return self.stack(children, empty);
        }
        self.own_content(layer)
    }

    fn layer(&mut self, layer: &Layer, below: NodeId) -> NodeId {
        let mask = self.mask(layer);
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
            LayerContent::Group(children)
                if layer.pass_through
                    && layer.effects.is_empty()
                    && !children
                        .iter()
                        .any(|c| c.visible && c.opacity > 0.0 && is_filter(c)) =>
            {
                let after = self.stack(children, below);
                Node::new(
                    Op::PassThrough {
                        visible: layer.visible,
                        opacity: layer.opacity * layer.fill_opacity,
                    },
                    vec![Some(below), Some(after), mask],
                )
            }
            _ => {
                let content = self.content(layer);
                Node::new(
                    Op::Layer {
                        props: LayerProps::of(layer),
                    },
                    vec![Some(below), Some(content), mask],
                )
            }
        };
        let id = self.graph.add(node.named(layer.name.clone()));
        self.layer_nodes.insert(layer.id, id);
        id
    }

    fn clip_group(&mut self, chain: &[Layer], below: NodeId) -> NodeId {
        let base = &chain[0];
        let base_content = self.content(base);
        let base_mask = self.mask(base);
        let mut inputs = vec![Some(below), Some(base_content), base_mask];
        let mut members = Vec::new();
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
                }
                _ => {
                    members.push(ClipMember::Layer {
                        props: Box::new(LayerProps::of(m)),
                    });
                    let content = self.content(m);
                    inputs.push(Some(content));
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
        .named(base.name.clone());
        let id = self.graph.add(node);
        for l in chain {
            self.layer_nodes.insert(l.id, id);
        }
        id
    }
}
