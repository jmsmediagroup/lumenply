# 11. All 27 Photoshop blend modes, still blended in linear light

Date: 2026-10-03. Status: accepted.

## Context

Lumenply had the ten W3C blend modes. PSDs using any of Photoshop's other
seventeen (Dissolve, the burns and dodges, Vivid/Linear/Pin Light, Hard
Mix, Exclusion, Subtract, Divide, Darker/Lighter Color, Hue, Saturation,
Color, Luminosity) opened as Normal with a warning.

## Decision

- `BlendMode` gains the seventeen modes **appended** after the original
  ten. Discriminants are stable (the WGSL shader indexes modes by them)
  and the serde names (`color-burn`, `hard-mix`, …) are the `.lumen`
  format, so neither may be reordered or renamed. `BlendMode::GROUPS`
  holds Photoshop's menu order; `label()` its display names.
- Formulas are Photoshop's, checked against real Photoshop composites in
  the psd-tools corpus: W3C where W3C defines the mode; Rec. 601 luma
  (0.3/0.59/0.11) with W3C `SetLum`/`SetSat`/`ClipColor` for the
  non-separable modes; Darker/Lighter Color pick the whole colour by
  that luma. Measured edge conventions: Color Burn/Dodge let the
  backdrop win at b = 1 / b = 0; **Vivid Light lets the source win**
  (s = 0 → 0, s = 1 → 1); Hard Mix is 1 where b + s ≥ 1 but keeps black
  under white and white under black. Comparisons with 0/1 use a 1e-6
  margin so float noise can't flip a channel.
- **Dissolve** thresholds the layer's alpha × opacity against a fixed
  integer hash of the canvas pixel coordinates (`blend::dissolve_noise`).
  The pattern is therefore part of how a saved document looks: changing
  the hash changes every existing Dissolve layer. It cannot match
  Photoshop's pattern pixel for pixel; its coverage matches (38.4 % vs
  38.6 % on dissolve.psd). The GPU path declines Dissolve (CPU fallback);
  every other mode is ported to WGSL and equality-tested.
- Blending stays in **linear light** (ADR 0005). The blend functions are
  colour-space agnostic (straight colour in [0, 1]), so a Photoshop-style
  gamma-blending option can reuse them unchanged.

## Measurements (corpus, mean |diff| on 0–255 against Photoshop's composite)

| | psd-tools blend-modes/ (29 files) | same without dissolve.psd | whole corpus (352 rendered files) |
|---|---|---|---|
| before (10 modes) | 20.45 | 18.67 | 15.38 |
| after, linear light | 18.07 | 16.33 | 15.14 |
| experiment: only B() on gamma values | 17.25 | 15.48 | 15.06 |
| experiment: layer compositing (`blend_pixel`) in gamma | 2.96 | 0.67 | 13.32 (109 better, 9 worse) |

The remaining gap on the blend test files is almost entirely the colour
space of compositing, not the formulas: with alpha compositing done on
gamma-encoded values, every one-mode file (normal.psd … luminosity.psd)
matches Photoshop to 0.25–0.52 levels (p99 4). The experiment only
converted `blend_pixel`; pass-through mixing, masks on adjustments and
clip chains stayed linear, which is where its 9 regressions are. Whether to offer that (per document, like Photoshop's
"Blend RGB colors using gamma") is left open; the experiment was not
committed.

## Consequences

- PSD, OpenRaster (`svg:color-burn`, `svg:color-dodge`, `svg:exclusion`,
  `svg:hue`, `svg:saturation`, `svg:color`, `svg:luminosity`; the rest
  export as normal with a warning) and `.lumen` carry every mode.
- Older builds reading a `.lumen` that uses a new mode fail to parse that
  layer's blend; files using only the original ten are unchanged.
