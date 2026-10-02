# NGE — next-gen open image editor

Rust raster image editor aiming to be a better GIMP: non-destructive by
default, Photoshop-familiar UX, PSD round-trip, fast. Working name "NGE".
**Read `ROADMAP.md` before choosing what to work on.** It lists what is done,
what is unverified, and what is next, in priority order.

## Build, run, test

Rust 1.85+ (stable is fine). Linux also needs `libxkbcommon-dev libgl-dev libx11-dev`.

```sh
cargo run --release -p nge-app -- --demo     # desktop app with the demo document
cargo run --release -p nge-app -- file.psd   # open .nge / .psd / .png / .jpg
cargo run --release -p nge -- --help         # headless CLI (render, info, export-psd, bench)
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
- **Pixels are premultiplied, linear-light.** Math in `f32`; tiles rest as 16-bit
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

- egui is 0.29: `drag_stopped()`, `id_salt()`, `ComboBox::from_label`. Record a drag's start
  position yourself on `drag_started` — `press_origin()` is already cleared on release.
- Shortcuts must be ignored while a text field has focus (`ctx.wants_keyboard_input()`).
- Pen pressure is not wired: the app passes pressure 1.0 to every stroke point.
- The bundled font is DejaVu Sans in `crates/render/fonts/` (Bitstream Vera licence).

## Working style

Small, verified steps: build → test → run the app and try the feature → commit.
Update `ROADMAP.md` checkboxes and add an ADR for any decision that is hard to reverse.
