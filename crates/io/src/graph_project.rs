//! Project format 3 (ADR 0026): the document as an edit graph (ADR 0025)
//! plus the pixels its operations name.
//!
//! A plain zip archive:
//!
//! ```text
//! manifest.json            format and version, canvas, which tiles each blob and hint has
//! graph.json               the graph, exactly as `Graph::to_json` writes it
//! meta.json                the editor's document state that isn't the image (opaque here)
//! blobs/<hash>/<x>_<y>     one tile of a blob, as it rests in memory, little-endian
//! cache/<key>/<x>_<y>      one tile of a render hint (optional)
//! ```
//!
//! A tile entry holds 256×256 premultiplied linear RGBA pixels in the
//! format the manifest names for it: `"u16"` (8 bytes a pixel, the compact
//! form tiles rest in, ADR 0004) or `"f32"` (16 bytes a pixel), so loading
//! gives back the same bytes and therefore the same content hash. Entries
//! are deflated by the zip layer.
//!
//! Only blobs something refers to are written: those the graph's ops name
//! (see [`lumenply_graph::blob_refs`]) and those listed in the meta's
//! `"blobs"` array (`["<hash>", ...]`), which is how editor state such as
//! saved selections keeps pixels. On load every blob is hashed again and
//! one whose hash doesn't match its name is dropped with a warning, so a
//! damaged entry loses that blob, not the file (ADR 0002). Render hints are
//! a cache: a damaged hint is dropped the same way and only costs time.
//!
//! Pattern overlay effects keep their pattern's pixels outside the graph's
//! JSON (`PatternRef::image` is derived state), so the manifest's
//! `patterns` table stores them as blobs and loading puts them back.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};
use std::path::Path;
use std::sync::Arc;

use lumenply_doc::{Document, LayerEffects, PatternRef};
use lumenply_graph::{
    blob_refs, BlobId, BlobStore, ClipMember, Graph, Hash, Key, Op, PatternPixels, RenderHints, TileHasher,
};
use lumenply_tiles::{Raster, Tile, TileCoord, TileStore, TILE_PIXELS};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::project::{self, ProjectError};

/// The `version` a graph project's manifest carries.
pub const GRAPH_FORMAT_VERSION: u32 = 3;

/// Largest `graph.json` or `meta.json` the loader reads (decompressed).
const MAX_JSON: usize = 256 << 20;

/// How one tile entry stores its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TileFormat {
    /// Premultiplied linear f32 RGBA, 16 bytes a pixel.
    F32,
    /// Premultiplied linear u16 RGBA, 8 bytes a pixel (ADR 0004).
    U16,
    /// A hint for a fully transparent tile; no entry.
    Empty,
}

impl TileFormat {
    /// The tag [`Tile::raw_bytes`] uses.
    fn tag(self) -> Option<u8> {
        match self {
            TileFormat::F32 => Some(0),
            TileFormat::U16 => Some(1),
            TileFormat::Empty => None,
        }
    }

    fn of(tile: &Tile) -> TileFormat {
        if tile.is_compact() {
            TileFormat::U16
        } else {
            TileFormat::F32
        }
    }

    /// Bytes per channel value.
    fn width(self) -> usize {
        match self {
            TileFormat::F32 => 4,
            TileFormat::U16 => 2,
            TileFormat::Empty => 0,
        }
    }
}

/// `[x, y, format]` in the manifest.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct TileRecord(i32, i32, TileFormat);

#[derive(Serialize, Deserialize)]
struct BlobRecord {
    id: BlobId,
    tiles: Vec<TileRecord>,
}

#[derive(Serialize, Deserialize)]
struct HintRecord {
    /// The content key the tiles were rendered under.
    key: Key,
    /// Hash of the tiles (see [`hint_hash`]), checked on load.
    hash: Hash,
    tiles: Vec<TileRecord>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    writer: String,
    width: u32,
    height: u32,
    #[serde(default)]
    blobs: Vec<BlobRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hints: Vec<HintRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    patterns: Vec<PatternRecord>,
}

/// A pattern that a pattern overlay in the graph uses. `PatternRef` keeps
/// its pixels as derived state that the graph's JSON leaves out, so the
/// file stores them as a blob and loading puts them back.
#[derive(Serialize, Deserialize)]
struct PatternRecord {
    id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    /// The pixels, as fill and shape ops name theirs.
    pixels: PatternPixels,
}

/// A loaded graph project.
pub struct GraphProject {
    pub graph: Graph,
    /// The blobs the graph and the meta name (minus any that were damaged).
    pub blobs: BlobStore,
    /// The editor's document state, as saved; `{}` when there was none.
    pub meta: Value,
    /// Saved render hints; seed a renderer with [`RenderHints::seed`].
    pub hints: RenderHints,
    /// What was lost or repaired while loading, for the user.
    pub warnings: Vec<String>,
    /// The format version of the file it came from (1 for a layer-tree
    /// project converted on load).
    pub source_version: u32,
}

/// What a save wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveStats {
    pub blobs: usize,
    pub blob_tiles: usize,
    /// Hinted tiles written, transparent ones (which take no entry) included.
    pub hint_tiles: usize,
    /// Blobs in the store that nothing refers to, left out of the file.
    pub dropped_blobs: usize,
    /// Size of the file.
    pub bytes: u64,
}

/// The blob ids the meta lists under `"blobs"`.
pub fn meta_blob_refs(meta: &Value) -> Vec<BlobId> {
    meta.get("blobs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().and_then(Hash::from_hex))
                .collect()
        })
        .unwrap_or_default()
}

/// Every blob `graph` and `meta` refer to.
pub fn referenced_blobs(graph: &Graph, meta: &Value) -> BTreeSet<BlobId> {
    let mut refs: BTreeSet<BlobId> = blob_refs(graph).into_keys().collect();
    refs.extend(meta_blob_refs(meta));
    refs
}

/// Write a graph project to `path`, replacing any existing file
/// atomically (a failed save leaves the old file as it was).
///
/// Only the blobs `graph` or `meta["blobs"]` refer to are written, and only
/// the hints whose content key is still some node's key in `graph`.
pub fn save_graph_project(
    path: impl AsRef<Path>,
    graph: &Graph,
    blobs: &BlobStore,
    meta: &Value,
    hints: Option<&RenderHints>,
) -> Result<SaveStats, ProjectError> {
    graph.validate()?;
    let path = path.as_ref();
    let mut stats = project::save_atomically(path, |tmp| write_archive(tmp, graph, blobs, meta, hints))?;
    stats.bytes = std::fs::metadata(path)?.len();
    Ok(stats)
}

fn write_archive(
    path: &Path,
    graph: &Graph,
    blobs: &BlobStore,
    meta: &Value,
    hints: Option<&RenderHints>,
) -> Result<SaveStats, ProjectError> {
    let file = File::create(path)?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let deflate = FileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut stats = SaveStats::default();
    let mut entries: Vec<(String, &Tile)> = Vec::new();

    let hasher = TileHasher::default();
    let (pattern_records, pattern_blobs) = collect_patterns(graph, &hasher);
    let refs = referenced_blobs(graph, meta);
    let mut to_write: BTreeMap<BlobId, &TileStore> = refs
        .iter()
        .filter_map(|id| Some((*id, blobs.get(id)?.as_ref())))
        .collect();
    for id in pattern_blobs.ids() {
        to_write
            .entry(*id)
            .or_insert(pattern_blobs.get(id).expect("listed id"));
    }
    let mut blob_records = Vec::new();
    for (id, store) in &to_write {
        let mut tiles = Vec::new();
        for c in project::sorted(store) {
            let tile = store.tile(c).expect("coord came from the store");
            entries.push((format!("blobs/{id}/{}_{}", c.x, c.y), tile));
            tiles.push(TileRecord(c.x, c.y, TileFormat::of(tile)));
        }
        stats.blob_tiles += tiles.len();
        blob_records.push(BlobRecord { id: *id, tiles });
    }
    stats.blobs = blob_records.len();
    stats.dropped_blobs = blobs.ids().filter(|id| !to_write.contains_key(id)).count();

    let mut hints = hints.cloned().unwrap_or_default();
    hints.retain_graph(graph);
    let mut by_key: BTreeMap<Key, Vec<HintTile>> = BTreeMap::new();
    for (k, c, t) in hints.iter() {
        by_key.entry(k).or_default().push((c, t));
    }
    let mut hint_records = Vec::new();
    for (key, tiles) in &by_key {
        let mut records = Vec::new();
        for (c, t) in tiles {
            let format = match t {
                Some(t) => {
                    entries.push((format!("cache/{key}/{}_{}", c.x, c.y), t));
                    TileFormat::of(t)
                }
                None => TileFormat::Empty,
            };
            records.push(TileRecord(c.x, c.y, format));
        }
        stats.hint_tiles += records.len();
        hint_records.push(HintRecord {
            key: *key,
            hash: hint_hash(&hasher, tiles.iter().map(|(c, t)| (*c, *t))),
            tiles: records,
        });
    }
    write_tiles(&mut zip, &entries, deflate)?;

    zip.start_file("graph.json", deflate)?;
    zip.write_all(graph.to_json().as_bytes())?;
    zip.start_file("meta.json", deflate)?;
    zip.write_all(&serde_json::to_vec_pretty(meta)?)?;
    let manifest = Manifest {
        format: "lumenply".into(),
        version: GRAPH_FORMAT_VERSION,
        writer: format!("Lumenply {}", env!("CARGO_PKG_VERSION")),
        width: graph.width,
        height: graph.height,
        blobs: blob_records,
        hints: hint_records,
        patterns: pattern_records,
    };
    zip.start_file("manifest.json", deflate)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    project::finish_zip(zip)?;
    Ok(stats)
}

/// Write tile entries, compressing them in parallel: each tile is deflated
/// into a one-entry archive in memory by the zip library itself, then
/// copied into `zip` as is. A batch at a time bounds the memory held.
fn write_tiles<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    entries: &[(String, &Tile)],
    opts: FileOptions,
) -> Result<(), ProjectError> {
    const BATCH: usize = 64;
    for batch in entries.chunks(BATCH) {
        let packed: Vec<zip::result::ZipResult<Vec<u8>>> = batch
            .par_iter()
            .map(|(name, tile)| {
                let mut one = ZipWriter::new(Cursor::new(Vec::new()));
                one.start_file(name.as_str(), opts)?;
                one.write_all(&le_bytes(tile))?;
                Ok(one.finish()?.into_inner())
            })
            .collect();
        for one in packed {
            let mut one = ZipArchive::new(Cursor::new(one?))?;
            zip.raw_copy_file(one.by_index_raw(0)?)?;
        }
    }
    Ok(())
}

/// Hash of one key's hinted tiles: coordinates, and each tile's content
/// hash (zeros for a transparent tile), in tile order.
fn hint_hash<'a>(
    hasher: &TileHasher,
    tiles: impl IntoIterator<Item = (TileCoord, Option<&'a Arc<Tile>>)>,
) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(b"hints");
    for (c, t) in tiles {
        h.update(&c.x.to_le_bytes());
        h.update(&c.y.to_le_bytes());
        h.update(&t.map_or([0; 32], |t| hasher.tile(t).0));
    }
    Hash(*h.finalize().as_bytes())
}

/// The layer effects of an op that has them.
fn op_effects(op: &Op) -> Vec<&LayerEffects> {
    match op {
        Op::Layer { props } => vec![&props.effects],
        Op::ClipGroup { base, members } => std::iter::once(&base.effects)
            .chain(members.iter().filter_map(|m| match m {
                ClipMember::Layer { props } => Some(&props.effects),
                _ => None,
            }))
            .collect(),
        _ => Vec::new(),
    }
}

fn op_effects_mut(op: &mut Op) -> Vec<&mut LayerEffects> {
    match op {
        Op::Layer { props } => vec![&mut props.effects],
        Op::ClipGroup { base, members } => std::iter::once(&mut base.effects)
            .chain(members.iter_mut().filter_map(|m| match m {
                ClipMember::Layer { props } => Some(&mut props.effects),
                _ => None,
            }))
            .collect(),
        _ => Vec::new(),
    }
}

/// The patterns the graph's pattern overlays use, each with its pixels as
/// a blob (keyed by its hash).
fn collect_patterns(graph: &Graph, hasher: &TileHasher) -> (Vec<PatternRecord>, BlobStore) {
    let mut records: Vec<PatternRecord> = Vec::new();
    let mut pixels = BlobStore::new();
    for (_, node) in graph.nodes() {
        for fx in op_effects(&node.op) {
            let Some(p) = fx.pattern_overlay.as_ref().map(|po| &po.pattern) else {
                continue;
            };
            let Some(image) = &p.image else {
                continue;
            };
            if records.iter().any(|r| r.id == p.id) {
                continue;
            }
            records.push(PatternRecord {
                id: p.id.clone(),
                name: p.name.clone(),
                pixels: PatternPixels::store(image, &mut pixels, hasher),
            });
        }
    }
    (records, pixels)
}

/// Give every pattern overlay in `graph` its pattern's pixels again, by
/// id, or by name for a reference whose id matches none.
fn resolve_patterns(graph: &mut Graph, patterns: &[(&PatternRecord, Arc<Raster>)]) {
    let find = |p: &PatternRef| {
        patterns
            .iter()
            .find(|(r, _)| r.id == p.id)
            .or_else(|| {
                patterns
                    .iter()
                    .find(|(r, _)| !p.name.is_empty() && r.name == p.name)
            })
            .map(|(_, image)| image.clone())
    };
    let unresolved: Vec<lumenply_graph::NodeId> = graph
        .nodes()
        .filter(|(_, n)| {
            op_effects(&n.op).iter().any(|fx| {
                fx.pattern_overlay
                    .as_ref()
                    .is_some_and(|po| po.pattern.image.is_none())
            })
        })
        .map(|(id, _)| id)
        .collect();
    for id in unresolved {
        let _ = graph.update(id, |n| {
            for fx in op_effects_mut(&mut n.op) {
                if let Some(po) = &mut fx.pattern_overlay {
                    if po.pattern.image.is_none() {
                        po.pattern.image = find(&po.pattern);
                    }
                }
            }
        });
    }
}

/// A tile's stored bytes, little-endian.
fn le_bytes(tile: &Tile) -> std::borrow::Cow<'_, [u8]> {
    let (_, raw) = tile.raw_bytes();
    if cfg!(target_endian = "little") {
        std::borrow::Cow::Borrowed(raw)
    } else {
        std::borrow::Cow::Owned(swap_bytes(raw, TileFormat::of(tile).width()))
    }
}

/// Reverse the byte order of every `width`-byte value.
fn swap_bytes(bytes: &[u8], width: usize) -> Vec<u8> {
    bytes
        .chunks_exact(width)
        .flat_map(|c| c.iter().rev().copied())
        .collect()
}

fn tile_from_le(format: TileFormat, bytes: &[u8]) -> Option<Tile> {
    let tag = format.tag()?;
    if cfg!(target_endian = "little") {
        Tile::from_raw_bytes(tag, bytes)
    } else {
        Tile::from_raw_bytes(tag, &swap_bytes(bytes, format.width()))
    }
}

fn read_manifest<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>) -> Result<Value, ProjectError> {
    let bytes = project::read_entry_max(zip, "manifest.json", project::MAX_MANIFEST as usize)
        .map_err(|_| ProjectError::NotAProject("missing manifest.json".into()))?;
    let v: Value = serde_json::from_slice(&bytes)?;
    match v.get("format").and_then(Value::as_str) {
        Some("lumenply" | "nge") => Ok(v),
        other => Err(ProjectError::NotAProject(format!("format field is {other:?}"))),
    }
}

/// The format version of the project at `path`: 1 for a layer-tree
/// project (`project::load`), 3 for a graph project.
pub fn project_version(path: impl AsRef<Path>) -> Result<u32, ProjectError> {
    let mut zip = ZipArchive::new(BufReader::new(File::open(path)?))?;
    let v = read_manifest(&mut zip)?;
    v.get("version")
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
        .ok_or_else(|| ProjectError::Corrupt("manifest has no version".into()))
}

/// Whether `path` is a graph project (format 3 or newer).
pub fn is_graph_project(path: impl AsRef<Path>) -> bool {
    project_version(path).is_ok_and(|v| v >= GRAPH_FORMAT_VERSION)
}

/// Read a graph project. Damaged blobs, hints or meta are dropped with a
/// warning in [`GraphProject::warnings`]; a damaged manifest or graph is an
/// error.
pub fn load_graph_project(path: impl AsRef<Path>) -> Result<GraphProject, ProjectError> {
    let path = path.as_ref();
    let mut zip = ZipArchive::new(BufReader::new(File::open(path)?))?;
    let raw = read_manifest(&mut zip)?;
    let version = raw.get("version").and_then(Value::as_u64).unwrap_or(0);
    if version < GRAPH_FORMAT_VERSION as u64 {
        return Err(ProjectError::NotAProject(format!(
            "a layer-tree project (version {version}); open it with project::load or load_any_as_graph"
        )));
    }
    if version > GRAPH_FORMAT_VERSION as u64 {
        return Err(ProjectError::GraphTooNew(version.min(u32::MAX as u64) as u32));
    }
    let manifest: Manifest = serde_json::from_value(raw)?;

    let graph_json = project::read_entry_max(&mut zip, "graph.json", MAX_JSON)?;
    let graph = Graph::from_json(
        std::str::from_utf8(&graph_json)
            .map_err(|_| ProjectError::Corrupt("graph.json is not UTF-8".into()))?,
    )?;
    if graph.width == 0
        || graph.height == 0
        || graph.width > project::MAX_CANVAS
        || graph.height > project::MAX_CANVAS
    {
        return Err(ProjectError::Corrupt(format!(
            "canvas {}×{} is not a sane size",
            graph.width, graph.height
        )));
    }
    let mut warnings = Vec::new();
    if (manifest.width, manifest.height) != (graph.width, graph.height) {
        warnings.push(format!(
            "the manifest says {}×{} but the graph is {}×{}; using the graph's size",
            manifest.width, manifest.height, graph.width, graph.height
        ));
    }

    let meta = match project::read_entry_max(&mut zip, "meta.json", MAX_JSON)
        .and_then(|b| Ok(serde_json::from_slice::<Value>(&b)?))
    {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!(
                "document settings (meta.json) could not be read and were reset: {e}"
            ));
            Value::Object(Default::default())
        }
    };
    drop(zip);

    let hasher = TileHasher::default();
    let users = blob_refs(&graph);
    let mut blobs = BlobStore::new();
    let groups: Vec<(Hash, &[TileRecord])> = manifest.blobs.iter().map(|r| (r.id, &r.tiles[..])).collect();
    for (rec, loaded) in manifest
        .blobs
        .iter()
        .zip(read_groups(path, "blobs", &groups, &hasher))
    {
        let loaded = loaded.and_then(|tiles| {
            let mut store = lumenply_tiles::TileStore::new();
            for (c, t) in tiles {
                store.insert(c, t.ok_or("a blob tile is marked empty")?);
            }
            // Every tile's hash is already known from reading it.
            let actual = hasher.store(&store);
            if actual == rec.id {
                Ok(store)
            } else {
                Err(format!("its content hash is {actual}"))
            }
        });
        match loaded {
            Ok(store) => blobs.insert_trusted(rec.id, store),
            Err(why) => {
                let who = match users.get(&rec.id) {
                    Some(nodes) => {
                        let names: Vec<String> = nodes.iter().map(|n| describe(&graph, *n)).collect();
                        format!("used by {}", names.join(", "))
                    }
                    None => match manifest.patterns.iter().find(|p| p.pixels.blob == rec.id) {
                        Some(p) => format!("the pixels of pattern \"{}\"", p.name),
                        None => "used by the document settings".into(),
                    },
                };
                warnings.push(format!(
                    "pixel data {} is damaged and was dropped ({why}); {who}",
                    rec.id
                ));
            }
        }
    }
    for id in meta_blob_refs(&meta) {
        if !blobs.contains(&id) && !manifest.blobs.iter().any(|r| r.id == id) {
            warnings.push(format!(
                "pixel data {id} named by the document settings is missing"
            ));
        }
    }

    let mut patterns = Vec::new();
    for rec in &manifest.patterns {
        let side = 1..=lumenply_doc::pattern::MAX_PATTERN_SIDE;
        let [w, h] = rec.pixels.size;
        // A sane size before allocating the image.
        let image = (side.contains(&w) && side.contains(&h))
            .then(|| rec.pixels.image(&blobs))
            .flatten();
        match image {
            Some(image) => patterns.push((rec, Arc::new(image))),
            _ => warnings.push(format!(
                "pattern \"{}\" was lost; pattern overlays using it show none",
                rec.name
            )),
        }
    }
    let mut graph = graph;
    resolve_patterns(&mut graph, &patterns);
    // Pattern pixels are kept on the pattern overlays; their blobs are only
    // the file's way of carrying them.
    let used = referenced_blobs(&graph, &meta);
    for rec in &manifest.patterns {
        if !used.contains(&rec.pixels.blob) {
            let keep: std::collections::HashSet<BlobId> = blobs
                .ids()
                .filter(|id| **id != rec.pixels.blob)
                .copied()
                .collect();
            blobs.retain(&keep);
        }
    }

    let mut hints = RenderHints::new();
    let groups: Vec<(Hash, &[TileRecord])> = manifest.hints.iter().map(|r| (r.key, &r.tiles[..])).collect();
    for (rec, loaded) in manifest
        .hints
        .iter()
        .zip(read_groups(path, "cache", &groups, &hasher))
    {
        let loaded = loaded.and_then(|tiles| {
            let actual = hint_hash(&hasher, tiles.iter().map(|(c, t)| (*c, t.as_ref())));
            if actual == rec.hash {
                Ok(tiles)
            } else {
                Err("their hash doesn't match".to_string())
            }
        });
        match loaded {
            Ok(tiles) => {
                for (c, t) in tiles {
                    hints.insert(rec.key, c, t);
                }
            }
            Err(why) => warnings.push(format!(
                "render hints {} are damaged and were ignored ({why})",
                rec.key
            )),
        }
    }

    Ok(GraphProject {
        graph,
        blobs,
        meta,
        hints,
        warnings,
        source_version: version as u32,
    })
}

/// A node as a warning names it: its id and its name, or for an unnamed
/// node (a layer's content) the name of the node it feeds.
fn describe(graph: &Graph, id: lumenply_graph::NodeId) -> String {
    let named = |n: lumenply_graph::NodeId| graph.node(n).and_then(|n| n.name.clone());
    if let Some(name) = named(id) {
        return format!("{id} ({name})");
    }
    let consumer = graph
        .nodes()
        .find(|(_, n)| n.inputs.contains(&Some(id)) && n.name.is_some());
    match consumer {
        Some((_, n)) => format!("{id} (in {})", n.name.as_deref().unwrap_or_default()),
        None => id.to_string(),
    }
}

type LoadedTiles = Vec<(TileCoord, Option<Arc<Tile>>)>;

/// One hinted tile being saved (`None`: known to be transparent).
type HintTile<'a> = (TileCoord, Option<&'a Arc<Tile>>);

/// The tiles each group lists under `<dir>/<name>/<x>_<y>`, or why they
/// couldn't be read, one result per group. Tiles are read, inflated and
/// hashed (into `hasher`'s memo) in parallel, each worker with its own
/// handle on the file.
fn read_groups(
    path: &Path,
    dir: &str,
    groups: &[(Hash, &[TileRecord])],
    hasher: &TileHasher,
) -> Vec<Result<LoadedTiles, String>> {
    let jobs: Vec<(usize, usize)> = groups
        .iter()
        .enumerate()
        .flat_map(|(g, (_, records))| {
            records
                .iter()
                .enumerate()
                .filter(|(_, r)| r.2 != TileFormat::Empty)
                .map(move |(i, _)| (g, i))
        })
        .collect();
    let read: Vec<Result<Arc<Tile>, String>> = jobs
        .par_iter()
        .map_init(
            || {
                File::open(path)
                    .map_err(ProjectError::from)
                    .and_then(|f| Ok(ZipArchive::new(BufReader::new(f))?))
                    .map_err(|e| e.to_string())
            },
            |zip, &(g, i)| {
                let zip = zip.as_mut().map_err(|e| e.clone())?;
                let (name, records) = &groups[g];
                let TileRecord(x, y, format) = records[i];
                let entry = format!("{dir}/{name}/{x}_{y}");
                let bytes =
                    project::read_entry_max(zip, &entry, TILE_PIXELS * 16).map_err(|e| e.to_string())?;
                let tile = tile_from_le(format, &bytes).ok_or_else(|| {
                    format!("tile ({x}, {y}) is {} bytes, not a {format:?} tile", bytes.len())
                })?;
                let tile = Arc::new(tile);
                hasher.tile(&tile);
                Ok(tile)
            },
        )
        .collect();
    let mut read = read.into_iter();
    groups
        .iter()
        .map(|(_, records)| {
            let mut out = Vec::with_capacity(records.len());
            let mut err = None;
            for &TileRecord(x, y, format) in records.iter() {
                let tile = match format {
                    TileFormat::Empty => None,
                    _ => match read.next().expect("one result per job") {
                        Ok(t) => Some(t),
                        Err(e) => {
                            err.get_or_insert(e);
                            None
                        }
                    },
                };
                match project::checked_coord(x, y) {
                    Ok(c) => out.push((c, tile)),
                    Err(e) => {
                        err.get_or_insert(e.to_string());
                    }
                }
            }
            match err {
                Some(e) => Err(e),
                None => Ok(out),
            }
        })
        .collect()
}

/// Any project as a graph: a graph project as saved, a layer-tree project
/// (`.lumen` version 1 or a legacy `.nge`) loaded by [`project::load`] and
/// lowered with [`lumenply_graph::lower`] (see [`document_to_graph`]).
pub fn load_any_as_graph(path: impl AsRef<Path>) -> Result<GraphProject, ProjectError> {
    let path = path.as_ref();
    if project_version(path)? >= GRAPH_FORMAT_VERSION {
        return load_graph_project(path);
    }
    let doc = project::load(path)?;
    Ok(document_to_graph(&doc, project::FORMAT_VERSION))
}

/// A layer-tree document as a graph project: the layers lowered to a graph
/// (pixel content becomes `image` blobs) and the document state that isn't
/// layers in the meta:
///
/// ```text
/// {"resolution": 72.0, "float_mode": false, "guides": [...],
///  "work_path": {...}, "saved_paths": [...],
///  "channels": [{"name": "Sky", "default": 0.0, "blob": "<hash>"}],
///  "blobs": ["<hash>", ...]}
/// ```
pub fn document_to_graph(doc: &Document, source_version: u32) -> GraphProject {
    let hasher = TileHasher::default();
    let mut blobs = BlobStore::new();
    let graph = lumenply_graph::lower(doc, &mut blobs, &hasher).graph;
    let mut meta_blobs = Vec::new();
    let channels: Vec<Value> = doc
        .saved_selections
        .iter()
        .map(|ch| {
            let blob = (!ch.mask.tiles.is_empty()).then(|| blobs.insert(&hasher, ch.mask.tiles.clone()));
            meta_blobs.extend(blob);
            serde_json::json!({"name": ch.name, "default": ch.mask.default, "blob": blob})
        })
        .collect();
    let mut meta = serde_json::json!({
        "resolution": doc.resolution,
        "float_mode": doc.float_mode,
        "guides": doc.guides,
        "saved_paths": doc.saved_paths,
        "channels": channels,
        "blobs": meta_blobs,
    });
    if let Some(p) = &doc.work_path {
        meta["work_path"] = serde_json::to_value(p).expect("paths serialise");
    }
    GraphProject {
        graph,
        blobs,
        meta,
        hints: RenderHints::new(),
        warnings: Vec::new(),
        source_version,
    }
}

#[cfg(test)]
mod tests;
