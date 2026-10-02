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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pass_through: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    clip: bool,
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
        mask,
        collapsed: layer.collapsed,
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
    if manifest.format != "nge" {
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
    let doc = Document::from_parts(manifest.width, manifest.height, layers, manifest.next_id);
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
            nge_render::text::refresh_cache(&mut t);
            LayerContent::Text(t)
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
    layer.mask = mask;
    layer.collapsed = r.collapsed;
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
    use nge_tiles::Raster;

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
        nge_render::text::refresh_cache(&mut tl);
        doc.add_layer(Layer::text(tid, tl));

        let path = temp("rt.nge");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();

        assert_eq!((back.width, back.height), (700, 300));
        assert_eq!(back.next_id(), doc.next_id());
        assert_eq!(back.layer_count(), 5);
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
