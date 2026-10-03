//! The crate's error type.

/// Anything that can go wrong loading, downloading or running a model.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// The model's files are not in the store: download it first.
    #[error("{0} is not installed")]
    NotInstalled(String),
    /// ONNX Runtime refused to load or run a model.
    #[error("ONNX Runtime: {0}")]
    Runtime(String),
    /// A model gave outputs of an unexpected name, type or shape.
    #[error("unexpected model output: {0}")]
    Model(String),
    /// The network request failed.
    #[error("download failed: {0}")]
    Download(String),
    /// The caller set the cancel flag; the partial file is gone.
    #[error("download cancelled")]
    Cancelled,
    /// A downloaded file is not the pinned one (corrupt or tampered with).
    #[error("{file} does not match its pinned SHA-256 (got {actual}, expected {expected})")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    /// A run would need more memory than the system has available now
    /// (checked before the run, against the model's measured peak).
    #[error(
        "{model} needs about {} of free memory for a run; {} is available",
        crate::memory::gb(*.needed),
        crate::memory::gb(*.available)
    )]
    OutOfMemory {
        model: String,
        /// Bytes a run needs beyond the loaded model.
        needed: u64,
        /// Bytes the system had available.
        available: u64,
    },
    /// The model file keeps tensor data in another file (ONNX external
    /// data). Refused: ONNX Runtime would read whatever file it names.
    #[error("{0} refers to data outside itself; only self-contained models are loaded")]
    ExternalData(String),
    /// A bad argument, such as two boxes in one prompt.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl<R> From<ort::Error<R>> for AiError {
    fn from(e: ort::Error<R>) -> Self {
        AiError::Runtime(e.to_string())
    }
}

pub type Result<T, E = AiError> = std::result::Result<T, E>;
