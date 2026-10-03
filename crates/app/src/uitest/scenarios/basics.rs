//! First steps: a new document, painting, layers, saving and reopening.

use crate::uitest::prelude::*;

scenario_list! {
    "first-steps" => first_steps: "Welcome screen to a painted, saved and reopened document",
}

/// Within `tol` of `want` on every channel.
fn near(got: [u8; 4], want: [u8; 4], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

fn first_steps(s: &mut Session) -> UiResult {
    s.expect_text("No document open")?;
    // Nothing to save yet: the File menu says why Save is greyed out.
    s.describe("Open the File menu");
    s.click("File")?;
    s.expect_disabled("Save", "Open or create a document first")?;
    s.describe("Close the menu again");
    s.key("Esc")?;

    s.describe("Make a new 800 × 600 image from File ▸ New");
    s.menu("File > New...")?;
    s.set_field("Width", "800")?;
    s.set_field("Height", "600")?;
    s.click("Create")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the new document is 800 × 600", size, (800, 600))?;

    s.describe("Pick the Brush from the toolbar");
    s.click_role(Role::Button, "Brush")?;
    s.describe("Paint a stroke across the top");
    s.canvas_stroke(&[(120.0, 150.0), (300.0, 120.0), (520.0, 180.0)], 8)?;
    s.describe("Paint a second stroke lower down");
    s.canvas_drag((150.0, 420.0), (650.0, 440.0), 14, "")?;
    s.wait_idle()?;
    let brush = s.pixel(300, 120)?;
    let second = s.pixel(400, 430)?;
    let blue = [0x1a, 0x2e, 0x8c, 255];
    s.check(
        "the first stroke is in the brush colour",
        near(brush, blue, 2),
        format!("{blue:?}"),
        format!("{brush:?}"),
    )?;
    s.check(
        "the second stroke is there too",
        near(second, blue, 2),
        format!("{blue:?}"),
        format!("{second:?}"),
    )?;

    // The Edit menu names the step it undoes, as the History strip does.
    let last = s.history()?.last().cloned().unwrap_or_default();
    s.describe("Undo the second stroke from the Edit menu");
    s.menu(&format!("Edit > Undo {last}"))?;
    let undone = s.pixel(400, 430)?;
    s.check(
        "the second stroke is gone",
        near(undone, [255; 4], 0),
        "white",
        format!("{undone:?}"),
    )?;
    let kept = s.pixel(300, 120)?;
    s.check(
        "the first stroke stays",
        near(kept, blue, 2),
        format!("{blue:?}"),
        format!("{kept:?}"),
    )?;

    s.describe("Add a layer from the Layer menu");
    s.menu("Layer > New pixel layer")?;
    let names = s.layer_names()?;
    s.check_eq(
        "a second layer on top",
        names,
        vec!["Layer 2".to_string(), "Background".to_string()],
    )?;
    s.describe("Double-click the new layer's name to rename it");
    s.double_click("Layer Layer 2")?;
    s.set_field("Layer name", "Sky")?;
    let names = s.layer_names()?;
    s.check_eq(
        "the layer is renamed",
        names,
        vec!["Sky".to_string(), "Background".to_string()],
    )?;
    s.expect_node("Layer Sky")?;

    let file = s.files().join("first-steps.lumen");
    s.describe("Save it with File ▸ Save as...");
    s.menu("File > Save as...")?;
    s.save_as(&file)?;
    s.check(
        "the project file was written",
        file.exists(),
        "a file",
        if file.exists() { "a file" } else { "nothing" },
    )?;
    s.expect_text("first-steps.lumen")?;

    s.describe("Close the document");
    s.menu("File > Close document")?;
    s.expect_text("No document open")?;

    s.describe("Reopen it from File ▸ Open recent");
    s.menu("File > Open recent > first-steps.lumen")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq(
        "both layers came back",
        names,
        vec!["Sky".to_string(), "Background".to_string()],
    )?;
    let back = s.pixel(300, 120)?;
    s.check(
        "the stroke came back",
        near(back, blue, 2),
        format!("{blue:?}"),
        format!("{back:?}"),
    )?;
    let white = s.pixel(400, 430)?;
    s.check(
        "the undone stroke stayed undone",
        near(white, [255; 4], 0),
        "white",
        format!("{white:?}"),
    )?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("still 800 × 600", size, (800, 600))?;
    Ok(())
}
