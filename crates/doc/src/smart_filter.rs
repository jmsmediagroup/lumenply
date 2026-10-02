//! Smart filters: non-destructive filters attached to one layer (see ADR
//! 0011).
//!
//! A layer's [`SmartFilters`] run on that layer's own pixels — before its
//! mask, effects and blend with the layers below — in list order, index 0
//! first (Photoshop lists them the other way up: the newest on top). Each
//! filter mixes into the image so far by its own opacity and blend mode, and
//! an optional filter mask fades the whole stack back to the unfiltered
//! pixels, as Photoshop's single "Smart Filters" mask does.
//!
//! The filtered pixels are derived state: `cache` is rebuilt by
//! `lumenply-render` whenever the layer's pixels, the filters, the mask or the
//! canvas change, and it is never saved.

use lumenply_tiles::{TileCoord, TileStore};
use serde::{Deserialize, Serialize};

use crate::{BlendMode, Filter, Mask};

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

/// One filter in a layer's smart-filter stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmartFilter {
    pub filter: Filter,
    /// The eye beside the filter in the Layers panel.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Blending options: how strongly the filtered result replaces the
    /// image it was computed from, 0..=1.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Blending options: the filtered result blends onto its own input in
    /// this mode before the opacity mix.
    #[serde(default)]
    pub blend: BlendMode,
}

impl SmartFilter {
    pub fn new(filter: Filter) -> Self {
        SmartFilter {
            filter,
            enabled: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }
}

/// Tile coordinates with the address of the tile stored there.
pub type TileIds = Vec<(TileCoord, usize)>;

/// What a filtered cache was rendered from. Source tiles are compared by
/// allocation identity: the editor always changes a document's tiles
/// copy-on-write while the previous document (holding the old tiles) is
/// still alive, so a changed tile can never reuse the old address.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmartFilterKey {
    pub source: TileIds,
    pub filters: Vec<SmartFilter>,
    /// The mask's default, enabled flag and tile identities.
    pub mask: Option<(f32, bool, TileIds)>,
    pub canvas: (u32, u32),
    pub float: bool,
}

/// The rendered (filtered) pixels of a layer and what they came from.
#[derive(Clone, Debug, Default)]
pub struct SmartFilterCache {
    pub store: TileStore,
    pub key: SmartFilterKey,
}

/// A layer's smart filters. Empty on every layer by default.
#[derive(Clone, Debug)]
pub struct SmartFilters {
    /// Application order: `filters[0]` runs first on the layer's pixels.
    pub filters: Vec<SmartFilter>,
    /// The eye on the "Smart Filters" row: off shows the unfiltered layer.
    pub enabled: bool,
    /// Photoshop's filter mask: where it is black the layer shows its
    /// unfiltered pixels. Covers the whole stack.
    pub mask: Option<Mask>,
    /// Derived: the filtered pixels, never saved.
    pub cache: Option<SmartFilterCache>,
}

impl Default for SmartFilters {
    fn default() -> Self {
        SmartFilters {
            filters: Vec::new(),
            enabled: true,
            mask: None,
            cache: None,
        }
    }
}

impl PartialEq for SmartFilters {
    /// Compares the editable state; the cache is derived.
    fn eq(&self, o: &Self) -> bool {
        self.filters == o.filters
            && self.enabled == o.enabled
            && match (&self.mask, &o.mask) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.default == b.default && a.enabled == b.enabled && a.tiles.len() == b.tiles.len()
                }
                _ => false,
            }
    }
}

impl SmartFilters {
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// True when the stack changes the layer's pixels at all: the master
    /// switch is on and at least one filter is enabled with some opacity.
    pub fn is_active(&self) -> bool {
        self.enabled && self.filters.iter().any(|f| f.enabled && f.opacity > 0.0)
    }

    /// The filters that run, in order.
    pub fn active(&self) -> impl Iterator<Item = &SmartFilter> {
        self.filters
            .iter()
            .filter(move |f| self.enabled && f.enabled && f.opacity > 0.0)
    }

    /// How far (px) the filtered layer reads around any output pixel: the
    /// running filters' reaches added up, since each reads the one below.
    pub fn pad(&self) -> i32 {
        self.active().map(|f| f.filter.pad()).sum()
    }

    /// The filtered pixels, when they are current enough to show.
    pub fn filtered(&self) -> Option<&TileStore> {
        if self.is_active() {
            self.cache.as_ref().map(|c| &c.store)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_and_pad_follow_the_switches() {
        let mut sf = SmartFilters::default();
        assert!(sf.enabled && !sf.is_active() && sf.pad() == 0);
        sf.filters
            .push(SmartFilter::new(Filter::GaussianBlur { radius: 6.0 }));
        sf.filters.push(SmartFilter::new(Filter::BoxBlur { radius: 4.0 }));
        // Gaussian 6: box radius round(6/√3) = 3, three passes = 9; box 4.
        assert_eq!(sf.pad(), 13);
        assert!(sf.is_active());
        sf.filters[0].enabled = false;
        assert_eq!(sf.pad(), 4);
        sf.filters[1].opacity = 0.0;
        assert!(!sf.is_active());
        assert_eq!(sf.pad(), 0);
        sf.filters[1].opacity = 0.5;
        sf.enabled = false;
        assert!(!sf.is_active());
    }

    #[test]
    fn smart_filter_json_defaults_fill_missing_fields() {
        let f: SmartFilter =
            serde_json::from_str(r#"{"filter":{"type":"gaussian-blur","radius":2.0}}"#).unwrap();
        assert_eq!(f, SmartFilter::new(Filter::GaussianBlur { radius: 2.0 }));
        let json = serde_json::to_string(&SmartFilter {
            opacity: 0.5,
            blend: BlendMode::Multiply,
            ..SmartFilter::new(Filter::FindEdges)
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"filter":{"type":"find-edges"},"enabled":true,"opacity":0.5,"blend":"multiply"}"#
        );
    }
}
