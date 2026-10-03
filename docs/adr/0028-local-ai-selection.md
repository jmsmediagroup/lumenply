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
| Subject / background removal | BiRefNet lite | `model_fp16.onnx` 115 MB or `model.onnx` 224 MB (huggingface.co/onnx-community/BiRefNet_lite-ONNX) | MIT |

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

## Consequences

- The app grows by the ONNX Runtime library (tens of MB); models are
  downloaded separately (45 MB for selection, 115–224 MB for subjects).
- Results differ slightly between machines (CoreML vs CPU). Tests check
  pipelines with tolerances and with fixed tiny models, never exact pixels
  from the real models.
- The first use of a feature needs the network once; afterwards everything
  works offline.
