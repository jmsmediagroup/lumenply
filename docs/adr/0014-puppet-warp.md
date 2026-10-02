# ADR 0014: Puppet Warp

Date: 2026-10-03. Status: accepted.

## Context

Photoshop's Edit ▸ Puppet Warp lays a triangle mesh over a layer's
content; the user drops pins and drags them, and the shape bends as if it
were stiff rubber (limbs bend, letters stay letters). Lumenply already had
affine, perspective and mesh warps (`render::transform`) and Liquify (a
painted displacement field), none of which gives that "move a few points,
keep everything else rigid" behaviour.

## Decision

**Destructive, one undo step.** `core::puppet::PuppetWarp { layer, mesh
params, mode, pins }` rebuilds the mesh from the layer's pixels, solves
and bakes into the pixel layer; the layer mask and smart-filter mask go
through the same mesh. Smart objects are refused with a reason (they keep
affine transforms only, as for Perspective and Warp); text, shape and fill
layers must be rasterized first. A non-destructive puppet (a smart filter
on a smart object, as in Photoshop) can come later: the command already
carries everything needed to re-run it.

**Mesh** (`render::puppet::PuppetMesh`). A regular grid over the content
bounds grown by the Expansion; Density sets the cell size (14 / 24 / 38
cells along the longer side, at least 4 px, at most 6000 cells). A cell is
kept when it comes within Expansion px of a pixel with alpha > 0.5/255;
each kept cell is two triangles with alternating diagonals. To follow the
outline, edge cells drop a triangle that holds nothing (exact box–triangle
test, choosing the diagonal that allows it), then outline vertices are
pulled in to Expansion + ½ px from the shape when sample points prove no
opaque pixel or margin leaves the mesh and no triangle loses more than 70%
of its area. Moves stay under one cell, so point location stays a lookup
in the 3×3 neighbouring cells. Row-major vertex numbering keeps the
systems banded.

**Solver.** Igarashi, Moscovich & Hughes 2005, both closed-form steps:
(1) a similarity step (each triangle's third corner in the local frame of
the other two), (2) a fit step: each triangle is fitted with a rotated
copy of its rest shape scaled by `s^k` (`s` its step-one scale; Rigid
k = 0, Normal k = ½, Distort k = 1), then the mesh is solved so edges
match the fitted edges. Pins are barycentric points of the rest mesh
held by a stiff penalty (weight 1e5 against O(1) mesh terms; a held pin
lands within ~1e-4 px). Each connected part of the mesh is handled on its
own: no pin = stays, one pin = translates, two or more = solved. Both
matrices depend only on the mesh and where pins sit, so they are factored
(banded Cholesky in f64) when pins are added or removed; a drag only
back-substitutes. Solves are taken relative to a least-squares similarity
of the pins, so unmoved pins reproduce the layer exactly and moves, turns
and uniform scales of all pins are exact.

**Rendering.** Each deformed triangle maps back to its rest triangle by
one affine map; pixel centres inside sample the source bilinearly
(premultiplied). Edge functions are evaluated canonically (from the lower
vertex index), so a pixel centre on a shared edge belongs to exactly one
triangle, and overlapping parts (folds) composite "over" in draw order.
Draw order is by the depth of each triangle's nearest pin (rest
distance), lower first; the workspace's Depth +/− buttons change it.

**Workspace** (`app::puppet_ui`). Full window like Liquify: the layer over
the composite of what lies under it, preview at most 1400 px on the long
side, solved and redrawn on every drag event.

## Consequences

- 1800 × 1205 layer: mesh build ~10 ms (Normal), factor ≤ 4 ms, solve
  ≤ 0.4 ms per drag, full-size bake ~8 ms; a preview frame ~5–20 ms.
- The mesh is a clipped grid rather than a constrained Delaunay
  triangulation of the outline: simpler and exactly covering, with some
  remaining zigzag along slanted outlines.
- Pins are soft (penalty), so a pin can trail its target by ~1e-4 px.
- Not yet: per-pin rotation (Photoshop's Alt-drag rotate ring), selecting
  several pins at once, puppet warp as a smart filter, PSD round trip of
  the warp (it bakes into pixels, which PSD carries).
