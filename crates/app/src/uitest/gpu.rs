//! Offscreen rendering of each frame with egui-wgpu: the same tessellated
//! meshes and texture uploads a window would get, painted into a texture
//! and read back as an image.
//!
//! The window itself paints with egui_glow; both draw the same meshes
//! with equivalent shaders into a gamma-space target, with dithering on
//! (eframe's default), so the two agree to within rounding.

use eframe::egui;

pub(crate) struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    size: [u32; 2],
    padded_row: u32,
    /// The largest texture side the device takes (what eframe reports to
    /// egui as `max_texture_side`).
    pub(crate) max_texture_side: usize,
    pub(crate) adapter: String,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Gpu {
    /// A renderer for frames of `size` physical pixels.
    pub(crate) fn new(size: [u32; 2]) -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or("no GPU adapter (rendering and video are off)")?;
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("uitest"),
                required_features: wgpu::Features::empty(),
                required_limits: limits.clone(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| format!("no GPU device: {e}"))?;
        let renderer = egui_wgpu::Renderer::new(&device, FORMAT, None, 1, true);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("uitest-frame"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = (size[0] * 4).div_ceil(align) * align;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uitest-readback"),
            size: padded_row as u64 * size[1] as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let info = adapter.get_info();
        Ok(Gpu {
            device,
            queue,
            renderer,
            target,
            view,
            readback,
            size,
            padded_row,
            max_texture_side: limits.max_texture_dimension_2d as usize,
            adapter: format!("{} ({:?})", info.name, info.backend),
        })
    }

    /// Apply a frame's texture changes, paint its meshes and read the
    /// result back (opaque RGBA, as the window shows it).
    pub(crate) fn paint(
        &mut self,
        textures: &egui::TexturesDelta,
        prims: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
    ) -> image::RgbaImage {
        for (id, delta) in &textures.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: self.size,
            pixels_per_point,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("uitest"),
            });
        let user = self
            .renderer
            .update_buffers(&self.device, &self.queue, &mut encoder, prims, &screen);
        {
            // eframe's default clear colour (App::clear_color).
            let [r, g, b, a] =
                egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32();
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("uitest-egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut pass = pass.forget_lifetime();
            self.renderer.render(&mut pass, prims, &screen);
        }
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row),
                    rows_per_image: Some(self.size[1]),
                },
            },
            wgpu::Extent3d {
                width: self.size[0],
                height: self.size[1],
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(user.into_iter().chain([encoder.finish()]));
        for id in &textures.free {
            self.renderer.free_texture(id);
        }

        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        let ok = rx.recv().map(|r| r.is_ok()).unwrap_or(false);
        let [w, h] = self.size;
        let mut img = image::RgbaImage::new(w, h);
        if ok {
            let data = slice.get_mapped_range();
            let row = (w * 4) as usize;
            for (y, out) in img.chunks_exact_mut(row).enumerate() {
                let start = y * self.padded_row as usize;
                out.copy_from_slice(&data[start..start + row]);
            }
            drop(data);
            self.readback.unmap();
        }
        // The window is opaque; the clear colour's alpha means nothing on screen.
        for p in img.pixels_mut() {
            p.0[3] = 255;
        }
        img
    }
}
