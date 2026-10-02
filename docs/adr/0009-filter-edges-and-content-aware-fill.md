# ADR 0009: Filter edges, filter units and content-aware fill

Date: 2026-10-03. Status: accepted.

## Context

Six Photoshop filters (Mosaic, Emboss, Find Edges, Surface Blur, Lens Blur,
Dust & Scratches), Select > Modify and Content-Aware Fill were added. Three
choices in them are hard to undo once documents exist.

1. **Stored units.** Filter parameters are saved in `.lumen` files. Surface
   Blur and Dust & Scratches have thresholds; Mosaic has a cell size.
2. **The canvas edge in destructive filters.** `apply_filter` treated
   everything outside a layer's painted pixels as transparent. A blur of a
   full-canvas photo therefore faded to transparent along the border, and a
   mosaic whose cells did not divide the canvas had half-transparent edge
   cells. Live filter layers already repeated the canvas edge pixel.
3. **Where Content-Aware Fill samples from**, and how it is made
   repeatable.

## Decision

1. Thresholds are stored as Photoshop levels (0–255, compared on
   gamma-encoded values, as adjustments are; see ADR 0005). Sizes and radii
   are stored in pixels. Mosaic cells are anchored to the canvas origin, so
   every tile of a live layer cuts the same grid. New `Filter` variants are
   added at the end of the enum; `FindEdges` has no fields
   (`{"type":"find-edges"}`).
2. `ApplyFilter` now calls `apply_filter_in_canvas`. Off-canvas pixels that
   the layer does not paint repeat the canvas edge, which is how live
   filters read their backdrop. Such pixels stay empty afterwards. Layer
   content beyond the canvas is filtered as it is. The edge-free
   `apply_filter` is unchanged.
3. Content-Aware Fill (`render::inpaint`) is multi-scale PatchMatch with
   EM voting. It uses 7×7 patches compared as premultiplied gamma-encoded
   RGBA, and a fixed seed. The fill samples only the selection's bounds
   plus a margin: by default the hole's larger side, at least 64 px.
   Parallel passes work on independent rows and columns, so results do not
   depend on thread scheduling.

## Consequences

- Destructive blurs, sharpening and high pass near the canvas border now
  match their live versions and Photoshop. Documents are unaffected,
  because results are baked when the filter is applied.
- Converting thresholds to another unit later needs a serde migration.
- Fill results are bit-identical across runs and machines with the same
  float behaviour. A different seed or patch size changes every future
  fill, but no saved data.
