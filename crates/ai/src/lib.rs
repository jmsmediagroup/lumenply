//! Local AI selection and masking (ADR 0028): ONNX models run through ONNX
//! Runtime on the platform's accelerator, downloaded on first use.
//!
//! - [`Segmenter`] (MobileSAM): click or box to select an object. The
//!   image is embedded once ([`Segmenter::embed`], cached by content), then
//!   every set of [`Prompt`]s only runs the small decoder.
//! - [`Matter`] (BiRefNet lite): the main subject's matte (Select Subject,
//!   Remove Background).
//! - [`refine`]: fits an upscaled mask to the full-resolution image's
//!   edges with Select and Mask's engine; both models' results come back
//!   refined.
//! - [`ModelStore`] downloads the pinned, hash-checked files ([`models`]
//!   lists them); [`Runtime`] picks the execution provider.
//!
//! Loading and running take from milliseconds (a click) to seconds (an
//! embedding on the CPU, a subject matte): call them off the UI thread.
//! [`Segmenter`], [`Matter`], [`Embedding`], [`Runtime`] and [`ModelStore`]
//! are `Send + Sync`; each model runs one call at a time.
//!
//! Engine crate: no window, GPU context or UI toolkit; images never leave
//! the machine.

mod birefnet;
mod error;
pub mod memory;
mod onnx_check;
mod prep;
mod refine;
mod registry;
mod runtime;
mod sam;
mod store;

pub use birefnet::{Detail, Matter};
pub use error::{AiError, Result};
pub use refine::{refine, RefineOptions};
pub use registry::{models, HighDetail, ModelFile, ModelId, ModelInfo};
pub use runtime::{ModelReport, Placement, Provider, Runtime, LOG_ENV, PROVIDER_ENV};
pub use sam::{Embedding, Prediction, Prompt, Segmenter};
pub use store::{sha256_file, ModelStore};

/// A coverage mask over an image: 0 outside, 1 inside, partial at soft
/// edges. Row-major, one value per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<f32>,
}

impl Matte {
    pub fn new(width: u32, height: u32, alpha: Vec<f32>) -> Matte {
        assert_eq!(alpha.len(), width as usize * height as usize, "matte size");
        Matte { width, height, alpha }
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> f32 {
        self.alpha[y as usize * self.width as usize + x as usize]
    }

    /// Mean coverage of the rectangle `[x0, x1) × [y0, y1)` (clipped to
    /// the matte; 0 when empty).
    pub fn coverage(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return 0.0;
        }
        let mut sum = 0f64;
        for y in y0..y1 {
            let row = &self.alpha[(y * self.width) as usize..((y + 1) * self.width) as usize];
            sum += row[x0 as usize..x1 as usize]
                .iter()
                .map(|&v| v as f64)
                .sum::<f64>();
        }
        (sum / ((x1 - x0) as f64 * (y1 - y0) as f64)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_can_share_everything_across_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Segmenter>();
        shared::<Matter>();
        shared::<Embedding>();
        shared::<Runtime>();
        shared::<ModelStore>();
        shared::<Matte>();
    }

    #[test]
    fn matte_coverage_averages_a_clipped_rectangle() {
        let m = Matte::new(4, 2, vec![1.0, 1.0, 0.0, 0.0, 1.0, 0.5, 0.0, 0.0]);
        assert_eq!(m.get(1, 1), 0.5);
        assert_eq!(m.coverage(0, 0, 2, 2), 0.875);
        assert_eq!(m.coverage(2, 0, 9, 9), 0.0);
        assert_eq!(m.coverage(0, 0, 4, 2), 3.5 / 8.0);
        assert_eq!(m.coverage(3, 0, 3, 2), 0.0);
    }
}
