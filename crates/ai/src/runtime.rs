//! ONNX Runtime: which execution provider runs a model, and loading one.
//!
//! The platform's accelerator is tried first (CoreML on macOS, DirectML on
//! Windows, CUDA where the `cuda` feature linked it), then the CPU. A
//! provider that fails to register or to load a model is skipped for that
//! model; ONNX Runtime itself runs on the CPU whatever part of a graph the
//! provider can't take. Each loaded model reports the provider it got and
//! how many of its graph nodes that provider took (from ONNX Runtime's own
//! placement log), so "running on CoreML" is a measured statement.

use std::path::Path;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use ort::ep::ExecutionProviderDispatch;
use ort::logging::LogLevel;
use ort::session::Session;

use crate::{AiError, Result};

/// An ONNX Runtime execution provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Apple's CoreML (Neural Engine, GPU and CPU), macOS.
    CoreMl,
    /// DirectX 12, Windows.
    DirectMl,
    /// NVIDIA CUDA (only with the `cuda` feature).
    Cuda,
    /// ONNX Runtime's own CPU kernels.
    Cpu,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::CoreMl,
        Provider::DirectMl,
        Provider::Cuda,
        Provider::Cpu,
    ];

    /// Display name ("CoreML").
    pub fn name(self) -> &'static str {
        match self {
            Provider::CoreMl => "CoreML",
            Provider::DirectMl => "DirectML",
            Provider::Cuda => "CUDA",
            Provider::Cpu => "CPU",
        }
    }

    /// The provider a name means ("coreml", "CPU"...).
    pub fn parse(name: &str) -> Option<Provider> {
        Provider::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(name.trim()))
    }

    /// ONNX Runtime's identifier, as its placement log prints it.
    fn ort_name(self) -> &'static str {
        match self {
            Provider::CoreMl => "CoreMLExecutionProvider",
            Provider::DirectMl => "DmlExecutionProvider",
            Provider::Cuda => "CUDAExecutionProvider",
            Provider::Cpu => "CPUExecutionProvider",
        }
    }

    /// Linked into this build (the CPU always is).
    pub fn compiled(self) -> bool {
        match self {
            Provider::CoreMl => cfg!(target_os = "macos"),
            Provider::DirectMl => cfg!(windows),
            Provider::Cuda => cfg!(feature = "cuda"),
            Provider::Cpu => true,
        }
    }

    /// The platform's order: accelerators first, the CPU last.
    pub fn platform_order() -> Vec<Provider> {
        Provider::ALL.into_iter().filter(|p| p.compiled()).collect()
    }

    /// The provider's registration, failing loudly so the next one is
    /// tried; `None` for the CPU (always there, needs none).
    #[allow(unused_variables)]
    fn dispatch(self, cache: Option<&Path>) -> Option<ExecutionProviderDispatch> {
        match self {
            #[cfg(target_os = "macos")]
            Provider::CoreMl => {
                use ort::ep::coreml::{ComputeUnits, ModelFormat};
                let mut ep = ort::ep::CoreML::default()
                    .with_model_format(ModelFormat::MLProgram)
                    .with_compute_units(ComputeUnits::All);
                if let Some(dir) = cache {
                    ep = ep.with_model_cache_dir(dir.display());
                }
                Some(ep.build().error_on_failure())
            }
            #[cfg(windows)]
            Provider::DirectMl => Some(ort::ep::DirectML::default().build().error_on_failure()),
            #[cfg(feature = "cuda")]
            Provider::Cuda => Some(ort::ep::CUDA::default().build().error_on_failure()),
            _ => None,
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The environment variable that forces a provider ("cpu", "coreml"...),
/// for measurements and bug reports.
pub const PROVIDER_ENV: &str = "LUMENPLY_AI_PROVIDER";
/// Set to `verbose`, `info` or `warning` to print ONNX Runtime's log.
pub const LOG_ENV: &str = "LUMENPLY_AI_LOG";

fn log_threshold() -> Option<LogLevel> {
    match std::env::var(LOG_ENV).ok()?.to_ascii_lowercase().as_str() {
        "verbose" => Some(LogLevel::Verbose),
        "info" => Some(LogLevel::Info),
        "warning" | "warn" | "1" => Some(LogLevel::Warning),
        "error" => Some(LogLevel::Error),
        _ => None,
    }
}

fn rank(l: LogLevel) -> u8 {
    match l {
        LogLevel::Verbose => 0,
        LogLevel::Info => 1,
        LogLevel::Warning => 2,
        LogLevel::Error => 3,
        LogLevel::Fatal => 4,
    }
}

/// ONNX Runtime's environment, set up once: quiet unless [`LOG_ENV`] asks.
fn init_environment() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let threshold = log_threshold();
        let _ = ort::init()
            .with_name("lumenply")
            .with_logger(Arc::new(move |level, _cat, _id, _loc, msg: &str| {
                if threshold.is_some_and(|t| rank(level) >= rank(t)) {
                    eprintln!("[onnxruntime {level:?}] {msg}");
                }
            }))
            .commit();
    });
}

/// The execution providers to try for each model, in order.
#[derive(Clone, Debug)]
pub struct Runtime {
    order: Vec<Provider>,
}

impl Runtime {
    /// The platform's providers ([`Provider::platform_order`]), or only
    /// the one [`PROVIDER_ENV`] names.
    pub fn new() -> Result<Runtime> {
        match std::env::var(PROVIDER_ENV) {
            Ok(name) if !name.trim().is_empty() => {
                let p = Provider::parse(&name)
                    .ok_or_else(|| AiError::Invalid(format!("{PROVIDER_ENV}: unknown provider {name:?}")))?;
                Runtime::with_providers(&[p])
            }
            _ => Runtime::with_providers(&Provider::platform_order()),
        }
    }

    /// Only the CPU.
    pub fn cpu() -> Result<Runtime> {
        Runtime::with_providers(&[Provider::Cpu])
    }

    /// Try `order`, then the CPU. Providers this build lacks are dropped.
    pub fn with_providers(order: &[Provider]) -> Result<Runtime> {
        init_environment();
        let mut list: Vec<Provider> = Vec::new();
        for &p in order.iter().chain(&[Provider::Cpu]) {
            if p.compiled() && !list.contains(&p) {
                list.push(p);
            }
        }
        Ok(Runtime { order: list })
    }

    /// The providers tried, in order (the CPU last).
    pub fn providers(&self) -> &[Provider] {
        &self.order
    }

    /// The provider models are expected to run on: the first one this
    /// ONNX Runtime build has (each loaded model also reports its own).
    pub fn provider(&self) -> &str {
        self.order
            .iter()
            .find(|p| available(**p))
            .unwrap_or(&Provider::Cpu)
            .name()
    }

    /// ONNX Runtime's build description.
    pub fn build_info() -> &'static str {
        init_environment();
        ort::info()
    }

    /// Load the model at `path`: the first provider in order, less those
    /// `how.avoid`s, that registers and loads it.
    pub(crate) fn load(&self, path: &Path, how: Load) -> Result<Model> {
        if !path.is_file() {
            return Err(AiError::NotInstalled(path.display().to_string()));
        }
        if crate::onnx_check::uses_external_data(path)? {
            return Err(AiError::ExternalData(path.display().to_string()));
        }
        let started = Instant::now();
        let attempts: Vec<Provider> = self
            .order
            .iter()
            .copied()
            .filter(|&p| p == Provider::Cpu || !how.avoid.contains(&p))
            .collect();
        // ONNX Runtime keys its CoreML cache on the graph alone, so a
        // compile with other dimension overrides (or by another ONNX
        // Runtime) would be reused; a folder per load fingerprint isn't.
        let cache = how.cache.map(|c| c.join(cache_key(path, how.dims)));
        let how = Load {
            cache: cache.as_deref(),
            ..how
        };
        let (provider, (session, nodes), fallback) =
            first_loading(&attempts, |p| build_session(p, path, p.dispatch(how.cache), how))
                .map_err(|e| AiError::Runtime(format!("{} could not be loaded: {e}", path.display())))?;
        if provider != Provider::CoreMl && attempts.contains(&Provider::CoreMl) {
            // A CoreML compile that failed can leave a gigabyte behind.
            if let Some(dir) = how.cache {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        Ok(Model {
            session: Mutex::new(session),
            provider,
            nodes,
            fallback,
            load_time: started.elapsed(),
            name: how.name.to_string(),
            run_bytes: how.run_bytes,
        })
    }
}

/// How to load a model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Load<'a> {
    /// Providers not to try (the CPU is always tried).
    pub avoid: &'a [Provider],
    /// Where CoreML keeps its compiled model, so later loads are quick.
    pub cache: Option<&'a Path>,
    /// Sizes for named dynamic dimensions: static shapes let CoreML
    /// compile a graph it can't compile with free dimensions.
    pub dims: &'a [(&'a str, i64)],
    /// ONNX Runtime's CPU memory arena (and memory patterns): quicker
    /// repeated runs, but a session then keeps its largest run's memory
    /// for as long as it lives.
    pub arena: bool,
    /// The model's name in messages.
    pub name: &'a str,
    /// Memory a run needs beyond the loaded model (measured); a run is
    /// refused when the system has less available. 0 skips the check.
    pub run_bytes: u64,
}

impl Default for Load<'_> {
    fn default() -> Self {
        Load {
            avoid: &[],
            cache: None,
            dims: &[],
            arena: true,
            name: "model",
            run_bytes: 0,
        }
    }
}

/// "<file stem>-<16 hex>": the file, the dimension overrides and the
/// ONNX Runtime build a compiled model belongs to.
fn cache_key(path: &Path, dims: &[(&str, i64)]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(path.file_name().unwrap_or_default().as_encoded_bytes());
    for (name, size) in dims {
        h.update(name.as_bytes());
        h.update(&size.to_le_bytes());
    }
    h.update(ort::info().as_bytes());
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    format!("{stem}-{}", &h.finalize().to_hex()[..16])
}

/// The first of `attempts` that `open` succeeds with, and the failures
/// before it ("CoreML: why").
fn first_loading<T>(
    attempts: &[Provider],
    mut open: impl FnMut(Provider) -> Result<T>,
) -> Result<(Provider, T, Vec<String>)> {
    let mut failures = Vec::new();
    for &p in attempts {
        match open(p) {
            Ok(t) => return Ok((p, t, failures)),
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    Err(AiError::Runtime(if failures.is_empty() {
        "no provider to try".into()
    } else {
        failures.join("; ")
    }))
}

/// ONNX Runtime has the provider compiled in.
fn available(p: Provider) -> bool {
    #[cfg(any(target_os = "macos", windows, feature = "cuda"))]
    use ort::ep::ExecutionProvider;
    match p {
        Provider::Cpu => true,
        #[cfg(target_os = "macos")]
        Provider::CoreMl => ort::ep::CoreML::default().is_available().unwrap_or(false),
        #[cfg(windows)]
        Provider::DirectMl => ort::ep::DirectML::default().is_available().unwrap_or(false),
        #[cfg(feature = "cuda")]
        Provider::Cuda => ort::ep::CUDA::default().is_available().unwrap_or(false),
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

/// How a model's graph was split between providers, from ONNX Runtime's
/// log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    /// The accelerator's own count, `(claimed, all)` graph nodes (CoreML
    /// logs it; the last report wins).
    pub claimed: Option<(usize, usize)>,
    /// Subgraphs the accelerator took (each runs as one fused node).
    pub partitions: Option<usize>,
    /// `(provider identifier, nodes)` after partitioning, as ONNX
    /// Runtime's placement log lists them (a fused subgraph counts once).
    pub placed: Vec<(String, usize)>,
}

/// The number after `label` in `msg`.
fn number_after(msg: &str, label: &str) -> Option<usize> {
    let rest = msg.split(label).nth(1)?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

impl Placement {
    /// Nodes placed on `p` after partitioning.
    pub fn placed_on(&self, p: Provider) -> usize {
        self.placed
            .iter()
            .filter(|(n, _)| n == p.ort_name())
            .map(|(_, c)| c)
            .sum()
    }

    /// Read one log message.
    fn read(&mut self, msg: &str) {
        // "CoreMLExecutionProvider::GetCapability, number of partitions
        // supported by CoreML: 1 number of nodes in the graph: 2 number of
        // nodes supported by CoreML: 2"
        if msg.contains("GetCapability") {
            let all = number_after(msg, "number of nodes in the graph:");
            let claimed = msg
                .split("number of nodes supported by")
                .nth(1)
                .and_then(|r| number_after(r, ":"));
            if let (Some(a), Some(c)) = (all, claimed) {
                self.claimed = Some((c, a));
            }
            if let Some(p) = msg
                .split("number of partitions supported by")
                .nth(1)
                .and_then(|r| number_after(r, ":"))
            {
                self.partitions = Some(p);
            }
            return;
        }
        // "Node(s) placed on [X]. Number of nodes: N", "All nodes placed on ..."
        let Some(rest) = msg.split("placed on [").nth(1) else {
            return;
        };
        let Some((name, rest)) = rest.split_once(']') else {
            return;
        };
        if let Some(n) = number_after(rest, "Number of nodes:") {
            self.placed.push((name.to_string(), n));
        }
    }
}

/// A session on `p` registered through `ep` (none for the CPU), with its
/// node placement, or why not.
fn build_session(
    p: Provider,
    path: &Path,
    ep: Option<ExecutionProviderDispatch>,
    how: Load,
) -> Result<(Session, Placement)> {
    let placed = Arc::new(Mutex::new(Placement::default()));
    let sink = placed.clone();
    let threshold = log_threshold();
    let mut b = Session::builder()?
        .with_log_level(LogLevel::Verbose)?
        .with_logger(Arc::new(move |level, _cat, _id, _loc, msg: &str| {
            if msg.contains("placed on [") || msg.contains("GetCapability") {
                if let Ok(mut s) = sink.lock() {
                    s.read(msg);
                }
            }
            if threshold.is_some_and(|t| rank(level) >= rank(t)) {
                eprintln!("[onnxruntime {level:?}] {msg}");
            }
        }))?;
    if p == Provider::DirectMl {
        // DirectML can't run with memory patterns (ONNX Runtime's docs).
        b = b.with_memory_pattern(false)?;
    }
    if !how.arena {
        use ort::AsPointer;
        // SAFETY: `b` owns a live OrtSessionOptions; the call only clears
        // a flag on it. ort doesn't wrap DisableCpuMemArena.
        let status = unsafe { (ort::api().DisableCpuMemArena)(b.ptr_mut()) };
        // SAFETY: a status this API returned, released by `Error` alone.
        unsafe { ort::Error::result_from_status(status) }?;
        b = b.with_memory_pattern(false)?;
    }
    for &(name, size) in how.dims {
        b = b.with_dimension_override(name, size)?;
    }
    if let Some(ep) = ep {
        if let Some(dir) = how.cache {
            std::fs::create_dir_all(dir)?;
        }
        b = b.with_execution_providers([ep])?;
    }
    let session = b.commit_from_file(path)?;
    let nodes = placed.lock().map(|c| c.clone()).unwrap_or_default();
    Ok((session, nodes))
}

/// A loaded model.
pub(crate) struct Model {
    session: Mutex<Session>,
    pub provider: Provider,
    pub nodes: Placement,
    /// Providers that were tried first and failed, with the reason.
    pub fallback: Vec<String>,
    pub load_time: Duration,
    name: String,
    run_bytes: u64,
}

impl Model {
    /// Run `f` with the session (ONNX Runtime sessions run one call at a
    /// time), unless the run would not fit in the memory available now
    /// ([`AiError::OutOfMemory`]).
    pub(crate) fn with<T>(&self, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
        if self.run_bytes > 0 {
            crate::memory::check(&self.name, self.run_bytes, crate::memory::available())?;
        }
        let mut s = self
            .session
            .lock()
            .map_err(|_| AiError::Runtime("a previous run panicked".into()))?;
        f(&mut s)
    }

    /// "CoreML (812 of 860 nodes, 3 partitions)", or the provider alone
    /// when the log gave no counts.
    pub(crate) fn describe(&self) -> String {
        match (self.provider, self.nodes.claimed) {
            (Provider::Cpu, _) | (_, None) => self.provider.name().to_string(),
            (p, Some((claimed, all))) => {
                let parts = self.nodes.partitions.unwrap_or(0);
                let s = if parts == 1 { "" } else { "s" };
                format!("{p} ({claimed} of {all} nodes, {parts} partition{s})")
            }
        }
    }
}

/// What a loaded model runs on, for the app and bug reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelReport {
    /// The provider the session was created with.
    pub provider: Provider,
    /// Nodes per provider (from ONNX Runtime's log).
    pub placement: Placement,
    /// Providers tried first that failed, with the reason.
    pub fallback: Vec<String>,
    pub load_time: Duration,
}

impl From<&Model> for ModelReport {
    fn from(m: &Model) -> Self {
        ModelReport {
            provider: m.provider,
            placement: m.nodes.clone(),
            fallback: m.fallback.clone(),
            load_time: m.load_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_lines_are_read() {
        let mut p = Placement::default();
        p.read(
            "CoreMLExecutionProvider::GetCapability, number of partitions supported by CoreML: 3 \
             number of nodes in the graph: 860 number of nodes supported by CoreML: 812",
        );
        p.read("Node(s) placed on [CoreMLExecutionProvider]. Number of nodes: 3");
        p.read(" Node(s) placed on [CPUExecutionProvider]. Number of nodes: 48");
        p.read("something else entirely");
        assert_eq!(p.claimed, Some((812, 860)));
        assert_eq!(p.partitions, Some(3));
        assert_eq!(p.placed_on(Provider::CoreMl), 3);
        assert_eq!(p.placed_on(Provider::Cpu), 48);
        let mut all = Placement::default();
        all.read(" All nodes placed on [CPUExecutionProvider]. Number of nodes: 7");
        assert_eq!(all.placed_on(Provider::Cpu), 7);
        assert_eq!(all.claimed, None);
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// `y = 2x + 1` through the fixture on `m`.
    fn run_affine(m: &Model) -> Vec<f32> {
        let x: Vec<f32> = (0..6).map(|v| v as f32).collect();
        m.with(|s| {
            let t = ort::value::TensorRef::from_array_view(([2usize, 3], &x[..]))?;
            let out = s.run(ort::inputs!["x" => t])?;
            let (shape, y) = out["y"].try_extract_tensor::<f32>()?;
            assert_eq!(shape[..], [2, 3]);
            Ok(y.to_vec())
        })
        .unwrap()
    }

    #[test]
    fn a_model_runs_on_the_platform_provider() {
        let rt = Runtime::with_providers(&Provider::platform_order()).unwrap();
        let m = rt.load(&fixture("affine.onnx"), Load::default()).unwrap();
        assert_eq!(run_affine(&m), vec![1.0, 3.0, 5.0, 7.0, 9.0, 11.0]);
        // The first provider this build has takes it: CoreML on macOS,
        // with ONNX Runtime's log accounting for both nodes. (Elsewhere a
        // machine may lack the accelerator; then the reason is kept.)
        let first = Provider::platform_order()[0];
        if cfg!(target_os = "macos") {
            assert_eq!(m.provider, Provider::CoreMl);
            assert!(m.fallback.is_empty(), "{:?}", m.fallback);
        } else {
            assert!(m.provider == first || !m.fallback.is_empty(), "{:?}", m.fallback);
        }
        if m.provider == Provider::CoreMl {
            // Mul and Add, both CoreML's, fused into one partition.
            assert_eq!(m.nodes.claimed, Some((2, 2)), "{:?}", m.nodes);
            assert_eq!(m.nodes.placed, vec![("CoreMLExecutionProvider".to_string(), 1)]);
            assert_eq!(m.describe(), "CoreML (2 of 2 nodes, 1 partition)");
        }
        assert_eq!(rt.provider(), first.name());
    }

    #[test]
    fn the_cpu_runtime_and_unaccelerated_models_stay_on_the_cpu() {
        let rt = Runtime::cpu().unwrap();
        let m = rt.load(&fixture("affine.onnx"), Load::default()).unwrap();
        assert_eq!(m.provider, Provider::Cpu);
        assert_eq!(m.describe(), "CPU");
        assert_eq!(m.nodes.placed_on(Provider::Cpu), 2, "{:?}", m.nodes);
        assert_eq!(run_affine(&m), vec![1.0, 3.0, 5.0, 7.0, 9.0, 11.0]);
        let platform = Runtime::with_providers(&Provider::platform_order()).unwrap();
        let m = platform
            .load(
                &fixture("affine.onnx"),
                Load {
                    avoid: &[Provider::CoreMl, Provider::DirectMl, Provider::Cuda],
                    arena: false,
                    ..Load::default()
                },
            )
            .unwrap();
        assert_eq!(m.provider, Provider::Cpu);
        // A model keeping data in another file is refused before ONNX
        // Runtime opens it (it would load: external_data.bin is there).
        assert!(fixture("external_data.bin").is_file());
        match rt.load(&fixture("external_data.onnx"), Load::default()) {
            Err(AiError::ExternalData(p)) => assert!(p.ends_with("external_data.onnx"), "{p}"),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("external data must be refused"),
        }
        // A run that wouldn't fit in the memory available now is refused
        // before it starts, with the numbers.
        let greedy = rt
            .load(
                &fixture("affine.onnx"),
                Load {
                    name: "Affine",
                    run_bytes: u64::MAX,
                    ..Load::default()
                },
            )
            .unwrap();
        if crate::memory::available().is_some() {
            match greedy.with(|_| Ok(())) {
                Err(AiError::OutOfMemory { model, needed, .. }) => {
                    assert_eq!((model.as_str(), needed), ("Affine", u64::MAX));
                }
                other => panic!("expected OutOfMemory, got {:?}", other.err()),
            }
        }
        // A missing file is "not installed", not a runtime failure.
        assert!(matches!(
            rt.load(&fixture("missing.onnx"), Load::default()),
            Err(AiError::NotInstalled(_))
        ));
    }

    #[test]
    fn a_failing_provider_falls_back_to_the_next() {
        let attempts = [Provider::CoreMl, Provider::Cuda, Provider::Cpu];
        let (p, v, failed) = first_loading(&attempts, |p| match p {
            Provider::Cpu => Ok(7),
            other => Err(AiError::Runtime(format!("{other} refused"))),
        })
        .unwrap();
        assert_eq!((p, v), (Provider::Cpu, 7));
        assert_eq!(
            failed,
            vec![
                "CoreML: ONNX Runtime: CoreML refused".to_string(),
                "CUDA: ONNX Runtime: CUDA refused".to_string()
            ]
        );
        let none = first_loading(&[Provider::CoreMl], |_| {
            Err::<(), _>(AiError::Runtime("no".into()))
        });
        assert_eq!(
            none.unwrap_err().to_string(),
            "ONNX Runtime: CoreML: ONNX Runtime: no"
        );
    }

    /// A CoreML registration ONNX Runtime rejects (an unknown compute unit)
    /// falls back to the CPU, which still runs the model.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_coreml_registration_failure_falls_back_to_the_cpu() {
        use ort::ep::ArbitrarilyConfigurableExecutionProvider;
        let _ = Runtime::cpu().unwrap();
        let path = fixture("affine.onnx");
        let (p, (session, _), failed) = first_loading(&[Provider::CoreMl, Provider::Cpu], |p| {
            let ep = (p == Provider::CoreMl).then(|| {
                ort::ep::CoreML::default()
                    .with_arbitrary_config("MLComputeUnits", "Abacus")
                    .build()
                    .error_on_failure()
            });
            build_session(p, &path, ep, Load::default())
        })
        .unwrap();
        assert_eq!(p, Provider::Cpu);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].starts_with("CoreML: "), "{failed:?}");
        let m = Model {
            session: Mutex::new(session),
            provider: p,
            nodes: Placement::default(),
            fallback: failed,
            load_time: Duration::ZERO,
            name: "affine".into(),
            run_bytes: 0,
        };
        assert_eq!(run_affine(&m), vec![1.0, 3.0, 5.0, 7.0, 9.0, 11.0]);
    }

    #[test]
    fn provider_names_and_order() {
        assert_eq!(Provider::parse("coreml"), Some(Provider::CoreMl));
        assert_eq!(Provider::parse(" CPU "), Some(Provider::Cpu));
        assert_eq!(Provider::parse("tpu"), None);
        let order = Provider::platform_order();
        assert_eq!(order.last(), Some(&Provider::Cpu));
        assert_eq!(order.contains(&Provider::CoreMl), cfg!(target_os = "macos"));
        let rt = Runtime::with_providers(&[Provider::Cpu, Provider::Cpu]).unwrap();
        assert_eq!(rt.providers(), &[Provider::Cpu]);
        assert_eq!(rt.provider(), "CPU");
    }
}
