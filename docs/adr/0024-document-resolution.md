# ADR 0024: Document resolution is ppi metadata, stored per format

Date: 2026-10-03 · Status: accepted

## Context

Print work needs a document resolution: Image Size in inches or
centimetres, New-document presets such as A4 at 300 ppi, PDF export at the
true page size, and files that tell the next program how large to print.
Every interchange format stores it differently, with different precision
and units.

## Decision

- `Document.resolution: f32` in **pixels per inch**, 72 by default (as
  Photoshop). It is metadata only: no compositor, tool or filter reads it.
  Undo covers it like any other document field (whole-document snapshots).
- `SetResolution { ppi }` changes it without resampling. `ResizeImage`
  gained `resolution: Option<f32>`, so Image Size is always **one**
  command (and one history step) whether it resamples, re-tags, or both;
  an unchanged pixel size never resamples. Range 1–30 000 ppi.
- Files:
  - `.lumen`: manifest field `resolution`, always written; a missing field
    (older files) loads as 72. Additive, no format-version bump.
  - PSD: resource 1005 (ResolutionInfo) always written, horizontal and
    vertical as 16.16 fixed **ppi**, display units ppi / inches. On read the
    value is ppi whatever the unit field says (the unit fields are display
    preferences only; Photoshop files with unit 2 = px/cm still store ppi,
    checked against the psd-tools corpus).
  - PNG: `pHYs` in pixels per metre; JPEG: JFIF density in whole dots per
    inch (fractional ppi round on JPEG export). Written by patching the
    encoded bytes (`crates/io/src/resolution.rs`), so the codecs and their
    callers stay resolution-free.
  - Read-only: EXIF/TIFF XResolution (+ ResolutionUnit) and a Photoshop
    APP13 1005 block inside JPEGs (precedence APP13 > EXIF > JFIF).
  - Values read from files snap to the whole number they came from when
    within 0.02 ppi (PNG's 11811 px/m reads 300, not 299.9994), else keep
    two decimals.
- Not stored: TIFF export (the `image` crate's TIFF encoder has no
  resolution tags), WebP and GIF (no standard field), ORA.

## Consequences

- Exports (File ▸ Export PNG/JPEG/16-bit PNG, Export As, `lumenply render`,
  `lumenply batch`) carry the document's resolution unchanged, even when
  Export As or batch resizes the pixels (the tag stays the document's; the
  print size of a resized export changes with its pixel count).
- PSDs written by Lumenply now always have a non-empty image-resources
  section; code that assumed an empty one (one test did) must read the
  section length.
- Print-size zoom has no reliable physical display DPI to work from; it
  uses a per-platform points-per-inch estimate and is labelled approximate.
