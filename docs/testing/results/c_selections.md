# C. Selections: user-journey results

Tested on 2026-10-03 with the `uitest` harness, real mouse and keyboard
input only, at 1440×900 and at 900×600. Scenarios:
`crates/app/src/uitest/scenarios/c_selections.rs` (14 journeys) plus the
harness author's `select-and-mask` example in `selection.rs`. Every
journey opens `shapes.png`, a 400 × 300 fixture with flat colour areas at
known places (red square 40..120, dark red beside it 120..160, a second red
square 280..340 × 40..100, a blue disk centred on (200, 210) with radius
50), so each check has exact expected bounds, coverage values or mask
values, and each one checks undo.

Recordings (each folder has `session.mp4`, `session.json`, `keyframes/`):

- before: `<recordings>/c_selections/before/{1440x900,900x600}/<scenario>/`
- after: `<recordings>/c_selections/after/{1440x900,900x600}/<scenario>/`

Object Selection and Select Subject belong to area I and were not tested.

## Journeys

| Journey | Result | After video (1440×900; same path under `900x600/`) |
| --- | --- | --- |
| `c-marquee-modifiers`: new rectangle, Shift adds, Alt subtracts, Shift+Alt intersects, three undos, Shift pressed mid-drag squares, Alt mid-drag centres, click deselects | fixed (findings 4, 8) | `after/1440x900/c-marquee-modifiers/session.mp4` |
| `c-ellipse-feather`: Shift+M, Shift-drag a circle with nothing selected, a 10 px options-bar feather softens the next ellipse, undo, 0 px is hard again | fixed (2, 3, 4) | `after/1440x900/c-ellipse-feather/session.mp4` |
| `c-lasso`: freehand triangle, Shift adds a second, undo; Polygonal Lasso by four clicks and a click on the first point; Esc drops an unfinished polygon; Close in the options bar; undo | fixed (5, 6) | `after/1440x900/c-lasso/session.mp4` |
| `c-magic-wand`: exact red square, tolerance 25 takes the dark red, Contiguous off takes the far square, Shift adds the disk, Alt subtracts, undo, Shift+Alt intersects, empty layer selects its transparency, All layers finds red | fixed (9, 12) | `after/1440x900/c-magic-wand/session.mp4` |
| `c-quick-selection`: brushing the red square selects it (not white or blue), a second stroke adds the disk, undo | pass | `after/1440x900/c-quick-selection/session.mp4` |
| `c-select-menu-basics`: Deselect greyed out with its reason; Select ▸ All, Deselect, Reselect, Invert; undo; Shift+Cmd+I, Cmd+D, Shift+Cmd+D, Cmd+A; undo | pass | `after/1440x900/c-select-menu-basics/session.mp4` |
| `c-colour-range`: Eyedropper on red, Select ▸ Colour range previews both red squares, Select keeps it; Fuzziness 100 widens to the disk; Cancel restores and leaves no history step | pass (proposal 2) | `after/1440x900/c-colour-range/session.mp4` |
| `c-modify`: Expand 10, Contract 10, Border 8, Smooth 10, each with undo; Expand reopens at 10 and Esc withdraws its preview without a step; Select ▸ Modify ▸ Feather… 10 px, undo; Shift+F6 reopens it at 10; the palette finds “feather” | fixed (1, 2, 10) | `after/1440x900/c-modify/session.mp4` |
| `c-grow-similar`: Grow takes the touching dark red, Similar the far red square, white and blue stay out, two undos | pass | `after/1440x900/c-grow-similar/session.mp4` |
| `c-save-load-selection`: Save selection “Box”, Load selection, Cmd-click the Box alpha channel, Load inverted, undo; save “Disk” and pick Box from the list | fixed (1, 11) | `after/1440x900/c-save-load-selection/session.mp4` |
| `c-quick-mask`: Q, D, B, paint black across the selection, Q: the band leaves the selection; undo | pass | `after/1440x900/c-quick-mask/session.mp4` |
| `c-select-and-mask`: Select and Mask on a wand selection, view On black, Refine Edge brush stroke, output to Layer mask (1 inside, 0 outside), undo; Alt+Cmd+R again remembers Layer mask, output to New layer with mask | pass | `after/1440x900/c-select-and-mask/session.mp4` |
| `c-layer-pixels`: Layer via copy of the red square, Layer ▸ Load layer pixels as selection, Cmd-click the layer thumbnail, undo | fixed (7) | `after/1440x900/c-layer-pixels/session.mp4` |
| `c-action-bar`: Fill, New layer, Mask and Clear from the floating bar, each undone | fixed (13) | `after/1440x900/c-action-bar/session.mp4` |
| `select-and-mask` (example): marquee, Select ▸ Modify ▸ Feather… 24 px, mask from selection | updated for finding 2 | `after/1440x900/select-and-mask/session.mp4` |

All 15 pass at both sizes after the fixes. Before the fixes, 7 of 14 failed
at 1440×900 and 9 of 14 at 900×600 (two of those were the scenario's own
fault, since fixed in the scenario: the Wand switch's shorter labels and
scrolling Select and Mask's settings to Output).

## Findings

Fix commit for every fixed finding: `fd2f33a`.

| # | Severity | Finding (step) | Status |
| --- | --- | --- | --- |
| 1 | blocker | A dialog reopened after a different dialog sat **below** the invisible modal backdrop, so the mouse could not reach any of its controls (only Esc/Enter worked). Seen opening Expand after Contract/Border/Smooth (`c-modify` step 31) and Save selection after Load selection (`c-save-load-selection` step 31). Cause: the backdrop moves to the top of egui's layer order every frame, and a reused window keeps its old place. Applies to every dialog in `dialogs.rs`. Now the window is the backdrop's sublayer. | fixed |
| 2 | major | Feather is not under Select ▸ Modify and has no dialog or Shift+F6; the menu item “Feather 12 px” applied the options-bar value to the current selection. Now Select ▸ Modify ▸ Feather… (Shift+F6, palette “Feather selection…”) opens a previewing dialog with a radius, like Expand. | fixed |
| 3 | major | The options bar's Feather (default 12 px; Photoshop's is 0) was ignored by new marquee and lasso selections; its Apply button feathered the current one. Now it starts at 0 and softens every new marquee, lasso and polygon selection; Apply is gone. | fixed |
| 4 | major | Marquees ignored Shift/Alt pressed mid-drag (no square, no from-centre), Shift-drag with nothing selected made no square, and the keys were read at the release, so pressing Alt mid-drag turned the new rectangle into a subtraction (`c-marquee-modifiers` steps 27–30, `c-ellipse-feather` step 10). Now the keys at the press choose the combination and keys pressed during the drag shape it, as in Photoshop; the hint says so. | fixed |
| 5 | major | Polygonal Lasso: Esc with a polygon open deselected the existing selection and left the polygon open on screen (`c-lasso` step 25). Now Esc drops the polygon and keeps the selection. | fixed |
| 6 | major | Polygonal Lasso: two quick clicks on different corners counted as a double-click and closed a two-point polygon, which deselected everything (`c-lasso` at 900×600, where corners are closer on screen). A double-click now closes only on the spot of the previous click. | fixed |
| 7 | minor | Layer ▸ Layer via copy (and the action bar's New layer) left the source layer active; Photoshop selects the copy. Load layer pixels then loaded the Background (whole canvas) instead of the copy (`c-layer-pixels` step 12). | fixed |
| 8 | minor | The status bar kept old messages long after they stopped being news (“Opened /private/…/shapes.png” over several edits, “Undid Smooth selection 10 px” while Border ran). Messages now show until the next edit, undo or tab switch, or for 10 s. | fixed |
| 9 | minor | Magic Wand ignored Shift+Alt (intersect), unlike the marquees and lassos (`c-magic-wand` step 23). | fixed |
| 10 | minor | Select ▸ Modify dialogs reopened at their defaults (4 px); Photoshop remembers the last amount. Now each remembers its last confirmed value for the session. | fixed |
| 11 | minor | The Load selection dialog's “Saved selection” list opened under the dialog: “Subtract” was in the way of the list item (`c-save-load-selection` step 36). | fixed |
| 12 | minor | At 900×600 the Magic Wand options bar clipped “All layers”. On a tight bar Tolerance is now a number field (still typeable and scrubbable). | fixed |
| 13 | polish | The selection action bar was placed under the selection's 256-px tile bounds, far below small selections. It now sits under the selection's pixels. | fixed |
| 14 | polish | The Wand mode switch's accessible names change with the window width (“Quick selection” at 1440, “Quick” at 900). A screen reader user hears different names for the same control. | open |
| 15 | minor | Colour Range only matches the brush colour: it can't sample by clicking the image while open, and has no Invert or Selection preview; the dialog covers part of the image. | open, proposal 2 |
| 16 | minor | Magic Wand / Grow / Similar tolerance is a percentage in linear light (default 12%), not Photoshop's 0–255 in sRGB (default 32); dark tones fall within tolerance far sooner than a Photoshop user expects. | open, proposal 1 |
| 17 | polish | Esc deselects whenever nothing else claims it (a deliberate “get me out” choice); Photoshop never deselects on Esc. | open, noted |
| 18 | polish | A 10 px feather spreads faint coverage about 46 px out (the status bar's selection size jumps from 100 to 192), and soft coverage to about 18 px. Photoshop's looks similar to the eye; not changed. | noted |

### Found outside this area (not changed)

- `adjust_dialogs.rs` (Image ▸ Adjustments dialogs) and `ai_ui.rs` use the
  same “move the backdrop to the top every frame” pattern as finding 1, so
  reopening one of those dialogs after another may leave it under its
  backdrop. The fix is the same one line (`ctx.set_sublayer(backdrop, window)`).
- The Layers panel shows only about one and a half rows at 1440×900 with the
  Properties panel open (area B/H).
- Spelling: the Select menu says “Colour range”, the Layer menu “Solid color”.

## Proposals (not done)

1. **Photoshop-scaled tolerance.** Measure Magic Wand, Grow and Similar
   tolerance on gamma-encoded 8-bit levels, 0–255 with a default of 32,
   matching Photoshop's numbers and its feel in shadows. This changes how
   an engine command (`flood_region`, `GrowSelection`) picks pixels and how
   saved documents and actions read the value, so it needs a decision and an
   ADR.
2. **Colour Range as Photoshop's.** Let the canvas take clicks while the
   dialog is open (an eyedropper: click to sample, Shift-click to add a
   colour, Alt-click to take one away), and add Invert and a small selection
   preview. That means the dialog stops being modal over the canvas, a
   larger UX change.
3. **Marquee Style.** Photoshop's marquee options offer Normal / Fixed Ratio
   / Fixed Size with width and height fields; not present here.

## Not tested, and why

- Object Selection and Select Subject (area I).
- Pen pressure in Quick Mask and the Refine Edge brush: the harness has no
  pen input.
- Whether the Refine Edge brush stroke changed the edge in a specific way:
  the scenario paints it and checks that the result is still the expected
  mask inside and outside, not the exact edge values.

## Proposed ROADMAP lines

- [x] Select ▸ Modify ▸ Feather… (Shift+F6); options-bar Feather softens
  new marquee/lasso selections (default 0 px).
- [x] Marquee modifier keys as in Photoshop (Shift/Alt at the press combine,
  mid-drag square/centre).
- [ ] Photoshop-scaled Magic Wand tolerance (0–255, sRGB) — needs an ADR.
- [ ] Colour Range: sample from the image while open, Invert, preview.
