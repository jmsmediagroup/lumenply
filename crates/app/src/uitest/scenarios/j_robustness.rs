//! Undo, history and robustness: undo and redo after every kind of edit,
//! slider drags as one step, the history limit, undo after reopening,
//! autosave and crash recovery, large documents, long sessions and the
//! careless things users do.

use std::hash::{Hash, Hasher};

use crate::uitest::prelude::*;

scenario_list! {
    "undo-paint-and-layers" => undo_paint_and_layers: "Paint, erase and layer edits, each undone and redone one step at a time",
    "undo-transform-and-canvas" => undo_transform_and_canvas: "Move, nudge, free transform, crop, canvas and image size, rotate: undo and redo",
    "undo-adjust-filter-text-shape" => undo_adjust_filter_text_shape: "Adjustment layers, a filter, text and a shape: undo and redo",
    "undo-selection-and-mask" => undo_selection_and_mask: "Marquee, add, subtract, invert, deselect, reselect, feather, mask: undo and redo",
    "history-jump-and-branch" => history_jump_and_branch: "Jump around the History strip, branch off with a new edit, snapshots",
    "unsaved-after-undo" => unsaved_after_undo: "Save, undo, edit again: the document must count as unsaved",
    "undo-after-reopen" => undo_after_reopen: "Save a layered document, reopen it, keep editing and undoing",
    "history-limit" => history_limit: "Undo steps capped at 5 in Preferences: what is kept, what undo can reach",
    "autosave-and-recover" => autosave_and_recover: "Autosave a backup, crash, relaunch and recover the work",
    "odd-behaviour" => odd_behaviour: "Menus under dialogs, tab switches mid-operation, Esc everywhere, drags off the window",
    "full-history-crop" => full_history_crop: "With the history full, cropping still finishes (frame gone, one crop)",
    "large-document" => large_document: "A 6000 × 4000 document with 30 layers: paint, transform, undo, save, reopen",
    "long-session" => long_session: "Hundreds of strokes, undos and redos: history and memory stay bounded",
}

fn full_history_crop(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    set_prefs(s, &[("Undo steps", "3")])?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    for y in [150.0, 300.0, 450.0] {
        s.describe("Paint a stroke");
        s.canvas_drag((100.0, y), (700.0, y), 12, "")?;
    }
    s.wait_idle()?;
    let n = s.history()?.len();
    s.check_eq("the history is full (3 steps)", n, 3)?;
    s.describe("Pick the Crop tool");
    s.click("Crop")?;
    s.describe("Drag a crop frame");
    s.canvas_drag((100.0, 100.0), (500.0, 400.0), 12, "")?;
    s.describe("Press Enter to crop");
    s.key("Enter")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("cropped to 400 × 300", size, (400, 300))?;
    s.expect_text("Cropped to 400 × 300 px")?;
    let frame = s.app(|a| a.crop.frame.map(|f| f.rect()))?;
    s.check(
        "the crop frame is reset to the new canvas",
        frame.is_none_or(|r| (r.x, r.y, r.w, r.h) == (0, 0, 400, 300)),
        "no frame or one around the whole 400 × 300 canvas",
        format!("{frame:?}"),
    )?;
    s.describe("Press Enter again");
    s.key("Enter")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("Enter again does not crop a second time", size, (400, 300))?;
    s.describe("Undo the crop");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("back to 800 × 600", size, (800, 600))?;
    Ok(())
}

/// The app's resident memory in MB, as Activity Monitor shows it.
fn rss_mb() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

/// Run `f` and note how long it took in wall-clock time.
fn timed<T>(s: &mut Session, what: &str, f: impl FnOnce(&mut Session) -> UiResult<T>) -> UiResult<(T, f64)> {
    let t = std::time::Instant::now();
    let r = f(s)?;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    s.note(&format!("TIMING {what}: {ms:.0} ms (RSS {:.0} MB)", rss_mb()))?;
    Ok((r, ms))
}

/// Editor memory figures for the log: (undo history MB, render cache MB).
fn memory(s: &mut Session) -> UiResult<(f64, f64)> {
    s.app(|a| {
        (
            a.editor.history_bytes() as f64 / 1048576.0,
            crate::session::render_cache_bytes(&a.editor) as f64 / 1048576.0,
        )
    })
}

fn large_document(s: &mut Session) -> UiResult {
    s.note(&format!("RSS at start: {:.0} MB", rss_mb()))?;
    timed(s, "new 6000 × 4000 document", |s| new_document(s, 6000, 4000))?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.set_field("Size", "120")?;
    timed(s, "29 new layers, a stroke on each", |s| {
        for i in 0..29 {
            s.describe(&format!(
                "New layer {} with Shift+Cmd+N, and a stroke on it",
                i + 2
            ));
            s.key("Shift+Cmd+N")?;
            let y = 200.0 + 125.0 * i as f32;
            s.canvas_drag(
                (300.0 + 40.0 * i as f32, y),
                (5700.0 - 40.0 * i as f32, y + 300.0),
                6,
                "",
            )?;
        }
        s.wait_idle()
    })?;
    let layers = s.layer_names()?.len();
    s.check_eq("30 layers", layers, 30)?;
    let (h, c) = memory(s)?;
    s.note(&format!(
        "Undo history {h:.0} MB, render cache {c:.0} MB, RSS {:.0} MB",
        rss_mb()
    ))?;
    let before = snap(s)?;
    timed(s, "a long brush stroke across the canvas on layer 30", |s| {
        s.describe("Paint a long stroke across the whole canvas");
        s.canvas_stroke(&[(200.0, 3800.0), (3000.0, 400.0), (5800.0, 3600.0)], 24)?;
        s.wait_idle()
    })?;
    let stroked = snap(s)?;
    timed(s, "undo the stroke", |s| {
        s.key("Cmd+Z")?;
        s.wait_idle()
    })?;
    let now = snap(s)?;
    s.check_eq(
        "undo brings the 30-layer document back exactly",
        now,
        before.clone(),
    )?;
    timed(s, "redo the stroke", |s| {
        s.key("Shift+Cmd+Z")?;
        s.wait_idle()
    })?;
    let now = snap(s)?;
    s.check_eq("redo brings the stroke back", now, stroked.clone())?;
    timed(s, "free transform of the top layer to 50%, typed", |s| {
        s.key("Cmd+T")?;
        s.set_field("Width", "50")?;
        s.set_field("Height", "50")?;
        s.key("Enter")?;
        s.wait_idle()
    })?;
    timed(s, "drag the top layer's opacity", |s| {
        s.within("Properties", |s| s.drag_slider("Opacity", 0.5))?;
        s.wait_idle()
    })?;
    timed(s, "hide the bottom layer with its eye", |s| {
        s.describe("Scroll to the bottom of the Layers panel");
        s.scroll("Layers", 3000.0)?;
        s.click_offset("Layer Background", 14.0, 19.0, "the eye")?;
        s.wait_idle()
    })?;
    let edited = snap(s)?;
    let (h, c) = memory(s)?;
    s.note(&format!(
        "Undo history {h:.0} MB, render cache {c:.0} MB, RSS {:.0} MB",
        rss_mb()
    ))?;
    let file = s.files().join("large.lumen");
    timed(s, "save as .lumen", |s| {
        s.menu("File > Save as...")?;
        s.save_as(&file)
    })?;
    let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    s.note(&format!("large.lumen is {:.1} MB", bytes as f64 / 1048576.0))?;
    s.describe("Close it");
    s.key("Cmd+W")?;
    timed(s, "reopen from Open recent", |s| {
        s.menu("File > Open recent > large.lumen")?;
        s.wait_idle()
    })?;
    let reopened = snap(s)?;
    s.check_eq(
        "the same 30 layers came back",
        reopened.layers.clone(),
        edited.layers.clone(),
    )?;
    s.check_eq(
        "the same samples",
        reopened.samples.clone(),
        edited.samples.clone(),
    )?;
    s.note(&format!("RSS at the end: {:.0} MB", rss_mb()))?;
    Ok(())
}

fn long_session(s: &mut Session) -> UiResult {
    new_document(s, 1600, 1200)?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    let mut rss = Vec::new();
    let mut edits = 0usize;
    let mut undos = 0usize;
    for i in 0..300usize {
        let y = 50.0 + (i % 22) as f32 * 50.0;
        let x0 = 50.0 + (i % 7) as f32 * 30.0;
        s.describe(&format!("Stroke {}", i + 1));
        s.canvas_drag((x0, y), (1550.0 - x0, y + 40.0), 3, "")?;
        edits += 1;
        if i % 25 == 24 {
            s.describe("Invert the whole canvas with Cmd+I (a full-canvas edit)");
            s.key("Cmd+I")?;
            edits += 1;
        }
        if i % 10 == 9 {
            s.describe("Undo three times, redo twice");
            for _ in 0..3 {
                s.key("Cmd+Z")?;
            }
            for _ in 0..2 {
                s.key("Shift+Cmd+Z")?;
            }
            undos += 5;
        }
        if i % 50 == 49 {
            s.wait_idle()?;
            let (h, c) = memory(s)?;
            let mb = rss_mb();
            let steps = s.history()?.len();
            rss.push(mb);
            s.note(&format!(
                "MEMORY after {edits} edits and {undos} undo/redos: {steps} steps, undo history {h:.0} MB, render cache {c:.0} MB, RSS {mb:.0} MB"
            ))?;
        }
    }
    let steps = s.history()?.len();
    s.check(
        "the history holds at most 100 steps",
        steps <= 100,
        "≤ 100",
        format!("{steps}"),
    )?;
    let (h, c) = memory(s)?;
    s.check(
        "the undo history stays under its 1024 MB cap",
        h <= 1024.0,
        "≤ 1024 MB",
        format!("{h:.0} MB"),
    )?;
    s.check(
        "the render cache stays under its 1536 MB budget",
        c <= 1536.0,
        "≤ 1536 MB",
        format!("{c:.0} MB"),
    )?;
    let growth = rss.last().copied().unwrap_or(0.0) - rss.get(1).copied().unwrap_or(0.0);
    s.check(
        "memory levels off: RSS grows less than 300 MB from stroke 100 to stroke 300",
        growth < 300.0,
        "< 300 MB",
        format!("{growth:.0} MB ({rss:?})"),
    )?;
    let t = std::time::Instant::now();
    let mut presses = 0;
    while !s.history()?.is_empty() && presses < 120 {
        s.describe("Undo with Cmd+Z, all the way back");
        s.key("Cmd+Z")?;
        presses += 1;
    }
    s.wait_idle()?;
    s.note(&format!(
        "TIMING {presses} undos: {:.0} ms in all, RSS {:.0} MB",
        t.elapsed().as_secs_f64() * 1000.0,
        rss_mb()
    ))?;
    let n = s.history()?.len();
    s.check_eq("at the oldest kept step", n, 0)?;
    Ok(())
}

fn set_prefs(s: &mut Session, fields: &[(&str, &str)]) -> UiResult {
    s.describe("Open the Edit menu");
    s.click("Edit")?;
    // In a small window the menu is taller than the room below the bar
    // and scrolls; a user rolls the wheel down to its end.
    s.describe("Scroll down the menu");
    s.scroll("Cut", 600.0)?;
    s.describe("Choose Preferences… at its end");
    s.click("Preferences...")?;
    for (name, value) in fields {
        s.set_field(name, value)?;
    }
    s.describe("Save the preferences");
    s.click("Save")
}

fn history_limit(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    set_prefs(s, &[("Undo steps", "5")])?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    let mut states = vec![snap(s)?];
    for i in 0..8 {
        let y = 60.0 + 65.0 * i as f32;
        s.describe(&format!("Paint stroke {}", i + 1));
        s.canvas_drag((100.0, y), (700.0, y), 12, "")?;
        s.wait_idle()?;
        states.push(snap(s)?);
    }
    let h = s.history()?;
    s.check_eq("only the last five steps are kept", h.len(), 5)?;
    s.screenshot("history strip at the limit");
    // The oldest card now shows three strokes: calling it "Open" would
    // promise the opened document.
    let open_card = s.has_node("History step 0: Open");
    s.check_eq("no card claims to be the opened document", open_card, false)?;
    for i in (3..8).rev() {
        s.describe("Undo with Cmd+Z");
        s.key("Cmd+Z")?;
        s.wait_idle()?;
        let now = snap(s)?;
        s.check_eq(&format!("back to {i} strokes"), now, states[i].clone())?;
    }
    s.describe("Press Cmd+Z once more");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the oldest kept state stays (3 strokes)", now, states[3].clone())?;
    s.describe("Open the Edit menu");
    s.click("Edit")?;
    let undo = s.node("Undo").map(|n| n.disabled);
    s.check_eq("Undo is greyed out", undo, Some(true))?;
    s.describe("Point at Undo to see why");
    s.hover("Undo")?;
    s.expect_text("Nothing to undo")?;
    s.key("Esc")?;
    for _ in 0..5 {
        s.describe("Redo with Shift+Cmd+Z");
        s.key("Shift+Cmd+Z")?;
    }
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("all eight strokes are back", now, states[8].clone())?;

    // Raising the limit keeps what is there and lets history grow again.
    set_prefs(s, &[("Undo steps", "50")])?;
    for i in 0..3 {
        s.describe("Paint another stroke");
        let x = 150.0 + 200.0 * i as f32;
        s.canvas_drag((x, 50.0), (x, 550.0), 12, "")?;
    }
    s.wait_idle()?;
    let h = s.history()?;
    s.check_eq("eight steps now", h.len(), 8)?;
    Ok(())
}

/// The scratch profile's autosave folder, as the app sees it.
fn autosave_files(s: &mut Session) -> UiResult<Vec<String>> {
    s.app(|_| {
        let mut v: Vec<String> = crate::session::autosave_backups()
            .into_iter()
            .map(|(p, src)| {
                format!(
                    "{} from {}",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    src.map_or("nowhere".into(), |s| s
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned())
                )
            })
            .collect();
        v.sort();
        v
    })
}

/// Wait `secs` of real time with the app running, as a user who stops to
/// think: the autosave clock is the wall clock.
fn pause(s: &mut Session, secs: u64) -> UiResult {
    s.note(&format!("Wait {secs} s"))?;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(250));
        s.wait_frames(1)?;
    }
    s.wait_idle()
}

fn autosave_and_recover(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    set_prefs(s, &[("Autosave every", "15")])?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke");
    s.canvas_drag((100.0, 200.0), (700.0, 200.0), 12, "")?;
    s.describe("Add a layer");
    s.menu("Layer > New pixel layer")?;
    s.describe("Paint on it");
    s.canvas_drag((100.0, 400.0), (700.0, 400.0), 12, "")?;
    s.wait_idle()?;
    let before = snap(s)?;
    // A second, saved document with changes of its own.
    s.describe("Open a second document with File ▸ New");
    s.menu("File > New...")?;
    s.click("Create")?;
    s.wait_idle()?;
    s.describe("Paint on it");
    s.canvas_drag((100.0, 100.0), (700.0, 500.0), 12, "")?;
    let file = s.files().join("second.lumen");
    s.describe("Save it");
    s.menu("File > Save as...")?;
    s.save_as(&file)?;
    s.describe("Change it after saving");
    s.canvas_drag((700.0, 100.0), (100.0, 500.0), 12, "")?;
    s.wait_idle()?;
    let second = snap(s)?;
    pause(s, 17)?;
    s.expect_text("Autosaved backups of 2 documents")?;
    let files = autosave_files(s)?;
    s.check_eq(
        "both unsaved documents are backed up",
        files.clone(),
        vec![
            "0.lumen from second.lumen".to_string(),
            "1.lumen from nowhere".to_string(),
        ],
    )?;

    s.describe(
        "Lumenply crashes; the user starts it again (stand-in: the app is replaced, nothing is cleaned up)",
    );
    s.app(|a| *a = crate::App::launch(&[]))?;
    s.wait_idle()?;
    s.expect_text("Recover autosaved document")?;
    s.expect_text("2 documents")?;
    s.describe("Recover them");
    s.click("Recover")?;
    s.wait_idle()?;
    let tabs = s.app(|a| a.tab_infos())?;
    s.note(&format!("Tabs after recovering: {tabs:?}"))?;
    s.check_eq("two tabs, both unsaved", tabs.iter().filter(|t| t.1).count(), 2)?;
    let now = snap(s)?;
    s.check(
        "the active tab is one of the recovered documents",
        now == before || now == second,
        "the first or the second document",
        format!("{:?}", now.layers),
    )?;
    let names: Vec<String> = tabs.iter().map(|t| t.0.clone()).collect();
    let other = names
        .iter()
        .position(|n| *n != tabs.iter().find(|t| t.0 == names[0]).unwrap().0);
    s.note(&format!("Other tab index: {other:?}"))?;
    s.describe("Switch to the other tab");
    let first_name = names[0].clone();
    s.click(&first_name)?;
    s.wait_idle()?;
    let a = snap(s)?;
    let second_name = names[1].clone();
    s.describe("And to the other one");
    s.click(&second_name)?;
    s.wait_idle()?;
    let b = snap(s)?;
    s.check(
        "both documents came back exactly",
        (a == before && b == second) || (a == second && b == before),
        "the unsaved document and the saved one with its later change",
        format!("{:?} / {:?}", a.layers, b.layers),
    )?;
    s.describe("Undo in the recovered document");
    s.click("Edit")?;
    s.dump_tree("edit menu after recovering");
    s.key("Esc")?;
    s.describe("Save the recovered copy of second.lumen with Cmd+S");
    if b == second {
        s.key("Cmd+S")?;
    } else {
        s.click(&first_name)?;
        s.key("Cmd+S")?;
    }
    s.wait_idle()?;
    let dialog = s.open_dialog().map(|d| d.title.clone());
    s.check_eq("Save goes straight back to second.lumen, no panel", dialog, None)?;
    let left = autosave_files(s)?;
    s.check_eq("one backup left (the never-saved document)", left.len(), 1)?;
    Ok(())
}

fn odd_behaviour(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke");
    s.canvas_drag((100.0, 300.0), (700.0, 300.0), 12, "")?;
    s.wait_idle()?;
    let base = snap(s)?;
    let steps = s.history()?.len();

    // A dialog is open: the menu bar and Cmd+Z must not edit underneath it.
    s.describe("Open Image ▸ Canvas size");
    s.menu("Image > Canvas size...")?;
    // The harness won't click what something covers; a user just clicks.
    let edit = s.point_in("Edit", 0.5, 0.5)?;
    s.describe("Click the Edit menu while the dialog is open");
    s.click_at(edit, "the Edit menu, under the dialog's backdrop")?;
    let menu = s.has_node("Free transform");
    s.check_eq("the menu does not open under a dialog", menu, false)?;
    s.describe("Press Cmd+Z while the dialog is open");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("nothing was undone under the dialog", now, base.clone())?;
    s.describe("Esc closes the dialog");
    s.key("Esc")?;
    let open = s.has_node("Canvas size");
    s.check_eq("the dialog is gone", open, false)?;
    let n = s.history()?.len();
    s.check_eq("and the history is as it was", n, steps)?;

    // Esc everywhere.
    s.describe("Open the File menu, then Esc");
    s.click("File")?;
    s.key("Esc")?;
    let open = s.has_node("Save as...");
    s.check_eq("the menu closed", open, false)?;
    s.describe("Open the command palette, then Esc");
    s.key("Cmd+K")?;
    s.key("Esc")?;
    s.describe("Free transform, drag a corner, then Esc");
    s.key("Cmd+T")?;
    s.canvas_drag((700.0, 314.0), (780.0, 400.0), 10, "")?;
    s.key("Esc")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("Esc cancelled the transform", now, base.clone())?;
    s.describe("Pick the Crop tool, drag a frame, then Esc");
    s.click("Crop")?;
    s.canvas_drag((100.0, 100.0), (500.0, 400.0), 10, "")?;
    s.key("Esc")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("Esc left the canvas uncropped", now, base.clone())?;
    s.describe("Pick the Polygonal Lasso, click two corners, then Esc");
    s.click("Polygonal Lasso")?;
    s.canvas_click((200.0, 200.0), "")?;
    s.canvas_click((400.0, 200.0), "")?;
    s.key("Esc")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("no selection was made", now.selection, None)?;
    let n = s.history()?.len();
    s.check_eq("none of the cancelled operations left a history step", n, steps)?;

    // Undo pressed many more times than there are steps.
    s.describe("Press Cmd+Z five times (one step to undo)");
    for _ in 0..5 {
        s.key("Cmd+Z")?;
    }
    s.wait_idle()?;
    let blank = snap(s)?;
    s.check_eq("everything is undone, nothing crashed", blank.layers.len(), 1)?;
    s.describe("Redo five times");
    for _ in 0..5 {
        s.key("Shift+Cmd+Z")?;
    }
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the stroke is back", now, base.clone())?;

    // A stroke dragged off the edge of the window.
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    let from = s.doc_to_screen(400.0, 450.0)?;
    let edge = pos2(s_width(s) + 60.0, from.y + 40.0);
    s.describe("Paint from the canvas off the right edge of the window and let go out there");
    s.drag(from, edge, 20, "")?;
    s.wait_idle()?;
    let n = s.history()?.len();
    s.check_eq("the stroke is one step", n, steps + 1)?;
    let painted = s.pixel(600, 460)?;
    s.check(
        "it reached the canvas edge",
        painted != [255; 4],
        "brush colour",
        format!("{painted:?}"),
    )?;
    s.describe("Click the canvas once");
    s.canvas_click((200.0, 100.0), "")?;
    s.wait_idle()?;
    let n = s.history()?.len();
    s.check_eq("a click is a new dab, not a continuation", n, steps + 2)?;
    let between = s.pixel(300, 300)?;
    s.check_eq(
        "nothing joined the click to the earlier stroke",
        between,
        base.samples[1],
    )?;

    // Marquee dragged past the canvas.
    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.describe("Drag a selection out past the top-left of the canvas");
    s.canvas_drag((300.0, 300.0), (-200.0, -150.0), 12, "")?;
    let sel = s.selection_bounds()?;
    s.check_eq(
        "the selection stops at the canvas edge",
        sel,
        Some((0, 0, 300, 300)),
    )?;

    // Two documents: switch tabs while something is half done.
    let doc1 = snap(s)?;
    s.describe("Open a second document");
    s.menu("File > New...")?;
    s.click("Create")?;
    s.wait_idle()?;
    let doc2 = snap(s)?;
    s.describe("Open Gaussian Blur from the command palette (the preview shows)");
    s.key("Cmd+K")?;
    s.type_text("Gaussian")?;
    s.key("Enter")?;
    let tab1 = s.point_in("Untitled-1", 0.5, 0.5)?;
    s.describe("Click the first tab while the dialog is open");
    s.click_at(tab1, "the first document's tab")?;
    let on = s.app(|a| a.cur_tab)?;
    s.check_eq("the dialog keeps its document in front", on, 1)?;
    s.describe("Cancel the blur");
    s.click("Cancel")?;
    s.wait_idle()?;
    let now = snap(s)?;
    let which = s.app(|a| a.cur_tab)?;
    s.check_eq("the second document is not blurred", now, doc2.clone())?;
    s.describe("Look at the first document");
    s.click("Untitled-1")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the first document is not blurred either", now, doc1.clone())?;
    s.describe("Back to the second");
    s.click("Untitled-2")?;
    s.wait_idle()?;

    s.describe("Free transform in this tab, then switch tabs mid-transform");
    s.key("Cmd+T")?;
    let other = if which == 0 { "Untitled-2" } else { "Untitled-1" };
    s.click(other)?;
    s.wait_idle()?;
    s.describe("Come back");
    let back = if which == 0 { "Untitled-1" } else { "Untitled-2" };
    s.click(back)?;
    s.wait_idle()?;
    let shown = s.has_node("Free Transform");
    s.note(&format!("Free transform still up after the round trip: {shown}"))?;
    s.key("Esc")?;

    s.describe("Pick the Text tool, start typing, then switch tabs");
    s.click("Text")?;
    s.canvas_click((200.0, 200.0), "")?;
    s.type_text("Half")?;
    s.click(other)?;
    s.wait_idle()?;
    s.click(back)?;
    s.wait_idle()?;
    let text = s.doc(|d| {
        d.layers().iter().find_map(|l| match &l.content {
            lumenply_doc::LayerContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
    })?;
    s.note(&format!(
        "The half-typed text after switching away and back: {text:?}"
    ))?;
    s.check_eq("the typed text was kept", text, Some("Half".to_string()))?;
    note_history(s)?;
    Ok(())
}

/// The window's width: the Export button sits 12 points from its right edge.
fn s_width(s: &mut Session) -> f32 {
    s.node("Export").map_or(1440.0, |n| n.rect.right() + 12.0)
}

fn undo_selection_and_mask(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke");
    s.canvas_stroke(&[(100.0, 300.0), (400.0, 200.0), (700.0, 300.0)], 8)?;
    s.wait_idle()?;
    let start = snap(s)?;
    let mut states = Vec::new();
    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    states.push(one_step(s, "marquee", |s| {
        s.describe("Drag a selection");
        s.canvas_drag((100.0, 100.0), (400.0, 400.0), 10, "")
    })?);
    let now = snap(s)?.selection;
    s.check_eq(
        "the selection is the dragged box",
        now,
        Some((100, 100, 300, 300)),
    )?;
    states.push(one_step(s, "add to selection", |s| {
        s.describe("Shift-drag to add a second box");
        s.canvas_drag((500.0, 150.0), (700.0, 350.0), 10, "Shift")
    })?);
    let now = snap(s)?.selection;
    s.check_eq("both boxes are selected", now, Some((100, 100, 600, 300)))?;
    states.push(one_step(s, "subtract from selection", |s| {
        s.describe("Alt-drag to cut a notch out of the first box");
        s.canvas_drag((100.0, 100.0), (200.0, 400.0), 10, "Alt")
    })?);
    let now = snap(s)?.selection;
    s.check_eq("the notch is gone", now, Some((200, 100, 500, 300)))?;
    states.push(one_step(s, "invert", |s| {
        s.describe("Select ▸ Invert");
        s.menu("Select > Invert")
    })?);
    states.push(one_step(s, "deselect", |s| {
        s.describe("Deselect with Cmd+D");
        s.key("Cmd+D")
    })?);
    let now = snap(s)?.selection;
    s.check_eq("nothing is selected", now, None)?;
    states.push(one_step(s, "reselect", |s| {
        s.describe("Reselect with Shift+Cmd+D");
        s.key("Shift+Cmd+D")
    })?);
    let now = snap(s)?.selection;
    s.check_eq("the inverted selection is back", now, states[3].1.selection)?;
    states.push(one_step(s, "invert back", |s| {
        s.describe("Invert again");
        s.key("Shift+Cmd+I")
    })?);
    states.push(one_step(s, "feather", |s| {
        s.describe("Select ▸ Modify ▸ Feather… 12 px");
        s.menu("Select > Modify > Feather...")?;
        s.set_field("Feather radius", "12")?;
        s.click("OK")
    })?);
    states.push(one_step(s, "mask", |s| {
        s.describe("Select ▸ Layer mask from selection");
        s.menu("Select > Layer mask from selection")
    })?);
    let masked = s.mask_at("Background", 300, 250)?;
    s.check(
        "the Background has a mask showing the selected area",
        masked.is_some_and(|v| v > 0.99),
        "about 1 inside",
        format!("{masked:?}"),
    )?;
    if snap(s)?.selection.is_some() {
        states.push(one_step(s, "Esc deselects", |s| {
            s.describe("Press Esc to drop the selection");
            s.key("Esc")
        })?);
    }
    states.push(one_step(s, "disable mask", |s| {
        s.describe("Layer ▸ Layer mask ▸ Disable mask");
        s.menu("Layer > Layer mask > Disable mask")
    })?);
    note_history(s)?;
    undo_redo_all(s, &start, &states)
}

/// The `i`-th card of the History strip (0 is "Open"), clicked by where it
/// sits: cards are 58 points wide with 8 between them.
fn click_history_card(s: &mut Session, i: usize) -> UiResult {
    let open = s.point_in("History step 0: *", 0.5, 0.5)?;
    s.click_at(open + vec2(66.0 * i as f32, 0.0), &format!("history card {i}"))
}

fn history_jump_and_branch(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    s.describe("Open the Edit menu before doing anything");
    s.click("Edit")?;
    s.expect_disabled("Undo", "Nothing to undo")?;
    s.key("Esc")?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    let mut states = vec![snap(s)?];
    for (i, y) in [100.0, 220.0, 340.0, 460.0].into_iter().enumerate() {
        s.describe(&format!("Paint stroke {}", i + 1));
        s.canvas_drag((100.0, y), (700.0, y), 12, "")?;
        s.wait_idle()?;
        states.push(snap(s)?);
    }
    s.describe("Open the Edit menu");
    s.click("Edit")?;
    s.expect_node("Undo Paint stroke")?;
    s.key("Esc")?;

    s.describe("Click the second card in the History strip");
    click_history_card(s, 2)?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the document is as after two strokes", now, states[2].clone())?;
    let (undo, redo) = s.app(|a| (a.editor.history().len(), a.editor.redo_history().len()))?;
    s.check_eq("two steps applied, two to redo", (undo, redo), (2, 2))?;
    s.describe("Click the Open card");
    click_history_card(s, 0)?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the document is blank again", now, states[0].clone())?;
    s.describe("Click the last card");
    click_history_card(s, 4)?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("all four strokes are back", now, states[4].clone())?;

    s.describe("Jump back to step 2 and paint something new there");
    click_history_card(s, 2)?;
    s.describe("Paint a diagonal stroke");
    s.canvas_drag((100.0, 550.0), (700.0, 50.0), 14, "")?;
    s.wait_idle()?;
    let branched = snap(s)?;
    let (undo, redo) = s.app(|a| (a.editor.history().len(), a.editor.redo_history().len()))?;
    s.check_eq("the new edit replaces the two undone ones", (undo, redo), (3, 0))?;
    s.screenshot("history after branching");
    // The newest card must show the diagonal stroke, not the third
    // horizontal stroke the old branch had at this step.
    let thumbs = s.app(|a| a.hist_thumbs.len())?;
    s.check_eq("one thumbnail per step", thumbs, 4)?;
    s.describe("Undo the diagonal stroke");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("back to two strokes", now, states[2].clone())?;
    s.describe("Redo it");
    s.key("Shift+Cmd+Z")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the diagonal stroke is back", now, branched.clone())?;

    s.describe("Right-click the current card to keep it as a snapshot");
    s.right_click("History step 3: Paint stroke")?;
    s.describe("Choose New snapshot");
    s.click("New snapshot")?;
    s.wait_idle()?;
    s.describe("Paint over everything");
    s.canvas_drag((50.0, 300.0), (750.0, 300.0), 14, "")?;
    s.wait_idle()?;
    let painted = snap(s)?;
    s.describe("Click the snapshot card to go back to it");
    s.click("History snapshot: Snapshot 1")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("the snapshot's document is back", now, branched.clone())?;
    note_history(s)?;
    s.describe("Undo restoring the snapshot");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let now = snap(s)?;
    s.check_eq("undo takes back the restore", now, painted)?;
    Ok(())
}

fn title_unsaved(s: &mut Session) -> bool {
    s.title.contains('•')
}

fn unsaved_after_undo(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke");
    s.canvas_drag((100.0, 200.0), (700.0, 200.0), 12, "")?;
    s.describe("Paint another");
    s.canvas_drag((100.0, 400.0), (700.0, 400.0), 12, "")?;
    let file = s.files().join("strokes.lumen");
    s.describe("Save it with Shift+Cmd+S");
    s.key("Shift+Cmd+S")?;
    s.save_as(&file)?;
    let t = title_unsaved(s);
    s.check(
        "the title shows no unsaved dot after saving",
        !t,
        "no •",
        s.title.clone(),
    )?;
    s.describe("Undo the second stroke");
    s.key("Cmd+Z")?;
    let t = title_unsaved(s);
    s.check("undoing makes it unsaved", t, "•", s.title.clone())?;
    s.describe("Paint a different stroke instead");
    s.canvas_drag((400.0, 50.0), (400.0, 550.0), 12, "")?;
    s.wait_idle()?;
    let t = title_unsaved(s);
    s.check(
        "the document differs from the file, so it is unsaved",
        t,
        "a • in the title",
        s.title.clone(),
    )?;
    s.describe("Close it with Cmd+W");
    s.key("Cmd+W")?;
    let warned = s.text_shown("unsaved changes");
    s.check(
        "closing asks about the unsaved changes",
        warned,
        "an unsaved-changes prompt",
        if warned {
            "a prompt"
        } else {
            "closed without asking"
        },
    )?;
    if warned {
        s.describe("Cancel, keep working");
        s.click("Cancel")?;
    }

    // The same with the history full: every edit drops the oldest step,
    // so the number of steps stays the same after saving.
    set_prefs(s, &[("Undo steps", "3")])?;
    for y in [100.0, 300.0, 500.0] {
        s.describe("Paint a stroke");
        s.canvas_drag((100.0, y), (700.0, y + 30.0), 12, "")?;
    }
    s.wait_idle()?;
    let n = s.history()?.len();
    s.check_eq("three steps kept", n, 3)?;
    s.describe("Save with Cmd+S");
    s.key("Cmd+S")?;
    s.wait_idle()?;
    let t = title_unsaved(s);
    s.check("saved", !t, "no •", s.title.clone())?;
    s.describe("Paint one more stroke");
    s.canvas_drag((100.0, 150.0), (700.0, 450.0), 12, "")?;
    s.wait_idle()?;
    let t = title_unsaved(s);
    s.check(
        "an edit after saving with a full history is unsaved",
        t,
        "a • in the title",
        s.title.clone(),
    )?;
    s.describe("Close it with Cmd+W");
    s.key("Cmd+W")?;
    let warned = s.text_shown("unsaved changes");
    s.check(
        "closing asks about the unsaved changes",
        warned,
        "an unsaved-changes prompt",
        if warned {
            "a prompt"
        } else {
            "closed without asking"
        },
    )?;
    Ok(())
}

fn undo_after_reopen(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    layer_with_rect(s, (100.0, 100.0, 300.0, 250.0))?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke on the new layer");
    s.canvas_drag((100.0, 400.0), (700.0, 450.0), 12, "")?;
    s.describe("Add Levels from the quick-add chips");
    s.click_role(Role::Button, "Levels")?;
    s.describe("Pick the Text tool");
    s.click("Text")?;
    s.canvas_click((450.0, 200.0), "")?;
    s.type_text("Kept")?;
    s.key("Esc")?;
    s.wait_idle()?;
    let saved = snap(s)?;
    let saved_flat = s.doc(lumenply_render::composite_raster)?;
    let file = s.files().join("layers.lumen");
    s.describe("Save it with Shift+Cmd+S");
    s.key("Shift+Cmd+S")?;
    s.save_as(&file)?;
    // A zip lists its entries' names uncompressed.
    let format = std::fs::read(&file)
        .ok()
        .map(|b| b.starts_with(b"PK") && b.windows(10).any(|w| w == b"graph.json"));
    s.check_eq(
        "the project is a format-3 file with a graph.json",
        format,
        Some(true),
    )?;
    s.describe("Close the document");
    s.key("Cmd+W")?;
    s.expect_text("No document open")?;
    s.describe("Reopen it from File ▸ Open recent");
    s.menu("File > Open recent > layers.lumen")?;
    s.wait_idle()?;
    let flat = s.doc(lumenply_render::composite_raster)?;
    let (mut n, mut max, mut bx) = (0usize, 0f32, (u32::MAX, u32::MAX, 0u32, 0u32));
    for y in 0..flat.height {
        for x in 0..flat.width {
            let (a, b) = (flat.get(x, y).to_straight(), saved_flat.get(x, y).to_straight());
            let d = a.iter().zip(b).map(|(p, q)| (p - q).abs()).fold(0.0, f32::max);
            if d > 0.0 {
                n += 1;
                max = max.max(d);
                bx = (bx.0.min(x), bx.1.min(y), bx.2.max(x), bx.3.max(y));
            }
        }
    }
    s.note(&format!(
        "Reopened composite differs at {n} pixels, by at most {max}, within {bx:?}"
    ))?;
    // The text layer's pixels are rendered again on opening; they may
    // differ from the edited ones in the last bit of 16.
    s.check(
        "the reopened image matches the saved one to 16 bits",
        max <= 1.0 / 65535.0,
        "no difference above 1/65535",
        format!("{n} pixels differ by up to {max}"),
    )?;
    let opened = snap(s)?;
    s.check_eq("the same layers", opened.layers.clone(), saved.layers.clone())?;
    s.describe("Look at the Edit menu");
    s.click("Edit")?;
    s.expect_disabled("Undo", "Nothing to undo")?;
    s.key("Esc")?;

    let mut states = Vec::new();
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Select the rectangle's layer");
    s.click("Layer Layer 2")?;
    states.push(one_step(s, "stroke after reopening", |s| {
        s.describe("Paint on it");
        s.canvas_drag((150.0, 120.0), (650.0, 520.0), 14, "")
    })?);
    states.push(one_step(s, "edit the text", |s| {
        s.describe("Pick the Text tool and click into the text");
        s.click("Text")?;
        s.canvas_click((470.0, 180.0), "")?;
        s.key("End")?;
        s.type_text(" too")?;
        s.key("Esc")
    })?);
    let text = s.doc(|d| {
        d.layers().iter().find_map(|l| match &l.content {
            lumenply_doc::LayerContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
    })?;
    s.check_eq(
        "the reopened text is still editable",
        text,
        Some("Kept too".to_string()),
    )?;
    states.push(one_step(s, "hide Levels", |s| {
        s.describe("Hide the Levels layer");
        s.click_offset("Layer Levels", 14.0, 19.0, "the eye")
    })?);
    note_history(s)?;
    undo_redo_all(s, &opened, &states)
}

fn undo_adjust_filter_text_shape(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke to have something to adjust");
    s.canvas_stroke(&[(100.0, 300.0), (400.0, 200.0), (700.0, 300.0)], 8)?;
    s.wait_idle()?;
    let start = snap(s)?;
    let mut states = Vec::new();

    states.push(one_step(s, "Curves layer", |s| {
        s.describe("Add Curves from the quick-add chips");
        s.click_role(Role::Button, "Curves")
    })?);
    // In a small window Properties is short: bring the curve into view.
    s.describe("Scroll Properties down to the curve");
    s.scroll("Properties", 120.0)?;
    states.push(one_step(s, "curve point", |s| {
        s.describe("Click the middle of the curve to add a point");
        s.click_in("Curve, *", 0.5, 0.5, "the middle of the curve")
    })?);
    states.push(one_step(s, "curve drag", |s| {
        s.describe("Drag the point up");
        s.drag_in("Curve, *", (0.5, 0.5), (0.5, 0.25), "the new point up")
    })?);
    s.describe("Scroll Properties back up");
    s.scroll("Properties", -300.0)?;
    states.push(one_step(s, "Curves opacity", |s| {
        s.describe("Drag the Curves layer's Opacity to 50%");
        s.within("Properties", |s| s.drag_slider("Opacity", 0.5))
    })?);
    s.describe("Select the Background layer");
    s.click("Layer Background")?;
    states.push(one_step(s, "invert", |s| {
        s.describe("Invert the Background with Cmd+I");
        s.key("Cmd+I")
    })?);
    states.push(one_step(s, "Gaussian Blur", |s| {
        s.describe("Find Gaussian Blur in the command palette");
        s.key("Cmd+K")?;
        s.type_text("Gaussian")?;
        s.key("Enter")?;
        s.describe("Apply the blur");
        s.click("Apply")
    })?);
    s.describe("Pick the Text tool");
    s.click("Text")?;
    states.push(one_step(s, "new text", |s| {
        s.describe("Click the canvas and type");
        s.canvas_click((150.0, 450.0), "")?;
        s.type_text("Hello")?;
        s.describe("Commit with Esc");
        s.key("Esc")
    })?);
    let text = s.doc(|d| match &find_layer(d, "Hello")?.content {
        lumenply_doc::LayerContent::Text(t) => Some(t.text.clone()),
        _ => None,
    })?;
    s.check_eq("a text layer says Hello", text, Some("Hello".to_string()))?;
    s.describe("Pick the Shape tool");
    s.click("Shape")?;
    states.push(one_step(s, "shape", |s| {
        s.describe("Drag out a rectangle");
        s.canvas_drag((500.0, 380.0), (700.0, 520.0), 12, "")
    })?);
    s.describe("Scroll Properties down to the shape's settings");
    s.scroll("Properties", 400.0)?;
    states.push(one_step(s, "shape corners", |s| {
        s.describe("Round its corners in Properties");
        s.within("Properties", |s| s.drag_slider("Corners", 0.3))
    })?);
    note_history(s)?;
    undo_redo_all(s, &start, &states)
}

/// A new layer holding a filled rectangle (document pixels x0, y0, x1, y1),
/// made the way a user would: marquee, Fill with foreground colour, deselect.
fn layer_with_rect(s: &mut Session, r: (f32, f32, f32, f32)) -> UiResult {
    s.describe("Add a layer with Layer ▸ New pixel layer");
    s.menu("Layer > New pixel layer")?;
    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.describe("Select a rectangle");
    s.canvas_drag((r.0, r.1), (r.2, r.3), 10, "")?;
    s.describe("Fill it with Edit ▸ Fill with foreground colour");
    s.menu("Edit > Fill with foreground colour")?;
    s.describe("Deselect with Cmd+D");
    s.key("Cmd+D")?;
    s.wait_idle()
}

fn undo_transform_and_canvas(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    layer_with_rect(s, (200.0, 150.0, 400.0, 300.0))?;
    let start = snap(s)?;
    let mut states = Vec::new();

    s.describe("Pick the Move tool");
    s.click("Move")?;
    states.push(one_step(s, "move drag", |s| {
        s.describe("Drag the rectangle 100 px right");
        s.canvas_drag((300.0, 220.0), (400.0, 220.0), 12, "")
    })?);
    let bounds = s.doc(|d| find_layer(d, "Layer 2").and_then(|l| l.raster_store()?.content_bounds()))?;
    s.check_eq(
        "the layer moved 100 px",
        bounds.map(|b| (b.x, b.y, b.w, b.h)),
        Some((300, 150, 200, 150)),
    )?;
    states.push(one_step(s, "three nudges", |s| {
        s.describe("Nudge it down three times with the arrow key");
        s.key("Down")?;
        s.key("Down")?;
        s.key("Down")
    })?);
    let bounds = s.doc(|d| find_layer(d, "Layer 2").and_then(|l| l.raster_store()?.content_bounds()))?;
    s.check_eq(
        "the nudges moved it 3 px down",
        bounds.map(|b| (b.x, b.y)),
        Some((300, 153)),
    )?;
    states.push(one_step(s, "free transform", |s| {
        s.describe("Free transform with Cmd+T");
        s.key("Cmd+T")?;
        s.describe("Type 50% into W in the options bar");
        s.set_field("Width", "50")?;
        s.describe("Press Enter to apply the transform");
        s.key("Enter")
    })?);
    let bounds = s.doc(|d| find_layer(d, "Layer 2").and_then(|l| l.raster_store()?.content_bounds()))?;
    s.note(&format!("Layer bounds after the 50% width transform: {bounds:?}"))?;
    let before = s.history()?.len();
    s.describe("Free transform again, then change your mind with Esc");
    s.key("Cmd+T")?;
    s.set_field("Height", "200")?;
    s.key("Esc")?;
    s.wait_idle()?;
    let after = s.history()?.len();
    s.check_eq("a cancelled transform adds no step", after, before)?;
    let now = snap(s)?;
    s.check_eq(
        "and leaves the document as it was",
        &now,
        &states.last().unwrap().1,
    )?;

    s.describe("Pick the Crop tool");
    s.click("Crop")?;
    states.push(one_step(s, "crop", |s| {
        s.describe("Drag a crop frame");
        s.canvas_drag((100.0, 100.0), (700.0, 500.0), 12, "")?;
        s.describe("Press Enter to crop");
        s.key("Enter")
    })?);
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the canvas is cropped to 600 × 400", size, (600, 400))?;
    states.push(one_step(s, "canvas size", |s| {
        s.describe("Widen the canvas with Image ▸ Canvas size");
        s.menu("Image > Canvas size...")?;
        s.set_field("Width", "1000")?;
        s.click("Apply")
    })?);
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the canvas is 1000 × 400", size, (1000, 400))?;
    states.push(one_step(s, "image size", |s| {
        s.describe("Halve it with Image ▸ Image size");
        s.menu("Image > Image size...")?;
        s.set_field("Width", "500")?;
        s.click("Apply")
    })?);
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the image is 500 × 200", size, (500, 200))?;
    states.push(one_step(s, "rotate", |s| {
        s.describe("Rotate it with Image ▸ Rotate 90° clockwise");
        s.menu("Image > Rotate 90° clockwise")
    })?);
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the image is 200 × 500", size, (200, 500))?;
    note_history(s)?;
    undo_redo_all(s, &start, &states)
}

// ---- helpers ------------------------------------------------------------------

/// What a user would call "the document": its size, its layers and their
/// settings, the selection, and every composite pixel (as a hash, plus a
/// few samples to show in the log).
#[derive(Clone, Debug, PartialEq)]
struct Snap {
    size: (u32, u32),
    layers: Vec<String>,
    selection: Option<(i32, i32, u32, u32)>,
    samples: Vec<[u8; 4]>,
    pixels: u64,
}

fn describe_layers(layers: &[lumenply_doc::Layer], depth: usize, out: &mut Vec<String>) {
    for l in layers.iter().rev() {
        let kind = match &l.content {
            lumenply_doc::LayerContent::Pixel(_) => "pixel",
            lumenply_doc::LayerContent::Group(_) => "group",
            lumenply_doc::LayerContent::Adjustment(_) => "adjustment",
            lumenply_doc::LayerContent::Filter(_) => "filter",
            lumenply_doc::LayerContent::Text(_) => "text",
            lumenply_doc::LayerContent::Smart(_) => "smart",
            lumenply_doc::LayerContent::Fill(_) => "fill",
            _ => "other",
        };
        out.push(format!(
            "{}{} [{kind}{}{} {:.0}% {:?}{}]",
            "  ".repeat(depth),
            l.name,
            if l.visible { "" } else { " hidden" },
            if l.mask.is_some() { " masked" } else { "" },
            l.opacity * 100.0,
            l.blend,
            if l.clip { " clipped" } else { "" },
        ));
        if let lumenply_doc::LayerContent::Group(children) = &l.content {
            describe_layers(children, depth + 1, out);
        }
    }
}

/// Points sampled for the log, as fractions of the canvas.
const SAMPLE_AT: [(f32, f32); 5] = [(0.2, 0.25), (0.5, 0.5), (0.75, 0.3), (0.3, 0.7), (0.8, 0.8)];

fn snap(s: &mut Session) -> UiResult<Snap> {
    s.doc(|d| {
        let flat = lumenply_render::composite_raster(d);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in &flat.pixels {
            for c in p.to_straight() {
                c.to_bits().hash(&mut h);
            }
        }
        let samples = SAMPLE_AT
            .iter()
            .map(|(fx, fy)| {
                let x = ((fx * flat.width as f32) as u32).min(flat.width - 1);
                let y = ((fy * flat.height as f32) as u32).min(flat.height - 1);
                let [r, g, b, a] = flat.get(x, y).to_straight();
                [
                    lumenply_io::linear_to_srgb(r),
                    lumenply_io::linear_to_srgb(g),
                    lumenply_io::linear_to_srgb(b),
                    (a.clamp(0.0, 1.0) * 255.0).round() as u8,
                ]
            })
            .collect();
        let mut layers = Vec::new();
        describe_layers(d.layers(), 0, &mut layers);
        Snap {
            size: (d.width, d.height),
            layers,
            selection: d.selection.as_ref().map(|s| {
                let b = s.tight_bounds(d.canvas());
                (b.x, b.y, b.w, b.h)
            }),
            samples,
            pixels: h.finish(),
        }
    })
}

/// A user action that should add exactly one history step: run it, then
/// check the history grew by one and record the document afterwards.
fn one_step(
    s: &mut Session,
    what: &str,
    act: impl FnOnce(&mut Session) -> UiResult,
) -> UiResult<(String, Snap)> {
    let before = s.history()?;
    act(s)?;
    s.wait_idle()?;
    let after = s.history()?;
    let label = after.last().cloned().unwrap_or_default();
    s.check(
        &format!("“{what}” is one history step"),
        after.len() == before.len() + 1 && after[..before.len()] == before[..],
        format!("{} steps", before.len() + 1),
        format!("{} steps, newest “{label}”", after.len()),
    )?;
    let now = snap(s)?;
    Ok((label, now))
}

/// Undo every step in `states` (newest last) with Cmd+Z, checking the
/// document matches the state before each, then redo them all with
/// Shift+Cmd+Z, checking each comes back.
fn undo_redo_all(s: &mut Session, start: &Snap, states: &[(String, Snap)]) -> UiResult {
    let n0 = s.history()?.len() - states.len();
    for i in (0..states.len()).rev() {
        let label = &states[i].0;
        s.describe(&format!("Undo “{label}” with Cmd+Z"));
        s.key("Cmd+Z")?;
        s.wait_idle()?;
        let want = if i == 0 { start } else { &states[i - 1].1 };
        let got = snap(s)?;
        s.check_eq(
            &format!("after undoing “{label}” the document is as before it"),
            &got,
            want,
        )?;
        let len = s.history()?.len();
        s.check_eq("one step undone", len, n0 + i)?;
    }
    for (label, want) in states {
        s.describe(&format!("Redo “{label}” with Shift+Cmd+Z"));
        s.key("Shift+Cmd+Z")?;
        s.wait_idle()?;
        let got = snap(s)?;
        s.check_eq(&format!("redoing “{label}” brings it back"), &got, want)?;
    }
    Ok(())
}

fn new_document(s: &mut Session, w: u32, h: u32) -> UiResult {
    s.describe(&format!("Make a new {w} × {h} image with File ▸ New"));
    s.menu("File > New...")?;
    s.set_field("Width", &w.to_string())?;
    s.set_field("Height", &h.to_string())?;
    s.click("Create")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq(&format!("the new document is {w} × {h}"), size, (w, h))?;
    Ok(())
}

fn note_history(s: &mut Session) -> UiResult {
    let h = s.history()?;
    s.note(&format!("History: Open → {}", h.join(" → ")))
}

// ---- scenarios ------------------------------------------------------------------

fn undo_paint_and_layers(s: &mut Session) -> UiResult {
    new_document(s, 800, 600)?;
    let start = snap(s)?;
    let mut states = Vec::new();

    s.describe("Pick the Brush");
    s.click_role(Role::Button, "Brush")?;
    states.push(one_step(s, "brush stroke", |s| {
        s.describe("Paint a stroke across the canvas");
        s.canvas_stroke(&[(100.0, 150.0), (400.0, 300.0), (700.0, 200.0)], 8)
    })?);
    s.describe("Pick the Eraser");
    s.click("Eraser")?;
    states.push(one_step(s, "eraser stroke", |s| {
        s.describe("Erase across the stroke");
        s.canvas_drag((400.0, 100.0), (400.0, 500.0), 12, "")
    })?);
    states.push(one_step(s, "new layer", |s| {
        s.describe("Add a layer with Layer ▸ New pixel layer");
        s.menu("Layer > New pixel layer")
    })?);
    s.describe("Pick the Brush again");
    s.click_role(Role::Button, "Brush")?;
    states.push(one_step(s, "stroke on the new layer", |s| {
        s.describe("Paint on the new layer");
        s.canvas_drag((150.0, 450.0), (650.0, 450.0), 14, "")
    })?);
    states.push(one_step(s, "rename", |s| {
        s.describe("Double-click the layer to rename it");
        s.double_click("Layer Layer 2")?;
        s.set_field("Layer name", "Paint")
    })?);
    states.push(one_step(s, "opacity drag", |s| {
        s.describe("Drag the layer's Opacity down to 40% in Properties");
        s.within("Properties", |s| s.drag_slider("Opacity", 0.4))
    })?);
    states.push(one_step(s, "hide", |s| {
        s.describe("Hide the layer with its eye");
        s.click_offset("Layer Paint", 14.0, 19.0, "the eye")
    })?);
    states.push(one_step(s, "show", |s| {
        s.describe("Show it again");
        s.click_offset("Layer Paint", 14.0, 19.0, "the eye")
    })?);
    states.push(one_step(s, "move down", |s| {
        s.describe("Move it below the Background with the arrow button");
        s.click("Move layer down")
    })?);
    states.push(one_step(s, "duplicate", |s| {
        s.describe("Duplicate it with Layer ▸ Duplicate layer");
        s.menu("Layer > Duplicate layer")
    })?);
    states.push(one_step(s, "delete", |s| {
        s.describe("Delete the copy with the bin button");
        s.click("Delete layer")
    })?);
    note_history(s)?;
    undo_redo_all(s, &start, &states)
}
