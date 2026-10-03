//! H. Viewing and the workspace: zoom, pan, Navigator, Info, Histogram,
//! Channels, History, proofing, the command palette, keyboard shortcuts,
//! Preferences, the welcome screen, the status bar and the layout.
//!
//! Every scenario reads the window's size from the session, so the same
//! journeys run at 1440×900, 1024×700 and 900×600 (`--size`).

use crate::uitest::prelude::*;
use eframe::egui::{Rect, Vec2};

scenario_list! {
    "h-zoom" => zoom: "Zoom with the View menu, keys and the zoom pill; fit, 100% and print size",
    "h-zoom-mode" => zoom_mode: "The Zoom mode of the Hand (Z): click zooms in at the pointer, Alt-click out",
    "h-pan" => pan: "Pan with Space-drag, the Hand and the wheel, without painting or editing",
    "h-navigator" => navigator: "Window ▸ Navigator: zoom by typing, pan by clicking, close, kept after a relaunch",
    "h-info" => info: "Window ▸ Info and the status bar read the colour and position under the pointer",
    "h-histogram" => histogram: "Find the histogram by its name and see it follow the image",
    "h-channels" => channels: "Channels panel: a colour channel alone, back to RGB, an alpha channel saved and loaded",
    "h-history" => history: "History: jump to a step, snapshots and the History Brush source",
    "h-proof" => proof: "View ▸ Proof colors and Gamut warning change the view, never the document",
    "h-palette" => palette: "The command palette: open with Cmd+K, search, run, blocked commands say why",
    "h-shortcuts" => shortcuts: "Help ▸ Keyboard shortcuts lists the keys that work",
    "h-rebind" => rebind: "Rebind a shortcut in Preferences, keep it after a relaunch, reset it",
    "h-preferences" => preferences: "Every setting in Preferences: save, cancel, and kept after a relaunch",
    "h-welcome-layout" => welcome_layout: "The welcome screen and the workspace fit the window without overlaps",
    "h-status-bar" => status_bar: "The status bar's facts, and messages that clear once they are out of date",
    "h-tooltips" => tooltips: "Tooltips of the workspace controls name what they do and their shortcuts",
}

// ---- helpers --------------------------------------------------------------------

/// Photoshop's zoom ladder (Zoom in / out and the Zoom tool's clicks step
/// through these), as fractions.
const LADDER: [f32; 21] = [
    0.05, 0.0625, 0.0833, 0.125, 0.1667, 0.25, 0.3333, 0.5, 0.6667, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0,
    8.0, 12.0, 16.0, 32.0,
];

fn step_up(z: f32) -> f32 {
    LADDER.iter().copied().find(|&p| p > z * 1.001).unwrap_or(32.0)
}

fn step_down(z: f32) -> f32 {
    LADDER
        .iter()
        .rev()
        .copied()
        .find(|&p| p < z / 1.001)
        .unwrap_or(0.05)
}

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn open_demo(s: &mut Session) -> UiResult {
    s.describe("Open the demo photo from the welcome screen");
    s.click("Open the demo photo")?;
    s.wait_idle()
}

fn new_image(s: &mut Session, w: u32, h: u32) -> UiResult {
    s.describe(&format!("Make a new {w} × {h} image with File ▸ New"));
    s.menu("File > New...")?;
    s.set_field("Width", &w.to_string())?;
    s.set_field("Height", &h.to_string())?;
    s.click("Create")?;
    s.wait_idle()
}

/// The canvas area (points).
fn canvas(s: &Session) -> UiResult<Rect> {
    s.tree()
        .matches("Canvas", None)
        .into_iter()
        .max_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
        .map(|n| n.rect)
        .ok_or_else(|| UiError("no canvas on screen".into()))
}

/// (zoom, pan) of the live document.
fn view(s: &mut Session) -> UiResult<(f32, Vec2)> {
    s.app(|a| (a.zoom, a.pan))
}

fn doc_size(s: &mut Session) -> UiResult<(f32, f32)> {
    s.doc(|d| (d.width as f32, d.height as f32))
}

/// The zoom "Fit on screen" gives on this canvas (a 40-point margin all
/// round, as the app lays it out).
fn fit_zoom(c: Rect, w: f32, h: f32) -> f32 {
    ((c.width() - 80.0) / w)
        .min((c.height() - 80.0) / h)
        .clamp(0.05, 32.0)
}

/// The document point at the centre of the canvas.
fn centre_doc(c: Rect, zoom: f32, pan: Vec2) -> (f32, f32) {
    (
        (c.width() / 2.0 - pan.x) / zoom,
        (c.height() / 2.0 - pan.y) / zoom,
    )
}

/// Edit ▸ Preferences..., the last item of a long menu: on a short
/// window the menu scrolls, so wheel down to it first.
fn open_preferences(s: &mut Session) -> UiResult {
    s.describe("Open the Edit menu");
    s.click("Edit")?;
    if !s.has_node("Preferences...") {
        s.describe("Scroll the Edit menu down to its end");
        s.scroll("Undo", 600.0)?;
    }
    s.click("Preferences...")
}

/// Scroll the list `list` (a named scroll area) with the wheel until a
/// control whose name starts with `prefix` shows wholly inside it.
fn scroll_to(s: &mut Session, list: &str, prefix: &str) -> UiResult {
    for _ in 0..12 {
        let view = s
            .node(list)
            .map(|n| n.rect)
            .ok_or_else(|| UiError(format!("no list “{list}”")))?;
        let item = s
            .tree()
            .nodes
            .iter()
            .find(|n| n.name.starts_with(prefix))
            .map(|n| n.rect)
            .ok_or_else(|| UiError(format!("nothing called “{prefix}…” in “{list}”")))?;
        if view.contains_rect(item) {
            return Ok(());
        }
        let dy = if item.center().y > view.center().y {
            120.0
        } else {
            -120.0
        };
        s.describe(&format!("Scroll “{list}” towards {prefix}"));
        s.scroll(list, dy)?;
    }
    Err(UiError(format!("“{prefix}…” never scrolled into “{list}”")))
}

fn history_len(s: &mut Session) -> UiResult<usize> {
    Ok(s.history()?.len())
}

/// The name the History strip gives step `i` (0 is the opened state).
fn step_name(i: usize, label: &str) -> String {
    format!("History step {i}: {label}")
}

fn tooltip(s: &mut Session, name: &str) -> UiResult<String> {
    Ok(s.hover(name)?.join(" "))
}

/// Check that a control is wholly inside the window.
fn on_screen(s: &mut Session, name: &str) -> UiResult<bool> {
    let size = s.window_size();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
    let r = s.node(name).map(|n| n.rect);
    s.check(
        &format!("“{name}” is wholly inside the window"),
        r.is_some_and(|r| screen.expand(0.5).contains_rect(r)),
        format!("inside {:.0}×{:.0}", size.x, size.y),
        format!("{r:?}"),
    )
}

/// Named controls on screen that overlap one another (a sign of a
/// layout that does not fit). Text labels and containers are left out.
fn overlapping_controls(s: &Session) -> Vec<String> {
    let size = s.window_size();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
    let interactive = |r: Role| {
        matches!(
            r,
            Role::Button
                | Role::CheckBox
                | Role::RadioButton
                | Role::Slider
                | Role::SpinButton
                | Role::ComboBox
                | Role::TextInput
                | Role::ColorWell
        )
    };
    let views: Vec<Rect> = s
        .tree()
        .nodes
        .iter()
        .filter(|n| n.role == Role::ScrollView)
        .map(|n| n.rect)
        .collect();
    // What shows of a control: a row scrolled half out of its list only
    // shows its visible part.
    let shown = |r: Rect| {
        let clip = views
            .iter()
            .filter(|v| v.contains(r.center()))
            .fold(screen, |c, v| c.intersect(*v));
        r.intersect(clip)
    };
    // Properties' contents come in the tree between its scroll area and
    // the splitter below it, and show only inside the scroll area.
    let nodes_all = &s.tree().nodes;
    let props_at = nodes_all
        .iter()
        .position(|n| n.name == "Properties" && n.role == Role::ScrollView);
    let split_at = nodes_all
        .iter()
        .position(|n| n.name == "Resize Properties and Layers");
    let in_props = |i: usize| matches!((props_at, split_at), (Some(a), Some(b)) if i > a && i < b);
    let props_rect = props_at.map(|i| nodes_all[i].rect);
    let nodes: Vec<(String, Rect)> = nodes_all
        .iter()
        .enumerate()
        .filter(|(_, n)| interactive(n.role) && !n.name.trim().is_empty())
        // The two colour wells overlap by design, as in Photoshop; rows of
        // a scrolled list run on under its edge, clipped.
        .filter(|(_, n)| n.role != Role::ColorWell)
        .filter(|(_, n)| {
            !["Layer ", "Channel ", "Alpha channel ", "Path "]
                .iter()
                .any(|p| n.name.starts_with(p))
        })
        .map(|(i, n)| {
            let r = match props_rect.filter(|_| in_props(i)) {
                Some(p) => n.rect.intersect(p),
                None => shown(n.rect),
            };
            (n.name.clone(), r)
        })
        .filter(|(_, r)| r.width() >= 1.0 && r.height() >= 1.0)
        .collect();
    let mut out = Vec::new();
    for (i, (an, a)) in nodes.iter().enumerate() {
        for (bn, b) in &nodes[i + 1..] {
            let o = a.intersect(*b);
            // One inside the other is a row and its part, not a clash.
            if o.width() > 2.0 && o.height() > 2.0 && !a.contains_rect(*b) && !b.contains_rect(*a) {
                out.push(format!("“{an}” × “{bn}”"));
            }
        }
    }
    out
}

// ---- zoom -----------------------------------------------------------------------

fn zoom(s: &mut Session) -> UiResult {
    open_demo(s)?;
    let (w, h) = doc_size(s)?;
    let c = canvas(s)?;
    let fit = fit_zoom(c, w, h);
    let steps = history_len(s)?;
    let (z, pan) = view(s)?;
    s.check(
        "the photo opens fitted to the window",
        near(z, fit, fit * 0.005) && near(pan.x, (c.width() - w * z) / 2.0, 2.0),
        format!("zoom {fit:.4}, centred"),
        format!("zoom {z:.4}, pan {pan:?}"),
    )?;
    s.expect_text(&format!("{:.0}%", z * 100.0))?;

    // The menu shows the keys Photoshop users know.
    s.describe("Open the View menu and read its zoom shortcuts");
    s.click("View")?;
    for keys in ["Cmd+=", "Cmd+−", "Cmd+0", "Cmd+1"] {
        let shown = s.text_shown(keys);
        s.check(
            &format!("the View menu shows {keys}"),
            shown,
            "shown next to its command",
            format!("{:?}", s.texts_matching("Cmd")),
        )?;
    }
    s.key("Esc")?;

    s.describe("Fit it exactly to this canvas with Cmd+0");
    s.key("Cmd+0")?;
    let (z, pan) = view(s)?;
    let mid = centre_doc(c, z, pan);
    s.menu("View > Zoom in")?;
    let (z1, p1) = view(s)?;
    let want = step_up(fit);
    let m1 = centre_doc(c, z1, p1);
    s.check(
        "View ▸ Zoom in steps to the next zoom level, about the centre",
        near(z1, want, 1e-3) && near(m1.0, mid.0, 1.0) && near(m1.1, mid.1, 1.0),
        format!("{:.2}% centred on {mid:?}", want * 100.0),
        format!("{:.2}% centred on {m1:?}", z1 * 100.0),
    )?;
    s.key("Cmd+=")?;
    let (z2, _) = view(s)?;
    s.check(
        "Cmd+= steps again (from a fitted 53% that is 100%)",
        near(z2, step_up(want), 1e-3),
        format!("{:.2}%", step_up(want) * 100.0),
        format!("{:.2}%", z2 * 100.0),
    )?;
    s.key("Cmd+-")?;
    let (z3, _) = view(s)?;
    s.check(
        "Cmd+− steps back down",
        near(z3, step_down(z2), 1e-3),
        format!("{:.2}%", step_down(z2) * 100.0),
        format!("{:.2}%", z3 * 100.0),
    )?;

    s.menu("View > Actual pixels")?;
    let (za, pa) = view(s)?;
    s.check(
        "View ▸ Actual pixels shows the photo at 100%, centred",
        za == 1.0 && near(pa.x, (c.width() - w) / 2.0, 0.5) && near(pa.y, (c.height() - h) / 2.0, 0.5),
        "100%, centred",
        format!("{:.2}%, pan {pa:?}", za * 100.0),
    )?;
    s.expect_text("100%")?;
    s.key("Cmd+0")?;
    let (zf, _) = view(s)?;
    s.check(
        "Cmd+0 fits it on screen",
        near(zf, fit, 1e-4),
        format!("{fit:.4}"),
        format!("{zf:.4}"),
    )?;
    s.key("Cmd+1")?;
    let (z100, _) = view(s)?;
    s.check_eq("Cmd+1 shows actual pixels", z100, 1.0)?;
    s.menu("View > Fit on screen")?;
    let (zf, _) = view(s)?;
    s.check(
        "View ▸ Fit on screen",
        near(zf, fit, 1e-4),
        format!("{fit:.4}"),
        format!("{zf:.4}"),
    )?;

    s.menu("View > Print size (approximate)")?;
    let (zp, _) = view(s)?;
    let want = crate::image_size_ui::print_size_zoom(72.0);
    s.check(
        "View ▸ Print size shows an inch of the 72 ppi photo as an inch of screen",
        near(zp, want, 1e-4),
        format!("{want:.4}"),
        format!("{zp:.4}"),
    )?;

    // The floating zoom pill in the canvas corner.
    s.describe("Fit again with the zoom pill's Fit");
    s.click("Fit")?;
    let (zf, _) = view(s)?;
    s.check(
        "the pill's Fit fits",
        near(zf, fit, 1e-4),
        format!("{fit:.4}"),
        format!("{zf:.4}"),
    )?;
    s.describe("Zoom in with the pill's +");
    s.click("Zoom in")?;
    let (zi, _) = view(s)?;
    s.check(
        "the pill's + steps like View ▸ Zoom in",
        near(zi, step_up(fit), 1e-3),
        format!("{:.2}%", step_up(fit) * 100.0),
        format!("{:.2}%", zi * 100.0),
    )?;
    s.describe("Zoom out with the pill's −");
    s.click("Zoom out")?;
    let (zo, _) = view(s)?;
    s.check(
        "the pill's − steps back",
        near(zo, step_down(zi), 1e-3),
        format!("{:.2}%", step_down(zi) * 100.0),
        format!("{:.2}%", zo * 100.0),
    )?;

    // Zooming is a view change, never an edit.
    let after = history_len(s)?;
    s.check_eq("zooming added no history step", after, steps)?;
    let title = s.title.clone();
    s.check(
        "the document is not marked as changed",
        !title.contains('•'),
        "no unsaved dot in the title",
        title,
    )?;
    Ok(())
}

fn zoom_mode(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.describe("Press Z for the Zoom mode");
    s.key("Z")?;
    let (tool, zm) = s.app(|a| (a.tool.name(), a.hand_zoom))?;
    s.check_eq("Z picks the Hand in Zoom mode", (tool, zm), ("Hand", true))?;
    s.expect_text("Zoom (Z)")?;
    let (z0, _) = view(s)?;
    let target = (1200.0, 400.0);
    let before = s.doc_to_screen(target.0, target.1)?;
    s.describe("Click the mountain to zoom in there");
    s.canvas_click(target, "")?;
    let (z1, _) = view(s)?;
    let after = s.doc_to_screen(target.0, target.1)?;
    s.check(
        "a click zooms one step in, keeping the clicked point under the pointer",
        near(z1, step_up(z0), 1e-3) && (after - before).length() < 1.0,
        format!("{:.2}% with the point at {before:?}", step_up(z0) * 100.0),
        format!("{:.2}% with the point at {after:?}", z1 * 100.0),
    )?;
    s.describe("Alt-click to zoom back out");
    s.canvas_click(target, "Alt")?;
    let (z2, _) = view(s)?;
    let back = s.doc_to_screen(target.0, target.1)?;
    s.check(
        "Alt-click zooms one step out about the same point",
        near(z2, step_down(z1), 1e-3) && (back - before).length() < 1.0,
        format!("{:.2}%", step_down(z1) * 100.0),
        format!("{:.2}% with the point at {back:?}", z2 * 100.0),
    )?;
    s.describe("Switch the Hand to Pan with the options bar");
    s.click("Pan (H)")?;
    let zm = s.app(|a| a.hand_zoom)?;
    s.check_eq("the Hand pans again", zm, false)?;
    let steps = history_len(s)?;
    s.check_eq("zooming made no history step", steps, 0)?;
    Ok(())
}

// ---- pan ------------------------------------------------------------------------

fn pan(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.describe("Zoom to 100% so there is room to pan");
    s.key("Cmd+1")?;
    s.describe("Pick the Brush, so a plain drag would paint");
    s.click_role(Role::Button, "Brush")?;
    let steps = history_len(s)?;
    let px = s.pixel(900, 500)?;
    let (z, p0) = view(s)?;
    let c = canvas(s)?;
    let m = centre_doc(c, z, p0);
    s.key_down("Space")?;
    s.describe("Space-drag across the canvas");
    s.canvas_drag(m, (m.0 - 200.0, m.1 - 100.0), 12, "")?;
    s.key_up("Space")?;
    let (_, p1) = view(s)?;
    let moved = p1 - p0;
    s.check(
        "Space-drag pans the view by the drag",
        near(moved.x, -200.0 * z, 1.0) && near(moved.y, -100.0 * z, 1.0),
        format!("({:.0}, {:.0})", -200.0 * z, -100.0 * z),
        format!("({:.0}, {:.0})", moved.x, moved.y),
    )?;
    let after = history_len(s)?;
    s.check_eq("and paints nothing", after, steps)?;
    let px2 = s.pixel(900, 500)?;
    s.check_eq("the photo is untouched", px2, px)?;

    s.describe("Press H for the Hand and drag");
    s.key("H")?;
    let (_, p2) = view(s)?;
    let m = centre_doc(c, z, p2);
    s.canvas_drag(m, (m.0 + 100.0, m.1 + 150.0), 12, "")?;
    let (_, p3) = view(s)?;
    let moved = p3 - p2;
    s.check(
        "the Hand pans by the drag",
        near(moved.x, 100.0 * z, 1.0) && near(moved.y, 150.0 * z, 1.0),
        format!("({:.0}, {:.0})", 100.0 * z, 150.0 * z),
        format!("({:.0}, {:.0})", moved.x, moved.y),
    )?;
    s.describe("Scroll the wheel down over the canvas");
    s.scroll("Canvas", 120.0)?;
    let (_, p4) = view(s)?;
    s.check(
        "the wheel pans the view up the page",
        near(p4.x, p3.x, 0.5) && near(p4.y, p3.y - 120.0, 3.0),
        format!("pan y {:.0}", p3.y - 120.0),
        format!("pan {p4:?}"),
    )?;
    let after = history_len(s)?;
    s.check_eq("panning made no history step", after, steps)?;
    Ok(())
}

// ---- Navigator and Info -----------------------------------------------------------

fn navigator(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.menu("Window > Navigator")?;
    let on = s.app(|a| a.prefs.panels.navigator)?;
    s.check_eq("the Navigator is on", on, true)?;
    s.expect_node("Navigator: click or drag to pan")?;
    s.describe("Type 200 in the Navigator's zoom field");
    s.set_field("Zoom percentage", "200")?;
    let (z, _) = view(s)?;
    s.check_eq("the canvas is at 200%", z, 2.0)?;

    // Click a quarter of the way across the picture: the canvas centres there.
    let (w, h) = doc_size(s)?;
    let area = s
        .node("Navigator: click or drag to pan")
        .map(|n| n.rect)
        .ok_or("no navigator")?;
    let (tw, th) = crate::navigator::nav_size(w as u32, h as u32);
    let img = Rect::from_center_size(area.center(), vec2(tw as f32, th as f32));
    let q = area.min + vec2(0.25 * area.width(), 0.5 * area.height());
    let want = ((q - img.min) / (img.width() / w)).to_pos2();
    s.describe("Click the left part of the Navigator's picture");
    s.click_in(
        "Navigator: click or drag to pan",
        0.25,
        0.5,
        "a quarter of the way across",
    )?;
    let c = canvas(s)?;
    let (z, p) = view(s)?;
    let m = centre_doc(c, z, p);
    s.check(
        "the canvas centres on the clicked point",
        near(m.0, want.x, 1.5) && near(m.1, want.y, 1.5),
        format!("({:.0}, {:.0})", want.x, want.y),
        format!("({:.0}, {:.0})", m.0, m.1),
    )?;
    s.describe("Zoom out with the Navigator's −");
    s.click("Navigator zoom out")?;
    let (z2, _) = view(s)?;
    s.check(
        "it steps out like View ▸ Zoom out",
        near(z2, step_down(2.0), 1e-3),
        format!("{:.0}%", step_down(2.0) * 100.0),
        format!("{:.1}%", z2 * 100.0),
    )?;
    s.describe("Close the Navigator");
    s.click("Close Navigator")?;
    let on = s.app(|a| a.prefs.panels.navigator)?;
    s.check_eq("the Navigator is off", on, false)?;
    s.menu("Window > Navigator")?;
    s.relaunch()?;
    open_demo(s)?;
    let on = s.app(|a| a.prefs.panels.navigator)?;
    s.check_eq("the Navigator is still open after a relaunch", on, true)?;
    s.expect_node("Navigator: click or drag to pan")?;
    Ok(())
}

fn info(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.menu("Window > Info")?;
    s.expect_text("INFO")?;
    let at = (900.0, 250.0);
    s.canvas_hover(at)?;
    // At a small zoom one screen point spans several pixels: read which
    // one the app says is under the pointer.
    let cur = s.app(|a| a.cursor_doc)?;
    s.check(
        "the pointer is over pixel (900, 250)",
        cur.is_some_and(|(x, y)| (x - 900).abs() <= 3 && (y - 250).abs() <= 3),
        "(900, 250), give or take the pixels one screen point spans",
        format!("{cur:?}"),
    )?;
    let (x, y) = cur.unwrap_or((900, 250));
    let px = s.pixel(x as u32, y as u32)?;
    let rgb = format!("R {:<3} G {:<3} B {:<3}", px[0], px[1], px[2]);
    s.expect_text(&rgb)?;
    for v in [px[0], px[1], px[2]] {
        s.expect_text(&v.to_string())?;
    }
    s.expect_text(&format!("x {x:<5} y {y:<5}"))?;
    s.expect_text("25.00 × 16.74 in")?;
    s.describe("Select a rectangle to see its size in Info");
    s.click("Rectangular Marquee")?;
    s.canvas_drag((200.0, 300.0), (520.0, 540.0), 10, "")?;
    s.wait_idle()?;
    // (Snap may pull an edge to a layer's edge nearby.)
    let (_, _, w, h) = s.selection_bounds()?.ok_or("no selection")?;
    s.expect_text(&w.to_string())?;
    s.expect_text(&h.to_string())?;
    s.expect_text(&format!("{w} × {h}"))?;
    s.describe("Close Info");
    s.click("Close Info")?;
    let on = s.app(|a| a.prefs.panels.info)?;
    s.check_eq("Info is off", on, false)?;
    Ok(())
}

fn histogram(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.describe("Look for the histogram in the command palette");
    s.key("Cmd+K")?;
    s.type_text("histogram")?;
    let found = !s.text_shown("No matching command");
    s.check(
        "the palette knows “histogram”",
        found,
        "a Histogram entry",
        format!("{:?}", s.texts_matching("istogram")),
    )?;
    s.key("Esc")?;
    s.describe("Look for it in the Window menu");
    s.menu("Window > Histogram")?;
    s.expect_node("Close Histogram")?;
    s.expect_node("Histogram of the image's luminance")?;
    let before = s
        .app(|a| crate::histogram::HistStats::of(&a.histogram))?
        .ok_or("an empty histogram")?;
    s.check_eq(
        "it counts every pixel of the 1800 × 1205 photo",
        before.pixels,
        1800 * 1205,
    )?;
    s.expect_text(&format!("{:.1}", before.mean))?;
    s.expect_text(&before.pixels.to_string())?;
    s.describe("Invert the photo's pixels (Cmd+I on the Background layer)");
    s.click("Layer Background")?;
    s.key("Cmd+I")?;
    s.wait_idle()?;
    let after = s
        .app(|a| crate::histogram::HistStats::of(&a.histogram))?
        .ok_or("an empty histogram")?;
    s.check(
        "the histogram follows the image: the mean flips about the middle",
        (after.mean - (255.0 - before.mean)).abs() < 30.0,
        format!("about {:.0}", 255.0 - before.mean),
        format!("{:.1}", after.mean),
    )?;
    s.expect_text(&format!("{:.1}", after.mean))?;
    s.describe("Close the Histogram");
    s.click("Close Histogram")?;
    let on = s.app(|a| a.prefs.panels.histogram)?;
    s.check_eq("the Histogram is off", on, false)?;
    Ok(())
}

// ---- Channels ---------------------------------------------------------------------

fn channels(s: &mut Session) -> UiResult {
    open_demo(s)?;
    let steps = history_len(s)?;
    let px = s.pixel(900, 250)?;
    s.click("Channels panel")?;
    s.describe("Click the Red channel to see it alone");
    s.click("Channel Red")?;
    let v = s.app(|a| format!("{:?}", a.panels.view))?;
    s.check_eq("the canvas shows the red channel", v, "Red".to_string())?;
    // The way back is spelled as the app spells every shortcut.
    let back = if cfg!(target_os = "macos") {
        "Cmd+2"
    } else {
        "Ctrl+2"
    };
    s.expect_text(&format!("Viewing the red channel alone ({back} returns to RGB)"))?;
    let px2 = s.pixel(900, 250)?;
    s.check_eq("the document itself is unchanged", px2, px)?;
    s.describe("Back to RGB with Cmd+2");
    s.key("Cmd+2")?;
    let v = s.app(|a| format!("{:?}", a.panels.view))?;
    s.check_eq("all channels again", v, "Composite".to_string())?;
    s.key("Cmd+4")?;
    let v = s.app(|a| format!("{:?}", a.panels.view))?;
    s.check_eq("Cmd+4 shows green alone", v, "Green".to_string())?;
    s.click("Channel RGB")?;
    let after = history_len(s)?;
    s.check_eq("viewing channels is not an edit", after, steps)?;

    s.click("Rectangular Marquee")?;
    s.describe("Select the mountain");
    s.canvas_drag((300.0, 200.0), (900.0, 700.0), 10, "")?;
    // (Snap pulls the marquee's edges to the demo's layer edges nearby.)
    let dragged = s.selection_bounds()?;
    s.check(
        "a rectangle is selected",
        dragged.is_some(),
        "a selection",
        format!("{dragged:?}"),
    )?;
    s.describe("Save the selection as an alpha channel");
    s.click("Save selection as channel")?;
    s.wait_idle()?;
    let saved = s.doc(|d| {
        d.saved_selections
            .iter()
            .map(|x| x.name.clone())
            .collect::<Vec<_>>()
    })?;
    s.check_eq("one alpha channel", saved.len(), 1)?;
    let name = saved.first().cloned().unwrap_or_default();
    s.expect_node(&format!("Alpha channel {name}"))?;
    s.describe("Deselect");
    s.key("Cmd+D")?;
    let b = s.selection_bounds()?;
    s.check_eq("nothing selected", b, None)?;
    s.describe("Cmd-click the alpha channel to load it");
    s.click_with(&format!("Alpha channel {name}"), "Cmd")?;
    let b = s.selection_bounds()?;
    s.check_eq("the same selection is back", b, dragged)?;
    s.describe("Click the alpha channel to see it");
    s.click(&format!("Alpha channel {name}"))?;
    let v = s.app(|a| format!("{:?}", a.panels.view))?;
    s.check_eq("the canvas shows the alpha channel", v, "Alpha(0)".to_string())?;
    s.click("Channel RGB")?;
    s.describe("Undo three times: the load, the deselect and the save");
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    let n = s.doc(|d| d.saved_selections.len())?;
    s.check_eq("undo removes the alpha channel", n, 0)?;
    Ok(())
}

// ---- History ----------------------------------------------------------------------

fn history(s: &mut Session) -> UiResult {
    new_image(s, 800, 600)?;
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint three strokes");
    s.canvas_drag((100.0, 100.0), (700.0, 100.0), 12, "")?;
    s.canvas_drag((100.0, 300.0), (700.0, 300.0), 12, "")?;
    s.canvas_drag((100.0, 500.0), (700.0, 500.0), 12, "")?;
    s.wait_idle()?;
    let labels = s.history()?;
    let n = labels.len();
    s.note(&format!("History: {}", labels.join(" → ")))?;
    let white = [255u8, 255, 255, 255];
    let first = n - 3;
    // The strip names each card by its number and label, so a screen
    // reader (and this scenario) never confuses it with a menu.
    let name = step_name(first + 1, &labels[first]);
    s.describe("Click the first stroke's card in the History strip");
    s.click(&name)?;
    let now = history_len(s)?;
    s.check_eq("history is back at the first stroke", now, first + 1)?;
    let (a, b, c) = (s.pixel(400, 100)?, s.pixel(400, 300)?, s.pixel(400, 500)?);
    s.check(
        "the first stroke stays, the others are gone",
        a != white && b == white && c == white,
        "stroke, white, white",
        format!("{a:?} {b:?} {c:?}"),
    )?;
    let redo = s.app(|a| a.editor.redo_history().len())?;
    s.check_eq("the two later strokes wait as redo steps", redo, 2)?;

    s.describe("Right-click that card for its menu");
    s.right_click(&name)?;
    s.click("New snapshot")?;
    let snaps = s.app(|a| a.snapshots.len())?;
    s.check_eq("a snapshot is kept", snaps, 1)?;
    s.expect_node("History snapshot: Snapshot 1")?;
    let last = step_name(n, &labels[n - 1]);
    s.describe("Jump to the last step again");
    s.click(&last)?;
    let c = s.pixel(400, 500)?;
    s.check(
        "all three strokes are back",
        c != white,
        "a stroke",
        format!("{c:?}"),
    )?;

    s.describe("Click the snapshot to go back to it");
    s.click("History snapshot: Snapshot 1")?;
    let (b, c) = (s.pixel(400, 300)?, s.pixel(400, 500)?);
    s.check_eq("the snapshot's state is back", (b, c), (white, white))?;
    s.describe("Undo the restore");
    s.key("Cmd+Z")?;
    let c = s.pixel(400, 500)?;
    s.check(
        "undo brings the strokes back",
        c != white,
        "a stroke",
        format!("{c:?}"),
    )?;

    // The History Brush paints from the chosen step.
    s.describe("Make the opened state the History Brush's source");
    s.right_click(&step_name(0, "Open"))?;
    s.click("Use as history brush source")?;
    let src = s.app(|a| a.history_source_step())?;
    s.check_eq("the source is step 0", src, 0)?;
    s.describe("Pick the History Brush from the command palette");
    s.key("Cmd+K")?;
    s.type_text("history brush")?;
    s.key("Enter")?;
    s.describe("Paint over the middle stroke");
    s.canvas_drag((150.0, 300.0), (650.0, 300.0), 14, "")?;
    s.wait_idle()?;
    let (a, b) = (s.pixel(400, 100)?, s.pixel(400, 300)?);
    s.check(
        "the middle stroke is painted away, the top one stays",
        a != white && b == white,
        "stroke, white",
        format!("{a:?} {b:?}"),
    )?;

    s.describe("Fold the History strip with its header");
    s.click("History, step *")?;
    let folded = s.app(|a| a.prefs.history_collapsed)?;
    s.check_eq("the strip is folded", folded, true)?;
    s.click("History, step *")?;
    let folded = s.app(|a| a.prefs.history_collapsed)?;
    s.check_eq("and open again", folded, false)?;
    Ok(())
}

// ---- proofing ---------------------------------------------------------------------

fn proof(s: &mut Session) -> UiResult {
    open_demo(s)?;
    let steps = history_len(s)?;
    let px = s.pixel(900, 250)?;
    s.menu("View > Proof colors")?;
    let on = s.app(|a| (a.proof_colors, a.gamut_warning))?;
    s.check_eq("Proof colors is on", on, (true, false))?;
    s.expect_text(crate::soft_proof::PROOF_NOTE)?;
    let px2 = s.pixel(900, 250)?;
    s.check_eq("the document's colours are untouched", px2, px)?;
    s.describe("Turn it off with Cmd+Y");
    s.key("Cmd+Y")?;
    let on = s.app(|a| a.proof_colors)?;
    s.check_eq("Proof colors is off", on, false)?;
    s.menu("View > Gamut warning")?;
    let on = s.app(|a| a.gamut_warning)?;
    s.check_eq("Gamut warning is on", on, true)?;
    s.describe("Turn it off with Shift+Cmd+Y");
    s.key("Cmd+Shift+Y")?;
    let on = s.app(|a| a.gamut_warning)?;
    s.check_eq("Gamut warning is off", on, false)?;
    let after = history_len(s)?;
    s.check_eq("proofing is not an edit", after, steps)?;
    Ok(())
}

// ---- the command palette ------------------------------------------------------------

fn palette(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.key("Cmd+1")?;
    s.describe("Open the command palette with Cmd+K");
    s.key("Cmd+K")?;
    s.expect_node("Search commands")?;
    s.type_text("fit")?;
    s.describe("Run the first match with Enter");
    s.key("Enter")?;
    let (w, h) = doc_size(s)?;
    let fit = fit_zoom(canvas(s)?, w, h);
    let (z, _) = view(s)?;
    s.check(
        "“fit” + Enter fits the photo",
        near(z, fit, 1e-4),
        format!("{fit:.4}"),
        format!("{z:.4}"),
    )?;
    let open = s.app(|a| a.palette.is_some())?;
    s.check_eq("the palette closed", open, false)?;

    if s.has_node("Search tools, filters and commands") {
        s.describe("Open it from the search box in the top bar");
        s.click("Search tools, filters and commands")?;
    } else {
        s.note("(no room for the search box in the top bar at this width)")?;
        s.menu("Help > Search commands...")?;
    }
    s.type_text("navigator")?;
    s.expect_node("Show or hide the Navigator")?;
    s.describe("Esc closes it without running anything");
    s.key("Esc")?;
    let (open, nav) = s.app(|a| (a.palette.is_some(), a.prefs.panels.navigator))?;
    s.check_eq("closed, nothing changed", (open, nav), (false, false))?;

    s.key("Cmd+K")?;
    s.type_text("deselect")?;
    s.expect_text("Make a selection first")?;
    s.describe("Enter on a command that can't run");
    s.key("Enter")?;
    let open = s.app(|a| a.palette.is_some())?;
    s.check_eq("the palette stays open", open, true)?;
    s.key("Esc")?;
    // Words a Photoshop user types.
    for (word, entry) in [
        ("100", "Actual pixels (100%)"),
        ("channels", "Channels panel"),
        ("preferences", "Preferences..."),
        ("shortcuts", "Keyboard shortcuts"),
        ("proof", "Proof colors (CMYK: U.S. Web Coated SWOP)"),
    ] {
        s.key("Cmd+K")?;
        s.type_text(word)?;
        let found = s.has_node(entry);
        let rows: Vec<String> = s
            .tree()
            .nodes
            .iter()
            .filter(|n| n.role == Role::Button && n.rect.min.y > 100.0 && n.rect.width() > 400.0)
            .map(|n| n.name.clone())
            .collect();
        s.check(
            &format!("“{word}” finds {entry}"),
            found,
            entry,
            format!("{rows:?}"),
        )?;
        s.key("Esc")?;
    }
    Ok(())
}

// ---- keyboard shortcuts ----------------------------------------------------------------

/// The keys a row of the Keyboard shortcuts dialog shows for `what`.
fn shortcut_row(s: &Session, what: &str) -> Option<String> {
    let labels: Vec<_> = s.tree().nodes.iter().filter(|n| n.role == Role::Label).collect();
    let row = labels.iter().find(|n| n.name == what)?;
    labels
        .iter()
        .filter(|n| (n.rect.center().y - row.rect.center().y).abs() < 3.0 && n.rect.min.x > row.rect.max.x)
        .min_by(|a, b| a.rect.min.x.total_cmp(&b.rect.min.x))
        .map(|n| n.name.clone())
}

fn shortcuts(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.menu("Help > Keyboard shortcuts")?;
    s.expect_text("COMMANDS")?;
    for (what, keys) in [
        ("Hand", "H"),
        ("Zoom", "Z"),
        ("Zoom in", "Cmd+="),
        ("Fit on screen", "Cmd+0"),
        ("Actual pixels (100%)", "Cmd+1"),
        ("Show or hide rulers", "Cmd+R"),
        ("View the RGB composite", "Cmd+2"),
        ("Search commands...", "Cmd+K"),
        ("Preferences...", "Cmd+,"),
    ] {
        let shown = shortcut_row(s, what);
        s.check_eq(
            &format!("the list shows {keys} for {what}"),
            shown,
            Some(keys.to_string()),
        )?;
    }
    s.describe("Esc closes the list");
    s.key("Esc")?;
    let open = s.app(|a| a.dialog.is_some())?;
    s.check_eq("closed", open, false)?;
    // The keys it lists do what it says.
    s.key("Cmd+R")?;
    let rulers = s.app(|a| a.prefs.show_rulers)?;
    s.check_eq("Cmd+R shows the rulers", rulers, true)?;
    s.key("Cmd+R")?;
    s.key("Cmd+,")?;
    let prefs = s.app(|a| matches!(a.dialog, Some(crate::dialogs::Dialog::Preferences(..))))?;
    s.check_eq("Cmd+, opens Preferences", prefs, true)?;
    s.key("Esc")?;
    Ok(())
}

fn rebind(s: &mut Session) -> UiResult {
    open_demo(s)?;
    open_preferences(s)?;
    s.describe("Click the Proof colors shortcut to change it");
    scroll_to(s, "Shortcuts", "Shortcut for Proof colors")?;
    s.click("Shortcut for Proof colors*")?;
    s.expect_text("press keys…")?;
    s.describe("Press Shift+Cmd+P");
    s.key("Cmd+Shift+P")?;
    scroll_to(s, "Shortcuts", "Shortcut for Proof colors")?;
    s.expect_node("Shortcut for Proof colors: Shift+Cmd+P")?;
    s.describe("Try a chord that Save already uses");
    scroll_to(s, "Shortcuts", "Shortcut for Gamut warning")?;
    s.click("Shortcut for Gamut warning*")?;
    s.key("Cmd+S")?;
    s.expect_text("Cmd+S is already the shortcut for Save")?;
    scroll_to(s, "Shortcuts", "Shortcut for Gamut warning")?;
    s.expect_node("Shortcut for Gamut warning: Shift+Cmd+Y")?;
    s.describe("A chord with Option keeps the Option");
    scroll_to(s, "Shortcuts", "Shortcut for Gamut warning")?;
    s.click("Shortcut for Gamut warning*")?;
    s.key("Cmd+Alt+G")?;
    scroll_to(s, "Shortcuts", "Shortcut for Gamut warning")?;
    s.expect_node("Shortcut for Gamut warning: Option+Cmd+G")?;
    s.click("Save")?;
    s.describe("Press the new keys");
    s.key("Cmd+Shift+P")?;
    let on = s.app(|a| a.proof_colors)?;
    s.check_eq("Shift+Cmd+P turns Proof colors on", on, true)?;
    s.key("Cmd+Alt+G")?;
    let on = s.app(|a| a.gamut_warning)?;
    s.check_eq("Option+Cmd+G turns Gamut warning on", on, true)?;
    s.describe("The old Cmd+Y no longer toggles proofing");
    s.key("Cmd+Y")?;
    let on = s.app(|a| a.proof_colors)?;
    s.check_eq("still on", on, true)?;
    s.click("View")?;
    s.expect_text("Shift+Cmd+P")?;
    s.key("Esc")?;

    s.relaunch()?;
    open_demo(s)?;
    s.describe("After a relaunch the new keys still work");
    s.key("Cmd+Shift+P")?;
    let on = s.app(|a| a.proof_colors)?;
    s.check_eq("Shift+Cmd+P still toggles Proof colors", on, true)?;
    open_preferences(s)?;
    s.describe("Reset the Proof colors shortcut");
    scroll_to(s, "Shortcuts", "Reset shortcut for Proof colors")?;
    s.click("Reset shortcut for Proof colors")?;
    scroll_to(s, "Shortcuts", "Reset shortcut for Gamut warning")?;
    s.click("Reset shortcut for Gamut warning")?;
    s.click("Save")?;
    s.key("Cmd+Y")?;
    let on = s.app(|a| a.proof_colors)?;
    s.check_eq("Cmd+Y toggles Proof colors again", on, false)?;
    Ok(())
}

// ---- Preferences ------------------------------------------------------------------------

fn preferences(s: &mut Session) -> UiResult {
    open_demo(s)?;
    open_preferences(s)?;
    s.set_field("Undo steps", "20")?;
    s.set_field("Undo memory", "512")?;
    s.set_field("Autosave every", "300")?;
    s.set_field("Render cache", "512")?;
    s.click("Show its memory use in the status bar")?;
    s.click("Light")?;
    s.set_field("Line every", "50")?;
    s.set_field("Subdivisions", "2")?;
    s.click("Save")?;
    s.expect_text("Preferences saved")?;
    let got = s.app(|a| {
        let p = &a.prefs;
        (
            p.undo_steps,
            p.undo_memory_mb,
            p.autosave_secs,
            p.render_cache_mb,
            p.show_render_cache,
            p.canvas_bg,
            p.grid_spacing,
            p.grid_subdivisions,
            a.editor.history_limit,
            a.editor.history_memory_limit >> 20,
        )
    })?;
    let want = (20, 512, 300, 512, true, [0xD8, 0xD8, 0xD8], 50.0, 2, 20, 512);
    s.check_eq("every setting took effect", got, want)?;
    let cache = s.texts_matching("Cache ");
    s.check(
        "the status bar shows the render cache",
        !cache.is_empty(),
        "Cache N MB",
        format!("{cache:?}"),
    )?;

    open_preferences(s)?;
    s.describe("Change Undo steps, then cancel with Esc");
    s.set_field("Undo steps", "70")?;
    s.key("Esc")?;
    let steps = s.app(|a| a.prefs.undo_steps)?;
    s.check_eq("cancelled: Undo steps stays 20", steps, 20)?;

    s.relaunch()?;
    let got = s.app(|a| {
        let p = &a.prefs;
        (
            p.undo_steps,
            p.undo_memory_mb,
            p.autosave_secs,
            p.render_cache_mb,
            p.show_render_cache,
            p.canvas_bg,
            p.grid_spacing,
            p.grid_subdivisions,
        )
    })?;
    s.check_eq(
        "the settings survive a relaunch",
        got,
        (20, 512, 300, 512, true, [0xD8, 0xD8, 0xD8], 50.0, 2),
    )?;
    open_demo(s)?;
    let limit = s.app(|a| a.editor.history_limit)?;
    s.check_eq("and apply to the next document", limit, 20)?;
    Ok(())
}

// ---- welcome screen and layout ------------------------------------------------------------

fn welcome_layout(s: &mut Session) -> UiResult {
    s.expect_text("No document open")?;
    for name in [
        "New image…",
        "Open…",
        "Open the demo photo",
        "File",
        "Help",
        "Export",
    ] {
        on_screen(s, name)?;
    }
    let clash = overlapping_controls(s);
    s.check(
        "nothing overlaps on the welcome screen",
        clash.is_empty(),
        "no overlaps",
        format!("{clash:?}"),
    )?;
    s.screenshot("welcome");
    open_demo(s)?;
    for name in [
        "Move",
        "Hand",
        "Eyedropper",
        "Foreground colour *",
        "View",
        "Window",
        "Export",
        "Layers panel",
        "Channels panel",
        "Delete layer",
        "History, step *",
        "Fit",
    ] {
        on_screen(s, name)?;
    }
    let clash = overlapping_controls(s);
    s.check(
        "nothing overlaps in the workspace",
        clash.is_empty(),
        "no overlaps",
        format!("{clash:?}"),
    )?;
    s.screenshot("workspace");
    // The dialogs of this area fit too.
    open_preferences(s)?;
    on_screen(s, "Save")?;
    on_screen(s, "Cancel")?;
    s.screenshot("preferences");
    s.key("Esc")?;
    s.menu("Help > Keyboard shortcuts")?;
    on_screen(s, "Keyboard shortcuts")?;
    s.screenshot("keyboard-shortcuts");
    s.key("Esc")?;
    s.menu("Window > Navigator")?;
    s.menu("Window > Info")?;
    on_screen(s, "Close Info")?;
    on_screen(s, "Zoom percentage")?;
    let clash = overlapping_controls(s);
    s.check(
        "Navigator and Info overlap nothing",
        clash.is_empty(),
        "no overlaps",
        format!("{clash:?}"),
    )?;
    s.screenshot("navigator-info");
    Ok(())
}

// ---- the status bar ---------------------------------------------------------------------------

fn status_bar(s: &mut Session) -> UiResult {
    open_demo(s)?;
    s.expect_text("1800 × 1205 px")?;
    s.expect_text("72 ppi")?;
    s.expect_text("8 layers")?;
    s.expect_text("No selection")?;
    let tip = tooltip(s, "72 ppi")?;
    s.check(
        "the resolution's tooltip gives the print size",
        tip.contains("Prints at 25.00 × 16.74 in at 72 ppi"),
        "Prints at 25.00 × 16.74 in at 72 ppi …",
        tip,
    )?;
    s.expect_text("Opened the demo")?;
    // A message describes what just happened; the next edit replaces or
    // clears it.
    s.describe("Add a layer with the Layer menu");
    s.menu("Layer > New pixel layer")?;
    let stale = s.text_shown("Opened the demo");
    s.check(
        "the opening message is gone after an edit",
        !stale,
        "cleared",
        format!("{:?}", s.texts_matching("Opened")),
    )?;
    s.describe("Undo it with Cmd+Z");
    s.key("Cmd+Z")?;
    s.expect_text("Undid")?;
    s.describe("Add a layer again");
    s.menu("Layer > New pixel layer")?;
    let stale = s.text_shown("Undid");
    s.check(
        "“Undid …” is gone after the next edit",
        !stale,
        "cleared",
        format!("{:?}", s.texts_matching("Undid")),
    )?;
    let top = s.layer_names()?.first().cloned().unwrap_or_default();
    s.describe("Rename it by double-clicking");
    s.double_click(&format!("Layer {top}"))?;
    s.set_field("Layer name", "Notes")?;
    s.expect_node("Layer Notes")?;
    s.expect_text("9 layers")?;
    s.describe("Undo twice, then redo");
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    s.key("Cmd+Shift+Z")?;
    s.expect_text("Redid")?;
    let undid = s.text_shown("Undid");
    s.check_eq("only the latest message shows", undid, false)?;
    s.click("Rectangular Marquee")?;
    s.canvas_drag((100.0, 100.0), (300.0, 250.0), 8, "")?;
    let (_, _, w, h) = s.selection_bounds()?.ok_or("no selection")?;
    s.expect_text(&format!("{w} × {h}"))?;
    let redid = s.text_shown("Redid");
    s.check_eq("the selection replaced the redo message", redid, false)?;
    Ok(())
}

// ---- tooltips -----------------------------------------------------------------------------------

fn tooltips(s: &mut Session) -> UiResult {
    open_demo(s)?;
    for (name, want) in [
        // (Neighbours apart: egui hands a showing tooltip straight on to
        // the next control, which the harness can't tell from no change.)
        ("Hand", "Hand (H)"),
        ("Zoom in", "Cmd+="),
        ("History, step *", "Fold the history strip"),
        ("Zoom out", "Cmd+−"),
        ("72 ppi", "Prints at"),
        ("Fit", "Cmd+0"),
    ] {
        let tip = tooltip(s, name)?;
        s.check(
            &format!("“{name}” explains itself"),
            tip.contains(want),
            format!("a tooltip with “{want}”"),
            tip,
        )?;
    }
    s.key("H")?;
    if s.window_size().x < 1100.0 {
        // The options bar's tight layout keeps tool hints in tooltips.
        return s.note("(the Hand's hint is in a tooltip at this width)");
    }
    let hint = s.texts_matching("scroll");
    s.check(
        "the Hand's hint says what the wheel does",
        hint.iter().any(|t| t.contains("Alt+scroll")) && !hint.iter().any(|t| t.contains("scroll to zoom")),
        "scroll pans, Alt+scroll zooms",
        format!("{hint:?}"),
    )?;
    Ok(())
}
