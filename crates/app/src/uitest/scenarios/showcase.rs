//! The demo recording at the top of the README: a portrait edited the way
//! a user would, showing what Lumenply does in half a minute. Steps whose
//! description starts with "✦ " are the video's captions;
//! `scripts/make-demo-video.py` keeps those, crops the harness's own
//! caption bar and turns the session into the README's GIF and MP4.
//!
//! The photo is "Brunette woman portrait (Unsplash)", CC0, from Wikimedia
//! Commons, 1920 px wide, taken from `LUMENPLY_UITEST_PHOTOS/woman.jpg`
//! (the folder the AI journeys use); Select Subject needs the AI models
//! (`--models-from` or `LUMENPLY_UITEST_MODELS`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::uitest::prelude::*;

scenario_list! {
    "showcase" => showcase: "The README demo: AI Select Subject, a colour pop with a masked adjustment layer, Curves, a headline, non-destructive toggling, the command palette",
}

fn photo() -> UiResult<PathBuf> {
    std::env::var_os("LUMENPLY_UITEST_PHOTOS")
        .map(|d| PathBuf::from(d).join("woman.jpg"))
        .filter(|p| p.is_file())
        .ok_or_else(|| UiError("set LUMENPLY_UITEST_PHOTOS to the folder with woman.jpg".into()))
}

/// Let `secs` of real time pass with frames running, so the video holds
/// on a result.
fn linger(s: &mut Session, secs: f32) -> UiResult {
    let until = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(60));
        s.wait_frames(2)?;
    }
    Ok(())
}

/// Run `f` looking only inside Properties when it is a scroll area.
fn props<T>(s: &mut Session, f: impl FnOnce(&mut Session) -> UiResult<T>) -> UiResult<T> {
    if s.tree().matches("Properties", Some(Role::ScrollView)).is_empty() {
        f(s)
    } else {
        s.within("Properties", f)
    }
}

/// Click a control in the options bar under the menu bar.
fn bar_click(s: &mut Session, name: &str) -> UiResult {
    let band = match s.node("Tool options") {
        Some(n) => n.rect,
        None => eframe::egui::Rect::from_min_max(pos2(0.0, 40.0), pos2(10_000.0, 84.0)),
    };
    let at = s
        .tree()
        .matches(name, None)
        .into_iter()
        .find(|n| band.contains(n.rect.center()))
        .map(|n| n.rect.center())
        .ok_or_else(|| UiError(format!("no “{name}” in the options bar")))?;
    s.click_at(at, name)
}

fn showcase(s: &mut Session) -> UiResult {
    let file = s.copy_in(photo()?)?;
    s.describe("Click Open… on the welcome screen");
    s.click("Open…")?;
    s.describe("Pick the portrait in the system's open panel");
    s.choose_file(&file)?;
    s.wait_idle()?;
    // White type for the headline later: default colours, then swap.
    s.key("D")?;
    s.key("X")?;
    linger(s, 0.8)?;

    s.describe("✦ One click: on-device AI selects the subject");
    s.menu("Select > Subject")?;
    s.wait_idle()?;
    let picked = s.selection_bounds()?.is_some();
    s.check(
        "the subject is selected",
        picked,
        "a selection",
        format!("{picked}"),
    )?;
    linger(s, 1.2)?;

    s.describe("✦ Invert it, add Black & White: masked to the selection");
    s.key("Cmd+Shift+I")?;
    s.click_role(Role::Button, "B & W")?;
    s.wait_idle()?;
    s.key("Cmd+D")?;
    linger(s, 1.4)?;

    s.describe("✦ Curves: press on the line and drag, as in Photoshop");
    s.click_role(Role::Button, "Curves")?;
    s.wait_idle()?;
    props(s, |s| {
        s.drag_in("Curve, *", (0.75, 0.25), (0.75, 0.15), "the highlights up")
    })?;
    props(s, |s| {
        s.drag_in("Curve, *", (0.25, 0.75), (0.25, 0.84), "the shadows down")
    })?;
    s.wait_idle()?;
    linger(s, 1.0)?;

    s.describe("✦ Live, editable type in any installed font");
    s.click_role(Role::Button, "Text")?;
    bar_click(s, "Bold")?;
    bar_click(s, "Font size")?;
    s.key("Cmd+A")?;
    s.type_text("140")?;
    s.key("Enter")?;
    s.canvas_click((70.0, 120.0), "")?;
    s.type_text("Lumenply")?;
    s.key("Esc")?;
    s.wait_idle()?;
    linger(s, 1.2)?;

    s.describe("✦ Nothing is destroyed: switch the black & white off…");
    s.click("Show Black & White")?;
    s.wait_idle()?;
    linger(s, 1.2)?;
    s.describe("✦ …and on again. Every layer stays editable");
    s.click("Show Black & White")?;
    s.wait_idle()?;
    linger(s, 1.2)?;

    s.describe("✦ Cmd+K finds any of 200+ commands");
    s.click("Layer Background")?;
    s.key("Cmd+K")?;
    s.type_text("blur")?;
    linger(s, 1.8)?;
    s.key("Esc")?;
    s.wait_idle()?;

    s.note("✦ Lumenply: free, open source, Photoshop-familiar")?;
    linger(s, 2.0)?;
    Ok(())
}
