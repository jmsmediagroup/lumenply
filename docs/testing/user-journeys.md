# User-journey test plan

Every feature is tested the way a regular user would use it: through the
menus, panels, tools and dialogs on screen, found by the names the UI shows,
with real mouse and keyboard input, recorded as a video. Each journey states
what the user is trying to do, what they should see, and what must be true
afterwards. The harness that runs and records them is described in
[harness.md](harness.md).

## What every journey is judged on

Correctness is only half of it. A journey passes when:

1. **It works**: the document ends up exactly as intended (checked on the
   document, not only by eye), and undo returns to the state before, one
   step per user action.
2. **It's findable**: a Photoshop user finds the command where they'd look
   first (menu, panel, tool options, right-click), or in the command palette
   (Cmd+K) by the word they'd type.
3. **It's understandable**: labels say what happens; units are shown;
   defaults are sensible; nothing needs explaining.
4. **It gives feedback**: the user sees that something happened (canvas,
   status bar, history entry); slow work shows progress and can be
   cancelled; errors say what went wrong and what to do.
5. **It's forgiving**: Esc cancels, Enter confirms, undo works, nothing is
   lost without a warning, a disabled command says why.
6. **It's consistent**: the same modifier means the same thing everywhere
   (Shift adds, Alt subtracts or copies, Cmd temporarily moves), dialogs look
   and behave alike, the same thing has the same name everywhere.
7. **It fits**: works at 1440×900 and at the 900×600 minimum, without
   clipped or overlapping controls.
8. **It feels fast**: no visible stall for everyday actions; anything longer
   than about a second shows progress.

Findings are recorded with a severity: **blocker** (feature broken or data
lost), **major** (works, but a user would likely fail or be misled),
**minor** (friction, unclear wording, inconsistency), **polish**.

## Areas and journeys

### A. Documents and files
- Start from the welcome screen; create a new document from a preset (A4 at
  300 ppi, HD) and a custom size; check the size, resolution and background.
- Open a PNG, a JPEG, a PSD with layers, a camera RAW, a HEIC, a format-1
  and a format-3 `.lumen`; place an image as a layer; drag a file onto the
  window.
- Save, Save As, reopen from Open Recent; edit and close with unsaved
  changes (warning); quit with several tabs, some unsaved.
- Autosave, then simulate a crash; on relaunch recover every document.
- Work with several tabs: switch, close, duplicate a document.
- Image Size (pixels, percent, cm/inches, resample on and off), Canvas Size
  with an anchor, Trim, Reveal All, rotate and flip the image.
- Export: PNG, JPEG (quality), Export As (formats, size, preview, file
  size), PSD (8 and 16 bit), OpenRaster, 16-bit PNG/TIFF, EXR, PDF, a 3D LUT.

### B. Layers
- Add, name, rename (double-click), delete, duplicate, group and ungroup,
  reorder by dragging, Bring to Front and Send to Back, move up and down.
- Visibility, Alt-click to show one layer alone, opacity, fill, blend modes.
- Locks: transparency, pixels, position, all; what a locked layer refuses
  and how it says so.
- Masks: from a selection, painting on the mask, disable, remove, apply.
- Clipping masks; merge down, merge visible, merge selected, flatten, stamp
  visible; Layer via Copy and Layer via Cut.
- Fill layers (solid, gradient, pattern) and editing them in Properties.
- Smart objects: convert, transform repeatedly, edit contents in a tab and
  save back, replace contents, rasterize.
- Align and distribute; the layers filter; the context menu; thumbnails.

### C. Selections
- Rectangle and ellipse marquees with Shift (add or constrain), Alt
  (subtract or from centre), feather; lasso and polygonal lasso.
- Magic Wand (tolerance, contiguous, all layers), Quick Selection, Object
  Selection (AI).
- Select All, Deselect, Reselect, Invert; Colour Range; Modify (Expand,
  Contract, Border, Smooth, Feather); Grow, Similar.
- Save and load selections (alpha channels); Quick Mask; Select and Mask
  (views, refine edge brush, output to a mask or a new layer); Select
  Subject; Load layer pixels as a selection; the selection action bar.

### D. Painting and retouching
- Brush: size, hardness, opacity, scatter, presets, the Brush settings
  panel, sampled tips, importing an `.abr`; Shift-click straight lines;
  bracket keys; Alt-click to pick a colour.
- Eraser and its Background and Magic modes; Paint Bucket; Gradient tool
  (presets, styles, stop editor, fill-layer mode); Eyedropper; the colour
  picker; swapping foreground and background.
- Clone Stamp; Healing (Spot, Healing, Patch, Content-Aware Move, Red Eye);
  Blur, Sharpen, History Brush; Dodge, Burn, Sponge, Smudge.
- Fill (and Alt/Cmd+Backspace), Stroke selection, Content-Aware Fill,
  Define Pattern, Define brush tip.

### E. Moving, transforming and the canvas
- Move tool: drag, arrow-key nudges, Alt-drag to copy, Smart Guides and
  snapping, auto-selecting a layer.
- Free Transform: scale (Shift for free aspect), rotate, skew, perspective,
  warp, typed values, Enter and Esc.
- Crop tool (ratios, straighten, delete cropped pixels), Perspective Crop.
- Liquify, Puppet Warp, Content-Aware Scale.
- Rulers, guides (drag from the ruler, move, delete), grid, snapping.

### F. Adjustments, filters and effects
- Every adjustment layer, added from the Properties quick-add and from the
  Layer menu, edited in Properties (curve points, levels sliders, presets),
  masked, hidden, deleted.
- Image ▸ Adjustments applied to pixels, including Shadows/Highlights,
  Replace Color, Match Color, Equalize, Desaturate, Auto Tone/Contrast/Color.
- Filters: each group, destructive and as live filter layers; smart filters
  (convert, stack, reorder, mask, edit); Camera Raw Filter; Color Lookup
  with built-in and loaded LUTs.
- Layer styles: each effect, edited in Properties, on several layers.

### G. Text, shapes and paths
- Text: click to type, paragraph boxes, selection and editing, fonts,
  character styles per letter, alignment, commit and cancel, editing again.
- Shapes: every kind, fill and stroke options, editing in Properties,
  transforming, rasterizing.
- Pen and paths: draw, edit anchors and handles, save, fill, stroke, make a
  selection, make a work path from a selection; the Paths panel.

### H. Viewing and the workspace
- Zoom (keys, Zoom mode, fit, 100%, print size), pan (space, Hand),
  Navigator, Info, Histogram.
- Channels panel (single channels, alpha channels), History (jump,
  snapshots, History Brush source), Proof Colors and Gamut Warning.
- Command palette, keyboard shortcuts dialog, Preferences (every setting),
  window sizes down to 900×600.

### I. AI and automation
- Object Selection, Select Subject and Remove Background, from first use
  (consent, download, progress, cancel) to results on real photos.
- Actions: record a sequence, play it on another document, rename, delete,
  built-in actions, what isn't recordable.
- A PSD round trip as a user: open, edit text and layers, export, reopen.

### J. Undo, history and robustness
- Undo and redo after every kind of edit; slider drags as one step; the
  history limit; undo after save and reopen (format 3).
- Large documents (6000×4000, 30 layers): opening, painting, transforming,
  saving; how responsive it feels.
- Long sessions: many edits, many undos; memory use stays bounded.
