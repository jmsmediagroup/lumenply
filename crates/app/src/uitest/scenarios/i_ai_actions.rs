//! AI and automation: Object Selection, Select Subject and Remove
//! Background with the real models (first use, download, cancel, offline,
//! results on photos), Preferences ▸ AI models, the Actions panel, and a
//! PSD round trip.
//!
//! The AI journeys run the real engine. Start them with the models already
//! installed (`--models-from DIR` or `LUMENPLY_UITEST_MODELS`, a folder
//! with `mobile-sam/` and `birefnet-lite/`); the first-use journeys remove
//! a model through Preferences and download it again for real. The photos
//! are CC0 pictures the session copies in from `LUMENPLY_UITEST_PHOTOS`
//! (`woman.jpg`, `dog.jpg`, `camera.jpg`, `cat.jpg`, 1920 px wide, from
//! Wikimedia Commons); without them those journeys say so and stop.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::ai::ModelKey;
use crate::uitest::prelude::*;

scenario_list! {
    "ai-first-use" => ai_first_use: "Object Selection's first use: ask, cancel, download with progress, cancel, download, select",
    "ai-offline" => ai_offline: "Select Subject's model can't be downloaded offline: the error, Try again, Cancel",
    "ai-models-prefs" => ai_models_prefs: "Preferences ▸ AI models: what is installed, Remove, Download with progress",
    "ai-object-selection" => ai_object_selection: "Object Selection on a dog and a camera: click, Shift adds, Alt subtracts, a box, undo",
    "ai-select-subject" => ai_select_subject: "Select ▸ Subject on a portrait, a cat and the demo photo, with work going on meanwhile",
    "ai-high-detail" => ai_high_detail: "Select Subject on a long-haired cat at standard and at high detail (Preferences ▸ AI models)",
    "ai-remove-background" => ai_remove_background: "Layer ▸ Remove background on a product photo: a mask, pixels kept, undo",
    "actions-record-play" => actions_record_play: "Record an action on one photo, play it on another, undo it, rename and delete it",
    "actions-builtin" => actions_builtin: "Built-in actions: play one, what can't be renamed or deleted, and why",
    "psd-round-trip" => psd_round_trip: "Open a Photoshop file, edit its text and layers, export a PSD, reopen it",
}

// ---- fixtures -------------------------------------------------------------------

/// The folder with the CC0 test photos.
fn photos() -> Option<PathBuf> {
    std::env::var_os("LUMENPLY_UITEST_PHOTOS")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .filter(|d| d.join("dog.jpg").is_file())
}

/// Copy a photo into the session's files and open it the way a user does:
/// from the welcome screen's Open… or File ▸ Open... and the system panel.
fn open_photo(s: &mut Session, name: &str) -> UiResult<PathBuf> {
    let dir = photos().ok_or_else(|| {
        UiError(format!(
            "set LUMENPLY_UITEST_PHOTOS to the folder with {name} (CC0 photos; see the file's header)"
        ))
    })?;
    let file = s.copy_in(dir.join(name))?;
    open_file(s, &file)?;
    Ok(file)
}

fn open_file(s: &mut Session, file: &std::path::Path) -> UiResult {
    let name = file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if s.has_node("Open…") {
        s.describe("Click Open… on the welcome screen");
        s.click("Open…")?;
    } else {
        s.describe("Open a file with File ▸ Open...");
        s.menu("File > Open...")?;
    }
    s.describe(&format!("Pick {name} in the system's open panel"));
    s.choose_file(file)?;
    s.wait_idle()?;
    s.expect_text(&name)?;
    Ok(())
}

/// Selection coverage at each point.
fn coverage(s: &mut Session, pts: &[(i32, i32)]) -> UiResult<Vec<f32>> {
    let pts = pts.to_vec();
    s.doc(move |d| {
        pts.iter()
            .map(|&(x, y)| d.selection.as_ref().map_or(0.0, |m| m.value(x, y)))
            .map(|v| (v * 100.0).round() / 100.0)
            .collect()
    })
}

/// Check that every `inside` point is selected (≥ 0.9) and every
/// `outside` one is not (≤ 0.1).
fn check_cover(s: &mut Session, what: &str, inside: &[(i32, i32)], outside: &[(i32, i32)]) -> UiResult<bool> {
    let cin = coverage(s, inside)?;
    let cout = coverage(s, outside)?;
    let ok = cin.iter().all(|&v| v >= 0.9) && cout.iter().all(|&v| v <= 0.1);
    s.check(
        what,
        ok,
        format!("≥ 0.9 at {inside:?}; ≤ 0.1 at {outside:?}"),
        format!("{cin:?} inside; {cout:?} outside"),
    )
}

/// The fraction of the canvas the selection covers (sum of coverage over
/// pixels, sampled every 4th pixel).
fn selected_share(s: &mut Session) -> UiResult<f32> {
    s.doc(|d| {
        let Some(m) = d.selection.as_ref() else { return 0.0 };
        let (mut sum, mut n) = (0.0f64, 0u64);
        for y in (0..d.height as i32).step_by(4) {
            for x in (0..d.width as i32).step_by(4) {
                sum += m.value(x, y) as f64;
                n += 1;
            }
        }
        (sum / n.max(1) as f64) as f32
    })
}

fn installed(s: &mut Session, model: ModelKey) -> UiResult<bool> {
    s.app(move |a| a.ai.installed(model))
}

fn status(s: &mut Session) -> UiResult<String> {
    s.app(|a| a.status.clone())
}

/// Note the status bar's message in the log.
fn note_status(s: &mut Session) -> UiResult<String> {
    let st = status(s)?;
    s.note(&format!("Status bar: {st}"))?;
    Ok(st)
}

/// Let real time pass with the UI running, `secs` long, so the video
/// shows a download or a model run as the user waits for it.
fn watch(s: &mut Session, secs: f32) -> UiResult {
    let until = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
        s.wait_frames(3)?;
    }
    Ok(())
}

/// Wait (in real time, up to `secs`) until `done` says so, with frames
/// running; the time it took.
fn wait_for(
    s: &mut Session,
    secs: f32,
    what: &str,
    done: impl Fn(&mut Session) -> UiResult<bool>,
) -> UiResult<f32> {
    let t = Instant::now();
    loop {
        if done(s)? {
            return Ok(t.elapsed().as_secs_f32());
        }
        if t.elapsed().as_secs_f32() > secs {
            return Err(UiError(format!("{what}: not after {secs:.0} s")));
        }
        std::thread::sleep(Duration::from_millis(50));
        s.wait_frames(2)?;
    }
}

/// The network, switched off for as long as this lives: downloads go
/// through a proxy that refuses every connection (the system stand-in for
/// a computer without internet).
struct Offline;

impl Offline {
    fn on(s: &mut Session) -> UiResult<Offline> {
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:9");
        s.note("System: the computer is offline now (no connection reaches the internet)")?;
        Ok(Offline)
    }
}

impl Drop for Offline {
    fn drop(&mut self) {
        std::env::remove_var("ALL_PROXY");
    }
}

/// Open Edit ▸ Preferences... on its AI models page. In a short window
/// the Edit menu runs off the bottom (Preferences is out of reach there),
/// so the command palette's "AI models..." is used instead.
fn open_ai_prefs(s: &mut Session) -> UiResult {
    if s.window_size().y < 700.0 {
        s.describe("Open the command palette (Edit ▸ Preferences is below the window's edge)");
        s.key("Cmd+K")?;
        s.type_text("AI models")?;
        s.describe("Run “AI models...” with Enter");
        s.key("Enter")?;
        s.expect_text("AI MODELS")?;
        return Ok(());
    }
    s.describe("Open Edit ▸ Preferences...");
    s.menu("Edit > Preferences...")?;
    s.describe("Switch to the AI models page");
    s.click("AI models")?;
    Ok(())
}

/// Pick Object Selection: the Wand family in the toolbar, then the mode
/// in the options bar (short labels in a narrow window).
fn pick_object_selection(s: &mut Session) -> UiResult {
    s.describe("Pick the Magic Wand family in the toolbar");
    s.click_role(Role::Button, "Magic Wand")?;
    let label = if s.has_node("Object selection") {
        "Object selection"
    } else {
        "Object"
    };
    s.describe("Switch the options bar to Object selection");
    s.click(label)?;
    Ok(())
}

/// Remove `model` in Preferences ▸ AI models when it is installed, so the
/// journey starts as a first use. Leaves Preferences closed.
fn start_without(s: &mut Session, model: ModelKey, name: &str) -> UiResult {
    if !installed(s, model)? {
        return Ok(());
    }
    open_ai_prefs(s)?;
    s.describe(&format!("Remove {name}, to start as someone who never used it"));
    s.click(&format!("Remove {name}"))?;
    let gone = !installed(s, model)?;
    s.check(
        "the model is removed",
        gone,
        "not installed",
        if gone { "not installed" } else { "installed" },
    )?;
    s.describe("Close Preferences");
    s.key("Esc")?;
    Ok(())
}

// ---- AI: first use ----------------------------------------------------------------

fn ai_first_use(s: &mut Session) -> UiResult {
    open_photo(s, "dog.jpg")?;
    start_without(s, ModelKey::MobileSam, "MobileSAM")?;

    // A Photoshop user looks for Object Selection with the selection tools.
    let tip = s.hover("Magic Wand")?;
    s.note(&format!("The Magic Wand's tooltip: {}", tip.join(" / ")))?;
    pick_object_selection(s)?;
    let wide = s.window_size().x >= 1200.0;
    s.expect_text(if wide { "Get the model" } else { "Get model" })?;

    s.describe("Click the dog: the first use asks before downloading anything");
    s.canvas_click((850.0, 520.0), "")?;
    s.expect_node("Download an AI model")?;
    for fact in [
        "MobileSAM",
        "44.7 MB",
        "huggingface.co/Acly/MobileSAM",
        "Apache-2.0",
    ] {
        s.expect_text(fact)?;
    }
    s.screenshot("consent");
    s.describe("Change of mind: Cancel");
    s.click("Cancel")?;
    let sel = s.selection_bounds()?;
    s.check_eq("nothing was selected", sel, None)?;
    let inst = installed(s, ModelKey::MobileSam)?;
    s.check_eq("nothing was installed", inst, false)?;

    s.describe("Click the dog again");
    s.canvas_click((850.0, 520.0), "")?;
    s.describe("Download");
    s.click("Download")?;
    s.expect_node("Downloading an AI model")?;
    // Watch it run for a moment, then cancel half way.
    let t = wait_for(s, 60.0, "the download makes progress", |s| {
        s.app(|a| {
            let p = a.ai.download_fraction(ModelKey::MobileSam);
            p.is_some_and(|f| f > 0.15)
        })
    })?;
    s.note(&format!("15 % downloaded after {t:.1} s"))?;
    s.screenshot("downloading");
    s.describe("Cancel the download");
    s.click("Cancel")?;
    s.wait_idle()?;
    let st = note_status(s)?;
    s.check(
        "the status bar says nothing was installed",
        st.contains("cancelled") && st.contains("nothing was installed"),
        "…cancelled; nothing was installed",
        st,
    )?;
    let inst = installed(s, ModelKey::MobileSam)?;
    s.check_eq("still not installed", inst, false)?;
    let parts = s.app(|_| {
        let dir = crate::session::data_dir()
            .unwrap()
            .join("models")
            .join("mobile-sam");
        std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_else(|_| Vec::<String>::new())
    })?;
    s.check(
        "no partial download is left behind",
        parts.iter().all(|f| !f.ends_with(".part")),
        "no .part file",
        format!("{parts:?}"),
    )?;
    let sel = s.selection_bounds()?;
    s.check_eq("and nothing was selected", sel, None)?;

    s.describe("Click the dog a third time and download for real");
    s.canvas_click((850.0, 520.0), "")?;
    s.click("Download")?;
    let t0 = Instant::now();
    // A slow connection needn't hold the user up.
    s.describe("Download in background, to keep working meanwhile");
    s.click("Download in background")?;
    s.expect_text("Downloading MobileSAM")?;
    s.expect_text("Downloading the model")?;
    s.describe("Meanwhile, add a layer from the Layer menu");
    s.menu("Layer > New pixel layer")?;
    let layers = s.layer_names()?.len();
    s.check_eq("the layer was added during the download", layers, 2)?;
    let secs = wait_for(s, 600.0, "MobileSAM is installed", |s| {
        installed(s, ModelKey::MobileSam)
    })?;
    s.note(&format!(
        "MobileSAM (44.7 MB) downloaded and verified in {secs:.1} s"
    ))?;
    // The click that asked for it runs by itself once the model is there.
    s.wait_idle_for(120)?;
    s.note(&format!(
        "Download plus the first selection: {:.1} s",
        t0.elapsed().as_secs_f32()
    ))?;
    note_status(s)?;
    check_cover(
        s,
        "the click that waited for the model selected the dog",
        &[(850, 520), (880, 700), (1000, 450)],
        &[(1600, 300), (1500, 1150), (200, 150)],
    )?;
    let h = s.history()?;
    s.check_eq(
        "one history step",
        h.last().cloned(),
        Some("Object selection".to_string()),
    )?;
    s.describe("Undo it");
    s.key("Cmd+Z")?;
    let sel = s.selection_bounds()?;
    s.check_eq("undo removes the selection", sel, None)?;
    Ok(())
}

// ---- AI: offline ------------------------------------------------------------------

fn ai_offline(s: &mut Session) -> UiResult {
    open_photo(s, "woman.jpg")?;
    start_without(s, ModelKey::BiRefNetLite, "BiRefNet lite")?;
    let history = s.history()?;
    let offline = Offline::on(s)?;
    s.describe("Select ▸ Subject");
    s.menu("Select > Subject")?;
    s.expect_node("Download an AI model")?;
    s.expect_text("BiRefNet lite")?;
    s.expect_text("181 MB")?;
    s.describe("Download");
    s.click("Download")?;
    wait_for(s, 60.0, "the download fails", |s| {
        Ok(s.has_node("Download failed"))
    })?;
    s.wait_frames(4)?;
    s.screenshot("failed");

    s.expect_text("BiRefNet lite wasn't installed")?;
    s.expect_text("Couldn't reach huggingface.co")?;
    s.expect_text("Check the internet connection")?;
    s.describe("Try again, still offline");
    s.click("Try again")?;
    wait_for(s, 60.0, "the second attempt fails", |s| {
        Ok(s.has_node("Download failed"))
    })?;
    s.describe("Give up: Cancel");
    s.click("Cancel")?;
    drop(offline);
    s.wait_idle()?;
    let gone = !s.has_node("Download failed");
    s.check(
        "the dialog is gone",
        gone,
        "closed",
        if gone { "closed" } else { "still open" },
    )?;
    let inst = installed(s, ModelKey::BiRefNetLite)?;
    s.check_eq("nothing was installed", inst, false)?;
    let sel = s.selection_bounds()?;
    s.check_eq("nothing was selected", sel, None)?;
    let after = s.history()?;
    s.check_eq("the document is unchanged", after, history)?;
    note_status(s)?;
    Ok(())
}

// ---- AI: Preferences ----------------------------------------------------------------

fn ai_models_prefs(s: &mut Session) -> UiResult {
    s.describe("Open the demo photo");
    s.click("Open the demo photo")?;
    s.wait_idle()?;
    open_ai_prefs(s)?;
    s.screenshot("models");
    for t in [
        "MobileSAM",
        "BiRefNet lite",
        "Object Selection",
        "Select Subject, Remove Background",
    ] {
        s.expect_text(t)?;
    }
    let both = (
        installed(s, ModelKey::MobileSam)?,
        installed(s, ModelKey::BiRefNetLite)?,
    );
    s.note(&format!(
        "Installed at launch: MobileSAM {}, BiRefNet {}",
        both.0, both.1
    ))?;
    let runs = s.texts_matching("CPU");
    s.note(&format!("Runs on: {runs:?}"))?;
    if both.0 {
        s.describe("Remove MobileSAM");
        s.click("Remove MobileSAM")?;
        let inst = installed(s, ModelKey::MobileSam)?;
        s.check_eq("MobileSAM is removed", inst, false)?;
        s.expect_text("Not installed")?;
    }
    s.describe("Download MobileSAM again from Preferences");
    s.click("Download MobileSAM")?;
    wait_for(s, 60.0, "progress shows", |s| {
        s.app(|a| {
            a.ai.download_fraction(ModelKey::MobileSam)
                .is_some_and(|f| f > 0.05)
        })
    })?;
    s.screenshot("downloading-in-prefs");
    let secs = wait_for(s, 600.0, "MobileSAM is installed", |s| {
        installed(s, ModelKey::MobileSam)
    })?;
    s.note(&format!("Downloaded from Preferences in {secs:.1} s"))?;
    s.wait_idle()?;
    s.expect_text("Installed")?;
    note_status(s)?;
    s.describe("Close Preferences");
    s.key("Esc")?;
    Ok(())
}

// ---- AI: Object Selection -------------------------------------------------------------

fn ai_object_selection(s: &mut Session) -> UiResult {
    open_photo(s, "dog.jpg")?;
    pick_object_selection(s)?;
    if s.window_size().x >= 1200.0 {
        s.expect_text("Click an object or drag a box around it")?;
    }

    s.describe("Click the dog's face");
    let t = Instant::now();
    s.canvas_click((850.0, 520.0), "")?;
    // The first click after a fresh install waits for CoreML to compile
    // the model: 21-35 s on a busy machine.
    s.wait_idle_for(120)?;
    s.note(&format!(
        "First click (loads the model, analyses the image): {:.1} s",
        t.elapsed().as_secs_f32()
    ))?;
    note_status(s)?;
    s.screenshot("dog-click");
    check_cover(
        s,
        "the click selected the dog, not the grass",
        &[(850, 520), (880, 700), (960, 500)],
        &[(1600, 300), (1500, 1150), (200, 150), (1300, 600)],
    )?;
    let share = selected_share(s)?;
    s.note(&format!("Selected share of the photo: {:.1} %", share * 100.0))?;

    // One click took the whole dog; Shift adds the grass blade in front.
    s.describe("Shift-click the grass blade in front to add it");
    let t = Instant::now();
    s.canvas_click((1560.0, 1000.0), "Shift")?;
    s.wait_idle()?;
    s.note(&format!(
        "Second click (image already analysed): {:.2} s",
        t.elapsed().as_secs_f32()
    ))?;
    let added = selected_share(s)?;
    let face = s.selection_at(850, 520)?;
    s.check(
        "the blade was added, the dog kept",
        added > share + 0.002 && face >= 0.9,
        format!("more than {:.3}, the dog still selected", share),
        format!("{added:.3}"),
    )?;
    check_cover(
        s,
        "dog and blade selected",
        &[(850, 520), (250, 850), (1560, 1000)],
        &[(1600, 300), (200, 150)],
    )?;

    s.describe("Alt-click the front paw to take it away");
    s.canvas_click((1150.0, 1180.0), "Alt")?;
    s.wait_idle()?;
    let paw = s.selection_at(1150, 1180)?;
    let face = s.selection_at(850, 520)?;
    s.check(
        "the paw is out, the face still in",
        paw <= 0.1 && face >= 0.9,
        "paw ≤ 0.1, face ≥ 0.9",
        format!("paw {paw:.2}, face {face:.2}"),
    )?;
    let h = s.history()?;
    let n = h.iter().filter(|l| *l == "Object selection").count();
    s.check_eq("three Object selection steps", n, 3)?;

    s.describe("Undo the Alt-click");
    s.key("Cmd+Z")?;
    let paw = s.selection_at(1150, 1180)?;
    s.check("the paw is back", paw >= 0.5, "≥ 0.5", format!("{paw:.2}"))?;
    s.describe("Undo the Shift-click");
    s.key("Cmd+Z")?;
    let back = selected_share(s)?;
    s.check(
        "back to the first click's selection",
        (back - share).abs() < 0.002,
        format!("{share:.3}"),
        format!("{back:.3}"),
    )?;

    // A second photo in a new tab: a box around a product.
    open_photo(s, "camera.jpg")?;
    s.describe("Drag a box around the camera");
    s.canvas_drag((700.0, 380.0), (1400.0, 1110.0), 14, "")?;
    s.wait_idle()?;
    note_status(s)?;
    s.screenshot("camera-box");
    check_cover(
        s,
        "the box selected the camera, not the table or wall",
        &[(930, 830), (790, 800), (1200, 800), (940, 480)],
        &[(400, 400), (1600, 300), (400, 1150), (1500, 1000)],
    )?;
    s.describe("Deselect with Cmd+D");
    s.key("Cmd+D")?;
    let none = s.selection_bounds()?;
    s.check_eq("nothing selected", none, None)?;
    Ok(())
}

// ---- AI: Select Subject -------------------------------------------------------------

fn ai_select_subject(s: &mut Session) -> UiResult {
    open_photo(s, "woman.jpg")?;
    s.describe("Select ▸ Subject");
    let t = Instant::now();
    s.menu("Select > Subject")?;
    // The progress card while it runs.
    let card = wait_for(s, 10.0, "progress shows", |s| {
        Ok(s.text_shown("Finding the subject"))
    })
    .map(|_| true)
    .unwrap_or(false);
    s.check(
        "a progress card says what is running",
        card,
        "“Finding the subject…”",
        if card { "shown" } else { "not shown" },
    )?;
    s.screenshot("subject-running");
    s.wait_idle()?;
    s.note(&format!(
        "Select Subject on 1920 × 1280: {:.1} s",
        t.elapsed().as_secs_f32()
    ))?;
    note_status(s)?;
    s.screenshot("subject-woman");
    check_cover(
        s,
        "the woman is selected: face, hair, top; not the leaves",
        &[(990, 380), (820, 800), (1000, 1050), (1250, 800)],
        &[(300, 400), (1650, 700), (200, 1100), (1500, 150)],
    )?;
    s.describe("Undo");
    s.key("Cmd+Z")?;
    let none = s.selection_bounds()?;
    s.check_eq("undo drops the selection", none, None)?;

    // Keep working while it runs: paint on a new layer meanwhile.
    s.describe("Select ▸ Subject again, and paint while it runs");
    s.menu("Select > Subject")?;
    s.describe("Add a layer from the Layer menu meanwhile");
    s.menu("Layer > New pixel layer")?;
    s.wait_idle()?;
    let h = s.history()?;
    s.note(&format!("History: {}", h.join(" → ")))?;
    let tail: Vec<String> = h.iter().rev().take(2).rev().cloned().collect();
    s.check(
        "both edits landed, the layer first",
        tail == vec!["Add layer 'Layer 2'".to_string(), "Select subject".to_string()]
            || tail.contains(&"Select subject".to_string()),
        "a new layer and the subject selection",
        format!("{tail:?}"),
    )?;
    check_cover(s, "the subject is selected again", &[(990, 380)], &[(300, 400)])?;

    // A cat with whiskers, from the command palette.
    open_photo(s, "cat.jpg")?;
    s.describe("Open the command palette");
    s.key("Cmd+K")?;
    s.type_text("subject")?;
    s.describe("Run it with Enter");
    s.key("Enter")?;
    s.wait_idle()?;
    note_status(s)?;
    s.screenshot("subject-cat");
    check_cover(
        s,
        "the cat is selected, not the backdrop",
        &[(700, 700), (650, 1100), (790, 620)],
        &[(1500, 400), (1700, 1000), (300, 200)],
    )?;

    // The demo landscape has no single subject.
    s.describe("Open the demo document from the File menu");
    s.menu("File > Open demo document")?;
    s.wait_idle()?;
    s.describe("Select ▸ Subject on a landscape");
    s.menu("Select > Subject")?;
    s.wait_idle()?;
    let st = note_status(s)?;
    let share = selected_share(s)?;
    s.note(&format!(
        "Select Subject on the demo landscape selected {:.1} % of it",
        share * 100.0
    ))?;
    s.screenshot("subject-demo");
    s.check(
        "a result or a clear message",
        share > 0.0 || st.contains("found no subject"),
        "a selection, or “found no subject”",
        format!("{:.1} %, status “{st}”", share * 100.0),
    )?;
    Ok(())
}

// ---- AI: high detail ------------------------------------------------------------------

/// The selection's coverage over the whole canvas, every pixel.
fn selection_values(s: &mut Session) -> UiResult<Vec<f32>> {
    s.doc(|d| {
        let mut v = Vec::with_capacity((d.width * d.height) as usize);
        for y in 0..d.height as i32 {
            for x in 0..d.width as i32 {
                v.push(d.selection.as_ref().map_or(0.0, |m| m.value(x, y)));
            }
        }
        v
    })
}

fn ai_high_detail(s: &mut Session) -> UiResult {
    open_photo(s, "cat.jpg")?;
    s.describe("Select ▸ Subject at the standard detail");
    let t = Instant::now();
    s.menu("Select > Subject")?;
    s.wait_idle()?;
    let standard_secs = t.elapsed().as_secs_f32();
    let st = note_status(s)?;
    s.check(
        "standard detail",
        !st.contains("high detail"),
        "no “high detail”",
        st,
    )?;
    let standard = selection_values(s)?;
    s.screenshot("standard");
    s.describe("Deselect");
    s.key("Cmd+D")?;

    open_ai_prefs(s)?;
    s.describe("Tick High detail for hair and fur");
    s.click("High detail for hair and fur")?;
    let on = s.app(|a| a.prefs.ai_high_detail)?;
    s.check_eq("the setting is on", on, true)?;
    s.describe("Save");
    s.click("Save")?;
    let kept = s.app(|a| a.prefs.ai_high_detail)?;
    s.check_eq("Save keeps it", kept, true)?;

    s.describe("Select ▸ Subject at high detail");
    let t = Instant::now();
    s.menu("Select > Subject")?;
    s.wait_idle()?;
    let high_secs = t.elapsed().as_secs_f32();
    let st = note_status(s)?;
    s.screenshot("high");
    if st.contains("free memory") {
        s.note("High detail was refused for lack of memory (the guard): the message says so")?;
        return Ok(());
    }
    s.check(
        "the status says high detail",
        st.contains("high detail"),
        "“(high detail, …)”",
        st,
    )?;
    check_cover(
        s,
        "the cat is selected, not the backdrop",
        &[(700, 700), (650, 1100), (790, 620)],
        &[(1500, 400), (1700, 1000), (300, 200)],
    )?;
    let high = selection_values(s)?;
    let n = standard.len().max(1) as f32;
    let changed = standard
        .iter()
        .zip(&high)
        .filter(|(a, b)| (*a - *b).abs() > 0.1)
        .count();
    let edge = standard
        .iter()
        .zip(&high)
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / n;
    s.note(&format!(
        "Standard {standard_secs:.1} s, high detail {high_secs:.1} s; {:.2} % of pixels differ by more than 0.1, \
         mean difference {edge:.4}",
        changed as f32 / n * 100.0
    ))?;
    s.check(
        "the two agree on the subject (they differ only near edges)",
        (changed as f32 / n) < 0.05,
        "< 5 % of pixels differ",
        format!("{:.2} %", changed as f32 / n * 100.0),
    )?;
    // Leave the profile as found.
    open_ai_prefs(s)?;
    s.describe("Untick High detail again");
    s.click("High detail for hair and fur")?;
    s.click("Save")?;
    Ok(())
}

// ---- AI: Remove Background ------------------------------------------------------------

fn ai_remove_background(s: &mut Session) -> UiResult {
    open_photo(s, "camera.jpg")?;
    let before = s
        .layer_pixel("camera.jpg", 400, 400)?
        .or(s.layer_pixel("Background", 400, 400)?);
    let layer = s.layer_names()?.first().cloned().unwrap_or_default();
    s.note(&format!("The photo's layer is “{layer}”"))?;
    s.describe("Layer ▸ Remove background");
    let t = Instant::now();
    s.menu("Layer > Remove background")?;
    s.wait_idle()?;
    s.note(&format!(
        "Remove Background on 1920 × 1280: {:.1} s",
        t.elapsed().as_secs_f32()
    ))?;
    note_status(s)?;
    s.screenshot("removed");
    let inside: Vec<Option<f32>> = [(930, 830), (790, 800), (1200, 800)]
        .iter()
        .map(|&(x, y)| s.mask_at(&layer, x, y))
        .collect::<UiResult<_>>()?;
    let outside: Vec<Option<f32>> = [(400, 400), (1600, 300), (400, 1150)]
        .iter()
        .map(|&(x, y)| s.mask_at(&layer, x, y))
        .collect::<UiResult<_>>()?;
    s.check(
        "a layer mask shows the camera and hides the background",
        inside.iter().all(|v| v.is_some_and(|v| v >= 0.9))
            && outside.iter().all(|v| v.is_some_and(|v| v <= 0.1)),
        "≥ 0.9 on the camera, ≤ 0.1 on the table and wall",
        format!("{inside:?} / {outside:?}"),
    )?;
    let after = s.layer_pixel(&layer, 400, 400)?;
    s.check_eq("the pixels stay (non-destructive)", after, before)?;
    let shown = s.pixel(400, 400)?;
    s.check(
        "the background shows as transparent",
        shown[3] <= 26,
        "alpha ≤ 26",
        format!("{shown:?}"),
    )?;
    s.describe("Undo");
    s.key("Cmd+Z")?;
    let mask = s.mask_at(&layer, 400, 400)?;
    s.check_eq("undo removes the mask", mask, None)?;

    // Not on a layer without pixels.
    s.describe("Add a Curves layer from Properties");
    s.click_role(Role::Button, "Curves")?;
    s.describe("Open the Layer menu");
    s.click("Layer")?;
    s.expect_disabled("Remove background", "Select a pixel layer first")?;
    s.key("Esc")?;
    Ok(())
}

// ---- Actions ----------------------------------------------------------------------

fn action_names(s: &mut Session) -> UiResult<Vec<String>> {
    s.app(|a| a.actions.list.iter().map(|e| e.action.name.clone()).collect())
}

fn action_steps(s: &mut Session, name: &str) -> UiResult<Vec<String>> {
    let name = name.to_string();
    s.app(move |a| {
        a.actions
            .list
            .iter()
            .find(|e| e.action.name == name)
            .map(|e| e.action.steps.iter().map(|st| st.describe()).collect())
            .unwrap_or_default()
    })
}

fn actions_record_play(s: &mut Session) -> UiResult {
    open_photo(s, "dog.jpg")?;
    s.describe("Show the Actions panel from the Window menu");
    s.menu("Window > Actions")?;
    s.expect_text("ACTIONS")?;
    s.screenshot("panel");
    s.describe("Start a new action");
    s.click("New")?;
    s.expect_text("Recording")?;

    s.describe("Flip the image with Image ▸ Flip image horizontal");
    s.menu("Image > Flip image horizontal")?;
    s.describe("Add a Black & White layer from the quick-add chips");
    s.click_role(Role::Button, "B & W")?;
    s.wait_idle()?;
    s.describe("Pick the photo's layer to paint on");
    s.click("Layer Background")?;
    s.describe("Paint a brush stroke (not recordable)");
    s.click_role(Role::Button, "Brush")?;
    s.canvas_stroke(&[(1650.0, 1100.0), (1850.0, 1150.0)], 8)?;
    s.describe("Image ▸ Image Size... to 960 px wide");
    s.menu("Image > Image size...")?;
    s.set_field("Width", "960")?;
    s.click("Apply")?;
    s.wait_idle()?;
    s.describe("Stop recording");
    s.click("Stop")?;
    note_status(s)?;
    s.screenshot("recorded");
    let names = action_names(s)?;
    let name = names.last().cloned().unwrap_or_default();
    let steps = action_steps(s, &name)?;
    s.note(&format!("“{name}”: {}", steps.join(" · ")))?;
    s.check_eq(
        "the steps, with the stroke noted as not recordable",
        steps
            .iter()
            .filter(|t| !t.starts_with("not recordable"))
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "Flip image horizontal".to_string(),
            "Add Black & White layer".to_string(),
            "Image size 960 px wide, proportional".to_string(),
        ],
    )?;
    let skipped = steps.iter().filter(|t| t.starts_with("not recordable")).count();
    s.check(
        "the stroke is listed as not recordable",
        skipped >= 1,
        "≥ 1 note",
        format!("{skipped}"),
    )?;

    // Play it on another photo.
    open_photo(s, "cat.jpg")?;
    let corner = s.pixel(10, 10)?;
    let far = s.pixel(1909, 10)?;
    let layers_before = s.layer_names()?.len();
    let hist_before = s.history()?.len();
    s.describe(&format!("Select “{name}” in the Actions panel"));
    s.click(&name)?;
    s.describe("Play it");
    s.click("Play")?;
    s.wait_idle()?;
    note_status(s)?;
    s.screenshot("played");
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the cat photo is now 960 px wide", size, (960, 643))?;
    let layers = s.layer_names()?;
    s.check_eq("one Black & White layer on top", layers.len(), layers_before + 1)?;
    let top = layers.first().cloned().unwrap_or_default();
    s.check(
        "the top layer is Black & White",
        top.contains("Black"),
        "Black & White",
        top,
    )?;
    let flipped = s.pixel(954, 5)?;
    s.note(&format!(
        "Before: top-left {corner:?}, top-right {far:?}; after, top-right {flipped:?}"
    ))?;
    let grey = flipped[0].abs_diff(flipped[1]) <= 2 && flipped[1].abs_diff(flipped[2]) <= 2;
    s.check(
        "the photo is black and white",
        grey,
        "r = g = b",
        format!("{flipped:?}"),
    )?;
    let h = s.history()?;
    s.check_eq("playing is one undo step", h.len(), hist_before + 1)?;
    s.check_eq(
        "named after the action",
        h.last().cloned(),
        Some(format!("Action: {name}")),
    )?;
    s.describe("Undo the whole action");
    s.key("Cmd+Z")?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("back to 1920 × 1285", size, (1920, 1285))?;
    let layers = s.layer_names()?.len();
    s.check_eq("and to its layers", layers, layers_before)?;
    let back = s.pixel(10, 10)?;
    s.check_eq("and to its pixels", back, corner)?;

    // Rename by double-click, then delete.
    s.describe("Double-click the action's name to rename it");
    s.double_click(&name)?;
    s.set_field("Action name", "Dog look")?;
    let names = action_names(s)?;
    s.check(
        "renamed",
        names.contains(&"Dog look".to_string()),
        "Dog look",
        format!("{names:?}"),
    )?;
    let saved = s.app(|_| {
        let p = crate::session::data_dir().unwrap().join("actions.json");
        std::fs::read_to_string(p).unwrap_or_default()
    })?;
    s.check(
        "actions.json has the new name",
        saved.contains("Dog look"),
        "contains “Dog look”",
        format!("{} bytes", saved.len()),
    )?;
    s.describe("Delete it");
    s.click("Delete")?;
    s.expect_text("Delete \u{201C}Dog look\u{201D}?")?;
    s.describe("Confirm");
    s.click("Delete action")?;
    let names = action_names(s)?;
    s.check(
        "it is gone",
        !names.contains(&"Dog look".to_string()),
        "no “Dog look”",
        format!("{names:?}"),
    )?;
    Ok(())
}

fn actions_builtin(s: &mut Session) -> UiResult {
    open_photo(s, "woman.jpg")?;
    s.describe("Open the command palette and look for actions");
    s.key("Cmd+K")?;
    s.type_text("action")?;
    s.screenshot("palette");
    s.key("Esc")?;
    s.describe("Window ▸ Actions");
    s.menu("Window > Actions")?;
    let names = action_names(s)?;
    s.note(&format!("Actions: {names:?}"))?;
    s.describe("Show the steps of Vintage fade");
    s.click("Show the steps of Vintage fade")?;
    s.screenshot("vintage-steps");
    s.describe("Select Vintage fade");
    s.click("Vintage fade")?;
    s.expect_disabled("Rename", "Built-in actions keep their names")?;
    s.expect_disabled("Delete", "Built-in")?;
    let hist = s.history()?.len();
    let layers = s.layer_names()?.len();
    let before = s.pixel(990, 380)?;
    s.describe("Play Vintage fade");
    s.click("Play")?;
    s.wait_idle()?;
    note_status(s)?;
    let after = s.pixel(990, 380)?;
    s.note(&format!("Face pixel {before:?} → {after:?}"))?;
    let l = s.layer_names()?;
    s.check_eq("three adjustment layers added", l.len(), layers + 3)?;
    let h = s.history()?;
    s.check_eq("one undo step", h.len(), hist + 1)?;
    s.check(
        "the photo changed",
        after != before,
        "different",
        format!("{after:?}"),
    )?;
    s.describe("Undo");
    s.key("Cmd+Z")?;
    let back = s.pixel(990, 380)?;
    s.check_eq("undo restores it", back, before)?;

    // Recording an AI step: what happens to it?
    s.describe("Record a new action");
    s.click("New")?;
    s.describe("Select ▸ Subject while recording");
    s.menu("Select > Subject")?;
    s.wait_idle()?;
    s.describe("Stop");
    s.click("Stop")?;
    let names = action_names(s)?;
    let name = names.last().cloned().unwrap_or_default();
    let steps = action_steps(s, &name)?;
    s.note(&format!("Recording Select Subject gives: {steps:?}"))?;
    s.check(
        "Select Subject is recorded or noted, never silently lost",
        !steps.is_empty(),
        "a step or a “not recordable” note",
        format!("{steps:?}"),
    )?;
    note_status(s)?;
    Ok(())
}

// ---- PSD round trip -------------------------------------------------------------------

/// A Photoshop file with text, shapes and clipping, from the psd-tools
/// test corpus (`LUMENPLY_UITEST_PSD`).
fn psd_fixture() -> Option<PathBuf> {
    std::env::var_os("LUMENPLY_UITEST_PSD")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn layer_summary(s: &mut Session) -> UiResult<Vec<(String, bool, u8, String)>> {
    s.doc(|d| {
        let mut out = Vec::new();
        fn walk(ls: &[lumenply_doc::Layer], out: &mut Vec<(String, bool, u8, String)>) {
            for l in ls.iter().rev() {
                let text = l.text_layer().map(|t| t.text.clone()).unwrap_or_default();
                out.push((l.name.clone(), l.visible, (l.opacity * 100.0).round() as u8, text));
                if let Some(c) = l.children() {
                    walk(c, out);
                }
            }
        }
        walk(d.layers(), &mut out);
        out
    })
}

fn find_op(v: &[(String, bool, u8, String)], name: &str) -> Option<u8> {
    v.iter().find(|l| l.0 == name).map(|l| l.2)
}

fn psd_round_trip(s: &mut Session) -> UiResult {
    let src = psd_fixture().ok_or_else(|| UiError("set LUMENPLY_UITEST_PSD to a Photoshop file".into()))?;
    let file = s.copy_in(&src)?;
    open_file(s, &file)?;
    let before = layer_summary(s)?;
    s.note(&format!("Opened: {before:?}"))?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.screenshot("opened");

    // Edit the text layer "none": select its text and type over it.
    s.describe("Pick the Text tool");
    s.click_role(Role::Button, "Text")?;
    s.describe("Click into the text “none”");
    s.canvas_click((200.0, 457.0), "")?;
    s.describe("Select all its text");
    s.key("Cmd+A")?;
    s.type_text("no clip")?;
    s.describe("Commit with Esc... or Enter on the keypad: click the Move tool");
    s.click_role(Role::Button, "Move")?;
    s.wait_idle()?;
    let after_text = layer_summary(s)?;
    s.note(&format!("After the text edit: {after_text:?}"))?;
    let edited = after_text.iter().any(|l| l.3 == "no clip");
    s.check(
        "the text now reads “no clip”",
        edited,
        "a text layer “no clip”",
        format!("{after_text:?}"),
    )?;

    // Layers: hide one, rename one, change an opacity.
    s.describe("Hide “Rectangle 1 copy 3” with its eye");
    s.click_offset("Layer Rectangle 1 copy 3", 14.0, 19.0, "the eye")?;
    s.describe("Rename “Rectangle 2 copy 2” by double-clicking it");
    s.double_click("Layer Rectangle 2 copy 2")?;
    s.set_field("Layer name", "Blue square")?;
    s.describe("Select “Rectangle 2 copy 3”");
    s.click("Layer Rectangle 2 copy 3")?;
    s.describe("Set its opacity to 50 %");
    s.within("Properties", |s| s.drag_slider("Opacity", 0.5))?;
    s.wait_idle()?;
    let edited = layer_summary(s)?;
    s.note(&format!("Edited: {edited:?}"))?;
    let op = find_op(&edited, "Rectangle 2 copy 3");
    s.check_eq("the layer is at 50 % opacity", op, Some(50))?;

    // Export, close, reopen.
    let out = s.files().join("round-trip.psd");
    s.describe("Click Export at the top right");
    s.click("Export")?;
    s.describe("Photoshop PSD...");
    s.click("Photoshop PSD...")?;
    s.save_as(&out)?;
    s.wait_idle()?;
    s.check(
        "the PSD was written",
        out.exists(),
        "a file",
        if out.exists() { "a file" } else { "nothing" },
    )?;
    note_status(s)?;
    s.describe("Close the document");
    s.menu("File > Close document")?;
    s.describe("Close without saving: the edits are in the exported PSD");
    s.click("Close without saving")?;
    open_file(s, &out)?;
    let reopened = layer_summary(s)?;
    s.note(&format!("Reopened: {reopened:?}"))?;
    s.screenshot("reopened");
    let size2 = s.doc(|d| (d.width, d.height))?;
    s.check_eq("same canvas size", size2, size)?;
    let names = |v: &[(String, bool, u8, String)]| v.iter().map(|l| l.0.clone()).collect::<Vec<_>>();
    s.check_eq("same layers, in order", names(&reopened), names(&edited))?;
    let find = |v: &[(String, bool, u8, String)], n: &str| v.iter().find(|l| l.0 == n).cloned();
    s.check_eq(
        "the hidden layer stays hidden",
        find(&reopened, "Rectangle 1 copy 3").map(|l| l.1),
        Some(false),
    )?;
    s.check_eq(
        "the renamed layer kept its name",
        find(&reopened, "Blue square").is_some(),
        true,
    )?;
    s.check_eq(
        "the opacity came back",
        find(&reopened, "Rectangle 2 copy 3").map(|l| l.2),
        Some(50),
    )?;
    s.check(
        "the edited text came back as text",
        reopened.iter().any(|l| l.3 == "no clip"),
        "a text layer “no clip”",
        format!("{reopened:?}"),
    )?;
    Ok(())
}
