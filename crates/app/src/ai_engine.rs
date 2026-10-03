//! The real [`AiService`]: a thin adapter over `lumenply-ai` (ADR 0028).
//!
//! Models live in the data folder's `models/` (under tests, the test data
//! folder). The ONNX Runtime starts on first use, not at launch. MobileSAM
//! stays loaded once used (clicks then only run its small decoder);
//! BiRefNet is loaded for each matte and dropped straight after, because a
//! run peaks at several gigabytes.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use lumenply_ai::{Matter, ModelId, ModelStore, Prompt, Runtime, Segmenter};
use lumenply_tiles::Raster;

use crate::ai::{catalogue, AiPrompt, AiService, ModelInfoView, ModelKey};

fn model_id(key: ModelKey) -> ModelId {
    match key {
        ModelKey::MobileSam => ModelId::MobileSam,
        ModelKey::BiRefNetLite => ModelId::BiRefNetLite,
    }
}

/// "https://huggingface.co/Acly/MobileSAM/tree/<rev>" as the UI shows it.
fn short_source(url: &str) -> String {
    let s = url.trim_start_matches("https://").trim_start_matches("http://");
    match s.find("/tree/").or_else(|| s.find("/resolve/")) {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

pub(crate) struct Engine {
    store: ModelStore,
    runtime: OnceLock<Result<Runtime, String>>,
    segmenter: Mutex<Option<Arc<Segmenter>>>,
}

impl Engine {
    /// The engine with models kept in `dir`.
    pub(crate) fn new(dir: PathBuf) -> Engine {
        Engine {
            store: ModelStore::new(dir),
            runtime: OnceLock::new(),
            segmenter: Mutex::new(None),
        }
    }

    /// The engine over the app's data folder, if there is one.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn for_app() -> Option<Engine> {
        crate::session::data_dir().map(|d| Engine::new(d.join("models")))
    }

    fn runtime(&self) -> Result<&Runtime, String> {
        self.runtime
            .get_or_init(|| Runtime::new().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| format!("The AI engine couldn't start: {e}"))
    }

    fn segmenter(&self) -> Result<Arc<Segmenter>, String> {
        let mut slot = self.segmenter.lock().map_err(|_| "the AI engine stopped")?;
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let s = Arc::new(Segmenter::load(self.runtime()?, &self.store).map_err(|e| e.to_string())?);
        *slot = Some(s.clone());
        Ok(s)
    }
}

impl AiService for Engine {
    fn models(&self) -> Vec<ModelInfoView> {
        catalogue()
            .into_iter()
            .map(|mut view| {
                let info = model_id(view.key).info();
                view.name = info.name.to_string();
                view.bytes = info.total_bytes;
                view.source = short_source(info.source);
                view.licence = info.licence.to_string();
                view.installed = self.store.installed(info.id);
                view
            })
            .collect()
    }

    fn download(
        &self,
        model: ModelKey,
        progress: &dyn Fn(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.store
            .download(model_id(model), progress, cancel)
            .map_err(|e| e.to_string())
    }

    fn remove(&self, model: ModelKey) -> Result<(), String> {
        if model == ModelKey::MobileSam {
            if let Ok(mut s) = self.segmenter.lock() {
                *s = None;
            }
        }
        self.store.remove(model_id(model)).map_err(|e| e.to_string())
    }

    fn provider(&self) -> String {
        match self.runtime() {
            Ok(rt) => rt.provider().to_string(),
            Err(_) => "unavailable".into(),
        }
    }

    fn unavailable(&self) -> Option<String> {
        self.runtime().err()
    }

    fn select(&self, image: &Raster, prompts: &[AiPrompt], encoding: &dyn Fn()) -> Result<Vec<f32>, String> {
        let seg = self.segmenter()?;
        let emb = match seg.cached_embedding(image) {
            Some(e) => e,
            None => {
                encoding();
                seg.embed(image).map_err(|e| e.to_string())?
            }
        };
        let prompts: Vec<Prompt> = prompts
            .iter()
            .map(|p| match *p {
                AiPrompt::Point { x, y, positive } => Prompt::Point { x, y, positive },
                AiPrompt::Box { x0, y0, x1, y1 } => Prompt::Box { x0, y0, x1, y1 },
            })
            .collect();
        let matte = seg.segment(&emb, &prompts).map_err(|e| e.to_string())?;
        sized(matte, image)
    }

    fn matte(&self, image: &Raster) -> Result<Vec<f32>, String> {
        // Loaded per run and dropped right after: a run peaks at gigabytes.
        let matter = Matter::load(self.runtime()?, &self.store).map_err(|e| e.to_string())?;
        let matte = matter.matte(image).map_err(|e| e.to_string())?;
        drop(matter);
        sized(matte, image)
    }
}

fn sized(m: lumenply_ai::Matte, image: &Raster) -> Result<Vec<f32>, String> {
    if (m.width, m.height) != (image.width, image.height) {
        return Err(format!(
            "the model returned a {}×{} mask for a {}×{} image",
            m.width, m.height, image.width, image.height
        ));
    }
    Ok(m.alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_show_the_registry_and_what_is_installed() {
        let dir = std::env::temp_dir().join(format!("lumenply-ai-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = Engine::new(dir.clone());
        let models = engine.models();
        assert_eq!(models.len(), 2);
        let sam = models.iter().find(|m| m.key == ModelKey::MobileSam).unwrap();
        assert_eq!(sam.name, "MobileSAM");
        assert_eq!(sam.bytes, 28_157_093 + 16_496_559);
        assert_eq!(sam.source, "huggingface.co/Acly/MobileSAM");
        assert!(!sam.installed, "nothing downloaded in a fresh folder");
        assert_eq!(sam.purpose, "Object Selection", "the app's own wording");
        // Running without the model installed explains itself.
        let img = Raster::new(8, 8);
        let err = engine.select(&img, &[], &|| {}).unwrap_err();
        assert!(!err.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_read_as_repository_names() {
        assert_eq!(
            short_source(
                "https://huggingface.co/Acly/MobileSAM/tree/0d3b403339b4674a82493d5e97964dd78089ddc8"
            ),
            "huggingface.co/Acly/MobileSAM"
        );
        assert_eq!(short_source("https://example.org/m"), "example.org/m");
    }
}
