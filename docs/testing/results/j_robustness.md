# J. Undo, history and robustness: results

Tested 2026-10-03 with the user-session harness (real input, recorded), at
1440 × 900 and 900 × 600. Scenarios: `crates/app/src/uitest/scenarios/j_robustness.rs`
(13 journeys). Videos: `<recordings>/j_robust/before/` (main at
28b864b plus the scenarios) and `.../after/` (this branch); the 900 × 600 runs are
in the `900x600/` folder of each.

Every undo journey compares the whole document after each undo with the state
before that step (size, layer tree with settings, selection, a hash of every
composite pixel) and checks one history step per user action; redo must bring
each state back exactly.

## Journeys

| Journey | Result | After video |
| --- | --- | --- |
| undo-paint-and-layers: brush, eraser, new layer, rename, opacity drag, hide, show, reorder, duplicate, delete; 11 undos, 11 redos | pass | after/undo-paint-and-layers/session.mp4 |
| undo-transform-and-canvas: Move drag, 3 nudges (one step), free transform typed 50 %, Esc-cancelled transform, crop, canvas size, image size, rotate | pass | after/undo-transform-and-canvas/session.mp4 |
| undo-adjust-filter-text-shape: Curves layer, curve point, curve drag, opacity, Invert, Gaussian Blur, text, shape, corner radius | pass (1440); 900 × 600 open, see O7 | after/undo-adjust-filter-text-shape/session.mp4 |
| undo-selection-and-mask: marquee, Shift add, Alt subtract, invert, deselect, reselect, feather, mask from selection, Esc, disable mask | pass | after/undo-selection-and-mask/session.mp4 |
| history-jump-and-branch: jump by History cards, branch off with a new edit, snapshot and restore, undo the restore | fixed (F4, F6) | after/history-jump-and-branch/session.mp4 |
| unsaved-after-undo: save, undo, other edit, close; same with a full history | fixed (F1) | after/unsaved-after-undo/session.mp4 |
| undo-after-reopen: format-3 save, close, reopen, edit (text still editable), undo to the opened state | pass (O2 noted) | after/undo-after-reopen/session.mp4 |
| history-limit: Undo steps 5 in Preferences, 8 strokes, undo to the oldest kept, greyed Undo says why, raise the limit | fixed (F5) | after/history-limit/session.mp4 |
| autosave-and-recover: autosave at 15 s of two unsaved tabs, crash (app replaced, nothing cleaned), Recover, both come back exactly, Cmd+S goes to the original file | pass | after/autosave-and-recover/session.mp4 |
| odd-behaviour: menu bar and Cmd+Z under a dialog, Esc on menu/palette/transform/crop/polygonal lasso, 5 × Cmd+Z with 1 step, stroke dragged off the window, marquee past the canvas, tab click under a filter dialog, tab switch mid-transform and mid-typing | pass | after/odd-behaviour/session.mp4 |
| full-history-crop: crop with the history full | fixed (F2) | after/full-history-crop/session.mp4 |
| large-document: 6000 × 4000, 30 layers, stroke, undo, redo, transform, opacity, hide, save, reopen | fixed (F3); painting speed open (O1) | after/large-document/session.mp4 |
| long-session: 300 strokes, 12 full-canvas inverts, 150 undo/redo, then 99 undos | pass | after/long-session/session.mp4 |

At 900 × 600 every journey but one passes after the fixes (journeys needing
Preferences go through the Edit menu, which now scrolls; Properties has to be
scrolled to reach the curve and shape settings).

## Findings

| # | Severity | Finding | Status | Commit |
| --- | --- | --- | --- | --- |
| F1 | blocker | Unsaved state was "history length differs from the one saved". Save, undo, make a different edit: the title lost its dot and Cmd+W closed without asking, losing the edit. With the history full (100 steps by default), every edit after a save kept the length, so nothing after the save counted as unsaved: no prompt on close, no autosave backup. | fixed: `Editor::revision` names each document state; unsaved = revision differs | 30e5336 |
| F2 | major | With the history full, crop, perspective crop, Content-Aware Scale, Convert to smart object, Select and Mask and Camera Raw Filter took their own successful edit for a failure (same length test): after a crop the view didn't fit, no status, the frame logic was skipped. | fixed (revision changed = applied) | 30e5336 |
| F3 | major | Undo on a 6000 × 4000, 30-layer document took 2.7–5.8 s (Editor::undo 1.4 s, redo 1.5 s): every undo re-evaluated every layer's content chain, which no longer fits the render cache. | fixed: projection reuses layers whose content key is unchanged; Editor::undo 9–12 ms, redo 1–4 ms; Cmd+Z to redrawn screen 0.2 s | a81ba8c |
| F4 | minor | History thumbnails were kept by position: after branching off with a new edit the newest card showed the discarded branch; at the limit every card showed its neighbour. | fixed: kept by revision | 30e5336 |
| F5 | minor | Once the limit dropped the opened state, the oldest card was still called "Open". | fixed: `Editor::base_label` | 30e5336 |
| F6 | polish | Snapshot card spoken as "Snapshot Snapshot 1". | fixed here, then superseded by main's card names ("History snapshot: …") | 30e5336 / merge |
| F7 | major | A dialog reopened after another dialog sat under the modal backdrop: painted but unclickable (File ▸ New's Create ignored clicks after Canvas size had been open). | found in parallel with C; dialogs.rs fixed on main (fd2f33a); the AI first-use dialog had the same bug, fixed here with a test (main's I merge has an equivalent fix, kept) | a8800fc |
| F8 | major | 900 × 600: the Edit menu is taller than the window; egui slid it over the menu bar, losing Undo at the top or Preferences at the bottom. | fixed here (scrolling menus); main's H merge fixed it the same way; kept main's, plus menus reopening at the top (test) | 9ff1140 / 2505b96 |
| F9 | minor (harness) | A key chord that opens a system file panel (Cmd+S, Shift+Cmd+S) stopped the session. | fixed in `press_key` | 688f90d |
| O1 | major | Painting on the 6000 × 4000 × 30 document: 84–240 ms per frame during a long stroke (6–17 s for a 72-frame stroke, varying with machine load); opacity drag 43–122 ms per frame; free transform commit 1.1–3.2 s. | open, proposal P1 | |
| O2 | polish | A reopened text layer differs from the edited one by at most 7.6e-6 (half a 16-bit step) in 3,367 pixels: the text cache rendered on load is not stored exactly like the one made while editing. Invisible, but reopen is not bit-identical. | open | |
| O3 | minor | Action recorder counts new steps by history length: with the history full, unrecordable edits are not noted. | open, proposal P2 | |
| O4 | polish | Names differ between menu and history: "Delete layer" / "Remove layer", "New pixel layer" / "Add layer 'Layer 2'", nudges are "Move" (Photoshop: "Nudge"). | open | |
| O5 | minor | Status bar kept "Duplicated as 'Paint copy'" after the copy was deleted (main has since added status expiry; not re-checked). | open | |
| O6 | polish | The greyed Undo item's tooltip ("Nothing to undo") covers the Redo item. | open | |
| O7 | minor (fits) | 900 × 600: Properties shows about 100 points; the curve editor and shape settings must be scrolled to. The scenario's last step (Corners slider) still fails there: scrolling Properties by a fixed amount overshoots. | open | |
| O8 | minor | Eraser on the Background layer erases to transparency (Photoshop paints the background colour). Area D. | open | |

Behaviour checked and fine: every edit kind is one step and undoes exactly (pixel
hash equal); slider drags (layer opacity, curve point drag, corner radius) are
one step; three arrow nudges are one step; Esc cancels free transform, crop,
polygonal lasso, menus and the palette without a step; Cmd+Z and the menu bar do
nothing under a modal dialog; clicking another tab under a filter dialog keeps
the dialog's document in front and Cancel leaves both documents untouched;
switching tabs drops a live transform and keeps half-typed text; a stroke
dragged off the window ends as one step and the next click is a separate dab;
a marquee dragged past the canvas stops at its edge; autosave backs up every
unsaved tab with its source, and Recover brings both back exactly.

## Performance and memory

Wall-clock times through the harness (they include its frame rendering and
video encoding; other testers' builds were running). 6000 × 4000, 30 layers
with a stroke each:

| | before | after |
| --- | --- | --- |
| New 6000 × 4000 | 2.6 s | 1.2 s |
| Long stroke across the canvas, 72 frames | 17.2 s (239 ms/frame) | 6.0 s (84 ms/frame) — not changed by this branch, load |
| Cmd+Z of that stroke, until idle | 5.8 s | 0.23 s |
| Redo | 0.9 s | 0.25 s |
| Editor::undo / redo alone | 1.4 s / 1.5 s | 9–12 ms / 1–4 ms |
| Free transform 50 %, Enter | 3.2 s | 1.1 s |
| Opacity drag, 31 frames | 3.8 s (122 ms/frame) | 1.3 s (43 ms/frame) |
| Save .lumen (0.9 MB file) | 1.4 s | 0.9 s |
| Reopen | 4.9 s | 1.6 s |
| RSS at the end | 3.2 GB | 3.5 GB |

The render cache holds 1.1 GB (budget 1.5 GB); undo history 0 MB (strokes are
ops). RSS includes the scenario's own full-resolution composites for its checks.

Long session (1600 × 1200, 300 strokes, 12 full-canvas inverts, 150 undo/redo):
history capped at 99–100 steps holding 35 MB; render cache 1.0–1.1 GB; RSS
between 0.86 and 1.53 GB with no upward trend from stroke 100 to stroke 300
(before and after alike). 99 undos one after another: 26 s before, 21 s after
(about 0.2 s per Cmd+Z including the harness's frames).

## Proposals

- **P1 (O1)**: profile a stroke preview on a large many-layer document: per
  frame the preview recomposites from the active layer up with the reference
  compositor; on 30 full-canvas layers that is most of the 84–240 ms.
- **P2 (O3)**: let the action recorder track `Editor::revisions()` instead of
  the history length.
- The render cache budget (1.5 GB) dominates memory even for small documents
  (1.1 GB for 1600 × 1200 after 300 strokes); consider scaling it with document
  size.
- ROADMAP lines: "Unsaved state and applied-edit checks by editor revision
  (J)"; "Undo/redo reuse unchanged layers' pixels: 1.4 s → 10 ms on 6000 × 4000
  × 30 (J)"; open: "Large-document painting speed (J, O1)".

## Not tested, and why

- Quitting with unsaved tabs: the harness treats the app's Close request as the
  end of the session, so the quit prompt could not be reached.
- Crash recovery is simulated by replacing the app in place (no cleanup runs);
  a real process kill and relaunch is outside the harness.
- Keys pressed in the middle of a drag (Cmd+Z mid-stroke): the harness's drags
  are atomic.
- graph_corpus_check: 482/482 identical after the core and graph changes (the
  corpus was fetched into this worktree; the main checkout had none).
