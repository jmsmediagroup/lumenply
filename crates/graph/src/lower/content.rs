//! Lowering a layer's own content: text, fill, shape and smart-object
//! layers become operations rather than the pixels they render to, and a
//! layer's smart filters become a chain of nodes after its content. Only
//! painted pixels, a smart object's source and a PSD text layer still
//! showing Photoshop's own rendering are `image` blobs.

use lumenply_doc::{Fill, Layer, LayerContent, Mask, TextLayer};
use lumenply_tiles::TileStore;

use super::Cx;
use crate::model::{Node, NodeId};
use crate::ops::Op;
use crate::ops_content::PatternPixels;

/// Whether a text layer's cache is our own rendering of it (or missing),
/// rather than the pixels Photoshop saved in a PSD. Those stand in until
/// the text is first edited (they are exact even when a font is missing
/// here), so such a layer stays an `image` blob of them for now. A cache
/// counts as ours when every tile equals our rendering, or our rendering
/// stored at 16 bits (the editor compacts caches at rest).
fn rendered_here(t: &TextLayer) -> bool {
    let Some(cache) = &t.cache else {
        return true;
    };
    let ours = lumenply_render::text::rasterize(t);
    cache.len() == ours.len()
        && ours.coords().all(|c| match (cache.tile(c), ours.tile(c)) {
            (Some(a), Some(b)) => {
                a == b
                    || (a.is_compact() && {
                        let mut b = b.clone();
                        b.compact();
                        *a == b
                    })
            }
            _ => false,
        })
}

impl Cx<'_> {
    fn image(&mut self, store: &TileStore) -> Op {
        if store.is_empty() {
            Op::Empty
        } else {
            Op::Image {
                blob: self.blobs.insert(self.hasher, store.clone()),
            }
        }
    }

    /// `fill` without the pattern pixels its reference carries, and those
    /// pixels as a blob.
    fn fill_and_pattern(&mut self, fill: &Fill) -> (Fill, Option<PatternPixels>) {
        let mut fill = fill.clone();
        let mut pixels = None;
        if let Fill::Pattern { pattern, .. } = &mut fill {
            if let Some(image) = pattern.image.take() {
                pixels = Some(PatternPixels::store(&image, self.blobs, self.hasher));
            }
        }
        (fill, pixels)
    }

    /// The node producing a non-group layer's pixels before its mask and
    /// blending: its content, then its smart filters.
    pub(super) fn own_content(&mut self, layer: &Layer) -> NodeId {
        let float = self.float;
        let op = match &layer.content {
            LayerContent::Pixel(store) => self.image(store),
            LayerContent::Text(t) if rendered_here(t) => Op::Text {
                text: Box::new(TextLayer {
                    cache: None,
                    ..t.clone()
                }),
            },
            LayerContent::Text(t) => match &t.cache {
                Some(photoshop) => self.image(photoshop),
                None => Op::Empty,
            },
            LayerContent::Fill(f) => {
                let (fill, pattern_pixels) = self.fill_and_pattern(&f.fill);
                Op::Fill {
                    fill,
                    pattern_pixels,
                    float,
                }
            }
            LayerContent::Shape(s) => {
                let mut shape = s.clone();
                shape.cache = None;
                shape.cache_canvas = (0, 0);
                let pattern_pixels = match shape.fill.take() {
                    Some(f) => {
                        let (fill, pixels) = self.fill_and_pattern(&f);
                        shape.fill = Some(fill);
                        pixels
                    }
                    None => None,
                };
                Op::Shape {
                    shape: Box::new(shape),
                    pattern_pixels,
                    float,
                }
            }
            LayerContent::Smart(s) => match self.image(&s.source) {
                Op::Empty => Op::Empty,
                source => {
                    let source = self.graph.add(Node::new(source, vec![]));
                    let placed = self.graph.add(Node::new(
                        Op::Transform {
                            matrix: s.transform.coeffs(),
                        },
                        vec![Some(source)],
                    ));
                    return self.smart_filters(layer, placed);
                }
            },
            LayerContent::Group(_) | LayerContent::Adjustment(_) | LayerContent::Filter(_) => Op::Empty,
        };
        let empty = matches!(op, Op::Empty);
        let node = self.graph.add(Node::new(op, vec![]));
        if empty {
            // Filters of nothing are nothing.
            node
        } else {
            self.smart_filters(layer, node)
        }
    }

    /// A layer's running smart filters after `content`, one node each, the
    /// filter mask (which covers the whole stack) on the last.
    fn smart_filters(&mut self, layer: &Layer, content: NodeId) -> NodeId {
        let sf = &layer.smart_filters;
        if !sf.is_active() {
            return content;
        }
        let mask = sf
            .mask
            .as_ref()
            .filter(|m| m.enabled)
            .map(|m| self.filter_mask(m));
        let running: Vec<_> = sf.active().cloned().collect();
        let mut below = content;
        for (i, f) in running.iter().enumerate() {
            let mut inputs = vec![Some(below)];
            if i + 1 == running.len() && mask.is_some() {
                inputs.push(mask);
            }
            let node = Node::new(
                Op::SmartFilter {
                    filter: f.filter.clone(),
                    opacity: f.opacity,
                    blend: f.blend,
                    float: self.float,
                },
                inputs,
            )
            .named(f.filter.name());
            below = self.graph.add(node);
        }
        below
    }

    fn filter_mask(&mut self, m: &Mask) -> NodeId {
        let blob = (!m.tiles.is_empty()).then(|| self.blobs.insert(self.hasher, m.tiles.clone()));
        self.graph.add(Node::new(
            Op::Mask {
                blob,
                default: m.default,
            },
            vec![],
        ))
    }
}
