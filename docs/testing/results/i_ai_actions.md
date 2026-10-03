# I. AI and automation: user-session results

Tested 2026-10-03 on an M4 Pro (macOS 26, 24 GB) with nine other testers
building and recording at the same time, so every time below is from a busy
machine. Scenarios: `crates/app/src/uitest/scenarios/i_ai_actions.rs`.
Videos: `<recordings>/i_ai/before/` (first runs, before the
fixes; `before/run2/` after the first scenario corrections, with the loading
message already in), `<recordings>/i_ai/after/` (1440 × 900)
and `after/900x600/`.

How to run them:

```sh
cargo build --release -p lumenply-app --features uitest
LUMENPLY_UITEST_PHOTOS=<dir with woman.jpg dog.jpg camera.jpg cat.jpg> \
LUMENPLY_UITEST_PSD=<psd-tools tests/psd_files/blend-and-clipping.psd> \
target/release/lumenply-app --uitest --models-from <dir with mobile-sam/ birefnet-lite/> \
    ai-first-use ai-offline ai-models-prefs ai-object-selection ai-select-subject \
    ai-high-detail ai-remove-background actions-record-play actions-builtin psd-round-trip
```

`--models-from` (or `LUMENPLY_UITEST_MODELS`) links the model files into the
session's scratch profile before launch, never into `~/.lumenply`; the
first-use journeys remove a model in Preferences and download it for real.
The photos are CC0 pictures from Wikimedia Commons, 1920 px wide: "Brunette
woman portrait (Unsplash)", "Earnest dog in the grass (Unsplash)", "Vintage
black kodak camera (Unsplash)", "Ginger long-haired cat (Unsplash)"; plus the
demo photo. Offline is simulated with a proxy that refuses every connection
(`ALL_PROXY`), the session's stand-in for a computer without internet.

## Journeys

All 19 after-sessions pass (10 journeys at 1440 × 900, 9 at 900 × 600;
`ai-models-prefs` was run at 1440 × 900 only, as the 900 × 600 runs of the
other journeys already go through the same page).

| Journey | Result | After video |
|---|---|---|
| `ai-first-use`: Object Selection's first click asks (name, 44.7 MB, source, licence); Cancel; Download, progress, Cancel half way (nothing installed, no `.part` left); Download, **Download in background**, add a layer meanwhile; the waiting click then selects the dog; undo | fixed | `after/ai-first-use/session.mp4`, `after/900x600/ai-first-use/session.mp4` |
| `ai-offline`: Select ▸ Subject without BiRefNet, offline: Download fails, the message, Try again, Cancel; nothing installed, document unchanged | fixed | `after/ai-offline/session.mp4`, `after/900x600/ai-offline/session.mp4` |
| `ai-models-prefs`: Edit ▸ Preferences ▸ AI models lists both models, Remove MobileSAM, Download it from there with progress | fixed | `after/ai-models-prefs/session.mp4` |
| `ai-object-selection`: Magic Wand ▸ Object selection; click the dog (whole dog, 33.9 % of the photo, grass 0 at 4 points); Shift-click adds a grass blade; Alt-click takes the paw out; three history steps, undo twice; a box around the camera on a second photo; Cmd+D | fixed (first-click feedback) | `after/ai-object-selection/session.mp4`, `after/900x600/ai-object-selection/session.mp4` |
| `ai-select-subject`: Select ▸ Subject on the portrait (face, hair, top 1.0; leaves 0.0), undo; again while adding a layer (both land); the cat from the command palette; the demo landscape | pass | `after/ai-select-subject/session.mp4`, `after/900x600/ai-select-subject/session.mp4` |
| `ai-high-detail`: Select Subject on the cat at standard, then with Preferences ▸ AI models ▸ High detail; Save keeps it; the status says "high detail" | new (added) | `after/ai-high-detail/session.mp4`, `after/900x600/ai-high-detail/session.mp4` |
| `ai-remove-background`: Layer ▸ Remove background on the camera: a mask (1.0 on the camera, 0.0 on table and wall), pixels unchanged, background transparent; undo removes the mask; greyed out on a Curves layer with "Select a pixel layer first" | pass | `after/ai-remove-background/session.mp4`, `after/900x600/ai-remove-background/session.mp4` |
| `actions-record-play`: Window ▸ Actions ▸ New; flip, B & W layer, a brush stroke, Image Size 960 px; Stop (3 steps + "not recordable yet: Paint stroke"); play on the cat (960 × 643, B & W, one undo step "Action: Action 1"); undo restores size, layers, pixels; rename by double-click (saved in actions.json); delete with confirmation | fixed | `after/actions-record-play/session.mp4`, `after/900x600/actions-record-play/session.mp4` |
| `actions-builtin`: the palette's action commands; Vintage fade's steps; Rename and Delete greyed out with the right reason; play (3 layers, one undo step), undo; recording Select Subject gives a "not recordable yet" note | fixed | `after/actions-builtin/session.mp4`, `after/900x600/actions-builtin/session.mp4` |
| `psd-round-trip`: open psd-tools' `blend-and-clipping.psd` (13 layers, 4 text); retype the text "none" as "no clip"; hide a layer, rename one, set one to 50 %; Export ▸ Photoshop PSD; close without saving; reopen: same size, layer order, names, visibility, opacity and text. psd-tools reads the export the same way (`no clip` type layer, hidden layer, opacity 128) | pass | `after/psd-round-trip/session.mp4`, `after/900x600/psd-round-trip/session.mp4` |

## Findings

| # | Severity | Finding (step) | Expected | Status | Commit |
|---|---|---|---|---|---|
| 1 | blocker | The first-use dialog, shown again after another modal dialog (Preferences), sat under the modal backdrop: a click on Download did nothing (found by the coordinator's report of the same bug in dialogs.rs; reproduced in a test) | The dialog takes the click | fixed | ad8e894 |
| 2 | major | The first Object Selection click waited 21 s (36–45 s later, with the machine busier) on "Selecting the object…" while MobileSAM loaded and CoreML compiled it (ai-object-selection, step "Click the dog's face") | Say what is happening and that it is a one-time wait | fixed: "Loading the Object Selection model (first click only)…" with elapsed seconds, then "Analysing the image (first click only)…"; picking Object Selection loads the model ahead of the click. The compile itself is a proposal (below) | 1d58444 |
| 3 | major | The first-use download is a modal dialog for its whole length: 45 MB took 118 s and 474 s on the test network, with nothing else possible (ai-first-use, ai-models-prefs) | Keep working while it downloads | fixed: "Download in background" closes the dialog; the progress card and the options bar ("Downloading the model… 34%") show it; a later click replaces the waiting one; a failure reopens the dialog | 1d58444 |
| 4 | major | An action recorded with Image Size (Keep aspect ratio on) replayed the recorded width and height: the 1920 × 1285 cat became 960 × 640, squashed (actions-record-play) | The height follows each image's proportions, as in Photoshop | fixed: the step keeps only the width when Keep aspect ratio was on ("Image size 960 px wide, proportional"); old actions.json files still load | 4456852 |
| 5 | major | At 900 × 600 the Actions panel covers most of the canvas and can't be moved (the stroke step couldn't reach the canvas) | Move it out of the way | fixed: drag it by its header | 4456852 |
| 6 | minor | Download errors showed the library's text: "download failed: https://huggingface.co/…/model.onnx: io: Connection refused (os error 61)", also in the status bar (ai-offline) | Say what went wrong and what to do | fixed: "Couldn't reach huggingface.co: the computer seems to be offline, or a firewall or proxy blocks the connection."; damaged, cut-off and disk-full downloads have their own words; the technical text is the message's tooltip | 1d58444 |
| 7 | minor | Preferences ▸ AI models: the long licence ran under the Remove button and out of the card | Text wraps beside the button | fixed | 1d58444 |
| 8 | minor | Preferences ▸ AI models: both rows' buttons are named "Remove" (and "Download"), so a screen reader (and the harness) can't tell them apart | "Remove MobileSAM", "Download BiRefNet lite", "Cancel the MobileSAM download" | fixed | 1d58444 |
| 9 | minor | Actions panel: Delete on a built-in said "Select one of your actions"; Rename with nothing selected said "Built-in actions keep their names"; Play while recording into the selected action said "Select an action to play" (actions-builtin) | The reason for the case at hand | fixed | 4456852 |
| 10 | minor | The delete confirmation's button is a second "Delete" just under the panel's own Delete | A distinct label | fixed: "Delete action" | 4456852 |
| 11 | minor | The Magic Wand's tooltip, "Magic Wand (W)", doesn't say Object Selection lives there; a Photoshop user looks for an Object Selection tool | Name the modes | fixed: "Magic Wand (W) · Quick Selection · Object Selection (Shift+W)" | 1d58444 |
| 12 | minor | No way to choose BiRefNet's high detail (1024²) | A simple choice | added: Preferences ▸ AI models ▸ "High detail for hair and fur" (tooltip: twice as slow, about 5 GB instead of 3 GB); kept by Save, named in the status bar ("Select subject (high detail, 7.7 s)") | 1d58444 |
| 13 | major | At 900 × 600 the Edit menu runs past the window's bottom: Preferences… can't be reached from it (the command palette's "AI models..." works) | The menu fits or scrolls | open (menus, area H) | — |
| 14 | minor | Select Subject (and the other AI edits) recorded into an action become "not recordable yet: Select subject" | Photoshop records Select Subject | open: proposal | — |
| 15 | minor | Undoing a played action that resized the image leaves the view zoomed to the old fit (99 %, the photo off-screen) | Fit again, as playing does | open | — |
| 16 | minor | Image Size's confirm button is "Apply"; Photoshop and most dialogs here say OK | Consistent naming | open (area A) | — |
| 17 | polish | "Runs on CoreML" in Preferences, though BiRefNet and SAM's decoder run on the CPU | Per model, or "CoreML and CPU" | open | — |
| 18 | polish | When a background download finishes and the waiting click runs, "Autosaved a backup" can replace the "Object selection (22.4 s)" status | The result stays | open | — |
| 19 | polish | The progress card's width changes as its seconds count up | Fixed width | open | — |
| 20 | polish | BiRefNet's licence line reads "MIT: … re-exported to ONNX … by senty-au licence" | Short licence in the row ("MIT"), the full text in the consent dialog | open | — |
| 21 | harness | `menu("File > Export > …")` clicks the toolbar's Export button instead of the File menu's Export; a layer row below the Layers panel's fold is reported "not on screen" instead of scrolled to | — | worked around in the scenarios; for the harness owner | — |

Undo: every journey checks it. AI edits are one step each (Object
selection, Select subject, Remove background), playing an action is one step
("Action: NAME"), and undo returns the selection, mask, size, layers and
pixels checked before.

## Timings and quality (busy machine, 1920 px photos)

| What | Before | After |
|---|---|---|
| MobileSAM download (44.7 MB) | 118 s (first use), 474 s (Preferences) | 56 s and 39 s; the user keeps working |
| First Object Selection click after install (CoreML compiles the encoder) | 21.4 s, "Selecting the object…" | 22–45 s with the machine busier, "Loading the Object Selection model (first click only)… 12 s" |
| Further clicks on the same image | 0.6 s | 1.0–2.2 s under load (status says 0.3–3.0 s) |
| A box on a new image | 0.7 s | 0.9–3.0 s |
| Select Subject (BiRefNet at 768², loaded per run) | 3.7–4.3 s | 4.1–5.3 s |
| Select Subject, high detail (1024²) | — | 7.7–8.6 s |
| Remove Background | 2.1–2.8 s | 3.6–4.7 s |
| Offline failure shown | under 1 s | under 1 s |

The after times are higher only because the machine was busier (other
testers' builds); nothing in the pipelines changed.

Quality, by eye in the videos and at the checked points:

- **Object Selection**: one click on the dog's face took the whole dog,
  ears and legs, and left the grass (coverage 1.0 / 0.0 at all points); the
  front paw is its own region, so Alt-click removes just that. A box around
  the camera selects the camera, strap included, not the table.
- **Select Subject**: the portrait's long straight hair is followed closely,
  shoulders and top included, leaves out; the cat's fur and whiskers are
  inside with a soft edge; on the demo landscape it picks the "AORAKI" title
  text (1.4 % of the image), a reasonable "salient subject" for a photo with
  no subject.
- **High detail** changes 0.75 % of the cat's pixels by more than 0.1 (mean
  difference 0.003), all on the fur's edge: finer strands, at about twice
  the time.
- **Remove Background** on the camera: clean edge, strap kept, the
  background fully transparent; the pixels stay.

## Proposals (not done)

- **First click without the CoreML compile** (engine, ADR 0028): the first
  load after install compiles MobileSAM's encoder for CoreML, 21–45 s on a
  busy machine, every first launch on a machine. Run the encoder on the CPU
  (0.1 s to load, about 0.2 s a run) until a background CoreML compile is
  ready, or compile right after the download, while the user still reads the
  "installed" message. Not done: it is the engine crate's provider logic.
- **Select Subject and Remove Background in actions**: record them as steps
  that run the model during playback. Playback is synchronous and one undo
  step (ADR 0023); an asynchronous step needs the player to wait for a job.
- **Edit menu at 900 × 600**: make long menus scroll or split Edit (area H).
- **Runs on**: list the provider per model in Preferences.

## Not tested, and why

- Pen input, drag-and-drop of files and other windows: not simulated by the
  harness.
- A real network failure half way through a download (the proxy refuses
  from the start), a corrupt download (the checksum message is unit-tested)
  and a full disk.
- The memory guard's refusal: the machine had enough memory for high detail
  each time; the message's wording is in the engine's tests.
- Windows (DirectML) and Linux (CPU): only macOS was available.
- ROADMAP lines proposed for the coordinator:
  - `[x] User-tested AI and automation: 10 journeys at 1440 × 900 and 900 × 600 (docs/testing/results/i_ai_actions.md)`
  - `[ ] Object Selection's first click without the CoreML compile wait (CPU encoder until CoreML is compiled)`
  - `[ ] Record Select Subject / Remove Background in actions`
