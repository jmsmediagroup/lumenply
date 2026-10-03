//! A graph version's [`DocState`] as a project file's `meta.json` (ADR
//! 0026): what the document holds besides its graph, in JSON, with any
//! pixels it needs stored as blobs and listed under `"blobs"`.
//!
//! ```text
//! {"resolution": 72.0, "float_mode": false, "guides": [...],
//!  "work_path": {...}, "saved_paths": [...],
//!  "channels": [{"name": "Sky", "default": 0.0, "blob": "<hash>"}],
//!  "patterns": [{"id": "...", "name": "...", "pixels": {"blob": "<hash>", "size": [w, h]}}],
//!  "next_id": 12, "empty": "n1",
//!  "layers": [{"id": 1, "name": "Background", "node": "n3",
//!              "content_node": "n2", "kind": "pixel", "in_graph": true}, ...],
//!  "blobs": ["<hash>", ...]}
//! ```
//!
//! A layer record keeps the layer's settings and UI state and names the
//! nodes that carry it. A pixel layer's pixels are its content node's
//! output (`in_graph`), so painting stays a chain of operations in the
//! file; the parameters of text, fill, shape and smart layers are kept here
//! as the editor holds them, and their rendered pixels, which are derived,
//! are rebuilt on [`open`]. The selection is not saved, as in every
//! project format before.

use std::collections::BTreeSet;
use std::sync::Arc;

use lumenply_doc::{
    Adjustment, BlendMode, Document, Fill, FillLayer, Filter, Guide, Layer, LayerContent, LayerEffects,
    LayerId, LayerLocks, Mask, NamedPath, Pattern, SavedSelection, ShapeLayer, SmartFilter, SmartFilters,
    SmartLayer, TextLayer, VectorPath,
};
use lumenply_tiles::{Affine, TileStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob::{BlobId, BlobStore, TileHasher};
use crate::eval::Renderer;
use crate::model::{Graph, NodeId};
use crate::ops::Op;
use crate::ops_content::PatternPixels;
use crate::sync::{project, sync, Base, DocState, LayerRecord};

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_normal(m: &BlendMode) -> bool {
    *m == BlendMode::Normal
}

fn default_resolution() -> f32 {
    lumenply_doc::DEFAULT_RESOLUTION
}

#[derive(Serialize, Deserialize)]
struct Meta {
    #[serde(default = "default_resolution")]
    resolution: f32,
    #[serde(default)]
    float_mode: bool,
    #[serde(default)]
    guides: Vec<Guide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_path: Option<VectorPath>,
    #[serde(default)]
    saved_paths: Vec<NamedPath>,
    /// Saved selections.
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    patterns: Vec<MetaPattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_id: Option<LayerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    empty: Option<NodeId>,
    /// The layer tree, bottom to top. Missing in a meta that only carries
    /// document settings: such a project can't be edited as layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layers: Option<Vec<MetaLayer>>,
    /// Every blob the meta names, so a save keeps them.
    #[serde(default)]
    blobs: Vec<BlobId>,
}

#[derive(Serialize, Deserialize)]
struct Channel {
    name: String,
    default: f32,
    blob: Option<BlobId>,
}

#[derive(Serialize, Deserialize)]
struct MetaPattern {
    id: String,
    name: String,
    pixels: PatternPixels,
}

#[derive(Serialize, Deserialize)]
struct MetaMask {
    default: f32,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blob: Option<BlobId>,
}

#[derive(Serialize, Deserialize)]
struct MetaSmartFilters {
    filters: Vec<SmartFilter>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<MetaMask>,
}

#[derive(Serialize, Deserialize)]
struct MetaLayer {
    id: LayerId,
    name: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    visible: bool,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    opacity: f32,
    #[serde(default, skip_serializing_if = "is_normal")]
    blend: BlendMode,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    fill_opacity: f32,
    #[serde(default, skip_serializing_if = "LayerEffects::is_empty")]
    effects: LayerEffects,
    #[serde(default, skip_serializing_if = "is_false")]
    clip: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pass_through: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    collapsed: bool,
    #[serde(default, skip_serializing_if = "LayerLocks::is_empty")]
    locks: LayerLocks,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<MetaMask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    smart_filters: Option<MetaSmartFilters>,
    /// The node compositing the layer.
    node: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_node: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask_node: Option<NodeId>,
    #[serde(flatten)]
    content: MetaContent,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum MetaContent {
    /// The pixels are the content node's output (`in_graph`), or a blob.
    Pixel {
        #[serde(default, skip_serializing_if = "is_false")]
        in_graph: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob: Option<BlobId>,
    },
    Group {
        children: Vec<MetaLayer>,
    },
    Adjustment {
        adjustment: Adjustment,
    },
    Filter {
        filter: Filter,
    },
    /// `cache` only for pixels that aren't our rendering of the text (a
    /// PSD's, kept until the text is edited).
    Text {
        text: TextLayer,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<BlobId>,
    },
    Smart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<BlobId>,
        transform: [f32; 6],
    },
    Fill {
        fill: Fill,
    },
    Shape {
        shape: ShapeLayer,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("the document settings could not be read: {0}")]
    Json(String),
    #[error("the project keeps no layer records, so it can't be edited as layers")]
    NoLayers,
    #[error("the document settings name pixels the project doesn't have ({0})")]
    MissingBlob(BlobId),
    #[error("layer {0} names node {1}, which the graph doesn't have")]
    MissingNode(LayerId, NodeId),
}

/// Writes a [`DocState`] as meta, putting the pixels it holds into the
/// blob store.
struct Writer<'a> {
    graph: &'a Graph,
    blobs: &'a mut BlobStore,
    hasher: &'a TileHasher,
    named: BTreeSet<BlobId>,
}

impl Writer<'_> {
    fn blob(&mut self, store: &TileStore) -> Option<BlobId> {
        if store.is_empty() {
            return None;
        }
        let id = self.blobs.insert(self.hasher, store.clone());
        self.named.insert(id);
        Some(id)
    }

    fn mask(&mut self, m: &Mask) -> MetaMask {
        MetaMask {
            default: m.default,
            enabled: m.enabled,
            blob: self.blob(&m.tiles),
        }
    }

    fn layer(&mut self, r: &LayerRecord) -> MetaLayer {
        let l = &r.layer;
        let content = match &l.content {
            LayerContent::Pixel(store) => MetaContent::Pixel {
                in_graph: r.pixels_in_graph,
                blob: if r.pixels_in_graph { None } else { self.blob(store) },
            },
            LayerContent::Group(_) => MetaContent::Group {
                children: r.children.iter().map(|c| self.layer(c)).collect(),
            },
            LayerContent::Adjustment(a) => MetaContent::Adjustment {
                adjustment: a.clone(),
            },
            LayerContent::Filter(f) => MetaContent::Filter { filter: f.clone() },
            LayerContent::Text(t) => {
                // No text op behind the layer's content: the lowering kept
                // pixels that aren't our rendering of it (a PSD's), which
                // stay as they are until the text is edited.
                let has_text_op = |c: NodeId| {
                    self.graph
                        .upstream(c)
                        .into_iter()
                        .any(|n| matches!(self.graph.node(n).map(|n| &n.op), Some(Op::Text { .. })))
                };
                let not_ours = r.content.is_some_and(|c| !has_text_op(c));
                MetaContent::Text {
                    text: TextLayer {
                        cache: None,
                        ..t.clone()
                    },
                    cache: match (&t.cache, not_ours) {
                        (Some(c), true) => self.blob(c),
                        _ => None,
                    },
                }
            }
            LayerContent::Smart(s) => MetaContent::Smart {
                source: self.blob(&s.source),
                transform: s.transform.coeffs(),
            },
            LayerContent::Fill(f) => MetaContent::Fill { fill: f.fill.clone() },
            LayerContent::Shape(s) => MetaContent::Shape { shape: s.clone() },
        };
        let sf = &l.smart_filters;
        MetaLayer {
            id: l.id,
            name: l.name.clone(),
            visible: l.visible,
            opacity: l.opacity,
            blend: l.blend,
            fill_opacity: l.fill_opacity,
            effects: l.effects.clone(),
            clip: l.clip,
            pass_through: l.pass_through,
            collapsed: l.collapsed,
            locks: l.locks,
            mask: l.mask.as_ref().map(|m| self.mask(m)),
            smart_filters: (!sf.filters.is_empty() || sf.mask.is_some()).then(|| MetaSmartFilters {
                filters: sf.filters.clone(),
                enabled: sf.enabled,
                mask: sf.mask.as_ref().map(|m| self.mask(m)),
            }),
            node: r.node,
            content_node: r.content,
            mask_node: r.mask,
            content,
        }
    }
}

impl DocState {
    /// This state as a project file's `meta.json`, next to `graph` (the
    /// version it belongs to). Pixels the state holds outside the graph (a
    /// saved selection, a smart object's source, a disabled mask) go into
    /// `blobs` and are listed under `"blobs"`; pixels the graph already
    /// holds are the same blobs, stored once.
    pub fn to_meta(&self, graph: &Graph, blobs: &mut BlobStore, hasher: &TileHasher) -> Value {
        let mut w = Writer {
            graph,
            blobs,
            hasher,
            named: BTreeSet::new(),
        };
        let channels = self
            .saved_selections
            .iter()
            .map(|s| Channel {
                name: s.name.clone(),
                default: s.mask.default,
                blob: w.blob(&s.mask.tiles),
            })
            .collect();
        let patterns = self
            .patterns
            .iter()
            .map(|p| {
                let pixels = PatternPixels::store(&p.image, w.blobs, w.hasher);
                w.named.insert(pixels.blob);
                MetaPattern {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    pixels,
                }
            })
            .collect();
        let layers = self.layers.iter().map(|r| w.layer(r)).collect();
        let meta = Meta {
            resolution: self.resolution,
            float_mode: self.float_mode,
            guides: self.guides.clone(),
            work_path: self.work_path.clone(),
            saved_paths: self.saved_paths.clone(),
            channels,
            patterns,
            next_id: Some(self.next_id),
            empty: Some(self.empty),
            layers: Some(layers),
            blobs: w.named.into_iter().collect(),
        };
        serde_json::to_value(meta).expect("document settings serialise")
    }

    /// The state a project file's `meta.json` describes, beside `graph`.
    /// Derived pixels (text, fill, shape and smart caches, smart filter
    /// results) are left out; [`open`] rebuilds them.
    pub fn from_meta(meta: &Value, graph: &Graph, blobs: &BlobStore) -> Result<DocState, MetaError> {
        let m: Meta = serde_json::from_value(meta.clone()).map_err(|e| MetaError::Json(e.to_string()))?;
        let pixels = |id: &Option<BlobId>| -> Result<TileStore, MetaError> {
            match id {
                Some(id) => blobs
                    .get(id)
                    .map(|s| (**s).clone())
                    .ok_or(MetaError::MissingBlob(*id)),
                None => Ok(TileStore::new()),
            }
        };
        let mask = |m: &MetaMask| -> Result<Mask, MetaError> {
            Ok(Mask {
                tiles: pixels(&m.blob)?,
                default: m.default,
                enabled: m.enabled,
            })
        };
        fn records(
            list: &[MetaLayer],
            graph: &Graph,
            pixels: &dyn Fn(&Option<BlobId>) -> Result<TileStore, MetaError>,
            mask: &dyn Fn(&MetaMask) -> Result<Mask, MetaError>,
        ) -> Result<Vec<LayerRecord>, MetaError> {
            let mut out = Vec::with_capacity(list.len());
            for ml in list {
                for n in [Some(ml.node), ml.content_node, ml.mask_node]
                    .into_iter()
                    .flatten()
                {
                    if !graph.contains(n) {
                        return Err(MetaError::MissingNode(ml.id, n));
                    }
                }
                let mut children = Vec::new();
                let mut in_graph = false;
                let content = match &ml.content {
                    MetaContent::Pixel { in_graph: g, blob } => {
                        in_graph = *g;
                        LayerContent::Pixel(pixels(blob)?)
                    }
                    MetaContent::Group { children: c } => {
                        children = records(c, graph, pixels, mask)?;
                        LayerContent::Group(Vec::new())
                    }
                    MetaContent::Adjustment { adjustment } => LayerContent::Adjustment(adjustment.clone()),
                    MetaContent::Filter { filter } => LayerContent::Filter(filter.clone()),
                    MetaContent::Text { text, cache } => LayerContent::Text(TextLayer {
                        cache: match cache {
                            Some(_) => Some(pixels(cache)?),
                            None => None,
                        },
                        ..text.clone()
                    }),
                    MetaContent::Smart { source, transform } => LayerContent::Smart(SmartLayer {
                        source: pixels(source)?,
                        transform: Affine::from_coeffs(*transform),
                        cache: None,
                    }),
                    MetaContent::Fill { fill } => LayerContent::Fill(FillLayer::new(fill.clone())),
                    MetaContent::Shape { shape } => LayerContent::Shape(ShapeLayer {
                        cache: None,
                        ..shape.clone()
                    }),
                };
                let smart_filters = match &ml.smart_filters {
                    Some(sf) => SmartFilters {
                        filters: sf.filters.clone(),
                        enabled: sf.enabled,
                        mask: sf.mask.as_ref().map(mask).transpose()?,
                        cache: None,
                    },
                    None => SmartFilters::default(),
                };
                let layer = Layer {
                    id: ml.id,
                    name: ml.name.clone(),
                    visible: ml.visible,
                    opacity: ml.opacity,
                    blend: ml.blend,
                    effects: ml.effects.clone(),
                    fill_opacity: ml.fill_opacity,
                    clip: ml.clip,
                    pass_through: ml.pass_through,
                    mask: ml.mask.as_ref().map(mask).transpose()?,
                    content,
                    collapsed: ml.collapsed,
                    locks: ml.locks,
                    smart_filters,
                };
                out.push(LayerRecord {
                    layer,
                    node: ml.node,
                    content: ml.content_node,
                    mask: ml.mask_node,
                    pixels_in_graph: in_graph,
                    children,
                });
            }
            Ok(out)
        }
        let layers = records(
            m.layers.as_deref().ok_or(MetaError::NoLayers)?,
            graph,
            &pixels,
            &mask,
        )?;
        let saved_selections = m
            .channels
            .iter()
            .map(|c| {
                Ok(SavedSelection {
                    name: c.name.clone(),
                    mask: Mask {
                        tiles: pixels(&c.blob)?,
                        default: c.default,
                        enabled: true,
                    },
                })
            })
            .collect::<Result<_, MetaError>>()?;
        let patterns = m
            .patterns
            .iter()
            .map(|p| {
                let image = p
                    .pixels
                    .image(blobs)
                    .ok_or(MetaError::MissingBlob(p.pixels.blob))?;
                Ok(Pattern {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    image: Arc::new(image),
                })
            })
            .collect::<Result<_, MetaError>>()?;
        let mut max_id = 0;
        fn walk(list: &[LayerRecord], max: &mut LayerId) {
            for r in list {
                *max = (*max).max(r.layer.id);
                walk(&r.children, max);
            }
        }
        walk(&layers, &mut max_id);
        Ok(DocState {
            selection: None,
            work_path: m.work_path,
            saved_paths: m.saved_paths,
            float_mode: m.float_mode,
            guides: m.guides,
            saved_selections,
            last_selection: None,
            patterns,
            resolution: m.resolution,
            next_id: m.next_id.unwrap_or(0).max(max_id + 1),
            empty: m.empty.unwrap_or(NodeId(0)),
            layers,
        })
    }
}

/// Rebuild what a layer derives from its parameters, as loading a
/// layer-tree project does: text glyphs, a smart object's placed source,
/// then fills, shapes and smart filters (`refresh_stale`).
fn rebuild_derived(doc: &mut Document) {
    doc.for_each_layer_mut(|l| match &mut l.content {
        LayerContent::Text(t) if t.cache.is_none() => lumenply_render::text::refresh_cache(t),
        LayerContent::Smart(s) => s.cache = Some(lumenply_render::transform_store(&s.source, &s.transform)),
        _ => {}
    });
    lumenply_render::fill::refresh_stale(doc);
}

/// A version read from a project file, ready to edit: its graph, the state
/// beside it and the document they stand for, derived pixels rebuilt.
/// Pixel layers keep their nodes, chains of strokes and moves included;
/// the content of text, fill, shape and smart layers is lowered again from
/// their parameters (the same ops, rendering their rebuilt caches exactly).
pub fn open(
    graph: &Graph,
    meta: &Value,
    blobs: &mut BlobStore,
    renderer: &Renderer,
) -> Result<(Graph, DocState, Document), MetaError> {
    let state = DocState::from_meta(meta, graph, blobs)?;
    let bare = project(graph, &state, blobs, renderer);
    let mut doc = bare.clone();
    rebuild_derived(&mut doc);
    let base = Base {
        graph,
        state: &state,
        doc: &bare,
    };
    let (graph, state) = sync(Some(base), &doc, blobs, &renderer.hasher);
    Ok((graph, state, doc))
}
