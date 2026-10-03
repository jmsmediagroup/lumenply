# User-session tests

The `uitest` harness runs the real Lumenply UI headlessly and drives it the way a
person does: pointer moves, clicks, drags, keys and typed text, aimed at controls by
the names the UI shows. It checks what a user would see or rely on, and records every
session as a video with the pointer and a caption drawn in.

The code is in `crates/app/src/uitest/`. It is compiled only with the `uitest`
feature, so normal builds don't include it or its extra crates.

## Running scenarios

```sh
# All scenarios, recorded to $TMPDIR/lumenply-uitest/<scenario>/
cargo run --release -p lumenply-app --features uitest -- --uitest

# Some of them, somewhere else, Retina-sized
cargo run --release -p lumenply-app --features uitest -- --uitest first-steps adjust-a-photo \
    --out /tmp/sessions --ppp 2 --video-scale 0.5

# The same scenarios as cargo tests (ignored by default; one at a time)
cargo test --release -p lumenply-app --features uitest uitest_ -- --ignored
```

`--uitest --help` lists the options: `--list`, `--size WxH` (default 1440x900),
`--ppp N` (1 or 2), `--fps N` (30), `--every N` (record every Nth frame), `--hold S`
(how long each step's last frame stays on, 0.6 s), `--video-scale X`, `--no-video`,
`--stop-on-fail`, `--models-from DIR` (or `LUMENPLY_UITEST_MODELS`: AI models in
`DIR/<model>/` are linked into the scratch profile before launch, so the session
starts with them installed; under `cargo test` sessions then run the real AI
engine). Two helpers for writing scenarios:

- `--uitest --tree [APP ARGS]` launches (`--demo` opens the demo) and prints every named
  control, with its role and position.
- `--uitest --still out.png [APP ARGS]` saves the rendered window once it is idle. The
  same as `lumenply-app [APP ARGS] --window-size 1440x900 --screenshot out.png` from
  the real window, for comparing the two.

Both take the rest of the command line as the app's arguments, so `--size` and `--ppp`
go before them: `--uitest --ppp 2 --still out.png --demo`.

Under `cargo test`, `LUMENPLY_UITEST_OUT` moves the output and
`LUMENPLY_UITEST_RECORD=0` skips rendering and video.

Each session leaves a folder:

```
<scenario>/
  session.mp4        H.264, constant 30 fps, the UI plus pointer and caption strip
  session.json       every step: description, action, expected, actual, pass/fail,
                     error, wall-clock and video times, frames, screenshot path
  keyframes/NNN-*.png   each step's last frame
  failures/NNN-*.png    the frame where a step failed
  stills/, trees/       from screenshot() and dump_tree()
  files/             the "user's" files: copies, saves (never the real ones)
  profile/           the scratch data folder (prefs, recent files, autosave)
```

## Writing a scenario

Scenarios live in `crates/app/src/uitest/scenarios/`, one file per feature area. A
file declares its scenarios with `scenario_list!` (which also makes each one an ignored
`cargo test`) and writes each as a function of `&mut Session`:

```rust
//! Layers: adding, renaming and hiding them.

use crate::uitest::prelude::*;

scenario_list! {
    "rename-a-layer" => rename_a_layer: "A new layer, renamed by double-clicking it",
}

fn rename_a_layer(s: &mut Session) -> UiResult {
    s.click("New image…")?;
    s.click("Create")?;
    s.describe("Add a layer from the Layer menu");
    s.menu("Layer > New pixel layer")?;
    s.double_click("Layer Layer 2")?;
    s.set_field("Layer name", "Sky")?;
    let names = s.layer_names()?;
    s.check_eq("the layer is renamed", names, vec!["Sky".to_string(), "Background".to_string()])?;
    Ok(())
}
```

A new area is a new file plus two lines in `scenarios/mod.rs`: its `mod` line and its
`SCENARIOS` in `AREAS`. New scenarios in an existing area touch only that file.

Every call is one logged step with a caption. `s.describe("...")` replaces the
automatic caption ("Click “Create”") of the next step with your words. Actions return
`Err` when they can't be done (no such control, greyed out, covered), which ends the
scenario through `?`. Checks return `Ok(false)` on failure and carry on, or `Err` with
`--stop-on-fail` / `Options::on_fail = OnFail::Stop`. Either way the failure is
logged with a screenshot and the session fails.

### Finding controls

Controls are found in the accessibility tree by the name a screen reader would say:
the button's label, the tooltip of an icon button, the name a custom control was
given. `--tree` and `s.dump_tree("label")` list them.

- The exact name wins; otherwise case is ignored. `…` and `...` are the same.
- A trailing `*` matches the start of a name: `"Curve, *"` for `"Curve, 3 points"`.
- When a label and a control share a name, the control wins. When two kinds of
  control do (a slider and its number field), say which: `click_role(Role::Slider, ..)`.
  When the same kind appears twice, the one drawn last (on top) is used;
  `s.within("Properties", |s| s.drag_slider("Opacity", 0.5))` limits the search to
  one panel.
- Layer rows are `"Layer <name>"`, history steps are
  `"History step <n>: <label>"` (0 is the opened state, `"History step 0: Open"`), tools are their
  names ("Brush", "Rectangular Marquee").
- A control must look the same for two frames before it is used (a menu's first frame
  is an invisible sizing pass), and the harness waits up to a dozen frames for one to
  appear.
- Before clicking, the harness moves the pointer there and asks egui what is under it.
  If the control is scrolled out of its panel it scrolls the panel with the wheel, as
  a person would; if something else covers it, the step fails and says what.
- Parts of a control without a name of their own (the eye in a layer row, a point on
  the curve) are reached with `click_offset(name, dx, dy, what)`, `click_in(name, fx,
  fy, what)` and `drag_in(name, from, to, what)`, relative to the named control.

### The API

Actions (all real input events into egui's `RawInput`):

| Call | What the user does |
| --- | --- |
| `click(name)`, `click_role(role, name)`, `click_with(name, "Shift")` | moves there and clicks |
| `double_click`, `right_click`, `hover` (returns the tooltip) | |
| `menu("Layer > New fill layer > Solid color")` | opens a menu-bar menu, hovers submenus, clicks |
| `key("Cmd+Shift+N")`, `key("Esc")` | Cmd is ⌘ on macOS and Ctrl elsewhere |
| `type_text("Sky")` | key presses and text, one character a frame |
| `set_field(name, "50")` | clicks a text or number field, selects all, types, Enter |
| `drag_slider(name, 0.5)` | drags the handle to a fraction of the track |
| `drag(from, to, steps, "Alt")`, `click_at(pos, what)` | screen points |
| `canvas_click((x, y), "")`, `canvas_drag(from, to, steps, "Shift")`, `canvas_stroke(&points, steps)` | document pixels, through the canvas's zoom and pan |
| `scroll(name, dy)` | the wheel over a control |
| `wait_idle()`, `wait_frames(n)` | |

`wait_idle` runs frames until the UI stops asking for an immediate redraw and no
background work is left (AI jobs, export previews, a pending canvas refresh); it fails
after 30 s. Steps already settle for a few frames after their input.

System file panels can't open headlessly. When the app asks for a file, the frame
stays parked inside that request, just as a modal panel would hold it, and the next
step answers it: `choose_file(path)` for an open panel, `save_as(path)` for a save
panel, `cancel_dialog()`. These are logged as "system dialog" steps and shown in the
video as a stand-in panel listing the title, the suggested name and the file types.
Use files under `s.files()` (`s.copy_in(fixture)` copies one there).

The clipboard is the session's own, never the system's: Cmd+C/X/V go through it
(images through the app's clipboard seam, text through egui's output), and
`clipboard_text()`, `set_clipboard_text()` and `clipboard_image_size()` read and set it.

Reading the app (closures run on the UI thread between frames, so they must be
`Send + 'static`; use `move` and owned values):

- `doc(|d| ...)`, `app(|a| ...)`, `layer_names()` (top first, as the panel lists
  them), `history()`, `selection_bounds()`, `selection_at(x, y)`, `pixel(x, y)` (the
  composite, straight sRGB 0–255), `layer_pixel(name, x, y)`, `mask_at(name, x, y)`,
  `find_layer(doc, name)`.
- `text_shown(needle)`: control names and values, and every piece of painted text
  (status bar, canvas overlays, dialogs).
- `node(name)`, `has_node(name)`, `tree()`, `title` (the window title).

Checks: `check(what, ok, expected, actual)`, `check_eq(what, actual, expected)`,
`expect_node`, `expect_enabled`, `expect_disabled(name, reason)` (hovers it and reads
the tooltip), `expect_text`, `expect_doc(what, expected, |d| (ok, actual))`.

Logging: `describe`, `note`, `screenshot(name)`, `dump_tree(label)`.

## How it works

- **The app.** `Session::start` builds the real `App` exactly as `App::launch` does,
  with the theme and accesskit on, on a thread of its own (see below), at 1440×900
  points and 1 or 2 pixels per point. Every frame runs the app's raw-input hook and
  `App::frame` inside `ctx.run`, as eframe does, and keeps the full output: the
  accessibility tree, texture changes, shapes, repaint requests, copied text and
  viewport commands (a Close quits the session).
- **A scratch profile.** Outside `cargo test` the session sets `LUMENPLY_DATA_DIR` (a
  data-folder override `session::data_dir` honours only when it is set) and `HOME`
  to folders inside the session, so the user's prefs, recent files and autosave are
  never read or written. Under `cargo test` the app's per-thread test folder is used.
  Either folder is emptied first, and the session refuses to run if the data folder
  is anything else.
- **The UI thread.** A file panel is modal: the app asks for a path in the middle of a
  frame. So the app lives on its own thread, and the dialog seam
  (`sys_dialog::FileDialog`, a stand-in for `rfd::FileDialog`) sends the request to the
  harness and blocks until it answers. The clipboard seam in `clipboard.rs` works the
  same way. Both are thread-locals installed only on a session's UI thread, so the
  same build behaves normally everywhere else.
- **Time.** Frames run on virtual time at the video's frame rate, so double clicks,
  tooltips and animations behave as they would at 30 fps, however fast the machine.
  Background threads run in real time; `wait_idle` gives them real time to finish.
- **Rendering.** Each frame is tessellated and painted offscreen by egui-wgpu (wgpu 22,
  Metal on macOS) into an RGBA texture that is read back. The window paints with
  egui_glow; both draw the same meshes with dithering into a gamma-space target.
  Compared with the real window's `--screenshot` at 1440×900 (1 pixel per point), the
  welcome screen matches to within 2 levels on every pixel, and the demo document on
  99.87% of pixels (mean difference 0.2 of 255), the rest being a sub-pixel shift of
  the options bar's text.
- **Recording.** The pointer (an arrow, with a ring while a button is down) and a
  caption strip below the UI are drawn into each video frame, not into the app. Frames
  identical to the one before are dropped while waiting; each step's last frame is
  held for `hold_secs`. ffmpeg (`LUMENPLY_FFMPEG`, Homebrew's, or the one on `PATH`)
  encodes H.264 at a constant frame rate. Without a GPU the session still runs and
  checks, with no frames or video.

## Limitations

- One window: egui viewports other than the root, drag-and-drop of files from the
  Finder, and pen pressure are not simulated.
- The accessibility tree is flat, so `within` and the scroll-into-view logic go by
  geometry: a control is "in" a panel when its centre is inside the panel's rectangle.
- Controls with no accessible name can only be reached relative to a named one; adding
  a name (`theme::a11y_name`) helps both screen readers and scenarios.
- macOS's Cmd+V monitor is emulated by setting the flag it sets; egui-winit's
  behaviour (Copy/Cut/Paste events in place of key presses) is reproduced.
