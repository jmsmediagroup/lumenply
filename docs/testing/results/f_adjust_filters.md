# F. Adjustments, filters and effects: user-journey results

Tested on 2026-10-03 with the `uitest` harness, real mouse and keyboard
input only, at 1440×900 and at 900×600. Scenarios:
`crates/app/src/uitest/scenarios/f_adjust_filters.rs` (12 journeys and a
timing session). Almost every journey opens `patches.png`, a 600 × 400 test
card written by the scenario: six flat greys along the top (0, 64, 128,
192, 255, 96; 100 px wide) and six colours along the bottom (pure red,
green and blue, (200, 40, 40), (230, 140, 30), (40, 160, 200)). Each check
is a known patch through a known adjustment with an explicit expected
value (for example Levels 64/192: 64 → 0, 128 → 128, 192 → 255; Exposure
+1 EV: 128 → 176; Hue +120°: red → green; Posterize 4: 96 → 85; Mosaic's
cell across the black | 64 edge → 55), and every journey checks undo.
Live previews are checked on the rendered canvas, not only in the
document.

Recordings (each folder has `session.mp4`, `session.json`, `keyframes/`):

- before (the scenarios on main, without the fixes): `<recordings>/f_adjust/before/<scenario>/`
- after: `<recordings>/f_adjust/after/<scenario>/`
- 900×600 runs of the same scenarios (after the fixes): `<recordings>/f_adjust/dev900/<scenario>/`
- timings without video: `<recordings>/f_adjust/timing/fx-big-document/`

## Journeys

| Journey | Result | After video |
| --- | --- | --- |
| `fx-quick-add-chips`: Levels (typed points, undo, redo), Exposure, Hue/Sat (typed hue, Lightness drag undone in one step), B & W, Curves preset, the Blur chip; each checked on the card and deleted with the bin | pass | `after/fx-quick-add-chips/session.mp4` |
| `fx-layer-menu-adjustments`: Brightness/Contrast, Vibrance, Color Balance, Threshold, Posterize, Invert, Gradient Map (Reverse), Channel Mixer (preset), Photo Filter, Selective Color from Layer ▸ New adjustment layer, each edited in Properties and checked; undo of a delete | fixed (9) | `after/fx-layer-menu-adjustments/session.mp4` |
| `fx-curves`: the demo's Lift shadows curve and presets fully in view at 1440×900; press on the line and drag (one step, undo, redo); drag a point; right-click removes; drag a point off the graph removes it; the Red channel; Lighten keeps the red curve | fixed (1, 2, 4) | `after/fx-curves/session.mp4` |
| `fx-adjustment-mask`: marquee the left half, add Invert from More…: masked to the selection in one step; hide and show with the eye; Layer ▸ Disable/Enable mask; Layer ▸ Delete layer and undo; a live blur over the right half is masked too | fixed (3) | `after/fx-adjustment-mask/session.mp4` |
| `fx-image-adjustments`: four dialogs opened and cancelled in a row, each taking the mouse; Levels (Cmd+L) live preview on the canvas, Preview off/on, Cancel leaves no step, OK is one step, undo; Hue/Sat (Cmd+U) with Enter; Curves (Cmd+M) with Esc; Color Balance (Cmd+B); Invert (Cmd+I); Desaturate (Shift+Cmd+U); Replace Color sampled on the canvas and Shift-clicked in its thumbnail, the dialog dragged aside; Shadows/Highlights; Match Color; Equalize; Auto contrast, Auto color and Auto tone on a dull blue-tinged card | fixed (6, 7, 8, 18) | `after/fx-image-adjustments/session.mp4` |
| `fx-filters`: Gaussian Blur preview on the canvas, Cancel (no step, canvas restored), Apply (one step named for the filter), undo, reopens at the radius last applied; Add Noise, Mosaic, Sharpen, Find Edges, Emboss, High Pass applied and checked; Box, Motion, Surface, Lens Blur, Median, Dust & Scratches opened and cancelled | fixed (5) | `after/fx-filters/session.mp4` |
| `fx-live-filters`: Filter ▸ Live filter layer ▸ Motion Blur, Distance drag is one step and undoes, eye, delete; Layer ▸ New live filter layer ▸ High Pass, radius typed, opacity 0% | pass (finding 11) | `after/fx-live-filters/session.mp4` |
| `fx-smart-filters`: Filter ▸ Convert for smart filters (the menu then says filters go on as smart filters, Convert greyed with its reason), Gaussian Blur and Find Edges stacked, reorder and undo, hide one, edit the blur's radius in place, mask to a selection, delete one, master switch off | pass (finding 15) | `after/fx-smart-filters/session.mp4` |
| `fx-camera-raw`: Shift+Cmd+A, Exposure +1 EV, Cancel leaves nothing; from the Filter menu, OK is one step; as a live filter layer at −1 EV, reopened from Properties | pass | `after/fx-camera-raw/session.mp4` |
| `fx-color-lookup`: Color Lookup with no table changes nothing; the Monochrome Contrast look; undo; Load 3D LUT… an inverting 2×2×2 `.cube` (64 → 191, red → cyan); a broken `.cube` says why and keeps the table; Layer ▸ New adjustment layer ▸ Load 3D LUT… adds a second one | pass | `after/fx-color-lookup/session.mp4` |
| `fx-layer-styles`: on a filled rectangle: Color overlay (exact colour, “EFFECTS · 1 ON”, undo), Stroke (3 px, then 10 px), Drop shadow, Outer glow, Inner shadow, Inner glow, Bevel, Gradient overlay, Pattern overlay each on and off; a second layer gets its own overlay; the histogram is there | fixed (10) | `after/fx-layer-styles/session.mp4` |
| `fx-big-document`: a 4000 × 3000 photo through Curves, Hue/Sat, a live blur, the Levels dialog and the Gaussian Blur dialog | pass (finding 12, timings below) | `after/fx-big-document/session.mp4` |

All 12 pass at 1440×900 after the fixes. Before them, 6 of the 11 recorded
journeys failed (`fx-big-document` was only timed before). At 900×600 the
journeys that edit in Properties stop where a control is scrolled out of
the 60–110 pt Properties section (finding 13), and the Layer menu's
adjustment submenu cannot be used (finding 14).

## Findings

| # | Severity | Finding (step) | Status | Commit |
| --- | --- | --- | --- | --- |
| 1 | major | At 1440×900 with the demo's layers, Properties showed only the top half of the curve; the shadows half and the Linear/Contrast/Lighten/Fade presets needed scrolling (`fx-curves` steps 6–7). Layers now give way (to three rows) until the active layer's settings fit, and if that is not enough the graph shrinks (to 150 pt) to keep its presets in view. Unfolded effects count too. | fixed | `1ee7179` |
| 2 | major | Pressing on the curve line and dragging did nothing; Photoshop adds a point there and drags it in one motion (`fx-curves` step 15). | fixed | `1ee7179` |
| 3 | major | An adjustment or live filter layer added with a selection was not masked to it (the whole card inverted, `fx-adjustment-mask` step 11). Photoshop masks it, and fill layers here already did. One undo step. | fixed | `de53cca` |
| 4 | minor | Dragging a curve point off the graph did not remove it (Photoshop's way; only right-click did). | fixed | `1ee7179` |
| 5 | minor | Filter dialogs reopened at their defaults; Image ▸ Adjustments dialogs and Photoshop remember the last applied settings (`fx-filters`). | fixed | `7036f77` |
| 6 | minor | Replace Color opens over the left of the image and only the canvas took eyedropper clicks, so the red patch under the dialog could not be sampled without moving it. Its thumbnail now samples too (Shift adds, Alt removes), as in Photoshop. | fixed | `5fa6f2e` |
| 7 | minor | Image ▸ Auto tone / Auto contrast / Auto color add a Levels adjustment layer (by design, non-destructive) but it was called “Levels”, so nothing said where it came from. It is now called “Auto Tone” etc., in the same undo step, and the history says so. | fixed | `5fa6f2e` |
| 8 | minor | Cmd+B (Color Balance) and Shift+Cmd+B (Auto Color) did nothing; Photoshop users press them. Both added as rebindable defaults. | fixed | `ec3059f` |
| 9 | minor | Channel Mixer's Preset menu read “Choose…” whatever preset was in use. It now names the preset, or “Custom”. | fixed | `b6b3cfe` |
| 10 | minor | Ticking Stroke drew a white stroke: invisible on white paper (`fx-layer-styles` step 28). New strokes are black, Photoshop's 3 px outside. | fixed | `1ee7179` |
| 11 | minor | High Pass (filter, live, smart) turns flat areas into 188 (50% in linear light), Emboss into 128; Photoshop's neutral for both is 128. 188 is Overlay's neutral in this linear-light compositor (ADR 0015), so it blends correctly here, but it looks wrong to a Photoshop user and the two filters disagree. | proposal 3 | |
| 12 | major | Filter dialogs compute their preview for the whole layer on the UI thread at every change: on 12 MP the Gaussian Blur dialog froze 1.5 s on opening, 2.5 s after typing a radius, and dragging its slider ran at about 5 frames a second, with no progress shown. (The Image ▸ Adjustments dialogs throttle to one preview per 120 ms and stay usable: 0.7 s for a typed value.) | proposal 1 | |
| 13 | major | At 900×600, Properties gets 60–110 pt between the quick-add chips and Layers (three rows): only Opacity of the active layer shows; every adjustment, smart filter and effect needs scrolling a sliver (`dev900/fx-curves` step 4). | proposal 2 | |
| 14 | major | At 900×600, Layer ▸ New adjustment layer / New live filter layer open their submenu clamped to the window and **behind** the two-column Layer menu: only “Load 3D LUT…” shows (`dev900/fx-color-lookup` step 11). Image ▸ Adjustments' right column, the quick-add More… menu and the Layers panel's buttons still work. The menu code is shared (theme.rs, menu.rs); reported to the coordinator. | open | |
| 15 | polish | Properties keeps a smart filter open by its place in the stack, so after undoing a reorder the other filter's settings are open (`fx-smart-filters` step 41). | open | |
| 16 | minor | Posterize goes through a 1024-entry linear-light table, so a value within about a level of a step comes out between two levels: 64 with 4 levels gives 57 instead of 85 (`before/fx-layer-menu-adjustments`, first run). Changes pixel maths. | proposal 4 | |
| 17 | minor | Filter dialogs have no Preview check box and say Apply; the Image ▸ Adjustments dialogs have Preview and say OK, and open at the canvas's top-left while filter dialogs open centred. No Filter ▸ Last Filter (Cmd+F) either. | proposal 5 | |
| 18 | minor | Image ▸ Adjustments dialogs move their backdrop to the top every frame like `dialogs.rs` did (finding C-1); opening four dialogs in a row could not reproduce the lost-mouse bug here, but the window is now kept just above its backdrop the same way. | fixed (defensive) | `5fa6f2e` |
| 19 | polish | Channel curves are drawn in the master's blue, and neither Curves nor Levels shows a histogram behind its controls; Levels is five sliders, not Photoshop's histogram with three input and two output handles. | proposal 6 | |
| 20 | polish | Replace Color says “Colour” under the title “Replace Color” (the app mixes colour/color: Colour range, Color Balance). | open | |
| 21 | polish | Turning Stroke on widens the dock by about 12 pt (its Outside/Center/Inside row). | open | |

### Harness notes (for the harness owner)

- `Session::menu("Layer > New adjustment layer > …")` treats the Layer menu as already open because the Layers panel has a button with the submenu's name, and then aims at that button; the scenarios open such submenus by hand (`open_submenu`).
- `within()` cannot scope by role: `within("Levels")` fails while the Levels dialog and the Levels chip are both on screen. The scenarios use `dlg()`.
- The Properties `ScrollView` is in the accessibility tree only while its content overflows (`a11y_scroll` names it only when the scroll area has a response), so `within("Properties")` fails on short content; and with a scope set, a control scrolled out of view is reported as missing instead of being scrolled to. The scenarios scroll with the wheel themselves (`reveal`).
- Added: `Session::right_click_at(point, what)`; `screen()` is now `pub(crate)` (to read the rendered canvas for preview checks).

## Proposals (not done)

1. **Filter previews off the UI thread, at view resolution** (finding 12). Preview the visible part of the layer at the canvas's zoom in a background job, show “Previewing…” while it runs, and coalesce slider changes like the adjustment dialogs do; Apply runs at full resolution with progress. Not done: it changes how filter previews are computed and needs a job/cancel design.
2. **A dock for 900×600** (finding 13). Options: fold the quick-add chips into one “Add ▾” button below 700 pt of height; let Layers go down to two rows while an adjustment, smart filter or effect is being edited; or put Properties and Layers in tabs on short windows. A layout decision for the whole dock, not only area F.
3. **One neutral grey for High Pass and Emboss** (finding 11): either both 50% linear (Overlay-neutral here) or both 128 with a gamma-domain Overlay option (ADR 0015 leaves “Blend RGB colors using gamma” open). Changes filter output.
4. **Posterize without table interpolation** (finding 16): compute it per pixel (a step function must not be interpolated) or snap the table lookup. Changes pixel maths; a one-line test: Posterize 4 of 64 is 85.
5. **One dialog convention** (finding 17): a Preview check box on filter dialogs, the same primary label (Photoshop's OK) and the same placement for both kinds, and Filter ▸ Last Filter (Ctrl/Cmd+F, reapply with the last settings).
6. **Histograms in Curves and Levels** (finding 19): the layer-below histogram behind the curve and Photoshop's Levels histogram with draggable black, grey and white input handles; channel curves in their colour.

Proposed ROADMAP lines:

- [ ] Filter dialog previews in the background at view resolution (12 MP previews freeze the UI for 1.5–2.5 s).
- [ ] A dock layout for 900×600: Properties gets 60–110 pt today.
- [ ] Layer menu submenus at 900×600 open behind the two-column menu.
- [ ] Filter dialogs: Preview check box, OK, Filter ▸ Last Filter (Cmd+F).
- [ ] Levels/Curves: histograms and Levels handles.
- [x] Curves: press-and-drag adds a point; drag off removes; all of the curve stays in view.
- [x] Adjustment and live filter layers made with a selection are masked to it.

## Timings

4000 × 3000 photo, release build, `--no-video` (frames still run at the
harness's 30 fps of virtual time; wall-clock from the action to idle),
Apple Silicon, nine other builds running in parallel:

| Action | Time |
| --- | --- |
| Open the PNG | 0.5 s |
| Add Curves from the chip | 0.5 s |
| Press-and-drag on the curve, 12 frames | 1.05 s (46 ms a frame) |
| Add Hue/Saturation | 0.12 s |
| Drag the Hue slider, 10 frames | 1.54 s (67 ms a frame) |
| Add a live Gaussian Blur | 0.17 s |
| Drag the live blur's Radius, 10 frames | 1.83 s (80 ms a frame) |
| Open Levels (Cmd+L) | 0.47 s |
| Type an input black point (preview) | 0.71 s |
| Drag Gamma in the dialog (preview) | 1.31 s |
| Apply Levels | 0.54 s |
| Open Filter ▸ Blur ▸ Gaussian Blur… (first preview) | 1.5 s (2.1 s with the menu) |
| Type a 20 px radius (preview) | 2.5 s |
| Drag the dialog's Radius | 3.6 s for 10 frames (~180 ms a frame) |
| Apply the blur | 1.16 s |
| Undo the blur | 0.46 s |

Adjustment layers and live filters stay interactive on 12 MP; filter
dialogs do not (finding 12).

## Not tested, and why

- Pen pressure, and dragging files from the Finder: not simulated by the harness.
- Painting on an adjustment layer's mask: covered by B (masks) and D (painting); here masks come from selections, are disabled, enabled and deleted.
- `.3dl` LUT files and very large `.cube` tables: only a 2×2×2 `.cube` and a broken one were loaded.
- Liquify (Filter menu): area E.
- PSD round trip of adjustment layers and styles: area I.
