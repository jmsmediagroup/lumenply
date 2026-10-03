# ADR 0016: Patterns live in the document; fills and effects reference them

Date: 2026-10-03. Status: accepted.

## Context

Photoshop users expect pattern fill layers, the Pattern Overlay effect,
Edit ▸ Define Pattern and a pattern library, and expect PSDs that use
patterns to open with them intact. Photoshop keeps the pixels of every
pattern a document uses in the global `Patt` / `Pat2` / `Pat3` tagged
blocks at the end of the layer-and-mask section and refers to them by
name plus a UUID string from `PtFl` fill layers and `patternFill` effect
descriptors.

## Decision

**Model.** A `Pattern` is `{ id, name, image: Arc<Raster> }` (premultiplied
linear like every raster, at most 1024 px a side). `Document::patterns`
holds the patterns the document uses and is covered by undo (clones share
the `Arc`). Fills and effects store a `PatternRef { id, name }` plus a
**derived** `image` pointer, never saved, that `Document::resolve_patterns`
points at the document's pixels (by id, then by name). The fill refresh
that runs after every command calls it first, so replacing a pattern
re-renders every layer that uses it. A reference that carries pixels the
document lacks (one picked from the library) is adopted into
`Document::patterns`, so documents stay self-contained without every
command having to add patterns explicitly.

**Rendering.** `Fill::Pattern { pattern, scale, offset, angle }` is
appended to the fill enum, so pattern fill layers and shape fills reuse
the fill cache and every compositor path (masks, blend, effects, GPU).
The pattern tiles from the canvas origin plus `offset` (Photoshop's
phase): canvas pixel centre `p` maps to `R(−angle) · (p − offset) / scale`
in pattern space, sampled bilinearly with wrap-around, so 100% reproduces
the pixels exactly. The Pattern Overlay effect (`LayerEffects::
pattern_overlay`) is painted inside the layer's coverage like the colour
and gradient overlays, under them (Photoshop's order).

**Files.** `.lumen` writes the used patterns as 16-bit PNGs
(`patterns/<n>-<id>.png`) listed in the manifest; unused document patterns
are not saved. PSD writes one 8-bit RGB `Patt` record per used pattern
(raw channels, transparency in the last slot), `PtFl` for pattern fills
(beside the rendered pixels, as for the other fills) and `patternFill` in
`lfx2`. On import, `Patt`/`Pat2`/`Pat3` (RGB, greyscale, indexed, CMYK,
Lab; raw or RLE; 8/16/32-bit) become document patterns; a `PtFl` whose
pattern the file lacks keeps its pixels with a warning. Photoshop anchors
a linked pattern where the layer was and does not always write that
phase down, so when the file stored the layer's pixels the importer
searches the one-period phase that reproduces them exactly and stores it
as the fill's offset. `.pat` preset files share the record reader.

**Library.** The app offers built-in generated patterns (stable ids
`lumenply-builtin-*`), the document's patterns and the user's own, kept
as 16-bit PNGs plus `library.json` in a `patterns` folder next to
prefs.json (Define Pattern and `.pat` import add to it).

## Consequences

- Pattern pixels are shared, not copied, across undo snapshots; a
  document that uses a 1024² pattern carries one copy.
- Pattern Overlay "Link with Layer" is kept for round trips but the
  pattern does not yet follow the layer when it moves (the phase stays
  canvas-anchored); Photoshop-made overlays whose phase it never wrote
  can be off by the layer's offset.
- Pattern strokes (stroke effect with a pattern), the Pattern Stamp tool
  and pattern-based brush textures are separate tasks.
- Older builds cannot open `.lumen` files that use pattern fills.
