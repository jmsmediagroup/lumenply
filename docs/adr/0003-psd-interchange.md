# ADR 0003: PSD import/export is our own codec, validated against psd-tools

Date: 2026-10-02 · Status: accepted

## Context

Reliable PSD round-trip is a core differentiator. Existing Rust crates read
PSDs partially and write nothing; Photoshop's format is documented but full
of edge cases.

## Decision

`nge-io::psd` implements both directions for 8-bit RGB: layer records with
RLE channels, groups via `lsct` section dividers, layer masks, opacity,
visibility, blend-mode keys, `luni` Unicode names, and an RLE merged
preview. Adjustment layers with a documented binary layout travel both
ways (`levl`, `curv`, `brit`+`CgEd`, `hue2`, `blnc`, `thrs`, `post`, `nvrt`);
descriptor-only types (Black & White, Exposure, Vibrance) are reported as
not carried over, and the merged preview still reflects them. Correctness of
the writer is checked by reading our files with the independent Python
library `psd-tools` in addition to our own round-trip tests.

## Consequences

- Layered PSDs from this editor open elsewhere with structure intact.
- 16/32-bit, CMYK/Lab/Grayscale, PSB (large documents) and Photoshop
  adjustment layers are not supported yet; each is a clear, separate task.
- Remaining adjustment types need Photoshop's descriptor format, for which
  a small generic descriptor reader/writer now exists in the codec.
