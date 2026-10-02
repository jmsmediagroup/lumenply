# 7. Fill layers, the shared gradient model and the newer adjustments

Date: 2026-10-03. Status: accepted.

## Context

Photoshop users expect Gradient Map, Channel Mixer, Photo Filter and
Selective Color adjustment layers, and Solid Color / Gradient fill layers,
and expect them to survive a PSD round trip. Fill layers have to composite
exactly like pixel layers (mask, blend, opacity, effects, clipping) while
staying editable.

## Decision

**Gradient model** (`doc::gradient`): two or more stops, each a position,
a straight linear colour and an opacity. Colours rest linear like every
other document colour, but interpolation runs on gamma-encoded values
(Photoshop's "classic" gradients; ADR 0005), linearly — Photoshop's
"smoothness" is written as 0 in PSD so it renders the same ramp. Midpoints
are not modelled (PSD midpoints are written as 50%).

**Fill layers** are a new `LayerContent::Fill(FillLayer)`: the settings
(`Fill::Solid` or `Fill::Gradient` with style, angle, scale, reverse and
centre offset) plus a **derived pixel cache rendered over exactly the
canvas**. Like the text and smart-object caches it is never saved; the
`Editor` re-renders stale caches after every command (so crop, canvas
size and image rotation re-span the fill) and on construction, and the
`.lumen` and PSD loaders render it once the canvas is known. Because the
cache is an ordinary `TileStore`, every compositor path — masks, blend
modes, effects, clip chains, the GPU path, thumbnails, rasterize —
handles fills without special cases. A solid fill shares one tile across
the canvas interior, so it costs one tile of memory.

Gradient geometry: the ramp's full length is the canvas projected onto
the angle (`|w cos a| + |h sin a|`) times the scale; linear runs across
it through the centre; reflected and radial use half of it; diamond uses
`|along| + |across|`; angle sweeps counter-clockwise from the direction.

**Newer adjustments**, all in the gamma domain:
- Gradient Map maps gamma luminance (Rec. 709 weights on encoded values,
  as Threshold does) through a 1024-entry table to linear colour.
- Channel Mixer: per-output rows `[r, g, b, constant]` plus a monochrome
  row.
- Photo Filter: multiply by the filter colour at the density, then
  optionally the W3C SetLum to restore the original luminance.
- Selective Color is an approximation of Photoshop's undocumented maths:
  family weights from the channel gaps (primaries: top channel's lead;
  secondaries: middle channel's lead over the bottom; whites/blacks/
  neutrals from distance to mid grey); C/M/Y shifts add to the channel's
  ink (absolute) or scale it (relative); black scales the result by
  `1 − k` (absolute) or `1 − k·(1 − max)` (relative).

**PSD**: `grdm`, `mixr` (four 5-short records, gray first when
monochrome), `phfl` (version 2 written; versions 2 and 3 read, v3 as
L\*a\*b\* × 100) and `selc` are binary blocks. Fills export as their
rendered pixels plus a `SoCo` / `GdFl` action descriptor (a small
descriptor codec in `io::psd::extra`), so readers without fill support
still see the pixels, and import back as fill layers. psd-tools reads
all six and renders the fills. OpenRaster bakes fills to pixels with a
warning.

## Consequences

- `.lumen` gains a `"kind": "fill"` layer record and four adjustment
  types; older files load unchanged, older builds cannot open files that
  use them.
- The GPU path renders fills (raster tiles) but sends the four new
  adjustments to the CPU, like the other per-pixel adjustments.
- Selective Color will not match Photoshop pixel for pixel; it follows
  the documented semantics (relative vs absolute, family targeting).
- A canvas-sized cache costs memory for gradient fills (as much as a full
  pixel layer); rendering on demand per tile is a possible later
  optimisation that would not change the file format.
