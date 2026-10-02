# ADR 0007: Crop is one command; guides live in the document

Date: 2026-10-03 · Status: accepted

## Context

The Crop tool, rulers, guides, grid and snapping arrived together. Three
choices are hard to undo once files and habits depend on them.

## Decisions

1. **One crop command for every case.** `CropCanvas { rect, angle,
   delete_cropped }` (crates/core/src/crop.rs) covers a plain crop, a crop
   that extends the canvas (the new area is transparent) and a straightened
   crop (the frame turned about its centre). It is one undo step. With
   `angle == 0` every layer moves by whole pixels, exactly. With a turn,
   pixel layers resample bilinearly, smart objects compose the turn into
   their transform, and masks follow their layer. **Text layers are not
   turned**: their anchor follows the mapping and they stay upright and
   editable, because text cannot rotate without rasterizing.
   `delete_cropped: false` keeps pixel-layer pixels beyond the new edges
   (Photoshop's "Delete Cropped Pixels" unticked). The older
   `CropDocument` (crop to selection) is unchanged.

2. **Guides are document data.** `Document::guides: Vec<Guide>` (vertical
   or horizontal, position in canvas pixels, fractions allowed) is covered
   by undo (`AddGuide`, `MoveGuide`, `RemoveGuide`, `ClearGuides`, which
   report an empty redraw area), shifted by crops, and saved:
   - `.lumen`: a `guides` array in the manifest (`#[serde(default)]`, left
     out when empty), so older files load and older builds ignore it;
   - PSD: image resource 1032 (grid and guides), positions in 1/32 px,
     validated with psd-tools; drags round positions to 1/32 px so the
     two formats agree.
   The view switches (rulers, guides shown/locked, grid, snap) and the grid
   spacing are user preferences, not document data.

3. **Snapping maths is engine code.** `lumenply_core::snap` finds the
   smallest nudge that puts a dragged edge (or a moving rectangle's edges
   and centre) onto a guide, canvas edge or centre, another layer's painted
   bounds, or a grid line, within a tolerance the UI passes in document
   pixels (6 screen px / zoom). Ties go guide > canvas > layer > grid.
   Layer bounds cost a pixel scan, so lines are collected once per drag.

## Consequences

- Scripts and the CLI can crop, straighten and edit guides through the
  same commands as the app.
- Straightening a document with text titles keeps them level; anyone who
  wants them turned rasterizes first.
- ORA export does not carry guides (the format has no place for them).
