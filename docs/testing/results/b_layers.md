# B. Layers: user-journey results

Ten recorded user sessions cover every journey listed for area B in
[user-journeys.md](../user-journeys.md), plus the Layer menu, the Layers panel
footer, its context and "More" menus, and the layer actions in the command
palette. They are in `crates/app/src/uitest/scenarios/b_layers.rs` and run as

```sh
cargo run --release -p lumenply-app --features uitest -- --uitest \
    layers-add-rename-delete layers-duplicate-group ... [--size 900x600]
```

Each session drives the real UI with real input (menus, panel buttons, keys,
right-clicks, drags), checks the document with explicit values, and checks
undo. Every journey ran at 1440×900 and at 900×600.

- Before the fixes: `<recordings>/b_layers/before/<scenario>/`
  (1440×900) and `.../before/900x600/<scenario>/`.
- After the fixes: `<recordings>/b_layers/after/<scenario>/`
  and `.../after/900x600/<scenario>/`. Each holds `session.mp4`,
  `session.json` and `keyframes/`.

## Journeys

| Journey (scenario) | Result | After video (1440×900; 900×600 under `after/900x600/`) |
| --- | --- | --- |
| Add (+ button, Layer menu, Shift+Cmd+N), rename (double-click, Layer ▸ Rename, context menu, Esc cancels), delete (bin button, Layer menu), undo (`layers-add-rename-delete`) | fixed | `after/layers-add-rename-delete/session.mp4` |
| Duplicate (Cmd+J, Layer menu), Cmd-click multi-select, group (folder button, Cmd+G), collapse, ungroup (Layer menu, Shift+Cmd+G), undo (`layers-duplicate-group`) | fixed | `after/layers-duplicate-group/session.mp4` |
| Reorder by dragging, Bring to Front (Shift+Cmd+]), Send to Back (menu), move up/down (buttons, Cmd+] / Cmd+[), greyed button says why, undo (`layers-reorder`) | fixed | `after/layers-reorder/session.mp4` |
| Visibility eye, Alt-click solo and back, Opacity, Fill, blend mode (Screen), undo (`layers-visibility-blend`) | pass | `after/layers-visibility-blend/session.mp4` |
| Locks: transparency (paint stays inside), pixels (paint refused, Edit fills greyed with reason, Move still works), position (move refused), all (toggles covered, Properties greyed), unlock (`layers-locks`) | fixed | `after/layers-locks/session.mp4` |
| Masks: Reveal selection from the Layer menu (deselects), paint black on the mask, disable from the context menu, enable, Shift-click thumbnail, Apply, undo, Delete, Esc in an open menu keeps the selection, Hide selection, Alt-click the mask button (`layers-masks`) | fixed | `after/layers-masks/session.mp4` |
| Clipping (Alt+Cmd+G, context menu Release, Layer menu), Merge clipping mask (Cmd+E), Layer via copy (Cmd+J) and cut (Shift+Cmd+J), stamp visible, merge selected, merge visible, flatten, undo (`layers-clip-merge`) | fixed | `after/layers-clip-merge/session.mp4` |
| Fill layers: solid from the Layer menu, switched to gradient in Properties, undo, gradient and pattern fill layers, rasterize from the context menu (`layers-fill-layers`) | fixed | `after/layers-fill-layers/session.mp4` |
| Smart objects: convert, Free Transform 50 % then 200 % (stays crisp), Edit contents in a tab and save back, Replace contents (file panel), Rasterize, undo (`layers-smart-objects`) | pass | `after/layers-smart-objects/session.mp4` |
| Align left edges and distribute centres (Layer menu, three selected layers), the layer filter, the context menu, thumbnails (`layers-align-filter-menu`) | pass | `after/layers-align-filter-menu/session.mp4` |

All ten pass at 1440×900 and at 900×600 after the fixes (0 failed checks in
every `session.json`).

## Findings

Commits: `ffb0199` (core mask commands), `ce772a4` (app fixes), `765cab6`
(scenarios), merged with main in `eee20ec`.

| # | Severity | Finding (what happened → what a Photoshop user expects; where) | Status | Fix |
| --- | --- | --- | --- | --- |
| 1 | major | Renaming opened with the cursor at the end and nothing selected, so typing "Sky" gave "Layer 2Sky" → the whole name selected, typing replaces it. Before: `layers-add-rename-delete` steps 12–15. | fixed | `ce772a4` |
| 2 | major | Two quick clicks on different rows (pick one, Cmd-click another) counted as a double click and opened a rename on the second row; the multi-selection was lost, so Group grouped the wrong pair (Layer 2 + Background) and Cmd+E merged nothing. Before: `layers-duplicate-group` step 25. | fixed: only a second click on the same row renames, never with Cmd | `ce772a4` |
| 3 | major | Esc in an open menu closed it **and** deselected (the Esc-to-deselect key ran first). A user dismissing the Layer menu lost their selection. Before: `layers-masks` step 19 ("Deselect" in History). | fixed: Esc only closes a menu or popup that was open | `ce772a4` |
| 4 | major | No Layer ▸ Layer Mask submenu: a mask from the selection lived only under Select, and there was no Hide All, Hide Selection or **Apply** at all; the selection stayed active after making a mask. Before: `layers-masks` steps 15–19. | fixed: Layer ▸ Layer mask ▸ Reveal all / Hide all / Reveal selection / Hide selection / Delete mask / Apply mask / Disable (Enable) mask; selection masks deselect (Reselect brings it back, undo restores both); Alt-click on the mask button hides; Apply mask in the context and More menus; palette entries | `ffb0199`, `ce772a4` |
| 5 | minor | The visibility eye had no accessible control (screen readers and Tab couldn't reach it). | fixed: a "Show <layer>" checkbox with its own tooltip (Alt-click hint) | `ce772a4` |
| 6 | minor | At 1440×900 with two layers the Background row was half hidden behind the footer (rows 740–778, list ended at 764). The dock's split guessed 24 pt of chrome (40 in fact) and the lock row 4 pt short. | fixed: the chrome is measured each frame; `HEADER_H` 34 | `ce772a4` |
| 7 | minor | The Group button's name and tooltip said "Ctrl+click" on macOS. | fixed: Cmd on macOS | `ce772a4` |
| 8 | minor | Missing Photoshop chords: Shift+Cmd+G (ungroup), Alt+Cmd+G (clip / release), Cmd+] and Cmd+[ (move up / down). Cmd+] did nothing. Before: `layers-reorder` step 25. | fixed; the menus show them | `ce772a4` |
| 9 | minor | Greyed footer buttons repeated their name instead of saying why ("Move layer up"). Before: `layers-reorder` step 30. | fixed: "Move layer up: Already at the top", same for down, Add mask, Group, Delete | `ce772a4` |
| 10 | minor | Layer via copy (Cmd+J with a selection) left the source layer active. | fixed (on main by C. Selections, `28b864b`; this branch's duplicate dropped in the merge); checked here | main |
| 11 | minor | On a pixel-locked layer, Edit ▸ Fill with background colour, Fill…, Content-Aware Fill, Stroke, Cut and Layer via cut stayed enabled and then failed in the status bar. | fixed: greyed out with "The layer's pixels are locked" | `ce772a4` |
| 12 | minor | With Lock all on, Opacity, Fill and Blend in Properties stayed live; typing 40 % was refused after the fact. Before: `layers-locks` step 46. | fixed: greyed out under a "The layer is locked" note | `ce772a4` |
| 13 | minor | The row context menu offered Rasterize for colour and gradient fills, text and smart objects, but not for pattern fills or shapes. Before: `layers-fill-layers` step 34. | fixed | `ce772a4` |
| 14 | minor | A fill layer's Properties had two rows labelled "Fill" (the Fill-opacity slider and the Solid color / Gradient / Pattern switch), and the switch was wide enough to push the dock ~25 pt wider, shrinking the canvas whenever a fill layer was picked. Before: `layers-fill-layers` step 14. | fixed: "Type: Solid · Gradient · Pattern"; the dock keeps its width (checked) | `ce772a4` |
| 15 | polish | Shift-click on a mask thumbnail (Photoshop: disable / enable the mask) did nothing. | fixed | `ce772a4` |
| 16 | polish | Mask wording mixed "Remove mask" (context menu) with Photoshop's "Delete Layer Mask". | fixed: "Delete mask" / "Delete layer mask" everywhere | `ce772a4` |
| 17 | minor | At 900×600 the dock leaves Properties about 70 pt: only Opacity and Fill show, Blend and everything else need scrolling, and with three or more layers the last row is cut by the list's edge. | open: proposal 1 | — |
| 18 | polish | Dragging a row shows only the accent insertion line; the dragged row isn't lifted or dimmed. | open: proposal 2 | — |
| 19 | polish | The layer filter matches names only; Photoshop also filters by kind (pixel, adjustment, text, shape, smart object). | open: proposal 3 | — |

Checked and fine: duplicating "Layer 2 copy" names it "Layer 2 copy 2", as
Photoshop does. A fill still named after its kind follows a kind change
("Color Fill" → "Gradient Fill"), and a renamed one keeps its name. Undo is one
step per action everywhere. Alt-click solo restores the earlier visibility.
Locked layers show a padlock and a "Locked: …" tooltip. The status bar names
each refusal. Smart objects stay crisp through repeated transforms.

Noticed outside area B, not changed here: the TRANSFORM "Apply" button in
Properties keeps its orange fill while disabled (E); the Properties histogram
of a fresh pixel layer is an empty box (H).

## Proposals (not done)

1. **Dock at 900×600.** Below about 700 pt of height, fold the "Add above
   active layer" chips (two rows, ~100 pt) into one row or a single
   "Add…" button, so Properties shows at least Opacity, Fill and Blend and the
   Layers list keeps three whole rows. This changes the dock's layout for
   every area (F owns the chips), so it needs a decision.
2. **Drag feedback.** Lift the dragged row (a translucent copy under the
   pointer) and dim its slot, and show the insertion line indented when the
   drop lands inside a group.
3. **Filter by kind.** A small kind menu beside "Filter layers" (Pixel,
   Adjustment, Text, Shape, Smart object, Fill), as Photoshop's Layers panel
   has.
4. **Layer mask extras.** Layer ▸ Layer mask ▸ From Transparency, and
   Alt-click on a mask thumbnail to view the mask alone.
5. **Harness.** `Session::menu` treats any node with the next item's name as
   an open menu item, so a History step named "Duplicate layer" or a
   Properties "Gradient" button is clicked instead of the menu item; it should
   prefer the most recently opened popup. `within()` drops controls scrolled
   out of the panel. The scenarios work around both (`layer_menu` aims inside
   the open menu, using the new `Session::move_to` to travel into submenus).

Suggested ROADMAP lines:

- [x] Layer ▸ Layer mask: reveal / hide all or selection (deselects), apply, delete, disable; Shift-click the mask thumbnail
- [x] Layers panel: rename selects the name; accessible eye; Photoshop chords Shift+Cmd+G, Alt+Cmd+G, Cmd+] / Cmd+[
- [ ] Dock layout below 700 pt height (proposal 1); drag feedback; filter layers by kind

## What couldn't be tested and why

- **Pen pressure and file drag-and-drop onto the panel**: the harness doesn't
  simulate them.
- **Thumbnails' look**: the sessions check that every layer has a thumbnail
  texture and look at them in the stills, but don't compare their pixels.
- **Dragging a layer into a collapsed group**, and nested groups more than one
  deep: not covered by these ten sessions; RelocateLayer has unit tests.
- **PSD round trip of masks and fill layers**: area I (PSD round trip).
