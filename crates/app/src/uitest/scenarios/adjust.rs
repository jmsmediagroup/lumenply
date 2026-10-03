//! Adjusting a photo: adjustment layers, the Properties panel, history.

use crate::uitest::prelude::*;

scenario_list! {
    "adjust-a-photo" => adjust_a_photo: "The demo photo through a Curves layer, visibility and history",
}

/// Where the scenario samples the photo (document pixels): sky,
/// mountain and lake, all mid-tones a Curves lift brightens.
const SAMPLES: [(u32, u32); 3] = [(900, 250), (700, 640), (1100, 900)];

fn brightness(px: &[[u8; 4]]) -> u32 {
    px.iter().map(|p| p[0] as u32 + p[1] as u32 + p[2] as u32).sum()
}

fn sample(s: &mut Session) -> UiResult<Vec<[u8; 4]>> {
    SAMPLES.iter().map(|&(x, y)| s.pixel(x, y)).collect()
}

fn adjust_a_photo(s: &mut Session) -> UiResult {
    s.describe("Open the demo photo from the welcome screen");
    s.click("Open the demo photo")?;
    s.wait_idle()?;
    let layers_before = s.layer_names()?.len();
    let before = sample(s)?;

    s.describe("Add Curves from the quick-add chips under Properties");
    s.click_role(Role::Button, "Curves")?;
    s.wait_idle()?;
    let layers = s.layer_names()?;
    s.check_eq("one more layer", layers.len(), layers_before + 1)?;
    s.expect_node("Layer Curves")?;
    let added = sample(s)?;
    s.check_eq(
        "a fresh Curves layer changes nothing yet",
        added.clone(),
        before.clone(),
    )?;

    // The curve starts as a diagonal: click its middle to add a point,
    // then drag that point up to lift the mid-tones.
    s.describe("Click the middle of the curve to add a point");
    s.click_in("Curve, *", 0.5, 0.5, "the middle of the curve")?;
    s.describe("Drag the new point up");
    s.drag_in("Curve, *", (0.5, 0.5), (0.5, 0.28), "the new point up")?;
    s.wait_idle()?;
    let lifted = sample(s)?;
    let points = s.doc(|d| match &find_layer(d, "Curves")?.content {
        lumenply_doc::LayerContent::Adjustment(lumenply_doc::Adjustment::Curves { points, .. }) => {
            Some(points.clone())
        }
        _ => None,
    })?;
    let mid = points.as_ref().and_then(|p| p.get(1).copied());
    s.check(
        "the curve has a raised middle point",
        points.as_ref().is_some_and(|p| p.len() == 3) && mid.is_some_and(|m| m[1] > m[0] + 0.1),
        "3 points, the middle one above the diagonal",
        format!("{points:?}"),
    )?;
    s.check(
        "the photo got brighter",
        brightness(&lifted) > brightness(&before) + 60,
        format!("brighter than {}", brightness(&before)),
        format!("{} ({lifted:?})", brightness(&lifted)),
    )?;

    // The eye at the left of the layer's row (it has no name of its own).
    s.describe("Hide the Curves layer with its eye in the Layers panel");
    s.click_offset("Layer Curves", 14.0, 19.0, "the eye")?;
    s.wait_idle()?;
    let hidden = sample(s)?;
    let visible = s.doc(|d| find_layer(d, "Curves").map(|l| l.visible))?;
    s.check_eq("the layer is hidden", visible, Some(false))?;
    s.check_eq("the photo is back to how it was", hidden, before.clone())?;
    s.describe("Show it again");
    s.click_offset("Layer Curves", 14.0, 19.0, "the eye")?;
    s.wait_idle()?;
    let shown = sample(s)?;
    s.check_eq("the lift is back", shown, lifted.clone())?;

    let history = s.history()?;
    s.note(&format!("History: {}", history.join(" → ")))?;
    s.describe("Click the first step in the History strip");
    s.click("Open")?;
    s.wait_idle()?;
    let back = sample(s)?;
    s.check_eq("the photo is as it was opened", back, before.clone())?;
    let layers = s.layer_names()?.len();
    s.check_eq("the Curves layer is gone at that step", layers, layers_before)?;
    let last = history.last().cloned().unwrap_or_default();
    s.describe("Click the last step to come back");
    s.click(&last)?;
    s.wait_idle()?;
    let again = sample(s)?;
    s.check_eq("the lift is back again", again, lifted.clone())?;

    // Properties has an Opacity slider, and so does the Brush's options bar.
    s.describe("Select the Curves layer");
    s.click("Layer Curves")?;
    s.describe("Halve the layer's strength with Opacity in Properties");
    s.within("Properties", |s| s.drag_slider("Opacity", 0.5))?;
    s.wait_idle()?;
    let opacity = s.doc(|d| find_layer(d, "Curves").map(|l| l.opacity))?;
    s.check(
        "the Curves layer is at half opacity",
        opacity.is_some_and(|o| (o - 0.5).abs() < 0.02),
        "0.5",
        format!("{opacity:?}"),
    )?;
    let half = sample(s)?;
    s.check(
        "the photo sits between the original and the full lift",
        brightness(&before) < brightness(&half) && brightness(&half) < brightness(&lifted),
        format!("between {} and {}", brightness(&before), brightness(&lifted)),
        format!("{}", brightness(&half)),
    )?;
    Ok(())
}
