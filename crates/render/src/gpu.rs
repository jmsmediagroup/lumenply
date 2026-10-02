//! GPU compositor (wgpu compute), mirroring the CPU reference path.
//!
//! The CPU compositor stays the reference (see DEVELOPMENT.md); this path must
//! match it within a small tolerance, which the tests at the bottom check
//! kernel by kernel against `composite_rect`.
//!
//! Scope of the GPU path (anything else returns `None` and the caller uses
//! the CPU): pixel and text layers, masks, opacity, every blend mode but
//! Dissolve (its noise needs canvas coordinates; it stays on the CPU),
//! isolated and pass-through groups, and adjustment layers that compile to
//! a LUT in Normal blend mode (Levels, Curves, Invert, Brightness/Contrast,
//! Posterize — the slider-heavy ones, uploaded as a 1024-entry table).
//! Live filters and per-pixel adjustments stay on the CPU for now.
//!
//! The orchestration walks the layer tree on the CPU and records one
//! compute pass per layer into ping-ponged RGBA32F textures; sources are
//! uploaded per call. Persistent per-layer textures and dirty-rect uploads
//! are the planned next step once this path is wired into the app.

use lumenply_doc::{CompiledAdjustment, Document, Layer, LayerContent, Mask};
use lumenply_tiles::{Raster, Rect, TileStore};

#[cfg(test)]
use crate::composite_rect as cpu_composite_rect;

/// Largest rect side the GPU path will take on in one call.
const MAX_SIDE: u32 = 4096;
const WG: u32 = 8;

const SHADER: &str = r#"
struct Params {
    mode: u32,     // blend mode index, matches BlendMode order
    opacity: f32,
    has_mask: u32,
    _pad: u32,
}

@group(0) @binding(0) var acc_in: texture_2d<f32>;
@group(0) @binding(1) var acc_out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var src_tex: texture_2d<f32>;
@group(0) @binding(3) var mask_tex: texture_2d<f32>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read> lut: array<f32>;

fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        return cb * 2.0 * cs;
    }
    let s = 2.0 * cs - 1.0;
    return cb + s - cb * s;
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    }
    var d: f32;
    if cb <= 0.25 {
        d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
    } else {
        d = sqrt(cb);
    }
    return cb + (2.0 * cs - 1.0) * (d - cb);
}

// Mirrors blend::EDGE.
const EDGE: f32 = 1e-6;

fn color_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 - EDGE {
        return 1.0;
    }
    if cs <= EDGE {
        return 0.0;
    }
    return 1.0 - min((1.0 - cb) / cs, 1.0);
}

fn color_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= EDGE {
        return 0.0;
    }
    if cs >= 1.0 - EDGE {
        return 1.0;
    }
    return min(cb / (1.0 - cs), 1.0);
}

// Separable blend function B(Cb, Cs); indices match BlendMode's
// discriminants. Mirrors blend_channel + blend::extra_channel.
fn blend_channel(mode: u32, cb: f32, cs: f32) -> f32 {
    switch mode {
        case 0u: { return cs; }                         // Normal
        case 1u: { return cb * cs; }                    // Multiply
        case 2u: { return cb + cs - cb * cs; }          // Screen
        case 3u: { return hard_light(cs, cb); }         // Overlay
        case 4u: { return min(cb, cs); }                // Darken
        case 5u: { return max(cb, cs); }                // Lighten
        case 6u: { return abs(cb - cs); }               // Difference
        case 7u: { return min(cb + cs, 1.0); }          // Add
        case 8u: { return hard_light(cb, cs); }         // HardLight
        case 9u: { return soft_light(cb, cs); }         // SoftLight
        case 11u: { return color_burn(cb, cs); }        // ColorBurn
        case 12u: { return max(cb + cs - 1.0, 0.0); }   // LinearBurn
        case 14u: { return color_dodge(cb, cs); }       // ColorDodge
        case 16u: {                                     // VividLight
            if cs <= EDGE {
                return 0.0;
            }
            if cs >= 1.0 - EDGE {
                return 1.0;
            }
            if cs <= 0.5 {
                return color_burn(cb, 2.0 * cs);
            }
            return color_dodge(cb, 2.0 * cs - 1.0);
        }
        case 17u: { return clamp(cb + 2.0 * cs - 1.0, 0.0, 1.0); } // LinearLight
        case 18u: {                                     // PinLight
            if cs <= 0.5 {
                return min(cb, 2.0 * cs);
            }
            return max(cb, 2.0 * cs - 1.0);
        }
        case 19u: {                                     // HardMix
            if cs >= 1.0 - EDGE {
                return select(0.0, 1.0, cb > EDGE);
            }
            if cs <= EDGE {
                return select(0.0, 1.0, cb >= 1.0 - EDGE);
            }
            return select(0.0, 1.0, cb + cs >= 1.0 - EDGE);
        }
        case 20u: { return cb + cs - 2.0 * cb * cs; }   // Exclusion
        case 21u: { return max(cb - cs, 0.0); }         // Subtract
        case 22u: {                                     // Divide
            if cs <= 0.0 {
                return select(0.0, 1.0, cb > 0.0);
            }
            return min(cb / cs, 1.0);
        }
        default: { return cs; }                         // Dissolve (CPU only)
    }
}

// Non-separable modes (W3C SetLum/SetSat/ClipColor, Rec. 601 luma).
fn lum(c: vec3f) -> f32 {
    return 0.3 * c.x + 0.59 * c.y + 0.11 * c.z;
}

fn clip_color(c: vec3f) -> vec3f {
    let l = lum(c);
    let n = min(min(c.x, c.y), c.z);
    let x = max(max(c.x, c.y), c.z);
    var out = c;
    if n < 0.0 && l - n > 1e-12 {
        out = vec3f(l) + (out - vec3f(l)) * (l / (l - n));
    }
    if x > 1.0 && x - l > 1e-12 {
        out = vec3f(l) + (out - vec3f(l)) * ((1.0 - l) / (x - l));
    }
    return out;
}

fn set_lum(c: vec3f, l: f32) -> vec3f {
    return clip_color(c + vec3f(l - lum(c)));
}

fn sat(c: vec3f) -> f32 {
    return max(max(c.x, c.y), c.z) - min(min(c.x, c.y), c.z);
}

fn set_sat(c: vec3f, s: f32) -> vec3f {
    var lo = 0u;
    var mid = 1u;
    var hi = 2u;
    if c[lo] > c[mid] {
        let t = lo; lo = mid; mid = t;
    }
    if c[mid] > c[hi] {
        let t = mid; mid = hi; hi = t;
    }
    if c[lo] > c[mid] {
        let t = lo; lo = mid; mid = t;
    }
    var out = vec3f(0.0);
    let range = c[hi] - c[lo];
    if range > 0.0 {
        out[mid] = (c[mid] - c[lo]) * s / range;
        out[hi] = s;
    }
    return out;
}

// The whole-colour blend B(Cb, Cs); mirrors blend::blend_color.
fn blend_color(mode: u32, cb: vec3f, cs: vec3f) -> vec3f {
    switch mode {
        case 13u: { return select(cb, cs, lum(cs) < lum(cb)); }   // DarkerColor
        case 15u: { return select(cb, cs, lum(cs) > lum(cb)); }   // LighterColor
        case 23u: { return set_lum(set_sat(cs, sat(cb)), lum(cb)); } // Hue
        case 24u: { return set_lum(set_sat(cb, sat(cs)), lum(cb)); } // Saturation
        case 25u: { return set_lum(cs, lum(cb)); }                // Color
        case 26u: { return set_lum(cb, lum(cs)); }                // Luminosity
        default: {
            return vec3f(
                blend_channel(mode, cb.x, cs.x),
                blend_channel(mode, cb.y, cs.y),
                blend_channel(mode, cb.z, cs.z),
            );
        }
    }
}

// Premultiplied blend of one source pixel over a backdrop pixel; mirrors
// blend_pixel in the CPU path exactly.
fn blend_pixel(b: vec4f, source: vec4f, mode: u32, opacity: f32) -> vec4f {
    var s = source;
    if opacity < 1.0 {
        s = s * opacity;
    }
    if s.a <= 0.0 {
        return b;
    }
    if mode == 0u || b.a <= 0.0 {
        return s + b * (1.0 - s.a); // plain over
    }
    let cb = b.rgb / b.a;
    let cs = s.rgb / s.a;
    let both = s.a * b.a;
    let mixed = blend_color(mode, cb, cs);
    let rgb = s.rgb * (1.0 - b.a) + b.rgb * (1.0 - s.a) + both * mixed;
    return vec4f(rgb, s.a + b.a - both);
}

fn mask_at(p: vec2u) -> f32 {
    if params.has_mask == 0u {
        return 1.0;
    }
    return textureLoad(mask_tex, vec2i(p), 0).r;
}

@compute @workgroup_size(8, 8)
fn blend_main(@builtin(global_invocation_id) gid: vec3u) {
    let dims = textureDimensions(acc_in);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let p = vec2i(gid.xy);
    let b = textureLoad(acc_in, p, 0);
    var s = textureLoad(src_tex, p, 0);
    s = s * mask_at(gid.xy);
    textureStore(acc_out, p, blend_pixel(b, s, params.mode, params.opacity));
}

fn lut_lookup(x: f32) -> f32 {
    let q = clamp(x, 0.0, 1.0) * 1023.0;
    let i = u32(q);
    if i >= 1023u {
        return lut[1023];
    }
    let t = q - f32(i);
    return lut[i] + (lut[i + 1u] - lut[i]) * t;
}

// A Normal-mode adjustment layer: push the backdrop's straight colour
// through the LUT, weighted by opacity × mask. Mirrors adjust_in_place.
@compute @workgroup_size(8, 8)
fn lut_main(@builtin(global_invocation_id) gid: vec3u) {
    let dims = textureDimensions(acc_in);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let p = vec2i(gid.xy);
    let b = textureLoad(acc_in, p, 0);
    if b.a <= 0.0 {
        textureStore(acc_out, p, b);
        return;
    }
    let w = params.opacity * mask_at(gid.xy);
    if w <= 0.0 {
        textureStore(acc_out, p, b);
        return;
    }
    let c = b.rgb / b.a;
    let adjusted = vec3f(lut_lookup(c.x), lut_lookup(c.y), lut_lookup(c.z));
    let mixed = c + (adjusted - c) * w;
    textureStore(acc_out, p, vec4f(mixed * b.a, b.a));
}

// Pass-through groups at partial strength: lerp the backdrop toward the
// group's result by opacity × mask. Mirrors mix_tiles.
@compute @workgroup_size(8, 8)
fn mix_main(@builtin(global_invocation_id) gid: vec3u) {
    let dims = textureDimensions(acc_in);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let p = vec2i(gid.xy);
    let before = textureLoad(acc_in, p, 0);
    let after = textureLoad(src_tex, p, 0);
    let w = params.opacity * mask_at(gid.xy);
    textureStore(acc_out, p, before + (after - before) * w);
}
"#;

pub struct GpuCompositor {
    /// Cached backdrop (everything below the edited top-level layer) for
    /// `cache_rect`, mirroring [`crate::BelowCache`] on the GPU: it kills
    /// the per-call re-upload of every lower layer while one layer is
    /// being edited. Driven by [`GpuCompositor::note_change`].
    cache_key: Option<lumenply_doc::LayerId>,
    cache_rect: Rect,
    backdrop: Option<wgpu::Texture>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    blend: wgpu::ComputePipeline,
    lut: wgpu::ComputePipeline,
    mix: wgpu::ComputePipeline,
    /// 1×1 white dummy bound when a pass has no mask.
    no_mask: wgpu::Texture,
    /// 4-entry dummy LUT bound to non-LUT passes.
    no_lut: wgpu::Buffer,
}

impl GpuCompositor {
    /// `None` when no usable adapter exists (CI without a GPU, say).
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("nge-compositor"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .ok()?;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compositor"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
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
            label: Some("compositor"),
            entries: &[
                tex(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: BT::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                tex(2),
                tex(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: BT::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
            label: None,
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
        let blend = pipeline("blend_main");
        let lut = pipeline("lut_main");
        let mix = pipeline("mix_main");

        let no_mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no-mask"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            no_mask.as_image_copy(),
            bytemuck_cast(&[1.0f32]),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        use wgpu::util::DeviceExt;
        let no_lut = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("no-lut"),
            contents: bytemuck_cast(&[0.0f32; 4]),
            usage: wgpu::BufferUsages::STORAGE,
        });

        Some(GpuCompositor {
            cache_key: None,
            cache_rect: Rect::default(),
            backdrop: None,
            device,
            queue,
            layout,
            blend,
            lut,
            mix,
            no_mask,
            no_lut,
        })
    }

    /// Whether this document (over any rect) is renderable on the GPU path.
    pub fn supports(doc: &Document) -> bool {
        fn ok(layers: &[Layer]) -> bool {
            layers.iter().all(|l| {
                // Clip chains and layer effects stay on the CPU for now.
                !l.clip
                    && l.effects.is_empty()
                    // Fill opacity below 100% renders on the CPU reference path.
                    && l.fill_opacity >= 1.0
                    // Smart filters render on the CPU (ADR 0011).
                    && !l.smart_filters.is_active()
                    && l.blend != lumenply_doc::BlendMode::Dissolve
                    && match &l.content {
                        LayerContent::Pixel(_)
                        | LayerContent::Text(_)
                        | LayerContent::Smart(_)
                        | LayerContent::Fill(_)
                        | LayerContent::Shape(_) => true,
                        LayerContent::Group(c) => ok(c),
                        LayerContent::Filter(_) => false,
                        LayerContent::Adjustment(a) => {
                            l.blend == lumenply_doc::BlendMode::Normal
                                && matches!(a.compile(), CompiledAdjustment::Lut(_))
                        }
                    }
            })
        }
        ok(doc.layers())
    }

    /// Tell the cache which layer an edit touched (`None` = anything).
    /// Consecutive edits inside the same top-level layer keep the cached
    /// backdrop; clip chains never split (same rule as the CPU cache).
    pub fn note_change(&mut self, doc: &Document, changed: Option<lumenply_doc::LayerId>) {
        let key = changed.and_then(|id| {
            let layers = doc.layers();
            let mut i = layers.iter().position(|l| contains_layer(l, id))?;
            while i > 0 && layers[i].clip {
                i -= 1;
            }
            Some(layers[i].id)
        });
        if key != self.cache_key || key.is_none() {
            self.cache_key = key;
            self.backdrop = None;
        }
    }

    /// Whether a warm backdrop is held (for tests and stats).
    pub fn backdrop_cached(&self) -> bool {
        self.backdrop.is_some()
    }

    /// Composite `rect` on the GPU. `None` when the document needs a CPU
    /// feature or the rect is too large; output matches
    /// [`crate::composite_rect`] within float tolerance.
    pub fn composite_rect(&mut self, doc: &Document, rect: Rect) -> Option<TileStore> {
        if rect.is_empty() || rect.w > MAX_SIDE || rect.h > MAX_SIDE || !Self::supports(doc) {
            return None;
        }
        if rect != self.cache_rect {
            self.cache_rect = rect;
            self.backdrop = None;
        }
        let layers = doc.layers();
        let split = self
            .cache_key
            .and_then(|k| layers.iter().position(|l| l.id == k))
            .filter(|s| *s > 0);
        let acc = match split {
            Some(split) => {
                if self.backdrop.is_none() {
                    self.backdrop = Some(self.render_layers(&layers[..split], rect, None));
                }
                self.render_layers(&layers[split..], rect, self.backdrop.as_ref())
            }
            None => self.render_layers(layers, rect, None),
        };
        let flat = self.read_back(&acc, rect.w, rect.h);
        let mut out = TileStore::from_raster(&flat, rect.x, rect.y);
        out.prune_blank();
        Some(out)
    }

    // ---- orchestration -------------------------------------------------------------------

    fn render_layers(&self, layers: &[Layer], rect: Rect, backdrop: Option<&wgpu::Texture>) -> wgpu::Texture {
        let mut acc = self.make_texture(rect.w, rect.h);
        match backdrop {
            Some(b) => self.copy_texture(b, &acc, rect),
            None => self.clear_texture(&acc, rect),
        }
        for layer in layers {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            let mask = layer.mask.as_ref().filter(|m| m.enabled);
            match &layer.content {
                LayerContent::Pixel(_)
                | LayerContent::Text(_)
                | LayerContent::Smart(_)
                | LayerContent::Fill(_)
                | LayerContent::Shape(_) => {
                    let Some(store) = layer.raster_store() else {
                        continue;
                    };
                    let src = self.upload_raster(&store.to_raster(rect));
                    acc = self.pass(
                        &self.blend,
                        &acc,
                        &src,
                        mask,
                        rect,
                        layer.blend as u32,
                        layer.opacity,
                        None,
                    );
                }
                LayerContent::Group(children) => {
                    if layer.pass_through {
                        let child = self.render_layers(children, rect, Some(&acc));
                        acc = self.pass(&self.mix, &acc, &child, mask, rect, 0, layer.opacity, None);
                    } else {
                        let child = self.render_layers(children, rect, None);
                        acc = self.pass(
                            &self.blend,
                            &acc,
                            &child,
                            mask,
                            rect,
                            layer.blend as u32,
                            layer.opacity,
                            None,
                        );
                    }
                }
                LayerContent::Adjustment(a) => {
                    let CompiledAdjustment::Lut(table) = a.compile() else {
                        unreachable!("gated by supports()");
                    };
                    let src = &acc; // unused by lut_main; bind acc as dummy src
                    let next = self.pass(
                        &self.lut,
                        &acc,
                        src,
                        mask,
                        rect,
                        0,
                        layer.opacity,
                        Some(&table[..]),
                    );
                    acc = next;
                }
                LayerContent::Filter(_) => unreachable!("gated by supports()"),
            }
        }
        acc
    }

    #[allow(clippy::too_many_arguments)]
    fn pass(
        &self,
        pipeline: &wgpu::ComputePipeline,
        acc_in: &wgpu::Texture,
        src: &wgpu::Texture,
        mask: Option<&Mask>,
        rect: Rect,
        mode: u32,
        opacity: f32,
        lut: Option<&[f32]>,
    ) -> wgpu::Texture {
        let out = self.make_texture(rect.w, rect.h);
        let mask_tex = mask.map(|m| self.upload_mask(m, rect));
        let params: [u32; 4] = [mode, opacity.to_bits(), u32::from(mask.is_some()), 0];
        use wgpu::util::DeviceExt;
        let params_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("params"),
            contents: bytemuck_cast(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let lut_buf = lut.map(|t| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("lut"),
                contents: bytemuck_cast(t),
                usage: wgpu::BufferUsages::STORAGE,
            })
        });
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(acc_in)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&out)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view(src)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&view(
                        mask_tex.as_ref().unwrap_or(&self.no_mask),
                    )),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: lut_buf.as_ref().unwrap_or(&self.no_lut).as_entire_binding(),
                },
            ],
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut cp = enc.begin_compute_pass(&Default::default());
            cp.set_pipeline(pipeline);
            cp.set_bind_group(0, &bind, &[]);
            cp.dispatch_workgroups(rect.w.div_ceil(WG), rect.h.div_ceil(WG), 1);
        }
        self.queue.submit([enc.finish()]);
        out
    }

    // ---- resources -----------------------------------------------------------------------

    fn make_texture(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("acc"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn clear_texture(&self, t: &wgpu::Texture, rect: Rect) {
        let zeros = vec![0u8; (rect.w * rect.h * 16) as usize];
        self.write_full(t, &zeros, rect.w, rect.h, 16);
    }

    fn copy_texture(&self, from: &wgpu::Texture, to: &wgpu::Texture, rect: Rect) {
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_texture(
            from.as_image_copy(),
            to.as_image_copy(),
            wgpu::Extent3d {
                width: rect.w,
                height: rect.h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
    }

    fn upload_raster(&self, r: &Raster) -> wgpu::Texture {
        let t = self.make_texture(r.width, r.height);
        // Rgba is #[repr(C)] { r, g, b, a: f32 } — reinterpret as bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(r.pixels.as_ptr() as *const u8, r.pixels.len() * 16) };
        self.write_full(&t, bytes, r.width, r.height, 16);
        t
    }

    fn upload_mask(&self, m: &Mask, rect: Rect) -> wgpu::Texture {
        let mut vals = Vec::with_capacity((rect.w * rect.h) as usize);
        for y in 0..rect.h as i32 {
            for x in 0..rect.w as i32 {
                vals.push(m.value(rect.x + x, rect.y + y));
            }
        }
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mask"),
            size: wgpu::Extent3d {
                width: rect.w,
                height: rect.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.write_full(&t, bytemuck_cast(&vals), rect.w, rect.h, 4);
        t
    }

    fn write_full(&self, t: &wgpu::Texture, bytes: &[u8], w: u32, h: u32, bpp: u32) {
        self.queue.write_texture(
            t.as_image_copy(),
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * bpp),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    fn read_back(&self, t: &wgpu::Texture, w: u32, h: u32) -> Raster {
        let bpr = (w * 16).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().expect("map callback").expect("map readback buffer");
        let data = slice.get_mapped_range();
        let mut out = Raster::new(w, h);
        for y in 0..h as usize {
            let row = &data[y * bpr as usize..y * bpr as usize + (w * 16) as usize];
            for x in 0..w as usize {
                let px = &row[x * 16..x * 16 + 16];
                let f = |i: usize| f32::from_le_bytes([px[i], px[i + 1], px[i + 2], px[i + 3]]);
                out.pixels[y * w as usize + x] = lumenply_tiles::Rgba::new(f(0), f(4), f(8), f(12));
            }
        }
        drop(data);
        buf.unmap();
        out
    }
}

fn contains_layer(l: &Layer, id: lumenply_doc::LayerId) -> bool {
    l.id == id
        || l.children()
            .is_some_and(|c| c.iter().any(|ch| contains_layer(ch, id)))
}

/// Plain-old-data byte view (all inputs here are `f32`/`u32` slices).
fn bytemuck_cast<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gradient_mask;
    use lumenply_doc::{Adjustment, BlendMode};
    use lumenply_tiles::Rgba;

    fn gpu() -> Option<GpuCompositor> {
        let g = GpuCompositor::new();
        if g.is_none() {
            eprintln!("no GPU adapter available; skipping GPU equality test");
        }
        g
    }

    fn assert_matches_cpu(doc: &Document, what: &str) {
        let Some(mut gpu) = gpu() else { return };
        assert_gpu_matches(&mut gpu, doc, what);
    }

    fn assert_gpu_matches(gpu: &mut GpuCompositor, doc: &Document, what: &str) {
        let rect = doc.canvas();
        let g = gpu
            .composite_rect(doc, rect)
            .expect("document is in the supported subset");
        let c = cpu_composite_rect(doc, rect);
        let (gr, cr) = (g.to_raster(rect), c.to_raster(rect));
        let mut maxdiff = 0.0f32;
        let mut at = (0usize, Rgba::TRANSPARENT, Rgba::TRANSPARENT);
        for (i, (a, b)) in gr.pixels.iter().zip(cr.pixels.iter()).enumerate() {
            for (x, y) in [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)] {
                let d = (x - y).abs();
                if d > maxdiff {
                    maxdiff = d;
                    at = (i, *a, *b);
                }
            }
        }
        assert!(
            maxdiff < 1e-4,
            "{what}: GPU differs from CPU by {maxdiff} at {} (gpu {:?} cpu {:?})",
            at.0,
            at.1,
            at.2
        );
    }

    /// Every GPU blend mode (all but Dissolve) over a gradient backdrop,
    /// with opacity and a mask.
    #[test]
    fn gpu_matches_cpu_for_blends_masks_and_opacity() {
        let modes: Vec<BlendMode> = BlendMode::ALL
            .into_iter()
            .filter(|m| *m != BlendMode::Dissolve)
            .collect();
        let w = 30 * modes.len() as u32 + 30;
        let mut doc = Document::new(w, 200);
        let bg = doc.add_pixel_layer("bg");
        let mut fill = Raster::new(w, 200);
        for y in 0..200 {
            for x in 0..w {
                // Irrational-ish steps keep Hard Mix and Darker/Lighter
                // Color away from exact ties, where GPU and CPU division
                // rounding could pick different sides of the step.
                let t = (x as f32 * 0.618_034 + 0.013) % 1.0;
                fill.set(x, y, Rgba::from_straight(t, y as f32 / 200.0 + 0.0021, 0.4, 1.0));
            }
        }
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);
        for (i, mode) in modes.into_iter().enumerate() {
            let id = doc.add_pixel_layer("top");
            let mut r = Raster::new(60, 200);
            for y in 0..200 {
                for x in 0..60 {
                    r.set(x, y, Rgba::from_straight(0.8, 0.3, y as f32 / 200.0, 0.75));
                }
            }
            *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, i as i32 * 30, 0);
            let l = doc.layer_mut(id).unwrap();
            l.blend = mode;
            l.opacity = 0.85;
            if i % 2 == 0 {
                let mut m = Mask::reveal_all();
                gradient_mask(&mut m, Rect::new(i as i32 * 30, 0, 60, 200));
                l.mask = Some(m);
            }
        }
        assert_matches_cpu(&doc, "every GPU blend mode");
    }

    /// Dissolve stays on the CPU: its noise is keyed to canvas pixels.
    #[test]
    fn dissolve_falls_back_to_the_cpu() {
        let mut doc = Document::new(64, 64);
        let id = doc.add_pixel_layer("a");
        assert!(GpuCompositor::supports(&doc));
        doc.layer_mut(id).unwrap().blend = BlendMode::Dissolve;
        assert!(!GpuCompositor::supports(&doc));
        let Some(mut g) = gpu() else { return };
        assert!(g.composite_rect(&doc, doc.canvas()).is_none());
    }

    /// Groups (isolated and pass-through) and LUT adjustments.
    #[test]
    fn gpu_matches_cpu_for_groups_and_adjustments() {
        let mut doc = Document::new(256, 256);
        let bg = doc.add_pixel_layer("bg");
        let fill = Raster::filled(256, 256, Rgba::from_straight(0.35, 0.5, 0.65, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);

        doc.add_adjustment(Adjustment::Levels {
            in_black: 0.1,
            in_white: 0.9,
            gamma: 1.3,
            out_black: 0.05,
            out_white: 1.0,
            channels: Default::default(),
        });

        let g = doc.add_group("iso");
        let inner_id = doc.alloc_id();
        let mut inner = Layer::pixel(inner_id, "inner");
        let r = Raster::filled(100, 100, Rgba::from_straight(0.9, 0.2, 0.1, 0.8));
        *inner.pixels_mut().unwrap() = TileStore::from_raster(&r, 60, 60);
        inner.blend = BlendMode::Multiply;
        doc.layer_mut(g).unwrap().children_mut().unwrap().push(inner);
        doc.layer_mut(g).unwrap().opacity = 0.7;

        let pt = doc.add_group("pass");
        let adj_id = doc.alloc_id();
        doc.layer_mut(pt)
            .unwrap()
            .children_mut()
            .unwrap()
            .push(Layer::adjustment(adj_id, Adjustment::Invert));
        let l = doc.layer_mut(pt).unwrap();
        l.pass_through = true;
        l.opacity = 0.5;
        let mut m = Mask::hide_all();
        m.fill_rect(Rect::new(0, 0, 128, 256), 1.0);
        l.mask = Some(m);

        assert_matches_cpu(&doc, "groups and adjustments");
    }

    /// The cached backdrop reuses lower layers across edits and stays
    /// exact; re-keying and invalidation keep it honest.
    #[test]
    fn gpu_backdrop_cache_stays_exact_across_edits() {
        let Some(mut gpu) = gpu() else { return };
        let mut doc = Document::new(200, 150);
        let bg = doc.add_pixel_layer("bg");
        let fill = Raster::filled(200, 150, Rgba::from_straight(0.3, 0.45, 0.6, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);
        doc.add_adjustment(Adjustment::Levels {
            in_black: 0.05,
            in_white: 0.95,
            gamma: 1.2,
            out_black: 0.0,
            out_white: 1.0,
            channels: Default::default(),
        });
        let top = doc.add_pixel_layer("top");
        doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
            100,
            75,
            Rgba::from_straight(0.9, 0.1, 0.2, 0.8),
        );

        gpu.note_change(&doc, Some(top));
        assert_gpu_matches(&mut gpu, &doc, "cold with key");
        assert!(gpu.backdrop_cached(), "backdrop cached after the cold pass");

        // Edit the top layer repeatedly: the backdrop is reused.
        doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
            20,
            20,
            Rgba::from_straight(0.1, 0.8, 0.3, 1.0),
        );
        doc.layer_mut(top).unwrap().opacity = 0.65;
        gpu.note_change(&doc, Some(top));
        assert!(gpu.backdrop_cached(), "same-layer edit keeps the backdrop");
        assert_gpu_matches(&mut gpu, &doc, "warm");

        // Editing a lower layer re-keys and drops the stale backdrop.
        doc.layer_mut(bg)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(10, 10, Rgba::WHITE);
        gpu.note_change(&doc, Some(bg));
        assert!(!gpu.backdrop_cached(), "re-key invalidates");
        assert_gpu_matches(&mut gpu, &doc, "re-keyed");

        // A structural change clears everything.
        gpu.note_change(&doc, None);
        assert!(!gpu.backdrop_cached());
        assert_gpu_matches(&mut gpu, &doc, "cleared");
    }

    /// Unsupported documents hand back None instead of a wrong answer.
    #[test]
    fn gpu_declines_unsupported_documents() {
        let Some(mut gpu) = gpu() else { return };
        let mut doc = Document::new(64, 64);
        doc.add_pixel_layer("bg");
        doc.add_filter(lumenply_doc::Filter::GaussianBlur { radius: 4.0 });
        assert!(!GpuCompositor::supports(&doc));
        assert!(gpu.composite_rect(&doc, doc.canvas()).is_none());

        let mut doc2 = Document::new(64, 64);
        doc2.add_pixel_layer("bg");
        doc2.add_adjustment(Adjustment::Vibrance {
            vibrance: 0.5,
            saturation: 0.0,
        });
        assert!(
            !GpuCompositor::supports(&doc2),
            "per-pixel adjustments stay on the CPU"
        );
    }

    /// Fill layers are plain raster tiles to the GPU path; the newer
    /// adjustments (gradient map table, per-pixel maths) stay on the CPU.
    #[test]
    fn gpu_renders_fill_layers_and_declines_the_newer_adjustments() {
        use lumenply_doc::{Fill, Gradient, GradientStyle};
        let mut doc = Document::new(300, 200);
        doc.add_pixel_layer("bg");
        let id = doc.alloc_id();
        let mut grad = Layer::fill(
            id,
            Fill::gradient(Gradient::two([0.9, 0.2, 0.1], [0.1, 0.3, 0.9])),
        );
        if let Some(lumenply_doc::fill::FillLayer {
            fill: Fill::Gradient { style, angle, .. },
            ..
        }) = grad.fill_layer_mut()
        {
            *style = GradientStyle::Diamond;
            *angle = 25.0;
        }
        grad.opacity = 0.8;
        doc.add_layer(grad);
        let id = doc.alloc_id();
        let mut solid = Layer::fill(
            id,
            Fill::Solid {
                color: [0.2, 0.7, 0.3],
            },
        );
        solid.blend = BlendMode::Screen;
        let mut m = Mask::hide_all();
        m.fill_rect(Rect::new(40, 30, 120, 90), 1.0);
        solid.mask = Some(m);
        doc.add_layer(solid);
        crate::fill::refresh_stale(&mut doc);
        assert!(GpuCompositor::supports(&doc));
        assert_matches_cpu(&doc, "fill layers");

        for adj in [
            Adjustment::gradient_map_default(),
            Adjustment::channel_mixer_default(),
            Adjustment::photo_filter_default(),
            Adjustment::selective_color_default(),
        ] {
            let mut d = Document::new(16, 16);
            d.add_pixel_layer("bg");
            d.add_adjustment(adj.clone());
            assert!(!GpuCompositor::supports(&d), "{} stays on the CPU", adj.name());
        }
    }

    /// Shape layers are plain raster tiles (their rendered cache) to the
    /// GPU path, so they render there and match the CPU reference.
    #[test]
    fn gpu_renders_shape_layers() {
        use lumenply_doc::shape::{ShapeGeometry, ShapeLayer, ShapeStroke, StrokeAlign};
        use lumenply_doc::Fill;
        let mut doc = Document::new(300, 200);
        doc.add_pixel_layer("bg");
        let id = doc.alloc_id();
        let mut shape = Layer::shape(
            id,
            ShapeLayer::new(
                ShapeGeometry::Ellipse {
                    rect: [30.5, 20.25, 200.0, 120.0],
                },
                Some(Fill::Solid {
                    color: [0.9, 0.3, 0.1],
                }),
                Some(ShapeStroke {
                    color: [0.1, 0.2, 0.8],
                    width: 6.0,
                    align: StrokeAlign::Outside,
                    dash: None,
                }),
            ),
        );
        shape.blend = BlendMode::Multiply;
        shape.opacity = 0.7;
        let mut m = Mask::hide_all();
        m.fill_rect(Rect::new(0, 0, 150, 200), 1.0);
        shape.mask = Some(m);
        doc.add_layer(shape);
        crate::fill::refresh_stale(&mut doc);
        assert!(GpuCompositor::supports(&doc));
        assert_matches_cpu(&doc, "shape layers");
    }
}
