//! Selections: marquee, feathering, masks.

use crate::uitest::prelude::*;

scenario_list! {
    "select-and-mask" => select_and_mask: "A feathered marquee selection becomes a layer mask",
    "copy-and-paste" => copy_and_paste: "Open a photo from disk, copy a selection, paste it as a layer",
}

fn select_and_mask(s: &mut Session) -> UiResult {
    s.describe("Open the demo photo from the welcome screen");
    s.click("Open the demo photo")?;
    s.wait_idle()?;
    // The photo's own layer sits at the bottom of the list, below the
    // fold of the Layers panel: the harness scrolls to it as a user would.
    s.describe("Pick the photo's Background layer in the Layers panel");
    s.click("Layer Background")?;
    s.describe("Pick the Rectangular Marquee from the toolbar");
    s.click("Rectangular Marquee")?;

    // Feather lives in the options bar; Select ▸ Feather applies it.
    s.describe("Type a feather radius of 24 px in the options bar");
    s.set_field("Feather", "24")?;
    s.describe("Drag a rectangle over the mountain");
    s.canvas_drag((400.0, 300.0), (1400.0, 900.0), 16, "")?;
    s.wait_idle()?;
    let b = s.selection_bounds()?;
    s.check_eq(
        "the selection is the dragged rectangle",
        b,
        Some((400, 300, 1000, 600)),
    )?;
    let edge = (s.selection_at(398, 600)?, s.selection_at(402, 600)?);
    s.check_eq("its edge is hard until feathered", edge, (0.0, 1.0))?;
    s.expect_text("Selection")?;

    s.describe("Feather it with Select ▸ Feather 24 px");
    s.menu("Select > Feather 24 px")?;
    s.wait_idle()?;
    let soft = (
        s.selection_at(380, 600)?,
        s.selection_at(400, 600)?,
        s.selection_at(420, 600)?,
        s.selection_at(900, 600)?,
    );
    s.check(
        "the edge is soft now, the middle still selected",
        soft.0 > 0.02 && soft.0 < soft.1 && soft.1 < soft.2 && soft.2 < 0.98 && soft.3 > 0.99,
        "rising coverage across the edge, 1 inside",
        format!("{soft:?}"),
    )?;

    s.describe("Turn the selection into a mask with Select ▸ Layer mask from selection");
    s.menu("Select > Layer mask from selection")?;
    s.wait_idle()?;
    let mask = (
        s.mask_at("Background", 100, 100)?,
        s.mask_at("Background", 400, 600)?,
        s.mask_at("Background", 900, 600)?,
    );
    s.check(
        "the Background layer has the feathered mask",
        mask.0 == Some(0.0)
            && mask.1.is_some_and(|v| (v - soft.1).abs() < 0.01)
            && mask.2.is_some_and(|v| v > 0.99),
        format!("hidden outside, {:.2} on the edge, shown inside", soft.1),
        format!("{mask:?}"),
    )?;
    let last = s.history()?.last().cloned().unwrap_or_default();
    s.note(&format!("History's last step: {last}"))?;
    s.expect_node("Layer Background")?;
    Ok(())
}

/// A small photo-like image the "user" has on disk: a colour gradient.
fn write_photo(path: &std::path::Path) -> UiResult {
    let img = image::RgbaImage::from_fn(240, 160, |x, y| {
        image::Rgba([(x * 255 / 239) as u8, (y * 255 / 159) as u8, 128, 255])
    });
    img.save(path)
        .map_err(|e| UiError(format!("writing {}: {e}", path.display())))
}

fn copy_and_paste(s: &mut Session) -> UiResult {
    let photo = s.files().join("gradient.png");
    write_photo(&photo)?;
    s.describe("Click Open... on the welcome screen");
    s.click("Open…")?;
    s.describe("Pick gradient.png in the system's open panel");
    s.choose_file(&photo)?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the photo opened at its size", size, (240, 160))?;
    s.expect_text("gradient.png")?;

    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.describe("Select a patch of the photo");
    s.canvas_drag((40.0, 30.0), (140.0, 110.0), 12, "")?;
    s.describe("Copy it");
    s.key("Cmd+C")?;
    s.check_eq(
        "the clipboard holds the 100 × 80 patch",
        s.clipboard_image_size(),
        Some((100, 80)),
    )?;
    s.describe("Paste it as a new layer");
    s.key("Cmd+V")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq(
        "a Pasted layer on top",
        names.first().cloned(),
        Some("Pasted".to_string()),
    )?;
    let pasted = s.layer_pixel("Pasted", 60, 50)?;
    let source = s.layer_pixel("Background", 60, 50)?;
    s.check_eq("the pasted pixels match the source, in place", pasted, source)?;
    let outside = s.layer_pixel("Pasted", 10, 10)?.map(|p| p[3]);
    s.check_eq("only the selection was pasted", outside, Some(0))?;
    s.expect_text("Pasted 100 × 80 px")?;
    Ok(())
}
