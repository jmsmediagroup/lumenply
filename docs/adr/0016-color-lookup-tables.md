# ADR 0016: Color Lookup tables (3D LUTs)

Date: 2026-10-03. Status: accepted.

## Context

Photoshop's Color Lookup adjustment applies a 3D LUT (`.cube`, `.3dl`,
`.look`) or an ICC abstract / device-link profile. Colourists exchange
looks as `.cube` files, and PSDs carry Color Lookup layers. A LUT is far
bigger than any other adjustment's parameters (a 33³ table is 36k RGB
triples), it must survive `.lumen` and PSD round trips, and a project must
never depend on a LUT file elsewhere on disk.

## Decision

**Model** (`doc::lut`): `Lut3D` is an `N³` lattice (2 ≤ N ≤ 65) over a
per-channel input domain, red varying fastest (the `.cube` order), plus an
optional per-channel 1D shaper applied first. A 1D-only file is a shaper
over the identity 2³ cube, so a long 1D table is never coarsened into a
cube. `Adjustment::ColorLookup { lut: Arc<Lut3D>, name }` shares the table,
so undo snapshots and per-tile compiles never copy it.

**Maths**: tetrahedral interpolation (Resolve / OCIO), inputs clamped to
the domain, outputs to `[0, 1]`. The table sees gamma-encoded sRGB values
like the other adjustments (ADR 0005), because LUT files are authored for
display-referred, encoded RGB; the per-pixel transfer uses the existing
4096-entry tables. The GPU path sends Color Lookup layers to the CPU.

**Files** (`io::lut_files`): Adobe and Resolve `.cube` (1D, 3D, both;
`DOMAIN_*`, `*_INPUT_RANGE`) and Autodesk `.3dl` (blue fastest; output
depth from `Mesh` or the largest value). Parsers return line-numbered
errors and never panic. The `.cube` writer prints shortest round-trip
decimals, so a table survives text exactly.

**`.lumen`**: each distinct table is one entry, `luts/<FNV-1a 64 of the
table>.cube`; a layer is a `"kind": "color-lookup"` record naming its entry
and display name. Equal tables are written once and share one `Arc` again
after loading.

**PSD** (`clrL`, version 1 + descriptor 16): read `LUT3DFileData` (the
original file's bytes) in the format `LUTFormat` names; an empty
descriptor is Photoshop's fresh, unchosen lookup (identity); profile-only
and `.look` lookups import as an empty lookup with a warning. Written the
way Photoshop writes it: the table as `.cube` text in `LUT3DFileData`, plus
the ICC v4 device link Photoshop stores in `profile` (identity curves
around a 16-bit CLUT). A Photoshop CS6 file confirms the layout: its CLUT
equals the embedded `.cube` read red-fastest exactly. A shaper plus cube
is baked into one 65³ cube for PSD (Adobe's `.cube` holds one table).

**Built-in looks** are formulas in code (`doc::lut::looks`) baked to 33³:
no third-party LUT files ship, so there is nothing to license.

**Export** (File ▸ Export ▸ Color Lookup Table, `render::lut_bake`): the
33³ lattice becomes the pixels of a scratch document, the visible
adjustment layers (inside visible groups too) are stacked over it with
their opacity and blend mode, and the reference compositor renders the
table, so it is exactly what the adjustments do. Masks, clipping and every
pixel, fill or filter layer are left out: a colour table cannot depend on
where a pixel is.

## Consequences

- Older builds cannot open projects with Color Lookup layers (unknown
  record kind); older projects load unchanged.
- PSD files grow by the `.cube` text and the device link (about 1 MB and
  215 KB for a 33³ table).
- ICC-profile lookups and `.look` files are not evaluated yet; they keep
  their layer (mask, blend, opacity) as an identity lookup.
- `dataOrder` / `tableOrder` are not interpreted: every Photoshop file seen
  uses the standard orders for its format.
