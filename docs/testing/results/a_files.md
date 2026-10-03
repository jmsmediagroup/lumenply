# A. Documents and files: user-journey results

Ten recorded scenarios (`crates/app/src/uitest/scenarios/a_files.rs`) cover
every journey listed for area A in [user-journeys.md](../user-journeys.md).
Each one ran at 1440×900 and at 900×600, before and after the fixes.

- Before: `<recordings>/a_files/before/<scenario>/session.mp4`
  (900×600 runs: `before/900x600/<scenario>/`). These are main's app code
  with this branch's harness and scenarios.
- After: `<recordings>/a_files/after/<scenario>/session.mp4`
  (900×600 runs: `after/900x600/<scenario>/`).

```sh
cargo run --release -p lumenply-app --features uitest -- --uitest new-document open-files \
  save-and-close quit-with-tabs quit-discarding autosave-recover tabs-and-duplicate \
  image-size canvas-and-rotate export-formats [--size 900x600]
```

`open-files` needs the psd-tools corpus (`scripts/psd_corpus.py fetch`) or
`LUMENPLY_PSD_CORPUS` pointing at a folder that holds
`psd-tools/tests/psd_files/hidden-layer.psd`.

## Journeys

| Scenario | Journey | Result | After video |
| --- | --- | --- | --- |
| new-document | Welcome screen ▸ New: A4 300 ppi preset, a custom size, the 4K preset, a size in cm, Esc to cancel, a Transparent background | fixed | after/new-document/session.mp4 |
| open-files | Open a PNG, a JPEG, a PSD with layers (one hidden), a camera RAW (DNG), a HEIC, a format 1 `.lumen`; reopen an open file; Place an image as a layer and undo it; drop a PNG and a PSD onto the window; a damaged file; Open Recent; six tabs and the Window menu | fixed | after/open-files/session.mp4 |
| save-and-close | Cmd+S (asks once, then saves in place), the unsaved mark through undo, redo and a different edit, Save As, close with unsaved changes (Cancel, Esc, Close without saving, Save and close), Open Recent from the menu and the welcome screen | fixed | after/save-and-close/session.mp4 |
| quit-with-tabs | Quit with three tabs, two unsaved: Cancel, then the window's close button, then save each document in turn (the untitled one through the save panel); both files checked on disk | fixed | after/quit-with-tabs/session.mp4 |
| quit-discarding | Quit with two unsaved: Don't save one, Cancel at the other, then quit and discard | fixed | after/quit-discarding/session.mp4 |
| autosave-recover | Autosave set to 15 s in Preferences; two unsaved documents (one with a file) autosave; crash; relaunch; Recover: both come back unsaved with their pixels, and Save goes to the original file | pass (dialog wording improved) | after/autosave-recover/session.mp4 |
| tabs-and-duplicate | Switch tabs, Image ▸ Duplicate (independent copy, unsaved), close with ×, close the copy with File ▸ Close document | fixed at 1440×900; open at 900×600 (see below) | after/tabs-and-duplicate/session.mp4 |
| image-size | Image Size in pixels (aspect kept), percent (Enter applies), inches with Resample off (ppi changes, pixels stay), centimetres; undo after each; Esc cancels | fixed (the unit list opened behind the dialog) | after/image-size/session.mp4 |
| canvas-and-rotate | Canvas Size anchored top left and centred, Trim transparent, shrink the canvas then Reveal All, rotate 90° both ways and 180°, flip, rotate by 30° (canvas grows to 447 × 374), undo for every step | fixed (a dialog reopened after another one could not be clicked) | after/canvas-and-rotate/session.mp4 |
| export-formats | PNG (alpha kept), JPEG at quality 20 and 95, PDF page size, PSD 8 and 16 bit (layers kept; also checked with psd-tools), OpenRaster, 16-bit PNG and TIFF, OpenEXR (linear 1.0), Export As (JPEG at 50 %, file size shown), a 3D LUT (disabled with a reason until there is an adjustment layer) | pass | after/export-formats/session.mp4 |

All ten pass after the fixes at 1440×900. At 900×600 nine pass. In
`tabs-and-duplicate` the harness can't tell a Window-menu item from the tab
with the same name once the tab strip has scrolled. The app itself works:
`open-files` switches through the Window menu at 900×600.

## Findings

| # | Severity | Finding | Status | Fix |
| --- | --- | --- | --- | --- |
| 1 | blocker | Quitting with several tabs: *Save and quit* saved only the live document, then quit, so other unsaved tabs were lost without a word. The dialog said "The document has unsaved changes" without naming the document. | fixed | f60fb97: quitting asks about each unsaved document in turn ("Save changes to “c.lumen” before quitting?", with how many others are unsaved), with Save / Cancel / Don't save. A cancelled save panel stops the quit. |
| 2 | major | The unsaved mark compared history lengths. Save, undo, then a different edit looked saved (no dot, no title mark, and closing gave no warning). Every edit past the history limit looked saved too. | fixed | f60fb97: `Editor::version()` gives each version a unique number; the app keeps the number at save. Tests: `core … a_version_number_tells_whether_the_document_is_the_one_saved`, `project_io … a_different_edit_after_undo_counts_as_unsaved`, and a scenario check. |
| 3 | major | A dialog shown again after a different dialog (Canvas size ▸ Trim ▸ Canvas size) sat under its own backdrop: visible, but clicks did nothing. | fixed | 0acaad4/42a79a2: `raise_modal` makes the dialog its backdrop's sublayer. Main's selections fix did the same; the merge keeps one mechanism. Test `dialogs … a_dialog_shown_again_after_another_is_on_top_of_its_backdrop`. |
| 4 | major | A combo-box list in a dialog opened a second time (New's Preset, Image Size's unit) opened behind the dialog, so it could not be used. | fixed | 0acaad4: the dialog and backdrop are raised after the dialog is shown, and not while one of its popups is open. Covered by the scenarios new-document and image-size, which fail before. |
| 5 | major | With many tabs (6+ at 1440, 3+ at 900), the tab strip pushed the search box, the **Export** button and the live tab off the window, and there was no other way to reach those documents. | fixed | f60fb97: the tabs scroll sideways inside the room left for Export and the search box, the live tab scrolls into view, and Window lists the open documents. Scenario open-files checks Export is shown and switches through Window. |
| 6 | minor | Opening an image, PSD, ORA or RAW that is already open opened a second copy (dragging a PSD did too). Projects already switched to their tab. | fixed | f60fb97: imports remember their file. Test `project_io … opening_an_open_image_again_comes_back_to_its_tab`. |
| 7 | minor | File ▸ New had no Background contents choice; a transparent document took deleting the Background. | fixed | f60fb97: White / Black / Transparent (kept for the session like the size unit). Test `image_size_ui … new_documents_have_the_chosen_background`. |
| 8 | polish | Duplicate was "Duplicate..." (no dialog follows) and named the copy "a.png copy". | fixed | f60fb97: "Duplicate"; the copy is "a copy" / "poster copy", as in Photoshop (test in `everyday_actions_run_from_the_registry`). |
| 9 | polish | Tab controls were spoken as "×", "+" and "name  ", and the unsaved state was not spoken. | fixed | f60fb97: "Close document", "New document (tab)", "name, unsaved changes". |
| 10 | polish | The close-tab dialog said "This document has unsaved changes" without the name. The recover dialog said "document" for two and didn't say which. | fixed | f60fb97: "Save changes to “x” before closing?". Recover is titled "Recover autosaved work", lists the documents ("Untitled, saved.lumen") and offers "Discard backups". |
| 11 | minor | Canvas Size fills the added area with transparency even on the Background layer; Photoshop uses the background colour and offers a canvas extension colour. | open | Proposal below (changes what ResizeCanvas computes). |
| 12 | minor | New always starts at HD 1920 × 1080. Photoshop remembers the last size and offers the clipboard image's size. | open | Proposal below. |
| 13 | minor | No Image Size (Cmd+Alt+I), Canvas Size (Cmd+Alt+C) or Export As (Cmd+Alt+Shift+W) shortcuts: rebindable chords have no Alt. In-app Ctrl+Q does not quit on Windows or Linux. | open | Needs Alt in `session::Chord`. Shared with H. |
| 14 | polish | Status messages give full paths ("Imported /private/tmp/…/hidden-layer.psd"), and a damaged file shows the codec's text ("image error: Format error decoding Png: Invalid PNG signature"). | open | Show the file name and a plain reason ("is not a valid PNG file"). Many status strings in dialogs.rs and their tests would change. |
| 15 | polish | File ▸ Export ▸ JPEG asks for the file first, then the quality. Photoshop asks for options, then the file. | open | Export As covers JPEG with quality, size and a preview. |
| 16 | polish | At 900×600, tabs scrolled out of view show no hint that more exist. The Window menu lists them all. | open | A small "more tabs" chevron at the strip's edge. |
| 17 | polish | (Other areas) The brush cursor circle is drawn over the tool rail when the pointer is near the canvas edge at high zoom. | open | For D or E. |

Checked and fine: the PNG keeps alpha and has sRGB and pHYs chunks; the JPEG
embeds an ICC profile; the PDF page is the print size; psd-tools reads both
PSDs with their layers and composite; Esc and Enter work in New, Image Size
and the close dialogs; the camera RAW opens through Camera Raw and develops
neutral grey; the HEIC opens through the macOS decoder.

## Harness additions (uitest/session.rs)

- A Close command reaches the app as a close request on the next frame,
  which the app may cancel (as eframe does). `close_window()` is the window's
  close button and `has_quit()` says whether the app quit; settling stops
  quietly once it has.
- `drop_files(paths)`: files dragged over the window, then dropped.
- `crash_and_relaunch()`: the app is replaced with a fresh one on the same
  profile, with no exit code run (autosave crash recovery).
- `wait_until(what, timeout, cond)`: real-time waits, for the timed autosave.
- `key()` no longer fails when the key opens a file panel (Cmd+O, Cmd+S):
  the key is released after the panel is answered.
- Known limit: a File ▸ Export submenu item can't be told from the Export
  button of the same name, so the scenarios use the Export button. It lists
  the same items.

## Proposals (not done)

- **Canvas extension colour** (finding 11): a Canvas Size option (Transparent,
  Background colour, White, Black) that fills the added area on the bottom
  layer when it is named Background. This changes ResizeCanvas's pixels, so
  it needs a decision.
- **New remembers the last size** (finding 12): keep the last Create's size,
  ppi and background in prefs, and add a "Clipboard" preset when the session
  clipboard holds an image. One `App::new_document_dialog()` would replace
  the four `Dialog::New(1920, 1080, 72.0)` call sites.
- **Alt in rebindable chords** (finding 13), for Photoshop's Image Size,
  Canvas Size and Export As keys.
- ROADMAP lines to add: "Quit asks about each unsaved document (A)";
  "Unsaved mark follows editor versions (A)"; "New: background contents (A)";
  "Canvas extension colour (proposal)".

## Not tested, and why

- Real Finder drag-and-drop, macOS Open With and the Dock: the harness
  injects egui's dropped-file events, which is the same path from there on.
- Cmd+Q from the macOS application menu: the system menu is outside egui.
  The window's close button and File ▸ Quit go through the same close
  request.
- Real camera RAW files: the scenario writes a minimal DNG (16-bit RGGB,
  sRGB camera matrix). Vendor formats (CR3, NEF…) were not tried.
- A 16-bit PSD's pixel values at full precision: depth and layers were
  checked, and the composite with psd-tools, but not each 16-bit value.
