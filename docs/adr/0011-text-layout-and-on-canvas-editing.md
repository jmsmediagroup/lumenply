# ADR 0011: Text layout, on-canvas editing, paragraph boxes and style runs

Date: 2026-10-03. Status: accepted.

## Context

Text was edited through a one-line field in the options bar; Photoshop
users expect to click into the text on the canvas, see a caret, select
with the mouse and keys, type into a paragraph box that wraps, and style
single words. The caret and the selection must sit exactly on the
rendered glyphs, and the whole edit has to stay one undo step.

## Decision

**One layout for drawing and editing** (`render::text_layout`). `layout`
places every character: explicit newlines, word wrap in a box, alignment
(left, centre, right, justify), tracking, leading, baseline shift, all
caps and per-character faces. The rasteriser draws from it, and the same
result answers caret position, hit testing (click → character), line
bounds and selection rectangles. Positions are byte offsets into the
layer's text at character boundaries. Without `kerning` advances stay
whole pixels (`ceil`, as fontdue's own layout did), so earlier documents
render as before to within a pixel of glyph placement; with it (new text
from the Text tool, Photoshop's "Metrics") the font's kerning pairs apply
within a face and advances keep their fractions.

**Paragraph text** is `TextLayer::box_size: Option<[w, h]>`. With a box,
`(x, y)` is the box's top-left corner and the first baseline sits one
ascent below it; without one, `(x, y)` is the first baseline's anchor as
before. Lines below the box are hidden (Photoshop's overflow "+"). Image
rotations, flips and crops move a box by its centre and keep its size.
Justify stretches wrapped lines only; point text treats it as left.

**Style runs** are `TextLayer::runs: Vec<TextRun>`: sorted,
non-overlapping byte ranges overriding colour, size, bold and italic
(the family stays layer-wide). `doc::text_runs` keeps them on their
characters through every edit — typing continues the style at the caret,
replacing a selection takes its first character's style — and normalises
them (no field repeating the layer's own value, adjacent equal runs
merged). Line height follows the largest characters on each line
(leading × size, from the previous baseline).

**All new fields are `#[serde(default)]`** (runs are omitted when
empty), so older `.lumen` files load unchanged. Older builds reading new
files ignore the unknown keys (losing boxes, runs, shift and caps), but
a layer aligned `"justify"` is an unknown enum value to them and stops
the file from opening there. The raster cache stays derived state.

**Editing is a session, not a command.** The app keeps the caret,
selection and an in-session undo stack; every change runs the existing
`SetText` through `Editor::execute_coalescing` under a key unique to the
session, so the whole session is one history step. New text adds its
layer under the same key; if it is closed empty, the new
`Editor::discard_coalescing` drops the run entirely (no layer, no step,
no redo entry). While a session takes keys the canvas holds egui's
keyboard focus, so `wants_keyboard_input()` silences tool shortcuts and
letters type.

## Consequences

- PSD export still rasterises text (now including runs and boxes);
  editable PSD text (TySh with paragraph and style-run data) can map onto
  these fields later.
- Per-run fonts, kerning tables and IME composition preview (pre-edit
  text) are not modelled; committed IME text is inserted.
- A future GPU or vector text path must use `text_layout` for positions
  so editing and drawing never disagree.
