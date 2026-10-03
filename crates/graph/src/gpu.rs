//! The edit graph on the GPU (ADR 0025 stage 4, ADR 0026).
//!
//! [`GpuRenderer`] evaluates a [`Graph`] with wgpu compute kernels and keeps
//! every node's tiles resident in GPU memory under the same content keys the
//! CPU [`Renderer`] caches them by, so an edit recomputes exactly what is
//! downstream of it and an undo finds the old tiles still there.
//!
//! **Kernels**: `empty`; `image` and `mask` (blob tiles upload once per
//! tile — a compact 16-bit tile uploads as stored when the adapter has
//! 16-bit normalised textures); `layer` without effects (every blend mode
//! but Dissolve, opacity, fill opacity, mask); `adjustment` when it
//! compiles to 1D tables (Levels, Curves, Invert, Brightness/Contrast,
//! Posterize; any blend mode but Dissolve); `pass-through`; and every
//! hidden layer, whose backdrop passes through untouched. The blend maths
//! is [`lumenply_render::gpu::BLEND_WGSL`], the WGSL the layer-tree GPU
//! compositor runs too.
//!
//! **Honest fallback**: every other op or parameter combination — layer
//! effects, Dissolve, live filters, clip groups, per-pixel adjustments,
//! the content ops computed in one piece (text, fills, shapes, transforms,
//! smart filters), and any op added later — runs per node on this
//! renderer's own CPU [`Renderer`], and the result is uploaded; the GPU
//! continues downstream. Before that, the inputs the CPU op reads
//! ([`crate::ops::input_needs`]) are computed on the GPU, read back and put
//! into the CPU renderer's cache under their content keys, so the CPU
//! evaluates only the fallback node itself, not the stack below it. That
//! cache is private to this renderer, so tiles that came from the GPU never
//! reach a reference renderer's cache.
//!
//! The CPU evaluator is the reference: output matches it within 1e-4 (the
//! tests below, and `lumenply graph --check --gpu` over the PSD corpus,
//! worst 8e-7). GPU and CPU arithmetic differ in the last bits (fused
//! multiply-adds, Metal's fast division), a few 1e-7 after a stack of
//! layers; an op as steep as a step (Posterize's table, Hard Mix) magnifies
//! that by its slope wherever a pixel sits right at the step, whichever
//! device runs it.
//!
//! **Scheduling**: a render first plans. Starting from the requested tiles
//! it walks the inputs each op reads, stopping at tiles the GPU cache holds,
//! and notes per node which tiles must exist on the GPU and which on the
//! CPU (inputs of a fallback node). Then it runs node by node in dependency
//! order: one compute pass and one submit per node, fallback nodes
//! evaluated by the CPU in parallel. A tile is pinned only until its last
//! consumer has run, so memory stays near the cache budget plus about two
//! nodes' worth of tiles, and an edit near the top of a stack finds the
//! tiles below it cached (the cache keeps the most recently computed nodes).
//! The GPU cache belongs to the one thread driving the renderer, so it
//! never waits for anything (the rule the CPU tile cache follows too).
//!
//! # Drawing the result without a readback
//!
//! [`GpuRenderer::render_gpu`] leaves the result on the GPU as a
//! [`GpuImage`]: one `Rgba32Float` (or, for an untouched compact image
//! tile, `Rgba16Unorm`) 256×256 texture per tile, premultiplied linear
//! light, or a solid colour, with transparent tiles left out. A canvas on
//! eframe's wgpu backend (`eframe` feature `wgpu`; egui 0.29 uses wgpu 22,
//! the version this crate links) draws it like this:
//!
//! 1. Build the renderer on eframe's device:
//!    `GpuRenderer::with_device(rs.device.clone(), rs.queue.clone())` with
//!    `rs = frame.wgpu_render_state()`, and store it in
//!    `rs.renderer.write().callback_resources`.
//! 2. Paint the canvas with `egui_wgpu::Callback::new_paint_callback(rect,
//!    CanvasCallback { .. })`. In `CallbackTrait::prepare`, call
//!    `render_gpu` for the visible document rect (cheap when nothing
//!    changed: every tile is a cache hit and nothing is dispatched) and
//!    build one bind group per tile texture.
//! 3. In `paint`, draw one quad per tile with a small render pipeline whose
//!    fragment shader `textureLoad`s the texel (`Rgba32Float` is not
//!    filterable without `FLOAT32_FILTERABLE`, so magnified views sample
//!    nearest or filter in the shader), composites it over the
//!    transparency checkerboard, and encodes linear light to the
//!    surface's sRGB format. Solid tiles draw as flat quads.
//!
//! The `GpuImage` holds `Arc`s to its textures, so keeping it until the
//! frame is presented is enough to keep them alive, whatever the cache
//! evicts meanwhile.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, Weak};

use lumenply_doc::adjust::LUT_SIZE;
use lumenply_doc::{Adjustment, BlendMode, CompiledAdjustment};
use lumenply_tiles::{Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;
use wgpu::util::DeviceExt;

use crate::blob::{BlobStore, Hash};
use crate::eval::{Ctx, Renderer};
use crate::key::Key;
use crate::model::{Graph, Node, NodeId};
use crate::ops::Op;

const SIDE: u32 = TILE_SIZE as u32;
/// Workgroup edge; a tile is `SIDE / WG` workgroups on each side.
const WG: u32 = 8;
/// Bytes of `Params` in the shader, and the stride between them in the
/// per-pass uniform buffer (the dynamic-offset alignment).
const PARAMS_SIZE: u64 = 96;
const PARAMS_STRIDE: u64 = 256;
/// Tiles per readback buffer (1 MiB each as `Rgba32Float`).
const READBACK_CHUNK: usize = 64;
/// Tiles per upload staging buffer.
const UPLOAD_CHUNK: usize = 64;
/// Free textures kept per format for reuse.
const POOL_MAX: usize = 256;

/// The executor's kernels. [`lumenply_render::gpu::BLEND_WGSL`] is
/// prepended (it supplies `blend_pixel` and `blend_color`).
const KERNELS: &str = r#"
struct Params {
    // x, y: inputs 0 and 1 (0 transparent, 1 texture, 2 solid colour);
    // z: the mask (0 none, 1 texture, 2 solid); w: blend mode index.
    kinds: vec4u,
    // x: 1 when the table holds separate R, G and B curves.
    opts: vec4u,
    // x: opacity.
    amount: vec4f,
    solid0: vec4f,
    solid1: vec4f,
    solid2: vec4f,
}

@group(0) @binding(0) var in0: texture_2d<f32>;
@group(0) @binding(1) var in1: texture_2d<f32>;
@group(0) @binding(2) var in2: texture_2d<f32>;
@group(0) @binding(3) var out_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read> lut: array<f32>;

const LUT_SIZE: u32 = LUT_SIZE_VALUEu;

fn input0(p: vec2i) -> vec4f {
    switch params.kinds.x {
        case 1u: { return textureLoad(in0, p, 0); }
        case 2u: { return params.solid0; }
        default: { return vec4f(0.0); }
    }
}

fn input1(p: vec2i) -> vec4f {
    switch params.kinds.y {
        case 1u: { return textureLoad(in1, p, 0); }
        case 2u: { return params.solid1; }
        default: { return vec4f(0.0); }
    }
}

// Mask coverage: the mask's alpha; 1 without a mask.
fn coverage(p: vec2i) -> f32 {
    switch params.kinds.z {
        case 1u: { return textureLoad(in2, p, 0).a; }
        case 2u: { return params.solid2.a; }
        default: { return 1.0; }
    }
}

// `layer` without effects: the content scaled by the mask (apply_mask),
// then blended onto the backdrop at opacity × fill (blend_pixel).
@compute @workgroup_size(8, 8)
fn layer_main(@builtin(global_invocation_id) gid: vec3u) {
    let p = vec2i(gid.xy);
    let s = input1(p) * coverage(p);
    textureStore(out_tex, p, blend_pixel(input0(p), s, params.kinds.w, params.amount.x));
}

// lut_lookup in lumenply-doc, on table `channel` when there are three.
fn lut_at(channel: u32, x: f32) -> f32 {
    var base = 0u;
    if params.opts.x != 0u {
        base = channel * LUT_SIZE;
    }
    let q = clamp(x, 0.0, 1.0) * f32(LUT_SIZE - 1u);
    let i = u32(q);
    if i >= LUT_SIZE - 1u {
        return lut[base + LUT_SIZE - 1u];
    }
    let t = q - f32(i);
    return lut[base + i] + (lut[base + i + 1u] - lut[base + i]) * t;
}

// `adjustment` compiled to tables: adjust_in_place. The straight colour
// goes through the tables, is blended against itself in the layer's mode
// and mixed in by opacity × mask; alpha is kept.
@compute @workgroup_size(8, 8)
fn lut_main(@builtin(global_invocation_id) gid: vec3u) {
    let p = vec2i(gid.xy);
    let b = input0(p);
    let w = params.amount.x * coverage(p);
    if b.a <= 0.0 || w <= 0.0 {
        textureStore(out_tex, p, b);
        return;
    }
    let c = b.rgb / b.a;
    let adjusted = vec3f(lut_at(0u, c.x), lut_at(1u, c.y), lut_at(2u, c.z));
    let m = blend_color(params.kinds.w, c, adjusted);
    textureStore(out_tex, p, vec4f((c + (m - c) * w) * b.a, b.a));
}

// `pass-through`: mix_tiles, `after` over `before` by opacity × mask.
@compute @workgroup_size(8, 8)
fn mix_main(@builtin(global_invocation_id) gid: vec3u) {
    let p = vec2i(gid.xy);
    let before = input0(p);
    let w = params.amount.x * coverage(p);
    textureStore(out_tex, p, before + (input1(p) - before) * w);
}
"#;

/// How a texture stores its texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TexFormat {
    /// Premultiplied linear f32: every computed tile, and f32 blob tiles.
    F32,
    /// A compact (16-bit) blob tile uploaded as stored.
    U16,
}

impl TexFormat {
    fn wgpu(self) -> wgpu::TextureFormat {
        match self {
            TexFormat::F32 => wgpu::TextureFormat::Rgba32Float,
            TexFormat::U16 => wgpu::TextureFormat::Rgba16Unorm,
        }
    }

    fn pixel_bytes(self) -> u32 {
        match self {
            TexFormat::F32 => 16,
            TexFormat::U16 => 8,
        }
    }

    fn tile_bytes(self) -> usize {
        (SIDE * SIDE * self.pixel_bytes()) as usize
    }
}

struct TexInner {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

type Recycle = Arc<Mutex<Vec<(TexFormat, TexInner)>>>;

/// One 256×256 tile texture. Dropping the last reference hands the texture
/// back to the renderer for reuse after the next submit.
pub struct GpuTexture {
    inner: Option<TexInner>,
    format: TexFormat,
    recycle: Recycle,
}

impl GpuTexture {
    pub fn texture(&self) -> &wgpu::Texture {
        &self.inner.as_ref().expect("live texture").texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.inner.as_ref().expect("live texture").view
    }

    /// `Rgba32Float`, or `Rgba16Unorm` for a compact image tile.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format.wgpu()
    }
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if let Ok(mut r) = self.recycle.lock() {
                r.push((self.format, inner));
            }
        }
    }
}

/// One tile on the GPU.
#[derive(Clone)]
pub enum GpuTile {
    /// Premultiplied linear RGBA in a 256×256 texture.
    Texture(Arc<GpuTexture>),
    /// The same premultiplied colour everywhere (a mask's default coverage).
    Solid([f32; 4]),
}

/// A render left on the GPU: the non-transparent tiles of the requested
/// area. See the module docs for drawing it.
pub struct GpuImage {
    pub tiles: Vec<(TileCoord, GpuTile)>,
}

/// Counters for tests, the CLI and benchmarks (cumulative; see
/// [`GpuRenderer::reset_stats`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuStats {
    /// Compute dispatches (one per tile a kernel produced).
    pub dispatches: u64,
    pub uploaded_tiles: u64,
    pub uploaded_bytes: u64,
    pub read_back_tiles: u64,
    pub read_back_bytes: u64,
    /// Tiles evaluated by the CPU fallback, by op type.
    pub fallback_tiles: BTreeMap<&'static str, u64>,
    /// GPU cache lookups while planning.
    pub hits: u64,
    pub misses: u64,
    /// What the GPU cache holds now.
    pub cached_tiles: usize,
    pub cached_bytes: usize,
}

impl GpuStats {
    pub fn cpu_tiles(&self) -> u64 {
        self.fallback_tiles.values().sum()
    }
}

struct Slot {
    tile: Option<GpuTile>,
    last_use: u64,
    bytes: usize,
}

/// GPU tiles keyed by `(content key, tile)` under a byte budget with
/// least-recently-used eviction, like [`crate::TileCache`].
struct GpuCache {
    slots: HashMap<(Key, TileCoord), Slot>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl GpuCache {
    fn get(&mut self, key: Key, c: TileCoord) -> Option<Option<GpuTile>> {
        self.clock += 1;
        let now = self.clock;
        self.slots.get_mut(&(key, c)).map(|s| {
            s.last_use = now;
            s.tile.clone()
        })
    }

    /// `bytes`: what the entry costs (0 for a tile shared with another
    /// entry, such as a backdrop passed through).
    fn insert(&mut self, key: Key, c: TileCoord, tile: Option<GpuTile>, bytes: usize) {
        self.clock += 1;
        let slot = Slot {
            tile,
            last_use: self.clock,
            bytes,
        };
        if let Some(old) = self.slots.insert((key, c), slot) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        if self.bytes > self.budget {
            self.evict();
        }
    }

    /// Drop least-recently-used tiles until under 90% of the budget.
    fn evict(&mut self) {
        let target = self.budget / 10 * 9;
        let mut order: Vec<((Key, TileCoord), u64)> =
            self.slots.iter().map(|(k, s)| (*k, s.last_use)).collect();
        order.sort_by_key(|(_, t)| *t);
        for (k, _) in order {
            if self.bytes <= target {
                break;
            }
            if let Some(s) = self.slots.remove(&k) {
                self.bytes -= s.bytes;
            }
        }
    }
}

/// What a node does on the GPU.
#[derive(Clone)]
enum Kind {
    /// `empty`, `image`, `mask`: uploaded (or solid) tiles.
    Leaf,
    /// A hidden layer of any kind: port 0 passes through.
    Pass,
    Layer {
        mode: u32,
        opacity: f32,
    },
    Lut {
        mode: u32,
        opacity: f32,
        table: Arc<wgpu::Buffer>,
        rgb: bool,
    },
    Mix {
        opacity: f32,
    },
    /// No kernel: the CPU evaluates it.
    Cpu,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Gpu,
    Cpu,
}

/// One render's plan and working set.
struct Run {
    root: NodeId,
    kinds: HashMap<NodeId, Kind>,
    /// Tiles each node must produce on the GPU.
    gpu: HashMap<NodeId, BTreeSet<TileCoord>>,
    /// Tiles each node must have in the CPU cache.
    cpu: HashMap<NodeId, BTreeSet<TileCoord>>,
    /// Consumers still to read each GPU tile.
    uses: HashMap<(NodeId, TileCoord), u32>,
    /// GPU tiles pinned until their consumers have run.
    res: HashMap<(NodeId, TileCoord), Option<GpuTile>>,
    /// The root's tiles produced on the CPU side (a fallback root, or a
    /// readback), for `render_to_store`.
    cpu_out: HashMap<TileCoord, Option<Arc<Tile>>>,
}

/// Which pipeline a dispatch runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pipe {
    Layer,
    Lut,
    Mix,
}

struct Dispatch {
    coord: TileCoord,
    pipe: Pipe,
    in0: Option<GpuTile>,
    in1: Option<GpuTile>,
    mask: Option<GpuTile>,
    mode: u32,
    opacity: f32,
    lut: Option<(Arc<wgpu::Buffer>, bool)>,
}

/// What a kernel node's tile turns out to be.
enum Outcome {
    /// An input's tile, unchanged (or transparent).
    Same(Option<GpuTile>),
    Run(Dispatch),
}

/// Renders graphs on the GPU, keeping tiles resident between renders. One
/// renderer serves every version of a document, like [`Renderer`].
pub struct GpuRenderer {
    /// Evaluates the nodes without a kernel; its cache also receives the
    /// GPU tiles those nodes read (same content keys).
    cpu: Renderer,
    core: Core,
}

struct Core {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    layout: wgpu::BindGroupLayout,
    layer_pipe: wgpu::ComputePipeline,
    lut_pipe: wgpu::ComputePipeline,
    mix_pipe: wgpu::ComputePipeline,
    /// Bound to inputs that are absent or solid (the kernel never reads it).
    _dummy: wgpu::Texture,
    dummy_view: wgpu::TextureView,
    dummy_lut: wgpu::Buffer,
    unorm16: bool,
    cache: GpuCache,
    /// Blob and CPU tiles already on the GPU, by the tile's address (the
    /// `Arc` kept alongside stops the address being reused).
    uploads: HashMap<usize, (Arc<Tile>, Weak<GpuTexture>)>,
    /// Compiled adjustment tables by the hash of the adjustment.
    luts: HashMap<Hash, Option<(Arc<wgpu::Buffer>, bool)>>,
    free: HashMap<TexFormat, Vec<TexInner>>,
    recycle: Recycle,
    stats: GpuStats,
}

impl GpuRenderer {
    /// A renderer on its own headless device; `None` when no adapter is
    /// available (CI without a GPU, say).
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))?;
        let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("lumenply-graph"),
                required_features: features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .ok()?;
        Some(Self::with_device(Arc::new(device), Arc::new(queue)))
    }

    /// A renderer on an existing device, such as eframe's
    /// (`RenderState::device` and `queue`), so its tiles can be drawn
    /// without a readback. Compact tiles upload as 16-bit when the device
    /// was created with `TEXTURE_FORMAT_16BIT_NORM`, as f32 otherwise.
    pub fn with_device(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let source = format!(
            "{}{}",
            lumenply_render::gpu::BLEND_WGSL,
            KERNELS.replace("LUT_SIZE_VALUE", &LUT_SIZE.to_string())
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("graph-kernels"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        use wgpu::BindingType as BT;
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: BT::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("graph-kernels"),
            entries: &[
                tex(0),
                tex(1),
                tex(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: BT::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: BT::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(PARAMS_SIZE),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: BT::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("graph-kernels"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                module: &module,
                entry_point: entry,
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (layer_pipe, lut_pipe, mix_pipe) =
            (pipeline("layer_main"), pipeline("lut_main"), pipeline("mix_main"));
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("graph-dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_view = dummy.create_view(&Default::default());
        let dummy_lut = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("graph-dummy-lut"),
            contents: &[0u8; 16],
            usage: wgpu::BufferUsages::STORAGE,
        });
        let unorm16 = device
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        GpuRenderer {
            cpu: Renderer::new(),
            core: Core {
                device,
                queue,
                layout,
                layer_pipe,
                lut_pipe,
                mix_pipe,
                _dummy: dummy,
                dummy_view,
                dummy_lut,
                unorm16,
                cache: GpuCache {
                    slots: HashMap::new(),
                    bytes: 0,
                    budget: 2 << 30,
                    clock: 0,
                },
                uploads: HashMap::new(),
                luts: HashMap::new(),
                free: HashMap::new(),
                recycle: Arc::new(Mutex::new(Vec::new())),
                stats: GpuStats::default(),
            },
        }
    }

    pub fn device(&self) -> &Arc<wgpu::Device> {
        &self.core.device
    }

    pub fn queue(&self) -> &Arc<wgpu::Queue> {
        &self.core.queue
    }

    /// GPU memory the tile cache may use (default 2 GiB).
    pub fn set_budget(&mut self, bytes: usize) {
        self.core.cache.budget = bytes;
        if self.core.cache.bytes > bytes {
            self.core.cache.evict();
        }
    }

    /// CPU memory for the fallback evaluator's cache (default 1 GiB): the
    /// fallback nodes' tiles and the GPU tiles read back for them.
    pub fn set_cpu_budget(&self, bytes: usize) {
        self.cpu.cache.set_budget(bytes);
    }

    pub fn stats(&self) -> GpuStats {
        let mut s = self.core.stats.clone();
        s.cached_tiles = self.core.cache.slots.len();
        s.cached_bytes = self.core.cache.bytes;
        s
    }

    pub fn reset_stats(&mut self) {
        self.core.stats = GpuStats::default();
    }

    /// Drop every cached tile, GPU and CPU.
    pub fn clear(&mut self) {
        self.core.cache.slots.clear();
        self.core.cache.bytes = 0;
        self.core.uploads.clear();
        self.cpu.cache.clear();
    }

    /// Forget memoised hashes and uploads nothing uses any more.
    pub fn prune(&mut self) {
        self.cpu.prune();
        self.core.uploads.retain(|_, (_, w)| w.strong_count() > 0);
    }

    /// Wait until the GPU has finished everything submitted so far (for
    /// timing; results are correct without it).
    pub fn finish(&self) {
        self.core.device.poll(wgpu::Maintain::Wait);
    }

    /// `node`'s output over the tiles touching `rect`, left on the GPU.
    pub fn render_node_gpu(
        &mut self,
        graph: &Graph,
        blobs: &BlobStore,
        node: NodeId,
        rect: Rect,
    ) -> GpuImage {
        let ctx = self.cpu.ctx(graph, blobs, node);
        let coords = rect.tiles();
        let run = self.core.run(&ctx, node, &coords, Side::Gpu);
        let tiles = coords
            .iter()
            .filter_map(|c| run.res.get(&(node, *c)).cloned().flatten().map(|t| (*c, t)))
            .collect();
        GpuImage { tiles }
    }

    /// The image (the output node) over the tiles touching `rect`, left on
    /// the GPU; empty when the graph has no output.
    pub fn render_gpu(&mut self, graph: &Graph, blobs: &BlobStore, rect: Rect) -> GpuImage {
        match graph.output {
            Some(o) => self.render_node_gpu(graph, blobs, o, rect),
            None => GpuImage { tiles: Vec::new() },
        }
    }

    /// `node`'s output over the tiles touching `rect`, read back: the same
    /// result as [`Renderer::render_node`], within float tolerance. The
    /// tiles are also kept in the CPU cache, so asking again is free.
    pub fn render_node_to_store(
        &mut self,
        graph: &Graph,
        blobs: &BlobStore,
        node: NodeId,
        rect: Rect,
    ) -> TileStore {
        let ctx = self.cpu.ctx(graph, blobs, node);
        let coords = rect.tiles();
        let mut run = self.core.run(&ctx, node, &coords, Side::Cpu);
        let mut out = TileStore::new();
        for c in coords {
            let t = match run.cpu_out.remove(&c) {
                Some(t) => t,
                // Cached on the CPU side already (or evicted since: then
                // the CPU evaluator recomputes it, correctly if slowly).
                None => ctx.tile(Some(node), c),
            };
            if let Some(t) = t {
                out.insert(c, t);
            }
        }
        out
    }

    /// The image over the tiles touching `rect`, read back.
    pub fn render_to_store(&mut self, graph: &Graph, blobs: &BlobStore, rect: Rect) -> TileStore {
        match graph.output {
            Some(o) => self.render_node_to_store(graph, blobs, o, rect),
            None => TileStore::new(),
        }
    }

    /// The whole canvas, read back.
    pub fn render_canvas_to_store(&mut self, graph: &Graph, blobs: &BlobStore) -> TileStore {
        self.render_to_store(graph, blobs, graph.canvas())
    }

    /// Read a [`GpuImage`] back into CPU tiles.
    pub fn read_back(&mut self, image: &GpuImage) -> TileStore {
        let tiles: Vec<Option<GpuTile>> = image.tiles.iter().map(|(_, t)| Some(t.clone())).collect();
        let back = self.core.read_tiles(&tiles);
        let mut out = TileStore::new();
        for ((c, _), t) in image.tiles.iter().zip(back) {
            if let Some(t) = t {
                out.insert(*c, t);
            }
        }
        out
    }
}

fn adjustment_hash(a: &Adjustment) -> Hash {
    let json = serde_json::to_vec(a).expect("adjustments always serialise");
    Hash(*blake3::hash(&json).as_bytes())
}

fn f32_bytes(v: &[f32]) -> &[u8] {
    // SAFETY: f32 is plain old data with no padding.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// The uniform block for one dispatch (`Params` in the shader).
fn params(kinds: [u32; 4], rgb: bool, opacity: f32, solids: [[f32; 4]; 3]) -> [u8; PARAMS_SIZE as usize] {
    let mut words = [0u32; 24];
    words[..4].copy_from_slice(&kinds);
    words[4] = u32::from(rgb);
    words[8] = opacity.to_bits();
    for (i, s) in solids.iter().enumerate() {
        for (j, v) in s.iter().enumerate() {
            words[12 + 4 * i + j] = v.to_bits();
        }
    }
    let mut out = [0u8; PARAMS_SIZE as usize];
    for (o, w) in out.chunks_exact_mut(4).zip(words) {
        o.copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Shader input kind and solid colour of an input tile.
fn input_kind(t: &Option<GpuTile>) -> (u32, [f32; 4]) {
    match t {
        None => (0, [0.0; 4]),
        Some(GpuTile::Texture(_)) => (1, [0.0; 4]),
        Some(GpuTile::Solid(c)) => (2, *c),
    }
}

/// A CPU tile from a readback of `format` texels.
fn tile_from_bytes(format: TexFormat, data: &[u8]) -> Tile {
    let mut t = Tile::new();
    let px = t.pixels_mut();
    match format {
        TexFormat::F32 => {
            for (p, b) in px.iter_mut().zip(data.chunks_exact(16)) {
                let f = |i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
                *p = Rgba::new(f(0), f(4), f(8), f(12));
            }
        }
        TexFormat::U16 => {
            for (p, b) in px.iter_mut().zip(data.chunks_exact(8)) {
                // As lumenply-tiles expands compact tiles.
                let f = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as f32 / 65535.0;
                *p = Rgba::new(f(0), f(2), f(4), f(6));
            }
        }
    }
    t
}

impl Core {
    /// Plan and run one render of `root` over `coords`; the root's tiles
    /// end up in `res` (`Side::Gpu`) or `cpu_out` (`Side::Cpu`).
    fn run(&mut self, ctx: &Ctx, root: NodeId, coords: &[TileCoord], side: Side) -> Run {
        if self.luts.len() > 256 {
            self.luts.clear();
        }
        let mut run = Run {
            root,
            kinds: HashMap::new(),
            gpu: HashMap::new(),
            cpu: HashMap::new(),
            uses: HashMap::new(),
            res: HashMap::new(),
            cpu_out: HashMap::new(),
        };
        self.plan(ctx, &mut run, coords, side);
        for n in self.order(ctx, &run) {
            let Some(node) = ctx.graph.node(n) else {
                continue;
            };
            let key = ctx.keys[&n];
            match run.kinds[&n].clone() {
                Kind::Leaf => self.run_leaf(ctx, &mut run, n, node, key),
                Kind::Cpu => self.run_cpu(ctx, &mut run, n, node, key),
                kind => {
                    self.run_kernel(&mut run, n, node, key, &kind);
                    self.read_back_for_cpu(ctx, &mut run, n, key);
                }
            }
        }
        self.uploads.retain(|_, (_, w)| w.strong_count() > 0);
        run
    }

    fn kind(&mut self, ctx: &Ctx, run: &mut Run, id: NodeId) -> Kind {
        if let Some(k) = run.kinds.get(&id) {
            return k.clone();
        }
        let hidden = |visible: bool, opacity: f32| !visible || opacity <= 0.0;
        let k = match ctx.graph.node(id).map(|n| &n.op) {
            None => Kind::Leaf,
            Some(Op::Empty | Op::Image { .. } | Op::Mask { .. }) => Kind::Leaf,
            Some(Op::Layer { props }) => {
                if hidden(props.visible, props.opacity) {
                    Kind::Pass
                } else if props.effects.is_empty() && props.blend != BlendMode::Dissolve {
                    Kind::Layer {
                        mode: props.blend as u32,
                        opacity: props.opacity * props.fill_opacity,
                    }
                } else {
                    Kind::Cpu
                }
            }
            Some(Op::Adjustment {
                adjustment,
                visible,
                opacity,
                blend,
            }) => {
                if hidden(*visible, *opacity) {
                    Kind::Pass
                } else if *blend == BlendMode::Dissolve {
                    Kind::Cpu
                } else {
                    match self.lut(adjustment) {
                        Some((table, rgb)) => Kind::Lut {
                            mode: *blend as u32,
                            opacity: *opacity,
                            table,
                            rgb,
                        },
                        None => Kind::Cpu,
                    }
                }
            }
            Some(Op::PassThrough { visible, opacity }) => {
                if hidden(*visible, *opacity) {
                    Kind::Pass
                } else {
                    Kind::Mix { opacity: *opacity }
                }
            }
            Some(Op::FilterLayer { visible, opacity, .. }) if hidden(*visible, *opacity) => Kind::Pass,
            Some(Op::ClipGroup { base, .. }) if hidden(base.visible, base.opacity) => Kind::Pass,
            // Live filters, clip groups and any op without a kernel.
            Some(_) => Kind::Cpu,
        };
        run.kinds.insert(id, k.clone());
        k
    }

    /// The adjustment's tables on the GPU, or `None` when it doesn't
    /// compile to 1D tables (it then runs on the CPU).
    fn lut(&mut self, adjustment: &Adjustment) -> Option<(Arc<wgpu::Buffer>, bool)> {
        let h = adjustment_hash(adjustment);
        if let Some(v) = self.luts.get(&h) {
            return v.clone();
        }
        let upload = |data: &[f32]| {
            Arc::new(self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("graph-lut"),
                contents: f32_bytes(data),
                usage: wgpu::BufferUsages::STORAGE,
            }))
        };
        let v = match adjustment.compile() {
            CompiledAdjustment::Lut(t) => Some((upload(&t[..]), false)),
            CompiledAdjustment::LutRgb(t) => {
                let flat: Vec<f32> = t.iter().flatten().copied().collect();
                Some((upload(&flat), true))
            }
            _ => None,
        };
        self.luts.insert(h, v.clone());
        v
    }

    fn use_tile(run: &mut Run, n: NodeId, c: TileCoord) {
        *run.uses.entry((n, c)).or_default() += 1;
    }

    /// Drop one consumer of `(n, c)`; unpin the tile after the last.
    fn release(run: &mut Run, n: NodeId, c: TileCoord) {
        if let Some(u) = run.uses.get_mut(&(n, c)) {
            *u -= 1;
            if *u == 0 {
                run.uses.remove(&(n, c));
                run.res.remove(&(n, c));
            }
        }
    }

    /// Note every tile the render needs, stopping at cached ones.
    fn plan(&mut self, ctx: &Ctx, run: &mut Run, coords: &[TileCoord], side: Side) {
        let root = run.root;
        let mut stack: Vec<(NodeId, TileCoord, Side)> = Vec::new();
        for &c in coords {
            if side == Side::Gpu {
                // The caller reads these: they stay pinned to the end.
                Self::use_tile(run, root, c);
            }
            stack.push((root, c, side));
        }
        while let Some((n, c, side)) = stack.pop() {
            let (Some(&key), Some(node)) = (ctx.keys.get(&n), ctx.graph.node(n)) else {
                run.res.insert((n, c), None);
                continue;
            };
            match side {
                Side::Gpu => {
                    if run.res.contains_key(&(n, c)) || run.gpu.get(&n).is_some_and(|s| s.contains(&c)) {
                        continue;
                    }
                    if let Some(t) = self.cache.get(key, c) {
                        self.stats.hits += 1;
                        run.res.insert((n, c), t);
                        continue;
                    }
                    self.stats.misses += 1;
                    run.gpu.entry(n).or_default().insert(c);
                    let kind = self.kind(ctx, run, n);
                    let mut read = |port: usize| {
                        if let Some(i) = node.input(port) {
                            Self::use_tile(run, i, c);
                            stack.push((i, c, Side::Gpu));
                        }
                    };
                    match kind {
                        Kind::Leaf => {}
                        Kind::Pass => read(0),
                        Kind::Layer { .. } | Kind::Mix { .. } => {
                            read(0);
                            read(1);
                            read(2);
                        }
                        Kind::Lut { .. } => {
                            read(0);
                            read(1);
                        }
                        Kind::Cpu => stack.push((n, c, Side::Cpu)),
                    }
                }
                Side::Cpu => {
                    if run.cpu.get(&n).is_some_and(|s| s.contains(&c)) || ctx.cache.contains(key, c) {
                        continue;
                    }
                    run.cpu.entry(n).or_default().insert(c);
                    match self.kind(ctx, run, n) {
                        // The CPU reads blobs directly.
                        Kind::Leaf => {}
                        // What its CPU op reads, so the GPU computes it
                        // first (an op computed in one piece reads its
                        // inputs itself, on the CPU).
                        Kind::Cpu => {
                            for (i, area) in crate::ops::input_needs(ctx, node, c) {
                                for t in area.tiles() {
                                    stack.push((i, t, Side::Cpu));
                                }
                            }
                        }
                        // Computed on the GPU, then read back.
                        _ => {
                            Self::use_tile(run, n, c);
                            stack.push((n, c, Side::Gpu));
                        }
                    }
                }
            }
        }
    }

    /// Nodes with work, every input before its consumers: a depth-first
    /// post-order from the root, which interleaves a stack's layers with
    /// their contents so few tiles are pinned at once.
    fn order(&self, ctx: &Ctx, run: &Run) -> Vec<NodeId> {
        let busy = |n: &NodeId| run.gpu.contains_key(n) || run.cpu.contains_key(n);
        let mut out = Vec::new();
        let mut seen = HashSet::from([run.root]);
        let mut stack = vec![(run.root, 0usize)];
        while let Some(&(n, port)) = stack.last() {
            let inputs = ctx.graph.node(n).map_or(&[][..], |node| &node.inputs[..]);
            if port == inputs.len() {
                stack.pop();
                if busy(&n) {
                    out.push(n);
                }
                continue;
            }
            stack.last_mut().unwrap().1 += 1;
            if let Some(i) = inputs[port] {
                if seen.insert(i) {
                    stack.push((i, 0));
                }
            }
        }
        out
    }

    fn pin(&mut self, run: &mut Run, n: NodeId, key: Key, c: TileCoord, tile: Option<GpuTile>, bytes: usize) {
        self.cache.insert(key, c, tile.clone(), bytes);
        if run.uses.contains_key(&(n, c)) {
            run.res.insert((n, c), tile);
        }
    }

    fn run_leaf(&mut self, ctx: &Ctx, run: &mut Run, n: NodeId, node: &Node, key: Key) {
        let coords: Vec<TileCoord> = run
            .gpu
            .get(&n)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let (blob, default) = match &node.op {
            Op::Image { blob } => (Some(blob), 0.0),
            Op::Mask { blob, default } => (blob.as_ref(), default.min(1.0)),
            _ => (None, 0.0),
        };
        let store = blob.and_then(|b| ctx.blob(b));
        let tiles: Vec<Option<Arc<Tile>>> = coords
            .iter()
            .map(|c| store.and_then(|s| s.tile_arc(*c)).cloned())
            .collect();
        let uploaded = self.upload(&tiles);
        for (c, (tile, bytes)) in coords.into_iter().zip(uploaded) {
            // A mask without a tile here covers at its default.
            let tile = match tile {
                None if default > 0.0 => Some(GpuTile::Solid([default; 4])),
                t => t,
            };
            self.pin(run, n, key, c, tile, bytes);
        }
        self.reclaim();
    }

    /// Evaluate a node without a kernel on the CPU (in parallel over its
    /// tiles), uploading the tiles the GPU needs.
    fn run_cpu(&mut self, ctx: &Ctx, run: &mut Run, n: NodeId, node: &Node, key: Key) {
        let on_gpu = run.gpu.get(&n).cloned().unwrap_or_default();
        let mut want: BTreeSet<TileCoord> = run.cpu.get(&n).cloned().unwrap_or_default();
        want.extend(on_gpu.iter().copied());
        let want: Vec<TileCoord> = want.into_iter().collect();
        // An op computed in one piece (text, a fill, a smart filter) is
        // made once, with the pieces it reads, before its tiles are served.
        if crate::ops_content::is_whole(&node.op) {
            crate::ops_content::prepare(ctx, n);
        }
        let tiles: Vec<(TileCoord, Option<Arc<Tile>>)> =
            want.par_iter().map(|&c| (c, ctx.tile(Some(n), c))).collect();
        *self.stats.fallback_tiles.entry(node.op.type_name()).or_default() += tiles.len() as u64;
        let (up_coords, up_tiles): (Vec<TileCoord>, Vec<Option<Arc<Tile>>>) =
            tiles.iter().filter(|(c, _)| on_gpu.contains(c)).cloned().unzip();
        let uploaded = self.upload(&up_tiles);
        for (c, (g, bytes)) in up_coords.into_iter().zip(uploaded) {
            self.pin(run, n, key, c, g, bytes);
        }
        if n == run.root {
            run.cpu_out.extend(tiles);
        }
        self.reclaim();
    }

    /// One compute pass over every tile a kernel node must produce.
    fn run_kernel(&mut self, run: &mut Run, n: NodeId, node: &Node, key: Key, kind: &Kind) {
        let coords: Vec<TileCoord> = run
            .gpu
            .get(&n)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if coords.is_empty() {
            return;
        }
        let ports = node.inputs.len().min(3);
        let mut jobs = Vec::new();
        for c in coords {
            let get = |port: usize| -> Option<GpuTile> {
                let id = node.input(port)?;
                match run.res.get(&(id, c)) {
                    Some(t) => t.clone(),
                    None => panic!("GPU graph executor: input {id} of {n} at {c:?} was not planned"),
                }
            };
            // The mask port's tile; `Err` = connected but empty here.
            let mask_at = |port: usize| -> Result<Option<GpuTile>, ()> {
                match node.input(port) {
                    None => Ok(None),
                    Some(_) => get(port).map(Some).ok_or(()),
                }
            };
            let outcome = match kind {
                Kind::Pass => Outcome::Same(get(0)),
                Kind::Layer { mode, opacity } => {
                    let backdrop = get(0);
                    match (node.input(1), get(1), mask_at(2)) {
                        (None, _, _) => Outcome::Same(None),
                        (_, None, _) | (_, _, Err(())) => Outcome::Same(backdrop),
                        (_, Some(s), Ok(mask)) => Outcome::Run(Dispatch {
                            coord: c,
                            pipe: Pipe::Layer,
                            in0: backdrop,
                            in1: Some(s),
                            mask,
                            mode: *mode,
                            opacity: *opacity,
                            lut: None,
                        }),
                    }
                }
                Kind::Lut {
                    mode,
                    opacity,
                    table,
                    rgb,
                } => match (get(0), mask_at(1)) {
                    (None, _) => Outcome::Same(None),
                    (b, Err(())) => Outcome::Same(b),
                    (b, Ok(mask)) => Outcome::Run(Dispatch {
                        coord: c,
                        pipe: Pipe::Lut,
                        in0: b,
                        in1: None,
                        mask,
                        mode: *mode,
                        opacity: *opacity,
                        lut: Some((table.clone(), *rgb)),
                    }),
                },
                Kind::Mix { opacity } => {
                    let (before, after) = (get(0), get(1));
                    match mask_at(2) {
                        Err(()) => Outcome::Same(before),
                        Ok(None) if *opacity >= 1.0 => Outcome::Same(after),
                        Ok(_) if before.is_none() && after.is_none() => Outcome::Same(None),
                        Ok(mask) => Outcome::Run(Dispatch {
                            coord: c,
                            pipe: Pipe::Mix,
                            in0: before,
                            in1: after,
                            mask,
                            mode: 0,
                            opacity: *opacity,
                            lut: None,
                        }),
                    }
                }
                Kind::Leaf | Kind::Cpu => unreachable!("not a kernel"),
            };
            let reads = match kind {
                Kind::Pass => 1,
                Kind::Lut { .. } => 2,
                _ => 3,
            };
            for port in 0..reads.min(ports) {
                if let Some(i) = node.input(port) {
                    Self::release(run, i, c);
                }
            }
            match outcome {
                Outcome::Same(t) => self.pin(run, n, key, c, t, 0),
                Outcome::Run(d) => jobs.push(d),
            }
        }
        if jobs.is_empty() {
            return;
        }
        let outs = self.dispatch(&jobs);
        for (d, out) in jobs.iter().zip(outs) {
            self.pin(
                run,
                n,
                key,
                d.coord,
                Some(GpuTile::Texture(out)),
                TexFormat::F32.tile_bytes(),
            );
        }
    }

    /// Encode and submit one pass running `jobs`; returns their outputs.
    fn dispatch(&mut self, jobs: &[Dispatch]) -> Vec<Arc<GpuTexture>> {
        let mut uniforms = vec![0u8; jobs.len() * PARAMS_STRIDE as usize];
        for (i, d) in jobs.iter().enumerate() {
            let (k0, s0) = input_kind(&d.in0);
            let (k1, s1) = input_kind(&d.in1);
            let (k2, s2) = input_kind(&d.mask);
            let rgb = d.lut.as_ref().is_some_and(|(_, rgb)| *rgb);
            let p = params([k0, k1, k2, d.mode], rgb, d.opacity, [s0, s1, s2]);
            let at = i * PARAMS_STRIDE as usize;
            uniforms[at..at + PARAMS_SIZE as usize].copy_from_slice(&p);
        }
        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("graph-params"),
            contents: &uniforms,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let outs: Vec<Arc<GpuTexture>> = jobs.iter().map(|_| self.alloc(TexFormat::F32)).collect();
        let binds: Vec<wgpu::BindGroup> = jobs
            .iter()
            .zip(&outs)
            .map(|(d, out)| {
                fn view<'a>(t: &'a Option<GpuTile>, dummy: &'a wgpu::TextureView) -> &'a wgpu::TextureView {
                    match t {
                        Some(GpuTile::Texture(t)) => t.view(),
                        _ => dummy,
                    }
                }
                let view = |t| view(t, &self.dummy_view);
                let lut = d.lut.as_ref().map_or(&self.dummy_lut, |(b, _)| b.as_ref());
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view(&d.in0)),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(view(&d.in1)),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(view(&d.mask)),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(out.view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &ubuf,
                                offset: 0,
                                size: NonZeroU64::new(PARAMS_SIZE),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: lut.as_entire_binding(),
                        },
                    ],
                })
            })
            .collect();
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("graph-node"),
                timestamp_writes: None,
            });
            let mut current = None;
            for (i, (d, bind)) in jobs.iter().zip(&binds).enumerate() {
                if current != Some(d.pipe) {
                    current = Some(d.pipe);
                    pass.set_pipeline(match d.pipe {
                        Pipe::Layer => &self.layer_pipe,
                        Pipe::Lut => &self.lut_pipe,
                        Pipe::Mix => &self.mix_pipe,
                    });
                }
                pass.set_bind_group(0, bind, &[(i as u64 * PARAMS_STRIDE) as u32]);
                pass.dispatch_workgroups(SIDE / WG, SIDE / WG, 1);
            }
        }
        self.queue.submit([enc.finish()]);
        self.stats.dispatches += jobs.len() as u64;
        self.reclaim();
        outs
    }

    /// Read back the tiles of a kernel node that fallback nodes read, into
    /// the CPU cache under the node's key.
    fn read_back_for_cpu(&mut self, ctx: &Ctx, run: &mut Run, n: NodeId, key: Key) {
        let coords: Vec<TileCoord> = run
            .cpu
            .get(&n)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if coords.is_empty() {
            return;
        }
        let tiles: Vec<Option<GpuTile>> = coords
            .iter()
            .map(|c| run.res.get(&(n, *c)).cloned().flatten())
            .collect();
        let back = self.read_tiles(&tiles);
        for ((c, g), t) in coords.iter().zip(&tiles).zip(back) {
            // Remember where it came from: a CPU op returning this tile
            // unchanged (a backdrop passed through) needs no upload.
            if let (Some(GpuTile::Texture(g)), Some(t)) = (g, &t) {
                self.uploads
                    .insert(Arc::as_ptr(t) as usize, (t.clone(), Arc::downgrade(g)));
            }
            let t = ctx.cache.get_or_compute(key, *c, || t);
            if n == run.root {
                run.cpu_out.insert(*c, t);
            }
            Self::release(run, n, *c);
        }
    }

    /// CPU tiles of GPU tiles, read back in batches.
    fn read_tiles(&mut self, tiles: &[Option<GpuTile>]) -> Vec<Option<Arc<Tile>>> {
        let mut out: Vec<Option<Arc<Tile>>> = vec![None; tiles.len()];
        let textured: Vec<usize> = tiles
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match t {
                Some(GpuTile::Texture(_)) => Some(i),
                Some(GpuTile::Solid(c)) => {
                    out[i] = Some(Arc::new(Tile::filled(Rgba::new(c[0], c[1], c[2], c[3]))));
                    None
                }
                None => None,
            })
            .collect();
        for chunk in textured.chunks(READBACK_CHUNK) {
            let texs: Vec<&Arc<GpuTexture>> = chunk
                .iter()
                .map(|&i| match &tiles[i] {
                    Some(GpuTile::Texture(t)) => t,
                    _ => unreachable!(),
                })
                .collect();
            let mut offsets = Vec::with_capacity(texs.len());
            let mut size = 0u64;
            for t in &texs {
                offsets.push(size);
                size += t.format.tile_bytes() as u64;
            }
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("graph-readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = self.device.create_command_encoder(&Default::default());
            for (t, off) in texs.iter().zip(&offsets) {
                enc.copy_texture_to_buffer(
                    t.texture().as_image_copy(),
                    wgpu::ImageCopyBuffer {
                        buffer: &buf,
                        layout: wgpu::ImageDataLayout {
                            offset: *off,
                            bytes_per_row: Some(SIDE * t.format.pixel_bytes()),
                            rows_per_image: Some(SIDE),
                        },
                    },
                    wgpu::Extent3d {
                        width: SIDE,
                        height: SIDE,
                        depth_or_array_layers: 1,
                    },
                );
            }
            self.queue.submit([enc.finish()]);
            let slice = buf.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.device.poll(wgpu::Maintain::Wait);
            rx.recv().expect("map callback").expect("map readback buffer");
            {
                let view = slice.get_mapped_range();
                let data: &[u8] = &view;
                let made: Vec<Tile> = texs
                    .par_iter()
                    .zip(&offsets)
                    .map(|(t, &off)| {
                        let n = t.format.tile_bytes();
                        tile_from_bytes(t.format, &data[off as usize..off as usize + n])
                    })
                    .collect();
                for (&i, t) in chunk.iter().zip(made) {
                    out[i] = Some(Arc::new(t));
                }
            }
            buf.unmap();
            self.stats.read_back_tiles += texs.len() as u64;
            self.stats.read_back_bytes += size;
        }
        self.reclaim();
        out
    }

    /// CPU tiles on the GPU. A tile already uploaded or read back (the very
    /// same `Arc`) reuses its texture; the rest are copied into one mapped
    /// staging buffer per batch, in parallel, and from there into their
    /// textures. Returns each tile and the bytes it newly occupies.
    fn upload(&mut self, tiles: &[Option<Arc<Tile>>]) -> Vec<(Option<GpuTile>, usize)> {
        let mut out: Vec<(Option<GpuTile>, usize)> = Vec::with_capacity(tiles.len());
        let mut todo: Vec<(usize, TexFormat)> = Vec::new();
        for (i, t) in tiles.iter().enumerate() {
            let Some(t) = t else {
                out.push((None, 0));
                continue;
            };
            let addr = Arc::as_ptr(t) as usize;
            let known = self
                .uploads
                .get(&addr)
                .filter(|(kept, _)| Arc::ptr_eq(kept, t))
                .and_then(|(_, weak)| weak.upgrade());
            match known {
                Some(tex) => out.push((Some(GpuTile::Texture(tex)), 0)),
                None => {
                    let format = if t.is_compact() && self.unorm16 {
                        TexFormat::U16
                    } else {
                        TexFormat::F32
                    };
                    todo.push((i, format));
                    out.push((None, format.tile_bytes()));
                }
            }
        }
        for batch in todo.chunks(UPLOAD_CHUNK) {
            let mut offsets = Vec::with_capacity(batch.len());
            let mut size = 0u64;
            for (_, f) in batch {
                offsets.push(size);
                size += f.tile_bytes() as u64;
            }
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("graph-upload"),
                size,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            });
            {
                let mut view = staging.slice(..).get_mapped_range_mut();
                let mut rest: &mut [u8] = &mut view;
                let mut parts = Vec::with_capacity(batch.len());
                for (_, f) in batch {
                    let (part, tail) = rest.split_at_mut(f.tile_bytes());
                    parts.push(part);
                    rest = tail;
                }
                parts.into_par_iter().zip(batch).for_each(|(part, (i, f))| {
                    let tile = tiles[*i].as_ref().expect("listed tiles exist");
                    let (tag, raw) = tile.raw_bytes();
                    match (f, tag) {
                        (TexFormat::U16, _) | (TexFormat::F32, 0) => part.copy_from_slice(raw),
                        // A compact tile without 16-bit textures: expand.
                        (TexFormat::F32, _) => {
                            for (d, p) in part.chunks_exact_mut(16).zip(tile.pixels().iter()) {
                                for (k, v) in [p.r, p.g, p.b, p.a].into_iter().enumerate() {
                                    d[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
                                }
                            }
                        }
                    }
                });
            }
            staging.unmap();
            let mut enc = self.device.create_command_encoder(&Default::default());
            for ((i, f), off) in batch.iter().zip(&offsets) {
                let tex = self.alloc(*f);
                enc.copy_buffer_to_texture(
                    wgpu::ImageCopyBuffer {
                        buffer: &staging,
                        layout: wgpu::ImageDataLayout {
                            offset: *off,
                            bytes_per_row: Some(SIDE * f.pixel_bytes()),
                            rows_per_image: Some(SIDE),
                        },
                    },
                    tex.texture().as_image_copy(),
                    wgpu::Extent3d {
                        width: SIDE,
                        height: SIDE,
                        depth_or_array_layers: 1,
                    },
                );
                let tile = tiles[*i].as_ref().expect("listed tiles exist");
                self.uploads
                    .insert(Arc::as_ptr(tile) as usize, (tile.clone(), Arc::downgrade(&tex)));
                out[*i].0 = Some(GpuTile::Texture(tex));
            }
            self.queue.submit([enc.finish()]);
            self.stats.uploaded_tiles += batch.len() as u64;
            self.stats.uploaded_bytes += size;
        }
        out
    }

    /// A tile texture, reused from the pool when one is free.
    fn alloc(&mut self, format: TexFormat) -> Arc<GpuTexture> {
        let inner = self.free.get_mut(&format).and_then(Vec::pop).unwrap_or_else(|| {
            let usage = match format {
                TexFormat::F32 => {
                    wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                }
                TexFormat::U16 => {
                    wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                }
            };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("graph-tile"),
                size: wgpu::Extent3d {
                    width: SIDE,
                    height: SIDE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.wgpu(),
                usage,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            TexInner { texture, view }
        });
        Arc::new(GpuTexture {
            inner: Some(inner),
            format,
            recycle: self.recycle.clone(),
        })
    }

    /// Textures released before the last submit may be reused: every
    /// command reading them is already queued ahead of any later write.
    fn reclaim(&mut self) {
        let released: Vec<(TexFormat, TexInner)> = std::mem::take(&mut *self.recycle.lock().unwrap());
        for (format, inner) in released {
            let free = self.free.entry(format).or_default();
            if free.len() < POOL_MAX {
                free.push(inner);
            }
        }
    }
}

#[cfg(test)]
#[path = "gpu_tests.rs"]
mod tests;
