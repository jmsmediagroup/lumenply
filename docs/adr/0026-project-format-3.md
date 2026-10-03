# ADR 0026: Project format 3: graph + blobs

Date: 2026-10-03 · Status: accepted

## Context

ADR 0025 makes the document an edit graph whose pixel data lives in a
content-addressed blob store. The layer-tree project (`.lumen` version 1,
ADR 0002: a manifest with the layer tree plus raw f32 tiles per layer)
can't hold a graph, and a graph whose history is long should open without
replaying it. ADR 0002's resilience goals stand: any zip tool can open the
file, a damaged entry loses a part, not the file.

## Decision

`.lumen` version 3 is a zip (`lumenply_io::graph_project`):

```text
manifest.json          {"format": "lumenply", "version": 3, "width", "height",
                        "blobs": [{"id", "tiles": [[x, y, "u16"|"f32"], ...]}],
                        "hints": [{"key", "hash", "tiles": [[x, y, "f32"|"u16"|"empty"], ...]}],
                        "patterns": [{"id", "name", "pixels": {"blob", "size"}}]}
graph.json             exactly Graph::to_json
meta.json              the editor's document state that isn't the image (opaque JSON)
blobs/<hash>/<x>_<y>   one tile of a blob, little-endian, in the format the manifest names
cache/<key>/<x>_<y>    one render-hint tile (optional)
```

- **Tiles keep their resting format.** A blob's hash covers each tile's
  storage format and bytes, so tiles are written as they rest in memory
  (16-bit after an edit, ADR 0004; f32 for imports and HDR) and come back
  bit for bit. Byte-plane shuffling and per-channel deltas before deflate
  were measured and rejected: they shrink smooth photographs by up to 60%
  but grow flat artwork and every f32 tile, so plain bytes stay the default.
- **Reachable blobs only.** A blob is written when an op names it (any
  64-hex-digit string in an op's parameters, `lumenply_graph::blob_refs`,
  so new ops need no registration) or the meta lists it under `"blobs"`
  (how editor state such as saved selections keeps pixels).
- **Integrity.** Loading re-hashes every blob; a blob whose hash, entry or
  size is wrong is dropped with a warning naming the layers using it, and
  everything else loads. Hints carry their own hash and are dropped the
  same way. A damaged `meta.json` resets to `{}` with a warning; a damaged
  manifest or graph is an error.
- **Render hints** are output tiles keyed like the render cache,
  `(content key, tile)` (`lumenply_graph::RenderHints`), seeded into a
  renderer's cache with `TileCache::seed`. A graph that changed has other
  keys, so a stale hint is never used; saving drops hints no node's key
  matches. They are exact (f32 as rendered), so a hinted render is
  bit-identical to a fresh one.
- **Pattern overlays.** A `layer` or `clip-group` op names its Pattern
  Overlay's pixels as a blob (`pattern_pixels` in its settings), as fill
  and shape ops name theirs, so the content key covers them and they are
  saved like any other blob; the effect's own `PatternRef` carries none in
  a lowered graph. (Stage 3b; before it, `PatternRef` kept them as derived
  state the JSON leaves out and the manifest's `patterns` table stored
  them. Loading still reads that table and puts the pixels back.)
- **Atomic, parallel I/O.** Saving writes a sibling temp file and renames
  it (as version 1 does). Tiles are deflated in parallel, each into a
  one-entry zip in memory that is raw-copied into the archive; loading
  reads, inflates and hashes tiles in parallel, one file handle per worker.
- **Versions.** The manifest keeps version 1's `format`/`version` fields.
  `project_version` peeks at them; `load_graph_project` refuses version 1
  and anything newer than 3. `project::load` is unchanged, so version 1 and
  legacy `.nge` files open exactly as before; `load_any_as_graph` lowers
  them (`document_to_graph`: resolution, guides, paths, float mode and
  saved selections go to the meta).

## Consequences

- Measured (`cargo run --release -p lumenply-cli --example project_sizes`):
  with 16-bit tiles the CLI demo document is 1.80 MB instead of 2.54 MB,
  a 65-layer PSD 3.88 MB instead of 4.96 MB and an A4 300 ppi fill-layer
  PSD 0.22 MB instead of 0.35 MB; saving is 7–15× faster and loading as
  fast or faster. Imported (f32) tiles take the same space as in version 1
  until an edit compacts them.
- Output hints cost about the flattened image again (f32): worth it when
  replaying the graph is slow (the A4 document opens and renders in 0.22 s
  with them, 0.48 s without), not for a few pixel layers.
- The blob hash is over native-endian bytes; every supported platform is
  little-endian, and the file is little-endian by definition.
- The meta's schema belongs to the editor (stage 3); only `"blobs"` is
  interpreted here.
