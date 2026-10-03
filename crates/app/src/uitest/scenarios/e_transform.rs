//! E. Moving, transforming and the canvas: the Move tool (drag, nudges,
//! Alt-drag copies, auto-select, Smart Guides), Free Transform (scale,
//! rotate, skew, perspective, warp, typed values, Enter and Esc), the
//! Crop tool and Perspective Crop, Liquify, Puppet Warp, Content-Aware
//! Scale, and rulers, guides and the grid.
//!
//! Most scenarios open `shapes.ora`, a small layered file the "user" has
//! on disk: a white 800 × 600 Background, a red 200 × 100 block ("Red")
//! at (100, 100) and a blue 100 × 100 square ("Blue") at (500, 300).

use crate::uitest::prelude::*;
use lumenply_doc::Document;
use lumenply_tiles::{Raster, Rgba, TileStore};

scenario_list! {
    "move-drag-and-nudge" => move_drag_and_nudge: "Move tool: drag a layer, nudge it with the arrows, undo each",
    "move-alt-drag-copy" => move_alt_drag_copy: "Move tool: Alt-drag moves a copy, the original stays",
    "move-auto-select" => move_auto_select: "Move tool: Cmd-click and Auto-Select pick the layer under the pointer",
    "move-smart-guides" => move_smart_guides: "Move tool: Smart Guides snap edges together, Cmd turns snapping off",
    "transform-scale" => transform_scale: "Free Transform: corner scale, Shift for free aspect, Esc cancels",
    "transform-typed" => transform_typed: "Free Transform: typed width, rotation and skew, Enter applies",
    "transform-rotate" => transform_rotate: "Free Transform: rotate by dragging outside, Shift for 15° steps",
    "transform-perspective-warp" => transform_perspective_warp: "Edit ▸ Perspective and Edit ▸ Warp by dragging corners and mesh points",
    "crop-ratio" => crop_ratio: "Crop tool: a 1:1 ratio, a corner drag, Enter crops, undo",
    "crop-straighten" => crop_straighten: "Crop tool: straighten by dragging outside the frame; delete cropped pixels",
    "perspective-crop" => perspective_crop: "Crop ▸ Perspective: four corners rectify the image",
    "liquify" => liquify: "Filter ▸ Liquify: push pixels, OK applies one undo step",
    "puppet-warp" => puppet_warp: "Edit ▸ Puppet Warp: pins and a drag bend the layer",
    "content-aware-scale" => content_aware_scale: "Edit ▸ Content-Aware Scale: typed width, Apply",
    "rulers-guides-grid" => rulers_guides_grid: "Rulers, guides dragged out, moved and deleted, the grid and snapping",
    "big-document" => big_document: "A 4000 × 3000 document: moving and transforming a large layer",
}

// ---- the user's files -----------------------------------------------------

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A document of solid blocks, bottom first: (name, x, y, w, h, colour).
fn blocks_doc(w: u32, h: u32, blocks: &[(&str, i32, i32, u32, u32, Rgba)]) -> Document {
    let mut d = Document::new(w, h);
    for &(name, x, y, bw, bh, c) in blocks {
        let id = d.add_pixel_layer(name);
        if let Some(l) = d.layer_mut(id) {
            l.content =
                lumenply_doc::LayerContent::Pixel(TileStore::from_raster(&Raster::filled(bw, bh, c), x, y));
        }
    }
    d
}

fn write_ora(path: &std::path::Path, doc: &Document) -> UiResult {
    lumenply_io::ora::save(path, doc)
        .map(|_| ())
        .map_err(|e| UiError(format!("writing {}: {e}", path.display())))
}

/// `shapes.ora` (see the module comment) in the session's files.
fn shapes_file(s: &Session) -> UiResult<std::path::PathBuf> {
    let path = s.files().join("shapes.ora");
    let white = Rgba::new(1.0, 1.0, 1.0, 1.0);
    let doc = blocks_doc(
        800,
        600,
        &[
            ("Background", 0, 0, 800, 600, white),
            ("Red", 100, 100, 200, 100, Rgba::new(1.0, 0.0, 0.0, 1.0)),
            ("Blue", 500, 300, 100, 100, Rgba::new(0.0, 0.0, 1.0, 1.0)),
        ],
    );
    write_ora(&path, &doc)?;
    Ok(path)
}

/// Open `shapes.ora` from the welcome screen.
fn open_shapes(s: &mut Session) -> UiResult {
    let path = shapes_file(s)?;
    s.describe("Click Open… on the welcome screen");
    s.click("Open…")?;
    s.describe("Pick shapes.ora in the system's open panel");
    s.choose_file(&path)?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq(
        "the three layers opened",
        names,
        vec!["Blue".to_string(), "Red".to_string(), "Background".to_string()],
    )?;
    Ok(())
}

/// A layer's painted bounds (x, y, w, h).
fn bounds(s: &mut Session, layer: &str) -> UiResult<Option<(i32, i32, u32, u32)>> {
    let layer = layer.to_string();
    s.doc(move |d| {
        let l = find_layer(d, &layer)?;
        let b = l.raster_store()?.content_bounds()?;
        Some((b.x, b.y, b.w, b.h))
    })
}

fn near(got: [u8; 4], want: [u8; 4], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

fn check_px(s: &mut Session, what: &str, x: u32, y: u32, want: [u8; 4]) -> UiResult<bool> {
    let got = s.pixel(x, y)?;
    s.check(what, near(got, want, 2), format!("{want:?}"), format!("{got:?}"))
}

// ---- the Move tool ----------------------------------------------------------

fn move_drag_and_nudge(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer in the Layers panel");
    s.click("Layer Red")?;
    s.describe("Pick the Move tool from the toolbar");
    s.click("Move")?;
    let steps = s.history()?.len();

    s.describe("Drag the red block 60 px right and 40 px down");
    s.canvas_drag((200.0, 150.0), (260.0, 190.0), 12, "")?;
    s.wait_idle()?;
    let b = bounds(s, "Red")?;
    s.check_eq("the red block moved by (60, 40)", b, Some((160, 140, 200, 100)))?;
    check_px(s, "red where the block landed", 350, 230, RED)?;
    check_px(s, "white where it was", 110, 110, WHITE)?;
    let n = s.history()?.len();
    s.check_eq("one history step for the drag", n, steps + 1)?;

    s.describe("Nudge it right three times with the arrow key");
    s.key("Right")?;
    s.key("Right")?;
    s.key("Right")?;
    s.describe("And 10 px down with Shift+Down");
    s.key("Shift+Down")?;
    s.wait_idle()?;
    let b = bounds(s, "Red")?;
    s.check_eq("nudged by (3, 10)", b, Some((163, 150, 200, 100)))?;
    let hist = s.history()?;
    s.note(&format!("History after nudging: {}", hist.join(" → ")))?;

    s.describe("Undo the nudges");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let b = bounds(s, "Red")?;
    s.check_eq("the nudges are undone together", b, Some((160, 140, 200, 100)))?;
    s.describe("Undo the drag");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let b = bounds(s, "Red")?;
    s.check_eq("back where it started", b, Some((100, 100, 200, 100)))?;
    check_px(s, "red at the start again", 110, 110, RED)?;
    Ok(())
}

/// The active layer's name.
fn active_name(s: &mut Session) -> UiResult<Option<String>> {
    s.app(|a| {
        a.active
            .and_then(|id| a.editor.doc().layer(id).map(|l| l.name.clone()))
    })
}

fn move_alt_drag_copy(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Blue layer");
    s.click("Layer Blue")?;
    s.describe("Pick the Move tool with its key, V");
    s.key("V")?;
    let steps = s.history()?.len();
    s.describe("Alt-drag the blue square 150 px to the left");
    s.canvas_drag((550.0, 350.0), (400.0, 350.0), 14, "Alt")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq(
        "a copy sits above the original",
        names,
        vec![
            "Blue copy".to_string(),
            "Blue".to_string(),
            "Red".to_string(),
            "Background".to_string(),
        ],
    )?;
    let copy = bounds(s, "Blue copy")?;
    s.check_eq("the copy moved 150 px left", copy, Some((350, 300, 100, 100)))?;
    let orig = bounds(s, "Blue")?;
    s.check_eq("the original stayed", orig, Some((500, 300, 100, 100)))?;
    check_px(s, "blue at the copy", 360, 310, BLUE)?;
    check_px(s, "blue at the original", 590, 390, BLUE)?;
    let active = active_name(s)?;
    s.check_eq(
        "the copy is the active layer",
        active,
        Some("Blue copy".to_string()),
    )?;
    let hist = s.history()?;
    s.note(&format!("History: {}", hist.join(" → ")))?;
    s.check_eq("one history step for the Alt-drag", hist.len(), steps + 1)?;
    s.describe("Undo once");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq(
        "one undo removes the copy",
        names,
        vec!["Blue".to_string(), "Red".to_string(), "Background".to_string()],
    )?;
    Ok(())
}

fn move_auto_select(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    s.describe("Pick the Move tool");
    s.click("Move")?;
    s.describe("Cmd-drag the blue square: Cmd picks the layer under the pointer");
    s.canvas_drag((550.0, 350.0), (550.0, 450.0), 12, "Cmd")?;
    s.wait_idle()?;
    let active = active_name(s)?;
    s.check_eq("Blue became the active layer", active, Some("Blue".to_string()))?;
    let blue = bounds(s, "Blue")?;
    s.check_eq("and it moved down 100 px", blue, Some((500, 400, 100, 100)))?;
    let red = bounds(s, "Red")?;
    s.check_eq("Red stayed put", red, Some((100, 100, 200, 100)))?;

    s.describe("Tick Auto-Select in the options bar");
    s.click("Auto-Select")?;
    s.describe("Drag the red block (no keys held)");
    s.canvas_drag((200.0, 150.0), (200.0, 250.0), 12, "")?;
    s.wait_idle()?;
    let active = active_name(s)?;
    s.check_eq("Red became the active layer", active, Some("Red".to_string()))?;
    let red = bounds(s, "Red")?;
    s.check_eq("and it moved down 100 px", red, Some((100, 200, 200, 100)))?;
    s.describe("A click on the white background picks the Background");
    s.canvas_click((700.0, 100.0), "")?;
    let active = active_name(s)?;
    s.check_eq("Background became active", active, Some("Background".to_string()))?;
    s.describe("Untick Auto-Select again");
    s.click("Auto-Select")?;
    Ok(())
}

fn move_smart_guides(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer and the Move tool");
    s.click("Layer Red")?;
    s.click("Move")?;
    // Red's left edge to x 498: two pixels short of Blue's left edge (500).
    s.describe("Drag the red block until its left edge is near the blue square's");
    s.canvas_drag((200.0, 150.0), (598.0, 150.0), 20, "")?;
    s.wait_idle()?;
    let red = bounds(s, "Red")?;
    s.check_eq(
        "its left edge snapped onto Blue's (x 500)",
        red,
        Some((500, 100, 200, 100)),
    )?;
    s.describe("Undo");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    // The harness holds keys for the whole drag; the press is on Red's
    // own pixels, so Cmd's auto-select keeps Red.
    s.describe("The same drag with Cmd held: no snapping");
    s.canvas_drag((200.0, 150.0), (598.0, 150.0), 20, "Cmd")?;
    s.wait_idle()?;
    let red = bounds(s, "Red")?;
    s.check_eq(
        "it lands exactly where it was dropped (x 498)",
        red,
        Some((498, 100, 200, 100)),
    )?;
    s.describe("Turn Smart Guides off in the View menu");
    s.menu("View > Show smart guides")?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    s.describe("Drag again with Smart Guides off");
    s.canvas_drag((200.0, 150.0), (598.0, 150.0), 20, "")?;
    s.wait_idle()?;
    let red = bounds(s, "Red")?;
    s.note(&format!(
        "With Smart Guides off (View ▸ Snap still on), Red is at {red:?}"
    ))?;
    s.describe("Turn Smart Guides back on");
    s.menu("View > Show smart guides")?;
    Ok(())
}

/// The live transform's state: (sx, sy, angle in degrees, shear in
/// degrees, dx, dy), when one is up.
/// (sx, sy, angle°, skew°, dx, dy) of a live transform.
type XformState = (f32, f32, f32, f32, f32, f32);

fn xform(s: &mut Session) -> UiResult<Option<XformState>> {
    s.app(|a| {
        a.xform.as_ref().map(|x| {
            (
                x.sx,
                x.sy,
                x.angle.to_degrees(),
                x.shear.atan().to_degrees(),
                x.dx,
                x.dy,
            )
        })
    })
}

fn approx(a: Option<(i32, i32, u32, u32)>, b: (i32, i32, u32, u32), tol: i32) -> bool {
    a.is_some_and(|a| {
        (a.0 - b.0).abs() <= tol
            && (a.1 - b.1).abs() <= tol
            && (a.2 as i32 - b.2 as i32).abs() <= tol
            && (a.3 as i32 - b.3 as i32).abs() <= tol
    })
}

fn check_bounds(
    s: &mut Session,
    what: &str,
    layer: &str,
    want: (i32, i32, u32, u32),
    tol: i32,
) -> UiResult<bool> {
    let got = bounds(s, layer)?;
    s.check(
        what,
        approx(got, want, tol),
        format!("{want:?} ± {tol}"),
        format!("{got:?}"),
    )
}

fn transform_scale(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    let steps = s.history()?.len();
    s.describe("Free Transform from the Edit menu");
    s.menu("Edit > Free transform")?;
    s.expect_node("Width")?;
    s.expect_text("Free Transform")?;
    // Photoshop: a corner drag scales from the opposite corner, keeping
    // the proportions; the box is (100, 100) to (300, 200).
    s.describe("Drag the bottom-right corner out to x 400");
    s.canvas_drag((300.0, 200.0), (400.0, 250.0), 14, "")?;
    let x = xform(s)?;
    s.check(
        "both axes scale by 1.5",
        x.is_some_and(|x| (x.0 - 1.5).abs() < 0.02 && (x.1 - 1.5).abs() < 0.02),
        "sx = sy = 1.5",
        format!("{x:?}"),
    )?;
    s.describe("Apply with Enter");
    s.key("Enter")?;
    s.wait_idle()?;
    // Resampling softens each edge by a pixel or two.
    check_bounds(
        s,
        "the top-left corner stayed, the block is 300 × 150",
        "Red",
        (100, 100, 300, 150),
        2,
    )?;
    check_px(s, "red just inside the old top-left corner", 102, 102, RED)?;
    check_px(s, "red near the new bottom-right corner", 397, 247, RED)?;
    let n = s.history()?.len();
    s.check_eq("one history step", n, steps + 1)?;

    s.describe("Free Transform again with Cmd+T");
    s.key("Cmd+T")?;
    s.describe("Shift-drag the bottom-right corner: free aspect");
    s.canvas_drag((400.0, 250.0), (500.0, 250.0), 14, "Shift")?;
    s.key("Enter")?;
    s.wait_idle()?;
    check_bounds(
        s,
        "only the width grew: 400 × 150",
        "Red",
        (100, 100, 400, 150),
        3,
    )?;
    check_px(s, "red near the new right edge", 496, 175, RED)?;
    check_px(s, "white below the unchanged bottom edge", 300, 254, WHITE)?;

    s.describe("Cmd+T, drag the right edge in, then Esc");
    s.key("Cmd+T")?;
    s.canvas_drag((500.0, 175.0), (300.0, 175.0), 14, "")?;
    s.key("Esc")?;
    s.wait_idle()?;
    check_bounds(s, "Esc left the block as it was", "Red", (100, 100, 400, 150), 3)?;
    check_px(s, "the right end is still red", 496, 175, RED)?;
    let x = xform(s)?;
    s.check_eq("and closed the transform", x.is_none(), true)?;
    let n = s.history()?.len();
    s.check_eq("no history step for the cancelled transform", n, steps + 2)?;

    s.describe("Undo twice");
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    check_bounds(s, "back to the original block", "Red", (100, 100, 200, 100), 0)?;
    Ok(())
}

/// Where a control of the options bar (the strip under the menus) is:
/// the Free Transform fields share their names with Properties' own.
fn bar_point(s: &Session, name: &str) -> UiResult<Pos2> {
    s.tree()
        .matches(name, None)
        .into_iter()
        .rfind(|n| (40.0..82.0).contains(&n.rect.center().y) && !matches!(n.role, Role::Label))
        .map(|n| n.rect.center())
        .ok_or_else(|| UiError(format!("no “{name}” in the options bar")))
}

/// Type into an options-bar field: click it, select all, type, Enter.
fn set_bar_field(s: &mut Session, name: &str, text: &str) -> UiResult {
    let p = bar_point(s, name)?;
    s.click_at(p, &format!("the options bar's {name} field"))?;
    s.key("Cmd+A")?;
    s.type_text(text)?;
    s.key("Enter")
}

fn transform_typed(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer and the Move tool");
    s.click("Layer Red")?;
    s.click("Move")?;
    s.describe("Click Free transform in the Move tool's options bar");
    s.click("Free transform")?;
    // Properties has Scale, Rotate and Apply of its own: they wait while
    // Free Transform is open, so the options bar's are the only live ones.
    let live: Vec<String> = ["Rotate", "Scale", "Apply"]
        .iter()
        .flat_map(|name| s.tree().matches(name, None))
        .filter(|n| n.rect.center().y > 82.0 && !n.disabled && !matches!(n.role, Role::Label))
        .map(|n| n.describe())
        .collect();
    s.check_eq(
        "Properties' transform controls are greyed out",
        live,
        Vec::<String>::new(),
    )?;
    s.describe("Type 50 % into W");
    set_bar_field(s, "Width", "50")?;
    let x = xform(s)?;
    s.check(
        "the width is at half",
        x.is_some_and(|x| (x.0 - 0.5).abs() < 1e-3 && (x.1 - 1.0).abs() < 1e-3),
        "sx 0.5, sy 1",
        format!("{x:?}"),
    )?;
    s.describe("Type 90° into Rotate");
    set_bar_field(s, "Rotate", "90")?;
    s.describe("Apply with the Apply button");
    let apply = bar_point(s, "Apply")?;
    s.click_at(apply, "Apply")?;
    s.wait_idle()?;
    // 100 × 100 after halving the width, turned about the centre (200, 150).
    check_bounds(
        s,
        "a 100 × 100 square about the old centre",
        "Red",
        (150, 100, 100, 100),
        1,
    )?;

    s.describe("Undo, then skew by 20°");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    s.key("Cmd+T")?;
    set_bar_field(s, "Skew", "20")?;
    s.key("Enter")?;
    s.wait_idle()?;
    // tan 20° × 100 px tall = 36 px of lean, split about the centre.
    check_bounds(
        s,
        "the skewed block is 36 px wider",
        "Red",
        (82, 100, 236, 100),
        2,
    )?;
    check_px(s, "the top-left corner leans left", 90, 105, RED)?;
    check_px(s, "the bottom-left corner leans right", 105, 195, WHITE)?;
    Ok(())
}

fn transform_rotate(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    s.key("Cmd+T")?;
    // Outside the box, to the right of its centre (200, 150): drag a
    // quarter turn clockwise about it.
    s.describe("Drag outside the box a quarter turn round its centre");
    s.canvas_stroke(&[(380.0, 150.0), (327.0, 277.0), (200.0, 330.0)], 8)?;
    let x = xform(s)?;
    s.check(
        "the box turned 90°",
        x.is_some_and(|x| (x.2 - 90.0).abs() < 1.0),
        "90°",
        format!("{x:?}"),
    )?;
    s.describe("Esc, then Cmd+T again");
    s.key("Esc")?;
    s.key("Cmd+T")?;
    s.describe("Shift-drag a little over 40°: Shift turns in 15° steps");
    s.canvas_drag((380.0, 150.0), (338.0, 266.0), 12, "Shift")?;
    let x = xform(s)?;
    s.check(
        "the angle snapped to 45°",
        x.is_some_and(|x| (x.2 - 45.0).abs() < 0.01),
        "45°",
        format!("{x:?}"),
    )?;
    s.key("Enter")?;
    s.wait_idle()?;
    // Turned 45° about (200, 150), the block reaches 70 px along each
    // axis from its centre: (290, 150) was red and is white now, (200,
    // 210) was white and is red now.
    check_px(s, "the block's centre is still red", 200, 150, RED)?;
    check_px(s, "its old right end is white now", 290, 150, WHITE)?;
    check_px(s, "a corner swung down below the old edge", 200, 210, RED)?;
    Ok(())
}

fn transform_perspective_warp(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    s.describe("Edit ▸ Perspective");
    s.menu("Edit > Perspective")?;
    s.describe("Drag the top-right corner up and out");
    s.canvas_drag((300.0, 100.0), (350.0, 60.0), 12, "")?;
    s.key("Enter")?;
    s.wait_idle()?;
    check_bounds(s, "the quad reaches (350, 60)", "Red", (100, 60, 250, 140), 2)?;
    check_px(s, "red where the corner was pulled", 310, 110, RED)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    s.describe("Edit ▸ Warp");
    s.menu("Edit > Warp")?;
    // The 4 × 4 mesh over (100, 100)–(300, 200): the second point of the
    // top row is at (166.7, 100).
    s.describe("Drag the top row's second mesh point up 40 px");
    s.canvas_drag((166.7, 100.0), (166.7, 60.0), 12, "")?;
    s.key("Enter")?;
    s.wait_idle()?;
    let b = bounds(s, "Red")?;
    s.check(
        "the top edge bulges up, the rest stays",
        b.is_some_and(|b| b.1 < 90 && b.1 > 55 && b.0 == 100 && b.1 + b.3 as i32 == 200),
        "top between 55 and 90, left 100, bottom 200",
        format!("{b:?}"),
    )?;
    check_px(s, "red above the old top edge", 166, 90, RED)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    check_bounds(s, "undo restores the block", "Red", (100, 100, 200, 100), 0)?;
    Ok(())
}

/// The crop frame: x, y, w, h and its turn in degrees.
type CropState = (i32, i32, u32, u32, f32);

fn crop_frame(s: &mut Session) -> UiResult<Option<CropState>> {
    s.app(|a| {
        a.crop.frame.map(|f| {
            let r = f.rect();
            (r.x, r.y, r.w, r.h, f.angle.to_degrees())
        })
    })
}

fn crop_ratio(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Crop tool with C");
    s.key("C")?;
    s.expect_node("Crop ratio")?;
    s.describe("Open the ratio menu");
    s.click("Crop ratio")?;
    s.describe("Pick 1:1");
    s.click("1:1")?;
    let f = crop_frame(s)?;
    s.check_eq(
        "the frame is the largest centred square",
        f,
        Some((100, 0, 600, 600, 0.0)),
    )?;
    s.describe("Drag the top-left corner in by 100 px");
    s.canvas_drag((100.0, 0.0), (200.0, 100.0), 14, "")?;
    let f = crop_frame(s)?;
    s.check_eq(
        "a 500 px square anchored bottom-right",
        f,
        Some((200, 100, 500, 500, 0.0)),
    )?;
    s.describe("Crop with Enter");
    s.key("Enter")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the canvas is 500 × 500", size, (500, 500))?;
    let blue = bounds(s, "Blue")?;
    s.check_eq("Blue moved with the crop", blue, Some((300, 200, 100, 100)))?;
    s.describe("Undo the crop");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the canvas is back to 800 × 600", size, (800, 600))?;
    let blue = bounds(s, "Blue")?;
    s.check_eq("and Blue is back", blue, Some((500, 300, 100, 100)))?;
    Ok(())
}

fn crop_straighten(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Crop tool from the toolbar");
    s.click("Crop")?;
    s.describe("Tick Delete cropped pixels");
    s.click("Delete cropped pixels")?;
    // Outside the frame's right edge, level with the centre (400, 300):
    // a drag downwards turns the frame clockwise.
    s.describe("Drag outside the frame to turn it about 10°");
    let (cx, cy) = (400.0f32, 300.0f32);
    let r = 460.0f32;
    let a = 10f32.to_radians();
    s.canvas_drag((cx + r, cy), (cx + r * a.cos(), cy + r * a.sin()), 12, "")?;
    let f = crop_frame(s)?;
    s.note(&format!("Frame after turning: {f:?}"))?;
    s.check(
        "the frame turned by 10° and shrank to stay inside the canvas",
        f.is_some_and(|f| (f.4.abs() - 10.0).abs() < 0.5 && f.2 < 800 && f.3 < 600),
        "±10°, smaller than 800 × 600",
        format!("{f:?}"),
    )?;
    let want = f.map(|f| (f.2, f.3));
    s.describe("Click Crop in the options bar");
    s.click("Crop to frame")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the canvas is the frame's size", Some(size), want)?;
    let corner = s.pixel(0, 0)?;
    s.check(
        "no transparent wedge in the corner",
        corner[3] == 255,
        "opaque",
        format!("{corner:?}"),
    )?;
    let red_tiles = s.doc(|d| {
        find_layer(d, "Red")
            .and_then(|l| l.pixels())
            .and_then(|p| p.content_bounds())
            .map(|b| (b.x, b.y, b.w, b.h))
    })?;
    s.note(&format!("Red after the crop: {red_tiles:?}"))?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("undo restores 800 × 600", size, (800, 600))?;
    Ok(())
}

fn perspective_crop(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Crop tool");
    s.click("Crop")?;
    // Narrow bars shorten the switch's label to "Persp.".
    s.describe("Switch the options bar to Perspective");
    let label = if s.has_node("Perspective") {
        "Perspective"
    } else {
        "Persp."
    };
    s.click(label)?;
    s.describe("Drag a frame over the left half");
    s.canvas_drag((50.0, 50.0), (400.0, 550.0), 14, "")?;
    s.describe("Pull the top-right corner in to make a keystone");
    s.canvas_drag((400.0, 50.0), (350.0, 80.0), 10, "")?;
    s.describe("Crop with Enter");
    s.key("Enter")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.note(&format!("Canvas after the perspective crop: {size:?}"))?;
    s.check(
        "the canvas is about the frame's size",
        size.0 > 280 && size.0 < 360 && size.1 > 460 && size.1 < 510,
        "about 300–350 × 470–500",
        format!("{size:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("undo restores 800 × 600", size, (800, 600))?;
    Ok(())
}

/// Where document point (x, y) shows inside a workspace's preview
/// (Liquify, Puppet Warp): the canvas fitted with a 24 pt margin.
fn workspace_point(s: &Session, preview: &str, doc: (f32, f32)) -> UiResult<impl Fn(f32, f32) -> Pos2> {
    let node = s.node(preview).ok_or_else(|| UiError(format!("no {preview}")))?;
    let rect = node.rect;
    let (cw, ch) = doc;
    let disp = ((rect.width() - 48.0) / cw).min((rect.height() - 48.0) / ch);
    let min = rect.center() - vec2(cw * disp / 2.0, ch * disp / 2.0);
    Ok(move |x: f32, y: f32| min + vec2(x * disp, y * disp))
}

fn liquify(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    let steps = s.history()?.len();
    s.describe("Filter ▸ Liquify...");
    s.menu("Filter > Liquify...")?;
    s.wait_idle()?;
    s.expect_node("Liquify preview")?;
    let at = workspace_point(s, "Liquify preview", (800.0, 600.0))?;
    s.describe("Push the red block's bottom edge down");
    s.drag(at(200.0, 170.0), at(200.0, 260.0), 20, "")?;
    s.describe("Click OK");
    s.click("OK")?;
    s.wait_idle()?;
    let px = s.pixel(200, 215)?;
    s.check(
        "red was pushed below the old edge",
        near(px, RED, 40),
        format!("{RED:?}"),
        format!("{px:?}"),
    )?;
    let n = s.history()?.len();
    s.check_eq("one history step", n, steps + 1)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    check_px(s, "undo restores white there", 200, 215, WHITE)?;
    Ok(())
}

fn puppet_warp(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    s.describe("Edit ▸ Puppet Warp");
    s.menu("Edit > Puppet Warp")?;
    s.wait_idle()?;
    let at = workspace_point(s, "Puppet Warp preview", (800.0, 600.0))?;
    s.describe("Pin the left end");
    s.click_at(at(115.0, 150.0), "the left end")?;
    s.describe("Pin the middle");
    s.click_at(at(200.0, 150.0), "the middle")?;
    s.describe("Pin the right end");
    s.click_at(at(285.0, 150.0), "the right end")?;
    s.describe("Drag the right pin down 80 px");
    s.drag(at(285.0, 150.0), at(285.0, 230.0), 16, "")?;
    s.wait_idle()?;
    s.describe("Click OK");
    s.click("OK")?;
    s.wait_idle()?;
    check_px(s, "red bent down at the right end", 285, 225, RED)?;
    check_px(s, "the left end stayed", 110, 150, RED)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    check_px(s, "undo restores white there", 285, 225, WHITE)?;
    Ok(())
}

fn content_aware_scale(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Red layer");
    s.click("Layer Red")?;
    s.describe("Edit ▸ Content-Aware Scale");
    s.menu("Edit > Content-Aware Scale")?;
    s.wait_idle()?;
    s.describe("Type 50 % into W");
    s.set_field("W percent", "50")?;
    s.wait_idle()?;
    s.describe("Click Apply");
    s.click("Apply")?;
    s.wait_idle()?;
    let b = bounds(s, "Red")?;
    s.check(
        "the block is 100 px wide, 100 tall",
        b.is_some_and(|b| b.2.abs_diff(100) <= 1 && b.3 == 100),
        "100 × 100",
        format!("{b:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    check_bounds(s, "undo restores the block", "Red", (100, 100, 200, 100), 0)?;
    Ok(())
}

fn guides(s: &mut Session) -> UiResult<Vec<(bool, f32)>> {
    s.doc(|d| d.guides.iter().map(|g| (g.is_vertical(), g.pos)).collect())
}

fn rulers_guides_grid(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("View ▸ Rulers");
    s.menu("View > Rulers")?;
    s.expect_node("Horizontal ruler: drag down to add a guide")?;
    let ruler = s
        .node("Horizontal ruler: drag down to add a guide")
        .ok_or("no ruler")?;
    let from = ruler.rect.center();
    let to = s.doc_to_screen(from.x, 250.0)?;
    let to = pos2(from.x, to.y);
    s.describe("Drag a guide down out of the top ruler to y 250");
    s.drag(from, to, 14, "")?;
    let g = guides(s)?;
    s.check_eq("a horizontal guide at 250", g, vec![(false, 250.0)])?;

    s.describe("With the Move tool, drag the guide to y 320");
    s.click("Move")?;
    let a = s.doc_to_screen(720.0, 250.0)?;
    let b = s.doc_to_screen(720.0, 320.0)?;
    s.drag(a, b, 12, "")?;
    let g = guides(s)?;
    s.check_eq("the guide moved to 320", g, vec![(false, 320.0)])?;

    s.describe("Drag the guide back onto the ruler to delete it");
    let a = s.doc_to_screen(720.0, 320.0)?;
    s.drag(a, pos2(a.x, from.y), 12, "")?;
    let g = guides(s)?;
    s.check_eq("the guide is gone", g, vec![])?;
    s.describe("Undo brings it back");
    s.key("Cmd+Z")?;
    let g = guides(s)?;
    s.check_eq("the guide is back at 320", g, vec![(false, 320.0)])?;

    s.describe("A vertical guide from the left ruler, near the red block's right edge");
    let left = s
        .node("Vertical ruler: drag right to add a guide")
        .ok_or("no left ruler")?;
    let lf = left.rect.center();
    let lt = s.doc_to_screen(302.0, 0.0)?;
    s.drag(lf, pos2(lt.x, lf.y), 14, "")?;
    let g = guides(s)?;
    s.check_eq(
        "it snapped onto the block's edge at 300",
        g,
        vec![(false, 320.0), (true, 300.0)],
    )?;

    s.describe("A marquee near the guides snaps to them");
    s.click("Rectangular Marquee")?;
    s.canvas_drag((303.0, 323.0), (450.0, 450.0), 12, "")?;
    let sel = s.selection_bounds()?;
    s.check_eq(
        "the marquee starts on the guides",
        sel,
        Some((300, 320, 150, 130)),
    )?;
    s.key("Cmd+D")?;

    s.describe("View ▸ Show grid");
    s.menu("View > Show grid")?;
    let on = s.app(|a| a.prefs.show_grid)?;
    s.check_eq("the grid is on", on, true)?;
    s.screenshot("grid");
    s.describe("View ▸ Clear guides");
    s.menu("View > Clear guides")?;
    let g = guides(s)?;
    s.check_eq("no guides left", g, vec![])?;
    s.describe("View ▸ Show grid again turns it off");
    s.menu("View > Show grid")?;
    s.describe("View ▸ Rulers off");
    s.menu("View > Rulers")?;
    Ok(())
}

/// A 4000 × 3000 photo-like document: a noisy gradient background and a
/// 1600 × 1200 layer of texture on top.
fn big_file(s: &Session) -> UiResult<std::path::PathBuf> {
    let path = s.files().join("big.ora");
    let (w, h) = (4000u32, 3000u32);
    let mut d = Document::new(w, h);
    let mut bg = Raster::new(w, h);
    let mut seed = 0x9e37_79b9u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed & 0xff) as f32 / 255.0
    };
    for y in 0..h {
        for x in 0..w {
            let n = rnd() * 0.08;
            bg.pixels[(y * w + x) as usize] = Rgba::new(
                x as f32 / w as f32 * 0.8 + n,
                y as f32 / h as f32 * 0.6 + n,
                0.3 + n,
                1.0,
            );
        }
    }
    let id = d.add_pixel_layer("Background");
    if let Some(l) = d.layer_mut(id) {
        l.content = lumenply_doc::LayerContent::Pixel(TileStore::from_raster(&bg, 0, 0));
    }
    let (lw, lh) = (1600u32, 1200u32);
    let mut top = Raster::new(lw, lh);
    for y in 0..lh {
        for x in 0..lw {
            let n = rnd() * 0.2;
            let v = (((x / 40) + (y / 40)) % 2) as f32 * 0.5 + n;
            top.pixels[(y * lw + x) as usize] = Rgba::new(v, 0.2 + n, 1.0 - v, 1.0);
        }
    }
    let id = d.add_pixel_layer("Photo");
    if let Some(l) = d.layer_mut(id) {
        l.content = lumenply_doc::LayerContent::Pixel(TileStore::from_raster(&top, 400, 400));
    }
    write_ora(&path, &d)?;
    Ok(path)
}

/// Wall time of a step, in milliseconds.
fn timed(s: &mut Session, f: impl FnOnce(&mut Session) -> UiResult) -> UiResult<f32> {
    let t = std::time::Instant::now();
    f(s)?;
    Ok(t.elapsed().as_secs_f32() * 1000.0)
}

fn big_document(s: &mut Session) -> UiResult {
    let path = big_file(s)?;
    s.describe("Open big.ora (4000 × 3000)");
    s.click("Open…")?;
    s.choose_file(&path)?;
    s.wait_idle()?;
    s.describe("Pick the Photo layer and the Move tool");
    s.click("Layer Photo")?;
    s.click("Move")?;
    // The same drag in 20 and in 60 frames: the difference over 40 is
    // what one frame of dragging costs (pointer glide and settling are
    // the same in both).
    s.describe("Drag the 1600 × 1200 layer 500 px right in 20 frames");
    let short = timed(s, |s| {
        s.canvas_drag((1200.0, 1000.0), (1700.0, 1000.0), 20, "Cmd")
    })?;
    s.wait_idle()?;
    s.describe("And 500 px further in 60 frames");
    let long = timed(s, |s| {
        s.canvas_drag((1700.0, 1000.0), (2200.0, 1000.0), 60, "Cmd")
    })?;
    s.wait_idle()?;
    s.note(&format!(
        "Move drag on 4000 × 3000: {:.1} ms a frame (20 frames {short:.0} ms, 60 frames {long:.0} ms)",
        (long - short) / 40.0
    ))?;
    let b = bounds(s, "Photo")?;
    s.check_eq("the layer moved 1000 px", b, Some((1400, 400, 1600, 1200)))?;

    s.describe("Cmd+T, drag the bottom-right corner in 20 frames, Esc");
    s.key("Cmd+T")?;
    let short = timed(s, |s| s.canvas_drag((3000.0, 1600.0), (3400.0, 1900.0), 20, ""))?;
    s.key("Esc")?;
    s.describe("Cmd+T, the same drag in 60 frames");
    s.key("Cmd+T")?;
    let long = timed(s, |s| s.canvas_drag((3000.0, 1600.0), (3400.0, 1900.0), 60, ""))?;
    s.note(&format!(
        "Transform drag on 4000 × 3000: {:.1} ms a frame (20 frames {short:.0} ms, 60 frames {long:.0} ms)",
        (long - short) / 40.0
    ))?;
    s.describe("Apply with Enter");
    let apply = timed(s, |s| {
        s.key("Enter")?;
        s.wait_idle()
    })?;
    s.note(&format!("Applying the transform: {apply:.0} ms"))?;
    let b = bounds(s, "Photo")?;
    s.check(
        "the layer grew from its top-left corner",
        b.is_some_and(|b| b.0.abs_diff(1400) <= 2 && b.1.abs_diff(400) <= 2 && b.2 > 1700),
        "top-left (1400, 400), wider than 1700",
        format!("{b:?}"),
    )?;
    s.describe("Undo");
    let undo = timed(s, |s| {
        s.key("Cmd+Z")?;
        s.wait_idle()
    })?;
    s.note(&format!("Undo: {undo:.0} ms"))?;
    Ok(())
}
