# NGE roadmap

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

- [ ] App builds and launches on Windows, macOS and Linux (X11 and Wayland/XWayland)
- [ ] Keyboard shortcuts: V B E S G Shift+G W L Shift+L T I M Shift+M H [ ] 0 1,
      Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, Ctrl+A/D/Shift+I, Ctrl+S/O/T/G, Shift+F5, Delete,
      Enter/Esc in free transform
- [ ] Typing in text fields: text tool, layer rename (double-click), file-path dialogs
- [ ] HiDPI / display scaling (canvas maths assume `pixels_per_point` handled by egui)
- [ ] Brush responsiveness at 4K in a release build; measure stroke latency
- [ ] Run the CI workflow on GitHub and fix what breaks
- [ ] Open real-world PSDs from Photoshop/Affinity/Photopea; collect failures as test files

## 2. Performance

- [ ] GPU compositor with wgpu: blend modes and adjustments in WGSL, CPU path as reference,
      equality tests between the two
- [ ] Per-layer composite caching so editing a top layer doesn't recomposite everything below
- [ ] Faster thumbnails (currently recomputed for every layer on full refreshes)
- [ ] Multithreaded filters inside a tile; SIMD for blend loops
- [ ] Memory: cap undo history by bytes, not step count; optional 8-bit storage mode
- [ ] Incremental brush rendering (only new dabs per frame, not the whole stroke)

## 3. Input

- [ ] Pen pressure and tilt (egui doesn't expose them; evaluate `octotablet`), mapping
      curves for size/opacity
- [ ] Customisable shortcuts; Space-drag to pan; Alt-scroll zoom
- [ ] Touch/trackpad gestures (pinch zoom works through egui, untested)

## 4. Colour and formats

- [ ] ICC colour management (lcms2): embedded profiles on import/export, display profile
- [ ] 16-bit PNG/TIFF import/export; float/HDR document mode (skip compaction; ADR 0004)
- [ ] TIFF, WebP, AVIF/HEIF, OpenEXR; camera RAW via rawler or LibRaw
- [ ] PSD: 16-bit, PSB (large documents), Black & White/Exposure/Vibrance descriptors,
      text layers as editable PSD text, live filters as smart filters (or rasterised with a note)
- [ ] OpenRaster (.ora) for GIMP/Krita interchange
- [ ] CMYK soft-proofing

## 5. Tools and features

- [ ] Healing brush and spot healing
- [ ] Paths / pen tool (vector), stroke and fill paths, selection from path
- [ ] Perspective, skew and warp transforms; non-uniform scale in free transform
- [ ] Brush engine: presets, textures, spacing jitter, smudge, dodge/burn, sponge
- [ ] Layer styles (drop shadow, stroke, glow) as non-destructive effects
- [ ] Smart objects (embedded documents with transforms)
- [ ] Clipping masks; pass-through blend mode for groups
- [ ] Histogram panel; auto levels/colour
- [ ] More filters: noise, motion blur, unsharp mask parameters, median, high pass
- [ ] Text: font picker (system fonts), italic, alignment, kerning/tracking, text on canvas
- [ ] Animated marching ants; quick-mask mode; select by colour range
- [ ] AI tools (local ONNX): subject select, object removal, upscaling — see project overview

## 6. Usability

- [ ] Native file dialogs (`rfd`) instead of typed paths
- [ ] Autosave and crash recovery; recent files; unsaved-changes prompt on close
- [ ] Docking panels (egui_dock); drag-to-reorder layers; layer context menu
- [ ] Preferences: theme, canvas colour, undo limit, memory limit
- [ ] Split `crates/app/src/main.rs` (~3,900 lines) into modules: tools, panels, dialogs, canvas
- [ ] Accessibility (AccessKit labels), translations

## 7. Ecosystem and release

- [ ] Scripting: Python via PyO3 on the command API; macro recording from history
- [ ] Sandboxed WASM plugins (wasmtime)
- [ ] Browser build (WebAssembly + WebGPU) — engine crates are UI-free by design
- [ ] Packaging: Windows installer, signed macOS app, Flatpak; nightly builds from CI
- [ ] Final project name and trademark search; full GPLv3 text in `LICENSE`; CLA bot
- [ ] User documentation and a website

---

## Known limitations and debts

- Pressure is fixed at 1.0 in the app (engine supports it).
- Free transform scales uniformly only.
- Text layers cannot be scaled or rotated without rasterizing.
- The selection outline is static, not animated.
- `LICENSE` is a placeholder pointing to the GPLv3 text.
- No git history yet: run `git init` before the first change.
