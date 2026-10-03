//! Local AI selection and masking, the app's side of ADR 0028: the small
//! service the features call, mirroring `lumenply-ai`'s API, plus the
//! stand-ins used until that engine is wired in.
//!
//! - [`AiService`] is what Object Selection, Select ▸ Subject, Layer ▸
//!   Remove Background and Preferences ▸ AI models need. The real
//!   implementation is a thin adapter over `ModelStore`, `Runtime`,
//!   `Segmenter` and `Matter`.
//! - [`NoEngine`] is the service in builds without the engine: the models
//!   are listed, nothing can run, and the UI says so.
//! - [`FakeAi`] answers instantly (or with configurable delays) from simple
//!   colour rules, for tests and headless screenshots (`ai:fake`).
//!
//! Every call blocks; the app runs them on worker threads (`ai_jobs.rs`).

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use lumenply_tiles::Raster;

/// The models the features use (`lumenply_ai::ModelId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ModelKey {
    /// MobileSAM: Object Selection (click and box prompts).
    MobileSam,
    /// BiRefNet lite: Select ▸ Subject and Layer ▸ Remove Background.
    BiRefNetLite,
}

impl ModelKey {
    pub(crate) const ALL: [ModelKey; 2] = [ModelKey::MobileSam, ModelKey::BiRefNetLite];

    /// Short name for debug tokens (`ai:consent=sam`).
    pub(crate) fn slug(self) -> &'static str {
        match self {
            ModelKey::MobileSam => "sam",
            ModelKey::BiRefNetLite => "birefnet",
        }
    }

    pub(crate) fn from_slug(s: &str) -> Option<ModelKey> {
        ModelKey::ALL.into_iter().find(|m| m.slug() == s)
    }
}

/// One model as the UI shows it (`lumenply_ai::ModelInfo` plus the store's
/// installed state).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelInfoView {
    pub key: ModelKey,
    /// "MobileSAM".
    pub name: String,
    /// What it is for, in menu words: "Object Selection".
    pub purpose: String,
    /// Download size of all its files.
    pub bytes: u64,
    /// Where the files come from: "huggingface.co/Acly/MobileSAM".
    pub source: String,
    /// The weights' licence: "Apache-2.0".
    pub licence: String,
    pub installed: bool,
}

/// A Segmenter prompt in image pixels (`lumenply_ai::Prompt`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AiPrompt {
    /// A click: on the object (`positive`) or on what to leave out.
    Point { x: f32, y: f32, positive: bool },
    /// A box drawn around the object.
    Box { x0: f32, y0: f32, x1: f32, y1: f32 },
}

/// What the AI features need from the engine. Images are the editor's own
/// rasters (premultiplied, linear light, canvas-sized); results are one
/// coverage value per pixel, row-major, at the image's full resolution,
/// already upscaled and edge-refined.
pub(crate) trait AiService: Send + Sync {
    /// Every model with its size, source, licence and installed state.
    fn models(&self) -> Vec<ModelInfoView>;
    /// Download, verify and install a model. `progress(done, total)` in
    /// bytes; stops early with an error once `cancel` is set, leaving
    /// nothing installed.
    fn download(
        &self,
        model: ModelKey,
        progress: &dyn Fn(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<(), String>;
    /// Delete an installed model's files.
    fn remove(&self, model: ModelKey) -> Result<(), String>;
    /// The execution provider in use: "CoreML", "DirectML", "CUDA", "CPU".
    fn provider(&self) -> String;
    /// Why nothing can run at all (no engine in this build, the runtime
    /// failed to load), or `None` when the features can run.
    fn unavailable(&self) -> Option<String> {
        None
    }
    /// Object Selection (MobileSAM): the mask for these prompts. Calls
    /// `encoding()` first when the image has to be encoded (the slow part;
    /// later calls on the same image reuse the cached embedding).
    fn select(&self, image: &Raster, prompts: &[AiPrompt], encoding: &dyn Fn()) -> Result<Vec<f32>, String>;
    /// Subject matte (BiRefNet): the foreground's coverage.
    fn matte(&self, image: &Raster) -> Result<Vec<f32>, String>;
}

/// The models of ADR 0028, as the UI lists them; nothing installed.
pub(crate) fn catalogue() -> Vec<ModelInfoView> {
    vec![
        ModelInfoView {
            key: ModelKey::MobileSam,
            name: "MobileSAM".into(),
            purpose: "Object Selection".into(),
            bytes: 44_500_000,
            source: "huggingface.co/Acly/MobileSAM".into(),
            licence: "Apache-2.0".into(),
            installed: false,
        },
        ModelInfoView {
            key: ModelKey::BiRefNetLite,
            name: "BiRefNet lite".into(),
            purpose: "Select Subject, Remove Background".into(),
            bytes: 115_000_000,
            source: "huggingface.co/onnx-community/BiRefNet_lite-ONNX".into(),
            licence: "MIT".into(),
            installed: false,
        },
    ]
}

/// "44.5 MB", "115 MB", "820 KB" (decimal units, as download sizes go).
pub(crate) fn format_bytes(n: u64) -> String {
    let mb = n as f64 / 1e6;
    if n < 1_000_000 {
        format!("{:.0} KB", n as f64 / 1e3)
    } else if mb < 100.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{mb:.0} MB")
    }
}

/// The service in a build without the engine: lists the models, runs
/// nothing.
pub(crate) struct NoEngine;

const NO_ENGINE: &str = "This build of Lumenply doesn't include the AI engine";

impl AiService for NoEngine {
    fn models(&self) -> Vec<ModelInfoView> {
        catalogue()
    }
    fn download(&self, _: ModelKey, _: &dyn Fn(u64, u64), _: &AtomicBool) -> Result<(), String> {
        Err(NO_ENGINE.into())
    }
    fn remove(&self, _: ModelKey) -> Result<(), String> {
        Err(NO_ENGINE.into())
    }
    fn provider(&self) -> String {
        "none".into()
    }
    fn unavailable(&self) -> Option<String> {
        Some(NO_ENGINE.into())
    }
    fn select(&self, _: &Raster, _: &[AiPrompt], _: &dyn Fn()) -> Result<Vec<f32>, String> {
        Err(NO_ENGINE.into())
    }
    fn matte(&self, _: &Raster) -> Result<Vec<f32>, String> {
        Err(NO_ENGINE.into())
    }
}

/// A stand-in engine with simple, exact rules:
///
/// - `select`: a click floods the 4-connected region whose colour is
///   within [`FakeAi::TOLERANCE`] of the clicked pixel's (a negative click
///   removes its region); a box takes every pixel inside it that differs
///   from the mean colour along the box's border. Values are 0 or 1.
/// - `matte`: the subject is whatever differs from the mean colour along
///   the image border, ramping from 0 at a colour distance of 0.15 to 1
///   at 0.3 (straight sRGB-encoded RGB, transparent pixels 0).
/// - `download` ticks `steps` times, `tick` apart, honouring cancel.
pub(crate) struct FakeAi {
    pub(crate) installed: Mutex<HashSet<ModelKey>>,
    /// Content keys of the images already "encoded".
    pub(crate) encoded: Mutex<HashSet<u64>>,
    /// Downloads fail as if there were no network.
    pub(crate) offline: AtomicBool,
    pub(crate) steps: u32,
    pub(crate) tick: Duration,
    /// Extra time the first `select` on an image takes (encoding).
    pub(crate) encode_delay: Duration,
    /// Time every `select` and `matte` takes.
    pub(crate) run_delay: Duration,
}

impl FakeAi {
    /// Colour distance (sRGB-encoded, 0–1 per channel) a click spreads over.
    pub(crate) const TOLERANCE: f32 = 0.12;

    /// Instant answers with `installed` already installed.
    pub(crate) fn new(installed: &[ModelKey]) -> FakeAi {
        FakeAi {
            installed: Mutex::new(installed.iter().copied().collect()),
            encoded: Mutex::new(HashSet::new()),
            offline: AtomicBool::new(false),
            steps: 4,
            tick: Duration::ZERO,
            encode_delay: Duration::ZERO,
            run_delay: Duration::ZERO,
        }
    }

    pub(crate) fn is_installed(&self, model: ModelKey) -> bool {
        self.installed.lock().expect("fake state").contains(&model)
    }

    fn require(&self, model: ModelKey) -> Result<(), String> {
        if self.is_installed(model) {
            Ok(())
        } else {
            Err(format!("{} isn't installed", info(model).name))
        }
    }
}

fn info(model: ModelKey) -> ModelInfoView {
    catalogue()
        .into_iter()
        .find(|m| m.key == model)
        .expect("every model is catalogued")
}

impl AiService for FakeAi {
    fn models(&self) -> Vec<ModelInfoView> {
        catalogue()
            .into_iter()
            .map(|m| ModelInfoView {
                installed: self.is_installed(m.key),
                ..m
            })
            .collect()
    }

    fn download(
        &self,
        model: ModelKey,
        progress: &dyn Fn(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let total = info(model).bytes;
        progress(0, total);
        if self.offline.load(Ordering::Relaxed) {
            std::thread::sleep(self.tick);
            return Err("Couldn't reach huggingface.co: the computer seems to be offline".into());
        }
        let steps = self.steps.max(1);
        for i in 1..=steps {
            std::thread::sleep(self.tick);
            if cancel.load(Ordering::Relaxed) {
                return Err("Download cancelled".into());
            }
            progress(total * i as u64 / steps as u64, total);
        }
        self.installed.lock().expect("fake state").insert(model);
        Ok(())
    }

    fn remove(&self, model: ModelKey) -> Result<(), String> {
        self.installed.lock().expect("fake state").remove(&model);
        Ok(())
    }

    fn provider(&self) -> String {
        "Simulated (no model)".into()
    }

    fn select(&self, image: &Raster, prompts: &[AiPrompt], encoding: &dyn Fn()) -> Result<Vec<f32>, String> {
        self.require(ModelKey::MobileSam)?;
        let key = raster_hash(image);
        if self.encoded.lock().expect("fake state").insert(key) {
            encoding();
            std::thread::sleep(self.encode_delay);
        }
        std::thread::sleep(self.run_delay);
        let n = image.width as usize * image.height as usize;
        let mut out = vec![0.0f32; n];
        for p in prompts {
            match *p {
                AiPrompt::Point { x, y, positive } => {
                    let region = flood(image, x, y);
                    for (o, r) in out.iter_mut().zip(&region) {
                        if *r {
                            *o = if positive { 1.0 } else { 0.0 };
                        }
                    }
                }
                AiPrompt::Box { x0, y0, x1, y1 } => {
                    for (i, v) in boxed(image, x0, y0, x1, y1) {
                        out[i] = out[i].max(v);
                    }
                }
            }
        }
        Ok(out)
    }

    fn matte(&self, image: &Raster) -> Result<Vec<f32>, String> {
        self.require(ModelKey::BiRefNetLite)?;
        std::thread::sleep(self.run_delay);
        let (w, h) = (image.width, image.height);
        let border: Vec<[f32; 3]> = (0..w)
            .flat_map(|x| [(x, 0), (x, h.saturating_sub(1))])
            .chain((0..h).flat_map(|y| [(0, y), (w.saturating_sub(1), y)]))
            .map(|(x, y)| encoded(image, x, y))
            .collect();
        let bg = mean(&border);
        Ok((0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                if image.get(x, y).a <= 0.0 {
                    return 0.0;
                }
                ((dist(encoded(image, x, y), bg) - 0.15) / 0.15).clamp(0.0, 1.0)
            })
            .collect())
    }
}

/// Straight, sRGB-encoded RGB of a pixel (what a model looks at).
fn encoded(image: &Raster, x: u32, y: u32) -> [f32; 3] {
    let [r, g, b, _] = image.get(x, y).to_straight();
    [r, g, b].map(|c| lumenply_io::linear_to_srgb_f(c.clamp(0.0, 1.0)))
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn mean(cs: &[[f32; 3]]) -> [f32; 3] {
    let n = cs.len().max(1) as f32;
    let s = cs
        .iter()
        .fold([0.0; 3], |a, c| [a[0] + c[0], a[1] + c[1], a[2] + c[2]]);
    s.map(|v| v / n)
}

/// The 4-connected region of colours near the one at (x, y).
fn flood(image: &Raster, x: f32, y: f32) -> Vec<bool> {
    let (w, h) = (image.width as i32, image.height as i32);
    let mut seen = vec![false; (w * h).max(0) as usize];
    let (sx, sy) = (x.floor() as i32, y.floor() as i32);
    if sx < 0 || sy < 0 || sx >= w || sy >= h {
        return seen;
    }
    let seed = encoded(image, sx as u32, sy as u32);
    let mut stack = vec![(sx, sy)];
    seen[(sy * w + sx) as usize] = true;
    while let Some((px, py)) = stack.pop() {
        for (nx, ny) in [(px + 1, py), (px - 1, py), (px, py + 1), (px, py - 1)] {
            if nx < 0 || ny < 0 || nx >= w || ny >= h {
                continue;
            }
            let i = (ny * w + nx) as usize;
            if !seen[i] && dist(encoded(image, nx as u32, ny as u32), seed) <= FakeAi::TOLERANCE {
                seen[i] = true;
                stack.push((nx, ny));
            }
        }
    }
    seen
}

/// Pixels inside a box that differ from the colour along its border.
fn boxed(image: &Raster, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(usize, f32)> {
    let (w, h) = (image.width as i32, image.height as i32);
    let clampx = |v: f32| (v.round() as i32).clamp(0, w);
    let clampy = |v: f32| (v.round() as i32).clamp(0, h);
    let (ax, bx) = (clampx(x0.min(x1)), clampx(x0.max(x1)));
    let (ay, by) = (clampy(y0.min(y1)), clampy(y0.max(y1)));
    if bx - ax < 2 || by - ay < 2 {
        return Vec::new();
    }
    let mut border = Vec::new();
    for x in ax..bx {
        border.push(encoded(image, x as u32, ay as u32));
        border.push(encoded(image, x as u32, (by - 1) as u32));
    }
    for y in ay..by {
        border.push(encoded(image, ax as u32, y as u32));
        border.push(encoded(image, (bx - 1) as u32, y as u32));
    }
    let bg = mean(&border);
    let mut out = Vec::new();
    for y in ay..by {
        for x in ax..bx {
            if dist(encoded(image, x as u32, y as u32), bg) > FakeAi::TOLERANCE {
                out.push(((y * w + x) as usize, 1.0));
            }
        }
    }
    out
}

/// Content key of an image, as the engine caches embeddings by.
fn raster_hash(image: &Raster) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (image.width, image.height).hash(&mut h);
    for p in &image.pixels {
        [p.r, p.g, p.b, p.a].map(f32::to_bits).hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    /// 40 × 20 white with a red 10 × 6 block at (5, 4).
    fn card() -> Raster {
        let mut r = Raster::filled(40, 20, Rgba::WHITE);
        for y in 4..10 {
            for x in 5..15 {
                r.set(x, y, Rgba::new(0.8, 0.05, 0.05, 1.0));
            }
        }
        r
    }

    fn count(v: &[f32]) -> f32 {
        v.iter().sum()
    }

    #[test]
    fn sizes_read_as_download_sizes() {
        assert_eq!(format_bytes(44_500_000), "44.5 MB");
        assert_eq!(format_bytes(115_000_000), "115 MB");
        assert_eq!(format_bytes(820_000), "820 KB");
        assert_eq!(ModelKey::from_slug("sam"), Some(ModelKey::MobileSam));
        assert_eq!(ModelKey::from_slug("birefnet"), Some(ModelKey::BiRefNetLite));
        assert_eq!(
            ModelKey::from_slug("rmbg"),
            None,
            "RMBG-2.0 is not offered (ADR 0028)"
        );
    }

    #[test]
    fn the_fake_selects_by_colour_and_reports_encoding_once() {
        let ai = FakeAi::new(&[ModelKey::MobileSam]);
        let encodes = std::cell::Cell::new(0);
        let on_red = AiPrompt::Point {
            x: 7.0,
            y: 5.0,
            positive: true,
        };
        let a = ai
            .select(&card(), &[on_red], &|| encodes.set(encodes.get() + 1))
            .unwrap();
        assert_eq!(count(&a), 60.0, "the 10 × 6 block");
        assert_eq!((a[5 * 40 + 7], a[5 * 40 + 20]), (1.0, 0.0));
        let b = ai
            .select(
                &card(),
                &[AiPrompt::Box {
                    x0: 2.0,
                    y0: 2.0,
                    x1: 20.0,
                    y1: 14.0,
                }],
                &|| encodes.set(encodes.get() + 1),
            )
            .unwrap();
        assert_eq!(b, a, "a box around the block finds the same block");
        assert_eq!(encodes.get(), 1, "the second call reuses the embedding");
        let on_white = AiPrompt::Point {
            x: 30.0,
            y: 15.0,
            positive: true,
        };
        let c = ai.select(&card(), &[on_white], &|| ()).unwrap();
        assert_eq!(count(&c), 800.0 - 60.0, "everything but the block");
        assert!(FakeAi::new(&[]).select(&card(), &[on_red], &|| ()).is_err());
    }

    #[test]
    fn the_fake_matte_is_what_differs_from_the_border() {
        let ai = FakeAi::new(&[ModelKey::BiRefNetLite]);
        let m = ai.matte(&card()).unwrap();
        assert_eq!(m.len(), 800);
        assert_eq!(count(&m), 60.0);
        assert_eq!((m[4 * 40 + 5], m[4 * 40 + 4], m[0]), (1.0, 0.0, 0.0));
    }

    #[test]
    fn the_fake_download_ticks_to_the_total_and_can_be_cancelled() {
        let ai = FakeAi::new(&[]);
        let seen = Mutex::new(Vec::new());
        let go = AtomicBool::new(false);
        ai.download(
            ModelKey::MobileSam,
            &|d, t| seen.lock().unwrap().push((d, t)),
            &go,
        )
        .unwrap();
        let total = 44_500_000;
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (0, total),
                (total / 4, total),
                (total / 2, total),
                (total * 3 / 4, total),
                (total, total)
            ]
        );
        assert!(ai.is_installed(ModelKey::MobileSam));
        let stop = AtomicBool::new(true);
        assert!(ai.download(ModelKey::BiRefNetLite, &|_, _| (), &stop).is_err());
        assert!(
            !ai.is_installed(ModelKey::BiRefNetLite),
            "cancelled: nothing installed"
        );
        let offline = FakeAi {
            offline: AtomicBool::new(true),
            ..FakeAi::new(&[])
        };
        let e = offline
            .download(ModelKey::BiRefNetLite, &|_, _| (), &go)
            .unwrap_err();
        assert!(e.contains("offline"), "{e}");
        ai.remove(ModelKey::MobileSam).unwrap();
        assert!(!ai.is_installed(ModelKey::MobileSam));
    }
}
