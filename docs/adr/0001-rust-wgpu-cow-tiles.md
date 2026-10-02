# ADR 0001: Rust engine, wgpu rendering, copy-on-write tile snapshots for undo

Date: 2026-10-02 · Status: accepted

## Context

A new raster editor needs a memory-safe core (it parses untrusted files), a
single GPU API across desktop and browser, and an undo system that stays
cheap on 100-megapixel documents.

## Decision

- Engine in Rust, split into UI-free crates (`tiles`, `doc`, `render`, `io`,
  `core`).
- Rendering through wgpu (WebGPU) with a CPU reference path that must match
  it; the CPU path lands first.
- Pixels stored as premultiplied linear `f32` in 256×256 tiles shared via
  `Arc`; documents are cloned as undo snapshots and tiles copy on write.
- Every edit is a `Command` executed through one `Editor`.

## Consequences

- Undo costs only the tiles touched; macros, scripting and headless use share
  one code path.
- 16 bytes per pixel is too much for very large files: 8/16-bit tile formats
  are required before Phase 2 ends.
- Desktop UI toolkit risk in Rust is mitigated by keeping the UI a thin layer
  over `Editor`; the toolkit can be replaced without touching the engine.
