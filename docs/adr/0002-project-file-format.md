# ADR 0002: Native project format is a zip of JSON + raw tiles

Date: 2026-10-02 · Status: accepted

## Context

The editor needs a lossless, versioned, non-destructive document format
from day one. It must survive partial corruption gracefully, be cheap for
mostly-empty layers, and be easy to inspect and to extend (text, vector,
smart-object layers; embedded ICC profiles; thumbnails).

## Decision

`.nge` is a zip archive with `manifest.json` (canvas, layer tree, every
layer property, an explicit format version) and one deflated entry per
allocated tile (`tiles/<id>/<x>_<y>.rgba`, `masks/<id>/<x>_<y>.a`).
Readers reject manifests with a newer major version. OpenRaster export
will reuse the same container code.

## Consequences

- Any zip tool can open a document; a corrupt tile loses one 256×256 block,
  not the file.
- Raw f32 tiles are large (1 MiB before deflate); when 8/16-bit tile formats
  land, each tile entry will carry its format in the manifest.
- Adding layer kinds or properties is additive: unknown JSON fields are kept
  on the read side for forward compatibility (to implement with `#[serde(flatten)]`
  on a catch-all map before 1.0).
