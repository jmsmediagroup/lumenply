//! The native project format (`.nge`).
//!
//! A plain zip archive:
//!
//! ```text
//! manifest.json                   canvas size, layer tree, layer properties
//! tiles/<layer id>/<x>_<y>.rgba   one tile: 256×256 premultiplied linear f32 RGBA, little-endian
//! masks/<layer id>/<x>_<y>.a      one mask tile: 256×256 f32 coverage, little-endian
//! ```
//!
//! Tiles are deflate-compressed by the zip layer. Only allocated tiles are
//! written, so a mostly empty layer costs almost nothing. The manifest is
//! versioned; readers must reject newer major versions.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use nge_doc::{Adjustment, BlendMode, Document, Filter, Layer, LayerContent, LayerId, Mask, TextLayer};
use nge_tiles::{Rgba, Tile, TileCoord, TileStore, TILE_PIXELS};
use serde::{Deserialize, Serialize};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::IoError;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("not an nge project: {0}")]
    NotAProject(String),
    #[error("project format version {0} is newer than this build supports ({FORMAT_VERSION})")]
    TooNew(u32),
    #[error("corrupt project: {0}")]
    Corrupt(String),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] IoError),
}

impl From<std::io::Error> for ProjectError {
    fn from(e: std::io::Error) -> Self {
        ProjectError::Io(IoError::Io(e))
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    width: u32,
    height: u32,
    next_id: LayerId,
    layers: Vec<LayerRecord>,
}

#[derive(Serialize, Deserialize)]
struct LayerRecord {
    id: LayerId,
    name: String,
    visible: bool,
    opacity: f32,
    blend: BlendMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<MaskRecord>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    collapsed: bool,
    #[serde(flatten)]
    content: ContentRecord,
}

#[derive(Serialize, Deserialize)]
struct MaskRecord {
    default: f32,
    enabled: bool,
    tiles: Vec<(i32, i32)>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ContentRecord {
    Pixel { tiles: Vec<(i32, i32)> },
    Group { children: Vec<LayerRecord> },
    Adjustment { adjustment: Adjustment },
    Filter { filter: Filter },
    Text { text: TextLayer },
}

/// Write a document to `path`, replacing any existing file.
pub fn save(path: impl AsRef<Path>, doc: &Document) -> Result<(), ProjectError> {
    let file = File::create(path)?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let deflate = FileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = FileOptions::default().compression_method(CompressionMethod::Stored);

    let mut layers = Vec::new();
    for l in doc.layers() {
        layers.push(write_layer(&mut zip, l, deflate)?);
    }
    let manifest = Manifest {
        format: "nge".into(),
        version: FORMAT_VERSION,
        width: doc.width,
        height: doc.height,
        next_id: doc.next_id(),
        layers,
    };
    zip.start_file("manifest.json", stored)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    zip.finish()?;
    Ok(())
}

fn write_layer<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    layer: &Layer,
    opts: FileOptions,
) -> Result<LayerRecord, ProjectError> {
    let content = match &layer.content {
        LayerContent::Pixel(store) => {
            let mut tiles = Vec::new();
            for c in sorted(store) {
                let tile = store.tile(c).expect("coord came from the store");
                zip.start_file(format!("tiles/{}/{}_{}.rgba", layer.id, c.x, c.y), opts)?;
                zip.write_all(&tile_bytes(tile))?;
                tiles.push((c.x, c.y));
            }
            ContentRecord::Pixel { tiles }
        }
        LayerContent::Group(children) => {
            let mut out = Vec::new();
            for c in children {
                out.push(write_layer(zip, c, opts)?);
            }
            ContentRecord::Group { children: out }
        }
        LayerContent::Adjustment(adj) => ContentRecord::Adjustment {
            adjustment: adj.clone(),
        },
        LayerContent::Filter(f) => ContentRecord::Filter { filter: f.clone() },
        LayerContent::Text(t) => ContentRecord::Text { text: t.clone() },
    };

    let mask = match &layer.mask {
        Some(m) => {
            let mut tiles = Vec::new();
            for c in sorted(&m.tiles) {
                let tile = m.tiles.tile(c).expect("coord came from the store");
                zip.start_file(format!("masks/{}/{}_{}.a", layer.id, c.x, c.y), opts)?;
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

    Ok(LayerRecord {
        id: layer.id,
        name: layer.name.clone(),
        visible: layer.visible,
        opacity: layer.opacity,
        blend: layer.blend,
        mask,
        collapsed: layer.collapsed,
        content,
    })
}

/// Read a document from `path`.
pub fn load(path: impl AsRef<Path>) -> Result<Document, ProjectError> {
    let file = File::open(path)?;
    let mut zip = ZipArchive::new(BufReader::new(file))?;
    let manifest: Manifest = {
        let mut f = zip
            .by_name("manifest.json")
            .map_err(|_| ProjectError::NotAProject("missing manifest.json".into()))?;
        let mut buf = String::new();
        f.read_to_string(&mut buf)?;
        serde_json::from_str(&buf)?
    };
    if manifest.format != "nge" {
        return Err(ProjectError::NotAProject(format!(
            "format field is '{}'",
            manifest.format
        )));
    }
    if manifest.version > FORMAT_VERSION {
        return Err(ProjectError::TooNew(manifest.version));
    }
    let mut layers = Vec::new();
    for r in &manifest.layers {
        layers.push(read_layer(&mut zip, r)?);
    }
    let mut max_id = 0;
    let doc = Document::from_parts(manifest.width, manifest.height, layers, manifest.next_id);
    doc.for_each_layer(|l| max_id = max_id.max(l.id));
    if manifest.next_id <= max_id {
        return Err(ProjectError::Corrupt(format!(
            "next_id {} is not above the highest layer id {max_id}",
            manifest.next_id
        )));
    }
    Ok(doc)
}

fn read_layer<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    r: &LayerRecord,
) -> Result<Layer, ProjectError> {
    let content = match &r.content {
        ContentRecord::Pixel { tiles } => {
            let mut store = TileStore::new();
            for &(x, y) in tiles {
                let bytes = read_entry(zip, &format!("tiles/{}/{}_{}.rgba", r.id, x, y))?;
                store.insert(TileCoord::new(x, y), Arc::new(tile_from_bytes(&bytes)?));
            }
            LayerContent::Pixel(store)
        }
        ContentRecord::Group { children } => {
            let mut out = Vec::new();
            for c in children {
                out.push(read_layer(zip, c)?);
            }
            LayerContent::Group(out)
        }
        ContentRecord::Adjustment { adjustment } => LayerContent::Adjustment(adjustment.clone()),
        ContentRecord::Filter { filter } => LayerContent::Filter(filter.clone()),
        ContentRecord::Text { text } => {
            let mut t = text.clone();
            nge_render::text::refresh_cache(&mut t);
            LayerContent::Text(t)
        }
    };

    let mask = match &r.mask {
        Some(m) => {
            let mut tiles = TileStore::new();
            for &(x, y) in &m.tiles {
                let bytes = read_entry(zip, &format!("masks/{}/{}_{}.a", r.id, x, y))?;
                tiles.insert(TileCoord::new(x, y), Arc::new(mask_from_bytes(&bytes)?));
            }
            Some(Mask {
                tiles,
                default: m.default,
                enabled: m.enabled,
            })
        }
        None => None,
    };

    let mut layer = Layer::with_content(r.id, r.name.clone(), content);
    layer.visible = r.visible;
    layer.opacity = r.opacity;
    layer.blend = r.blend;
    layer.mask = mask;
    layer.collapsed = r.collapsed;
    Ok(layer)
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>, ProjectError> {
    let mut f = zip
        .by_name(name)
        .map_err(|_| ProjectError::Corrupt(format!("manifest references missing entry {name}")))?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn sorted(store: &TileStore) -> Vec<TileCoord> {
    let mut v: Vec<TileCoord> = store.coords().collect();
    v.sort();
    v
}

fn tile_bytes(tile: &Tile) -> Vec<u8> {
    let mut out = Vec::with_capacity(TILE_PIXELS * 16);
    for p in tile.pixels().iter() {
        for c in [p.r, p.g, p.b, p.a] {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    out
}

fn tile_from_bytes(bytes: &[u8]) -> Result<Tile, ProjectError> {
    if bytes.len() != TILE_PIXELS * 16 {
        return Err(ProjectError::Corrupt(format!(
            "tile is {} bytes, expected {}",
            bytes.len(),
            TILE_PIXELS * 16
        )));
    }
    let mut tile = Tile::new();
    for (p, chunk) in tile.pixels_mut().iter_mut().zip(bytes.chunks_exact(16)) {
        let f = |i: usize| f32::from_le_bytes([chunk[i], chunk[i + 1], chunk[i + 2], chunk[i + 3]]);
        *p = Rgba::new(f(0), f(4), f(8), f(12));
    }
    Ok(tile)
}

fn mask_bytes(tile: &Tile) -> Vec<u8> {
    let mut out = Vec::with_capacity(TILE_PIXELS * 4);
    for p in tile.pixels().iter() {
        out.extend_from_slice(&p.a.to_le_bytes());
    }
    out
}

fn mask_from_bytes(bytes: &[u8]) -> Result<Tile, ProjectError> {
    if bytes.len() != TILE_PIXELS * 4 {
        return Err(ProjectError::Corrupt(format!(
            "mask tile is {} bytes, expected {}",
            bytes.len(),
            TILE_PIXELS * 4
        )));
    }
    let mut tile = Tile::new();
    for (p, chunk) in tile.pixels_mut().iter_mut().zip(bytes.chunks_exact(4)) {
        let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        *p = Rgba::new(v, v, v, v);
    }
    Ok(tile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nge_tiles::Raster;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("nge-project-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn round_trip_preserves_everything() {
        let mut doc = Document::new(700, 300);
        let bg = doc.add_pixel_layer("Background");
        let fill = Raster::filled(700, 300, Rgba::from_straight(0.1, 0.2, 0.3, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);

        let g = doc.add_group("Effects");
        let adj_id = doc.alloc_id();
        let mut adj = Layer::adjustment(
            adj_id,
            Adjustment::HueSaturation {
                hue: 30.0,
                saturation: 0.2,
                lightness: -0.1,
            },
        );
        adj.opacity = 0.7;
        adj.blend = BlendMode::SoftLight;
        let mut mask = Mask::reveal_all();
        mask.set_value(600, 100, 0.5);
        adj.mask = Some(mask);
        let grp = doc.layer_mut(g).unwrap();
        grp.visible = false;
        grp.children_mut().unwrap().push(adj);
        doc.add_filter(Filter::GaussianBlur { radius: 3.5 });
        let tid = doc.alloc_id();
        let mut tl = TextLayer::new("Round trip", 10.0, 60.0, 32.0, [0.0, 0.0, 1.0, 1.0]);
        nge_render::text::refresh_cache(&mut tl);
        doc.add_layer(Layer::text(tid, tl));

        let path = temp("rt.nge");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();

        assert_eq!((back.width, back.height), (700, 300));
        assert_eq!(back.next_id(), doc.next_id());
        assert_eq!(back.layer_count(), 5);
        assert!(
            matches!(back.layers()[2].content, LayerContent::Filter(Filter::GaussianBlur { radius }) if radius == 3.5)
        );
        let t = back.layer(tid).unwrap().text_layer().unwrap();
        assert_eq!(t.text, "Round trip");
        assert!(
            t.cache.as_ref().is_some_and(|c| !c.is_empty()),
            "text is re-rasterised on load"
        );
        assert_eq!(back.layer(bg).unwrap().pixels().unwrap().len(), 6);
        assert_eq!(
            back.layer(bg).unwrap().pixels().unwrap().get_pixel(699, 299),
            Rgba::from_straight(0.1, 0.2, 0.3, 1.0)
        );
        let g2 = back.layer(g).unwrap();
        assert!(!g2.visible);
        let a2 = back.layer(adj_id).unwrap();
        assert_eq!(a2.opacity, 0.7);
        assert_eq!(a2.blend, BlendMode::SoftLight);
        match &a2.content {
            LayerContent::Adjustment(Adjustment::HueSaturation { hue, .. }) => assert_eq!(*hue, 30.0),
            other => panic!("wrong content: {other:?}"),
        }
        let m = a2.mask.as_ref().unwrap();
        assert_eq!(m.value(600, 100), 0.5);
        assert_eq!(m.value(601, 100), 1.0);
        assert_eq!(m.value(0, 0), 1.0);

        // The rendered result is identical.
        let before = nge_render::composite_raster(&doc);
        let after = nge_render::composite_raster(&back);
        assert_eq!(before, after);
    }

    #[test]
    fn rejects_garbage_and_newer_versions() {
        let path = temp("garbage.nge");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(matches!(load(&path), Err(ProjectError::Zip(_))));

        let path = temp("future.nge");
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("manifest.json", FileOptions::default()).unwrap();
        zip.write_all(br#"{"format":"nge","version":99,"width":1,"height":1,"next_id":1,"layers":[]}"#)
            .unwrap();
        zip.finish().unwrap();
        assert!(matches!(load(&path), Err(ProjectError::TooNew(99))));
    }
}
