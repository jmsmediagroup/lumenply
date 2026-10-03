# 25. The document is a graph of operations

Date: 2026-10-03. Status: accepted (in progress; see "Stages").

## Context

Until now a document has been a layer tree whose pixel layers hold baked
tiles (ADR 0001). Painting, retouching and destructive filters write into
those tiles, undo keeps copy-on-write snapshots of the whole document, and
rendering walks the tree per tile with one cache: the composite below the
layer being edited (`render::BelowCache`). Adjustment, fill, text, shape and
smart layers and smart filters are already non-destructive, but everything
else ends as pixels, so a stroke from an hour ago can't be changed, files
grow with every paint session, and the renderer can't reuse work beyond the
one cached backdrop.

The owner asked for a non-destructive edit graph: the document stored as a
DAG of operations in JSON, each node a type plus parameters plus input
references; rendering by walking the graph with a cache per node that is
invalidated downstream of a change; undo and redo as graph history; a Rust
core with tiled, GPU-side compositing through wgpu.

## Decision

### Model

A new engine crate, `lumenply-graph`, sits between `render` and `core`
(`core → graph → render → doc → tiles`; no UI, no window).

```text
Graph { canvas, output: NodeId, nodes: {NodeId → Node} }
Node  { op: Op, inputs: [NodeId | null], name? }
Op    = tagged enum: "type" + parameters (serde, JSON)
```

`NodeId` is a node's stable identity, the one edits, the UI and the
Layers panel refer to. Inputs are positional ports defined by the op (for
example `layer`: backdrop, content, mask). A node never stores pixels.
Pixel data an operation needs (an imported photo, a brush tip, a pattern)
lives in a content-addressed **blob store** and is referenced by its hash,
so the JSON stays small and identical data is stored once.

### Content keys: invalidation without bookkeeping

Every node has a **content key**: a BLAKE3 hash of its op's canonical JSON,
the canvas size (a fill covers the canvas, filters treat its edge
specially) and the content keys of its inputs, computed bottom-up. Changing a
parameter changes that node's key and therefore the key of everything
downstream, and of nothing else. The render cache is keyed by
`(content key, tile)`, so:

- invalidation is automatic and exact: downstream nodes miss, siblings and
  upstream nodes keep hitting;
- undo and redo are instant: the previous graph's keys are still in the
  cache until memory pressure evicts them;
- two nodes that compute the same thing share one cache entry.

### Evaluation

Rendering pulls tiles: the output node is asked for the tiles in view, and
each op asks its inputs for the tiles it needs (a blur pads by its radius,
a transform maps the tile back through its inverse, a whole-image op such
as Equalize asks for everything once). Tiles are evaluated in parallel,
and nothing ever waits for another thread's computation: a thread that
wants a tile someone else is computing computes it too, and the first
result is kept. Waiting deadlocks under rayon, whose threads run other
queued jobs while they wait, so a thread can end up waiting for a tile its
own stack is still computing. The duplicate work this allows is small (up
to 3% of tiles on the corpus); `Renderer::plan` trades it for a
node-by-node schedule that never duplicates but renders cold documents
30–50% slower. An
op whose effect doesn't reach a tile (a brush stroke elsewhere) returns its
input tile unchanged, sharing the same `Arc`, so long chains cost neither
time nor memory outside the area they touch.

The cache holds tiles under a byte budget with least-recently-used
eviction. Results can also be persisted in the project file as hints keyed
by content key, so a document with a long history opens without replaying
it.

Some ops are whole-image computations: a text layer's glyphs, a fill or
shape over the canvas, a smart object's transform, a stack of smart
filters. They call the same `lumenply-render` function the layer tree's
derived caches come from, so their pixels are bit-identical to it. Each
makes its whole output once per content key (`Ctx::whole`, kept in a
separate byte-budgeted cache) and serves tiles from it. A render first
makes the whole outputs it will read, in dependency order and independent
ones in parallel, and only then pulls tiles: a thread that blocked waiting
for another's whole result inside rayon's work stealing could deadlock, so
nothing ever waits for one. A run of smart filters is one node per filter
but is evaluated as one fused, chunked pass, exactly as the layer tree does:
the box blurs' running sums depend on where a pass starts, so filtering node
by node would differ in the last bits.

### History

An edit produces a new graph version; the previous version is kept. Undo
and redo move a cursor over versions. Versions share unchanged nodes
(`Arc`), so a step costs the nodes it changed — parameters, not pixels.
Slider drags coalesce into one version, as today.

### Storage

`.lumen` becomes a zip of `graph.json` (the graph, format version 3) plus
`blobs/<hash>` entries and optional `cache/<key>/<x>_<y>` render hints.
Version 1 and 2 projects (layer trees with tiles) still open: they are
converted to a graph whose pixel layers become `image` nodes.

### GPU

The CPU evaluator is the reference. The wgpu executor runs the same graph
with one compute kernel per op, keeps tiles resident in GPU memory under
the same content keys, and draws into the UI's wgpu surface without a
readback. An op without a kernel runs on the CPU for that node and the
result is uploaded; every kernel lands with an equality test against the
CPU op (ADR 0006's rule, now per op).

### The Layers panel

The Photoshop-style layer stack stays the main UI. A layer is a view of a
small, regular region of the graph: the content chain (an image or fill
source followed by strokes, filters and transforms) feeding a `layer` node
that composites onto the layer below. The editor keeps the layer tree as a
projection of the graph for the existing tools and commands; commands are
ported one by one to emit graph edits directly, starting with brush strokes,
layer properties, filters and adjustments. A command not yet ported still
works: its pixel result is stored as a new `image` node (a blob), so it is
undoable and saved, but not editable after the fact.

## Stages

1. **Graph core**: model, JSON, blob store, content keys, pull evaluator
   with per-node tile cache, history. Lowering from today's `Document` to a
   graph, proven by rendering every test document and the PSD corpus both
   ways and requiring equal pixels.
2. **Operations instead of pixels**: brush strokes, erasing, fills, filters
   and adjustments applied to pixels become nodes; dab rasterisation moves
   from `core` into `render` so the graph can replay it.
3. **Graph-backed editor**: the editor's source of truth and undo history
   become graph versions; the app renders through the graph evaluator
   (replacing `BelowCache`); `.lumen` v3.
4. **GPU executor**: per-op WGSL kernels with CPU parity tests, resident
   tiles, eframe on its wgpu backend so the canvas is drawn without a
   readback.

## Consequences

- Any edit stays editable as long as its node exists; files store the
  recipe plus the source pixels it needs.
- Opening a document with a long history replays it unless render hints
  are saved with it; hints are a cache and may be dropped at any time.
- Rendering does strictly less work than today for repeated edits, and the
  one-layer `BelowCache` becomes a special case of the node cache.
- Determinism becomes a hard requirement for every op: same inputs and
  parameters, same pixels, on every machine and thread count. Ops that use
  randomness hash it from parameters (as brush scatter and Dissolve already
  do).
- The node cache can use a lot of memory; the budget is a preference and
  eviction is safe because everything can be recomputed.
- PSD export flattens the graph to layers exactly as today; PSD import
  produces a graph through the lowering.
