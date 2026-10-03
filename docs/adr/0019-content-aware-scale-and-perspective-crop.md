# ADR 0019: Content-Aware Scale and Perspective Crop

Date: 2026-10-03 · Status: accepted

## Context

Edit ▸ Content-Aware Scale (Alt+Shift+Cmd+C) and the Crop tool's
Perspective mode arrived together. Both are new commands whose results
users will depend on, and a few of their choices would change every future
result if revisited.

## Decisions

1. **Seam carving with forward energy** (`render::seam_carve`). A pixel's
   energy is the gradient magnitude of its gamma-encoded luminance (plus a
   step of 0.5 for full transparency, so silhouettes count) and the forward
   energy of Rubinstein et al. 2008: the jumps between the pixels a removal
   makes neighbours. Enlarging finds k seams on a copy and inserts each as
   the average of the seam pixel and its right-hand neighbour, at most half
   the width per round. Heights run on the transposed image; width is
   carved first.

2. **Protection is energy.** A protect mask adds 1e5 per fully covered
   pixel, more than any seam can collect from content, so seams cross it
   only when nothing else is left. "Protect skin tones" adds 200 per pixel
   inside the gamma YCbCr box Cb 77–127, Cr 133–173 (Y > 40): a strong
   preference, weaker than an explicit mask. Warm sunset skies fall in that
   box too, as in Photoshop.

3. **Amount** is the share of the size change done by carving; a Lanczos
   resample (`render::resample`, as Image Size uses) does the rest. Amount
   0 is exactly that resample.

4. **Several seams per pass.** One dynamic-programming pass yields every
   seam that stays disjoint from the ones already taken (with a one-pixel
   gap), costs at most 1.5× the best plus 0.05 per row, and fits in a
   twentieth of the width. Results are deterministic, but a different
   batching rule would change future results (no saved data). Measured on
   the 1800 × 1205 demo photo narrowed to 70 %: 0.19 s with 14 threads,
   0.25 s on one (the cumulative pass is branch-free f32 and vectorises).

5. **What gets scaled** is the active pixel layer's painted bounds, placed
   at the box the user drags. Its layer mask is carved along the same seams.
   Text, smart objects, shapes and fills are refused with "rasterize first",
   as Perspective and Warp do. An active selection is offered as a
   protection, not as the area to scale (Photoshop scales a selection's
   content; Lumenply leaves that for later).

6. **Perspective Crop is one command**, `PerspectiveCrop { quad, width,
   height }`, run from a mode of the Crop tool. Every pixel layer, layer
   mask and smart-filter mask is resampled bilinearly through the
   homography from the output rectangle to the quad; the result always
   deletes what lies outside (a perspective has no meaningful overflow).
   **Text, shapes and smart objects are rasterized in the same undo step**,
   because none of them can hold a perspective change live; the options bar
   says how many before the crop. Fill and adjustment layers simply cover
   the new canvas, paths follow the mapping, guides are dropped. The size
   defaults to the averages of opposite edge lengths; W × H fields override
   it.

## Consequences

- Scripts and the CLI can run both through the same commands as the app.
- Content-aware results on a selection-only area, per-layer smart-object
  support and a GPU carve remain open.
- Perspective crops of documents with live text cost editability; users who
  want it keep a copy of the text layer above the crop.
