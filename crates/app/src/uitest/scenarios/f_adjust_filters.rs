//! F. Adjustments, filters and effects: adjustment layers from the
//! quick-add chips and the Layer menu, edited in Properties; Image ▸
//! Adjustments on pixels; filters destructive, live and smart; Camera Raw
//! Filter; Color Lookup; layer styles; the histogram.
//!
//! Most journeys open `patches.png`, a 600 × 400 test card of flat
//! patches (see [`write_patches`]), so every check is a known input
//! through a known adjustment with an explicit expected value.

use crate::uitest::prelude::*;

scenario_list! {
    "fx-quick-add-chips" => quick_add_chips: "The Properties quick-add chips: each adjustment edited, checked, deleted",
    "fx-layer-menu-adjustments" => layer_menu_adjustments: "Every other adjustment layer from Layer ▸ New adjustment layer, edited in Properties",
    "fx-curves" => curves: "Curves in Properties: all of it in view, press-and-drag on the line, remove, channels",
    "fx-adjustment-mask" => adjustment_mask: "An adjustment layer made with a selection is masked by it; hide, mask off, delete, undo",
    "fx-image-adjustments" => image_adjustments: "Image ▸ Adjustments on pixels: live preview, Cancel, OK, undo, every dialog and the auto commands",
    "fx-filters" => filters: "Every Filter-menu group on pixels: dialog, live preview, Cancel, Apply, undo",
    "fx-live-filters" => live_filters: "Live filter layers from the Filter and Layer menus, edited in Properties",
    "fx-smart-filters" => smart_filters: "Smart filters: convert, stack, reorder, hide, mask, edit, delete",
    "fx-camera-raw" => camera_raw: "Camera Raw Filter: exposure on pixels, Cancel, and as a live layer",
    "fx-color-lookup" => color_lookup: "Color Lookup: a built-in look, a loaded .cube, a broken file",
    "fx-layer-styles" => layer_styles: "Layer styles in Properties on two layers: each effect on, edited, undone",
    "fx-big-document" => big_document: "Speed on a 4000 × 3000 photo: adjustment drags, dialog previews, filters",
}

/// Grey patches along the top row (y 0..200), 100 px wide each.
const GREYS: [u8; 6] = [0, 64, 128, 192, 255, 96];
/// Colour patches along the bottom row (y 200..400).
const COLOURS: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [200, 40, 40],
    [230, 140, 30],
    [40, 160, 200],
];

/// Write the test card into the session's files and return its path.
fn write_patches(s: &Session) -> UiResult<std::path::PathBuf> {
    let mut img = image::RgbImage::new(600, 400);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let i = (x / 100) as usize;
        *p = if y < 200 {
            image::Rgb([GREYS[i]; 3])
        } else {
            image::Rgb(COLOURS[i])
        };
    }
    let path = s.files().join("patches.png");
    img.save(&path)
        .map_err(|e| UiError(format!("writing the test card: {e}")))?;
    Ok(path)
}

/// Open the test card with File ▸ Open, as a user would.
fn open_patches(s: &mut Session) -> UiResult {
    let path = write_patches(s)?;
    s.describe("Open the test card with File ▸ Open");
    s.menu("File > Open...")?;
    s.choose_file(&path)?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the 600 × 400 test card is open", size, (600, 400))?;
    Ok(())
}

/// The centre of grey patch `i` (top row).
fn grey(i: usize) -> (u32, u32) {
    (50 + 100 * i as u32, 100)
}

/// The centre of colour patch `i` (bottom row).
fn colour(i: usize) -> (u32, u32) {
    (50 + 100 * i as u32, 300)
}

fn rgb(s: &mut Session, at: (u32, u32)) -> UiResult<[u8; 3]> {
    let p = s.pixel(at.0, at.1)?;
    Ok([p[0], p[1], p[2]])
}

fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

/// Check the composite at `at` is `want` (within `tol` per channel).
fn expect_rgb(s: &mut Session, what: &str, at: (u32, u32), want: [u8; 3], tol: u8) -> UiResult<bool> {
    let got = rgb(s, at)?;
    s.check(
        what,
        near(got, want, tol),
        format!("{want:?} at {at:?}"),
        format!("{got:?}"),
    )
}

/// Check every patch is back to the test card's own colours.
fn expect_original(s: &mut Session, what: &str) -> UiResult<bool> {
    let mut got = Vec::new();
    for i in 0..6 {
        got.push(rgb(s, grey(i))?);
        got.push(rgb(s, colour(i))?);
    }
    let want: Vec<[u8; 3]> = (0..6).flat_map(|i| [[GREYS[i]; 3], COLOURS[i]]).collect();
    let ok = got.iter().zip(&want).all(|(g, w)| near(*g, *w, 1));
    s.check(what, ok, format!("{want:?}"), format!("{got:?}"))
}

/// Delete the active layer with the Layers panel's bin and check the
/// test card is untouched again.
fn delete_active_layer(s: &mut Session, name: &str) -> UiResult {
    s.describe(&format!("Delete the {name} layer with the bin under Layers"));
    s.click("Delete layer")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq("only the photo is left", names, vec!["Background".to_string()])?;
    expect_original(s, "the test card is as it was")?;
    Ok(())
}

/// Run `f` inside the Properties panel. The panel is a named region only
/// while its contents scroll; otherwise the whole window is searched
/// (the names used here are unique to Properties then).
fn props<T>(s: &mut Session, f: impl FnOnce(&mut Session) -> UiResult<T>) -> UiResult<T> {
    let named = !s.tree().matches("Properties", Some(Role::ScrollView)).is_empty();
    if named {
        s.within("Properties", f)
    } else {
        f(s)
    }
}

// ---- the quick-add chips ------------------------------------------------------

fn quick_add_chips(s: &mut Session) -> UiResult {
    open_patches(s)?;

    // Levels: input black 64 and white 192 stretch the middle of the
    // range: 64 → 0, 128 → 128, 192 → 255.
    s.describe("Add Levels from the quick-add chips under Properties");
    s.click_role(Role::Button, "Levels")?;
    s.wait_idle()?;
    s.expect_node("Layer Levels")?;
    expect_original(s, "a fresh Levels layer changes nothing")?;
    s.describe("Type 64 for the input black point");
    props(s, |s| s.set_field("Input black", "64"))?;
    s.describe("Type 192 for the input white point");
    props(s, |s| s.set_field("Input white", "192"))?;
    s.wait_idle()?;
    expect_rgb(s, "grey 64 becomes black", grey(1), [0; 3], 1)?;
    expect_rgb(s, "grey 128 stays mid grey", grey(2), [128; 3], 1)?;
    expect_rgb(s, "grey 192 becomes white", grey(3), [255; 3], 1)?;
    s.describe("Undo the white point with Cmd+Z");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    // Black 64, white 255: 192 → (192 − 64) / 191 = 0.670 → 171.
    expect_rgb(s, "only the black point is left: 192 → 171", grey(3), [171; 3], 1)?;
    s.describe("Redo it with Shift+Cmd+Z");
    s.key("Cmd+Shift+Z")?;
    s.wait_idle()?;
    expect_rgb(s, "the white point is back", grey(3), [255; 3], 1)?;
    delete_active_layer(s, "Levels")?;

    // Exposure works in linear light: +1 EV doubles it, 128 → 176.
    s.describe("Pick the photo, then add Exposure from the chips");
    s.click("Layer Background")?;
    s.click_role(Role::Button, "Exposure")?;
    s.wait_idle()?;
    s.expect_node("Layer Exposure")?;
    s.describe("Type +1 EV of exposure");
    props(s, |s| s.set_field("Exposure", "1"))?;
    s.wait_idle()?;
    expect_rgb(s, "+1 EV: grey 128 → 176", grey(2), [176; 3], 1)?;
    expect_rgb(s, "+1 EV: grey 64 → 90", grey(1), [90; 3], 1)?;
    delete_active_layer(s, "Exposure")?;

    // Hue/Saturation: +120° turns red into green and green into blue.
    s.click("Layer Background")?;
    s.describe("Add Hue/Saturation from the chips");
    s.click_role(Role::Button, "Hue/Sat")?;
    s.wait_idle()?;
    s.expect_node("Layer Hue/Saturation")?;
    s.describe("Type a hue shift of 120°");
    props(s, |s| s.set_field("Hue", "120"))?;
    s.wait_idle()?;
    expect_rgb(s, "red turns green", colour(0), [0, 255, 0], 1)?;
    expect_rgb(s, "green turns blue", colour(1), [0, 0, 255], 1)?;
    expect_rgb(s, "grey has no hue to turn", grey(2), [128; 3], 1)?;
    s.describe("Drag Lightness all the way down");
    props(s, |s| s.drag_slider("Lightness", 0.0))?;
    s.wait_idle()?;
    expect_rgb(s, "lightness −100 is black", grey(4), [0; 3], 1)?;
    s.describe("Undo the lightness drag in one step");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_rgb(s, "white is white again", grey(4), [255; 3], 1)?;
    expect_rgb(s, "the hue shift stays", colour(0), [0, 255, 0], 1)?;
    delete_active_layer(s, "Hue/Saturation")?;

    // Black & White: Rec. 709 weights on gamma values, red → 54.
    s.click("Layer Background")?;
    s.describe("Add Black & White from the chips");
    s.click_role(Role::Button, "B & W")?;
    s.wait_idle()?;
    s.expect_node("Layer Black & White")?;
    expect_rgb(s, "red becomes a dark grey", colour(0), [54; 3], 1)?;
    expect_rgb(s, "green becomes a light grey", colour(1), [182; 3], 1)?;
    s.describe("Type 100% for Red");
    props(s, |s| s.set_field("Red", "100"))?;
    s.wait_idle()?;
    // Weights 1, 0.7152, 0.0722 normalised: red → 255 / 1.7874 = 143.
    expect_rgb(s, "a stronger red weight lightens red", colour(0), [143; 3], 1)?;
    delete_active_layer(s, "Black & White")?;

    // Curves: the Contrast preset darkens the quarter tones and lifts
    // the three-quarter tones; the middle stays.
    s.click("Layer Background")?;
    s.describe("Add Curves from the chips");
    s.click_role(Role::Button, "Curves")?;
    s.wait_idle()?;
    s.expect_node("Layer Curves")?;
    s.describe("Pick the Contrast preset under the curve");
    props(s, |s| s.click("Contrast"))?;
    s.wait_idle()?;
    let dark = rgb(s, grey(1))?;
    let light = rgb(s, grey(3))?;
    s.check(
        "darker quarter tones, lighter three-quarter tones",
        dark[0] < 50 && light[0] > 205,
        "64 → below 50, 192 → above 205",
        format!("{dark:?}, {light:?}"),
    )?;
    expect_rgb(s, "mid grey stays", grey(2), [128; 3], 2)?;
    delete_active_layer(s, "Curves")?;

    // The two filter chips add live filter layers.
    s.click("Layer Background")?;
    s.describe("Add a live blur from the Blur chip");
    s.click_role(Role::Button, "Blur")?;
    s.wait_idle()?;
    s.expect_node("Layer Gaussian Blur")?;
    let edge = rgb(s, (200, 100))?;
    s.check(
        "the edge between grey 64 and 128 is soft",
        edge[0] > 70 && edge[0] < 122,
        "between 70 and 122",
        format!("{edge:?}"),
    )?;
    expect_rgb(s, "the middle of a patch keeps its grey", grey(2), [128; 3], 1)?;
    delete_active_layer(s, "Gaussian Blur")?;
    Ok(())
}

/// Open `top` ▸ `sub` by hand. The Layers panel has buttons named like
/// some submenus ("New adjustment layer"), and `Session::menu` would aim
/// at those: this clicks the topmost control of that name, the menu's.
fn open_submenu(s: &mut Session, top: &str, sub: &str) -> UiResult {
    s.describe(&format!("Open the {top} menu"));
    s.click_role(Role::Button, top)?;
    s.wait_frames(2)?;
    let row = s
        .tree()
        .matches(sub, Some(Role::Button))
        .into_iter()
        .min_by(|a, b| a.rect.min.y.total_cmp(&b.rect.min.y))
        .map(|n| n.rect.center())
        .ok_or_else(|| UiError(format!("no “{sub}” in the {top} menu")))?;
    s.click_at(row, &format!("{top} ▸ {sub}"))
}

// ---- Layer ▸ New adjustment layer ------------------------------------------------

/// Add an adjustment layer from the Layer menu above the photo.
fn add_from_layer_menu(s: &mut Session, name: &str, layer: &str) -> UiResult {
    s.click("Layer Background")?;
    open_submenu(s, "Layer", "New adjustment layer")?;
    s.describe(&format!("Add {name} from Layer ▸ New adjustment layer"));
    s.menu(&format!("Layer > New adjustment layer > {name}"))?;
    s.wait_idle()?;
    s.expect_node(&format!("Layer {layer}"))?;
    Ok(())
}

fn layer_menu_adjustments(s: &mut Session) -> UiResult {
    open_patches(s)?;

    // Brightness +20 adds 20% to every gamma value: 128 → 179.
    add_from_layer_menu(s, "Brightness/Contrast", "Brightness/Contrast")?;
    expect_original(s, "a fresh Brightness/Contrast changes nothing")?;
    s.describe("Type +20 for Brightness");
    props(s, |s| s.set_field("Brightness", "20"))?;
    s.wait_idle()?;
    expect_rgb(s, "brightness +20: grey 128 → 179", grey(2), [179; 3], 1)?;
    delete_active_layer(s, "Brightness/Contrast")?;

    // Vibrance's Saturation −100 leaves the HSL lightness: red → 128.
    add_from_layer_menu(s, "Vibrance", "Vibrance")?;
    s.describe("Type −100 for Saturation");
    props(s, |s| s.set_field("Saturation", "-100"))?;
    s.wait_idle()?;
    expect_rgb(s, "red loses its colour", colour(0), [128; 3], 1)?;
    delete_active_layer(s, "Vibrance")?;

    // Color Balance: midtones +100 towards red with luminosity kept.
    add_from_layer_menu(s, "Color Balance", "Color Balance")?;
    s.describe("Pick Midtones");
    props(s, |s| s.click("Midtones"))?;
    s.describe("Type +100 for Cyan ↔ Red");
    props(s, |s| s.set_field("Cyan ↔ Red", "100"))?;
    s.wait_idle()?;
    expect_rgb(
        s,
        "mid grey turns red at the same luminance",
        grey(2),
        [181, 114, 114],
        1,
    )?;
    delete_active_layer(s, "Color Balance")?;

    // Threshold at 128 (the default) splits 96 from 128.
    add_from_layer_menu(s, "Threshold", "Threshold")?;
    expect_rgb(s, "grey 96 falls below the threshold", grey(5), [0; 3], 0)?;
    expect_rgb(s, "grey 128 is at or above it", grey(2), [255; 3], 0)?;
    s.describe("Type 200 for the threshold level");
    props(s, |s| s.set_field("Level", "200"))?;
    s.wait_idle()?;
    expect_rgb(s, "grey 192 is below 200 now", grey(3), [0; 3], 0)?;
    expect_rgb(s, "white stays white", grey(4), [255; 3], 0)?;
    delete_active_layer(s, "Threshold")?;

    // Posterize 4 (steps at 63.75, 127.5 and 191.25): 96 → 85 and
    // (200, 40, 40) → (255, 0, 0); then 2 levels: 64 → 0, 192 → 255.
    // (Values within a level of a step come out in between: see the
    // results doc.)
    add_from_layer_menu(s, "Posterize", "Posterize")?;
    expect_rgb(s, "4 levels: 96 → 85", grey(5), [85; 3], 1)?;
    expect_rgb(s, "4 levels: (200, 40, 40) → red", colour(3), [255, 0, 0], 1)?;
    s.describe("Type 2 levels");
    props(s, |s| s.set_field("Levels", "2"))?;
    s.wait_idle()?;
    expect_rgb(s, "2 levels: 64 → 0", grey(1), [0; 3], 0)?;
    expect_rgb(s, "2 levels: 192 → 255", grey(3), [255; 3], 0)?;
    delete_active_layer(s, "Posterize")?;

    // Invert: 64 → 191, red → cyan.
    add_from_layer_menu(s, "Invert", "Invert")?;
    expect_rgb(s, "grey 64 → 191", grey(1), [191; 3], 0)?;
    expect_rgb(s, "red → cyan", colour(0), [0, 255, 255], 0)?;
    delete_active_layer(s, "Invert")?;

    // Gradient Map, black to white: a grey image keeps its greys; Reverse
    // inverts them.
    add_from_layer_menu(s, "Gradient Map", "Gradient Map")?;
    expect_rgb(s, "black → white keeps grey 64", grey(1), [64; 3], 2)?;
    expect_rgb(s, "black → white keeps grey 192", grey(3), [192; 3], 2)?;
    s.describe("Tick Reverse");
    props(s, |s| s.click("Reverse"))?;
    s.wait_idle()?;
    expect_rgb(s, "reversed: grey 64 → 191", grey(1), [191; 3], 2)?;
    delete_active_layer(s, "Gradient Map")?;

    // Channel Mixer, "B&W with red filter": gray = red channel.
    add_from_layer_menu(s, "Channel Mixer", "Channel Mixer")?;
    s.describe("Open the Preset list");
    props(s, |s| s.click("Preset"))?;
    s.describe("Pick B&W with red filter");
    s.click("B&W with red filter")?;
    s.wait_idle()?;
    expect_rgb(s, "red → white", colour(0), [255; 3], 0)?;
    expect_rgb(s, "green → black", colour(1), [0; 3], 0)?;
    expect_rgb(s, "(200, 40, 40) → 200", colour(3), [200; 3], 1)?;
    s.describe("The Preset menu names the preset picked");
    s.expect_text("B&W with red filter")?;
    delete_active_layer(s, "Channel Mixer")?;

    // Photo Filter: Warming (85) at 100% without luminosity: grey 128
    // times the filter colour (236, 138, 0) → (118, 69, 0).
    add_from_layer_menu(s, "Photo Filter", "Photo Filter")?;
    let warm = rgb(s, grey(2))?;
    s.check(
        "the default warming filter warms mid grey",
        warm[0] > warm[1] && warm[1] > warm[2],
        "red > green > blue",
        format!("{warm:?}"),
    )?;
    s.describe("Untick Preserve luminosity");
    props(s, |s| s.click("Preserve luminosity"))?;
    s.describe("Type 100% density");
    props(s, |s| s.set_field("Density", "100"))?;
    s.wait_idle()?;
    expect_rgb(s, "grey 128 through the filter colour", grey(2), [118, 69, 0], 1)?;
    delete_active_layer(s, "Photo Filter")?;

    // Selective Color: Reds, Black +100% absolute turns pure red black
    // and leaves blue alone.
    add_from_layer_menu(s, "Selective Color", "Selective Color")?;
    s.describe("Pick Absolute");
    props(s, |s| s.click("Absolute"))?;
    s.describe("Type +100% Black for the reds");
    props(s, |s| s.set_field("Black", "100"))?;
    s.wait_idle()?;
    expect_rgb(s, "red goes black", colour(0), [0; 3], 1)?;
    expect_rgb(s, "blue is not a red", colour(2), [0, 0, 255], 1)?;
    delete_active_layer(s, "Selective Color")?;

    // Undo the last delete: the layer and its settings come back.
    s.describe("Undo the delete with Cmd+Z");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    s.expect_node("Layer Selective Color")?;
    expect_rgb(s, "the setting came back with it", colour(0), [0; 3], 1)?;
    Ok(())
}

// ---- Curves -----------------------------------------------------------------------

type Points = Vec<[f32; 2]>;

/// The points of the master curve of the layer called `name`, and of its
/// red channel.
fn curve_points(s: &mut Session, name: &str) -> UiResult<(Points, Points)> {
    let name = name.to_string();
    let p = s.doc(move |d| match &find_layer(d, &name)?.content {
        lumenply_doc::LayerContent::Adjustment(lumenply_doc::Adjustment::Curves { points, channels }) => {
            Some((points.clone(), channels[0].clone()))
        }
        _ => None,
    })?;
    p.ok_or_else(|| UiError("no Curves layer".into()))
}

/// Is all of `name` inside the Properties panel (no scrolling needed)?
fn fully_in_properties(s: &mut Session, what: &str, name: &str) -> UiResult<bool> {
    let node = s.node(name);
    let panel = s
        .tree()
        .matches("Properties", Some(Role::ScrollView))
        .first()
        .map(|n| n.rect);
    let ok = match (&node, panel) {
        (Some(n), Some(p)) => p.contains_rect(n.rect),
        // Properties has nothing to scroll: all of it is in view.
        (Some(_), None) => true,
        (None, _) => false,
    };
    s.check(
        what,
        ok,
        "inside the Properties panel, no scrolling",
        format!("{:?} in panel {:?}", node.map(|n| n.rect), panel),
    )
}

fn curves(s: &mut Session) -> UiResult {
    // The demo's own Curves layer, at 1440 × 900 with all its layers.
    s.describe("Open the demo photo from the welcome screen");
    s.click("Open the demo photo")?;
    s.wait_idle()?;
    s.describe("Select the demo's Lift shadows curve");
    s.click("Layer Lift shadows")?;
    s.wait_idle()?;
    fully_in_properties(s, "the whole curve is in view", "Curve, *")?;
    fully_in_properties(s, "the curve presets are in view", "Fade")?;
    s.screenshot("demo-curves");

    open_patches(s)?;
    s.describe("Add Curves from the chips");
    s.click_role(Role::Button, "Curves")?;
    s.wait_idle()?;
    fully_in_properties(s, "the whole curve is in view", "Curve, *")?;

    // Photoshop: press anywhere on the line and drag, and a point is
    // added there and follows the pointer in one motion.
    s.describe("Press on the line at three-quarter tones and drag up");
    props(s, |s| {
        s.drag_in("Curve, *", (0.75, 0.25), (0.75, 0.10), "on the line, upwards")
    })?;
    s.wait_idle()?;
    let (pts, _) = curve_points(s, "Curves")?;
    let added = pts.get(1).copied();
    s.check(
        "a point was added where the press was and dragged up",
        pts.len() == 3 && added.is_some_and(|p| (p[0] - 0.75).abs() < 0.02 && (p[1] - 0.9).abs() < 0.02),
        "3 points, the new one at (0.75, 0.90)",
        format!("{pts:?}"),
    )?;
    expect_rgb(s, "grey 192 is lifted to about 230", grey(3), [230; 3], 3)?;
    let steps = s.history()?.len();
    s.describe("Undo the press-and-drag in one step");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let (pts, _) = curve_points(s, "Curves")?;
    s.check_eq("the curve is a diagonal again", pts.len(), 2)?;
    expect_rgb(s, "grey 192 is back", grey(3), [192; 3], 1)?;
    s.describe("Redo it");
    s.key("Cmd+Shift+Z")?;
    s.wait_idle()?;
    let after = s.history()?.len();
    s.check_eq("one history step for the whole drag", after, steps)?;

    // Dragging an existing point moves it.
    s.describe("Drag the new point down, below the diagonal");
    props(s, |s| {
        s.drag_in("Curve, *", (0.75, 0.10), (0.75, 0.40), "the point down")
    })?;
    s.wait_idle()?;
    let (pts, _) = curve_points(s, "Curves")?;
    s.check(
        "the same point moved, no new one",
        pts.len() == 3 && (pts[1][1] - 0.6).abs() < 0.02,
        "3 points, the middle one at y 0.60",
        format!("{pts:?}"),
    )?;

    // Right-click removes a point.
    s.describe("Right-click the point to remove it");
    let at = props(s, |s| s.point_in("Curve, *", 0.75, 0.40))?;
    s.right_click_at(at, "the point")?;
    s.wait_idle()?;
    let (pts, _) = curve_points(s, "Curves")?;
    s.check_eq("back to two points", pts.len(), 2)?;

    // Photoshop: dragging a point off the graph removes it.
    s.describe("Add a point at the quarter tones by clicking the line");
    props(s, |s| {
        s.click_in("Curve, *", 0.25, 0.75, "the line at the quarter tones")
    })?;
    let (pts, _) = curve_points(s, "Curves")?;
    s.check_eq("a third point", pts.len(), 3)?;
    s.describe("Drag that point off the graph");
    let from = props(s, |s| s.point_in("Curve, *", 0.25, 0.75))?;
    let to = props(s, |s| s.point_in("Curve, *", -0.4, 0.75))?;
    s.drag(from, to, 12, "")?;
    s.wait_idle()?;
    let (pts, _) = curve_points(s, "Curves")?;
    s.check_eq("dragged off, the point is gone", pts.len(), 2)?;

    // A channel curve changes only that channel.
    s.describe("Switch to the Red channel");
    props(s, |s| s.click("Red"))?;
    s.describe("Press on the red line at the middle and drag down");
    props(s, |s| {
        s.drag_in("Curve, *", (0.5, 0.5), (0.5, 0.75), "the red line down")
    })?;
    s.wait_idle()?;
    let (master, red) = curve_points(s, "Curves")?;
    s.check(
        "the red curve has a lowered middle point; the master is straight",
        master.len() == 2 && red.len() == 3 && red[1][1] < 0.3,
        "master 2 points, red 3 with the middle below 0.3",
        format!("master {master:?}, red {red:?}"),
    )?;
    let g = rgb(s, grey(2))?;
    s.check(
        "mid grey loses red only",
        g[0] < 90 && g[1].abs_diff(128) <= 1 && g[2].abs_diff(128) <= 1,
        "red below 90, green and blue 128",
        format!("{g:?}"),
    )?;
    s.describe("Back to the master curve");
    props(s, |s| s.click("Master"))?;
    s.describe("Pick the Lighten preset");
    props(s, |s| s.click("Lighten"))?;
    s.wait_idle()?;
    let (master, red) = curve_points(s, "Curves")?;
    s.check(
        "Lighten sets the master curve and keeps the red one",
        master.len() == 3 && red.len() == 3,
        "master 3 points, red 3",
        format!("master {master:?}, red {red:?}"),
    )?;
    Ok(())
}

// ---- masks, visibility, deleting ---------------------------------------------------

fn mask_value(s: &mut Session, layer: &str, x: i32, y: i32) -> UiResult<Option<f32>> {
    s.mask_at(layer, x, y)
}

fn adjustment_mask(s: &mut Session) -> UiResult {
    open_patches(s)?;
    s.describe("Pick the Rectangular Marquee");
    s.click_role(Role::Button, "Rectangular Marquee")?;
    s.describe("Select the left half of the card");
    s.canvas_drag((0.0, 0.0), (300.0, 400.0), 12, "")?;
    s.wait_idle()?;
    let b = s.selection_bounds()?;
    s.check_eq("the left half is selected", b, Some((0, 0, 300, 400)))?;

    // Photoshop: a new adjustment layer takes the selection as its mask.
    s.describe("Add Invert from the More… menu of the quick-add chips");
    s.click("More…")?;
    s.click("Invert")?;
    s.wait_idle()?;
    s.expect_node("Layer Invert")?;
    let m = (
        mask_value(s, "Invert", 150, 100)?,
        mask_value(s, "Invert", 450, 100)?,
    );
    s.check_eq("the layer is masked to the selection", m, (Some(1.0), Some(0.0)))?;
    expect_rgb(s, "inside the selection grey 64 → 191", grey(1), [191; 3], 0)?;
    expect_rgb(s, "red → cyan inside", colour(0), [0, 255, 255], 0)?;
    expect_rgb(s, "outside, white stays white", grey(4), [255; 3], 0)?;
    expect_rgb(s, "outside, the orange stays", colour(4), COLOURS[4], 0)?;
    let history = s.history()?;
    s.check_eq(
        "one history step for the masked layer",
        history.last().cloned(),
        Some("Add Invert layer".to_string()),
    )?;

    s.describe("Deselect with Cmd+D");
    s.key("Cmd+D")?;
    s.describe("Hide the Invert layer with its eye");
    s.click_offset("Layer Invert", 14.0, 19.0, "the eye")?;
    s.wait_idle()?;
    expect_original(s, "hidden, the card is as it was")?;
    s.describe("Show it again");
    s.click_offset("Layer Invert", 14.0, 19.0, "the eye")?;
    s.wait_idle()?;
    expect_rgb(s, "shown, the left half is inverted again", grey(1), [191; 3], 0)?;

    s.describe("Turn the mask off with Layer ▸ Disable mask");
    s.menu("Layer > Disable mask")?;
    s.wait_idle()?;
    expect_rgb(s, "without its mask the whole card inverts", grey(4), [0; 3], 0)?;
    s.describe("And on again with Layer ▸ Enable mask");
    s.menu("Layer > Enable mask")?;
    s.wait_idle()?;
    expect_rgb(s, "masked again", grey(4), [255; 3], 0)?;

    s.describe("Delete the layer with Layer ▸ Delete layer");
    s.menu("Layer > Delete layer")?;
    s.wait_idle()?;
    expect_original(s, "deleted, the card is as it was")?;
    s.describe("Undo the delete");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_rgb(s, "the masked inversion is back", grey(1), [191; 3], 0)?;
    expect_rgb(s, "and stays masked", grey(4), [255; 3], 0)?;

    // A live filter layer takes the selection as its mask too.
    s.click("Layer Background")?;
    s.describe("Select the right half");
    s.click_role(Role::Button, "Rectangular Marquee")?;
    s.canvas_drag((300.0, 0.0), (600.0, 400.0), 12, "")?;
    s.describe("Add a live blur from the Blur chip");
    s.click_role(Role::Button, "Blur")?;
    s.wait_idle()?;
    let m = (
        mask_value(s, "Gaussian Blur", 150, 100)?,
        mask_value(s, "Gaussian Blur", 450, 100)?,
    );
    s.check_eq("the blur is masked to the right half", m, (Some(0.0), Some(1.0)))?;
    Ok(())
}

// ---- Image ▸ Adjustments --------------------------------------------------------

/// Check a dialog titled `title` is open (by role: a chip or a layer can
/// share its name).
fn expect_dialog(s: &mut Session, title: &str) -> UiResult<bool> {
    s.wait_frames(2)?;
    let open = s
        .tree()
        .matches(title, Some(Role::Dialog))
        .first()
        .map(|n| n.describe());
    s.check(
        &format!("the {title} dialog is open"),
        open.is_some(),
        "a dialog",
        format!("{open:?}"),
    )
}

/// Run `f` inside the open dialog `title`. `within` can't scope by role,
/// so when another control shares the title (the Levels chip) the whole
/// window is searched: the names used in dialogs here are unique then.
fn dlg<T>(s: &mut Session, title: &str, f: impl FnOnce(&mut Session) -> UiResult<T>) -> UiResult<T> {
    if s.tree().matches(title, Some(Role::Dialog)).is_empty() {
        return Err(UiError(format!("no {title} dialog is open")));
    }
    let shared = s
        .tree()
        .matches(title, None)
        .iter()
        .any(|n| n.role != Role::Dialog);
    if shared {
        f(s)
    } else {
        s.within(title, f)
    }
}

/// The colour the canvas shows at document pixel `at` (what the user sees,
/// previews included), or None when frames aren't rendered.
fn on_screen(s: &mut Session, at: (u32, u32)) -> UiResult<Option<[u8; 3]>> {
    let p = s.doc_to_screen(at.0 as f32 + 0.5, at.1 as f32 + 0.5)?;
    let Some(img) = s.ui_image() else {
        return Ok(None);
    };
    let ppp = img.width() as f32 / s.screen().width().max(1.0);
    let (x, y) = ((p.x * ppp) as u32, (p.y * ppp) as u32);
    Ok((x < img.width() && y < img.height()).then(|| {
        let q = img.get_pixel(x, y).0;
        [q[0], q[1], q[2]]
    }))
}

/// Check what the canvas shows at `at` (skipped without rendering).
fn expect_shown(s: &mut Session, what: &str, at: (u32, u32), want: [u8; 3], tol: u8) -> UiResult<bool> {
    match on_screen(s, at)? {
        Some(got) => s.check(
            what,
            near(got, want, tol),
            format!("{want:?} on screen"),
            format!("{got:?}"),
        ),
        None => {
            s.note(&format!("{what}: not checked (no rendering)"))?;
            Ok(true)
        }
    }
}

/// Write a low-contrast card (greys 64 to 192 with a blue cast) for the
/// auto commands.
fn write_dull(s: &Session) -> UiResult<std::path::PathBuf> {
    let mut img = image::RgbImage::new(400, 200);
    for (x, _, p) in img.enumerate_pixels_mut() {
        let v = (64 + (x * 128 / 399)) as u8;
        *p = image::Rgb([v, v, v.saturating_add(20).min(212)]);
    }
    let path = s.files().join("dull.png");
    img.save(&path)
        .map_err(|e| UiError(format!("writing the dull card: {e}")))?;
    Ok(path)
}

fn image_adjustments(s: &mut Session) -> UiResult {
    open_patches(s)?;
    let steps = s.history()?.len();

    // Dialog after dialog: each one must take the mouse, never sit under
    // the backdrop of the one before.
    s.describe("Open Shadows/Highlights, then cancel it with its button");
    s.menu("Image > Adjustments > Shadows/Highlights...")?;
    dlg(s, "Shadows/Highlights", |s| s.click("Cancel"))?;
    s.describe("Open a filter dialog and cancel it with its button");
    filter_dialog(s, "Blur", "Box Blur")?;
    dlg(s, "Box Blur", |s| s.click("Cancel"))?;
    s.describe("Open Replace Color and cancel it with its button");
    s.menu("Image > Adjustments > Replace Color...")?;
    dlg(s, "Replace Color", |s| s.click("Cancel"))?;
    s.describe("Open Equalize's neighbour Match Color, cancel with its button");
    s.menu("Image > Adjustments > Match Color...")?;
    dlg(s, "Match Color", |s| s.click("Cancel"))?;
    s.wait_idle()?;
    let n = s.history()?.len();
    s.check_eq("four dialogs cancelled, nothing changed", n, steps)?;

    // Levels with Cmd+L: the canvas previews, Cancel leaves no trace.
    s.describe("Open Levels with Cmd+L");
    s.key("Cmd+L")?;
    expect_dialog(s, "Levels")?;
    s.describe("Type 64 for the input black point");
    dlg(s, "Levels", |s| s.set_field("Input black", "64"))?;
    s.wait_idle()?;
    expect_shown(s, "the canvas previews it: 192 shows 171", grey(3), [171; 3], 3)?;
    expect_rgb(
        s,
        "the document is untouched while previewing",
        grey(1),
        [64; 3],
        0,
    )?;
    s.describe("Untick Preview");
    dlg(s, "Levels", |s| s.click("Preview"))?;
    s.wait_idle()?;
    expect_shown(
        s,
        "without preview the canvas shows the original",
        grey(3),
        [192; 3],
        3,
    )?;
    s.describe("Tick Preview again");
    dlg(s, "Levels", |s| s.click("Preview"))?;
    s.wait_idle()?;
    s.describe("Cancel");
    dlg(s, "Levels", |s| s.click("Cancel"))?;
    s.wait_idle()?;
    expect_shown(
        s,
        "after Cancel the canvas shows the original",
        grey(3),
        [192; 3],
        3,
    )?;
    let after = s.history()?.len();
    s.check_eq("Cancel adds no history step", after, steps)?;

    s.describe("Open Image ▸ Adjustments ▸ Levels... from the menu");
    s.menu("Image > Adjustments > Levels...")?;
    dlg(s, "Levels", |s| s.set_field("Input black", "64"))?;
    dlg(s, "Levels", |s| s.set_field("Input white", "192"))?;
    s.describe("OK");
    dlg(s, "Levels", |s| s.click("OK"))?;
    s.wait_idle()?;
    expect_rgb(s, "64 → 0 in the pixels", grey(1), [0; 3], 1)?;
    expect_rgb(s, "192 → 255 in the pixels", grey(3), [255; 3], 1)?;
    let n = s.layer_names()?.len();
    s.check_eq("one layer still", n, 1)?;
    let h = s.history()?;
    s.check_eq("one history step", h.len(), steps + 1)?;
    s.note(&format!("History: {}", h.join(" → ")))?;
    s.describe("Undo it");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_original(s, "undone, the card is as it was")?;

    // Hue/Saturation with Cmd+U, Enter confirms.
    s.describe("Open Hue/Saturation with Cmd+U");
    s.key("Cmd+U")?;
    dlg(s, "Hue/Saturation", |s| s.set_field("Hue", "120"))?;
    s.describe("Press Enter to apply");
    s.key("Enter")?;
    s.wait_idle()?;
    let open = s.has_node("Hue/Saturation") && s.has_node("OK");
    s.check_eq("Enter closes the dialog", open, false)?;
    expect_rgb(s, "red is green in the pixels", colour(0), [0, 255, 0], 1)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Esc cancels.
    s.describe("Open Curves with Cmd+M");
    s.key("Cmd+M")?;
    expect_dialog(s, "Curves")?;
    dlg(s, "Curves", |s| s.click("Contrast"))?;
    s.describe("Press Esc to cancel");
    s.key("Esc")?;
    s.wait_idle()?;
    expect_shown(s, "Esc leaves the canvas as it was", grey(3), [192; 3], 3)?;
    expect_original(s, "and the pixels")?;

    // Invert (Cmd+I) and Desaturate (Shift+Cmd+U) apply at once.
    s.describe("Invert with Cmd+I");
    s.key("Cmd+I")?;
    s.wait_idle()?;
    expect_rgb(s, "64 → 191", grey(1), [191; 3], 0)?;
    s.key("Cmd+Z")?;
    s.describe("Desaturate with Shift+Cmd+U");
    s.key("Cmd+Shift+U")?;
    s.wait_idle()?;
    expect_rgb(s, "red → its HSL lightness, 128", colour(0), [128; 3], 1)?;
    expect_rgb(s, "(200, 40, 40) → 120", colour(3), [120; 3], 1)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Replace Color: the dialog opens over the left of the image, where
    // the red patch is. Move the dialog, sample red on the canvas, then
    // Shift-click the dark red patch, now under the dialog, in its
    // thumbnail.
    s.describe("Open Image ▸ Adjustments ▸ Replace Color...");
    s.menu("Image > Adjustments > Replace Color...")?;
    expect_dialog(s, "Replace Color")?;
    let dialog = s
        .tree()
        .matches("Replace Color", Some(Role::Dialog))
        .first()
        .map(|n| n.rect);
    if let Some(r) = dialog {
        s.describe("Drag the dialog to the right by its title");
        let from = r.min + vec2(r.width() / 2.0, 18.0);
        s.drag(from, from + vec2(560.0, 0.0), 12, "")?;
        let moved = s
            .tree()
            .matches("Replace Color", Some(Role::Dialog))
            .first()
            .map(|n| n.rect);
        s.check(
            "the dialog moved out of the way",
            moved.is_some_and(|m| m.min.x > r.min.x + 400.0),
            "500 px or so further right",
            format!("{r:?} → {moved:?}"),
        )?;
    }
    // The dialog's backdrop takes the click (it samples), so the harness
    // sees no canvas there: click the screen point.
    let red = s.doc_to_screen(50.0, 300.0)?;
    s.click_at(red, "the red patch on the canvas")?;
    dlg(s, "Replace Color", |s| s.set_field("Hue", "120"))?;
    s.wait_idle()?;
    expect_shown(s, "the preview turns red green", colour(0), [0, 255, 0], 4)?;
    expect_rgb(
        s,
        "the document is untouched while previewing",
        colour(0),
        [255, 0, 0],
        0,
    )?;
    s.describe("Show the image in the thumbnail");
    dlg(s, "Replace Color", |s| s.click("Image"))?;
    s.describe("Shift-click the dark red patch in the thumbnail to add it");
    let at = dlg(s, "Replace Color", |s| {
        s.point_in("Replace Color preview", 350.0 / 600.0, 300.0 / 400.0)
    })?;
    s.drag(at, at, 2, "Shift")?;
    s.wait_idle()?;
    s.expect_text("+1 added")?;
    dlg(s, "Replace Color", |s| s.click("OK"))?;
    s.wait_idle()?;
    expect_rgb(s, "red → green", colour(0), [0, 255, 0], 2)?;
    expect_rgb(s, "(200, 40, 40) → (40, 200, 40)", colour(3), [40, 200, 40], 2)?;
    expect_rgb(s, "blue untouched", colour(2), [0, 0, 255], 0)?;
    expect_rgb(s, "grey untouched", grey(2), [128; 3], 0)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Shadows/Highlights lifts the dark patch, leaves white.
    s.describe("Open Image ▸ Adjustments ▸ Shadows/Highlights...");
    s.menu("Image > Adjustments > Shadows/Highlights...")?;
    expect_dialog(s, "Shadows/Highlights")?;
    dlg(s, "Shadows/Highlights", |s| s.click("OK"))?;
    s.wait_idle()?;
    let dark = rgb(s, grey(1))?;
    s.check(
        "the 64 patch is lifted",
        dark[0] > 70,
        "above 70",
        format!("{dark:?}"),
    )?;
    expect_rgb(s, "white stays", grey(4), [255; 3], 1)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Match Color with no source: Luminance 150 brightens.
    s.describe("Open Image ▸ Adjustments ▸ Match Color...");
    s.menu("Image > Adjustments > Match Color...")?;
    expect_dialog(s, "Match Color")?;
    dlg(s, "Match Color", |s| s.set_field("Luminance", "150"))?;
    dlg(s, "Match Color", |s| s.click("OK"))?;
    s.wait_idle()?;
    let mid = rgb(s, grey(2))?;
    s.check(
        "mid grey is brighter",
        mid[0] > 140,
        "above 140",
        format!("{mid:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Equalize spreads the six greys and six colours over the range.
    s.describe("Image ▸ Adjustments ▸ Equalize");
    s.menu("Image > Adjustments > Equalize")?;
    s.wait_idle()?;
    let (lo, hi) = (rgb(s, grey(0))?, rgb(s, grey(4))?);
    s.check(
        "black and white stay the ends",
        lo[0] <= 2 && hi[0] >= 253,
        "0 and 255",
        format!("{lo:?} {hi:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_original(s, "everything undone, the card is as it was")?;

    // The auto commands on a dull card (greys 64 to 192, a blue cast).
    let dull = write_dull(s)?;
    s.describe("Open a dull, blue-tinged card");
    s.menu("File > Open...")?;
    s.choose_file(&dull)?;
    s.wait_idle()?;
    s.describe("Image ▸ Auto contrast");
    s.menu("Image > Auto contrast")?;
    s.wait_idle()?;
    let (l, r) = (rgb(s, (2, 100))?, rgb(s, (397, 100))?);
    s.check(
        "the darkest grey goes near black, the lightest near white",
        l[0] < 12 && r[0] > 243,
        "left < 12, right > 243",
        format!("{l:?} {r:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    s.describe("Image ▸ Auto color");
    s.menu("Image > Auto color")?;
    s.wait_idle()?;
    let m = rgb(s, (200, 100))?;
    s.check(
        "the blue cast is gone in the middle",
        m[2].abs_diff(m[0]) <= 6,
        "blue within 6 of red",
        format!("{m:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    s.describe("Image ▸ Auto tone");
    s.menu("Image > Auto tone")?;
    s.wait_idle()?;
    let (l, r) = (rgb(s, (2, 100))?, rgb(s, (397, 100))?);
    s.check(
        "auto tone stretches the range",
        l[0] < 12 && r[2] > 243,
        "left < 12, right blue > 243",
        format!("{l:?} {r:?}"),
    )?;
    let names = s.layer_names()?;
    s.note(&format!("Layers after Auto tone: {names:?}"))?;
    Ok(())
}

// ---- filters ------------------------------------------------------------------------

/// Open Filter ▸ `group` ▸ `item`... (the quick-add chips are called Blur
/// and Sharpen too, so the group is opened by hand).
fn filter_dialog(s: &mut Session, group: &str, item: &str) -> UiResult<bool> {
    open_submenu(s, "Filter", group)?;
    s.describe(&format!("Pick {item}..."));
    s.click(&format!("{item}..."))?;
    s.wait_idle()?;
    expect_dialog(s, item)
}

/// Open a filter's dialog and cancel it: nothing changes.
fn open_and_cancel(s: &mut Session, group: &str, item: &str) -> UiResult {
    filter_dialog(s, group, item)?;
    s.describe("Cancel");
    s.key("Esc")?;
    s.wait_idle()?;
    expect_shown(s, "the canvas shows the original again", grey(4), [255; 3], 2)?;
    expect_original(s, &format!("{item} cancelled: nothing changed"))?;
    Ok(())
}

/// Several pixels of the 128 patch, for spotting noise.
fn patch_samples(s: &mut Session) -> UiResult<Vec<u8>> {
    let mut v = Vec::new();
    for i in 0..8 {
        v.push(rgb(s, (210 + i * 10, 40 + i * 15))?[0]);
    }
    Ok(v)
}

fn filters(s: &mut Session) -> UiResult {
    open_patches(s)?;
    let steps = s.history()?.len();

    // Blur ▸ Gaussian Blur: live preview, Cancel, then Apply.
    filter_dialog(s, "Blur", "Gaussian Blur")?;
    s.describe("Type a 4 px radius");
    dlg(s, "Gaussian Blur", |s| s.set_field("Radius", "4"))?;
    s.wait_idle()?;
    let edge = on_screen(s, (499, 100))?;
    s.check(
        "the canvas previews a soft edge between white and grey 96",
        edge.is_none_or(|e| e[0] > 110 && e[0] < 250),
        "between 110 and 250",
        format!("{edge:?}"),
    )?;
    expect_rgb(
        s,
        "the document is untouched while previewing",
        (499, 100),
        [255; 3],
        0,
    )?;
    s.describe("Cancel");
    dlg(s, "Gaussian Blur", |s| s.click("Cancel"))?;
    s.wait_idle()?;
    expect_shown(s, "Cancel puts the canvas back", (499, 100), [255; 3], 2)?;
    let n = s.history()?.len();
    s.check_eq("Cancel adds no history step", n, steps)?;
    filter_dialog(s, "Blur", "Gaussian Blur")?;
    let r = s
        .tree()
        .matches("Radius", Some(Role::SpinButton))
        .first()
        .and_then(|n| n.numeric);
    s.check_eq("Cancelled settings are not kept: 8 px", r, Some(8.0))?;
    dlg(s, "Gaussian Blur", |s| s.set_field("Radius", "4"))?;
    s.describe("Apply");
    dlg(s, "Gaussian Blur", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let e = rgb(s, (499, 100))?;
    s.check(
        "the edge is soft in the pixels",
        e[0] > 110 && e[0] < 250,
        "between 110 and 250",
        format!("{e:?}"),
    )?;
    expect_rgb(s, "the middle of a patch keeps its grey", grey(2), [128; 3], 1)?;
    let h = s.history()?;
    s.check_eq(
        "one step, named for the filter",
        h.last().cloned(),
        Some("Gaussian Blur".to_string()),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_original(s, "undone")?;
    // Photoshop: a filter dialog reopens with the settings last applied.
    filter_dialog(s, "Blur", "Gaussian Blur")?;
    let r = s
        .tree()
        .matches("Radius", Some(Role::SpinButton))
        .first()
        .and_then(|n| n.numeric);
    s.check_eq("it reopens at the 4 px last applied", r, Some(4.0))?;
    s.key("Esc")?;
    s.wait_idle()?;

    // Noise ▸ Add Noise makes a flat patch vary.
    filter_dialog(s, "Noise", "Add Noise")?;
    dlg(s, "Add Noise", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let v = patch_samples(s)?;
    let spread = v.iter().max().unwrap_or(&0) - v.iter().min().unwrap_or(&0);
    s.check(
        "the flat grey is noisy now",
        spread >= 6,
        "a spread of 6 levels or more",
        format!("{v:?}"),
    )?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Pixelate ▸ Mosaic: 16 px cells; the cell x 96..112 holds 4 px of
    // black and 12 of grey 64: averaged in linear light, 55.
    filter_dialog(s, "Pixelate", "Mosaic")?;
    dlg(s, "Mosaic", |s| s.click("Apply"))?;
    s.wait_idle()?;
    expect_rgb(
        s,
        "the cell across the black | 64 edge is one mixed grey",
        (98, 100),
        [55; 3],
        1,
    )?;
    expect_rgb(s, "all of that cell", (110, 100), [55; 3], 1)?;
    expect_rgb(s, "a cell inside the 64 patch stays 64", (150, 100), [64; 3], 1)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Sharpen ▸ Sharpen: overshoot at the 64 | 128 edge, flat stays flat.
    open_submenu(s, "Filter", "Sharpen")?;
    s.describe("Pick Sharpen...");
    s.click("Sharpen...")?;
    expect_dialog(s, "Sharpen")?;
    dlg(s, "Sharpen", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let dark = rgb(s, (198, 100))?;
    let light = rgb(s, (201, 100))?;
    s.check(
        "darker just left of the edge, lighter just right",
        dark[0] < 64 && light[0] > 128,
        "below 64 | above 128",
        format!("{dark:?} | {light:?}"),
    )?;
    expect_rgb(s, "flat grey stays", grey(2), [128; 3], 1)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Stylize ▸ Find Edges: flat areas white, edges dark.
    filter_dialog(s, "Stylize", "Find Edges")?;
    dlg(s, "Find Edges", |s| s.click("Apply"))?;
    s.wait_idle()?;
    expect_rgb(s, "a flat patch turns white", grey(2), [255; 3], 2)?;
    let e = rgb(s, (200, 100))?;
    s.check("the edge is dark", e[0] < 200, "below 200", format!("{e:?}"))?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Stylize ▸ Emboss: flat areas mid grey.
    filter_dialog(s, "Stylize", "Emboss")?;
    dlg(s, "Emboss", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let flat = rgb(s, grey(1))?;
    s.note(&format!("Emboss on flat grey 64: {flat:?}"))?;
    expect_rgb(s, "a flat patch turns mid grey", grey(1), [128; 3], 2)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;

    // Other ▸ High Pass: flat areas mid grey.
    filter_dialog(s, "Other", "High Pass")?;
    dlg(s, "High Pass", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let flat = rgb(s, grey(1))?;
    s.note(&format!("High Pass on flat grey 64: {flat:?}"))?;
    // Neutral is 50% in linear light (188), the compositor's Overlay neutral;
    // Photoshop's is 128 (see the results doc).
    expect_rgb(s, "a flat patch turns linear mid grey", grey(1), [188; 3], 2)?;
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_original(s, "all undone")?;

    // The rest open, preview and cancel cleanly.
    open_and_cancel(s, "Blur", "Box Blur")?;
    open_and_cancel(s, "Blur", "Motion Blur")?;
    open_and_cancel(s, "Blur", "Surface Blur")?;
    open_and_cancel(s, "Blur", "Lens Blur")?;
    open_and_cancel(s, "Noise", "Median")?;
    open_and_cancel(s, "Noise", "Dust & Scratches")?;
    let n = s.history()?.len();
    s.check_eq("cancelling added no steps", n, steps)?;
    Ok(())
}

// ---- live filter layers ----------------------------------------------------------

fn live_filters(s: &mut Session) -> UiResult {
    open_patches(s)?;
    s.describe("Add a live Motion Blur from Filter ▸ Live filter layer");
    s.menu("Filter > Live filter layer > Motion Blur")?;
    s.wait_idle()?;
    s.expect_node("Layer Motion Blur")?;
    let e = rgb(s, (499, 100))?;
    s.check(
        "horizontal motion blur softens the vertical edge",
        e[0] < 250,
        "below 250",
        format!("{e:?}"),
    )?;
    expect_rgb(
        s,
        "the photo's own pixels are untouched",
        (499, 100),
        [255; 3],
        255,
    )?;
    let own = s.layer_pixel("Background", 499, 100)?;
    s.check_eq("Background keeps its pixels", own.map(|p| p[0]), Some(255))?;
    let steps = s.history()?.len();
    s.describe("Drag Distance down to its minimum");
    props(s, |s| s.drag_slider("Distance", 0.0))?;
    s.wait_idle()?;
    let n = s.history()?.len();
    s.check_eq("the drag is one history step", n, steps + 1)?;
    let e2 = rgb(s, (499, 100))?;
    s.check(
        "a 1 px blur barely touches the edge",
        e2[0] > e[0],
        format!("above {}", e[0]),
        format!("{e2:?}"),
    )?;
    s.describe("Undo the drag");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let e3 = rgb(s, (499, 100))?;
    s.check_eq("the 20 px blur is back", e3, e)?;
    s.describe("Hide the live filter with its eye");
    s.click_offset("Layer Motion Blur", 14.0, 19.0, "the eye")?;
    s.wait_idle()?;
    expect_original(s, "hidden, the card is as it was")?;
    s.click_offset("Layer Motion Blur", 14.0, 19.0, "the eye")?;
    delete_active_layer(s, "Motion Blur")?;

    // From the Layer menu (and the Layers panel button's menu).
    s.click("Layer Background")?;
    open_submenu(s, "Layer", "New live filter layer")?;
    s.describe("Pick High Pass");
    s.click("High Pass")?;
    s.wait_idle()?;
    s.expect_node("Layer High Pass")?;
    let flat = rgb(s, grey(1))?;
    s.check(
        "a flat patch turns linear mid grey (188)",
        flat[0].abs_diff(188) <= 2,
        "188",
        format!("{flat:?}"),
    )?;
    s.describe("Type a 10 px radius");
    props(s, |s| s.set_field("Radius", "10"))?;
    s.wait_idle()?;
    let r = s.doc(|d| match &find_layer(d, "High Pass")?.content {
        lumenply_doc::LayerContent::Filter(lumenply_doc::Filter::HighPass { radius }) => Some(*radius),
        _ => None,
    })?;
    s.check_eq("the radius is 10 px", r, Some(10.0))?;
    s.describe("Set the live filter's opacity to 0%");
    props(s, |s| s.set_field("Opacity", "0"))?;
    s.wait_idle()?;
    expect_original(s, "at 0% the card shows through untouched")?;
    Ok(())
}

// ---- smart filters --------------------------------------------------------------

fn smart_filter_names(s: &mut Session) -> UiResult<Vec<String>> {
    s.doc(|d| {
        find_layer(d, "Background")
            .map(|l| {
                l.smart_filters
                    .filters
                    .iter()
                    .map(|f| f.filter.name().to_string())
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn smart_filters(s: &mut Session) -> UiResult {
    open_patches(s)?;
    s.describe("Filter ▸ Convert for smart filters");
    s.menu("Filter > Convert for smart filters")?;
    s.wait_idle()?;
    let smart = s.doc(|d| find_layer(d, "Background").is_some_and(|l| l.smart_layer().is_some()))?;
    s.check_eq("the photo is a smart object", smart, true)?;
    s.describe("Open the Filter menu: it says filters go on as smart filters");
    s.click_role(Role::Button, "Filter")?;
    s.expect_text("Filters below are added as smart filters")?;
    s.expect_disabled("Convert for smart filters", "Already a smart object")?;
    s.key("Esc")?;

    filter_dialog(s, "Blur", "Gaussian Blur")?;
    dlg(s, "Gaussian Blur", |s| s.set_field("Radius", "4"))?;
    dlg(s, "Gaussian Blur", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let names = smart_filter_names(s)?;
    s.check_eq(
        "a smart filter, not baked",
        names,
        vec!["Gaussian Blur".to_string()],
    )?;
    let soft = rgb(s, (499, 100))?;
    s.check(
        "the edge is soft",
        soft[0] < 250,
        "below 250",
        format!("{soft:?}"),
    )?;

    filter_dialog(s, "Stylize", "Find Edges")?;
    dlg(s, "Find Edges", |s| s.click("Apply"))?;
    s.wait_idle()?;
    let names = smart_filter_names(s)?;
    s.check_eq(
        "two smart filters, in the order applied",
        names,
        vec!["Gaussian Blur".to_string(), "Find Edges".to_string()],
    )?;
    let stacked = rgb(s, grey(2))?;
    s.note(&format!("Blur then Find Edges at the 128 patch: {stacked:?}"))?;

    s.describe("Move Find Edges down, so it applies first");
    props(s, |s| s.click("Move Find Edges down"))?;
    s.wait_idle()?;
    let names = smart_filter_names(s)?;
    s.check_eq(
        "the order is swapped",
        names,
        vec!["Find Edges".to_string(), "Gaussian Blur".to_string()],
    )?;
    s.describe("Undo the move");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    let names = smart_filter_names(s)?;
    s.check_eq(
        "back in the first order",
        names.first().cloned(),
        Some("Gaussian Blur".to_string()),
    )?;

    s.describe("Hide Find Edges with its check box");
    props(s, |s| s.click("Show Find Edges"))?;
    s.wait_idle()?;
    let e = rgb(s, (499, 100))?;
    s.check_eq("only the blur shows", e, soft)?;
    s.describe("Show it again");
    props(s, |s| s.click("Show Find Edges"))?;
    s.wait_idle()?;

    // Properties keeps the filter open by its place in the stack, so after
    // the undo the blur's settings are the ones open (see the results).
    if !s.has_node("Radius") {
        s.describe("Open the Gaussian Blur filter's settings");
        props(s, |s| s.click("Gaussian Blur"))?;
    }
    s.describe("Type a 1 px radius for the blur");
    props(s, |s| s.set_field("Radius", "1"))?;
    s.wait_idle()?;
    let r = s.doc(|d| {
        find_layer(d, "Background").and_then(|l| match l.smart_filters.filters.first()?.filter {
            lumenply_doc::Filter::GaussianBlur { radius } => Some(radius),
            _ => None,
        })
    })?;
    s.check_eq("the blur's radius is edited in place", r, Some(1.0))?;

    // A filter mask from a selection.
    s.describe("Select the left half");
    s.click_role(Role::Button, "Rectangular Marquee")?;
    s.canvas_drag((0.0, 0.0), (300.0, 400.0), 12, "")?;
    s.click("Layer Background")?;
    s.describe("Mask the smart filters to the selection");
    props(s, |s| s.click("Mask"))?;
    s.wait_idle()?;
    s.key("Cmd+D")?;
    s.wait_idle()?;
    expect_rgb(
        s,
        "outside the mask the right half is the original",
        (499, 100),
        [255; 3],
        0,
    )?;
    let left = rgb(s, grey(2))?;
    s.check(
        "inside the mask the filters show",
        left != [128; 3],
        "not plain 128",
        format!("{left:?}"),
    )?;

    s.describe("Delete Find Edges");
    props(s, |s| s.click("Delete Find Edges"))?;
    s.wait_idle()?;
    let names = smart_filter_names(s)?;
    s.check_eq("only the blur is left", names, vec!["Gaussian Blur".to_string()])?;
    s.describe("Turn every smart filter off");
    props(s, |s| s.click("Show smart filters"))?;
    s.wait_idle()?;
    expect_original(s, "with the smart filters off, the original")?;
    Ok(())
}

// ---- Camera Raw Filter -----------------------------------------------------------

/// Check a number field called `name` is on screen (its label and slider
/// share the name, so `expect_node` can't tell which is meant).
fn expect_field(s: &mut Session, name: &str) -> UiResult<bool> {
    s.wait_frames(2)?;
    let n = s
        .tree()
        .matches(name, Some(Role::SpinButton))
        .first()
        .map(|n| n.describe());
    s.check(
        &format!("the {name} field is shown"),
        n.is_some(),
        "a number field",
        format!("{n:?}"),
    )
}

fn camera_raw(s: &mut Session) -> UiResult {
    open_patches(s)?;
    s.describe("Open the Camera Raw Filter with Shift+Cmd+A");
    s.key("Cmd+Shift+A")?;
    s.wait_idle()?;
    s.dump_tree("camera-raw");
    s.screenshot("camera-raw");
    expect_field(s, "Exposure")?;
    s.describe("Type +1 EV of exposure");
    s.set_field("Exposure", "1")?;
    s.wait_idle()?;
    s.describe("Cancel");
    s.click("Cancel")?;
    s.wait_idle()?;
    expect_original(s, "Cancel leaves the card as it was")?;
    let n = s.layer_names()?.len();
    s.check_eq("and adds no layer", n, 1)?;

    s.describe("Open it again from the Filter menu");
    s.menu("Filter > Camera Raw Filter...")?;
    s.wait_idle()?;
    s.set_field("Exposure", "1")?;
    s.wait_idle()?;
    s.describe("OK");
    s.click("OK")?;
    s.wait_idle()?;
    let g = rgb(s, grey(2))?;
    s.check(
        "+1 EV brightens mid grey a lot",
        g[0] > 160,
        "above 160",
        format!("{g:?}"),
    )?;
    expect_rgb(s, "black stays black", grey(0), [0; 3], 1)?;
    let h = s.history()?;
    s.note(&format!("History: {}", h.join(" → ")))?;
    s.describe("Undo");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_original(s, "undone in one step")?;

    // As a live layer.
    s.menu("Filter > Camera Raw Filter...")?;
    s.wait_idle()?;
    s.describe("Apply it as a live filter layer");
    s.click("Apply the Camera Raw Filter as")?;
    s.click("Live filter layer")?;
    s.set_field("Exposure", "-1")?;
    s.click("OK")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq("a live Camera Raw layer above the photo", names.len(), 2)?;
    s.note(&format!("Layers: {names:?}"))?;
    let g = rgb(s, grey(2))?;
    s.check(
        "−1 EV darkens mid grey",
        g[0] < 110,
        "below 110",
        format!("{g:?}"),
    )?;
    s.describe("Re-open it from Properties");
    props(s, |s| s.click("Edit in Camera Raw…"))?;
    s.wait_idle()?;
    expect_field(s, "Exposure")?;
    s.key("Esc")?;
    s.wait_idle()?;
    Ok(())
}

// ---- Color Lookup ----------------------------------------------------------------

/// A 2 × 2 × 2 .cube that inverts.
const INVERT_CUBE: &str = "TITLE \"Invert\"\nLUT_3D_SIZE 2\n\
1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

fn color_lookup(s: &mut Session) -> UiResult {
    open_patches(s)?;
    add_from_layer_menu(s, "Color Lookup", "Color Lookup")?;
    expect_original(s, "with no table chosen nothing changes")?;
    s.expect_text("No table chosen")?;
    s.describe("Open the Look list");
    props(s, |s| s.click("Look"))?;
    s.describe("Pick Monochrome Contrast");
    s.click("Monochrome Contrast")?;
    s.wait_idle()?;
    let r = rgb(s, colour(0))?;
    s.check(
        "red turns grey",
        r[0] == r[1] && r[1] == r[2],
        "equal channels",
        format!("{r:?}"),
    )?;
    s.describe("Undo the look");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_original(s, "undone: no table again")?;

    let cube = s.files().join("invert.cube");
    std::fs::write(&cube, INVERT_CUBE).map_err(|e| UiError(format!("writing the cube: {e}")))?;
    s.describe("Load a .cube file with Load 3D LUT…");
    props(s, |s| s.click("Load 3D LUT…"))?;
    s.choose_file(&cube)?;
    s.wait_idle()?;
    expect_rgb(s, "the inverting table: 64 → 191", grey(1), [191; 3], 1)?;
    expect_rgb(s, "red → cyan", colour(0), [0, 255, 255], 1)?;
    s.expect_text("invert · 2×2×2 table")?;

    let bad = s.files().join("broken.cube");
    std::fs::write(&bad, "LUT_3D_SIZE 2\n0 0 0\n").map_err(|e| UiError(format!("writing: {e}")))?;
    s.describe("Try to load a broken .cube");
    props(s, |s| s.click("Load 3D LUT…"))?;
    s.choose_file(&bad)?;
    s.wait_idle()?;
    s.expect_text("Could not load broken.cube")?;
    expect_rgb(s, "the loaded table stays", grey(1), [191; 3], 1)?;

    // Layer ▸ New adjustment layer ▸ Load 3D LUT... on the photo adds one.
    s.click("Layer Background")?;
    open_submenu(s, "Layer", "New adjustment layer")?;
    s.describe("Pick Load 3D LUT...");
    s.click("Load 3D LUT...")?;
    s.choose_file(&cube)?;
    s.wait_idle()?;
    let n = s.layer_names()?.len();
    s.check_eq("a second Color Lookup layer", n, 3)?;
    expect_rgb(s, "inverted twice: back to 64", grey(1), [64; 3], 2)?;
    Ok(())
}

// ---- layer styles -----------------------------------------------------------------

/// A new white document with a brush-coloured rectangle on its own layer.
fn shape_document(s: &mut Session) -> UiResult {
    s.describe("Make a new 400 × 300 image");
    s.menu("File > New...")?;
    s.set_field("Width", "400")?;
    s.set_field("Height", "300")?;
    s.click("Create")?;
    s.wait_idle()?;
    s.describe("Add a layer");
    s.menu("Layer > New pixel layer")?;
    s.describe("Select a rectangle");
    s.click_role(Role::Button, "Rectangular Marquee")?;
    s.canvas_drag((100.0, 100.0), (300.0, 200.0), 10, "")?;
    s.describe("Fill it with the brush colour");
    s.menu("Edit > Fill with brush colour")?;
    s.key("Cmd+D")?;
    s.wait_idle()?;
    expect_rgb(
        s,
        "the rectangle is the brush colour",
        (200, 150),
        [0x1a, 0x2e, 0x8c],
        1,
    )?;
    Ok(())
}

/// Scroll Properties with the wheel until the `role` control `name` is in
/// view, as a person would.
fn reveal(s: &mut Session, role: Role, name: &str) -> UiResult {
    for _ in 0..12 {
        let row = s.tree().matches(name, Some(role)).first().map(|n| n.rect);
        let panel = s
            .tree()
            .matches("Properties", Some(Role::ScrollView))
            .first()
            .map(|n| n.rect);
        match (row, panel) {
            (Some(r), Some(p)) if r.bottom() > p.bottom() => s.scroll("Properties", 60.0)?,
            (Some(r), Some(p)) if r.top() < p.top() => s.scroll("Properties", -60.0)?,
            _ => break,
        }
    }
    Ok(())
}

fn tick_effect(s: &mut Session, name: &str) -> UiResult {
    reveal(s, Role::CheckBox, name)?;
    s.describe(&format!("Tick {name}"));
    props(s, |s| s.click_role(Role::CheckBox, name))?;
    s.wait_idle()
}

fn layer_styles(s: &mut Session) -> UiResult {
    shape_document(s)?;
    s.describe("Open EFFECTS in Properties");
    props(s, |s| s.click("EFFECTS"))?;

    // Color overlay: the default orange, (1, 0.45, 0.1) linear.
    tick_effect(s, "Color overlay")?;
    expect_rgb(
        s,
        "the rectangle is overlaid orange",
        (200, 150),
        [255, 179, 89],
        1,
    )?;
    expect_rgb(s, "the paper stays white", (50, 50), [255; 3], 0)?;
    s.expect_text("EFFECTS · 1 ON")?;
    s.describe("Undo it");
    s.key("Cmd+Z")?;
    s.wait_idle()?;
    expect_rgb(s, "back to the brush colour", (200, 150), [0x1a, 0x2e, 0x8c], 1)?;

    // Stroke: 3 px outside, visible on white paper.
    tick_effect(s, "Stroke")?;
    expect_rgb(s, "a dark stroke just outside the edge", (98, 150), [0; 3], 8)?;
    expect_rgb(s, "nothing 6 px out", (94, 150), [255; 3], 0)?;
    s.describe("Make it 10 px");
    reveal(s, Role::SpinButton, "Size")?;
    props(s, |s| s.set_field("Size", "10"))?;
    s.wait_idle()?;
    expect_rgb(s, "10 px out is stroked now", (93, 150), [0; 3], 8)?;
    tick_effect(s, "Stroke")?;
    expect_rgb(s, "stroke off", (98, 150), [255; 3], 0)?;

    // Drop shadow: 4 px down and right, soft.
    tick_effect(s, "Drop shadow")?;
    let below = rgb(s, (200, 203))?;
    s.check(
        "a shadow under the bottom edge",
        below[0] < 220,
        "darker than 220",
        format!("{below:?}"),
    )?;
    expect_rgb(s, "no shadow above the top edge", (200, 95), [255; 3], 2)?;
    tick_effect(s, "Drop shadow")?;

    // The rest change the rectangle or around it.
    for (name, at) in [
        ("Outer glow", (96, 150)),
        ("Inner shadow", (102, 102)),
        ("Inner glow", (102, 150)),
        ("Bevel", (101, 101)),
        ("Gradient overlay", (200, 150)),
        ("Pattern overlay", (200, 150)),
    ] {
        let before = rgb(s, at)?;
        tick_effect(s, name)?;
        let after = rgb(s, at)?;
        s.check(
            &format!("{name} changes the pixels at {at:?}"),
            after != before,
            format!("not {before:?}"),
            format!("{after:?}"),
        )?;
        tick_effect(s, name)?;
        let off = rgb(s, at)?;
        s.check_eq(&format!("{name} off again"), off, before)?;
    }

    // A second layer gets its own effects.
    s.describe("Add a second layer with a small square");
    s.menu("Layer > New pixel layer")?;
    s.click_role(Role::Button, "Rectangular Marquee")?;
    s.canvas_drag((320.0, 20.0), (380.0, 80.0), 8, "")?;
    s.menu("Edit > Fill with brush colour")?;
    s.key("Cmd+D")?;
    tick_effect(s, "Color overlay")?;
    expect_rgb(s, "the square is orange", (350, 50), [255, 179, 89], 1)?;
    expect_rgb(
        s,
        "the first rectangle keeps its colour",
        (200, 150),
        [0x1a, 0x2e, 0x8c],
        1,
    )?;
    let fx = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.effects.is_empty()))?;
    s.check_eq("the first layer has no effects on", fx, Some(true))?;

    // The histogram follows the image.
    s.expect_text("HISTOGRAM")?;
    Ok(())
}

// ---- a big document -----------------------------------------------------------------

fn timed(s: &mut Session, what: &str, f: impl FnOnce(&mut Session) -> UiResult) -> UiResult<u128> {
    let t = std::time::Instant::now();
    f(s)?;
    s.wait_idle()?;
    let ms = t.elapsed().as_millis();
    s.note(&format!("TIMING {what}: {ms} ms"))?;
    Ok(ms)
}

fn big_document(s: &mut Session) -> UiResult {
    let path = s.files().join("big.png");
    let mut img = image::RgbImage::new(4000, 3000);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([
            (x * 255 / 3999) as u8,
            (y * 255 / 2999) as u8,
            ((x ^ y) & 255) as u8,
        ]);
    }
    img.save(&path).map_err(|e| UiError(format!("writing: {e}")))?;
    timed(s, "open a 4000 × 3000 PNG", |s| {
        s.menu("File > Open...")?;
        s.choose_file(&path)
    })?;
    timed(s, "add Curves from the chip", |s| {
        s.click_role(Role::Button, "Curves")
    })?;
    timed(s, "press-and-drag on the curve (12 frames)", |s| {
        props(s, |s| {
            s.drag_in("Curve, *", (0.5, 0.5), (0.5, 0.3), "the curve up")
        })
    })?;
    timed(s, "add Hue/Saturation", |s| s.click_role(Role::Button, "Hue/Sat"))?;
    timed(s, "drag the Hue slider (10 frames)", |s| {
        props(s, |s| s.drag_slider("Hue", 0.8))
    })?;
    timed(s, "add a live Gaussian Blur", |s| {
        s.click_role(Role::Button, "Blur")
    })?;
    timed(s, "drag the live blur's Radius (10 frames)", |s| {
        props(s, |s| s.drag_slider("Radius", 0.7))
    })?;
    s.click("Layer Background")?;
    timed(s, "open Levels (Cmd+L)", |s| s.key("Cmd+L"))?;
    timed(s, "type an input black point (preview)", |s| {
        s.set_field("Input black", "40")
    })?;
    timed(s, "drag Gamma in the dialog (preview)", |s| {
        s.drag_slider("Gamma", 0.4)
    })?;
    timed(s, "apply Levels", |s| s.click("OK"))?;
    timed(s, "open Gaussian Blur...", |s| {
        open_submenu(s, "Filter", "Blur")?;
        s.click("Gaussian Blur...")
    })?;
    timed(s, "type a 20 px radius (preview)", |s| {
        s.set_field("Radius", "20")
    })?;
    timed(s, "drag the dialog's Radius (preview)", |s| {
        s.drag_slider("Radius", 0.3)
    })?;
    timed(s, "apply the blur", |s| s.click("Apply"))?;
    timed(s, "undo the blur", |s| s.key("Cmd+Z"))?;
    Ok(())
}
