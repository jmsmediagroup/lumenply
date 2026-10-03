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
    /// Where it runs and why, and other caveats.
    pub note: Option<&'static str>,
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
            },
            ModelFile {
                name: "sam_mask_decoder_multi.onnx",
                url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/sam_mask_decoder_multi.onnx",
                bytes: 16_496_559,
                sha256: "8976b90a87ba50a6a72217a5ff994f7d25ce16f2229fcc1ed259e1294c622ffe",
                avoid: &[Provider::CoreMl],
            },
        ],
        licence: "Apache-2.0 (MobileSAM and SAM weights); MIT (ONNX export)",
        source: "https://huggingface.co/Acly/MobileSAM/tree/0d3b403339b4674a82493d5e97964dd78089ddc8",
        total_bytes: 28_157_093 + 16_496_559,
        input_size: 1024,
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
            name: "birefnet_lite.onnx",
            url: "https://huggingface.co/onnx-community/BiRefNet_lite-ONNX/resolve/de15b22ba131738a16dff04aab8bdf8dc32e3ac1/onnx/model.onnx",
            bytes: 224_005_088,
            sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
            avoid: &[Provider::CoreMl],
        }],
        licence: "MIT",
        source: "https://huggingface.co/onnx-community/BiRefNet_lite-ONNX/tree/de15b22ba131738a16dff04aab8bdf8dc32e3ac1",
        total_bytes: 224_005_088,
        input_size: 1024,
        note: Some(
            "fp32 weights: on the CPU the fp16 export (115 MB) gives the same matte within one 8-bit \
             step but runs 7 % slower and peaks 1 GB higher (its casts). Runs on the CPU on macOS: \
             ONNX Runtime 1.28's CoreML provider can't compile either export as an ML Program (a \
             convolution without explicit pads) and is about 100 times slower than the CPU as a \
             NeuralNetwork. A run peaks at about 10.5 GB of memory: its deformable convolution is \
             exported as plain gathers of 800 MB tensors.",
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
    const BIREFNET_REV: &str = "de15b22ba131738a16dff04aab8bdf8dc32e3ac1";

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
            for f in m.files {
                assert!(f.url.starts_with("https://huggingface.co/"), "{}", f.url);
                assert!(f.url.contains(&format!("/resolve/{rev}/")), "pinned: {}", f.url);
                assert_eq!(f.sha256.len(), 64);
                assert!(f
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                assert!(!f.name.contains('/'));
            }
        }
        assert_eq!(ModelId::MobileSam.info().total_bytes, 44_653_652);
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
