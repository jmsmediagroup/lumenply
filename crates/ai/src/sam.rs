//! Click-to-select with MobileSAM (Object Selection).
//!
//! The image is resized so its longer side is 1024 (SAM's
//! `ResizeLongestSide`), padded to 1024² with SAM's mean colour and
//! encoded once into a 256×64×64 embedding, kept by the image's content
//! key so later clicks only run the decoder. The decoder takes points
//! (label 1 include, 0 exclude) and a box (its corners labelled 2 and 3)
//! in the 1024 frame, and answers four candidate masks as 256×256 logits
//! over the padded frame with a predicted IoU each. SAM's own rule picks
//! one; its logits are upscaled straight to the image (a logit grid cell
//! spans 4 frame pixels), thresholded at 0 and refined against the image.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lumenply_tiles::Raster;
use ort::value::TensorRef;

use crate::prep::{content_key, longest_side, resize_raster, sam_input, upsample};
use crate::refine::{refine_guided, Guide, RefineOptions};
use crate::registry::{ModelFile, ModelId};
use crate::runtime::{Load, Model, ModelReport, Runtime};
use crate::store::ModelStore;
use crate::{AiError, Matte, Result};

/// The encoder's frame (pixels per side).
pub const FRAME: u32 = 1024;
/// The decoder's mask grid (cells per side; one cell is 4 frame pixels).
pub const GRID: usize = 256;
/// Embedding shape: `[1, 256, 64, 64]`.
const EMBED_SHAPE: [usize; 4] = [1, 256, 64, 64];
/// Embeddings kept for repeated clicks (one per image).
const CACHE: usize = 3;

/// What the user pointed at, in image pixel coordinates as SAM's
/// predictor takes them: a point is a pixel's `(x, y)` (the model looks at
/// its centre), a box runs between two such points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prompt {
    /// A click: include (`positive`) or exclude the object there.
    Point { x: f32, y: f32, positive: bool },
    /// A box around the object (corners in any order). At most one.
    Box { x0: f32, y0: f32, x1: f32, y1: f32 },
}

/// The encoder's view of one image, ready for any number of prompts.
/// Cheap to clone.
#[derive(Clone)]
pub struct Embedding {
    inner: Arc<EmbeddingInner>,
}

struct EmbeddingInner {
    key: [u8; 32],
    features: Vec<f32>,
    width: u32,
    height: u32,
    /// The image's size inside the 1024 frame.
    resized: (u32, u32),
    guide: Guide,
}

impl Embedding {
    pub fn width(&self) -> u32 {
        self.inner.width
    }

    pub fn height(&self) -> u32 {
        self.inner.height
    }

    /// The image's content key (blake3 of its size and pixels).
    pub fn key(&self) -> [u8; 32] {
        self.inner.key
    }

    /// The encoder's output, `[1, 256, 64, 64]` row-major.
    pub fn features(&self) -> &[f32] {
        &self.inner.features
    }

    /// The same encoder run (a cache hit hands out the same embedding).
    pub fn is_same(&self, other: &Embedding) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Image pixels per cell of the decoder's mask grid.
    pub fn cell(&self) -> f32 {
        let (rw, rh) = self.inner.resized;
        let sx = self.inner.width as f32 / rw as f32;
        let sy = self.inner.height as f32 / rh as f32;
        4.0 * sx.max(sy)
    }
}

impl std::fmt::Debug for Embedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Embedding({}×{})", self.inner.width, self.inner.height)
    }
}

/// The decoder's chosen mask before upscaling.
#[derive(Clone, Debug, PartialEq)]
pub struct Prediction {
    /// 256×256 logits over the padded 1024 frame (> 0 is inside).
    pub logits: Vec<f32>,
    /// The decoder's predicted IoU of this mask.
    pub score: f32,
    /// Which of the four candidates it is (0: the single-mask output).
    pub index: usize,
}

impl Prediction {
    /// This mask at `emb`'s image resolution: [`RefineOptions::OFF`] for a
    /// quick look, then refined without decoding again.
    pub fn matte(&self, emb: &Embedding, opts: RefineOptions) -> Matte {
        upscale_prediction(emb, self, opts)
    }
}

/// Prompts as the decoder takes them: `(coords, labels)` with `coords`
/// `[x, y]` pairs in the 1024 frame. Points keep their order, the box's
/// corners follow (labels 2 and 3); without a box SAM's padding point
/// `(0, 0)` labelled −1 ends the list.
pub(crate) fn encode_prompts(
    prompts: &[Prompt],
    width: u32,
    height: u32,
    resized: (u32, u32),
) -> Result<(Vec<f32>, Vec<f32>)> {
    if prompts.is_empty() {
        return Err(AiError::Invalid("nothing to select: no point or box".into()));
    }
    let sx = resized.0 as f32 / width as f32;
    let sy = resized.1 as f32 / height as f32;
    let (mut coords, mut labels) = (Vec::new(), Vec::new());
    let mut boxed = None;
    for p in prompts {
        match *p {
            Prompt::Point { x, y, positive } => {
                if !(x.is_finite() && y.is_finite()) {
                    return Err(AiError::Invalid("a point is not a number".into()));
                }
                coords.extend([x * sx, y * sy]);
                labels.push(if positive { 1.0 } else { 0.0 });
            }
            Prompt::Box { x0, y0, x1, y1 } => {
                if boxed.is_some() {
                    return Err(AiError::Invalid("only one box per selection".into()));
                }
                if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
                    return Err(AiError::Invalid("a box is not a number".into()));
                }
                boxed = Some([x0.min(x1) * sx, y0.min(y1) * sy, x0.max(x1) * sx, y0.max(y1) * sy]);
            }
        }
    }
    match boxed {
        Some([a, b, c, d]) => {
            coords.extend([a, b, c, d]);
            labels.extend([2.0, 3.0]);
        }
        None => {
            coords.extend([0.0, 0.0]);
            labels.push(-1.0);
        }
    }
    Ok((coords, labels))
}

/// SAM's choice among the four candidates (`SamOnnxModel.select_masks`):
/// with two or fewer decoder points (one click plus padding, or a box
/// alone) the prompt is ambiguous and the best-scored of the three
/// multimask outputs (1..=3) wins; with more, the single-mask output 0.
pub(crate) fn pick_mask(scores: &[f32], decoder_points: usize) -> usize {
    if decoder_points > 2 || scores.len() < 2 {
        return 0;
    }
    (1..scores.len())
        .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .expect("at least two scores")
}

/// MobileSAM's encoder and decoder.
pub struct Segmenter {
    encoder: Model,
    decoder: Model,
    cache: Mutex<VecDeque<Embedding>>,
}

impl Segmenter {
    /// Load MobileSAM from the store.
    pub fn load(rt: &Runtime, store: &ModelStore) -> Result<Segmenter> {
        let id = ModelId::MobileSam;
        if !store.installed(id) {
            return Err(AiError::NotInstalled(id.to_string()));
        }
        let info = id.info();
        let cache = store.compile_cache(id);
        let [enc, dec] = [&info.files[0], &info.files[1]];
        Segmenter::open(
            rt,
            (&store.file_path(id, enc), enc),
            (&store.file_path(id, dec), dec),
            Some(&cache),
        )
    }

    /// Load from explicit files (tests, tools), on any provider, without
    /// the memory check.
    pub fn load_files(rt: &Runtime, encoder: &Path, decoder: &Path) -> Result<Segmenter> {
        const ANY: ModelFile = ModelFile {
            name: "",
            url: "",
            bytes: 0,
            sha256: "",
            avoid: &[],
            run_bytes: 0,
        };
        Segmenter::open(rt, (encoder, &ANY), (decoder, &ANY), None)
    }

    fn open(
        rt: &Runtime,
        (encoder, enc_file): (&Path, &ModelFile),
        (decoder, dec_file): (&Path, &ModelFile),
        cache: Option<&Path>,
    ) -> Result<Segmenter> {
        // The encoder's input is `[image_height, image_width, 3]`; it is
        // always fed the padded frame, and CoreML only compiles it with
        // those sizes fixed.
        let frame = FRAME as i64;
        let name = ModelId::MobileSam.info().name;
        let enc = Load {
            avoid: enc_file.avoid,
            cache,
            dims: &[("image_height", frame), ("image_width", frame)],
            arena: true,
            name,
            run_bytes: enc_file.run_bytes,
        };
        let dec = Load {
            avoid: dec_file.avoid,
            dims: &[],
            run_bytes: dec_file.run_bytes,
            ..enc
        };
        Ok(Segmenter {
            encoder: rt.load(encoder, enc)?,
            decoder: rt.load(decoder, dec)?,
            cache: Mutex::new(VecDeque::new()),
        })
    }

    /// What each half runs on: "CoreML (… nodes)" for encoder and decoder.
    pub fn provider(&self) -> String {
        let (e, d) = (self.encoder.describe(), self.decoder.describe());
        if e == d {
            e
        } else {
            format!("encoder {e}, decoder {d}")
        }
    }

    /// Details of the encoder and decoder sessions.
    pub fn reports(&self) -> [ModelReport; 2] {
        [(&self.encoder).into(), (&self.decoder).into()]
    }

    /// The embedding of `image`, from the cache when the same pixels were
    /// embedded recently.
    pub fn embed(&self, image: &Raster) -> Result<Embedding> {
        if image.width == 0 || image.height == 0 {
            return Err(AiError::Invalid("the image is empty".into()));
        }
        let key = content_key(image);
        if let Some(hit) = self.cached(&key) {
            return Ok(hit);
        }
        let (rw, rh) = longest_side(image.width, image.height, FRAME);
        let small = resize_raster(image, rw as usize, rh as usize);
        let input = sam_input(&small, rw as usize, rh as usize, FRAME as usize);
        let f = FRAME as usize;
        let features = self.encoder.with(|s| {
            let t = TensorRef::from_array_view(([f, f, 3], &input[..]))?;
            let out = s.run(ort::inputs!["input_image" => t])?;
            let (shape, data) = out["image_embeddings"].try_extract_tensor::<f32>()?;
            let want: Vec<i64> = EMBED_SHAPE.iter().map(|&v| v as i64).collect();
            if shape[..] != want[..] {
                return Err(AiError::Model(format!(
                    "image_embeddings is {shape:?}, expected {want:?}"
                )));
            }
            Ok(data.to_vec())
        })?;
        let emb = Embedding {
            inner: Arc::new(EmbeddingInner {
                key,
                features,
                width: image.width,
                height: image.height,
                resized: (rw, rh),
                guide: Guide::new(image),
            }),
        };
        if let Ok(mut c) = self.cache.lock() {
            c.push_front(emb.clone());
            c.truncate(CACHE);
        }
        Ok(emb)
    }

    /// The cached embedding of `image`, if it was embedded recently: lets
    /// a caller tell the user an image is about to be analysed (the slow
    /// part) only when it really is.
    pub fn cached_embedding(&self, image: &Raster) -> Option<Embedding> {
        self.cached(&content_key(image))
    }

    fn cached(&self, key: &[u8; 32]) -> Option<Embedding> {
        let mut c = self.cache.lock().ok()?;
        let at = c.iter().position(|e| &e.inner.key == key)?;
        let hit = c.remove(at)?;
        c.push_front(hit.clone());
        Some(hit)
    }

    /// The decoder's chosen mask for `prompts` (low resolution).
    pub fn predict(&self, emb: &Embedding, prompts: &[Prompt]) -> Result<Prediction> {
        let e = &emb.inner;
        let (coords, labels) = encode_prompts(prompts, e.width, e.height, e.resized)?;
        let n = labels.len();
        let mask_input = vec![0f32; GRID * GRID];
        // The `masks` output is upscaled to this size inside the model;
        // the frame size keeps that cheap (the logits are used instead).
        let size = [e.resized.1 as f32, e.resized.0 as f32];
        let (scores, logits) = self.decoder.with(|s| {
            let out = s.run(ort::inputs![
                "image_embeddings" => TensorRef::from_array_view((EMBED_SHAPE, &e.features[..]))?,
                "point_coords" => TensorRef::from_array_view(([1, n, 2], &coords[..]))?,
                "point_labels" => TensorRef::from_array_view(([1, n], &labels[..]))?,
                "mask_input" => TensorRef::from_array_view(([1, 1, GRID, GRID], &mask_input[..]))?,
                "has_mask_input" => TensorRef::from_array_view(([1], &[0f32][..]))?,
                "orig_im_size" => TensorRef::from_array_view(([2], &size[..]))?,
            ])?;
            let (_, scores) = out["iou_predictions"].try_extract_tensor::<f32>()?;
            let (shape, logits) = out["low_res_masks"].try_extract_tensor::<f32>()?;
            let k = scores.len();
            if shape[..] != [1, k as i64, GRID as i64, GRID as i64] || k == 0 {
                return Err(AiError::Model(format!(
                    "low_res_masks is {shape:?} for {k} scores, expected [1, {k}, {GRID}, {GRID}]"
                )));
            }
            Ok((scores.to_vec(), logits.to_vec()))
        })?;
        let index = pick_mask(&scores, n);
        Ok(Prediction {
            logits: logits[index * GRID * GRID..(index + 1) * GRID * GRID].to_vec(),
            score: scores[index],
            index,
        })
    }

    /// The selection `prompts` make: full resolution, refined with
    /// [`RefineOptions::selection`].
    pub fn segment(&self, emb: &Embedding, prompts: &[Prompt]) -> Result<Matte> {
        self.segment_with(emb, prompts, RefineOptions::selection(emb.cell()))
    }

    /// [`Segmenter::segment`] with other refinement ([`RefineOptions::OFF`]
    /// gives the plain thresholded upscale).
    pub fn segment_with(&self, emb: &Embedding, prompts: &[Prompt], opts: RefineOptions) -> Result<Matte> {
        let pred = self.predict(emb, prompts)?;
        Ok(upscale_prediction(emb, &pred, opts))
    }

    /// Time the encoder and the decoder for `prompts` (milliseconds of each
    /// run), bypassing the embedding cache: for measurements.
    pub fn bench(&self, image: &Raster, prompts: &[Prompt], runs: usize) -> Result<(Vec<f64>, Vec<f64>)> {
        let (mut enc, mut dec) = (Vec::new(), Vec::new());
        for _ in 0..runs.max(1) {
            if let Ok(mut c) = self.cache.lock() {
                c.clear();
            }
            let t = Instant::now();
            let emb = self.embed(image)?;
            enc.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            self.predict(&emb, prompts)?;
            dec.push(t.elapsed().as_secs_f64() * 1e3);
        }
        Ok((enc, dec))
    }
}

/// The prediction at the image's resolution: logits upscaled bilinearly
/// (an image pixel centre maps through the resize into the 256 grid),
/// thresholded at 0, refined.
pub(crate) fn upscale_prediction(emb: &Embedding, pred: &Prediction, opts: RefineOptions) -> Matte {
    let e = &emb.inner;
    let (w, h) = (e.width as usize, e.height as usize);
    let kx = e.resized.0 as f64 / e.width as f64 / 4.0;
    let ky = e.resized.1 as f64 / e.height as f64 / 4.0;
    let logits = upsample(&pred.logits, GRID, GRID, w, h, kx, ky);
    let hard = Matte {
        width: e.width,
        height: e.height,
        alpha: logits.iter().map(|&l| if l > 0.0 { 1.0 } else { 0.0 }).collect(),
    };
    refine_guided(&e.guide, &hard, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    #[test]
    fn prompts_scale_into_the_frame_with_sam_labels() {
        // 1800×1205 → 1024×686: x × 1024/1800, y × 686/1205.
        let (c, l) = encode_prompts(
            &[Prompt::Point {
                x: 900.0,
                y: 602.5,
                positive: true,
            }],
            1800,
            1205,
            (1024, 686),
        )
        .unwrap();
        assert_eq!(l, vec![1.0, -1.0], "a click, then SAM's padding point");
        assert_eq!(c[0], 512.0);
        assert!((c[1] - 343.0).abs() < 1e-4, "{}", c[1]);
        assert_eq!(&c[2..], &[0.0, 0.0]);

        // An exclude click and a box given corner-swapped: the box's
        // corners come last, labelled 2 and 3, and no padding point.
        let (c, l) = encode_prompts(
            &[
                Prompt::Point {
                    x: 100.0,
                    y: 50.0,
                    positive: false,
                },
                Prompt::Box {
                    x0: 400.0,
                    y0: 300.0,
                    x1: 200.0,
                    y1: 100.0,
                },
            ],
            800,
            400,
            (1024, 512),
        )
        .unwrap();
        assert_eq!(l, vec![0.0, 2.0, 3.0]);
        assert_eq!(c, vec![128.0, 64.0, 256.0, 128.0, 512.0, 384.0]);

        assert!(encode_prompts(&[], 10, 10, (1024, 1024)).is_err());
        let b = Prompt::Box {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        };
        assert!(encode_prompts(&[b, b], 10, 10, (1024, 1024)).is_err());
    }

    #[test]
    fn sam_picks_a_multimask_output_only_for_ambiguous_prompts() {
        let scores = [0.83, 0.90, 0.88, 0.93];
        // One click (+ padding) or a box alone: best of 1..=3.
        assert_eq!(pick_mask(&scores, 2), 3);
        // Two clicks (+ padding), or a click and a box: the single mask.
        assert_eq!(pick_mask(&scores, 3), 0);
        assert_eq!(pick_mask(&[0.5, 0.9, 0.95, 0.1], 2), 2);
        assert_eq!(pick_mask(&[0.7], 2), 0);
    }

    /// An embedding of a `w`×`h` image without running the encoder.
    fn fake_embedding(w: u32, h: u32, guide: &Raster) -> Embedding {
        Embedding {
            inner: Arc::new(EmbeddingInner {
                key: [0; 32],
                features: Vec::new(),
                width: w,
                height: h,
                resized: longest_side(w, h, FRAME),
                guide: Guide::new(guide),
            }),
        }
    }

    #[test]
    fn logits_upscale_through_the_resize_and_threshold_at_zero() {
        // A 2048×1024 image sits in the frame at 1024×512, so one grid
        // cell is 8×8 image pixels. Logits positive in grid columns
        // 0..=9 (frame x < 40, image x < 80) and grid rows < 64 (image
        // y < 512).
        let (w, h) = (2048u32, 1024u32);
        let mut logits = vec![-10.0f32; GRID * GRID];
        for gy in 0..64 {
            for gx in 0..10 {
                logits[gy * GRID + gx] = 10.0;
            }
        }
        let pred = Prediction {
            logits,
            score: 1.0,
            index: 1,
        };
        let emb = fake_embedding(w, h, &Raster::filled(w, h, Rgba::WHITE));
        assert_eq!(emb.cell(), 8.0);
        let m = upscale_prediction(&emb, &pred, RefineOptions::OFF);
        assert_eq!((m.width, m.height), (w, h));
        // Image x maps to grid (x + ½)/8 − ½: the sign change between
        // cells 9 and 10 (−9.5 … +9.5 linearly) lands at grid 9.5, i.e.
        // image x = 80.
        let at = |x: u32, y: u32| m.alpha[(y * w + x) as usize];
        assert_eq!((at(79, 100), at(80, 100)), (1.0, 0.0));
        assert_eq!((at(40, 511), at(40, 512)), (1.0, 0.0));
        assert_eq!(at(0, 0), 1.0);
        assert_eq!(at(2047, 1023), 0.0);
        // 80 × 512 pixels, less the 6 nearest the corner (80, 512): the
        // zero line of bilinear interpolation rounds the corner off.
        let covered: f32 = m.alpha.iter().sum();
        assert_eq!(covered, 80.0 * 512.0 - 6.0);
        let cut = [(77, 511), (78, 510), (78, 511), (79, 509), (79, 510), (79, 511)];
        assert!(cut.iter().all(|&(x, y)| at(x, y) == 0.0));
        assert_eq!((at(76, 511), at(79, 508), at(77, 510)), (1.0, 1.0, 1.0));
    }
}
