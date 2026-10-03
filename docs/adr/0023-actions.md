# ADR 0023: Actions (recorded macros) as user-level steps

Status: accepted (first version)

## Context

Photoshop's Actions record a sequence of edits, replay it on any document
and batch it over folders. Lumenply's edits are `Command`s, but commands are
plain Rust structs, not serialisable, and many hold `LayerId`s that mean
nothing in another document. The editor's history keeps document snapshots,
not commands, so "macro recording from history" can't simply re-run history.

## Decision

An **Action** is a named list of **steps** recorded at the level a user
thinks in, serialised as JSON (`crates/core/src/actions.rs`):

- `menu {id}`: a parameter-free registry action (`flatten`, `invert-sel`,
  `rot-cw`, `duplicate-layer`, ...). Only ids in `RECORDABLE_MENU` are
  accepted at playback, so a shared action file can never open a file
  dialog or quit.
- `adjust {adjustment}`: add an adjustment layer with its full settings
  (`Adjustment` is already serde).
- `filter {filter}`: run a filter on the active layer: baked into a pixel
  layer or added as a smart filter on a smart object, as the dialogs do.
- `image-size`, `fit-image` (long edge, never enlarges), `canvas-size`,
  `rotate-canvas`, `trim`: what the dialogs confirm.
- `skipped {what}`: an edit made while recording that can't be recorded
  yet (brush strokes, drags, typing). Playback ignores it; the panel shows
  "not recordable yet: Paint stroke".

Steps **never name layers**: they act on the active layer at replay time.
JSON is `{"version": 1, "actions": [...]}` with `#[serde(tag = "step")]`;
unknown step kinds are load errors naming the action, never panics.

**Playback** runs through an `ActionHost` trait: the app implements
`run_menu` with its registry (`run_menu_action`), `CoreHost` implements the
engine-only subset for the CLI batch and tests. A played action is **one
undo step** ("Action: name"): `Editor::squash_newest` merges the steps it
pushed (history limits are lifted during playback so the count is exact).
On an error, `Editor::rollback_newest` takes the steps back without leaving
a redo entry and the error names the 1-based step that failed.

**Recording** hooks two places only: the registry runner (a recordable id
becomes a `menu` step; ids that open a dialog record nothing, their
dialog's confirmation does) and the dialog confirmations. While recording,
any history entry no hook claimed becomes a `skipped` step, so nothing is
silently lost. An adjustment layer's settings are re-read from the layer
until the next step is recorded, so Properties tweaks right after adding it
land in the step.

User actions live in `actions.json` in the user data dir (next to prefs);
three built-in examples ship in code and are never written to the file.

## Consequences

- Actions survive refactors of command structs: the JSON is user-level.
- New features become recordable by adding a step kind (and a test) rather
  than by making every command serialisable.
- Brush strokes, transforms, crop-tool drags and text are not recordable
  yet; recording them needs coordinate-relative steps (a later ADR).
- `menu` steps whose behaviour needs the app (auto colour reads the
  composite histogram; masks and groups use app layer logic) fail in the
  headless batch with "needs the app" instead of doing something else.
