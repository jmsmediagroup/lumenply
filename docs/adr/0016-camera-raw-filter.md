# ADR 0016: Camera Raw Filter

Date: 2026-10-03. Status: accepted.

## Context

Photoshop's Filter ▸ Camera Raw Filter applies the Camera Raw develop
controls to any layer, destructively or as a smart filter. Lumenply had
the develop (`render::develop`) only for opening RAW files. Making it a
filter means it is saved in `.lumen` files and must render tile by tile,
so three choices are hard to undo.

1. **Where the settings live, and their stored form.**
2. **What the size-dependent controls measure against.** Highlights,
   shadows, texture, clarity and dehaze read a blurred neighbourhood whose
   radius is a fraction of the image size. The vignette needs the image's
   centre and corners. A filter renders from padded tiles and chunks,
   which do not know the image.
3. **The identity.** A RAW develop starts with a camera tone curve; a
   finished image must not.

## Decision

1. `Develop` moves to `lumenply-doc` (`doc::develop`) with serde. Every
   field has a serde default, so settings written before a control existed
   still load (Texture, Clarity, Dehaze, Vignette and its midpoint were
   added here). The filter is `Filter::Develop { settings, frame }`,
   appended to the enum: `{"type":"develop","settings":{...},"frame":[x,y,w,h]}`.
2. `frame` is a canvas rectangle fixed when the filter is made (the
   canvas). Radii are fractions of its longer side (tone 2 %, clarity 1 %,
   texture 0.25 %, as the RAW path always used for tone), and the vignette
   falls off towards its corners. `Filter::pad()` is three times the widest
   radius in use (three box passes), zero for the global controls. The
   local blurs are coverage-weighted (transparent pixels don't count),
   clipped at the raster edge, and their window sums come from f64 prefix
   sums, so a pixel's value doesn't depend on where its tile starts. Tests
   check live layers against one whole-canvas pass and smart filters
   (across chunk seams) against the destructive filter, both within 1e-4.
   Opening a RAW file runs the same code with the frame set to the image.
3. `Develop::NEUTRAL` (all zero, tone curve off) is the identity and is
   what the filter starts with. `Develop::default()` (tone curve on) stays
   the RAW starting point.

The workspace is the RAW one in a filter mode (camera_raw_filter.rs): on a
pixel layer OK bakes (`ApplyFilter`), on a smart object it adds a smart
filter (`AddSmartFilter`; re-opened edits are one `SetSmartFilter`), and
"Apply as ▸ Live filter layer" adds `AddFilterLayer`. No new command was
needed.

## Consequences

- Cropping or resizing the canvas does not move a filter's frame: its
  vignette and radii stay measured on the canvas it was made on. Updating
  frames from canvas commands would change no file format.
- Large frames have large reaches (a 4000 px canvas with Shadows set reads
  240 px around each tile), so live Camera Raw layers cost more per tile
  than most filters. Measured on 4000×3000: 0.18 s (global only) to 0.52 s
  (every control) destructive, 0.13–0.98 s as a smart filter.
- PSD and OpenRaster bake the smart filter (ADR 0011) and skip the live
  layer with a warning; Photoshop's Camera Raw smart filter (`SoLd`
  descriptors) is not read or written.
- Dehaze is an approximation (a veil from the blurred darkest channel, no
  dark-channel min filter, no global airlight), so it differs from Adobe's.
