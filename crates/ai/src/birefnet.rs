//! Subject matte with BiRefNet lite (Select Subject, Remove Background).
//!
//! The image is resized to 1024² (aspect not kept, as the model was
//! trained), sRGB-encoded and normalised with ImageNet's mean and
//! deviation (the model's `preprocessor_config.json`), NCHW. The model
//! answers 1024² logits; their sigmoid is the matte at the model's
//! resolution, carried to the image's by the guided filter used as an
//! upsampler (its local colour model fitted where the matte matches the
//! image the model saw, then applied to the full-resolution colours).

use std::path::Path;

use lumenply_tiles::Raster;
use ort::value::TensorRef;

use lumenply_core::refine::{global_refine, RefineParams};

use crate::prep::{encode_srgb, imagenet_nchw, resize_raster, sigmoid, upsample};
use crate::refine::{guided_upsample, refine, Guide, RefineOptions};
use crate::registry::ModelId;
use crate::runtime::{Load, Model, ModelReport, Runtime};
use crate::store::ModelStore;
use crate::{AiError, Matte, Result};

/// The model's input side.
pub const SIZE: usize = 1024;

/// BiRefNet lite.
pub struct Matter {
    model: Model,
    size: usize,
}

impl Matter {
    /// Load BiRefNet lite from the store.
    pub fn load(rt: &Runtime, store: &ModelStore) -> Result<Matter> {
        let id = ModelId::BiRefNetLite;
        if !store.installed(id) {
            return Err(AiError::NotInstalled(id.to_string()));
        }
        let info = id.info();
        let path = store.file_path(id, &info.files[0]);
        let cache = store.compile_cache(id);
        Ok(Matter {
            model: rt.load(
                &path,
                Load {
                    avoid: info.files[0].avoid,
                    cache: Some(&cache),
                    dims: &[],
                    arena: false,
                },
            )?,
            size: SIZE,
        })
    }

    /// Load from an explicit file whose input is `size`² (tests, tools),
    /// on any provider.
    pub fn load_file(rt: &Runtime, path: &Path, size: usize) -> Result<Matter> {
        Ok(Matter {
            model: rt.load(
                path,
                Load {
                    arena: false,
                    ..Load::default()
                },
            )?,
            size,
        })
    }

    /// What the model runs on, e.g. "CoreML (… nodes)".
    pub fn provider(&self) -> String {
        self.model.describe()
    }

    pub fn report(&self) -> ModelReport {
        (&self.model).into()
    }

    /// The model's matte of `image` at its own resolution: `size`²
    /// coverage values (the sigmoid of its logits).
    pub fn predict(&self, image: &Raster) -> Result<Vec<f32>> {
        Ok(self.run(image)?.1)
    }

    /// The image as the model saw it (linear, `size`²) and its matte.
    fn run(&self, image: &Raster) -> Result<(Vec<[f32; 3]>, Vec<f32>)> {
        if image.width == 0 || image.height == 0 {
            return Err(AiError::Invalid("the image is empty".into()));
        }
        let n = self.size;
        let small = resize_raster(image, n, n);
        let input = imagenet_nchw(&small);
        let matte = self.model.with(|s| {
            let t = TensorRef::from_array_view(([1, 3, n, n], &input[..]))?;
            let out = s.run(ort::inputs!["input_image" => t])?;
            let (shape, data) = out["output_image"].try_extract_tensor::<f32>()?;
            if shape[..] != [1, 1, n as i64, n as i64] {
                return Err(AiError::Model(format!(
                    "output_image is {shape:?}, expected [1, 1, {n}, {n}]"
                )));
            }
            Ok(data.iter().map(|&v| sigmoid(v)).collect())
        })?;
        Ok((small, matte))
    }

    /// Image pixels per model pixel (the longer side's).
    fn scale(&self, image: &Raster) -> f32 {
        image.width.max(image.height) as f32 / self.size as f32
    }

    /// The subject's matte at `image`'s resolution, refined with
    /// [`RefineOptions::matte`].
    pub fn matte(&self, image: &Raster) -> Result<Matte> {
        self.matte_with(image, RefineOptions::matte(self.scale(image)))
    }

    /// [`Matter::matte`] with other refinement. Without `matting` the
    /// guided filter runs as an upsampler (see `refine::guided_upsample`):
    /// fitted at the model's resolution in windows of `opts.window` image
    /// pixels (at least one model pixel), evaluated at the image's. With
    /// `matting` the plain upscale goes through [`refine`].
    /// [`RefineOptions::OFF`] gives the plain upscale.
    pub fn matte_with(&self, image: &Raster, opts: RefineOptions) -> Result<Matte> {
        let (small, matte) = self.run(image)?;
        let (w, h) = (image.width, image.height);
        if opts.radius <= 0.0 || !opts.radius.is_finite() {
            return Ok(self.upscale(&matte, w, h));
        }
        if opts.matting {
            return Ok(refine(image, &self.upscale(&matte, w, h), opts));
        }
        let n = self.size;
        let low: Vec<[f32; 3]> = small.iter().map(|c| c.map(encode_srgb)).collect();
        let r = ((opts.window as f32 / self.scale(image)).round() as usize).max(1);
        let mut alpha = guided_upsample(&low, &matte, n, n, r, opts.eps.max(1e-8), &Guide::new(image));
        if opts.contrast > 0.0 {
            let params = RefineParams {
                contrast: opts.contrast,
                ..RefineParams::default()
            };
            alpha = global_refine(alpha, w as usize, h as usize, &params, 1.0);
        }
        snap(&mut alpha);
        Ok(Matte {
            width: w,
            height: h,
            alpha,
        })
    }

    /// The model's `size`² matte stretched back over a `w`×`h` image.
    pub(crate) fn upscale(&self, small: &[f32], w: u32, h: u32) -> Matte {
        let n = self.size;
        let mut alpha = upsample(
            small,
            n,
            n,
            w as usize,
            h as usize,
            n as f64 / w as f64,
            n as f64 / h as f64,
        );
        snap(&mut alpha);
        Matte {
            width: w,
            height: h,
            alpha,
        }
    }
}

/// A sigmoid never reaches 0 or 1: values within half an 8-bit step of
/// them become exact, so the background is truly clear.
fn snap(alpha: &mut [f32]) {
    const SNAP: f32 = 0.5 / 255.0;
    for a in alpha {
        *a = if *a < SNAP {
            0.0
        } else if *a > 1.0 - SNAP {
            1.0
        } else {
            *a
        };
    }
}
