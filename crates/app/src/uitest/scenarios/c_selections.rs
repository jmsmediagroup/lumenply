//! C. Selections: marquees and lassos with their modifier keys, the Magic
//! Wand and Quick Selection, the Select menu (All, Deselect, Reselect,
//! Invert, Colour Range, Modify, Grow, Similar, Feather), saving and
//! loading selections, Quick Mask, Select and Mask, loading a layer's
//! pixels and the floating selection action bar.
//!
//! Every scenario opens `shapes.png`, a 400 × 300 image with flat colour
//! areas at known places, so the selections have exact expected bounds.

use crate::uitest::prelude::*;

scenario_list! {
    "c-marquee-modifiers" => marquee_modifiers: "Rectangular Marquee: new, Shift adds, Alt subtracts, both intersect; Shift squares, Alt centres",
    "c-ellipse-feather" => ellipse_feather: "Elliptical Marquee: a circle, and a feather typed in the options bar softens new selections",
    "c-lasso" => lasso: "Lasso and Polygonal Lasso: freehand, Shift adds, clicked corners, Esc and Close",
    "c-magic-wand" => magic_wand: "Magic Wand: tolerance, contiguous, Shift adds, Alt subtracts, all layers",
    "c-quick-selection" => quick_selection: "Quick Selection: brushing over an area selects it, a second stroke adds",
    "c-select-menu-basics" => select_menu_basics: "Select ▸ All, Deselect, Reselect, Invert from the menu and the keyboard",
    "c-colour-range" => colour_range: "Select ▸ Colour Range from a sampled colour; Cancel restores the selection",
    "c-modify" => modify: "Select ▸ Modify: Expand, Contract, Border, Smooth, Feather; Esc cancels",
    "c-grow-similar" => grow_similar: "Select ▸ Grow and Similar widen a Magic Wand selection",
    "c-save-load-selection" => save_load_selection: "Save a selection as an alpha channel and load it back (menu, Channels panel, invert)",
    "c-quick-mask" => quick_mask: "Quick Mask: paint black to take a band out of the selection",
    "c-select-and-mask" => select_and_mask: "Select and Mask: change the view, output to a layer mask and to a new layer",
    "c-layer-pixels" => layer_pixels: "Load a layer's pixels as a selection from the Layer menu and by Cmd-clicking its thumbnail",
    "c-action-bar" => action_bar: "The selection action bar: Fill, New layer, Mask, Clear",
}

/// The tool's usage hint: in the options bar, or on a narrow window in
/// the tool name's tooltip.
fn hint(s: &mut Session, text: &str) -> UiResult {
    if s.text_shown(text) {
        s.expect_text(text)?;
    } else {
        s.note(&format!(
            "the bar is narrow: “{text}” is in the tool name's tooltip"
        ))?;
    }
    Ok(())
}

/// The fixture's colours (sRGB) and areas.
const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [220, 30, 30];
/// Next to the first red square; outside the wand's default tolerance.
const DARK_RED: [u8; 3] = [190, 45, 45];
const BLUE: [u8; 3] = [30, 60, 200];

/// 400 × 300: red square (40..120, 40..120), dark red beside it
/// (120..160, 40..120), a second red square (280..340, 40..100) and a
/// blue disk centred on (200, 210), radius 50, on white.
fn shapes_pixel(x: u32, y: u32) -> [u8; 3] {
    let (fx, fy) = (x as f32 + 0.5 - 200.0, y as f32 + 0.5 - 210.0);
    if (40..120).contains(&x) && (40..120).contains(&y) {
        RED
    } else if (120..160).contains(&x) && (40..120).contains(&y) {
        DARK_RED
    } else if (280..340).contains(&x) && (40..100).contains(&y) {
        RED
    } else if fx * fx + fy * fy <= 50.0 * 50.0 {
        BLUE
    } else {
        WHITE
    }
}

/// Write shapes.png into the session's files and open it from the welcome
/// screen's Open… with the system panel.
fn open_shapes(s: &mut Session) -> UiResult {
    let path = s.files().join("shapes.png");
    let img = image::RgbImage::from_fn(400, 300, |x, y| image::Rgb(shapes_pixel(x, y)));
    img.save(&path)
        .map_err(|e| UiError(format!("writing {}: {e}", path.display())))?;
    s.describe("Click Open… on the welcome screen");
    s.click("Open…")?;
    s.describe("Pick shapes.png in the system's open panel");
    s.choose_file(&path)?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("shapes.png opened at 400 × 300", size, (400, 300))?;
    Ok(())
}

type Bounds = Option<(i32, i32, u32, u32)>;

/// Bounds within `tol` pixels of `want` on every number.
fn near_bounds(got: Bounds, want: (i32, i32, u32, u32), tol: i32) -> bool {
    got.is_some_and(|g| {
        (g.0 - want.0).abs() <= tol
            && (g.1 - want.1).abs() <= tol
            && (g.2 as i32 - want.2 as i32).abs() <= tol
            && (g.3 as i32 - want.3 as i32).abs() <= tol
    })
}

fn check_bounds(s: &mut Session, what: &str, want: Bounds) -> UiResult<bool> {
    let b = s.selection_bounds()?;
    s.check_eq(what, b, want)
}

fn check_bounds_near(s: &mut Session, what: &str, want: (i32, i32, u32, u32), tol: i32) -> UiResult<bool> {
    let b = s.selection_bounds()?;
    s.check(
        what,
        near_bounds(b, want, tol),
        format!("{want:?} ± {tol}"),
        format!("{b:?}"),
    )
}

/// Coverage at each point, compared with the expected value within `tol`.
fn check_coverage(s: &mut Session, what: &str, points: &[((i32, i32), f32)], tol: f32) -> UiResult<bool> {
    let mut got = Vec::new();
    for &((x, y), _) in points {
        got.push(s.selection_at(x, y)?);
    }
    let ok = points
        .iter()
        .zip(&got)
        .all(|((_, want), g)| (g - want).abs() <= tol);
    let want: Vec<String> = points
        .iter()
        .map(|((x, y), v)| format!("({x},{y})={v:.2}"))
        .collect();
    let got: Vec<String> = points
        .iter()
        .zip(&got)
        .map(|(((x, y), _), v)| format!("({x},{y})={v:.2}"))
        .collect();
    s.check(what, ok, want.join(" "), got.join(" "))
}

fn history_len(s: &mut Session) -> UiResult<usize> {
    Ok(s.history()?.len())
}

// ---- marquees ---------------------------------------------------------------

fn marquee_modifiers(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Rectangular Marquee with its key, M");
    s.key("M")?;
    hint(s, "Shift adds, Alt subtracts")?;
    s.describe("Drag a rectangle");
    s.canvas_drag((20.0, 20.0), (120.0, 100.0), 12, "")?;
    check_bounds(
        s,
        "the selection is the dragged rectangle",
        Some((20, 20, 100, 80)),
    )?;
    s.expect_text("100 × 80")?;

    s.describe("Shift-drag a second rectangle: it is added");
    s.canvas_drag((100.0, 60.0), (200.0, 160.0), 12, "Shift")?;
    check_bounds(s, "the union of both rectangles", Some((20, 20, 180, 140)))?;
    check_coverage(
        s,
        "both rectangles are selected, the corner between them is not",
        &[((50, 50), 1.0), ((150, 150), 1.0), ((50, 150), 0.0)],
        0.0,
    )?;

    s.describe("Alt-drag inside: the rectangle is taken out");
    s.canvas_drag((40.0, 40.0), (80.0, 80.0), 10, "Alt")?;
    check_coverage(
        s,
        "a hole where Alt dragged",
        &[((60, 60), 0.0), ((30, 30), 1.0), ((150, 150), 1.0)],
        0.0,
    )?;

    s.describe("Shift+Alt-drag: only the overlap stays");
    s.canvas_drag((0.0, 0.0), (60.0, 60.0), 10, "Shift+Alt")?;
    check_bounds(s, "the intersection with (0,0)–(60,60)", Some((20, 20, 40, 40)))?;
    check_coverage(
        s,
        "the hole is still there",
        &[((50, 50), 0.0), ((30, 30), 1.0)],
        0.0,
    )?;

    s.describe("Undo the intersection");
    s.key("Cmd+Z")?;
    check_bounds(s, "back to the union with the hole", Some((20, 20, 180, 140)))?;
    s.describe("Undo the subtraction");
    s.key("Cmd+Z")?;
    check_coverage(s, "the hole is gone", &[((60, 60), 1.0)], 0.0)?;
    s.describe("Undo the addition");
    s.key("Cmd+Z")?;
    check_bounds(s, "the first rectangle alone", Some((20, 20, 100, 80)))?;

    s.describe("Deselect with Cmd+D");
    s.key("Cmd+D")?;
    check_bounds(s, "nothing selected", None)?;

    // Photoshop: Shift pressed after the drag starts constrains to a square.
    s.describe("Drag, then hold Shift on the way: a square");
    s.canvas_path(
        &[
            ((200.0, 150.0), ""),
            ((230.0, 165.0), "Shift"),
            ((300.0, 190.0), "Shift"),
        ],
        8,
    )?;
    check_bounds(
        s,
        "a 100 × 100 square from the start point",
        Some((200, 150, 100, 100)),
    )?;

    s.describe("Drag, then hold Alt on the way: drawn from the centre");
    s.canvas_path(
        &[
            ((100.0, 100.0), ""),
            ((120.0, 110.0), "Alt"),
            ((150.0, 140.0), "Alt"),
        ],
        8,
    )?;
    check_bounds(
        s,
        "a 100 × 80 rectangle centred on the start point",
        Some((50, 60, 100, 80)),
    )?;

    s.describe("Click the canvas without dragging: deselects");
    s.canvas_click((350.0, 250.0), "")?;
    check_bounds(s, "nothing selected", None)?;
    Ok(())
}

fn ellipse_feather(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Elliptical Marquee with Shift+M");
    s.key("Shift+M")?;
    s.expect_text("Feather")?;
    let feather = s.node("Feather").and_then(|n| n.numeric);
    s.check_eq("Feather starts at 0 px, as in Photoshop", feather, Some(0.0))?;

    s.describe("Shift-drag with nothing selected: a circle");
    s.canvas_drag((100.0, 50.0), (200.0, 120.0), 12, "Shift")?;
    check_bounds_near(
        s,
        "a 100 × 100 circle from the start point",
        (100, 50, 100, 100),
        1,
    )?;
    check_coverage(
        s,
        "inside the circle, and outside its corner",
        &[((150, 100), 1.0), ((103, 53), 0.0)],
        0.0,
    )?;

    s.describe("Type a feather of 10 px in the options bar");
    s.set_field("Feather", "10")?;
    s.describe("Drag a new ellipse");
    s.canvas_drag((200.0, 100.0), (360.0, 260.0), 12, "")?;
    // Photoshop: the options bar's feather applies to each new selection.
    check_coverage(
        s,
        "the new ellipse has a soft edge: half selected on the outline",
        &[((280, 180), 1.0), ((200, 180), 0.5), ((185, 180), 0.0)],
        0.2,
    )?;
    check_bounds_near(s, "feathering spreads it past the drag", (182, 82, 218, 218), 3)?;

    s.describe("Undo the feathered ellipse");
    s.key("Cmd+Z")?;
    check_bounds_near(s, "the circle is back", (100, 50, 100, 100), 1)?;

    s.describe("Set the feather back to 0 px");
    s.set_field("Feather", "0")?;
    s.canvas_drag((200.0, 100.0), (360.0, 260.0), 12, "")?;
    check_coverage(
        s,
        "a hard edge again",
        &[((198, 180), 0.0), ((203, 180), 1.0)],
        0.01,
    )?;
    Ok(())
}

// ---- lassos -------------------------------------------------------------------

fn lasso(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Lasso with L");
    s.key("L")?;
    s.describe("Draw a triangle freehand");
    s.canvas_stroke(&[(50.0, 50.0), (250.0, 50.0), (150.0, 250.0), (52.0, 52.0)], 10)?;
    check_coverage(
        s,
        "inside the triangle selected, beside it not",
        &[((150, 100), 1.0), ((60, 200), 0.0), ((240, 200), 0.0)],
        0.01,
    )?;
    check_bounds_near(s, "the triangle's bounds", (50, 50, 200, 200), 2)?;

    s.describe("Shift-draw a second triangle: it is added");
    s.canvas_path(
        &[
            ((300.0, 200.0), "Shift"),
            ((380.0, 200.0), "Shift"),
            ((340.0, 280.0), "Shift"),
            ((302.0, 202.0), "Shift"),
        ],
        8,
    )?;
    check_coverage(
        s,
        "both triangles selected",
        &[((150, 100), 1.0), ((340, 220), 1.0)],
        0.01,
    )?;
    s.describe("Undo the second triangle");
    s.key("Cmd+Z")?;
    check_coverage(
        s,
        "only the first one",
        &[((150, 100), 1.0), ((340, 220), 0.0)],
        0.01,
    )?;

    s.describe("Pick the Polygonal Lasso with Shift+L");
    s.key("Shift+L")?;
    hint(s, "Click to add points")?;
    s.describe("Click four corners");
    for p in [(260.0, 40.0), (380.0, 40.0), (380.0, 140.0), (260.0, 140.0)] {
        s.canvas_click(p, "")?;
    }
    s.describe("Click the first corner again to close the polygon");
    s.canvas_click((260.0, 40.0), "")?;
    check_bounds_near(
        s,
        "the clicked rectangle replaces the selection",
        (260, 40, 120, 100),
        1,
    )?;

    s.describe("Click two corners of another polygon, then press Esc");
    s.canvas_click((20.0, 200.0), "")?;
    s.canvas_click((120.0, 200.0), "")?;
    s.key("Esc")?;
    check_bounds_near(
        s,
        "Esc dropped the unfinished polygon, the selection stays",
        (260, 40, 120, 100),
        1,
    )?;

    s.describe("Click three corners and close with the options bar's Close");
    s.canvas_click((20.0, 200.0), "")?;
    s.canvas_click((120.0, 200.0), "")?;
    s.canvas_click((70.0, 280.0), "")?;
    s.click("Close")?;
    check_coverage(s, "the new triangle", &[((70, 230), 1.0), ((300, 90), 0.0)], 0.01)?;
    s.describe("Undo it");
    s.key("Cmd+Z")?;
    check_bounds_near(s, "the rectangle polygon is back", (260, 40, 120, 100), 1)?;
    Ok(())
}

// ---- wand and quick selection ---------------------------------------------------

fn magic_wand(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Magic Wand with W");
    s.key("W")?;
    s.expect_text("Tolerance")?;
    s.describe("Click the red square");
    s.canvas_click((80.0, 80.0), "")?;
    check_bounds(s, "exactly the red square", Some((40, 40, 80, 80)))?;

    s.describe("Raise the tolerance to 25");
    s.set_field("Tolerance", "25")?;
    s.canvas_click((80.0, 80.0), "")?;
    check_bounds(s, "the dark red beside it joins", Some((40, 40, 120, 80)))?;

    s.describe("Untick Contiguous");
    s.click("Contiguous")?;
    s.canvas_click((80.0, 80.0), "")?;
    check_bounds(s, "the other red square joins too", Some((40, 40, 300, 80)))?;

    s.describe("Shift-click the blue disk: added");
    s.canvas_click((200.0, 210.0), "Shift")?;
    check_bounds(s, "reds and the disk", Some((40, 40, 300, 220)))?;

    s.describe("Alt-click a red square: the reds are taken out");
    s.canvas_click((300.0, 60.0), "Alt")?;
    check_bounds(s, "only the disk", Some((150, 160, 100, 100)))?;
    s.describe("Undo the subtraction");
    s.key("Cmd+Z")?;
    check_bounds(s, "reds and disk again", Some((40, 40, 300, 220)))?;

    s.describe("Shift+Alt-click the disk: intersect keeps just it");
    s.canvas_click((200.0, 210.0), "Shift+Alt")?;
    check_bounds(s, "only the disk (intersection)", Some((150, 160, 100, 100)))?;

    s.describe("Tick Contiguous again");
    s.click("Contiguous")?;
    s.describe("Add an empty layer from the Layer menu");
    s.menu("Layer > New pixel layer")?;
    s.canvas_click((80.0, 80.0), "")?;
    check_bounds(
        s,
        "on the empty layer the wand selects its transparency: everything",
        Some((0, 0, 400, 300)),
    )?;
    s.describe("Tick All layers");
    s.click("All layers")?;
    s.canvas_click((80.0, 80.0), "")?;
    check_bounds(
        s,
        "sampling all layers finds the red again",
        Some((40, 40, 120, 80)),
    )?;
    Ok(())
}

fn quick_selection(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Magic Wand");
    s.click("Magic Wand")?;
    s.describe("Switch it to Quick selection in the options bar");
    // A narrow bar shortens the switch's labels.
    if s.has_node("Quick selection") {
        s.click("Quick selection")?;
    } else {
        s.click("Quick")?;
    }
    s.expect_text("Size")?;
    s.describe("Brush across the red square");
    s.canvas_stroke(&[(60.0, 60.0), (100.0, 100.0)], 10)?;
    s.wait_idle()?;
    check_coverage(
        s,
        "the red square is selected, white and blue are not",
        &[
            ((45, 45), 1.0),
            ((80, 80), 1.0),
            ((115, 115), 1.0),
            ((20, 20), 0.0),
            ((200, 210), 0.0),
        ],
        0.1,
    )?;
    // The dark red beside it is close in colour: it may join, nothing else.
    let b = s.selection_bounds()?;
    s.check(
        "the red square, perhaps with the dark red beside it",
        near_bounds(b, (40, 40, 80, 80), 4) || near_bounds(b, (40, 40, 120, 80), 4),
        "(40, 40, 80, 80) or (40, 40, 120, 80), ± 4",
        format!("{b:?}"),
    )?;

    s.describe("Brush across the blue disk: added");
    s.canvas_stroke(&[(180.0, 200.0), (220.0, 220.0)], 10)?;
    s.wait_idle()?;
    check_coverage(
        s,
        "the disk joins the red square",
        &[((200, 210), 1.0), ((80, 80), 1.0), ((20, 20), 0.0)],
        0.1,
    )?;
    s.describe("Undo the second stroke");
    s.key("Cmd+Z")?;
    check_coverage(
        s,
        "only the red square",
        &[((200, 210), 0.0), ((80, 80), 1.0)],
        0.1,
    )?;
    Ok(())
}

// ---- the Select menu --------------------------------------------------------------

fn select_menu_basics(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Open the Select menu: Deselect is greyed out with a reason");
    s.click("Select")?;
    s.expect_disabled("Deselect", "Make a selection first")?;
    s.key("Esc")?;

    s.describe("Select ▸ All");
    s.menu("Select > All")?;
    check_bounds(s, "the whole canvas", Some((0, 0, 400, 300)))?;
    s.describe("Select ▸ Deselect");
    s.menu("Select > Deselect")?;
    check_bounds(s, "nothing", None)?;
    s.describe("Select ▸ Reselect");
    s.menu("Select > Reselect")?;
    check_bounds(s, "everything again", Some((0, 0, 400, 300)))?;

    s.describe("Draw a rectangle with the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.canvas_drag((20.0, 20.0), (120.0, 100.0), 10, "")?;
    s.describe("Select ▸ Invert");
    s.menu("Select > Invert")?;
    check_coverage(s, "inverted", &[((50, 50), 0.0), ((300, 250), 1.0)], 0.0)?;
    s.describe("Undo the invert");
    s.key("Cmd+Z")?;
    check_coverage(s, "back", &[((50, 50), 1.0), ((300, 250), 0.0)], 0.0)?;

    s.describe("Shift+Cmd+I inverts from the keyboard");
    s.key("Cmd+Shift+I")?;
    check_coverage(s, "inverted", &[((50, 50), 0.0), ((300, 250), 1.0)], 0.0)?;
    s.key("Cmd+Shift+I")?;
    s.describe("Cmd+D deselects");
    s.key("Cmd+D")?;
    check_bounds(s, "nothing", None)?;
    s.describe("Shift+Cmd+D reselects");
    s.key("Cmd+Shift+D")?;
    check_bounds(s, "the rectangle", Some((20, 20, 100, 80)))?;
    s.describe("Cmd+A selects all");
    s.key("Cmd+A")?;
    check_bounds(s, "everything", Some((0, 0, 400, 300)))?;
    s.describe("Undo Select All");
    s.key("Cmd+Z")?;
    check_bounds(s, "the rectangle again", Some((20, 20, 100, 80)))?;
    Ok(())
}

fn colour_range(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.describe("Pick the Eyedropper and click the red square");
    s.click("Eyedropper")?;
    s.canvas_click((80.0, 80.0), "")?;
    s.describe("Select ▸ Colour range…");
    s.menu("Select > Colour range...")?;
    s.expect_text("Fuzziness")?;
    s.wait_idle()?;
    check_coverage(
        s,
        "the preview selects both red squares, not white or blue",
        &[
            ((80, 80), 1.0),
            ((300, 60), 1.0),
            ((20, 20), 0.0),
            ((200, 210), 0.0),
        ],
        0.05,
    )?;
    s.describe("Click Select");
    s.click("Select")?;
    check_coverage(
        s,
        "kept after Select",
        &[((80, 80), 1.0), ((300, 60), 1.0), ((20, 20), 0.0)],
        0.05,
    )?;
    let steps = history_len(s)?;

    s.describe("Open Colour range again and raise Fuzziness to 100%");
    s.menu("Select > Colour range...")?;
    s.set_field("Fuzziness", "100")?;
    s.wait_idle()?;
    let wide = s.selection_at(200, 210)?;
    s.check(
        "the preview widens to the blue disk",
        wide > 0.3,
        "> 0.3",
        format!("{wide:.2}"),
    )?;
    s.describe("Cancel");
    s.click("Cancel")?;
    check_coverage(
        s,
        "Cancel restores the reds-only selection",
        &[((80, 80), 1.0), ((200, 210), 0.0), ((20, 20), 0.0)],
        0.05,
    )?;
    let after = history_len(s)?;
    s.check_eq("Cancel leaves no history step", after, steps)?;
    Ok(())
}

fn modify(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Rectangular Marquee")?;
    s.describe("Select a 100 × 100 square");
    s.canvas_drag((100.0, 100.0), (200.0, 200.0), 10, "")?;
    check_bounds(s, "the square", Some((100, 100, 100, 100)))?;

    s.describe("Select ▸ Modify ▸ Expand… by 10 px");
    s.menu("Select > Modify > Expand...")?;
    s.set_field("Expand by", "10")?;
    s.click("OK")?;
    check_bounds(s, "10 px bigger on each side", Some((90, 90, 120, 120)))?;
    s.key("Cmd+Z")?;
    check_bounds(s, "undo: the square", Some((100, 100, 100, 100)))?;

    s.describe("Select ▸ Modify ▸ Contract… by 10 px");
    s.menu("Select > Modify > Contract...")?;
    s.set_field("Contract by", "10")?;
    s.click("OK")?;
    check_bounds(s, "10 px smaller on each side", Some((110, 110, 80, 80)))?;
    s.key("Cmd+Z")?;

    s.describe("Select ▸ Modify ▸ Border… 8 px wide");
    s.menu("Select > Modify > Border...")?;
    s.set_field("Width", "8")?;
    s.click("OK")?;
    check_coverage(
        s,
        "a band on the old edge, the middle empty",
        &[((100, 150), 1.0), ((150, 150), 0.0), ((90, 150), 0.0)],
        0.05,
    )?;
    s.key("Cmd+Z")?;

    s.describe("Select ▸ Modify ▸ Smooth… 10 px");
    s.menu("Select > Modify > Smooth...")?;
    s.set_field("Sample radius", "10")?;
    s.click("OK")?;
    check_coverage(
        s,
        "corners rounded, middle kept",
        &[((101, 101), 0.0), ((150, 150), 1.0), ((150, 101), 1.0)],
        0.05,
    )?;
    s.key("Cmd+Z")?;

    let steps = history_len(s)?;
    s.describe("Open Expand again: it remembers 10 px; change it, then press Esc");
    s.menu("Select > Modify > Expand...")?;
    let last = s.node("Expand by").and_then(|n| n.numeric);
    s.check_eq("Expand reopens with the last amount", last, Some(10.0))?;
    s.set_field("Expand by", "30")?;
    check_bounds(s, "the canvas previews it", Some((70, 70, 160, 160)))?;
    s.key("Esc")?;
    check_bounds(s, "Esc puts the square back", Some((100, 100, 100, 100)))?;
    let after = history_len(s)?;
    s.check_eq("and leaves no history step", after, steps)?;

    // Photoshop: Select ▸ Modify ▸ Feather… (Shift+F6) asks for a radius.
    s.describe("Select ▸ Modify ▸ Feather… 10 px");
    s.menu("Select > Modify > Feather...")?;
    s.set_field("Feather radius", "10")?;
    s.click("OK")?;
    check_coverage(
        s,
        "a soft edge: half on the old outline",
        &[((150, 150), 1.0), ((100, 150), 0.5), ((80, 150), 0.0)],
        0.15,
    )?;
    s.key("Cmd+Z")?;
    check_coverage(s, "undo: hard again", &[((100, 150), 1.0), ((99, 150), 0.0)], 0.0)?;
    s.describe("Shift+F6 opens Feather too");
    s.key("Shift+F6")?;
    s.expect_text("Feather selection")?;
    let again = s.node("Feather radius").and_then(|n| n.numeric);
    s.check_eq("it reopens with the last radius", again, Some(10.0))?;
    s.key("Esc")?;
    check_coverage(s, "Esc: still hard", &[((100, 150), 1.0), ((99, 150), 0.0)], 0.0)?;
    s.describe("Find it in the command palette by typing “feather”");
    s.key("Cmd+K")?;
    s.type_text("feather")?;
    s.expect_text("Feather selection")?;
    s.key("Enter")?;
    s.expect_text("Feather selection")?;
    s.key("Esc")?;
    check_coverage(s, "Esc: still hard", &[((100, 150), 1.0), ((99, 150), 0.0)], 0.0)?;
    Ok(())
}

fn grow_similar(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Magic Wand")?;
    s.describe("Click the red square with the Magic Wand");
    s.canvas_click((80.0, 80.0), "")?;
    check_bounds(s, "the red square", Some((40, 40, 80, 80)))?;
    s.describe("Raise the tolerance to 25");
    s.set_field("Tolerance", "25")?;
    s.describe("Select ▸ Grow");
    s.menu("Select > Grow")?;
    check_bounds(s, "grows into the touching dark red", Some((40, 40, 120, 80)))?;
    s.describe("Select ▸ Similar");
    s.menu("Select > Similar")?;
    check_bounds(s, "adds the far red square", Some((40, 40, 300, 80)))?;
    check_coverage(
        s,
        "white and blue stay out",
        &[((20, 20), 0.0), ((200, 210), 0.0)],
        0.0,
    )?;
    s.key("Cmd+Z")?;
    check_bounds(s, "undo Similar", Some((40, 40, 120, 80)))?;
    s.key("Cmd+Z")?;
    check_bounds(s, "undo Grow", Some((40, 40, 80, 80)))?;
    Ok(())
}

// ---- saved selections, quick mask, select and mask ---------------------------------

fn save_load_selection(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Rectangular Marquee")?;
    s.canvas_drag((20.0, 20.0), (120.0, 100.0), 10, "")?;
    s.describe("Select ▸ Save selection…");
    s.menu("Select > Save selection...")?;
    s.set_field("Selection name", "Box")?;
    s.click("Save")?;
    s.describe("Deselect");
    s.key("Cmd+D")?;
    check_bounds(s, "nothing selected", None)?;

    s.describe("Select ▸ Load selection…");
    s.menu("Select > Load selection...")?;
    s.expect_text("Box")?;
    s.click("Load")?;
    check_bounds(s, "the saved rectangle is back", Some((20, 20, 100, 80)))?;

    s.describe("Open the Channels panel: Box is an alpha channel");
    s.click("Channels panel")?;
    s.expect_node("Alpha channel Box")?;
    s.key("Cmd+D")?;
    s.describe("Cmd-click the Box channel to load it");
    s.click_with("Alpha channel Box", "Cmd")?;
    check_bounds(s, "loaded from the Channels panel", Some((20, 20, 100, 80)))?;

    s.describe("Load it inverted from Select ▸ Load selection…");
    s.menu("Select > Load selection...")?;
    s.click("Invert")?;
    s.click("Load")?;
    check_coverage(
        s,
        "the inverse of Box",
        &[((50, 50), 0.0), ((300, 250), 1.0)],
        0.0,
    )?;
    s.key("Cmd+Z")?;
    check_coverage(s, "undo: Box again", &[((50, 50), 1.0), ((300, 250), 0.0)], 0.0)?;

    s.describe("Save a second selection, the blue disk, as Disk");
    s.click("Magic Wand")?;
    s.canvas_click((200.0, 210.0), "")?;
    s.menu("Select > Save selection...")?;
    s.set_field("Selection name", "Disk")?;
    s.click("Save")?;
    s.expect_node("Alpha channel Disk")?;
    s.describe("Load selection…: pick Box in the list");
    s.menu("Select > Load selection...")?;
    s.click("Saved selection")?;
    s.click("Box")?;
    s.click("Load")?;
    check_bounds(s, "Box, chosen from the list", Some((20, 20, 100, 80)))?;
    s.click("Layers panel")?;
    Ok(())
}

fn quick_mask(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Rectangular Marquee")?;
    s.canvas_drag((100.0, 100.0), (300.0, 200.0), 10, "")?;
    s.describe("Press Q for Quick Mask");
    s.key("Q")?;
    s.expect_text("Quick mask")?;
    s.describe("Press D for black and white colours, B for the Brush");
    s.key("D")?;
    s.key("B")?;
    s.describe("Paint black across the middle");
    s.canvas_drag((110.0, 150.0), (290.0, 150.0), 16, "")?;
    s.describe("Press Q to leave Quick Mask");
    s.key("Q")?;
    check_coverage(
        s,
        "the painted band left the selection",
        &[
            ((200, 150), 0.0),
            ((200, 110), 1.0),
            ((200, 190), 1.0),
            ((50, 50), 0.0),
        ],
        0.05,
    )?;
    s.describe("Undo the paint");
    s.key("Cmd+Z")?;
    check_coverage(s, "the band is selected again", &[((200, 150), 1.0)], 0.05)?;
    Ok(())
}

fn select_and_mask(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Magic Wand")?;
    s.canvas_click((80.0, 80.0), "")?;
    s.describe("Select ▸ Select and Mask…");
    s.menu("Select > Select and Mask...")?;
    s.expect_node("Select and Mask preview")?;
    s.describe("View it on black");
    s.click("View")?;
    s.click("On black")?;
    s.describe("Paint along the square's top edge with the Refine Edge brush");
    s.drag_in(
        "Select and Mask preview",
        (0.12, 0.13),
        (0.30, 0.13),
        "the Refine Edge brush",
    )?;
    s.wait_idle()?;
    s.describe("Output to a layer mask (scrolling the settings down to it)");
    scroll_settings(s)?;
    s.click("Output to")?;
    s.click("Layer mask")?;
    s.click("OK")?;
    s.wait_idle()?;
    let mask = (s.mask_at("Background", 80, 80)?, s.mask_at("Background", 20, 20)?);
    s.check(
        "the Background has the red square as its mask",
        mask.0.is_some_and(|v| v > 0.95) && mask.1.is_some_and(|v| v < 0.05),
        "1 inside, 0 outside",
        format!("{mask:?}"),
    )?;
    s.describe("Undo the mask");
    s.key("Cmd+Z")?;
    let gone = s.mask_at("Background", 80, 80)?;
    s.check_eq("no mask after undo", gone, None)?;
    check_bounds(s, "and the selection is back", Some((40, 40, 80, 80)))?;

    s.describe("Select and Mask again, out to a new layer with mask");
    s.key("Cmd+Alt+R")?;
    s.expect_node("Select and Mask preview")?;
    scroll_settings(s)?;
    s.describe("It remembers the last output: Layer mask");
    s.expect_text("Layer mask")?;
    s.click("Output to")?;
    s.click("New layer with mask")?;
    s.click("OK")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq("a new layer on top", names.len(), 2)?;
    Ok(())
}

/// On a short window the Select and Mask settings scroll: bring Output to
/// into view, as a user would with the wheel.
fn scroll_settings(s: &mut Session) -> UiResult {
    if s.has_node("Select and Mask settings") {
        s.scroll("Select and Mask settings", 600.0)?;
    }
    Ok(())
}

fn layer_pixels(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Magic Wand")?;
    s.canvas_click((80.0, 80.0), "")?;
    s.describe("Layer ▸ Layer via copy puts the red square on its own layer");
    s.menu("Layer > Layer via copy")?;
    let names = s.layer_names()?;
    s.check_eq(
        "a copy layer on top",
        names.clone(),
        vec!["Background copy".to_string(), "Background".to_string()],
    )?;
    s.key("Cmd+D")?;
    s.describe("Layer ▸ Load layer pixels as selection");
    s.menu("Layer > Load layer pixels as selection")?;
    check_bounds(s, "the copied square's pixels", Some((40, 40, 80, 80)))?;
    s.key("Cmd+D")?;
    s.describe("Cmd-click the layer's thumbnail");
    s.click_offset_with("Layer Background copy", 50.0, 19.0, "Cmd", "the thumbnail")?;
    check_bounds(s, "the same selection", Some((40, 40, 80, 80)))?;
    s.key("Cmd+Z")?;
    check_bounds(s, "undo: nothing selected", None)?;
    Ok(())
}

fn action_bar(s: &mut Session) -> UiResult {
    open_shapes(s)?;
    s.click("Rectangular Marquee")?;
    s.canvas_drag((180.0, 40.0), (260.0, 120.0), 10, "")?;
    s.expect_node("New layer")?;
    s.describe("Fill from the action bar");
    s.click_role(Role::Button, "Fill")?;
    let filled = s.pixel(220, 80)?;
    s.check_eq("filled with the brush colour", filled, [0x1a, 0x2e, 0x8c, 255])?;
    s.key("Cmd+Z")?;
    let back = s.pixel(220, 80)?;
    s.check_eq("undo: white again", back, [255, 255, 255, 255])?;

    s.describe("New layer from the action bar copies the selection");
    s.click_role(Role::Button, "New layer")?;
    let names = s.layer_names()?;
    s.check_eq("a copy on top", names.len(), 2)?;
    s.key("Cmd+Z")?;

    s.describe("Mask from the action bar");
    s.click_role(Role::Button, "Mask")?;
    let mask = (
        s.mask_at("Background", 220, 80)?,
        s.mask_at("Background", 20, 20)?,
    );
    s.check_eq(
        "the Background is masked to the selection",
        mask,
        (Some(1.0), Some(0.0)),
    )?;
    s.key("Cmd+Z")?;

    s.describe("Clear from the action bar");
    s.click_role(Role::Button, "Clear")?;
    let cleared = s.layer_pixel("Background", 220, 80)?.map(|p| p[3]);
    s.check_eq("the selected pixels are transparent", cleared, Some(0))?;
    s.key("Cmd+Z")?;
    let back = s.layer_pixel("Background", 220, 80)?.map(|p| p[3]);
    s.check_eq("undo: opaque again", back, Some(255))?;
    Ok(())
}
