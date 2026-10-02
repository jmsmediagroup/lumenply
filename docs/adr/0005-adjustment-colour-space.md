# 5. Adjustments work on gamma-encoded values

Date: 2026-10-02. Status: accepted.

## Context

Pixels rest in premultiplied linear light (ADR 0004), which is right for
compositing, blurring and resampling. The tonal adjustments, however, were
also doing their maths on linear values, and that made every tool behave
unlike what users of Photoshop, GIMP or Krita expect: linear 0.5 is not mid
grey (sRGB 0.5 is linear ~0.214), so a contrast pivot at 0.5 brightened
images, Levels endpoints didn't line up with any familiar histogram, curves
bent around the wrong tones, and PSD adjustment parameters (which Photoshop
stores as gamma-domain values) shifted on import.

## Decision

Adjustment mathematics runs on gamma-encoded (sRGB) values. Each
`Adjustment` declares its working space via `gamma_space()`; everything
returns true except **Exposure**, which is a linear-light tool by
definition (as in Photoshop). Compositing, blend modes, filters,
transforms and storage remain linear — only the adjustment's transfer
curve is computed in the encoded domain.

Implementation: adjustments that compile to a LUT bake
`decode ∘ f ∘ encode` into the table, so they cost nothing extra per
pixel; Invert, Brightness/Contrast and Posterize became LUTs for the same
reason. The remaining per-pixel adjustments (HSL-based ones, Color
Balance, Black & White, Threshold) convert through 4096-entry transfer
LUTs. The exact transfer functions are `adjust::srgb_encode/srgb_decode`.

The UI follows: the histogram and the auto-contrast percentiles are
binned in the gamma domain, matching the Levels scale.

## Consequences

- Adjustments now match the familiar Photoshop behaviour, and PSD
  adjustment parameters import with their intended meaning.
- Documents saved before this change render slightly differently wherever
  they contain adjustment layers (Exposure excepted). There is no format
  change; only the maths moved.
- A future GPU port must reproduce the same wrapping (cheap: the LUTs
  upload as 1-D textures).
- Numeric tests express expectations in the gamma domain via the exact
  transfer functions, so the intent stays visible in the assertions.
