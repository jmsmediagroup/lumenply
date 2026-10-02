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
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                              # rustfmt.toml: max_width 110
```

## Layout

```
crates/tiles   sparse copy-on-write 256×256 tiles, Rect, Affine, Raster; compact 16-bit storage
crates/doc     document model: layer tree (pixel, group, adjustment, filter, text), masks,
               selections, Adjustment and Filter enums
crates/render  CPU compositor (reference path), blend modes, filters, transforms, text rasteriser
crates/io      PNG/JPEG load+save, native .nge (zip + JSON), PSD import/export
crates/core    Editor (undo/redo, coalescing, history jumping) and every editing Command
crates/cli     `nge` headless front end
crates/app     `nge-app` egui desktop shell (single large main.rs for now)
docs/adr       architecture decisions — read before changing storage, file formats or PSD
```

Dependencies point strictly downward: app/cli → core → render → doc → tiles; io beside them.

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
  saved, rebuilt on load and on every edit.
- PSD writer changes must be validated with the independent reader:
  `pip install psd-tools` and open the written file (see ADR 0003).
- Every behaviour change gets a numeric test with explicit expected values.

## Gotchas learned the hard way

- **Verify UI changes by looking at them**: `cargo run --release -p lumenply-app -- --demo
  --screenshot /tmp/ui.png` renders a few frames, saves the window and exits —
  no macOS screen-recording permission needed. To reach a state behind a click,
  add `--screenshot-do tok,tok` (palette action ids, plus per-area debug
  tokens in `debug.rs`: `popups:click=File`, `popups:rclick=X:Y`,
  `text:click:X:Y`, `color:fg`, `start:fake-recent`, `layout:tool=…`), and
  `--window-size 960x640` for narrow layouts. Run screenshots with `HOME`
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
- Pen pressure is not wired: the app passes pressure 1.0 to every stroke point.
- The bundled font is DejaVu Sans in `crates/render/fonts/` (Bitstream Vera licence).
- The `--demo` document is built in `crates/app/src/demo.rs` from a CC0 photo
  (`crates/app/assets/NOTICE.md`); the CLI keeps the synthetic `lumenply_core::demo`.

## Working style

Small, verified steps: build → test → run the app and try the feature → commit.
Update `ROADMAP.md` checkboxes and add an ADR for any decision that is hard to reverse.
