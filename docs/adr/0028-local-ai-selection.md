# 28. Local AI selection and masking

Date: 2026-10-03. Status: accepted (in progress).

## Context

Photoshop's Object Selection, Select Subject and Remove Background are now
expected of a photo editor. The owner asked for the same, running locally:
ONNX models run through ONNX Runtime with the platform's accelerator
(CoreML, DirectML, CUDA) picked automatically; MobileSAM or EfficientSAM
for click-to-select; BiRefNet or RMBG-2.0 for background removal; the input
downscaled to the model's size, the mask upscaled and its edges refined;
models downloaded on first use so the installer stays small.

## Decision

### Runtime

A new engine crate, `lumenply-ai` (no UI; `app/cli → ai → core`), runs
models through the `ort` crate (ONNX Runtime 1.2x, prebuilt binaries linked
at build time). Sessions register CoreML on macOS, DirectML on Windows and
CUDA where its libraries are present, in that order, and ONNX Runtime falls
back to the CPU for anything a provider can't run or when none registers.
The provider actually used is reported, so the UI and bug reports can say
"running on CoreML".

### Models

| Task | Model | Files | Licence |
|---|---|---|---|
| Click-to-select (Object Selection) | MobileSAM (TinyViT encoder + SAM decoder) | `mobile_sam_image_encoder.onnx` 28 MB, `sam_mask_decoder_multi.onnx` 16.5 MB (huggingface.co/Acly/MobileSAM) | MIT export of Apache-2.0 weights |
| Subject / background removal | BiRefNet lite, run at 768² (high detail: 1024²) | `onnx/model.onnx` 181 MB (huggingface.co/senty-au/BiRefNet_lite-ONNX-dynamic: ZhengPeng7/BiRefNet_lite's weights re-exported with native DeformConv); see "BiRefNet re-export" below | MIT |

RMBG-2.0 is not offered: its weights are CC BY-NC 4.0 (non-commercial
only; commercial use needs a paid licence from BRIA), which a free editor
anyone may use commercially can't pass on. EfficientSAM-Ti (Apache-2.0)
stays a drop-in alternative to MobileSAM behind the same interface.

Every model file is pinned to a repository revision and verified by
SHA-256 after download. A registry in the crate lists, per model, the files,
their URLs, sizes, hashes, licence and input size.

### Download on first use

Nothing is bundled. The first time a feature needs a model, the app says
what it will download (name, size, source, licence) and downloads only
after the user agrees, with progress and Cancel. Files go to the data
folder (`~/.lumenply/models/<model>/`), written to a temporary name and
renamed after the hash checks out. Preferences lists the models with their
size and a Remove button. Offline, the feature explains that the model
isn't installed. Images never leave the machine.

### Pipelines

- **Click-to-select**: the image (the active layer or the composite) is
  resized so its longest side is 1024, padded to 1024², normalised, and
  encoded once; the embedding is cached by the image's content hash, so
  every further click only runs the 4 ms decoder. Prompts are positive and
  negative points and boxes. Of the decoder's candidate masks the best
  scored one is kept; its low-resolution logits are upscaled to the image
  size, thresholded, then refined.
- **Background removal**: the image is resized to 768² (1024² for high
  detail), normalised, run,
  and the sigmoid output upscaled to the image size, then refined.
- **Edge refinement**: a guided filter with the full-resolution image as
  the guide turns the upscaled mask into a soft, edge-following matte
  (the same engine as Select and Mask, whose matting can be applied on
  top for hair).

### In the app

- Object Selection: a mode of the Quick Selection tool. Click selects the
  object under the pointer, Alt-click excludes, drag a box around an
  object; Shift adds to the current selection, Alt subtracts.
- Select ▸ Subject (BiRefNet) and Layer ▸ Remove Background, which adds
  a layer mask from the matte (non-destructive; the pixels stay).
- Inference runs off the UI thread with a progress indicator; the result
  lands as an ordinary undoable edit.

### In the edit graph (ADR 0025)

Model output is not bit-identical across providers and machines, so an AI
mask can't be an operation re-evaluated from its parameters. It is stored as
a mask blob; the node records the model and prompts for provenance and for
an explicit "run again".

### API contract (`lumenply-ai`)

```rust
pub enum ModelId { MobileSam, BiRefNetLite }
pub struct ModelInfo { id, name, files: &[ModelFile], licence, total_bytes }
pub fn models() -> &'static [ModelInfo];
pub struct ModelStore;   // new(dir), installed(id), download(id, progress, cancel), remove(id)
pub struct Runtime;      // new(), provider() -> &str
pub struct Segmenter;    // load(&Runtime, &ModelStore), embed(&Raster) -> Embedding,
                         // segment(&Embedding, &[Prompt]) -> Matte
pub enum Prompt { Point { x, y, positive }, Box { x0, y0, x1, y1 } }
pub struct Matter;       // load(&Runtime, &ModelStore), matte(&Raster) -> Matte
pub struct Matte { width, height, alpha: Vec<f32> }  // full resolution, refined
pub fn refine(&Raster, &Matte, RefineOptions) -> Matte;
```

## Implementation notes (engine, 2026-10-03)

Measured on an M4 Pro (macOS 26) with other builds running, so times are
the best of several runs; the 1800×1205 demo photo unless stated.

**Runtime.** `ort` 2.0.0-rc.13 links pyke's static ONNX Runtime 1.28
(macOS arm64 with CoreML, Windows with DirectML, Linux CPU). There is no
prebuilt library for Intel Macs: there `lumenply-ai` needs a self-built
ONNX Runtime (`ORT_LIB_LOCATION`). CUDA is a cargo feature (`cuda`) because
its library bundle is a large separate download at build time; DirectML
needs `DirectML.dll` shipped beside the executable (ort copies it into the
target folder). The CLI's `ai` feature (on by default) keeps ONNX Runtime
out of builds that don't want it. It adds 20.4 MB to a stripped binary
(16.3 → 36.7 MB for `lumenply`; 28.7 MB unstripped) and links
CoreML.framework and libc++; the app grows by the same.

**Which provider runs what** is decided per model file, from measurement
(`ModelFile::avoid`), and every load falls back provider by provider: a
provider that fails to register or to compile the model is skipped, the
reason kept in `ModelReport::fallback`. ONNX Runtime 1.28's CoreML provider
has two defects that matter here:

- MobileSAM's encoder, whose input `[image_height, image_width, 3]` has
  free dimensions, compiled into an ML Program that then failed to load
  (error −14). It is always fed the padded 1024² frame, so those
  dimensions are pinned (`with_dimension_override`) and it compiles;
- it rejects a Conv or AveragePool without an explicit `pads` attribute
  ("Required param 'pad' is missing"), which BiRefNet's ONNX export has
  (fp32 and fp16 alike). As a NeuralNetwork instead of an ML Program,
  BiRefNet compiled for over 25 minutes for the Neural Engine, and on GPU
  and CPU it ran 380–406 s against 2.5 s on ONNX Runtime's CPU (Python
  ONNX Runtime 1.30 behaves the same).

| Model | CoreML | CPU only | Used on macOS |
|---|---|---|---|
| MobileSAM encoder (521 of 581 nodes on CoreML, 21 partitions) | 50–58 ms a run; first load 7.4 s (compile), then 1.4–1.6 s (cached) | 182–237 ms; load 0.1 s | CoreML |
| MobileSAM decoder (390 of 455 nodes, 19 partitions) | 50–57 ms a click | 11.5–13 ms | CPU |
| BiRefNet lite fp32 | fails to compile (87 s) | 2.46–2.55 s; load 1.0 s | CPU |
| BiRefNet lite fp16 | fails to compile (82 s) | 2.64–2.72 s; load 1.3 s | — |

CoreML's and the CPU's MobileSAM masks agree on every pixel of the demo
photo. CoreML's compiled models are cached per model file, dimension
overrides and ONNX Runtime build (ONNX Runtime's own key ignores the
overrides and reused a stale compile), and the cache is deleted when a
CoreML compile fails (a failed BiRefNet compile left 1.1 GB).

**fp32 BiRefNet** (superseded by the re-export below). The onnx-community
fp16 export's matte equals fp32's within one 8-bit step, but on the CPU it
is 7 % slower and peaks 1 GB higher (its casts).

**Memory.** onnx-community's BiRefNet export writes the deformable
convolution as plain gathers over `[1, 64, 49, 256, 256]` tensors (822 MB
each): a run peaked at about 10.5 GB resident on the CPU (Python ONNX
Runtime: 13.9 GB), too much for the 8–16 GB Macs most people have, hence
the re-export below. Subject sessions run without ONNX Runtime's CPU arena,
which otherwise keeps a run's peak for the session's life; the app drops
the `Matter` after use. MobileSAM peaks at 0.95 GB (CoreML) or 1.2 GB (CPU)
on the demo photo, 2.9 GB on a 6000×4016 image (whose f32 raster alone is
385 MB).

**Refinement** differs from the plan in two ways:

- Selections: the guided filter only aligns a mask's transition with an
  image edge; each side keeps its local mean of the mask, so an edge a cell
  off (MobileSAM's grid cell is 7 px on the demo photo, 23 px at 6000 px)
  stays partly wrong. On the demo photo the raw and filtered masks cut 3–6
  px of rock off the ridges and kept sky above the summit. Select and Mask's
  colour-sampling matting in a band of 1.5 cells puts the edge on the rock;
  it is the default (`RefineOptions::selection`). Cost: 50 ms on the demo
  photo, 0.56 s at 24 MP; the 12 ms decode stays available unrefined
  (`Segmenter::predict`) for hover previews.
- Mattes: the filter in a full-resolution band left a tail of 3–26 %
  coverage up to 10 px outside an edge at a 4.7× upscale. Instead the
  guided filter is used as an upsampler (He and Sun's fast guided filter):
  its coefficients are fitted at the model's resolution against the image the
  model saw, where its matte matches the image, and evaluated at every
  full-resolution pixel. Edges land on the image's edges with no halo; 45 ms
  at 24 MP.

The guide is the image over white, sRGB-encoded at 8 bits (3 bytes a pixel,
kept in the embedding so a click needs no image).

### BiRefNet re-export (2026-10-03, later the same day)

Measured on the CPU (M4 Pro) on four CC0 photos from Unsplash via Wikimedia
Commons with clear subjects (a dark-haired woman by a tree, curly hair
spread out, a dog in grass, a sleeping tabby with whiskers; 1920 px wide)
and the synthetic ball. Quality is against the onnx-community lite export
at 1024², both after the guided upsampling: IoU of the ≥ ½ masks, and the
mean difference within 8 px of the subject's edge.

| Model | Download | Peak resident | A run | IoU | Edge band |
|---|---|---|---|---|---|
| onnx-community lite, 1024² (before) | 224 MB | 9.65–10.3 GB | 2.6–2.8 s | — | — |
| onnx-community BiRefNet_512x512 fp32 (full Swin-L model) | 940 MB | 4.5–4.65 GB | 1.0–1.2 s | 0.981–0.996 | 0.043–0.136 |
| same, fp16 | 473 MB | 4.3–4.9 GB | 1.3–1.4 s | same | same |
| re-exported lite, 1024² (high detail) | 181 MB | 5.0–5.3 GB | 1.0–1.2 s | 0.998–0.9997 | 0.003–0.022 |
| re-exported lite, 768² (**default**) | 181 MB | 3.1–3.3 GB | 0.5–0.67 s | 0.990–0.997 | 0.040–0.091 |
| re-exported lite, 512² | 181 MB | 1.7–1.8 GB | 0.25–0.35 s | 0.956–0.995 | 0.058–0.149 |

Looked at (cut-outs over a checkerboard): whiskers survive everywhere;
768² follows 1024² closely, a little softer on the dog's ear behind a
blurred grass blade; the 512x512 model fills more tile between the curls
with a soft halo; 512² of the lite model drops that ear altogether. So the
default runs the lite model at 768² and `Detail::High` the same file at
1024²: one 181 MB download, a third (or half) of the old memory, faster.

**Provenance.** The weights are ZhengPeng7/BiRefNet_lite (Peng Zheng et
al., MIT). The ONNX file is senty-au's re-export
(huggingface.co/senty-au/BiRefNet_lite-ONNX-dynamic, MIT, uploaded
2026-09-18), pinned to commit `173d635935b93839608b9b9039da8d1d212471e9`,
`onnx/model.onnx`, 180,839,545 bytes, SHA-256
`1e0da42f0fde010e32e938bad388457ecefe35806fde9d923421997861ae9391`. It maps
`torchvision::deform_conv2d` to ONNX's `DeformConv` (opset 19) and exports
the input size as free `h` × `w` (multiples of 64), which Lumenply pins
per run. Checked before adopting it:

- the same network: all 390 weight tensors (of 64 values or more) are
  bit-identical to onnx-community's export, matched by content hash; at
  1024² its mattes match that export with IoU 0.99968 (woman by a tree),
  0.99950 (curly hair), 0.99916 (dog), 0.99812 (cat) and 0.999997 (ball),
  mean differences 0.0001–0.0009;
- the file: IR 9 from PyTorch 2.14, only the default `ai.onnx` domain at
  opset 19 (15,942 nodes, 20 of them `DeformConv`, no other domains, no
  local functions, no custom operators, no metadata), no external data,
  437 fp32 initialisers totalling 178.5 MB (the largest 9.4 MB, a Swin MLP
  matrix), no string tensors; `onnx.checker` passes;
- the app's consent dialog shows its source as the registry has it: the
  re-export's repository, with the original named in the licence text.

If that repository disappears, the fallback is onnx-community's
BiRefNet_512x512-ONNX fp16 (commit `b0b30aff33d009f6bcd7dacdc3cdcf2f8f42175b`,
`onnx/model_fp16.onnx`, 473,498,113 bytes, SHA-256
`1b254749feb1edf83667a4c9da710cc1962a1980b8d0b2f4432100278c9ac0af`; fp32:
940,413,603 bytes, `617a11ca04cb13a2817bfad605969b4a9a040fceedbaaa46348a3957f0f6c254`),
or re-exporting ZhengPeng7/BiRefNet_lite ourselves with the export script
published in that repository.

**CoreML.** The re-export compiles (1305 of 1556 nodes, 42 partitions; the
`DeformConv`s stay on the CPU) and gives the CPU's matte exactly, at 0.28–0.32
s a run; but compiling takes 69 s, loading from the compile cache still
15 s, and it peaks 1.5 GB higher. The app loads the model for each matte,
so it runs on the CPU (0.7 s to load and 0.6 s to run). The 512x512 export
fails CoreML's compile in a third way (an fp16 `real_div` typed wrongly).

**Only self-contained models.** An ONNX tensor can keep its data in
another file, which ONNX Runtime then reads wherever the path points. Every
model is scanned before ONNX Runtime opens it (a streaming walk of the
protobuf: graphs, functions, nodes, attributes, subgraphs and tensors, the
weights skipped; 18 ms for the 181 MB file) and refused with
`AiError::ExternalData` if any tensor uses external data.

**Memory guard.** Each model file (and the high-detail run) carries its
measured run memory, the peak resident size of a run less that after
loading: 3.0 GB for BiRefNet at 768², 5.0 GB at 1024², 0.75 GB for
MobileSAM's encoder. Before a run, `lumenply_ai::memory::available()` (macOS:
`kern.memorystatus_level` of `hw.memsize`, the kernel's own count of
available memory, compressible pages included; Linux: `MemAvailable`;
Windows: `GlobalMemoryStatusEx`) must cover it, or the run fails with
`AiError::OutOfMemory` ("BiRefNet (high detail) needs about 5.0 GB of free
memory for a run; 4.2 GB is available"). Where the figure is unknown the
run goes ahead.

**API as built**, against the contract above: every fallible call returns
`Result<_, AiError>` (`embed`, `segment`, `matte`, `load`, `Runtime::new`,
`download`, `remove`). Additions: `Segmenter::predict` (the chosen
low-resolution logits and score), `segment_with` / `matte_with` (explicit
`RefineOptions`; `RefineOptions::OFF` for the plain upscale),
`Segmenter::load_files` / `Matter::load_file` (other exports, tests),
`Matter::load_detail(rt, store, Detail::High)` (1024² instead of 768²;
`ModelInfo::high_detail` describes it), `Prediction::matte`,
`provider()` and `reports()` per model (provider, node counts, fallbacks,
load time), `Embedding::{width, height, cell, key}`, `ModelStore::{default_dir,
verify, disk_bytes, model_dir, file_path}`, `ModelInfo::{task, source,
input_size, note}` and `ModelFile::avoid`. Prompt points use SAM's pixel
convention (`x, y` of a pixel; the model looks at its centre). With one
click, or a box alone, SAM's best-scored multimask output is kept; with
more prompts its single-mask output (SAM's own `select_masks` rule).
`LUMENPLY_AI_PROVIDER=cpu` forces a provider; `LUMENPLY_AI_LOG=info` prints
ONNX Runtime's log.

## Consequences

- The app grows by the ONNX Runtime library (about 20 MB stripped); models
  are downloaded separately (45 MB for selection, 181 MB for subjects).
- Subject selection needs about 3 GB of memory for under a second (5 GB at
  high detail), and is refused with a clear message when that isn't
  available.
- The subject model comes from a third-party re-export, verified against
  the original weights and pinned by hash; its source is shown as is.
- Results differ slightly between machines (CoreML vs CPU). Tests check
  pipelines with tolerances and with fixed tiny models, never exact pixels
  from the real models.
- The first use of a feature needs the network once; afterwards everything
  works offline.
