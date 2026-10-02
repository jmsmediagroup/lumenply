# ADR 0014: PSD colour modes convert to RGB on import

Date: 2026-10-03 · Status: accepted

## Context

The PSD reader refused everything but 8/16-bit RGB. In a corpus of 482
real Photoshop files, 70 failed to open: Grayscale (8/16/32-bit), 32-bit
RGB, CMYK, Lab, Bitmap, Indexed, Multichannel and Duotone. The document
model is RGB, premultiplied, linear light (ADR 0004/0005), with a float
mode for HDR.

## Decision

Every Photoshop mode opens by converting to the document's RGB on import
(`crates/io/src/psd/color_modes.rs`); `psd.rs` keeps small hooks. Each
converted file gets one warning that names the conversion and says that
saving writes an RGB PSD.

- Grayscale: gray → R = G = B through the embedded gray profile (qcms,
  256-entry table, interpolated for 16-bit); sRGB gray without one.
  Duotone opens as its gray data (inks not applied). Levels/Curves of
  gray documents keep the gray channel in record/curve 1, mapped onto the
  master.
- 32-bit: Photoshop's floats are linear and are used as they are; the
  document opens in float mode so values above 1 survive. Descriptor
  colours in 32-bit files are linear 0..255 and are re-encoded after
  parsing. ZIP prediction at this depth is byte-planar per row.
- CMYK: through the embedded CMYK profile with qcms (8-bit precision,
  perceptual A2B0), else a built-in Yule–Nielsen Neugebauer model of
  U.S. Web Coated (SWOP) v2: 16 primaries sampled with LittleCMS and
  fitted, mean error 4.6/255 against the profile (the naive
  (1 − C)(1 − K) formula: 15.5/255). Per-channel CMYK levels are dropped.
- Lab: CIELAB D50 → XYZ → linear sRGB with Bradford adaptation, clipped.
- Indexed: palette lookup, transparent index from resource 1047. Bitmap:
  set bits are black. Multichannel: first three channels as RGB.
- 16/32-bit Photoshop files keep their layers in `Lr16`/`Lr32` global
  blocks; the reader now follows them instead of using the flat composite.

## Consequences

- Every corpus file opens (482/482); see the commit for per-mode numbers.
- Layers of CMYK and Grayscale documents now blend, and adjustments now
  apply, in RGB/linear light: blend modes and per-channel adjustments can
  look different from Photoshop (Invert of CMYK inverts RGB, for example).
- A CMYK or Lab document cannot round-trip: saving writes RGB. CMYK
  export and soft-proofing remain separate roadmap items.
- 16-bit CMYK passes through qcms at 8-bit precision.
