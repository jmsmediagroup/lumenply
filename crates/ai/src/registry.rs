//! The models Lumenply can download: what they are, where they come from,
//! and the exact bytes expected (ADR 0028).
//!
//! Every URL is pinned to a repository commit, never a branch, and every
//! file carries the SHA-256 of the bytes downloaded from it (computed when
//! the entry was added, and equal to the hash the Hugging Face LFS pointer
//! records). A file that does not match is never installed.

use crate::runtime::Provider;

/// A model the app can download and run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelId {
    /// MobileSAM: click or box to select an object (Object Selection).
    MobileSam,
    /// BiRefNet lite: the salient subject's matte (Select Subject, Remove
    /// Background).
    BiRefNetLite,
}

impl ModelId {
    pub const ALL: [ModelId; 2] = [ModelId::MobileSam, ModelId::BiRefNetLite];

    /// Short stable name: the folder in the store and the CLI's NAME.
    pub fn key(self) -> &'static str {
        match self {
            ModelId::MobileSam => "mobile-sam",
            ModelId::BiRefNetLite => "birefnet-lite",
        }
    }

    /// The model a user-typed name means: its key or display name, any
    /// case, with or without separators ("mobilesam", "BiRefNet-Lite").
    pub fn parse(name: &str) -> Option<ModelId> {
        let squash = |s: &str| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        };
        let want = squash(name);
        ModelId::ALL.into_iter().find(|id| {
            let info = id.info();
            want == squash(id.key())
                || want == squash(info.name)
                || (want == "birefnet" && *id == ModelId::BiRefNetLite)
        })
    }

    pub fn info(self) -> &'static ModelInfo {
        &MODELS[self as usize]
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.info().name)
    }
}

/// One file of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelFile {
    /// File name in the model's folder of the store.
    pub name: &'static str,
    /// Where it is downloaded from, pinned to a repository commit.
    pub url: &'static str,
    /// Exact size in bytes.
    pub bytes: u64,
    /// SHA-256 of the file, lower-case hex.
    pub sha256: &'static str,
    /// Providers not to run it on: measured to fail on it or to be slower
    /// than the CPU (the model's `note` says which and why).
    pub avoid: &'static [Provider],
    /// Memory a run needs beyond the loaded model, measured (peak resident
    /// size of a run less that after loading, on the CPU). A run is
    /// refused with [`crate::AiError::OutOfMemory`] when less is available.
    pub run_bytes: u64,
}

/// Everything the app shows before a download and in Preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: ModelId,
    /// Display name.
    pub name: &'static str,
    /// What the app uses it for.
    pub task: &'static str,
    pub files: &'static [ModelFile],
    /// Licence of the weights and of the ONNX export.
    pub licence: &'static str,
    /// The repository page (at the pinned revision).
    pub source: &'static str,
    /// Sum of the files' sizes.
    pub total_bytes: u64,
    /// The square the image is resized into (pixels per side).
    pub input_size: u32,
    /// A finer run of the same files the model offers (BiRefNet's high
    /// detail), chosen with [`crate::Detail::High`].
    pub high_detail: Option<HighDetail>,
    /// Where it runs and why, and other caveats.
    pub note: Option<&'static str>,
}

/// A model's high-detail run: the same file at a larger input size, which
/// costs more memory and time. No extra download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighDetail {
    /// Display name.
    pub name: &'static str,
    /// The square the image is resized into (pixels per side).
    pub input_size: u32,
    /// Memory a run needs beyond the loaded model (measured, as
    /// [`ModelFile::run_bytes`]).
    pub run_bytes: u64,
}

static MODELS: [ModelInfo; 2] = [
    ModelInfo {
        id: ModelId::MobileSam,
        name: "MobileSAM",
        task: "Object selection: click or drag a box around an object",
        files: &[
            ModelFile {
                name: "mobile_sam_image_encoder.onnx",
                url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/mobile_sam_image_encoder.onnx",
                bytes: 28_157_093,
                sha256: "580f5fb648ea1062c0aabc26217aed56921985f03f0cbbd852bba81d760cc749",
                avoid: &[],
                // Measured: 0.56 GB (CoreML) to 0.70 GB (CPU) above the
                // loaded model, embedding the demo photo.
                run_bytes: 750_000_000,
            },
            ModelFile {
                name: "sam_mask_decoder_multi.onnx",
                url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/sam_mask_decoder_multi.onnx",
                bytes: 16_496_559,
                sha256: "8976b90a87ba50a6a72217a5ff994f7d25ce16f2229fcc1ed259e1294c622ffe",
                avoid: &[Provider::CoreMl],
                // A click's working set is a few megabytes.
                run_bytes: 0,
            },
        ],
        licence: "Apache-2.0 (MobileSAM and SAM weights); MIT (ONNX export)",
        source: "https://huggingface.co/Acly/MobileSAM/tree/0d3b403339b4674a82493d5e97964dd78089ddc8",
        total_bytes: 28_157_093 + 16_496_559,
        input_size: 1024,
        high_detail: None,
        note: Some(
            "The encoder runs on the platform accelerator; on macOS the decoder runs on the CPU, \
             where it is faster (CoreML splits it into 19 partitions: 50 ms a click against 12 ms \
             on an M4 Pro).",
        ),
    },
    ModelInfo {
        id: ModelId::BiRefNetLite,
        name: "BiRefNet lite",
        task: "Subject selection and background removal",
        files: &[ModelFile {
            name: "birefnet_lite_dynamic.onnx",
            url: "https://huggingface.co/senty-au/BiRefNet_lite-ONNX-dynamic/resolve/173d635935b93839608b9b9039da8d1d212471e9/onnx/model.onnx",
            bytes: 180_839_545,
            sha256: "1e0da42f0fde010e32e938bad388457ecefe35806fde9d923421997861ae9391",
            avoid: &[Provider::CoreMl],
            // Measured on the CPU at 768²: 2.66–2.82 GB above the loaded
            // model (1920 px photos).
            run_bytes: 3_000_000_000,
        }],
        licence: "MIT: BiRefNet lite weights by Peng Zheng et al. (huggingface.co/ZhengPeng7/BiRefNet_lite), \
                  re-exported to ONNX with native DeformConv by senty-au",
        source: "https://huggingface.co/senty-au/BiRefNet_lite-ONNX-dynamic/tree/173d635935b93839608b9b9039da8d1d212471e9",
        total_bytes: 180_839_545,
        input_size: 768,
        high_detail: Some(HighDetail {
            name: "BiRefNet (high detail)",
            input_size: 1024,
            // Measured on the CPU at 1024²: 4.46–4.74 GB.
            run_bytes: 5_000_000_000,
        }),
        note: Some(
            "BiRefNet lite (ZhengPeng7/BiRefNet_lite, MIT) as re-exported by senty-au: the same \
             weights as onnx-community's export, bit for bit, with ONNX's native DeformConv instead \
             of gathers and a free input size. Runs at 768² (a run needs about 3 GB of memory and \
             0.6 s on an M4 Pro's CPU); high detail runs the same file at 1024² (about 5 GB, 1.1 s). \
             On the CPU on macOS: CoreML gives the same matte and runs it in 0.3 s, but compiling \
             takes 69 s and loading from its cache still 15 s, and it peaks 1.5 GB higher.",
        ),
    },
];

/// Every model the app knows, in menu order.
pub fn models() -> &'static [ModelInfo] {
    &MODELS
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAM_REV: &str = "0d3b403339b4674a82493d5e97964dd78089ddc8";
    const BIREFNET_REV: &str = "173d635935b93839608b9b9039da8d1d212471e9";

    #[test]
    fn the_registry_is_consistent_and_pinned() {
        for (i, m) in models().iter().enumerate() {
            assert_eq!(m.id as usize, i, "MODELS is indexed by ModelId");
            assert_eq!(m.id.info(), m);
            assert_eq!(m.total_bytes, m.files.iter().map(|f| f.bytes).sum::<u64>());
            let rev = match m.id {
                ModelId::MobileSam => SAM_REV,
                ModelId::BiRefNetLite => BIREFNET_REV,
            };
            assert!(m.source.ends_with(rev));
            // The source shown before a download is the repository the
            // files really come from.
            let repo = m.source.split("/tree/").next().unwrap();
            for f in m.files {
                assert!(f.url.starts_with("https://huggingface.co/"), "{}", f.url);
                assert!(
                    f.url.starts_with(&format!("{repo}/resolve/{rev}/")),
                    "pinned: {}",
                    f.url
                );
                assert_eq!(f.sha256.len(), 64);
                assert!(f
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                assert!(!f.name.contains('/'));
            }
        }
        assert_eq!(ModelId::MobileSam.info().total_bytes, 44_653_652);
        let hd = ModelId::BiRefNetLite.info().high_detail.expect("high detail");
        assert_eq!(
            (ModelId::BiRefNetLite.info().input_size, hd.input_size),
            (768, 1024)
        );
        assert!(hd.run_bytes > ModelId::BiRefNetLite.info().files[0].run_bytes);
        // A third-party export names the original in what the user reads.
        let b = ModelId::BiRefNetLite.info();
        assert!(b.source.contains("/senty-au/BiRefNet_lite-ONNX-dynamic/"));
        assert!(b.licence.contains("ZhengPeng7/BiRefNet_lite") && b.licence.contains("senty-au"));
    }

    #[test]
    fn names_parse_loosely() {
        assert_eq!(ModelId::parse("mobile-sam"), Some(ModelId::MobileSam));
        assert_eq!(ModelId::parse("MobileSAM"), Some(ModelId::MobileSam));
        assert_eq!(ModelId::parse("birefnet"), Some(ModelId::BiRefNetLite));
        assert_eq!(ModelId::parse("BiRefNet_lite"), Some(ModelId::BiRefNetLite));
        assert_eq!(ModelId::parse("rmbg"), None);
        assert_eq!(ModelId::BiRefNetLite.key(), "birefnet-lite");
    }
}
