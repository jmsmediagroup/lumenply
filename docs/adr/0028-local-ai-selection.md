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
| Subject / background removal | BiRefNet lite | `onnx/model.onnx` 224 MB, fp32 (huggingface.co/onnx-community/BiRefNet_lite-ONNX); see the implementation notes for fp32 over fp16 | MIT |

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
- **Background removal**: the image is resized to 1024², normalised, run,
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

**fp32 BiRefNet.** The fp16 export's matte equals fp32's within one 8-bit
step, but on the CPU, where it runs on macOS, it is 7 % slower and peaks
1 GB higher (its casts); on x86 CPUs, which lack fp16 kernels, it would be
worse. The extra 109 MB of download is paid once.

**Memory.** BiRefNet's deformable convolution is exported as plain gathers
over `[1, 64, 49, 256, 256]` tensors (822 MB each): a run peaks at about
10.5 GB resident on the CPU (Python ONNX Runtime: 13.9 GB). Its session runs
without ONNX Runtime's CPU arena, which otherwise kept 13 GB for the
session's life; the app should drop the `Matter` after use and may warn on
machines under 16 GB. MobileSAM peaks at 0.95 GB (CoreML) or 1.2 GB (CPU) on
the demo photo, 2.9 GB on a 6000×4016 image (whose f32 raster alone is
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
  its coefficients are fitted at the model's 1024² against the image the
  model saw, where its matte matches the image, and evaluated at every
  full-resolution pixel. Edges land on the image's edges with no halo; 45 ms
  at 24 MP.

The guide is the image over white, sRGB-encoded at 8 bits (3 bytes a pixel,
kept in the embedding so a click needs no image).

**API as built**, against the contract above: every fallible call returns
`Result<_, AiError>` (`embed`, `segment`, `matte`, `load`, `Runtime::new`,
`download`, `remove`). Additions: `Segmenter::predict` (the chosen
low-resolution logits and score), `segment_with` / `matte_with` (explicit
`RefineOptions`; `RefineOptions::OFF` for the plain upscale),
`Segmenter::load_files` / `Matter::load_file` (other exports, tests),
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
  are downloaded separately (45 MB for selection, 224 MB for subjects).
- Subject selection needs about 10.5 GB of memory for a few seconds.
- Results differ slightly between machines (CoreML vs CPU). Tests check
  pipelines with tolerances and with fixed tiny models, never exact pixels
  from the real models.
- The first use of a feature needs the network once; afterwards everything
  works offline.
