# 27. The edit graph on the GPU

Date: 2026-10-03. Status: accepted (engine; drawing into the app's canvas
is the remaining step of ADR 0025 stage 4).

## Context

ADR 0025 makes the document a graph whose render cache is keyed by content:
`(content key, tile)`. Its stage 4 asks for a wgpu executor that evaluates
the same graph on the GPU with tiles resident in GPU memory. ADR 0006's
`GpuCompositor` walks the layer tree per call, re-uploads every layer and
reads the result back, and declines a whole document when one layer needs
the CPU; it optimises nothing by itself.

## Decision

### Where it lives

`lumenply_graph::gpu`, a module of the graph crate, compiled
unconditionally. Not a cargo feature: wgpu is already an unconditional
dependency of `lumenply-render` (ADR 0006), which the graph crate depends
on, so a feature would remove nothing from any build. Not a separate
crate: the executor reads the evaluator's crate-private pieces (`Ctx`,
`ops::input_needs`, `ops_content::prepare`) and must follow every op the
graph gains, so it sits beside `ops.rs`. It opens no window and no
surface: `GpuRenderer::new()` makes a headless device (`None` without an
adapter, so the engine-crate rule holds and machines without a GPU skip),
and `GpuRenderer::with_device` runs on a caller's device, such as eframe's.

### Resident tiles under content keys

The GPU cache maps `(content key, tile)` to one 256×256 texture
(`Rgba32Float`; a compact blob tile uploads as stored into `Rgba16Unorm`
when the adapter has 16-bit normalised textures, halving upload and
memory) or to a solid colour (a mask's default coverage), under a byte
budget (default 2 GiB) with least-recently-used eviction, as
`cache.rs` does on the CPU. The keys are the CPU's, so an edit recomputes
exactly what is downstream on either device and undo finds the previous
version's textures. Evicted textures return to a free pool once every
command reading them has been submitted. Blob tiles upload once per tile
identity (the `Arc`), so a repainted layer, a new blob sharing all but a
few tiles with the old, uploads only the tiles that changed. Uploads go
through a mapped staging buffer filled in parallel.

### Kernels

WGSL compute kernels, one per op, mirroring the CPU maths; the blend
functions are `lumenply_render::gpu::BLEND_WGSL`, shared with ADR 0006's
compositor rather than copied:

- `layer` without effects: every blend mode but Dissolve, opacity × fill
  opacity, mask;
- `adjustment` compiling to 1D tables (`Lut`, `LutRgb`: Levels, Curves,
  Invert, Brightness/Contrast, Posterize), any blend mode but Dissolve;
- `pass-through` (mix by opacity × mask);
- `image`, `mask`, `empty` (uploads and solids), and any hidden layer
  (its backdrop passes through, no work).

### Honest fallback, per node

Every other op or parameter combination (layer effects, Dissolve, live
filters, clip groups, per-pixel and gradient-map adjustments, text, fill,
shape, transform, smart filters, brush strokes, and any op added later,
through the match's catch-all arm) runs on the renderer's own CPU
`Renderer` for that node, and the result is uploaded. Before that, the
tiles its CPU op reads (`ops::input_needs`) are computed on the GPU, read
back and put into that CPU renderer's cache under their content keys, so
the CPU evaluates the fallback node only, not the stack below it. Ops
computed in one piece, and smudge or blur strokes, are made once first
(`ops_content::prepare`, `ops_paint::prepare`). The fallback renderer is
private to the GPU renderer, so tiles that came from the GPU never land in
a reference renderer's cache.

### Scheduling

A render plans first: from the requested tiles it walks the inputs each op
reads, stopping at cached tiles, and notes per node which tiles must exist
on the GPU and which in the CPU cache. It then runs node by node in a
depth-first post-order from the output (a stack's layers interleave with
their contents): one compute pass and one submit per node, fallback nodes
on the CPU in parallel over their tiles. Tiles are pinned by use counts
until their last consumer ran, so memory stays near the budget plus about
two nodes of tiles. Node-by-node order also suits the cache: the most
recently computed nodes, the top of the stack, are the ones kept, which is
what an edit near the top needs.

### APIs

- `render_gpu` / `render_node_gpu` → `GpuImage`: the result left on the
  GPU (textures held by `Arc`); the module docs describe drawing it in an
  `egui_wgpu` paint callback without a readback.
- `render_to_store` / `render_node_to_store` → `TileStore`: read back,
  same pixels as `Renderer::render_node` within 1e-4; `read_back(&GpuImage)`.
- `stats()`: dispatches, uploads, readbacks, CPU-fallback tiles by op.

### Equality

The CPU evaluator is the reference and the tolerance is 1e-4. Tests cover
every blend mode with masks (painted, hidden by default, partial default),
fractional opacity and fill, on f32 and 16-bit tiles; table adjustments in
several modes, one table or three; pass-through and isolated groups; every
fallback kind between GPU layers (and that the CPU never recomputed the
stack below); every content op; stroke chains (paint, smudge, blur) and an
edited stroke; caching across edits and undo; upload reuse; a tiny budget. `lumenply graph FILE --check --gpu` and
`scripts/graph_corpus_check.py --gpu` compare the corpus.

GPU and CPU arithmetic differ in the last bits (fused multiply-adds,
Metal's fast division): a few 1e-7 after a stack of layers. An op as steep
as a step (Posterize's table, Hard Mix, Hue/Saturation modes on nearly grey
colours) magnifies that by its slope where a pixel sits at the step; that
is floating point, not a GPU feature, and moving the steep op to the CPU
would not help, as its input would still come from the GPU. The corpus
stays far inside the tolerance (see Consequences).

## Consequences

- Corpus (482 PSDs, `graph_corpus_check.py --gpu`): every file within
  1e-4 of the CPU evaluator, worst difference 8.3e-7. 243 files send some
  tiles to the CPU: 2520 shape, 1039 layer (effects, Dissolve), 1018
  fill, 135 adjustment (per-pixel), 69 clip-group and 17 text tiles.
- Timings (`lumenply bench --graph cpu|gpu`, 20 full 4096² layers, the
  second layer from the top edited; M4 Pro, 14 CPU and 20 GPU cores, best
  of three, ms; "resident" waits for the GPU but leaves the result there):

  | | CPU, 8 GiB cache | GPU, 2 GiB | GPU, 8 GiB |
  |---|---|---|---|
  | cold, mixed modes | 212 | 345 (+22 readback) | 426 |
  | warm | 0.25 | 0.10 | 0.13 |
  | opacity edit | 27 | 16 resident, 36 read back | 20, 44 |
  | one-pixel paint edit | 32 | 15 | 21 |
  | cold, all Normal | 85 | 308 | |
  | opacity edit, all Normal | 11 | 15 resident, 36 read back | |

  A cold GPU render uploads every layer (2.6 GB of 16-bit texels here) and
  commits fresh GPU memory for every node's tiles, so the 14-core CPU beats
  it; under a budget the stack overflows, evicted textures are reused and
  it gets faster. Edits in blend modes other than Normal are where the GPU
  wins, as long as the result stays on the GPU: reading the canvas back
  (256 MB) costs about as much as the edit. Normal-mode compositing is
  memory-bound on both and the CPU's simple loop keeps up. Every render
  that waits for the GPU pays a ~1.5 ms submit-and-wait round trip; a
  canvas drawn from the resident tiles in the same frame doesn't wait. The
  CPU renderer with a 2 GiB cache rerenders everything on every edit (167
  ms warm): pulling tiles through the whole stack in parallel keeps the
  most recent *tiles*, not the most recent *nodes*, while the GPU's
  node-by-node schedule keeps the top of the stack.
- Next speed-ups, aimed at the bottlenecks above: fewer, larger dispatches
  (several tiles per dispatch, or texture arrays); blob tiles kept resident
  across documents; drawing without waiting.
- A new op falls back to the CPU automatically; a kernel for it lands with
  its equality test in the same commit (ADR 0006's rule, per op), and its
  `input_needs` keeps its inputs on the GPU. Strokes run on the CPU, which
  walks a stroke chain in one go (`ops_paint::tile`); their GPU tiles are
  keyed by content key, and the upload memo means unchanged stroke tiles
  (the same `Arc` from the CPU's tile-keyed cache) never upload twice.
- Wiring into the app (stage 4's remaining step): eframe on its wgpu
  backend (`eframe` feature `wgpu` instead of `glow`; egui 0.29 uses wgpu
  22, the version linked here), the GPU renderer built on eframe's device
  with `with_device`, the canvas drawn from `GpuImage` tiles in an
  `egui_wgpu` paint callback (a quad per tile, `textureLoad`,
  checkerboard, linear to sRGB). Until then the CPU evaluator renders the
  app.
