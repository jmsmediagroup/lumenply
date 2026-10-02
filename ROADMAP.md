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
- [ ] Typing in text fields: text tool, layer rename (double-click), dialog fields
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
- [~] Filters are row-parallel via rayon (box blur, noise, motion blur,
      median, high pass). Blend loops measured (`lumenply bench --size 2048
      --layers 10`, 14 threads): Normal-only 4.5 ms (~9.3 GP·layers/s),
      mixed modes 20.9 ms (~2.0 GP·layers/s). Tried A/B and rejected:
      a branchless Normal loop (within noise), per-mode monomorphised
      loops (mixed 75% slower), and fusing the u16→f32 decode into the
      blend loop (Normal 2.7× slower — the separate decode pass vectorises,
      the fused branchy loop doesn't). Profile (Instruments / flamegraph)
      before trying anything else here
- [x] Memory: cap undo history by bytes (Editor::history_memory_limit, default 1 GiB)
- [ ] Optional 8-bit storage mode
- [x] Incremental brush rendering (only the smoothing window repaints per frame)

## 3. Input

- [~] Pen pressure: Windows (pen touches' `force`) and macOS (AppKit event
      monitor on tablet mouse events) feed `StrokePoint` size and a new per-dab
      opacity, routed by Photoshop-style pen toggles beside Size and Opacity in
      the brush bar (saved in prefs). Still open: verify on real tablets, Linux
      (X11/Wayland tablet protocols, e.g. `octotablet`), tilt, response curves
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
- [x] Float/HDR document mode (SetFloatMode command + Image-menu toggle;
      compaction skipped while on, lazy 16-bit return when off; EXR opens
      in float mode; flag + raw-f32 HDR values survive .lumen)
- [x] WebP import; OpenEXR import/export (linear f32 both ways — lossless for
      this engine's native pixels; bit-exact round-trip tested)
- [x] Camera RAW import (rawler 0.7, pure Rust: CR2/CR3/NEF/ARW/RAF/ORF/RW2/
      PEF/DNG and more): demosaic, camera white balance and colour matrix,
      default crop, EXIF orientation; linear output into the 16-bit document.
      Verified on CC0 raw.pixls.us samples (Sony A7S ARW, Canon R6 CR3,
      Panasonic LX7 RW2: 0.1-0.3 s each); lossy-JPEG DNGs (some cinema
      cameras) are not supported by rawler 0.7 and fail with a clear error.
      Still open: a develop dialog (exposure, highlights/shadows, profile
      tone curve) instead of the flat linear default
- [ ] AVIF/HEIF
- [~] PSD: 16-bit import (raw, RLE and ZIP ± prediction channels) and
      export (raw channels; Export menu), full precision both ways and
      psd-tools-cross-checked; Black & White/Exposure/Vibrance round-trip
      (expA fixed block; blwh/vibA Action Descriptors, psd-tools-parsed —
      blwh maps PS's six weights onto our three); PSB import (version 2:
      8-byte section/channel lengths, 4-byte RLE counts, 300k dim cap,
      wide-length block keys; .psb accepted by app and CLI, fixture
      psd-tools-verified). Still open: PSB export, text layers as
      editable PSD text, live filters as smart filters
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
- [x] Path editing: drag anchors and handles after placing (click selects a
      node and shows its handles, symmetric handles stay mirrored, Backspace
      deletes the node); holes via even-odd across subpaths (fill and
      path-to-selection); multiple named paths (Save path + Paths list in
      the pen options bar, saved in .lumen, every change undoable)
- [x] Skew and non-uniform scale in free transform (edge handles stretch one
      axis, mirror across the centre; W/H/Rotate/Skew fields in the options bar)
- [x] Perspective and warp transforms: homography resampler (rect → quad)
      with PerspectiveLayer, and a mesh warp (WarpGrid of (n+1)² points,
      per-cell closed-form inverse bilinear, bilinear sampling) with
      WarpLayer; masks follow both mappings. Edit menu + palette gain
      Perspective and Warp (straight into the mode); free transform has
      Perspective/Warp toggles (4×4 mesh, drag points, drag elsewhere to
      move all; disabled with a reason for smart objects)
- [~] Brush engine: spacing jitter, dodge/burn, smudge (drags pixels along
      the stroke from a per-dab snapshot), sponge (Sat+/Sat− scale chroma
      around gamma luminance) and presets (shape parameters saved in
      prefs.json; Save preset + dropdown in the brush bar). Still to do:
      textures
- [x] Layer styles: drop shadow, outer glow and stroke as non-destructive
      per-layer effects (EFFECTS section in Properties, tile-seam-safe,
      saved in .nge; PSD/ORA warn instead of silently dropping)
- [~] More styles: inner shadow and inner glow (blurred inverse coverage,
      clipped to the layer, rendered over it); colour and gradient overlays
      (gradient spans the content bounds at any angle); bevel (emboss from
      the blurred-coverage gradient, light angle/depth/size); effects on
      clip-chain members (rendered inside the unit, clipped by the base)
      and on chain bases; a styled pass-through group composites isolated,
      as in Photoshop. Still to do: pattern overlay
- [~] Smart objects v1: a layer keeps untouched source pixels plus a
      cumulative affine; every transform (free transform, image resize,
      rotate, flip, crop shift) composes and re-renders from the source, so
      repeated transforms never degrade. Convert/Rasterize in the layer
      context menu + palette, "Smart" chip, saved in .lumen (cache
      rebuilds on load), PSD/ORA export as pixels with a warning.
      Still open: embedded multi-layer documents ("edit contents")
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
- [~] Text: searchable font picker (system fonts via fontdb, .ttc face index
      honoured), bold/italic/bold-italic (real faces, else synthetic oblique
      and synthetic bold), alignment (left/centre/right), tracking in em/1000;
      missing fonts render in DejaVu Sans with a "Missing fonts" notice on open;
      Text-tool clicks edit or reposition the active text (Shift+click or
      "New text" adds a layer). Still to do: on-canvas caret editing,
      editable PSD text (TySh; export rasterises and names the font)
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


- [x] Native file dialogs (`rfd`) instead of typed paths: per-kind filters,
      start in an existing folder, suggested names, enforced extensions
      (Linux needs xdg-desktop-portal or zenity at run time)
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
- [x] Accessibility: eframe's `accesskit` is on, so VoiceOver / Narrator /
      Orca see the UI. Every focusable control has a spoken name (custom-painted
      ones via `widget_info` or `theme::a11y_name`), dialogs announce as
      dialogs with their title, floating bars and backdrops are not Tab stops,
      visible focus rings. `a11y_tests` (main.rs) render every tool, layer kind,
      layer effect, adjustment, filter, dialog and the palette and fail on any
      unnamed control. (The earlier `futures-sink` resolve failure was a stale
      local index cache entry, not the registry.)
- [ ] Accessibility, still open: real screen-reader walkthrough on each OS;
      egui's own scroll bars are focusable but unnamed (inside egui, so the
      test skips thin unnamed strips); translations

## 6b. Quality pass (2026-10-02)

- [x] Popups, menus, context menus and combo boxes size to their content,
      never wrap, and stay on-screen (shared helpers in theme.rs; the
      "one letter per line" bug is gone everywhere)
- [x] One action registry (palette.rs) behind menus, palette and runner:
      unavailable items are greyed with a reason; shortcut labels follow the
      platform and the user's rebindings; File ▸ Export submenu; Help menu
- [x] Layout: responsive at 900×600 and up (two-column rail, folding options
      bar, capped dock), elided names with tooltips (no hard clips), compact
      foldable History strip, modal dialogs with Enter/Esc, consistent
      label-left rows, mono numbers, hover/focus/disabled states
- [x] Colour picker: one sRGB picker (spectrum, hex, R/G/B, recents,
      canvas eyedropper) for every colour control; fixed swatches that
      displayed sRGB values as linear (washed out)
- [x] Welcome screen and no-document state (New, Open, demo, recent files,
      drop zone); File ▸ Close / Ctrl+W; crash recovery as a tested method
- [x] Demo document is a real photograph ("Aoraki Sunrise", CC0, see
      crates/app/assets/NOTICE.md) with non-destructive layers
- [x] Headless verification: `--screenshot-do` debug tokens per UI area,
      `--window-size`, screenshot windows open without taking focus;
      before/after images in docs/screenshots/quality/
- [x] Second pass, from a scripted audit of all 16 tool bars (1600/1024 px),
      every menu, dialog and Properties layer kind, and the 900×600 minimum:
      Preferences fits short windows (scrolling shortcut list); two-column
      quick-add menu; tight-tier bars fit at 1024 px; square checkboxes;
      0–1 values in human units (%, ±100, 0–255) without float drift;
      draggable Properties/Layers divider; smart objects get a Properties
      section and lossless flips everywhere; View ▸ Zoom in/out (Cmd+= / Cmd+−)
      and History toggle; egui's keyboard UI zoom disabled so Cmd+=/−/0 act
      on the canvas; Esc cancels "New text"; Untitled-N never skips

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
- Opening files from Finder (double-click, Open With, Dock drop) does nothing:
  eframe 0.29 does not deliver those events.
- egui menus cannot scroll; the quick-add "More..." list is tall (~500 px).

