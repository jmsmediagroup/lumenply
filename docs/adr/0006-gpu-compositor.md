# 6. GPU compositor: wgpu compute mirroring the CPU reference

Date: 2026-10-02. Status: accepted (engine milestone; app wiring experimental).

## Context

Compositing is the hot loop. The CPU path is complete and is the reference
(DEVELOPMENT.md), and the roadmap calls for a GPU path whose equality with the
CPU is checked by tests.

## Decision

`render::gpu::GpuCompositor` is a headless wgpu compute compositor — no
window, no UI toolkit, so the engine-crate rule holds. The WGSL kernels
mirror the CPU math function-for-function: `blend_main` is `blend_pixel`
(premultiplied over + the ten separable blend formulas), `lut_main` is
`adjust_in_place` for Normal-mode LUT adjustments (the 1024-entry
`CompiledAdjustment::Lut` uploads as a storage buffer — the gamma wrapping
of ADR 0005 comes along for free since it is baked into the LUT), and
`mix_main` is `mix_tiles` for pass-through groups. Orchestration walks the
layer tree on the CPU, one pass per layer, ping-ponging RGBA32F textures.

**Honest fallback, never a wrong answer**: `composite_rect` returns `None`
whenever the document needs anything the kernels do not cover — live
filter layers, per-pixel (non-LUT) adjustments, non-Normal adjustment
blending, rects over 4096 px — and callers use the CPU. `supports(doc)`
exposes the gate.

Equality tests cover every blend mode with masks and fractional opacity,
isolated and pass-through groups, LUT adjustments, and the decline path;
tolerance 1e-4. Machines without an adapter skip with a notice.

## Consequences

- This milestone optimises nothing by itself: every call re-uploads layer
  rects and reads the result back. The follow-up (persistent per-layer
  textures, dirty-rect uploads, rendering straight into the UI's surface
  via an eframe-wgpu backend) is where the speed arrives; until then the
  app may offer the GPU path only as an experimental preference.
- New kernels must land in pairs: CPU reference first, WGSL mirror plus an
  equality test second. A feature only the GPU has is a bug.
- wgpu (Metal/Vulkan/DX12) joins the render crate's dependencies.
