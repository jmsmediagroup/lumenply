# ADR 0004: Tiles rest in 16-bit storage, compute in f32

Date: 2026-10-02 · Status: accepted

## Context

Premultiplied linear f32 pixels (16 bytes each) made large documents
impossible: twenty full 4K layers needed 5.4 GB before compositing began
(ADR 0001 flagged this). Blend, filter and adjustment math still wants f32.

## Decision

`Tile` holds either `F32` or `U16` data. Reads return f32 (borrowed for f32
tiles, converted for compact ones); any write expands the tile to f32. After
every command the editor compacts the tiles the new document owns outright,
so only edited tiles are converted and history snapshots are untouched.
16-bit linear storage round-trips every 8-bit sRGB level exactly enough
(error ≤ 1/65535), so imported photos lose nothing.

## Consequences

- Layer memory halves; the previously fatal 4K benchmark runs at ~10% lower
  compositing throughput from the conversion.
- Values outside `[0, 1]` are clamped on compaction; HDR documents will need
  an explicit float mode that skips compaction (one flag on `Document`).
- An 8-bit variant (4 bytes/pixel, sRGB-encoded) can follow the same path
  for documents that never need more; the GPU renderer can upload u16
  tiles directly.
