# Lumenply roadmap

Status as of 2026-10-02. About 12,900 lines of Rust, 76 engine tests, clippy-clean.
Built and tested in a headless Linux container: the app was driven with simulated mouse
input under a virtual X display with software OpenGL. **Nothing has run on real
hardware yet**, so section 1 comes before any new features.

Legend: `[x]` done and tested · `[~]` done but unverified or partial · `[ ]` not started

---

## 0. Done so far

### Engine
- [x] Sparse copy-on-write 256×256 tile store; cheap document clones for undo snapshots
- [x] Compact 16-bit tile storage at rest, f32 while editing (halves memory, ADR 0004)
- [x] Premultiplied linear-light compositing, 10 blend modes (W3C formulas), opacity
- [x] Layer tree: pixel, group (isolated), adjustment, live filter, text layers
- [x] Layer masks (sparse, enable/disable), selections (coverage masks)
- [x] Parallel tiled compositor (rayon), partial recomposite of a rectangle
- [x] Affine transforms: exact integer moves, exact 90°/mirror remaps, bilinear otherwise
- [x] 11 adjustments: Brightness/Contrast, Levels, Curves (monotone cubic), Exposure,
      Vibrance, Hue/Saturation, Color Balance, Black & White, Threshold, Posterize, Invert
- [x] Filters: Gaussian blur, box blur, sharpen — destructive or as live layers
      (padded per-tile, edge-clamped)
- [x] Text rasterising with fontdue + bundled DejaVu Sans (regular, bold)

### Editing commands (crates/core)
- [x] Undo/redo with snapshots, coalescing for slider drags, history limit, jump to any step
- [x] Dirty-rect reporting (`Command::affected`) for partial redraws
- [x] Paint/erase strokes with pressure-scaled dabs, Catmull-Rom smoothing
- [x] Fill, clear, paint bucket (tolerance, contiguous, sample merged), gradient (linear/radial)
- [x] Clone stamp (layer or merged source, locked offset)
- [x] Selections: rect, ellipse, lasso/polygon (antialiased), magic wand, union/subtract/
      intersect, invert, feather, mask from selection
- [x] Layers: add, remove, reorder, rename, group/ungroup, collapse, visibility, opacity, blend
- [x] Masks: add (from selection), remove, enable, paint
- [x] Move, free transform (scale/rotate/move), flip layer
- [x] Crop to selection, canvas size with anchor, image size, rotate image 90/180, flip image
- [x] Text layers: add, edit (text, size, bold, colour), move, rasterize

### File formats (crates/io)
- [x] PNG and JPEG import; PNG and JPEG (quality) export
- [x] Native `.nge`: zip with JSON manifest and deflated tiles, versioned (ADR 0002)
- [x] PSD import/export, 8-bit RGB: layers, groups, masks, opacity, visibility, blend modes,
      Unicode names, merged preview; 8 adjustment types both ways; validated with psd-tools (ADR 0003)

### Desktop app (crates/app, egui 0.29)
- [x] Menus: File, Edit, Image, Select, Layer, Filter, View
- [x] 14 tools: Move, Rect/Ellipse marquee, Lasso, Polygonal lasso, Magic wand, Brush, Eraser,
      Clone stamp, Paint bucket, Gradient, Text, Eyedropper, Hand
- [x] Canvas: zoom at cursor, pan, fit/100%, checkerboard, selection outline, brush cursor
- [x] On-canvas free-transform handles with live preview
- [x] Layer panel: tree with indentation, thumbnails, mask thumbnails, collapse, multi-select
- [x] Properties: opacity, blend, per-adjustment controls, interactive curve editor, filter and
      text controls
- [x] Clickable history including redo steps; status bar; dialogs with live filter preview

### Tooling
- [x] CLI: paint demo, composite, render, info, export-psd, bench (with memory report)
- [~] GitHub Actions CI file written but **never run** (`.github/workflows/ci.yml`)
- [x] ADRs 0001–0004 in `docs/adr/`, screenshots in `docs/screenshots/`

---

## 1. Verify on real hardware — do this first

None of these could be tested in the container.

- [~] App builds and launches on Windows, macOS and Linux (X11 and Wayland/XWayland)
      — macOS (Apple Silicon, 2026-10-02): builds clean (tests, clippy, fmt),
      launches and renders; Windows and Linux still unverified
- [ ] Keyboard shortcuts: V B E S G Shift+G W L Shift+L T I M Shift+M H [ ] 0 1,
      Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, Ctrl+A/D/Shift+I, Ctrl+S/O/T/G, Shift+F5, Delete,
      Enter/Esc in free transform -- i checked this
- [ ] Typing in text fields: text tool, layer rename (double-click), file-path dialogs
- [ ] HiDPI / display scaling (canvas maths assume `pixels_per_point` handled by egui)
- [ ] Brush responsiveness at 4K in a release build; measure stroke latency
- [ ] Run the CI workflow on GitHub and fix what breaks -- skip for now
- [ ] Open real-world PSDs from Photoshop/Affinity/Photopea; collect failures as test files

## 2. Performance

- [x] GPU compositor with wgpu (render::GpuCompositor): blend modes, masks, groups
      (incl. pass-through) and LUT adjustments in WGSL, equality-tested against the
      CPU reference; falls back to CPU for live filters and per-pixel adjustments
- [~] GPU follow-up: the backdrop below the edited layer is now cached on the
      GPU (note_change-driven, clip-aware, equality-tested warm and cold);
      still to come: dirty-rect uploads of the edited layer itself, and
      rendering into the UI surface (eframe wgpu backend) to drop the readback
- [x] Per-layer composite caching (render::BelowCache): consecutive edits to one
      layer reuse the composited backdrop below it; live filters above the edit
      fall back to the reference path
- [x] Faster thumbnails (group composites only rebuild when the change touches them)
- [ ] Multithreaded filters inside a tile; SIMD for blend loops
- [x] Memory: cap undo history by bytes (Editor::history_memory_limit, default 1 GiB)
- [ ] Optional 8-bit storage mode
- [x] Incremental brush rendering (only the smoothing window repaints per frame)

## 3. Input

- [ ] Pen pressure and tilt (egui doesn't expose them; evaluate `octotablet`), mapping
      curves for size/opacity
- [x] Customisable shortcuts (the command chords rebind in Preferences with
      click-to-capture; stored in prefs.json; Ctrl+Y stays a redo alias)
- [ ] Touch/trackpad gestures (pinch zoom works through egui, untested)

## 4. Colour and formats

- [~] ICC colour management (qcms, pure Rust): embedded PNG/JPEG profiles are
      converted to sRGB on import (neutral-preserving, no-op on bad profiles);
      exports are tagged — PNGs (8/16-bit) carry the sRGB chunk, JPEGs embed
      a CC0 compact sRGB profile (APP2), both Pillow-verified. Still open:
      profiles on deep (16-bit) imports (qcms transforms 8-bit only),
      display profile
- [x] 16-bit PNG/TIFF import/export (full precision in, 16-bit sRGB out)
- [ ] Float/HDR document mode (skip compaction; ADR 0004)
- [x] WebP import; OpenEXR import/export (linear f32 both ways — lossless for
      this engine's native pixels; bit-exact round-trip tested)
- [ ] AVIF/HEIF; camera RAW via rawler or LibRaw
- [~] PSD: 16-bit import (raw, RLE and ZIP ± prediction channels) and
      export (raw channels; Export menu), full precision both ways and
      psd-tools-cross-checked; Black & White/Exposure/Vibrance round-trip
      (expA fixed block; blwh/vibA Action Descriptors, psd-tools-parsed —
      blwh maps PS's six weights onto our three). Still open: PSB (large
      documents), text layers as editable PSD text, live filters as
      smart filters
- [x] OpenRaster (.ora) import/export for GIMP/Krita interchange (layers, groups,
      opacity, visibility, the ten blend modes; masks baked in, adjustments and
      live filters skipped with warnings)
- [ ] CMYK soft-proofing

## 5. Tools and features

- [x] Healing brush and spot healing (rim-diffusion colour, optional texture
      from a clone-style source; new Heal tool, key J, Spot toggle)
- [x] Pen tool and paths: cubic-bezier work path (click corners, drag curves,
      close on the first point; one undo step per path), fill / stroke /
      selection-from-path, saved in .nge
- [~] Path editing: drag anchors and handles after placing (click selects a
      node and shows its handles, symmetric handles stay mirrored, Backspace
      deletes the node); holes via even-odd across subpaths (fill and
      path-to-selection). Still to do: multiple named paths
- [x] Skew and non-uniform scale in free transform (edge handles stretch one
      axis, mirror across the centre; W/H/Rotate/Skew fields in the options bar)
- [ ] Perspective and warp transforms (non-affine; needs a mesh resampler)
- [~] Brush engine: spacing jitter, dodge/burn, smudge (drags pixels along
      the stroke from a per-dab snapshot) and sponge (Sat+/Sat− scale chroma
      around gamma luminance) are in. Still to do: presets, textures
- [x] Layer styles: drop shadow, outer glow and stroke as non-destructive
      per-layer effects (EFFECTS section in Properties, tile-seam-safe,
      saved in .nge; PSD/ORA warn instead of silently dropping)
- [~] More styles: inner shadow and inner glow (blurred inverse coverage,
      clipped to the layer, rendered over it). Still to do: bevel,
      gradient/pattern overlay; effects on clip-chain members and
      pass-through groups
- [ ] Smart objects (embedded documents with transforms)
- [x] Clipping masks (clip chains composite as a unit gated by the base's alpha
      and carrying its blend/opacity; context menu + palette; PSD clipping byte
      round-trips; GPU path falls back to CPU for clipped documents)
- [x] Pass-through groups (compositor, Blend dropdown, .nge/.psd/.ora round trip)
- [x] Histogram panel (composite luminance, in Properties); auto contrast as a
      Levels adjustment layer
- [x] Per-channel Levels (Master/R/G/B selector; LutRgb compilation, CPU
      path; PSD levl records 1-3 round-trip, psd-tools-verified) and
      Auto color (Image menu + palette: per-channel 0.1% percentile
      stretch as a Levels adjustment layer)
- [x] More filters: noise (position-seeded), motion blur, median, high pass —
      destructive or live, with dialog previews and palette entries
- [~] Text: font picker (system fonts via fontdb, .ttc face index honoured),
      italic (real face or synthetic oblique), alignment (left/centre/right).
      Still to do: kerning/tracking controls, on-canvas text editing
- [x] Animated marching ants (boundary dashes march; huge outlines fall back
      to the static texture)
- [x] Select by colour range (Select menu + palette: fuzziness slider with live
      marching-ants preview, soft graded edges, cancel undoes)
- [x] Quick-mask mode (Q): paint the selection under the classic red overlay —
      white selects, black deselects, live while stroking
- [ ] AI tools (local ONNX): subject select, object removal, upscaling — see project overview

## 6. Usability

- [x] Multiple open documents: tab strip in the top bar (switch, close with
      per-tab unsaved confirm, + for new; opening an already-open file
      focuses its tab; per-document zoom/pan/active layer; quitting checks
      every tab). Autosave still covers the active document only
- [x] Command palette (Ctrl+K) searching every menu action
- [x] New layer from selection (`NewLayerFromSelection` command; selection
      action bar "New layer" and palette "Layer via copy")


- [x] Native file dialogs (`rfd`) instead of typed paths
- [x] Autosave every 2 min to ~/.nge (atomic, off-thread), crash-recovery prompt
      at startup, recent-files menu
- [x] Unsaved-changes prompt on close
- [ ] Docking panels (egui_dock)
- [x] Drag-to-reorder layers (across groups, with an insertion line)
- [x] Layer context menu (rename, reorder, flip, mask, ungroup, delete)
- [x] Preferences (Edit menu, persisted to ~/.nge/prefs.json): canvas surround
      colour, undo step and memory caps, autosave interval
- [x] Split `crates/app/src/main.rs` into modules (theme, tools, menu, options bar,
      canvas, layers, properties, history, status, dialogs, palette)
- [ ] Accessibility (AccessKit labels), translations

## 7. Ecosystem and release

- [ ] Scripting: Python via PyO3 on the command API; macro recording from history
- [ ] Sandboxed WASM plugins (wasmtime)
- [ ] Browser build (WebAssembly + WebGPU) — engine crates are UI-free by design
- [ ] Packaging: Windows installer, signed macOS app, Flatpak; nightly builds from CI
- [x] Project name: **Lumenply** (brand assets in img/; crates, CLI, titles,
      `.lumen` extension and `~/.lumenply` all renamed; legacy `.nge` loads)
- [~] Full GPLv3 text now in `LICENSE`. A preliminary web search (Oct 2025)
      found no "Lumenply" mark; the closest is "LUMENLY" (US filing, 2023,
      lighting fixtures — a different class). A proper clearance search
      before wide publication, and a CLA bot, remain open
- [ ] User documentation and a website

---

## Fixed after code review (2026-10-02)

- `Mask::combine` seeded new tiles at 0 instead of the mask default (broke
  Select All + subtract/intersect)
- masks were corrupted by move/transform/resize/rotate/flip (zero tiles pruned,
  area outside tiles reset); masks now transform through `transform_mask`
- project save is atomic (temp file + rename); a failed save no longer
  destroys the existing file
- PSD reader and .nge loader hardened against malformed/malicious files
  (bounds checks, size caps, duplicate-id and coordinate validation)
- blur radii sanitized (NaN/negative/infinite); curves no longer panic on NaN
- app shell: shortcuts no longer fire during dialogs, drags or free transform;
  stale transform previews cleared; clone single-click offset fixed

## Known limitations and debts

- Pressure is fixed at 1.0 in the app (engine supports it).

- Text layers cannot be scaled or rotated without rasterizing.
- The selection outline is static, not animated.
- `LICENSE` is a placeholder pointing to the GPLv3 text.

