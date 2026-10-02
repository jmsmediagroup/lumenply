//! A layer's smart filters in the `.lumen` manifest (ADR 0011): the filter
//! list and switch as JSON, the filter mask's tiles under
//! `sfmasks/<layer id>/<x>_<y>.a` (the layer-mask tile format). The
//! filtered pixels are derived and re-render on load.

use std::io::{Read, Seek, Write};
use std::sync::Arc;

use lumenply_doc::{Layer, LayerId, Mask, SmartFilter, SmartFilters};
use lumenply_tiles::TileStore;
use serde::{Deserialize, Serialize};
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

use super::{checked_coord, mask_bytes, mask_from_bytes, read_entry, sorted, MaskRecord, ProjectError};

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

#[derive(Serialize, Deserialize)]
pub(super) struct SmartFiltersRecord {
    #[serde(default)]
    filters: Vec<SmartFilter>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<MaskRecord>,
}

impl Default for SmartFiltersRecord {
    fn default() -> Self {
        SmartFiltersRecord {
            filters: Vec::new(),
            enabled: true,
            mask: None,
        }
    }
}

impl SmartFiltersRecord {
    pub(super) fn is_empty(&self) -> bool {
        self.filters.is_empty() && self.mask.is_none()
    }
}

pub(super) fn write<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    layer: &Layer,
    opts: FileOptions,
) -> Result<SmartFiltersRecord, ProjectError> {
    let sf = &layer.smart_filters;
    let mask = match &sf.mask {
        Some(m) => {
            let mut tiles = Vec::new();
            for c in sorted(&m.tiles) {
                let tile = m.tiles.tile(c).expect("coord came from the store");
                zip.start_file(format!("sfmasks/{}/{}_{}.a", layer.id, c.x, c.y), opts)?;
                zip.write_all(&mask_bytes(tile))?;
                tiles.push((c.x, c.y));
            }
            Some(MaskRecord {
                default: m.default,
                enabled: m.enabled,
                tiles,
            })
        }
        None => None,
    };
    Ok(SmartFiltersRecord {
        filters: sf.filters.clone(),
        enabled: sf.enabled,
        mask,
    })
}

pub(super) fn read<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    id: LayerId,
    r: &SmartFiltersRecord,
) -> Result<SmartFilters, ProjectError> {
    let mask = match &r.mask {
        Some(m) => {
            let mut tiles = TileStore::new();
            for &(x, y) in &m.tiles {
                let c = checked_coord(x, y)?;
                let bytes = read_entry(zip, &format!("sfmasks/{id}/{x}_{y}.a"))?;
                tiles.insert(c, Arc::new(mask_from_bytes(&bytes)?));
            }
            Some(Mask {
                tiles,
                default: m.default.clamp(0.0, 1.0),
                enabled: m.enabled,
            })
        }
        None => None,
    };
    let filters = r
        .filters
        .iter()
        .map(|f| SmartFilter {
            opacity: if f.opacity.is_finite() {
                f.opacity.clamp(0.0, 1.0)
            } else {
                1.0
            },
            ..f.clone()
        })
        .collect();
    Ok(SmartFilters {
        filters,
        enabled: r.enabled,
        mask,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use lumenply_doc::{BlendMode, Document, Filter, Mask, SmartFilter};
    use lumenply_tiles::{Raster, Rgba, TileStore};

    #[test]
    fn smart_filters_round_trip_and_re_render() {
        let mut doc = Document::new(64, 32);
        let id = doc.add_pixel_layer("Step");
        let mut r = Raster::new(64, 32);
        for y in 0..32 {
            for x in 0..32 {
                r.set(x, y, Rgba::new(1.0, 1.0, 1.0, 1.0));
            }
        }
        {
            let l = doc.layer_mut(id).unwrap();
            *l.pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
            l.smart_filters
                .filters
                .push(SmartFilter::new(Filter::BoxBlur { radius: 2.0 }));
            l.smart_filters.filters.push(SmartFilter {
                enabled: false,
                opacity: 0.25,
                blend: BlendMode::Screen,
                ..SmartFilter::new(Filter::Noise { amount: 0.3 })
            });
            let mut m = Mask::reveal_all();
            m.set_value(32, 3, 0.0);
            l.smart_filters.mask = Some(m);
        }
        let dir = std::env::temp_dir().join(format!("lumenply-sf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sf.lumen");
        crate::project::save(&path, &doc).unwrap();
        let back = crate::project::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let l = back.layer(id).unwrap();
        assert_eq!(l.smart_filters, doc.layer(id).unwrap().smart_filters);
        assert_eq!(l.smart_filters.filters[1].opacity, 0.25);
        assert_eq!(l.smart_filters.filters[1].blend, BlendMode::Screen);
        let m = l.smart_filters.mask.as_ref().unwrap();
        assert_eq!((m.value(32, 3), m.value(32, 4)), (0.0, 1.0));
        // The cache re-renders on load: two white pixels of five at x = 32,
        // except where the mask hides the filter.
        let shown = l.raster_store().unwrap();
        assert!((shown.get_pixel(32, 10).r - 0.4).abs() < 1e-4);
        assert_eq!(shown.get_pixel(32, 3).r, 0.0);
        assert_eq!(l.content_store().unwrap().get_pixel(32, 10).r, 0.0);
    }

    #[test]
    fn files_without_smart_filters_load_with_none() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("Plain");
        let dir = std::env::temp_dir().join(format!("lumenply-sf0-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain.lumen");
        crate::project::save(&path, &doc).unwrap();
        // No smart_filters key is written for a layer without them.
        let f = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let mut json = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("manifest.json").unwrap(), &mut json).unwrap();
        assert!(!json.contains("smart_filters"), "{json}");
        let back = crate::project::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(back.layers()[0].smart_filters.is_empty());
        assert!(back.layers()[0].smart_filters.enabled);
    }
}
