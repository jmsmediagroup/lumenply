# ADR 0010: Sampled brush tips, shape dynamics and ABR import

Date: 2026-10-03. Status: accepted.

## Context

Photoshop users bring brush libraries (`.abr`) and expect sampled tips,
tip angle and roundness, Shape Dynamics, Scattering, Transfer and Texture.
The engine had one computed round tip with spacing and scatter. Strokes
are replayed from their points for the live preview and must stamp
identical pixels every time, and the default brush must not change.

## Decision

**Tip model** (`core::brush_tip`). `Brush` gains `tip:
Option<Arc<BrushTip>>` (`None` is the computed round tip), `angle`
(degrees, counter-clockwise), `roundness` and `dynamics: BrushDynamics`.
`Brush` is therefore no longer `Copy`; clones share the tip. A tip is a
grayscale coverage image (1 = paint) with a box-filtered mip chain; a dab
stamps it with its longer side spanning the brush diameter, sampling
bilinearly and blending the two mip levels nearest the dab's footprint
(chosen once per dab), so small dabs of detailed tips stay anti-aliased.
Hardness applies only to the round tip.

**The round tip renders bit-identically** to before: with no tip and
roundness 1 the old coverage code runs unchanged, and the old scatter
hashes (salts and dab indices) are kept. A test compares against a
verbatim copy of the old painter.

**Dynamics are hashed, never random**: size, angle, roundness, flips,
scatter, count and transfer jitter each hash the dab's index in the
stroke with their own salt. Texture is a canvas-anchored grain (value
noise over a per-pixel tooth) applied through a soft threshold at the
depth, so the bare paper survives dab build-up. Dabs still build up per
dab (flow-like); there is no stroke-level opacity buffer, so opacity and
flow jitter compound rather than differ as in Photoshop.

**ABR import** (`io::abr`) reads v1/v2 computed and sampled brushes and
v6/v7/v10 `8BIMsamp` tips (raw or PackBits, 8 or 16 bit). The `8BIMdesc`
descriptor is scanned, not fully parsed, for each preset's name,
diameter, angle, roundness, spacing, hardness and tip key; descriptor
dynamics are ignored. Unreadable brushes are skipped with a warning.

**Storage**: imported tips are app data, not document data. Each is a
16-bit grayscale PNG `brushes/abr-<FNV-1a of size+pixels>.png` beside
`prefs.json`, listed in `brushes/index.json` (id, name, file, spacing,
size); the content hash makes re-imports idempotent. Built-in tips are
generated at run time (`builtin:<name>`). Presets in `prefs.json` refer
to tips by id; a missing tip falls back to round.

## Consequences

- Code that copied `Brush` must clone it; struct literals need
  `..Brush::default()`.
- Documents never embed tips: replaying history is fine (commands hold
  the `Arc`), but a macro or script that names a tip needs the tip
  installed.
- A stroke-level opacity buffer (true Photoshop flow vs opacity), pen
  tilt/rotation controls, dual brush, colour dynamics and pattern
  textures from ABR files remain open.
