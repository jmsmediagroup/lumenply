//! The native project format (`.lumen`; the legacy `.nge` extension and
//! format id from the working title still load).
//!
//! A plain zip archive:
//!
//! ```text
//! manifest.json                   canvas size, layer tree, layer properties
//! tiles/<layer id>/<x>_<y>.rgba   one tile: 256×256 premultiplied linear f32 RGBA, little-endian
//! masks/<layer id>/<x>_<y>.a      one mask tile: 256×256 f32 coverage, little-endian
//! channels/<n>/<x>_<y>.a          one tile of saved selection n, as mask tiles
//! ```
//!
//! Tiles are deflate-compressed by the zip layer. Only allocated tiles are
//! written, so a mostly empty layer costs almost nothing. The manifest is
//! versioned; readers must reject newer major versions.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use lumenply_doc::{Adjustment, BlendMode, Document, Filter, Layer, LayerContent, LayerId, Mask, TextLayer};
use lumenply_tiles::{Rgba, Tile, TileCoord, TileStore, TILE_PIXELS};
use serde::{Deserialize, Serialize};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::IoError;

mod smart_filters;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_path: Option<lumenply_doc::VectorPath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    saved_paths: Vec<lumenply_doc::NamedPath>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    float_mode: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    guides: Vec<lumenply_doc::Guide>,
    /// Saved selections (Select ▸ Save Selection), tiles under channels/.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    channels: Vec<ChannelRecord>,
    layers: Vec<LayerRecord>,
}

#[derive(Serialize, Deserialize)]
struct ChannelRecord {
    name: String,
    default: f32,
    tiles: Vec<(i32, i32)>,
}

#[derive(Serialize, Deserialize)]
struct LayerRecord {
    id: LayerId,
    name: String,
    visible: bool,
    opacity: f32,
    blend: BlendMode,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pass_through: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    clip: bool,
    #[serde(default, skip_serializing_if = "lumenply_doc::LayerEffects::is_empty")]
    effects: lumenply_doc::LayerEffects,
    /// Photoshop's Fill; missing in older files: 100%.
    #[serde(default = "full_fill", skip_serializing_if = "is_full_fill")]
    fill_opacity: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<MaskRecord>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    collapsed: bool,
    /// Missing in files from before layer locks: unlocked.
    #[serde(default, skip_serializing_if = "lumenply_doc::LayerLocks::is_empty")]
    locks: lumenply_doc::LayerLocks,
    /// Smart filters on the layer's own pixels (ADR 0011); missing in
    /// older files: none.
    #[serde(default, skip_serializing_if = "smart_filters::SmartFiltersRecord::is_empty")]
    smart_filters: smart_filters::SmartFiltersRecord,
    #[serde(flatten)]
    content: ContentRecord,
}

fn full_fill() -> f32 {
    1.0
}

fn is_full_fill(v: &f32) -> bool {
    *v >= 1.0
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
    Pixel {
        tiles: Vec<(i32, i32)>,
    },
    Group {
        children: Vec<LayerRecord>,
    },
    Adjustment {
        adjustment: Adjustment,
    },
    Filter {
        filter: Filter,
    },
    Text {
        text: TextLayer,
    },
    /// Smart object: the source tiles plus the cumulative transform as
    /// its six coefficients; the rendered cache is derived, never saved.
    Smart {
        tiles: Vec<(i32, i32)>,
        transform: [f32; 6],
    },
    /// Fill layer: only the settings; the pixels re-render on load.
    Fill {
        fill: lumenply_doc::Fill,
    },
    /// Shape layer: the vector outline, transform, fill and stroke; the
    /// pixels re-render on load.
    Shape {
        shape: lumenply_doc::ShapeLayer,
    },
}

/// Write a document to `path`, replacing any existing file.
///
/// The archive is written to a sibling temporary file and renamed into
/// place once it is complete and flushed, so a failure partway through
/// (disk full, say) never destroys an existing project.
pub fn save(path: impl AsRef<Path>, doc: &Document) -> Result<(), ProjectError> {
    let path = path.as_ref();
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp_name);
    let result = write_archive(&tmp, doc).and_then(|()| Ok(std::fs::rename(&tmp, path)?));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_archive(path: &Path, doc: &Document) -> Result<(), ProjectError> {
    let file = File::create(path)?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let deflate = FileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = FileOptions::default().compression_method(CompressionMethod::Stored);

    let mut layers = Vec::new();
    for l in doc.layers() {
        layers.push(write_layer(&mut zip, l, deflate)?);
    }
    let mut channels = Vec::new();
    for (n, ch) in doc.saved_selections.iter().enumerate() {
        let mut tiles = Vec::new();
        for c in sorted(&ch.mask.tiles) {
            let tile = ch.mask.tiles.tile(c).expect("coord came from the store");
            zip.start_file(format!("channels/{n}/{}_{}.a", c.x, c.y), deflate)?;
            zip.write_all(&mask_bytes(tile))?;
            tiles.push((c.x, c.y));
        }
        channels.push(ChannelRecord {
            name: ch.name.clone(),
            default: ch.mask.default,
            tiles,
        });
    }
    let manifest = Manifest {
        format: "lumenply".into(),
        version: FORMAT_VERSION,
        width: doc.width,
        height: doc.height,
        next_id: doc.next_id(),
        work_path: doc.work_path.clone(),
        saved_paths: doc.saved_paths.clone(),
        float_mode: doc.float_mode,
        guides: doc.guides.clone(),
        channels,
        layers,
    };
    zip.start_file("manifest.json", stored)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    let mut writer = zip.finish()?;
    writer.flush()?;
    writer.into_inner().map_err(|e| e.into_error())?.sync_all()?;
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
        LayerContent::Smart(s) => {
            let mut tiles = Vec::new();
            for c in sorted(&s.source) {
                let tile = s.source.tile(c).expect("coord came from the store");
                zip.start_file(format!("tiles/{}/{}_{}.rgba", layer.id, c.x, c.y), opts)?;
                zip.write_all(&tile_bytes(tile))?;
                tiles.push((c.x, c.y));
            }
            ContentRecord::Smart {
                tiles,
                transform: s.transform.coeffs(),
            }
        }
        LayerContent::Fill(f) => ContentRecord::Fill { fill: f.fill.clone() },
        LayerContent::Shape(sh) => {
            let mut shape = sh.clone();
            shape.cache = None;
            ContentRecord::Shape { shape }
        }
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
        pass_through: layer.pass_through,
        clip: layer.clip,
        effects: layer.effects.clone(),
        fill_opacity: layer.fill_opacity,
        mask,
        collapsed: layer.collapsed,
        locks: layer.locks,
        smart_filters: smart_filters::write(zip, layer, opts)?,
        content,
    })
}

/// Largest canvas side a manifest may declare.
const MAX_CANVAS: u32 = 1_000_000;
/// Largest manifest the loader will read (decompressed).
const MAX_MANIFEST: u64 = 64 << 20;
/// Tile coordinates must survive `x * 256` pixel arithmetic.
const MAX_TILE_COORD: i32 = i32::MAX / 256;

/// Read a document from `path`.
pub fn load(path: impl AsRef<Path>) -> Result<Document, ProjectError> {
    let file = File::open(path)?;
    let mut zip = ZipArchive::new(BufReader::new(file))?;
    let manifest: Manifest = {
        let f = zip
            .by_name("manifest.json")
            .map_err(|_| ProjectError::NotAProject("missing manifest.json".into()))?;
        let mut buf = String::new();
        // The declared size in the zip is attacker-controlled; cap what we
        // actually decompress.
        f.take(MAX_MANIFEST + 1).read_to_string(&mut buf)?;
        if buf.len() as u64 > MAX_MANIFEST {
            return Err(ProjectError::Corrupt("manifest is implausibly large".into()));
        }
        serde_json::from_str(&buf)?
    };
    if manifest.format != "lumenply" && manifest.format != "nge" {
        return Err(ProjectError::NotAProject(format!(
            "format field is '{}'",
            manifest.format
        )));
    }
    if manifest.version > FORMAT_VERSION {
        return Err(ProjectError::TooNew(manifest.version));
    }
    if manifest.width == 0
        || manifest.height == 0
        || manifest.width > MAX_CANVAS
        || manifest.height > MAX_CANVAS
    {
        return Err(ProjectError::Corrupt(format!(
            "canvas {}×{} is not a sane size",
            manifest.width, manifest.height
        )));
    }
    let mut layers = Vec::new();
    for r in &manifest.layers {
        layers.push(read_layer(&mut zip, r)?);
    }
    let mut max_id = 0;
    let mut seen = std::collections::HashSet::new();
    let mut dup = None;
    let mut doc = Document::from_parts(manifest.width, manifest.height, layers, manifest.next_id);
    doc.work_path = manifest.work_path.clone().filter(|p| !p.subpaths.is_empty());
    doc.saved_paths = manifest
        .saved_paths
        .iter()
        .filter(|n| !n.path.subpaths.is_empty())
        .cloned()
        .collect();
    doc.float_mode = manifest.float_mode;
    doc.guides = manifest
        .guides
        .iter()
        .filter(|g| g.pos.is_finite())
        .copied()
        .collect();
    for (n, ch) in manifest.channels.iter().enumerate() {
        let mut tiles = TileStore::new();
        for &(x, y) in &ch.tiles {
            let c = checked_coord(x, y)?;
            let bytes = read_entry(&mut zip, &format!("channels/{n}/{x}_{y}.a"))?;
            tiles.insert(c, Arc::new(mask_from_bytes(&bytes)?));
        }
        doc.saved_selections.push(lumenply_doc::SavedSelection {
            name: ch.name.clone(),
            mask: Mask {
                tiles,
                default: ch.default.clamp(0.0, 1.0),
                enabled: true,
            },
        });
    }
    doc.for_each_layer(|l| {
        max_id = max_id.max(l.id);
        if !seen.insert(l.id) {
            dup = Some(l.id);
        }
    });
    if let Some(id) = dup {
        return Err(ProjectError::Corrupt(format!("layer id {id} appears twice")));
    }
    if manifest.next_id <= max_id {
        return Err(ProjectError::Corrupt(format!(
            "next_id {} is not above the highest layer id {max_id}",
            manifest.next_id
        )));
    }
    lumenply_render::fill::refresh_stale(&mut doc);
    Ok(doc)
}

fn checked_coord(x: i32, y: i32) -> Result<TileCoord, ProjectError> {
    if x.abs() > MAX_TILE_COORD || y.abs() > MAX_TILE_COORD {
        return Err(ProjectError::Corrupt(format!(
            "tile coordinate ({x}, {y}) is out of range"
        )));
    }
    Ok(TileCoord::new(x, y))
}

fn read_layer<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    r: &LayerRecord,
) -> Result<Layer, ProjectError> {
    let content = match &r.content {
        ContentRecord::Pixel { tiles } => {
            let mut store = TileStore::new();
            for &(x, y) in tiles {
                let c = checked_coord(x, y)?;
                let bytes = read_entry(zip, &format!("tiles/{}/{}_{}.rgba", r.id, x, y))?;
                store.insert(c, Arc::new(tile_from_bytes(&bytes)?));
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
            if !(0.0..=10_000.0).contains(&t.size) {
                return Err(ProjectError::Corrupt(format!("text size {} is not sane", t.size)));
            }
            lumenply_render::text::refresh_cache(&mut t);
            LayerContent::Text(t)
        }
        ContentRecord::Smart { tiles, transform } => {
            let mut source = TileStore::new();
            for &(x, y) in tiles {
                let c = checked_coord(x, y)?;
                let bytes = read_entry(zip, &format!("tiles/{}/{}_{}.rgba", r.id, x, y))?;
                source.insert(c, Arc::new(tile_from_bytes(&bytes)?));
            }
            if !transform.iter().all(|v| v.is_finite()) {
                return Err(ProjectError::Corrupt("smart transform is not finite".into()));
            }
            let transform = lumenply_tiles::Affine::from_coeffs(*transform);
            let cache = lumenply_render::transform_store(&source, &transform);
            LayerContent::Smart(lumenply_doc::SmartLayer {
                source,
                transform,
                cache: Some(cache),
            })
        }
        // The cache renders once the canvas size is known (end of `load`).
        ContentRecord::Fill { fill } => LayerContent::Fill(lumenply_doc::FillLayer::new(fill.clone())),
        // Also rendered at the end of `load`.
        ContentRecord::Shape { shape } => {
            if !shape.geometry.is_finite() || !shape.transform.coeffs().iter().all(|v| v.is_finite()) {
                return Err(ProjectError::Corrupt("shape numbers are not finite".into()));
            }
            let mut shape = shape.clone();
            shape.cache = None;
            LayerContent::Shape(shape)
        }
    };

    let mask = match &r.mask {
        Some(m) => {
            let mut tiles = TileStore::new();
            for &(x, y) in &m.tiles {
                let c = checked_coord(x, y)?;
                let bytes = read_entry(zip, &format!("masks/{}/{}_{}.a", r.id, x, y))?;
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

    let mut layer = Layer::with_content(r.id, r.name.clone(), content);
    layer.visible = r.visible;
    layer.opacity = r.opacity.clamp(0.0, 1.0);
    layer.blend = r.blend;
    layer.pass_through = r.pass_through;
    layer.clip = r.clip;
    layer.effects = r.effects.clone();
    layer.fill_opacity = if r.fill_opacity.is_finite() {
        r.fill_opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    layer.mask = mask;
    layer.collapsed = r.collapsed;
    layer.locks = r.locks;
    layer.smart_filters = smart_filters::read(zip, r.id, &r.smart_filters)?;
    Ok(layer)
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>, ProjectError> {
    // The largest legitimate entry is a pixel tile. The declared size and
    // the decompressed stream are both attacker-controlled, so cap the
    // allocation and the read rather than trusting either.
    const MAX_ENTRY: usize = TILE_PIXELS * 16;
    let f = zip
        .by_name(name)
        .map_err(|_| ProjectError::Corrupt(format!("manifest references missing entry {name}")))?;
    let mut buf = Vec::with_capacity((f.size() as usize).min(MAX_ENTRY));
    f.take(MAX_ENTRY as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > MAX_ENTRY {
        return Err(ProjectError::Corrupt(format!(
            "entry {name} is implausibly large"
        )));
    }
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
    use lumenply_tiles::Raster;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("nge-project-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// Build a .nge zip by hand with the given manifest and extra entries.
    fn craft_project(name: &str, manifest: &str, entries: &[(&str, Vec<u8>)]) -> std::path::PathBuf {
        let path = temp(name);
        let mut zip = ZipWriter::new(BufWriter::new(File::create(&path).unwrap()));
        let opts: FileOptions = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("manifest.json", opts).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        for (ename, data) in entries {
            zip.start_file(*ename, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn corrupt_manifests_error_instead_of_panicking() {
        // Duplicate layer ids: every id-addressed command would resolve to
        // the first layer only.
        let dup = r#"{"format":"nge","version":1,"width":10,"height":10,"next_id":5,
            "layers":[{"id":1,"name":"a","visible":true,"opacity":1.0,"blend":"normal",
                       "kind":"pixel","tiles":[]},
                      {"id":1,"name":"b","visible":true,"opacity":1.0,"blend":"normal",
                       "kind":"pixel","tiles":[]}]}"#;
        assert!(load(craft_project("dup-id.nge", dup, &[])).is_err());

        // A tile coordinate that would overflow pixel arithmetic (x * 256).
        let far = format!(
            r#"{{"format":"nge","version":1,"width":10,"height":10,"next_id":2,
            "layers":[{{"id":1,"name":"a","visible":true,"opacity":1.0,"blend":"normal",
                       "kind":"pixel","tiles":[[{},0]]}}]}}"#,
            i32::MAX
        );
        let entry_name = format!("tiles/1/{}_0.rgba", i32::MAX);
        let entries = [(entry_name.as_str(), vec![0u8; TILE_PIXELS * 16])];
        assert!(load(craft_project("far-tile.nge", &far, &entries)).is_err());

        // An absurd canvas size.
        let huge = r#"{"format":"nge","version":1,"width":4000000000,"height":10,
            "next_id":1,"layers":[]}"#;
        assert!(load(craft_project("huge-canvas.nge", huge, &[])).is_err());
    }

    #[test]
    fn failed_save_leaves_the_existing_project_intact() {
        let path = temp("atomic.nge");
        let mut doc = Document::new(50, 40);
        doc.add_pixel_layer("keep me");
        save(&path, &doc).unwrap();

        // A save that cannot complete (the temp file cannot be created
        // because the target sits in a directory that no longer exists)
        // must error without touching the existing file.
        let gone = temp("no-such-dir").join("x.nge");
        assert!(save(&gone, &doc).is_err());

        // Overwriting works, is loadable afterwards, and leaves no temp file.
        let mut doc2 = Document::new(60, 60);
        doc2.add_pixel_layer("second");
        save(&path, &doc2).unwrap();
        let back = load(&path).unwrap();
        assert_eq!((back.width, back.height), (60, 60));
        let mut tmp_name = path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        assert!(
            !std::path::PathBuf::from(tmp_name).exists(),
            "temp file cleaned up"
        );
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
                colorize: false,
            },
        );
        adj.opacity = 0.7;
        adj.blend = BlendMode::SoftLight;
        let mut mask = Mask::reveal_all();
        mask.set_value(600, 100, 0.5);
        adj.mask = Some(mask);
        let grp = doc.layer_mut(g).unwrap();
        grp.visible = false;
        grp.pass_through = true;
        grp.children_mut().unwrap().push(adj);
        grp.children_mut().unwrap()[0].clip = true;
        doc.add_filter(Filter::GaussianBlur { radius: 3.5 });
        let tid = doc.alloc_id();
        let mut tl = TextLayer::new("Round trip", 10.0, 60.0, 32.0, [0.0, 0.0, 1.0, 1.0]);
        lumenply_render::text::refresh_cache(&mut tl);
        doc.add_layer(Layer::text(tid, tl));

        doc.layer_mut(bg).unwrap().effects.stroke = Some(lumenply_doc::StrokeFx::default());
        doc.work_path = Some(lumenply_doc::VectorPath {
            subpaths: vec![lumenply_doc::SubPath {
                closed: true,
                nodes: vec![
                    lumenply_doc::PathNode::corner(1.0, 2.0),
                    lumenply_doc::PathNode::corner(50.0, 2.0),
                    lumenply_doc::PathNode::corner(25.0, 40.0),
                ],
            }],
        });
        doc.saved_paths = vec![lumenply_doc::NamedPath {
            name: "Outline".into(),
            path: doc.work_path.clone().unwrap(),
        }];
        // A smart object: source tiles and the transform persist; the
        // cache rebuilds on load.
        let sid = doc.alloc_id();
        let mut src = TileStore::new();
        for y in 0..8 {
            for x in 0..8 {
                src.set_pixel(x, y, lumenply_tiles::Rgba::from_straight(0.2, 0.9, 0.4, 1.0));
            }
        }
        let st = lumenply_tiles::Affine::around(4.0, 4.0, 2.0, 2.0, 0.0);
        doc.add_layer(Layer::with_content(
            sid,
            "Smart",
            lumenply_doc::LayerContent::Smart(lumenply_doc::SmartLayer {
                cache: Some(lumenply_render::transform_store(&src, &st)),
                source: src,
                transform: st,
            }),
        ));

        // Float mode plus an HDR value: tiles serialize as raw f32, so a
        // value above 1 must come back exactly.
        doc.float_mode = true;
        doc.layer_mut(bg).unwrap().pixels_mut().unwrap().set_pixel(
            3,
            3,
            lumenply_tiles::Rgba::from_straight(2.5, 0.5, 0.1, 1.0),
        );
        let path = temp("rt.nge");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();

        assert_eq!((back.width, back.height), (700, 300));
        assert_eq!(back.work_path, doc.work_path, "work path survives");
        assert_eq!(back.saved_paths, doc.saved_paths, "named paths survive");
        assert!(back.float_mode, "float mode survives");
        let p = back.layers()[0].pixels().unwrap().get_pixel(3, 3).to_straight();
        assert!((p[0] - 2.5).abs() < 1e-6, "HDR value survives the file: {p:?}");
        assert_eq!(
            back.layers()[0].effects,
            doc.layers()[0].effects,
            "effects survive"
        );
        assert_eq!(back.next_id(), doc.next_id());
        assert_eq!(back.layer_count(), 6);
        let smart = back.layer(sid).unwrap().smart_layer().unwrap();
        assert_eq!(smart.transform.coeffs(), st.coeffs(), "smart transform survives");
        let sp = smart.source.get_pixel(3, 3).to_straight();
        assert!((sp[1] - 0.9).abs() < 2e-4, "smart source survives: {sp:?}");
        let cache = smart.cache.as_ref().expect("cache rebuilt on load");
        assert!(
            cache.get_pixel(1, 1).a > 0.9,
            "cache rendered through the transform"
        );
        assert!(back.layers()[1].pass_through, "pass-through survives");
        assert!(back.layers()[1].children().unwrap()[0].clip, "clip flag survives");
        assert!(!back.layers()[0].pass_through);
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
        let before = lumenply_render::composite_raster(&doc);
        let after = lumenply_render::composite_raster(&back);
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

    #[test]
    fn fill_opacity_and_layer_style_details_survive() {
        use lumenply_doc::{BlendMode, LayerEffects, ShadowFx, StrokeAlign, StrokeFx};
        let mut doc = Document::new(16, 16);
        let a = doc.add_pixel_layer("Styled");
        let b = doc.add_pixel_layer("Plain");
        {
            let l = doc.layer_mut(a).unwrap();
            l.fill_opacity = 0.3;
            l.effects = LayerEffects {
                drop_shadow: Some(ShadowFx {
                    spread: 2.5,
                    blend: BlendMode::Multiply,
                    knockout: true,
                    ..ShadowFx::default()
                }),
                stroke: Some(StrokeFx {
                    position: StrokeAlign::Inside,
                    blend: BlendMode::Screen,
                    ..StrokeFx::default()
                }),
                ..LayerEffects::default()
            };
        }
        let path = temp("fill-opacity.lumen");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.layer(a).unwrap().fill_opacity, 0.3);
        assert_eq!(back.layer(a).unwrap().effects, doc.layer(a).unwrap().effects);
        assert_eq!(back.layer(b).unwrap().fill_opacity, 1.0);
        let _ = std::fs::remove_file(&path);
        // Effects written before these fields existed: an outside stroke,
        // normal blending, no spread and no knockout; 100% fill.
        let old: lumenply_doc::StrokeFx =
            serde_json::from_str(r#"{"size":3.0,"color":[1.0,1.0,1.0],"opacity":1.0}"#).unwrap();
        assert_eq!(old.position, StrokeAlign::Outside);
        assert_eq!(old.blend, BlendMode::Normal);
        let old: lumenply_doc::ShadowFx =
            serde_json::from_str(r#"{"dx":1.0,"dy":2.0,"blur":3.0,"color":[0.0,0.0,0.0],"opacity":0.5}"#)
                .unwrap();
        assert_eq!((old.spread, old.knockout), (0.0, false));
        assert_eq!(full_fill(), 1.0);
    }

    #[test]
    fn layer_locks_survive_and_unlocked_layers_stay_unlocked() {
        let mut doc = Document::new(16, 16);
        let a = doc.add_pixel_layer("Locked");
        let b = doc.add_pixel_layer("Free");
        let locks = lumenply_doc::LayerLocks {
            transparency: true,
            position: true,
            ..lumenply_doc::LayerLocks::NONE
        };
        doc.layer_mut(a).unwrap().locks = locks;
        let path = temp("locks.lumen");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.layer(a).unwrap().locks, locks);
        // Unlocked layers write no "locks" key, which is also exactly what
        // a file from before locks looks like.
        assert_eq!(back.layer(b).unwrap().locks, lumenply_doc::LayerLocks::NONE);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saved_selections_survive_the_file() {
        let mut doc = Document::new(300, 200);
        doc.add_pixel_layer("p");
        let mut sel = lumenply_doc::Selection::rect(lumenply_tiles::Rect::new(20, 30, 260, 100));
        sel.feather(4.0);
        let mask = sel.to_mask();
        doc.saved_selections = vec![
            lumenply_doc::SavedSelection {
                name: "Sky".into(),
                mask: mask.clone(),
            },
            lumenply_doc::SavedSelection {
                name: "All".into(),
                mask: Mask::reveal_all(),
            },
        ];
        let path = temp("channels.lumen");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back.saved_selections.len(), 2);
        assert_eq!(back.saved_selections[0].name, "Sky");
        for (x, y) in [(20, 30), (22, 80), (150, 60), (279, 129), (10, 10)] {
            assert_eq!(
                back.saved_selections[0].mask.value(x, y),
                mask.value(x, y),
                "({x}, {y})"
            );
        }
        assert_eq!(back.saved_selections[1].mask.value(5, 5), 1.0);
    }

    #[test]
    fn guides_survive_the_file_and_old_files_load_without_them() {
        let mut doc = Document::new(40, 30);
        doc.add_pixel_layer("p");
        doc.guides = vec![
            lumenply_doc::Guide::vertical(12.5),
            lumenply_doc::Guide::horizontal(20.0),
        ];
        let path = temp("guides.lumen");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.guides, doc.guides);

        // A manifest written before guides existed has no "guides" key.
        let old = r#"{"format":"nge","version":1,"width":4,"height":4,"next_id":1,"layers":[]}"#;
        assert!(load(craft_project("no-guides.nge", old, &[]))
            .unwrap()
            .guides
            .is_empty());
        let one = r#"{"format":"nge","version":1,"width":4,"height":4,"next_id":1,"layers":[],
            "guides":[{"orientation":"horizontal","pos":3.0}]}"#;
        let g = load(craft_project("one-guide.nge", one, &[])).unwrap().guides;
        assert_eq!(g, vec![lumenply_doc::Guide::horizontal(3.0)]);
    }
}
