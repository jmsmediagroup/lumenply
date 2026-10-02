# ADR 0010: Shape layers

Date: 2026-10-03. Status: accepted.

## Context

Photoshop users draw rectangles, ellipses, polygons, lines and custom
shapes as editable vector layers with a fill and a stroke, scale them
without losing sharpness, and expect them to survive a PSD round trip as
shape layers. Lumenply already had a pen work path (cubic beziers,
even-odd across subpaths) and fill layers with a shared gradient model
(ADR 0008).

## Decision

**Model** (`doc::shape`): `LayerContent::Shape(ShapeLayer)` holds a
parametric `ShapeGeometry` — rectangle with a corner radius, ellipse,
regular polygon, line (a bar `weight` px thick with optional arrowheads),
a built-in custom shape (star, arrow, heart, speech bubble) or a free
`VectorPath` — followed by an affine `transform`. Free transform, Move,
flips, crops, image size and rotation compose into `transform`, so the
outline is redrawn crisply each time; pixels are never resampled. The
geometry stays parametric so Properties can still change a corner radius
or side count after a transform. The fill reuses `doc::Fill` (solid or
gradient; a gradient spans the shape's own bounds, not the canvas). The
stroke has a colour, a width in canvas pixels (transforms do not scale
it, as in Photoshop), an alignment (inside — the default, as in
Photoshop — centre, outside) and optional dashes (dash and gap as
multiples of the width, Photoshop's units).

**Rendering** (`render::shape`): the outline is flattened within 0.05 px
(straight segments stay single chords). The fill is a scanline fill with
16 sub-scanlines and exact horizontal coverage, even-odd across subpaths
like the pen's fills. The stroke is a distance field around the chords:
coverage ramps over one pixel at the stroke edge; inside and outside
strokes are that band clipped by the fill coverage, so their outline edge
is exactly the fill's edge, and an outside stroke is added (not
composited) to the fill so their shared anti-aliased edge has no seam.
Joins and dash ends are therefore round, except that an inside stroke
keeps convex corners sharp. Work is parallel per 256-row band.

**Cache**: like fills and text, the rendered tiles (`ShapeLayer::cache`)
are derived state, clipped to the canvas, never saved, rebuilt by
commands that change the shape and by `fill::refresh_stale` (which now
refreshes shapes too) after every command and on load. Every compositor
path — masks, blend modes, opacity, effects, clip chains, groups, the GPU
path, thumbnails, rasterize — sees an ordinary raster.

**Files**: `.lumen` gains a `"kind": "shape"` record holding the geometry,
the transform as six coefficients, the fill and the stroke. PSD writes
Photoshop's own shape-layer encoding: the fill block (`SoCo` / `GdFl`), a
vector stroke (`vstk`) and a vector mask (`vmsk`, 8.24 fixed-point knots;
subpaths after the first use operation −1, "merged", which fills them
even-odd), beside the rendered pixels, with the record's
pixel-data-irrelevant flag set. Reading turns a fill plus a readable
vector mask back into a shape whose geometry is a path. OpenRaster bakes
shapes to pixels with a warning.

## Consequences

- Older builds cannot open `.lumen` files that contain shape layers.
- A PSD round trip turns parametric shapes into path shapes (no `vogk`
  "live shape" data is written), so the corner-radius and side-count
  controls are not available after re-import. The outline, fill, stroke
  and layer settings come back.
- Round outer joins differ from Photoshop's default miter joins for
  centre and outside strokes on sharp corners. A polygon offsetter
  could add miter joins later without changing the file format.
- psd-tools 1.23 parses every block. It renders fills, vector masks and
  inside strokes to within anti-aliasing of our pixels, but clips centre
  and outside strokes to the path and ignores dashes and gradient
  offsets. Those are limits of the reader, not of the file.
- Tagged-block lengths in PSD layer records now include their pad byte,
  as the spec says; psd-tools lost sync on odd-length blocks before.
