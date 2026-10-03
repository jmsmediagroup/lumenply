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
- [x] Premultiplied linear-light compositing, all 27 Photoshop blend modes
      (W3C/Photoshop formulas incl. Hue/Saturation/Color/Luminosity,
      Darker/Lighter Color, canvas-anchored Dissolve; ADR 0015), opacity
- [ ] Decision for the owner: optional gamma blending (Photoshop's "Blend RGB
      colors using gamma"). Measured: with layer compositing on gamma values
      the 29 blend-mode corpus files go from 18.1 to 3.0 mean difference
      (single-mode files within 0.25-0.52 levels) and the whole corpus from
      15.4 to 13.3; linear light stays physically right for blur/resample
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
- [~] Open real-world PSDs: a corpus of 482 real Photoshop files (the psd-tools
      and ag-psd test fixtures) renders through the CLI and is compared with
      the composite Photoshop saved in each file (scripted; mean/p99 diff).
      Baseline 2026-10-03 00:30: 412 open, 186 match within 1/255. After
      tonight's reader work: 482/482 open, 220 within 1/255 (masks, path
      operations, pass-through groups, per-channel Curves, Colorize, Photo
      Filter colour spaces, colour modes, blend modes). Biggest remaining
      gaps: layer effects and fill opacity (in progress), knockout, noise
      gradients, artboards, modern Brightness/Contrast, linear-vs-gamma
      blending. `scripts/psd_corpus.py` runs it. Still open:
      Affinity/Photopea files, a checked-in regression subset

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
      Opening a RAW goes through a Camera Raw–style workspace (live preview,
      histogram; Temperature, Tint, Exposure, Contrast, Highlights, Shadows,
      Whites, Blacks, Vibrance, Saturation, camera tone curve; Auto, Reset,
      before/after with P) that develops at full size into the new document
      (render::develop, hue-preserving tone on perceptual luminance, local
      highlights/shadows from a blurred log-luminance base)
- [x] Filter ▸ Camera Raw Filter (Shift+Cmd+A): the develop workspace on any
      pixel layer or smart object — baked, as a smart filter that re-opens
      with its settings (double-click its row or Properties ▸ Edit in Camera
      Raw; OK is one step), or as a live filter layer; new Texture, Clarity,
      Dehaze and Vignette (also when opening RAW); tile, chunk and
      destructive renders agree within 1e-4 (ADR 0016). Still open: the
      frame following crop/resize, faster live layers on huge canvases,
      PSD Camera Raw smart filters
- [x] Export As (File ▸ Export, palette): PNG / JPEG / lossless WebP, quality,
      transparency (or onto white), output size in px or % with Lanczos-3
      resampling in linear light (render::resample), a preview of the encoded
      result (JPEG decoded back, so artefacts show) and its real file size,
      encoded on a worker thread
- [~] HEIC / HEIF / AVIF import on macOS through ImageIO (orientation and
      colour profile applied, decoded straight into linear-light float;
      tested against HEICs written by the system's sips). Still open:
      other platforms (libheif/dav1d would add C dependencies), export
- [~] PSD: 16-bit import (raw, RLE and ZIP ± prediction channels) and
      export (raw channels; Export menu), full precision both ways and
      psd-tools-cross-checked; Black & White/Exposure/Vibrance round-trip
      (expA fixed block; blwh/vibA Action Descriptors, psd-tools-parsed —
      blwh maps PS's six weights onto our three); PSB import (version 2:
      8-byte section/channel lengths, 4-byte RLE counts, 300k dim cap,
      wide-length block keys; .psb accepted by app and CLI, fixture
      psd-tools-verified). Masks as Photoshop stores them: the pixel mask
      in channel −3 beside a vector mask, vector masks rasterised from their
      path with path operations (combine/subtract/intersect/exclude),
      mask density and feather, both masks multiplied; shape outlines an
      even-odd shape can't hold keep Photoshop's pixels (corpus:
      intersect-group 113.9 → 0, layer_mask_data 46.6 → 26.0). Layer
      styles both ways (lfx2/lmfx/lfxs: drop/inner shadow, outer/inner glow,
      colour/gradient overlay with full gradients, stroke inside/centre/
      outside, bevel; blend modes, spread/choke, global light, shadow
      knockout; psd-tools-verified; layer_effects 108.6 → 5.1) and Fill
      opacity (iOpa) both ways. Artboards come in as groups clipped to
      their rectangle over an editable background fill (artboard-bgcolor
      66.8 → 0.02). Still open: PSB export, live filters as
      smart filters, knockout, satin, pattern overlay, gradient/pattern
      strokes
- [x] PSD import of every colour mode and depth: Grayscale (embedded gray
      profile via qcms), Duotone (as gray), 32-bit RGB/Gray (linear float,
      opens in float mode), CMYK 8/16 (embedded profile via qcms, else a
      fitted U.S. Web Coated SWOP model, 4.6/255 from the real profile),
      Lab (D50 → sRGB, Bradford), Indexed, Bitmap, Multichannel; one warning
      per conversion; layers of 16/32-bit Photoshop files read from
      Lr16/Lr32 (they used to open flattened) (ADR 0014). Still open:
      CMYK/Gray blending and per-channel adjustments in the source space,
      16-bit CMYK at full precision, Duotone inks
- [x] OpenRaster (.ora) import/export for GIMP/Krita interchange (layers, groups,
      opacity, visibility, the ten blend modes; masks baked in, adjustments and
      live filters skipped with warnings)
- [ ] CMYK soft-proofing

## 5. Tools and features

- [x] Healing brush and spot healing (rim-diffusion colour, optional texture
      from a clone-style source; new Heal tool, key J, Spot toggle)
- [x] Retouching as tool modes: Heal ▸ Spot (content-aware by default) /
      Healing / Patch (Source/Destination, Content-Aware) / Move
      (content-aware move, Adapt) / Red Eye; Brush ▸ Blur / Sharpen /
      History (source = any history step, from the bar's From menu or a
      History card's context menu); Eraser ▸ Background (Once/Continuous,
      Contiguous, Protect FG) / Magic; Shift+J and Shift+E cycle modes; a
      Sample menu (Current / Current & below / All) lets Patch and Spot work
      on an empty layer; strokes heal as one region on a parallel membrane
      solver (render::membrane, 900×600 in 0.06 s). Still open: Patch
      Diffusion/Transparent, Move's Structure/Color, Healing "Aligned"
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
- [x] Brush engine: spacing jitter, dodge/burn, smudge, sponge, presets;
      sampled tips (mip-mapped bilinear, angle/roundness) used by every
      brush tool; shape dynamics (size/angle/roundness jitter, angle follows
      stroke, flip), scattering, count, transfer (opacity/flow jitter),
      colour dynamics, canvas-anchored paper grain; Brush settings panel
      with tip picker; Photoshop .abr import (v1/v2, v6/v7/v10, RLE,
      8/16-bit, preset names/spacing/size; 438-tip CC0 set in 0.1 s);
      Edit ▸ Define brush tip; 5 generated tips; the last brush returns at
      launch (ADR 0012). Still open: stroke-level opacity vs flow, pen
      tilt/rotation, dual brush, ABR dynamics/textures
- [x] Layer styles: drop shadow, outer glow and stroke as non-destructive
      per-layer effects (EFFECTS section in Properties, tile-seam-safe,
      saved in .nge; PSD/ORA warn instead of silently dropping)
- [x] Fill opacity (Photoshop's Fill): fades a layer's content but not its
      effects, incl. clip-chain bases and pass-through groups; Fill slider
      under Opacity; SetFillOpacity; .lumen, PSD iOpa, ORA folds it into
      opacity; GPU falls back to CPU below 100%
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
      Edit contents opens the source in its own tab and Save there writes it
      back (one "Update smart object" step in the original, transform kept);
      Replace contents loads an image file in place (Layer menu, palette,
      Properties). Still open: keeping the contents' layers (a nested
      multi-layer document) instead of flattening on save
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
- [x] Photoshop filters: Mosaic (canvas-anchored cells), Emboss, Find Edges,
      Surface Blur, Lens Blur (disc kernel, highlight boost), Dust & Scratches —
      destructive or live, dialog previews, palette entries; tile-seam tested;
      Filter menu grouped Blur/Noise/Pixelate/Sharpen/Stylize/Other (ADR 0009)
- [x] Select > Modify: Expand, Contract, Border, Smooth (exact Euclidean
      distance, antialiased, canvas edge doesn't shrink), live marching-ants
      preview dialogs; Select > Grow and Similar (wand tolerance)
- [x] Content-Aware Fill (multi-scale PatchMatch + EM voting, seeded; Edit menu,
      Shift+Backspace Fill dialog, selection bar): 500×400 hole in 2400×1600 in ~0.2 s
- [x] Puppet Warp (Edit menu, palette): full-window workspace; a mesh over
      the layer's opaque area that hugs the outline (Density, Expansion);
      click to pin, drag to warp live with as-rigid-as-possible deformation
      (Igarashi 2005 two-step, banded Cholesky per pin set; Rigid/Normal/
      Distort); Alt-click/Delete removes; pin depth for folds; OK bakes one
      PuppetWarp step (masks follow; smart objects refused); 1800×1205
      bake ~8 ms (ADR 0018). Still open: pin rotation, multi-select, as a
      smart filter
- [x] Liquify (Filter menu, Shift+Cmd+X): modal workspace with Forward warp,
      Reconstruct, Smooth, Twirl, Pucker, Bloat; advected displacement field,
      per-stroke undo, mesh view; bakes 12 MP in ~20 ms as one undo step
- [x] Quick Selection (the Wand's sibling: Shift+W or the Wand bar's switch):
      paint and the selection grows to edges — geodesic segmentation with a
      stroke colour model and colour-line edge refinement (core::quick_select,
      ~0.2 s per stroke); New turns into Add after the first stroke, Alt
      subtracts, [ ] resize, one undo step per stroke
- [x] Crop tool (C): whole-canvas frame (or the selection), 8 handles, move,
      straighten by dragging outside (fits inside the canvas), ratio presets +
      custom W:H + swap, Delete cropped pixels (off by default), checkerboard
      where the crop extends the canvas, Enter/Esc; one CropCanvas undo step
      (whole-pixel exact; bilinear when turned; text stays upright) (ADR 0007)
- [x] Rulers (Cmd+R), guides (drag from a ruler, move with V, drop on a ruler to
      delete; Show/Lock/Clear, New guide…; saved in .lumen; undoable), grid
      (Cmd+', spacing/subdivisions in Preferences), snapping (Shift+Cmd+;) to
      guides, grid, canvas edges/centre and layer bounds for Move, marquees,
      crop and free-transform moves
- [x] Duplicate layer (any kind, groups deep-copied; Cmd+J without a
      selection), Merge down / Merge group / Merge clipping mask (Cmd+E, blocked
      with reasons), Merge visible (Shift+Cmd+E), Flatten image, Stamp visible
      (Shift+Alt+Cmd+E); merged pixels come from the reference compositor and
      equal the composite (tested)
- [x] Layer locks (transparency, pixels, position, all; group locks cover
      children), enforced by the editor on every command; Layers-header
      toggles, row padlocks, Layer ▸ Lock; saved in .lumen; PSD lspf
      round-trip, psd-tools-verified
- [x] Align (6 edges, to each other or to the selection or canvas) and
      distribute (3+ layers) as one undo step; Move tool bar and Layer ▸
      Align / Distribute
- [x] Color Lookup adjustment (3D LUTs, ADR 0019): .cube (1D/3D/both) and
      .3dl, tetrahedral on gamma values, seven built-in looks generated in
      code, Load 3D LUT, tables embedded once per .lumen; PSD clrL both ways
      with Photoshop's ICC device link (psd-tools and littleCMS checked);
      File ▸ Export ▸ Color Lookup Table bakes the visible adjustment layers.
      Still open: ICC-profile lookups, .look files, GPU path
- [x] Curves per channel (Master/Red/Green/Blue in the editor; PSD both
      ways incl. channel-only curves and the 'Crv ' section) and
      Hue/Saturation Colorize (PSD both ways); Photo Filter colours in
      Lab/HSB/CMYK/Gray from PSD
- [x] Adjustments: Gradient Map, Channel Mixer, Photo Filter, Selective Color
      (gamma-domain; GPU falls back to CPU for them); PSD grdm/mixr/phfl/selc
      round-trip, psd-tools-verified
- [x] Fill layers: Solid Color and Gradient (linear/radial/angle/reflected/
      diamond, angle, scale, reverse, offset) compositing like pixel layers from
      a derived canvas cache; Layer ▸ New fill layer; PSD SoCo/GdFl round-trip
      (ADR 0008)
- [x] Gradient tool: multi-stop gradients, Linear/Radial/Angle/Reflected/
      Diamond, Reverse, Dither (deterministic, against 8-bit banding),
      Transparency, opacity and blend mode, blended by the selection, locks
      respected; paints layer masks; a Fill-layer mode makes an editable
      gradient fill layer from the drag. Options bar: gradient swatch →
      popover with presets (foreground→background/transparent, 10
      built-ins, user presets in prefs) and the stop editor; style icons;
      live preview with guide line, Shift 45°, Esc cancels. Still open:
      screen-resolution preview for huge documents, editing after release
- [x] Gradient stop editor (click to add, drag to move, drag off to remove,
      per-stop colour picker and opacity, presets), shared by Gradient Map and
      gradient fills
- [x] Saved selections (Photoshop's alpha channels): Select ▸ Save selection /
      Load selection (New/Add/Subtract/Intersect, Invert, Delete), undoable,
      saved in .lumen (channels/ tiles) and PSD (named alpha channels after
      RGB + transparency, resources 1006/1045; psd-tools-verified both
      depths)
- [x] Dock tabs Layers | Channels | Paths (persisted) and a Window menu.
      Channels panel: RGB/R/G/B with thumbnails, view one channel as
      grayscale (Cmd+2..5), alpha channels (view, red overlay, Cmd-click
      load with Shift/Alt, rename, save, delete), load a colour channel or
      luminosity as a selection (luminosity masks). Paths panel: work path
      and saved paths with thumbnails, show on canvas, fill/stroke/select
      any path in one undo step, make work path from selection (traced,
      straight segments), save/duplicate/rename/delete. Navigator (drag to
      pan, zoom slider and field) and Info (RGB/HSB, X/Y, exact selection
      and document size) float over the canvas. Still open: Pen editing of
      saved paths, curve fitting when tracing, painting into one channel
- [x] Edit ▸ Cut / Copy / Copy merged / Paste / Paste in place (Cmd+X/C/
      Shift+Cmd+C/V/Shift+Cmd+V): the selection's pixels (soft edges kept) or
      the layer, pasted as a new layer above the active one in place; copies
      also go to the system clipboard as an image and images from other apps
      paste centred (arboard). macOS catches Cmd+V with an image-only
      clipboard via a key monitor; elsewhere use Edit ▸ Paste for that.
      Not exercised live tonight (would overwrite the user's clipboard)
- [x] Image ▸ Trim (transparent or top-left-colour borders), Reveal all (grow
      the canvas to every layer's pixels, e.g. after a non-destructive crop),
      Rotate by angle (canvas grows to fit, transparent corners) — all through
      CropCanvas, so masks, guides, paths and smart objects follow. Text
      layers stay upright when the canvas turns (as in a straightened crop)
- [x] Everyday edits: Layer via Cut (Shift+Cmd+J), Reselect (Shift+Cmd+D),
      Edit ▸ Stroke (inside/centre/outside, width, opacity, foreground
      colour), Cmd-click a layer thumbnail to load its pixels as the
      selection (Shift adds, Alt subtracts, both intersect), Bring to Front /
      Send to Back (Shift+Cmd+] / [), Image ▸ Duplicate; arrow keys nudge
      the layer (Move tool) or the selection outline (selection tools) by 1
      px, Shift by 10, a burst as one undo step; Alt-drag with the Move tool
      moves a copy; Alt-click with the Brush picks a colour; Shift-click
      paints a straight line from the last stroke; Alt+Backspace /
      Cmd+Backspace fill with the foreground / background colour
- [x] Merge selected layers (Cmd+E with several layers selected, "Merge
      layers"): the selected visible siblings composite into the topmost
      one's slot and name; hidden ones stay; picture unchanged (tested)
- [x] Lock-aware Properties transform controls (dimmed with the reason)
- [x] Shape layers + Shape tool (U): rectangle, rounded rectangle, ellipse,
      polygon, line with arrowheads, star/arrow/heart/speech bubble; fill
      none/solid/gradient, stroke colour/width/inside-centre-outside/dashes;
      live drag preview, Shift/Alt, snapping; Properties edits; transforms stay
      vector; Rasterize, Make work path, New shape from path; .lumen; PSD as
      Photoshop shape layers (SoCo/GdFl + vstk + vmsk) both ways,
      psd-tools-verified; ORA bakes (ADR 0010)
- [x] Patterns (ADR 0020): pattern fill layers and pattern-filled shapes
      (scale, angle, phase from the canvas origin), Pattern Overlay effect,
      Edit ▸ Define Pattern, a picker with 8 generated built-ins plus a user
      library next to prefs, Photoshop .pat import; PSD Patt/Pat2/Pat3
      read, PtFl and patternFill both ways (phase recovered from stored
      pixels), psd-tools-verified (ag-psd pattern 13.2 → 0.9). Still open:
      Pattern Stamp, pattern stroke effect, "Link with Layer" following moves
- [~] Text: searchable font picker (system fonts via fontdb, .ttc face index
      honoured), bold/italic (real faces, else synthetic), tracking;
      missing fonts render in DejaVu Sans with a notice. On-canvas editing:
      caret, mouse/keyboard selection (word/line jumps, multi-click),
      clipboard, in-session undo, Esc/Cmd+Enter/click-outside commit as one
      step, Cancel reverts, empty new text leaves no layer. Paragraph text:
      drag a box, word wrap, left/centre/right/justify, resize handles,
      overflow marker, Point⇄Paragraph. Character options: leading,
      baseline shift, all caps, metrics kerning, underline, strikethrough;
      per-character colour/size/bold/italic/underline/strike (ADR 0013).
      PSD text layers are editable type both ways (TySh + EngineData:
      text, PostScript fonts, size, colour, tracking, leading, shift, caps,
      kerning, underline/strike, faux styles, alignment, style runs,
      point/box; ADR 0017); rotated/warped/on-path type keeps Photoshop's
      pixels with a warning; psd-tools-verified, not yet opened in
      Photoshop itself. Still open: IME pre-edit display, per-character
      fonts, rotated text, paragraph spacing/indents
- [x] Animated marching ants (boundary dashes march; huge outlines fall back
      to the static texture)
- [x] Select by colour range (Select menu + palette: fuzziness slider with live
      marching-ants preview, soft graded edges, cancel undoes)
- [x] Select and Mask (Select menu, Option+Cmd+R, palette): full-window
      workspace — overlay / on black / on white / black & white / marching
      ants and Show Edge, zoom and pan; Radius, Smart Radius, Refine Edge
      brush, Smooth, Feather, Contrast, Shift Edge, Decontaminate Colours;
      output to selection / layer mask / new layer / new layer with mask,
      one undo step each. Engine core::refine: local colour-cluster matting
      in linear light + guided filter (1800×1205 in ~0.1 s). Still open:
      quick-select/lasso inside, refining an existing mask, a preview
      worker thread, closed-form matting for hair
- [x] Smart filters (ADR 0011): a per-layer filter stack (enable, opacity,
      blend, one filter mask from the selection) on pixel, smart, text, fill
      and shape layers; the Filter menu adds them on smart objects, Filter ▸
      Convert for smart filters; Layers sub-rows and a Properties section;
      tile-exact incremental cache; .lumen round trip; PSD/ORA bake with a
      warning; GPU falls back to CPU. Still open: painting the filter mask,
      PSD smart filters, faster drags on huge layers
- [x] Quick-mask mode (Q): paint the selection under the classic red overlay —
      white selects, black deselects, live while stroking
- [ ] AI tools (local ONNX): subject select, upscaling — see project overview
      (object removal without AI exists: Content-Aware Fill)

## 6. Usability

- [x] Multiple open documents: tab strip in the top bar (switch, close with
      per-tab unsaved confirm, + for new; opening an already-open file
      focuses its tab; per-document zoom/pan/active layer; quitting checks
      every tab). Autosave backs up every unsaved tab (one slot each,
      stale slots dropped) and crash recovery reopens them all
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

- [x] `lumenply batch` (Photoshop's Image Processor): images, camera RAW
      (camera tone curve, `--auto` exposure/whites/blacks), PSD and projects
      to PNG / JPEG / WebP with `--resize 50%|2048|1920x1080` (Lanczos, never
      enlarging a fit), per-file report, non-zero exit when any file fails

- [ ] Scripting: Python via PyO3 on the command API; macro recording from history
- [ ] Sandboxed WASM plugins (wasmtime)
- [ ] Browser build (WebAssembly + WebGPU) — engine crates are UI-free by design
- [~] Packaging: `scripts/bundle-macos.sh` builds Lumenply.app (release
      binary, .icns rendered by the app's own `--write-icon` on Apple's icon
      grid, Info.plist with file types: .lumen/.nge owner, PSD/PSB, ORA,
      images, camera RAW). Files opened from Finder (double-click, Open With,
      Dock drop) arrive through an `application:openURLs:` method added at
      launch (macos_open.rs), cold launch verified with `open -a`. Still
      open: code signing + notarisation, Windows installer, Flatpak,
      nightly CI builds
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

- Pen pressure works on macOS and Windows only (no Linux source yet), untested
  on a real tablet.
- Gradient midpoints/smoothness are not modelled; Selective Color approximates
  Photoshop's undocumented maths; fill layers can't be scaled or rotated
  without rasterizing; shapes re-import from PSD as path shapes (radius/sides
  not editable after); centre/outside strokes have round joins.
- Dust & Scratches radius is capped at 8 (per-pixel median).

- Text layers cannot be scaled or rotated without rasterizing.
- egui menus cannot scroll; the quick-add "More..." list is tall (~500 px).

