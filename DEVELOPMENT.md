# Lumenply — free photo editor

Rust raster image editor aiming to be a better GIMP: non-destructive by
default, Photoshop-familiar UX, PSD round-trip, fast. The name is
**Lumenply** (brand assets in `img/`); crates are `lumenply-*`, projects
save as `.lumen` (legacy `.nge` still loads).
**Read `ROADMAP.md` before choosing what to work on.** It lists what is done,
what is unverified, and what is next, in priority order.

## Build, run, test

Rust 1.85+ (stable is fine). Linux also needs `libxkbcommon-dev libgl-dev libx11-dev`.

```sh
cargo run --release -p lumenply-app -- --demo  # desktop app with the demo document
cargo run --release -p lumenply-app -- file.psd  # open .lumen / .psd / .ora / images
cargo run --release -p lumenply -- --help      # headless CLI (render, info, export-psd, bench)
cargo test --workspace                       # must stay green
scripts/bundle-macos.sh [outdir]             # Lumenply.app (unsigned) with icon + file types
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                              # rustfmt.toml: max_width 110
```

## Layout

```
crates/tiles   sparse copy-on-write 256×256 tiles, Rect, Affine, Raster; compact 16-bit storage
crates/doc     document model: layer tree (pixel, group, adjustment, filter, text), masks,
               selections, Adjustment and Filter enums
crates/render  CPU compositor (reference path), blend modes, filters, transforms, text rasteriser
crates/graph   the document as a graph of operations (ADR 0025): JSON model, blobs, content
               keys, cached tile evaluation, history, lowering from layer trees
crates/io      PNG/JPEG load+save, native .nge (zip + JSON), PSD import/export
crates/core    Editor (undo/redo, coalescing, history jumping) and every editing Command
crates/cli     `nge` headless front end
crates/app     `nge-app` egui desktop shell (single large main.rs for now)
docs/adr       architecture decisions — read before changing storage, file formats or PSD
```

Dependencies point strictly downward: app/cli → core → render → doc → tiles; io beside them;
graph sits on render (core moves onto it in stage 3 of ADR 0025).

## Rules that keep the design intact

- **Every edit is a `Command`** executed through `Editor::execute` (or
  `execute_coalescing` for slider drags). The UI never mutates the document directly.
  New features = a new command in `crates/core/src/commands.rs` + a test there.
- **Engine crates never depend on a window, GPU or UI toolkit.**
- **Pixels are premultiplied, linear-light** — but **adjustment maths runs on
  gamma-encoded values** (except Exposure; see ADR 0005). Math in `f32`; tiles rest as 16-bit
  (`Tile::compact`) after each command. Never call `tile.pixels()` inside a per-pixel
  loop: for compact tiles it converts the whole tile each call (this caused a real hang).
  Hoist it once per tile.
- **The CPU compositor is the reference.** A future GPU path must match it within
  tolerance, checked by tests.
- `Command::affected()` should return the changed canvas area when cheap to compute;
  the app uses it to redraw only that region.
- Text layers keep a raster cache (`TextLayer::cache`) that is derived state: never
  saved, rebuilt on load and on every edit. A PSD import seeds it with Photoshop's
  own pixels (exact even with missing fonts) until the first edit re-renders it.
- PSD writer changes must be validated with the independent reader:
  `pip install psd-tools` and open the written file (see ADR 0003).
- Every behaviour change gets a numeric test with explicit expected values.

## Gotchas learned the hard way

- **Verify UI changes by looking at them**: `cargo run --release -p lumenply-app -- --demo
  --screenshot /tmp/ui.png` renders a few frames, saves the window and exits —
  no macOS screen-recording permission needed. To reach a state behind a click,
  add `--screenshot-do tok,tok` (palette action ids, plus per-area debug
  tokens in `debug.rs`: `popups:click=File`, `popups:rclick=X:Y`,
  `text:click:X:Y`, `color:fg`, `start:fake-recent`, `layout:tool=…`,
  `layout:layer=background`, `select:rect=X:Y:W:H`, `select:caf`,
  `crop:frame=X0:Y0:X1:Y1`, `guides:add=v:X`,
  `guides:smart-drag=X0:Y0:X1:Y1` (Move-tool drag of the active layer, held
  mid-drag so Smart Guides show), `adj:add=gradient-map`,
  `adj:fill=gradient`, `liquify:demo`, `raw:open=PATH`, `layout:tool=shape`,
  `shape:kind=star`, `shape:draw=X0:Y0:X1:Y1`, `gradient:open|style=…|draw=…`,
  `brush:panel|tip=Name|set=key:value|stroke=X0:Y0:X1:Y1|import=PATH`,
  `text:box=X:Y:W:H|edit|select=A:B|caret=I|commit`, `refine:…` (Select and
  Mask), `retouch:…`, `export-as:…`, `panels:tab=channels|view=red|navigator|info`,
  `puppet:open|pin=X:Y|drag=I:X:Y|ok`, `adjx:open=sh|equalize|replace|match|adjd-<kind>`
  (Image ▸ Adjustments dialogs; `adjx:sh=…`, `adjx:pick=X:Y`, `adjx:ok`),
  `actions:show|record|stop|play=NAME|open=NAME|select=NAME`,
  `ai:fake|fake-installed|fake-offline` (local AI on a fake engine: simple
  colour rules, visible delays), `ai:consent=sam|birefnet`,
  `ai:download=sam|birefnet` (first-use dialog, then its download),
  `ai:fetch=…` (download as Preferences does), `ai:object` (Wand ▸ Object
  Selection), `ai:select=X:Y[:shift|alt]`, `ai:box=X0:Y0:X1:Y1`,
  `ai:subject`, `ai:remove-bg`, `ai:prefs` (Preferences ▸ AI models),
  `ai:wait` (blocks until every AI job has finished and landed),
  `size:unit=in|cm|mm|px|pct|resample=on|off|width=V|res=V|new=N|doc-ppi=V`
  (Image Size / New; end dialog shots with `popups:sleep=400,popups:wait`),
  `proof-colors` / `gamut-warning` (palette ids: View ▸ Proof Colors / Gamut
  Warning, display-only CMYK soft proof); drags as
  `popups:press=X:Y,popups:move=X:Y,popups:release=X:Y`, typing as
  `popups:type=…`/`popups:key=Shift+W`; tokens that toggle
  view prefs such as `rulers`/`grid` save prefs — another reason for a scratch
  `HOME`), and
  `--window-size 960x640` for narrow layouts. All `--screenshot-do` tokens run in
  one frame (`popups:sleep` only pauses), so anything a dialog does on a later
  frame, such as `adjx:ok`, lands after every token: put it last. Run screenshots with `HOME`
  pointed at a scratch folder so recent files, prefs and the autosave of the
  real user are never read or written. The dark theme is forced via
  `ctx.set_theme(ThemePreference::Dark)`; without it eframe follows the OS and
  repaints everything in egui's stock light palette on a light-mode system.
- **Menus, palette and keys share one action registry** (`palette.rs`:
  `action_block` says why an action can't run, `action_keys` formats its
  shortcut for this platform, `run_menu_action` runs it). Add new actions
  there, then reference the id from a menu with `self.act(ui, label, id)`.
  Popups go through the theme helpers (`button_menu`, `context_menu`,
  `popup_style`) so they size to content and never wrap.
- The app has a no-document state (`App::no_doc`, the welcome screen):
  `run`/`run_coalescing` refuse edits and `action_block` greys out
  document-only actions. Tests can build the whole app with `App::launch`;
  under `cfg(test)` `session::data_dir` is a temp folder.
- Several git worktrees sharing one `CARGO_TARGET_DIR` share cargo units, so
  a "fresh" build can silently link another worktree's code. Give the
  worktree's sources a future mtime before building
  (`find crates -name '*.rs' -exec touch -t 203001010000 {} +`).

- egui is 0.29: `drag_stopped()`, `id_salt()`, `ComboBox::from_label`. Record a drag's start
  position yourself on `drag_started` — `press_origin()` is already cleared on release.
- Shortcuts must be ignored while a text field has focus (`ctx.wants_keyboard_input()`).
- accesskit is on: every custom-painted or separately-labelled control needs a spoken
  name (`resp.widget_info(..)` or `theme::a11y_name`); floating areas use
  `theme::BACKDROP_SENSE` so they swallow clicks without being Tab stops. `a11y_tests`
  in main.rs render the whole UI (`App::frame`) and fail on any unnamed control.
- `Response::has_focus()` is false whenever the window itself is unfocused (as in
  headless screenshot runs); check `ctx.memory(|m| m.has_focus(id))` instead.
- **The graph must render what the layer tree renders**: `lumenply graph FILE --check`
  lowers a document, renders it both ways and fails on any difference. Run it over the
  corpus after touching `crates/graph` ops or the compositor.
- **PSD fidelity is measured, not guessed**: `scripts/psd_corpus.py` renders the
  public psd-tools and ag-psd test files (~480 real Photoshop files) with the CLI
  and compares each with the composite Photoshop saved inside it. Run it before
  and after any PSD reader or compositor change and compare the two result files.
- Tests that run app frames must not autosave (see the `launch` helper in `a11y_tests`):
  a backup left in the test data folder opens the Recover dialog in every later test.
- Pen pressure (`crates/app/src/pen.rs`): Windows pens arrive as egui touches with `force`;
  macOS reads `NSEvent.pressure` through an AppKit local event monitor (installed in
  `App::new`); Linux has no source yet. `App::pen_point` routes it to `StrokePoint`
  pressure (dab size) and opacity per the Brush bar's two pen toggles (prefs).
- The bundled font is DejaVu Sans in `crates/render/fonts/` (Bitstream Vera licence).
- The `--demo` document is built in `crates/app/src/demo.rs` from a CC0 photo
  (`crates/app/assets/NOTICE.md`); the CLI keeps the synthetic `lumenply_core::demo`.

## Working style

Small, verified steps: build → test → run the app and try the feature → commit.
Update `ROADMAP.md` checkboxes and add an ADR for any decision that is hard to reverse.
