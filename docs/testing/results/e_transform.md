# E. Moving, transforming and the canvas: user-journey results

Tested on 2026-10-03 with the `uitest` harness, real mouse and keyboard
input only, at 1440×900 and at 900×600. Scenarios:
`crates/app/src/uitest/scenarios/e_transform.rs` (16 journeys). Fifteen open
`shapes.ora`, a layered file written into the session's files: a white
800 × 600 Background, a red 200 × 100 block “Red” at (100, 100) and a blue
100 × 100 square “Blue” at (500, 300), so every check has exact expected
bounds, pixels, canvas sizes or guide positions; each one checks undo.
`big-document` opens a 4000 × 3000 file with a noisy full-canvas Background
and a 1600 × 1200 “Photo” layer and measures frame times.

Recordings (each folder has `session.mp4`, `session.json`, `keyframes/`):

- before: `<recordings>/e_transform/before/<scenario>/` and
  `<recordings>/e_transform/before-900x600/<scenario>/`
- after: `<recordings>/e_transform/after/<scenario>/` and
  `<recordings>/e_transform/after-900x600/<scenario>/`

## Journeys

| Journey | Result | After video (1440×900; same name under `after-900x600/`) |
| --- | --- | --- |
| `move-drag-and-nudge`: pick Red and the Move tool, drag (+60, +40), three Right nudges and Shift+Down, undo the nudges (one step), undo the drag | fixed (1) | `after/move-drag-and-nudge/session.mp4` |
| `move-alt-drag-copy`: V, Alt-drag Blue 150 px left: “Blue copy” above, original stays, copy active, one undo removes it | fixed (3) | `after/move-alt-drag-copy/session.mp4` |
| `move-auto-select`: with Red active, Cmd-drag Blue moves Blue; tick Auto-Select, a plain drag on Red moves Red, a click on white picks the Background | fixed (2) | `after/move-auto-select/session.mp4` |
| `move-smart-guides`: drag Red's left edge to 2 px short of Blue's: snaps to x 500 (magenta guide and ΔX/ΔY pill in the video); the same drag with Cmd lands at 498; Smart Guides off from the View menu | pass | `after/move-smart-guides/session.mp4` |
| `transform-scale`: Edit ▸ Free transform, corner drag scales 1.5× from the opposite corner, Enter; Cmd+T, Shift-drag widens only; edge drag then Esc changes nothing and adds no step; two undos | fixed (4, 8) | `after/transform-scale/session.mp4` |
| `transform-typed`: Free transform from the Move options bar; Properties' transform controls are greyed out; W 50 %, Rotate 90°, Apply: a 100 × 100 square about the old centre; undo; Cmd+T, Skew 20°, Enter: 36 px of lean | fixed (5, 9) | `after/transform-typed/session.mp4` |
| `transform-rotate`: drag outside the box a quarter turn (90°); Esc; Shift-drag about 40° snaps to 45°; Enter; pixels checked at the turned corners | fixed (7) | `after/transform-rotate/session.mp4` |
| `transform-perspective-warp`: Edit ▸ Perspective, pull a corner out, Enter; undo; Edit ▸ Warp, drag a mesh point up, Enter; undo | pass | `after/transform-perspective-warp/session.mp4` |
| `crop-ratio`: C, ratio menu ▸ 1:1 (600 px centred square), corner drag to a 500 px square anchored bottom-right, Enter: canvas 500 × 500 and Blue moved with it; undo | pass | `after/crop-ratio/session.mp4` |
| `crop-straighten`: Crop tool from the rail, Delete cropped pixels, drag outside the frame 10°: the frame shrinks inside the canvas; Crop (options bar): canvas = frame size, opaque corners; undo | fixed (10, 12) | `after/crop-straighten/session.mp4` |
| `perspective-crop`: Crop ▸ Perspective switch, drag a frame, pull a corner into a keystone, Enter: about 326 × 486; undo | pass (polish 16 open) | `after/perspective-crop/session.mp4` |
| `liquify`: Filter ▸ Liquify…, push the block's edge down on the preview, OK: one history step, red below the old edge; undo | pass | `after/liquify/session.mp4` |
| `puppet-warp`: Edit ▸ Puppet Warp, three pins along the block, drag the right one down 80 px, OK; undo | pass | `after/puppet-warp/session.mp4` |
| `content-aware-scale`: Edit ▸ Content-Aware Scale, W 50 %, Apply: 100 × 100; undo | fixed (13); finding 17 open | `after/content-aware-scale/session.mp4` |
| `rulers-guides-grid`: View ▸ Rulers, drag a guide out of the top ruler to y 250, move it to 320 with the Move tool, drag it onto the ruler (deleted), undo; a guide from the left ruler snaps to Red's edge at 300; a marquee snaps to both guides; grid on, Clear guides, grid and rulers off | fixed (11) | `after/rulers-guides-grid/session.mp4` |
| `big-document`: 4000 × 3000, Move drags of the 1600 × 1200 layer in 20 and 60 frames, Free Transform corner drags in 20 and 60 frames, Enter, undo | fixed (6); finding 18 open | `after/big-document/session.mp4` (1440×900 only) |

All 16 pass at 1440×900 and the 15 sized journeys pass at 900×600 after the
fixes. Before them, 9 of 16 failed at 1440×900 and 8 of the 14 run at
900×600 (`move-auto-select` was added to that set afterwards). Two
of those failures were the scenario's own fault and were fixed in the
scenario: an edge tolerance of 1 px in `transform-perspective-warp`, and at
900×600 the Crop/Perspective switch labelled “Persp.”.

## Findings

Fix commit for every fixed finding: `e0b3692`.

| # | Severity | Finding (step) | Status |
| --- | --- | --- | --- |
| 1 | major | Shift+arrow nudged the layer by 1 px, not 10 (`move-drag-and-nudge` step 17: (163, 141) instead of (163, 150)). egui's `consume_key(NONE, …)` ignores an extra Shift, so the plain-arrow match ate Shift+arrow first. The selection nudge had the same bug. Shift is now matched first; test in `everyday_ui::nudge_tests`. | fixed |
| 2 | major | The Move tool had no Auto-Select and Cmd-click did nothing: the only way to move another layer was to find it in the Layers panel (`move-auto-select`: Blue stayed, Red moved instead). Now Auto-Select sits in the Move options bar, and Cmd (Ctrl) at the press picks the topmost visible layer with pixels under the pointer (or skips that with the box ticked), as in Photoshop. Groups are looked into, hidden layers and masked-out pixels skipped, adjustments never picked (`move_tool.rs`). | fixed |
| 3 | minor | An Alt-drag copy took two undo steps (“Duplicate layer”, then “Move”); one Cmd+Z left a copy sitting on the original. Now the duplicate and the move are one step. | fixed |
| 4 | major | Free Transform scaled about the box centre from every handle: dragging the bottom-right corner moved the top-left one too (`transform-scale`: (−1, 49, 402, 202) instead of (100, 100, 300, 150); `big-document` likewise). Photoshop keeps the opposite corner or edge in place and scales about the centre with Alt. Now it does, also when the box is turned; corners keep proportions, Shift frees them (`Xform::drag_scale`). Also reported by the area G tester. | fixed |
| 5 | major | Properties' TRANSFORM section (Scale, Rotate, Apply, Flip, Free transform…) stayed live during Free Transform. Its Rotate and Apply share names and look with the options bar's, and they transformed the layer underneath the live transform: typing W 50 % then Rotate 90° in Properties and clicking Apply gave a 100 × 200 block, with the transform's width lost (`transform-typed` before). Now the section is greyed out with “Free Transform is open: set its values in the options bar”. | fixed |
| 6 | major | With the Move tool, every frame scanned every pixel of the active layer about seven times: each align button's availability check called `content_bounds`, and Properties computed the layer's centre on every frame for any tool. With a 4000 × 3000 layer active, frames took 60–200 ms while just hovering (10 fps; the whole UI lagged), and a Move drag cost 99 ms a frame. The align check now stops at the first painted tile (`align::has_content`) and the centre is found only when Apply is pressed: idle frames are under 1 ms and a Move drag 37–47 ms. | fixed |
| 7 | minor | Shift while rotating did not snap to 15° steps (`transform-rotate`: 40.05°). | fixed |
| 8 | minor | The transform box drew only its four corner handles; the edge midpoints that stretch one side were invisible (they worked when found by accident). All eight are drawn now, the edge ones slightly smaller. | fixed |
| 9 | minor | At 900×600 the Free Transform bar overflowed: Cancel was clipped and the bar scrolled sideways. The narrow bar now drops its title and the buttons' key hints (kept in their tooltips) and narrows the fields; everything fits. | fixed |
| 10 | minor | A straightened crop with Delete cropped pixels left a part-transparent corner pixel (alpha 179 at (0, 0)): the frame was fitted exactly onto the turned canvas edge. It now keeps a pixel clear of it. | fixed |
| 11 | minor | The grid was invisible over white paper (light grey at 38 % opacity) and only showed over coloured pixels. It is a mid grey now: 180 over white, 75 over black. | fixed |
| 12 | minor | The Crop tool on the rail and the Crop button in its options bar had the same accessible name; a screen reader (and the scenario) could not tell them apart, and clicking “Crop” picked the tool (`crop-straighten` before). The button is now “Crop to frame” (in perspective mode too). | fixed |
| 13 | minor | Content-Aware Scale committed with “Commit”, Free Transform with “Apply”. Both say Apply now. | fixed |
| 14 | polish | Perspective crop's greyed-out Crop button said nothing about why. Now: “Drag a frame over the image first”. | fixed |
| 15 | polish | The Move tool's hint said only “Drag to move the active layer”. It now mentions Alt-drag copies and Shift+arrows; the Auto-Select tooltip explains Cmd-click. | fixed |
| 16 | polish | The Crop / Perspective switch reads “Persp.” whenever the crop bar is narrow, which is already the case at 1440×900, and that is also its spoken name. Naming it needs a change to the shared `theme::segmented`. | open |
| 17 | minor | Content-Aware Scale's typed W scales from the left edge; Free Transform's typed W scales about the centre (Photoshop: both about the reference point, the centre by default). | open (proposal 4) |
| 18 | major | Free Transform drags on a 4000 × 3000 document run at about 76 ms a frame (13 fps): every frame resamples the whole layer at full resolution, composites it and converts about 2 Mpx to a texture, although the canvas shows it at 22 %. | open (proposal 1) |
| 19 | minor | Straightening turns the crop frame over a level image; Photoshop turns the image under a level frame (so horizons line up with the frame's grid) and has a Straighten tool (draw along the horizon). | open (proposal 2) |
| 20 | minor | Free Transform has no drag skew (Cmd+Shift on an edge) or distort (Cmd on a corner), the arrow keys don't nudge the box while it is open, and Cmd+Z can't step back inside it. | open (proposal 3) |
| 21 | polish | Liquify shows the layer alone over a checkerboard while Puppet Warp shows the layers below it, which makes lining a warp up with the rest of the picture hard in Liquify. | open (proposal 5) |

What worked well: the live Move preview with a ΔX/ΔY pill and magenta
Smart Guides, Cmd turning snapping off, guides out of the rulers with
snapping to layer edges and deletion by dropping on a ruler, the crop
ratios and the anchored crop corners, Perspective Crop, Warp, and the
Liquify and Puppet Warp workspaces (each one undo step, Esc cancels).

## Proposals (not done)

1. **Screen-resolution transform previews.** While dragging, resample the
   layer at the canvas's zoom (a 1600 × 1200 layer is 350 × 260 on screen at
   22 %) and render the full-resolution result only on Enter, or tile-limit
   the preview to the visible area. Changes how previews are rendered, so
   it needs a design and a GPU-path discussion; not a tester's fix.
2. **Straighten like Photoshop.** Keep the crop frame level and turn the
   image under it during a straighten drag, and add a Straighten mode (draw
   a line along the horizon). A large UX change to the Crop tool.
3. **Free Transform modifiers and keys.** Cmd-drag a corner to distort (the
   Perspective mode's free corners), Cmd+Shift-drag an edge to skew, arrow
   keys to nudge the box, and Cmd+Z to undo the last handle change inside
   the transform.
4. **One reference point.** Give Free Transform and Content-Aware Scale
   Photoshop's reference-point grid, centre by default, and use it for typed
   values and Alt-drags in both.
5. **Liquify backdrop.** Show the layers below (as Puppet Warp does), with
   a “Show backdrop” toggle and opacity.

Suggested ROADMAP lines (not edited here):

- [x] Move tool Auto-Select and Cmd-click layer picking; Alt-drag copy as one undo step
- [x] Free Transform anchors the opposite corner/edge (Alt: centre), Shift-rotate 15°, eight handles
- [ ] Screen-resolution previews for Free Transform drags on large documents
- [ ] Crop straighten: level frame over a turning image, Straighten tool
- [ ] Free Transform distort/skew drags, arrow nudges and undo inside the transform

## Frame times

Measured with `--no-video` (no offscreen rendering or encoding), on an
M-series Mac shared with nine other test runs, so absolute numbers vary by
about ±20 %. A drag's cost per frame is the difference between the same
drag in 60 and in 20 frames, over 40.

| 4000 × 3000 document | Before | After |
| --- | --- | --- |
| Idle frame, Move tool, 4000 × 3000 Background active (frame probe) | 64–206 ms | < 1 ms |
| Idle frame, Brush tool, same layer active (frame probe) | 8–114 ms | < 1 ms |
| Move drag of the 1600 × 1200 layer, per frame | 99 ms | 37–47 ms |
| Free Transform corner drag, per frame | 76 ms | 76–82 ms (finding 18) |
| Enter (apply the transform) | 0.7–0.9 s | 0.17–0.36 s |
| Undo of the transform | 0.3–0.7 s | 0.09–0.16 s |

On the 800 × 600 file every drag, nudge and transform previews at the
harness's 30 fps without a visible stall. The preview pipeline for a Move
frame on the large document (move the tiles, composite, flatten, convert)
measures about 15–20 ms; the rest of a drag frame is converting the patch
to a texture and the UI.

## Not tested, and why

- View ▸ New guide… and Lock guides: not driven (the guide drags cover the
  guide commands; lock was read in code: the Move tool leaves locked guides
  alone).
- Transforming text, shapes and smart objects (areas G and B), and moving
  layers inside groups by dragging on the canvas.
- Liquify's other brushes (Twirl, Pucker, Bloat, Reconstruct, Smooth) and
  Puppet Warp's modes, depth and pin removal: opened and seen in the
  workspace, not checked numerically.
- Real-window input: the harness drives the egui UI headlessly; pen
  pressure and the macOS window are out of its reach.
