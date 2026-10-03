# H. Viewing and the workspace: user-journey results

Tested on 2026-10-03 with the `uitest` harness, real mouse and keyboard
input only, at 1440×900, 1024×700 and 900×600. Scenarios:
`crates/app/src/uitest/scenarios/h_workspace.rs` (16 journeys). Every
journey reads the window size from the session, so its expected values
(the fit zoom, the zoom levels, centred pans, the Navigator's picture)
are computed for that size rather than hard-coded. Most open the demo
photo (1800 × 1205 px, 72 ppi); the History journey paints on a new
800 × 600 image so every stroke's pixels are known.

Recordings (each folder has `session.mp4`, `session.json`, `keyframes/`,
and for the layout journey `stills/`):

- before: `<recordings>/h_workspace/before/{1440x900,1024x700,900x600}/<scenario>/`
- after: `<recordings>/h_workspace/after/{1440x900,1024x700,900x600}/<scenario>/`

The harness gained `relaunch()` (quit, keeping the profile, and start
again), `key_down` / `key_up` (Space held while dragging), `canvas_hover`
and `window_size`, and `menu()` no longer takes a section title such as
Properties' HISTOGRAM for an open menu's item.

## Journeys

| Journey | Result | After video (1440×900; same path under `1024x700/`, `900x600/`) |
| --- | --- | --- |
| `h-zoom`: the photo opens fitted and centred; the View menu shows Cmd+=, Cmd+−, Cmd+0, Cmd+1; View ▸ Zoom in, Cmd+=, Cmd+− step through the zoom levels about the centre (fit 52.8% → 66.7% → 100%); Actual pixels is 100% centred; Cmd+0, Cmd+1, Fit on screen; Print size is an inch per inch at 72 ppi; the zoom pill's Fit, + and −; no history step and no unsaved dot | fixed (3, 7, 16) | `after/1440x900/h-zoom/session.mp4` |
| `h-zoom-mode`: Z picks the Hand in Zoom mode; a click zooms one level in keeping the clicked pixel under the pointer; Alt-click one level out about it; Pan (H) in the options bar | fixed (3) | `after/1440x900/h-zoom-mode/session.mp4` |
| `h-pan`: with the Brush active, Space-drag pans by exactly the drag and paints nothing; H and a drag pan; the wheel pans 120 points; no history step | pass | `after/1440x900/h-pan/session.mp4` |
| `h-navigator`: Window ▸ Navigator, type 200 in its zoom field (200%), click a quarter across the picture (the canvas centres on that pixel ±1.5), its − steps to 150%, close, reopen, relaunch: still open | fixed (3, 10) | `after/1440x900/h-navigator/session.mp4` |
| `h-info`: Window ▸ Info; pointing at (900, 250) shows that pixel's R G B in Info and the status bar, x/y, the print size 25.00 × 16.74 in; a marquee's W × H in Info and the status bar; close | pass | `after/1440x900/h-info/session.mp4` |
| `h-histogram`: the palette finds “histogram”; Window ▸ Histogram shows the graph with mean, median and 2 169 000 pixels; inverting the photo flips the mean about 127.5; close | fixed (4) | `after/1440x900/h-histogram/session.mp4` |
| `h-channels`: Channels tab, the Red channel alone (document untouched, message spells Cmd+2), Cmd+2 back, Cmd+4 green, no history step; a marquee saved as an alpha channel, Cmd+D, Cmd-click the channel loads the same bounds, viewing the alpha channel, three undos remove it | fixed (17) | `after/1440x900/h-channels/session.mp4` |
| `h-history`: three strokes; clicking card “History step N: Paint stroke” goes back (two redo steps wait); right-click ▸ New snapshot; the last step; the snapshot card restores; Cmd+Z; Open as History Brush source, the History Brush (from the palette) paints the middle stroke away; the header folds and unfolds the strip | fixed (2) | `after/1440x900/h-history/session.mp4` |
| `h-proof`: View ▸ Proof colors (status bar note, document colours untouched), Cmd+Y off, View ▸ Gamut warning, Shift+Cmd+Y off, no history step | pass | `after/1440x900/h-proof/session.mp4` |
| `h-palette`: Cmd+K, “fit” + Enter fits and closes; the top bar's search box (Help ▸ Search commands at 900 wide) and “navigator”; Esc changes nothing; “deselect” shows “Make a selection first” and Enter keeps the palette open; “100”, “channels”, “preferences”, “shortcuts”, “proof” each find their command | fixed (14) | `after/1440x900/h-palette/session.mp4` |
| `h-shortcuts`: Help ▸ Keyboard shortcuts lists H, Z, Cmd+=, Cmd+0, Cmd+1, Cmd+R, Cmd+2, Cmd+K, Cmd+,; Esc closes; Cmd+R and Cmd+, do what it says | fixed (7, 8, 9) | `after/1440x900/h-shortcuts/session.mp4` |
| `h-rebind`: Preferences ▸ Proof colors → Shift+Cmd+P; Gamut warning → Cmd+S is refused (“Cmd+S is already the shortcut for Save”), → Option+Cmd+G is kept with its Option; Save; both new chords work, Cmd+Y no longer proofs, the View menu shows Shift+Cmd+P; relaunch: still bound; Reset both; Cmd+Y proofs again | fixed (5) | `after/1440x900/h-rebind/session.mp4` |
| `h-preferences`: Undo steps 20, Undo memory 512 MB, Autosave 300 s, Render cache 512 MB, show cache in the status bar, Light surround, grid 50 px / 2; Save (“Preferences saved”, editor limits applied, “Cache N MB” shown); change and Esc keeps 20; relaunch: every value kept and applied to the next document; reached through the Edit menu at every size (it scrolls at 900×600) | fixed (6) | `after/1440x900/h-preferences/session.mp4` |
| `h-welcome-layout`: welcome screen and workspace controls wholly inside the window and not overlapping (Properties' clipped contents allowed); Preferences' Save/Cancel, Keyboard shortcuts, Navigator + Info fit; stills of each | fixed (15); open (18, 19, 20) | `after/1440x900/h-welcome-layout/session.mp4` |
| `h-status-bar`: size, ppi (print size on hover), layer count, selection; “Opened the demo…” clears after the first edit; “Undid …” shows, then clears after the next edit; rename; undo, undo, redo: only “Redid …”; a marquee's size replaces it | fixed (1) | `after/1440x900/h-status-bar/session.mp4` |
| `h-tooltips`: Hand (H), the zoom pill's + − Fit with Cmd+= Cmd+− Cmd+0, the History header, the ppi's print size; the Hand's hint says scroll pans and Alt+scroll zooms (a tooltip below 1100 wide) | fixed (11, 12) | `after/1440x900/h-tooltips/session.mp4` |

All 16 pass at all three sizes after the fixes. Before them 10 of 16
failed at 1440×900, 12 at 1024×700 and 14 at 900×600 (the before runs
used the scenarios' final checks, so they show each finding as a failed
step; a few of the 1024×700 and 900×600 failures were the scenarios'
own, since corrected: h-pan's drag points and selection sizes that Snap
moves to the demo's layer edges). The other areas' 18 scenarios still pass with these changes.

## Findings

Severity as in user-journeys.md. Commits on branch
`worktree-agent-ade79307724a0a0be`: `b3b1a45` (the fixes), `8505de2`
(merge of main, one status-bar rule), `cda1788` (harness), `0bae97f`
(scenarios).

| # | Severity | Finding (step) | Status | Fix |
| --- | --- | --- | --- | --- |
| 1 | major | Status messages went stale: “Opened the demo: …” stayed through adding and renaming layers, “Undid …” through the next edit, “Redid …” after a new selection (h-status-bar). A user reads the last line as what just happened. | fixed | `b3b1a45`, unified with main's 10 s rule in `8505de2` |
| 2 | major | History cards were named by their step label alone, so a step called “Select” or “Open” shared its name with the menu; a screen reader (and the harness) clicking “Select” jumped back in history. Snapshots read “Snapshot Snapshot 1”. | fixed: “History step 3: Select”, “History snapshot: Snapshot 1” | `b3b1a45` |
| 3 | major | Zoom in/out multiplied by 1.25 from wherever the view was: from a fitted 52.8% it went 66.0%, 82.5%, 103.1% and never landed on 100% (blurry pixels). Zoom-mode clicks doubled instead, the pill and the Navigator's buttons ×1.25: three different steps. | fixed: every step follows Photoshop's levels (… 50, 66.7, 100, 150, 200 …%) | `b3b1a45` |
| 4 | major | No Histogram in the Window menu or the palette; the only one was a 56-point strip at the foot of Properties, unnamed for screen readers. | fixed: Window ▸ Histogram floating panel with Mean, Median, Pixels; palette entry; the graph is named | `b3b1a45` |
| 5 | major | Rebinding in Preferences: the buttons were named by their chord only (“Cmd+Y”), so nothing said which command they bind; Option was dropped (Option+Cmd+G saved as Cmd+G, which is Group layers); a chord another command holds was accepted and then one of the two silently never fired; a bare letter was accepted and broke that tool key. | fixed: “Shortcut for Proof colors: Cmd+Y”, “Reset shortcut for …”; Option recorded (old prefs load); taken chords and bare keys refused with the reason; chords tried most-modifiers first | `b3b1a45` |
| 6 | major | At 900×600 the Edit menu ran off the bottom of the window: Clear, Define brush tip and Preferences… could not be reached. | fixed: menus taller than the window scroll | `b3b1a45` |
| 7 | minor | View ▸ Fit on screen and Actual pixels showed “0” and “1”; Photoshop's Cmd+0 / Cmd+1 worked but were shown nowhere. | fixed (the bare keys still work) | `b3b1a45` |
| 8 | minor | Cmd+, (every Mac app's Preferences) did nothing; Preferences had no shortcut. | fixed | `b3b1a45` |
| 9 | minor | Keyboard shortcuts listed no Zoom (Z) and no Search commands (Cmd+K). | fixed | `b3b1a45` |
| 10 | minor | The Navigator's − and + were named “Zoom out”/“Zoom in”, the same as the zoom pill's. | fixed: “Navigator zoom out/in” | `b3b1a45` |
| 11 | minor | The zoom pill's −, + and Fit had no tooltips, so their keys could not be learnt there. | fixed: “Zoom in (Cmd+=)”, “Fit on screen (Cmd+0)” … | `b3b1a45` |
| 12 | minor | The Hand's hint said “Drag to pan, scroll to zoom”; the wheel pans and Alt+scroll zooms. | fixed | `b3b1a45` |
| 13 | minor | The top bar's search box changed its accessible name with the window width (“Search tools, filters...   Cmd+K”, “Search...   Cmd+K”, “Cmd+K”). | fixed: always “Search tools, filters and commands” | `b3b1a45` |
| 14 | minor | Typing “100” in the palette found nothing (Photoshop users call it 100%). | fixed: “Actual pixels (100%)” | `b3b1a45` |
| 15 | minor | At 900×600 the welcome screen's demo card ran below the window, its caption hidden. | fixed: the photo letterboxes below 700 points | `b3b1a45` |
| 16 | polish | A document was fitted to the canvas of its first frame; the dock settles a frame later, so the photo ran 2% into the margin (0.5283 for 0.5272 at 1440×900, 0.2283 for 0.2239 at 900×600). | fixed: a fitted view stays fitted while the canvas changes size, until the user zooms or pans | `b3b1a45` |
| 17 | polish | The channel message said “⌘2 returns to RGB” while every other shortcut reads “Cmd+2”. | fixed | `b3b1a45` |
| 18 | major | At 900×600, with an adjustment layer selected, Properties gets about 70 points: the Curves editor sits in a one-row scroll area. | open: proposal 1 | |
| 19 | minor | At 900×600 Preferences' shortcut list is two rows tall. | open: proposal 2 | |
| 20 | minor | At 900 wide with a document open the top bar has no room for the search box (Cmd+K and Help ▸ Search commands still work). | open: proposal 3 | |
| 21 | minor | Not this area: the Layers footer's “Group selected layers (Ctrl+click to multi-select)” says Ctrl on macOS. | reported | |

## The status-bar rule (shared)

One rule for every message, in `App::status_message` (status.rs): a
message shows until the document moves on (an edit, undo, redo, history
jump or tab switch) or for 10 seconds, whichever comes first. Code keeps
setting `self.status` anywhere. A message set anew starts over even with
the same words (two undos of two “Paint stroke” steps both show), told
apart by its string buffer; a message first seen in the middle of a frame
takes the document version that frame ends with (`App::settle_status` at
the start of each frame), so a message set just before its own edit is
not cleared by it. This merges main's 10-second display rule (C.
Selections) with this branch's version tracking.

## Proposals

1. **Properties on short windows** (finding 18): below about 700 points
   fold the “Add above active layer” chips to one row with a menu (the
   Layers footer's adjustment button already offers them) and let Layers
   shrink to two rows, which gives Properties about 200 points at 900×600.
   Not done here: other areas' scenarios click those chips by name.
2. **A Shortcuts page in Preferences** (finding 19), beside General and
   AI models, so the list can use the dialog's full height.
3. **An icon-only search button** in the top bar when there is no room for
   the box (finding 20).
4. **Zoom**: make the pill's percentage editable; drag in Zoom mode to
   zoom to a rectangle (or scrubby zoom); double-click the Hand for Fit and
   Z-double-click for 100%, as in Photoshop.
5. **Histogram**: a channel menu (RGB, Red, Green, Blue, Luminosity) and
   statistics for the selection only.
6. **History**: rename snapshots (double-click), as Photoshop does.

## Not tested, and why

- Middle-button drag, pinch zoom and Alt+scroll zoom: the harness has no
  middle button, gestures, or modifiers on the wheel.
- Window resizing during a session: the harness's window size is fixed
  per session, so each size is its own run.
- The Preferences render-cache budget's effect on memory: only the saved
  value, the editor's undo limits and the status bar readout are checked.
