# ADR 0011: Smart filters

Date: 2026-10-03. Status: accepted.

## Context

Photoshop's smart filters are filters attached to one layer. They stay
editable, can be hidden, reordered and faded, and they leave the layers
below alone. Lumenply already had live filter *layers*
(`LayerContent::Filter`), but those filter the composite of everything
below them. Applying a filter to one layer non-destructively needs a
filter stack that belongs to the layer and runs on its own pixels.

## Decision

**Model** (`doc::smart_filter`): every `Layer` has a `smart_filters:
SmartFilters`. It holds an ordered list of `SmartFilter`s (a `Filter`
plus `enabled`, `opacity` and `blend`), a stack switch (`enabled`, the
eye on the Layers panel's "Smart Filters" row), an optional filter mask,
and a derived `cache`. The list is in application order: `filters[0]`
runs first. The UI shows it reversed, newest on top, as Photoshop does.
Each filter's result is mixed onto its own input. In Normal mode at
100 % the result is the filtered pixel. Any other mode first blends the
filtered pixel onto the input with the layer blend formulas, and the
opacity then interpolates between the input and that. There is one
filter mask for the whole stack, as in Photoshop: where the mask is black,
the layer shows its unfiltered pixels. Pixel, smart object, text, fill
and shape layers accept smart filters. Groups, adjustment layers and
live filter layers do not.

**Order in the pipeline**: smart filters run on the layer's own pixels,
before its mask, effects, clipping and blend. `Layer::raster_store()`
returns the filtered pixels when the stack is active.
`Layer::content_store()` returns the unfiltered ones. Every path that
reads `raster_store()` therefore sees the filtered layer without special
cases: the compositor (masks, blend modes, effects, clip chains, groups),
thumbnails, merges, stamp, align and snapping bounds, quick select, and
the OpenRaster exporter. Commands that change the layer's own content
read the content directly and keep working on the unfiltered pixels.

**Rendering** (`render::smart_filters`): the cache is filled in chunks of
4×4 tiles. Each chunk reads the layer over its area padded by the sum of
the running filters' `pad()`s. Each filter reads at most its own pad, so
the error from the raster's edge shrinks inward by one pad per filter
and never reaches the chunk's own tiles. The canvas edge is treated as
`apply_filter_in_canvas` treats it (ADR 0009): off-canvas pixels that the
layer does not paint repeat the edge and stay empty afterwards. A smart
filter is therefore exactly the same filters applied destructively, one
after another. Tests check this for one filter, for two filters in both
orders, and at tile and chunk seams. They also check the composite
against the destructive bake, for a pixel layer and for a smart object.

**Caching**: the cache is derived state. It is never saved and is
rebuilt by `fill::refresh_stale`, which the editor runs after every
command and loaders run once. It stores a key: the identities of the
layer's source tiles, the active filters, the mask (default, enabled
flag, tile identities), the canvas size and float mode. Tile identity is
the tile's address. The editor changes tiles copy-on-write while the
previous document, which holds the old tiles, is still alive. A changed
tile therefore always has a new address, so the comparison cannot be
fooled by a reused address. When only source tiles changed, only their
neighbourhood (the changed tiles grown by the pad) is filtered again,
and the other cached tiles are shared. Otherwise the whole layer is
filtered again. Output tiles are stored as compact 16-bit tiles unless
the document is in float mode. The editor widens every command's
affected area by the document's largest smart-filter reach, because a
pixel edit under a blur changes the image that far around it.

**GPU**: `GpuCompositor::supports` returns false for any layer with an
active stack, so those documents composite on the CPU, which is the
reference. The cache is an ordinary raster, so a later GPU path could
upload it, but it would first need a test showing that it matches the
CPU render.

**Commands** (`core::smart_filter_cmds`): `AddSmartFilter`,
`SetSmartFilter` (parameters, opacity, blend mode and enabled flag;
meant for `execute_coalescing` during slider drags), `RemoveSmartFilter`
(removing the last filter drops the mask), `ReorderSmartFilter`,
`SetSmartFiltersEnabled` and `SetSmartFilterMask` (from the selection,
or cleared). Each is one undo step. Lock all blocks them, as it blocks
effects. `RasterizeLayer` bakes the stack into the pixels and clears it,
as Photoshop does.

**Files**: in a `.lumen` manifest, a layer record gains an optional
`smart_filters` object holding `filters`, `enabled` (written only when
false) and `mask`. The mask's tiles are stored under
`sfmasks/<layer id>/<x>_<y>.a`, in the same format as layer masks.
Layers without smart filters write nothing new, and older files load
with an empty stack. PSD and OpenRaster export the filtered pixels and
warn ("smart filters were baked into its pixels"). In a PSD, the fill or
shape settings of such a layer are left out, so that readers do not
re-render the layer from those settings over the baked pixels.
psd-tools reads the baked layer correctly.

**UI**: on a smart object, Photoshop's rule applies: every Filter-menu
dialog previews the filter as a smart filter and adds it as one. Plain
pixel layers keep the destructive dialogs. Filter ▸ Convert for smart
filters turns a pixel layer into a smart object.

## Consequences

- Older builds ignore the `smart_filters` key, so they open newer files
  without the filters (serde ignores unknown fields).
- A full re-filter costs about as much as a destructive filter.
  Measured on a 4000×3000 layer: Gaussian 8 px takes 0.22 s, Gaussian
  30 px 0.17 s and Mosaic 25 ms. A one-tile paint edit takes 2–24 ms.
  Parameter slider drags on very large layers therefore update at a few
  frames per second. Rendering only the visible area, or a
  reduced-resolution preview during drags, would change no file format.
- The filter mask does not yet follow move, transform, flip or crop
  commands, as layer masks do. It stays in canvas space. It can be
  painted only by recreating it from a selection.
- Photoshop smart filters in PSD files (`SoLd` / `filterFX`) are not
  read or written. Smart-filter layers export as baked pixels.
- Memory: a filtered layer keeps its own pixels plus the cache, about
  twice the memory of the layer. Undo snapshots share cache tiles
  copy-on-write like any other tiles, and the history byte count
  includes them.
