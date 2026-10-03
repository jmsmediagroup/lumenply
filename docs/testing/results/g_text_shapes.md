# G. Text, shapes and paths: user-journey results

Tested on 2026-10-03 with the `uitest` harness, real mouse and keyboard
input only (canvas clicks, drags, double and triple clicks, typed text,
key chords), at 1440×900 and at 900×600. Scenarios:
`crates/app/src/uitest/scenarios/g_text_shapes.rs` (17 journeys). Each
starts from File ▸ New, checks the document (layer kinds, text and its
per-character styles, shape geometry, path anchors and handles, pixels at
points) with explicit expected values, and checks undo.

Recordings (each folder has `session.mp4`, `session.json`, `keyframes/`):

- before: `<recordings>/g_text/before/{1440x900,900x600}/<scenario>/`
- after: `<recordings>/g_text/after/{1440x900,900x600}/<scenario>/`

The before runs are the original app (commit `89e9a0e`: main plus the
scenarios). `text-keyboard-styles` and `find-tools-in-the-palette` were
written after the first fixes, so they have no before run; what they test
did not exist or found nothing before (findings 11 and 12).

## Journeys

| Journey | Result | After video (1440×900; same path under `900x600/`) |
| --- | --- | --- |
| `text-click-to-type`: Text tool, click, type “Vote Big Today” (V, B and T type letters, no tool switch), Backspace ×6, Cmd+Left, type, Alt+Right, Shift+Alt+Right selects a word and typing replaces it, Esc commits (status “Text committed”), one history step, V picks Move again, undo/redo; an empty click then Esc leaves no layer and no step | pass | `after/1440x900/text-click-to-type/session.mp4` |
| `text-paragraph-box`: drag a 300×200 box, type until it wraps, Justify, drag the right handle 100 px, click outside commits without starting new text, the whole edit undoes in one step | fixed (5) | `after/1440x900/text-paragraph-box/session.mp4` |
| `text-style-a-word`: double-click “brave”, Bold and 60 px from the options bar (selection kept), red from the colour well's hex field, Esc closes the picker and the edit goes on, Cmd+A, Cmd+Shift+> grows every letter by 2 px, undo takes the session back | fixed (3, 4, 5, 6, 10) | `after/1440x900/text-style-a-word/session.mp4` |
| `text-font-and-align`: font picker, search “Georgia”, Enter picks it, Align centre puts the anchor in the middle of the glyphs, type 96 px, two undos back to left-aligned 72 px | fixed (4, 7, 10) | `after/1440x900/text-font-and-align/session.mp4` |
| `text-cancel-and-edit-again`: click inside existing text (caret at the click), type, Cancel restores without a history step, Enter edits the active text, Cmd-drag moves it while editing, Cmd+Enter commits, the Move tool drags it, undo | pass | `after/1440x900/text-cancel-and-edit-again/session.mp4` |
| `text-properties`: replace the text in Properties (two lines, layer named after the first), Leading to its end (4×), Paragraph conversion, rename the layer, Bold in Properties keeps the name, right-click ▸ Rasterize, undo | fixed (8) | `after/1440x900/text-properties/session.mp4` |
| `text-keyboard-styles`: Shift+Alt+Left selects a word, Cmd+Shift+B and U style it alone, triple-click selects the line, Cmd+Shift+I italicises all, Cmd+Shift+R right-aligns, no letters typed, one history step | new (12) | `after/1440x900/text-keyboard-styles/session.mp4` |
| `find-tools-in-the-palette`: Cmd+K “type” finds the Text tool, “rectangle” and “ellipse” the Shape tool | new (11) | `after/1440x900/find-tools-in-the-palette/session.mp4` |
| `shape-every-kind`: View ▸ Snap off, each of the nine kinds dragged in its own cell with the exact geometry checked, the ellipse's fill pixel, Shift-drag square, Shift+Alt circle round the press point, undo | fixed (9) | `after/1440x900/shape-every-kind/session.mp4` |
| `shape-fill-and-stroke`: fill None, stroke on, width 10, centred (in Properties at 900×600, where the bar has no alignment): white inside, black outline; gradient fill with a red fill colour from the well runs red to the background colour; undo | fixed (10) | `after/1440x900/shape-fill-and-stroke/session.mp4` |
| `shape-edit-in-properties`: Corners to half, Stroke, Dashed, Gradient fill, four steps, two undos; a polygon's Sides | pass | `after/1440x900/shape-edit-in-properties/session.mp4` |
| `shape-transform-and-rasterize`: Cmd+T, typed W 150 % and H 200 %, Enter: the vector redraws crisply at [50, 50, 350, 250]; Rasterize in Properties keeps the pixels; undo | pass (finding 16 was area E's) | `after/1440x900/shape-transform-and-rasterize/session.mp4` |
| `pen-draw-a-path`: two corner clicks, a drag pulls a smooth node (mirrored handles), a corner, a click on the first point closes; History names each step; one undo reopens the path, redo closes it; three more clicks and Esc keep an open second subpath; the next click starts a third | fixed (1, 2, 13) | `after/1440x900/pen-draw-a-path/session.mp4` |
| `pen-edit-anchors`: drag an anchor, click the curved anchor and drag its handle (the twin mirrors), click an anchor and Backspace deletes it, undo | pass | `after/1440x900/pen-edit-anchors/session.mp4` |
| `paths-panel`: Paths tab, save the work path (Path 1), double-click to rename “Outline”, fill with the brush colour (inside painted, outside white), undo, load as selection, a marquee then Make work path from selection, delete the saved path, undo | pass | `after/1440x900/paths-panel/session.mp4` |
| `path-to-shape-and-back`: Layer ▸ New shape from path (four-anchor path shape, filled), Clear path in the Pen bar, a star, Properties ▸ Make work path (ten anchors) | pass | `after/1440x900/path-to-shape-and-back/session.mp4` |

All 17 pass at both sizes after the fixes. Before them, 6 of 15 failed at
each size, all for real findings below.

## Findings

| # | Severity | Finding (step) | Status | Fix commit |
| --- | --- | --- | --- | --- |
| 1 | major | Pen: every click of a drawing session was coalesced into one undo step (“Edit path”), so Cmd+Z after one misplaced anchor removed the whole path (`pen-draw-a-path` step 17). Each click, drag and Backspace is its own step now. | fixed | `e8e671f` |
| 2 | major | Pen: Esc threw away the subpath being drawn (`pen-draw-a-path` step 24). Photoshop ends the path and keeps it; Esc now does what Enter does. | fixed | `e8e671f` |
| 3 | major | Text: after clicking Bold, an alignment or typing a size in the options bar mid-edit, the keyboard stayed off the text: further typing went nowhere, Cmd+A selected the whole canvas and Esc deselected it (`text-style-a-word`, history “… → Select → Deselect”). The keyboard returns to the text now. | fixed | `6161207` |
| 4 | major | Text: the Enter that confirms the options bar's size field also opened an edit of the text, so the next Cmd+Z went to that empty edit and seemed to do nothing (`text-font-and-align` step 26). | fixed | `6161207` |
| 5 | minor | Text: restyling during an edit (justify, a box resize, bold, size, colour) split one edit into three to six history steps, and Cancel did not take those back (`text-paragraph-box` step 23). The whole session is one step now, as in Photoshop. | fixed | `6161207` |
| 6 | major | Text: Esc to close the colour picker also committed the text edit (`text-style-a-word` step 23); with finding 3 the keys that followed then acted on the canvas. Esc in a popup now only closes it. | fixed | `6161207` |
| 7 | minor | Typing a size of 96 made two undo steps, the first at 9 px (each keystroke ended the step). | fixed | `6161207` |
| 8 | major | A text layer the user renamed went back to its first line at the next text or style edit (`text-properties` step 24). The name follows the text only while it is the automatic one. | fixed | `ddcbea2` |
| 9 | minor | The Shape kind list stopped after seven rows; Heart and Speech Bubble sat below a thin scroll hint and could not be clicked where the harness (or a user) looked for them (`shape-every-kind` step 35). | fixed | `78981b6` |
| 10 | minor | Accessibility: the fill and stroke colour wells (options bar and Properties) and the text colour well were spoken only as a hex value, so fill and stroke could not be told apart; the font picker was spoken as “Font ” for the bundled font. Now “Fill colour #3D85EB”, “Stroke colour #000000”, “Text colour #1A2E8C”, “Font family, DejaVu Sans (bundled)”. | fixed | `78981b6`, `6161207` |
| 11 | minor | Findable: Cmd+K “type”, “rectangle” or “ellipse” found nothing; Photoshop users call these the Type tool and the Rectangle/Ellipse tools. | fixed | `553d205` |
| 12 | minor | Consistent: Photoshop's text keys Cmd+Shift+B / I / U / slash (bold, italic, underline, strikethrough) and Cmd+Shift+L / C / R / J (alignment) did nothing while typing. They work now and are in the shortcuts sheet. | fixed | `553d205` |
| 13 | polish | Feedback: History called every Pen action “Edit path”. Now “Add anchor”, “Move anchor”, “Move handle”, “Close path”, “Delete anchor”. | fixed | `0da876b` |
| 14 | major | Fits: at 900×600 Properties gets about 70–100 px between the panel header and the quick-add chips, two or three rows; editing text or a shape there means scrolling a sliver, and the options bar drops Leading, Tracking and stroke alignment there too (`after/900x600/text-properties`, `shape-fill-and-stroke` step 13). | open: proposal 1 | |
| 15 | minor | Findable: at 1440×900 the text section of Properties does not fit either: Kerning, Baseline, Point/Paragraph and Box are below the fold, behind a thin scrollbar (`text-properties`). The code comment says it fits. | open: proposal 1 | |
| 16 | major | Free Transform: a plain corner drag scaled about the centre, not from the opposite corner (`shape-transform-and-rasterize`, first draft). Area E's; reported to the E tester, who fixed it on their branch. The scenario now uses typed W/H. | reported to E | |
| 17 | minor | Command palette ranking is plain substring order: “pen” finds “Open…” before the Pen tool, “path” finds Paths actions first. Area H's (palette search). | open: proposal 3 | |
| 18 | polish | History's thumbnail for “Add text” is blank: it is taken when the empty text layer is made, before anything is typed. | open | |
| 19 | polish | Paths panel: Save path keeps both “Work Path” and “Path 1”; Photoshop turns the work path into Path 1. Harmless, but two rows for one path. | open | |
| 20 | polish | Snapping at low zoom: at 46–51 % (900×600) a shape's corner lands up to 10 px from where it was aimed when it passes near the canvas centre or another layer's edge. That is View ▸ Snap working; the scenarios turn it off for exact geometry. | note | |
| 21 | polish | A paragraph box resized at low zoom keeps fractional sizes (399.99997 px); Cmd-drag moves round to whole pixels, resizing does not. | open | |

Not findings, checked and fine: Esc commits typed text (Photoshop 2019+
behaviour), an empty new text leaves no layer or step, Shift+click starts
new text over existing text, the overflow “+” on a too-small paragraph box,
caret and selection drawing, word and line jumps with Alt/Cmd+arrows,
in-session Cmd+Z, shapes stay vector through transforms (crisp edge pixels
checked after a 150/200 % scale), gradient fills run to the background
colour as the tooltip says, a new shape goes above the active layer, the
Pen's handles mirror, Fill/Stroke path greyed out with a reason when the
active layer is not pixels.

## Proposals (not done)

1. **Room for Properties at narrow and medium sizes** (findings 14, 15).
   The dock gives the quick-add chips, the Layers panel and Properties
   fixed shares; at 900×600 Properties is two rows. Options: collapse the
   “Add above active layer” chips to a single “Add…” button when the window
   is short, or let the Properties/Layers split be dragged and remember it,
   or give Properties a minimum of about 240 px and let Layers shrink.
   This is the dock layout (main.rs/panels.rs), shared by every area, so it
   is a layout decision for the coordinator rather than a local fix.
2. **Text session undo inside a session**: in-session Cmd+Z restores
   typing, character runs and size, but not an alignment or box change made
   in that session (those are undone with the session's history step after
   committing). Snapshotting the whole text layer would cover them.
3. **Palette ranking** (finding 17): prefer matches at a word start and
   tool entries for one-word queries, so “pen” and “path” reach the Pen.
4. **Work path to saved path** (finding 19): Save path could rename the
   work path into the saved one, as Photoshop does, instead of copying it.

Proposed ROADMAP lines (not edited here):

- [x] Text: one undo step per editing session; the keyboard stays on the
  text across options-bar edits; Photoshop's Cmd+Shift style and alignment
  keys; renamed text layers keep their names.
- [x] Pen: one undo step per anchor; Esc finishes the path; named history
  steps.
- [ ] Dock: Properties usable at 900×600 (see results/g_text_shapes.md, proposal 1).

## What could not be tested

- **IME composition** (Japanese, Chinese, dead keys): the harness sends
  `Text` events, not an input method's preedit and commit sequence, and
  there is no system IME headlessly. The canvas does accept `Ime::Commit`
  and reports a caret rectangle for the candidate window (code read only).
- **System clipboard**: the harness has its own clipboard; copy and paste
  of text inside a session are covered by unit tests in `text_edit.rs`.
- **Font availability**: the font scenario relies on Georgia, present on
  macOS; on a machine without it the search finds nothing (the picker then
  says “No installed font matches”).
- **Pen pressure, drag-and-drop of files, other windows**: not simulated by
  the harness; not needed for this area.
- **PSD round trip of text and shapes** belongs to area I.
