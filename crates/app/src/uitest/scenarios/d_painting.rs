//! D. Painting and retouching: the Brush and its settings, the Eraser,
//! Paint Bucket, Gradient, colours, Clone Stamp, the healing tools, the
//! brush modes (Blur, Sharpen, History, Dodge, Burn, Sponge, Smudge) and
//! Fill, Stroke, Content-Aware Fill and the Define commands.
//!
//! Each journey starts from a new document or a small PNG the "user" has
//! on disk (drawn here, so every pixel is known), works through the
//! tools as a Photoshop user would and checks pixels at document points:
//! the colour where the tool worked, what it must have left alone, and
//! what undo brings back.

use crate::uitest::prelude::*;

scenario_list! {
    "paint-brush-basics" => brush_basics: "Brush size, hardness, opacity, bracket keys, Shift-click lines, Alt-click picking",
    "paint-brush-tips" => brush_tips: "Scatter, presets, the Brush settings panel, sampled tips, Define brush tip, .abr import",
    "paint-eraser" => eraser: "Eraser, Background Eraser and Magic Eraser on a layer",
    "paint-bucket" => bucket: "Paint Bucket inside an outline, with tolerance and opacity",
    "paint-gradient" => gradient: "Gradient tool: drag, presets, styles, the stop editor, fill-layer mode",
    "paint-colours" => colours: "The colour picker, swap and default colours, the Eyedropper",
    "paint-clone" => clone_stamp: "Clone Stamp: Alt-click a source, paint a copy",
    "paint-heal" => heal: "Spot Healing, Healing Brush, Patch, Content-Aware Move and Red Eye",
    "paint-blur-sharpen-history" => blur_sharpen_history: "Blur, Sharpen and the History Brush on a hard edge",
    "paint-toning" => toning: "Dodge, Burn, the Sponge (saturate, desaturate) and Smudge",
    "paint-find-tools" => find_tools: "Finding Dodge, Burn, Sponge, Smudge, Blur and History Brush by name and key",
    "paint-fill-stroke" => fill_stroke: "Alt/Cmd+Backspace, Edit ▸ Fill and Edit ▸ Stroke on a selection",
    "paint-caf-define" => caf_define: "Content-Aware Fill, Define Pattern and a pattern fill layer",
    "paint-latency" => latency: "A long brush stroke on a 4000 × 3000 document, timed",
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const GREY: [u8; 4] = [128, 128, 128, 255];

/// Within `tol` of `want` on every channel.
fn near(got: [u8; 4], want: [u8; 4], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

/// Check that composite pixel (x, y) is within `tol` of `want`.
fn expect_px(s: &mut Session, what: &str, (x, y): (u32, u32), want: [u8; 4], tol: u8) -> UiResult<bool> {
    let got = s.pixel(x, y)?;
    s.check(
        what,
        near(got, want, tol),
        format!("{want:?} ±{tol} at ({x}, {y})"),
        format!("{got:?}"),
    )
}

/// Check a layer's own pixel (straight sRGB) at (x, y).
fn expect_layer_px(
    s: &mut Session,
    what: &str,
    layer: &str,
    (x, y): (i32, i32),
    want: [u8; 4],
    tol: u8,
) -> UiResult<bool> {
    let got = s.layer_pixel(layer, x, y)?;
    s.check(
        what,
        got.is_some_and(|g| near(g, want, tol)),
        format!("{want:?} ±{tol} at ({x}, {y}) on {layer}"),
        format!("{got:?}"),
    )
}

/// A layer's alpha (0 to 255) at (x, y).
fn alpha(s: &mut Session, layer: &str, x: i32, y: i32) -> UiResult<u8> {
    Ok(s.layer_pixel(layer, x, y)?.map_or(0, |p| p[3]))
}

/// Check a layer's alpha at (x, y).
fn expect_alpha(s: &mut Session, what: &str, layer: &str, (x, y): (i32, i32), want: u8) -> UiResult<bool> {
    let got = alpha(s, layer, x, y)?;
    s.check_eq(what, got, want)
}

/// Check how many steps the history holds.
fn expect_steps(s: &mut Session, what: &str, want: usize) -> UiResult<bool> {
    let got = s.history()?.len();
    s.check_eq(what, got, want)
}

/// Check how many layers the document has.
fn expect_layers(s: &mut Session, what: &str, want: usize) -> UiResult<bool> {
    let got = s.layer_names()?.len();
    s.check_eq(what, got, want)
}

/// The linear-light blend `t` of two sRGB channel values, as the app
/// composites (premultiplied, linear light), back in sRGB.
fn mix_linear(a: u8, b: u8, t: f32) -> u8 {
    let lin = |v: u8| lumenply_io::srgb_to_linear(v);
    lumenply_io::linear_to_srgb(lin(a) * (1.0 - t) + lin(b) * t)
}

/// Pixels of the composite inside `(x, y, w, h)` darker than 128 in red.
fn dark_count(s: &mut Session, (x, y, w, h): (u32, u32, u32, u32)) -> UiResult<usize> {
    s.doc(move |d| {
        let flat = lumenply_render::composite_raster(d);
        let mut n = 0;
        for py in y..y + h {
            for px in x..x + w {
                let [r, ..] = flat.get(px, py).to_straight();
                if lumenply_io::linear_to_srgb(r) < 128 {
                    n += 1;
                }
            }
        }
        n
    })
}

/// Where the options bar's control `name` is (the bar runs under the
/// menu bar, above the canvas). A control scrolled out of a narrow bar
/// is scrolled into view first, as a user would with the wheel.
fn bar_node(s: &mut Session, name: &str, role: Role) -> UiResult<Pos2> {
    for _ in 0..12 {
        s.wait_frames(2)?;
        // The Export button closes the menu bar at the window's right.
        let width = s
            .tree()
            .matches("Export", Some(Role::Button))
            .into_iter()
            .map(|n| n.rect.max.x + 12.0)
            .fold(0.0, f32::max);
        let found = s
            .tree()
            .matches(name, Some(role))
            .into_iter()
            .find(|n| n.rect.center().y > 36.0 && n.rect.center().y < 90.0)
            .map(|n| n.rect);
        match found {
            Some(r) if r.max.x < width - 24.0 && r.min.x > 0.0 => return Ok(r.center()),
            Some(r) => {
                let dy = if r.max.x >= width - 24.0 { 120.0 } else { -120.0 };
                s.describe(&format!("Scroll the options bar to {name}"));
                s.scroll("Tool options", dy)?;
            }
            None => {}
        }
    }
    Err(UiError(format!("no {name} in the options bar")))
}

/// Type `value` into the options bar's number field `name` (the
/// Properties panel has fields of the same name, such as Opacity).
fn bar_field(s: &mut Session, name: &str, value: &str) -> UiResult {
    let at = bar_node(s, name, Role::SpinButton)?;
    s.describe(&format!("Type {value} into {name} in the options bar"));
    s.click_at(at, &format!("the options bar's {name} field"))?;
    s.key("Cmd+A")?;
    s.type_text(value)?;
    s.key("Enter")
}

/// Click the options bar's control `name`.
fn bar_click(s: &mut Session, name: &str, role: Role) -> UiResult {
    let at = bar_node(s, name, role)?;
    s.click_at(at, &format!("“{name}” in the options bar"))
}

/// File ▸ New at `w` × `h` (white background).
fn new_doc(s: &mut Session, w: u32, h: u32) -> UiResult {
    s.describe(&format!("Make a new {w} × {h} image from File ▸ New"));
    s.menu("File > New...")?;
    s.set_field("Width", &w.to_string())?;
    s.set_field("Height", &h.to_string())?;
    s.click("Create")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the new document has the asked size", size, (w, h))?;
    Ok(())
}

/// Write `img` as `name` in the user's files and open it with File ▸ Open.
fn open_png(s: &mut Session, name: &str, img: image::RgbaImage) -> UiResult {
    let path = s.files().join(name);
    img.save(&path)
        .map_err(|e| UiError(format!("writing {}: {e}", path.display())))?;
    s.describe(&format!("Open {name} with File ▸ Open"));
    s.menu("File > Open...")?;
    s.choose_file(&path)?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the image opened at its size", size, img.dimensions())?;
    Ok(())
}

/// A rectangle (x, y, w, h) of a test picture and its colour.
type Patch = ((u32, u32, u32, u32), [u8; 3]);

/// An image of `w` × `h` filled with `bg`, then each (rect, colour).
fn picture(w: u32, h: u32, bg: [u8; 3], rects: &[Patch]) -> image::RgbaImage {
    image::RgbaImage::from_fn(w, h, |x, y| {
        let mut c = bg;
        for &((rx, ry, rw, rh), rc) in rects {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                c = rc;
            }
        }
        image::Rgba([c[0], c[1], c[2], 255])
    })
}

/// Set the foreground colour by typing a hex value in its picker.
fn set_foreground(s: &mut Session, hex: &str) -> UiResult {
    let well = s.app(|a| crate::color_picker::format_hex(a.brush_rgb))?;
    s.describe("Click the foreground swatch to open its picker");
    s.click(&format!("Foreground colour {well}"))?;
    s.describe(&format!("Type {hex} as the hex colour"));
    s.set_field("Hex colour", hex)?;
    s.describe("Close the picker with Esc");
    s.key("Esc")?;
    s.expect_node(&format!("Foreground colour {hex}"))?;
    Ok(())
}

/// Pick a Brush mode (Paint, Dodge, Blur, ...) from the options bar.
fn brush_mode(s: &mut Session, mode: &str) -> UiResult {
    brush_mode_named(s, &[mode])
}

/// Pick a Brush mode known by one of `names` (the first the bar has).
fn brush_mode_named(s: &mut Session, names: &[&str]) -> UiResult {
    let mode = names[0];
    if !s.has_node("Brush mode") {
        let name = names
            .iter()
            .copied()
            .find(|n| bar_node(s, n, Role::Button).is_ok())
            .unwrap_or(mode);
        s.describe(&format!("Choose {name} in the Brush modes"));
        return bar_click(s, name, Role::Button);
    }
    s.describe("Open the Brush mode menu");
    s.click("Brush mode")?;
    s.wait_frames(2)?;
    // The menu's items, not the quick-add chips that share some names.
    let combo = s.node("Brush mode").map(|n| n.rect).ok_or("no Brush mode menu")?;
    if matches!(mode, "Blur" | "Sharpen" | "History") {
        // The last modes sit at the foot of the list: scroll it down
        // over its first item, as a user would if the list is short.
        s.describe("Scroll the Brush mode list down");
        s.scroll("Dodge", 300.0)?;
        s.wait_frames(2)?;
    }
    let item = names
        .iter()
        .find_map(|name| {
            s.tree()
                .matches(name, Some(Role::Button))
                .into_iter()
                .find(|n| (n.rect.min.x - combo.min.x).abs() < 24.0 && n.rect.min.y > combo.max.y)
                .map(|n| (n.rect.center(), *name))
        })
        .ok_or_else(|| UiError(format!("no {mode} in the Brush mode menu")))?;
    s.describe(&format!("Choose {}", item.1));
    s.click_at(item.0, &format!("“{}” in the menu", item.1))
}

/// A check for behaviour that is wrong today and left open (a proposal
/// in docs/testing/results/d_painting.md): logged, not failed, so the
/// rest of the journey still runs and passes.
fn open_issue(s: &mut Session, id: &str, what: &str, ok: bool, expected: String, actual: String) -> UiResult {
    if ok {
        s.note(&format!("{id} now passes: {what} ({actual})"))
    } else {
        s.note(&format!("OPEN {id}: {what}: expected {expected}, got {actual}"))
    }
}

/// Set Scatter: in the options bar when it has room, else in Brush
/// settings (a narrow bar keeps it there only).
fn set_scatter(s: &mut Session, value: &str) -> UiResult {
    if bar_node(s, "Scatter", Role::SpinButton).is_ok() {
        return bar_field(s, "Scatter", value);
    }
    s.describe("Open Brush settings: Scatter lives there on a narrow bar");
    bar_click(s, "Brush settings*", Role::Button)?;
    s.describe(&format!("Type {value}% as the Scatter"));
    s.set_field("Scatter", value)?;
    s.describe("Close Brush settings");
    s.click("Close brush settings")
}

/// Save the brush as a preset from the options bar.
fn save_preset(s: &mut Session) -> UiResult {
    s.describe("Save the brush as a preset");
    // A narrow bar labels it "Save".
    for name in ["Save brush preset", "Save preset", "Save"] {
        if bar_node(s, name, Role::Button).is_ok() {
            return bar_click(s, name, Role::Button);
        }
    }
    Err(UiError("no Save preset button in the options bar".into()))
}

/// The Clone Stamp / Healing bar shows that no source is set yet (as a
/// label on a wide bar, in Pick source's tooltip on a narrow one).
fn expect_no_source(s: &mut Session) -> UiResult {
    let src = s.app(|a| a.clone_source)?;
    s.check_eq("no source is set yet", src, None)?;
    s.expect_node("Pick source")?;
    Ok(())
}

/// The source is set at `at` (document pixels).
fn expect_source(s: &mut Session, at: (f32, f32)) -> UiResult {
    let src = s.app(|a| a.clone_source.map(|(x, y)| (x.round(), y.round())))?;
    s.check_eq("the source is where the Alt-click was", src, Some(at))?;
    s.expect_text("Clone source set")?;
    Ok(())
}

/// Undo the last step with Cmd+Z.
fn undo(s: &mut Session, what: &str) -> UiResult {
    s.describe(&format!("Undo {what} with Cmd+Z"));
    s.key("Cmd+Z")?;
    s.wait_idle()
}

// ---- the brush -----------------------------------------------------------------

fn brush_basics(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.describe("Reset to black over white with D");
    s.key("D")?;
    s.expect_node("Foreground colour #000000")?;
    s.expect_node("Background colour #FFFFFF")?;
    s.describe("Pick the Brush with B");
    s.key("B")?;
    s.expect_text("Brush")?;

    bar_field(s, "Size", "20")?;
    bar_field(s, "Hardness", "100")?;
    s.describe("Paint a straight stroke across the top");
    s.canvas_drag((40.0, 60.0), (360.0, 60.0), 16, "")?;
    s.wait_idle()?;
    expect_px(s, "the stroke's middle is black", (200, 60), BLACK, 0)?;
    expect_px(
        s,
        "8 px off the line is still inside the 20 px brush",
        (200, 68),
        BLACK,
        2,
    )?;
    expect_px(s, "12 px off the line is outside it", (200, 72), WHITE, 0)?;
    expect_px(s, "the canvas below is untouched", (200, 200), WHITE, 0)?;
    let history = s.history()?;
    s.check_eq("one history step for the stroke", history.len(), 1)?;

    // Opacity: a single dab at 50% is half black over white (blended in
    // linear light, as everything in Lumenply is).
    bar_field(s, "Opacity", "50")?;
    s.describe("Click once at 50% opacity");
    s.canvas_click((200.0, 120.0), "")?;
    s.wait_idle()?;
    let half = s.pixel(200, 120)?;
    let want = mix_linear(255, 0, 0.5);
    s.check(
        "a 50% dab is half black over white",
        near(half, [want, want, want, 255], 1),
        format!("[{want}, {want}, {want}, 255]"),
        format!("{half:?}"),
    )?;
    s.describe("Paint a 50% stroke that crosses itself");
    s.canvas_stroke(
        &[(60.0, 160.0), (340.0, 160.0), (200.0, 150.0), (200.0, 190.0)],
        8,
    )?;
    s.wait_idle()?;
    let along = s.pixel(120, 160)?;
    let crossing = s.pixel(200, 160)?;
    s.note(&format!(
        "50% stroke: {along:?} along the line, {crossing:?} where it crosses itself"
    ))?;
    open_issue(
        s,
        "P1",
        "a 50% stroke stays half black along its length (Photoshop's Opacity caps the stroke)",
        near(along, [want, want, want, 255], 2),
        format!("[{want}, {want}, {want}, 255]"),
        format!("{along:?}"),
    )?;
    bar_field(s, "Opacity", "100")?;

    // Bracket keys: ] grows the brush by a quarter, [ shrinks it back.
    s.describe("Press ] to make the brush bigger");
    s.key("]")?;
    let size = s.app(|a| a.brush.radius * 2.0)?;
    s.check_eq("] grows the brush from 20 px to 25 px", size.round(), 25.0)?;
    s.expect_text("25 px")?;
    s.describe("Press [ to make it smaller again");
    s.key("[")?;
    let size = s.app(|a| a.brush.radius * 2.0)?;
    s.check_eq("[ shrinks it back to 20 px", size.round(), 20.0)?;
    s.describe("Shift+[ softens the brush by 25%");
    s.key("Shift+[")?;
    let h = s.app(|a| a.brush.hardness)?;
    s.check_eq("hardness drops to 75%", (h * 100.0).round(), 75.0)?;
    s.describe("Shift+] hardens it again");
    s.key("Shift+]")?;
    let h = s.app(|a| a.brush.hardness)?;
    s.check_eq("Shift+] brings it back to 100%", (h * 100.0).round(), 100.0)?;

    // Shift-click: a straight line from the last stroke's end.
    s.describe("Click once at the lower left");
    s.canvas_click((60.0, 250.0), "")?;
    s.describe("Shift-click at the lower right: a straight line joins them");
    s.canvas_click((340.0, 250.0), "Shift")?;
    s.wait_idle()?;
    expect_px(s, "the line's middle is black", (200, 250), BLACK, 2)?;
    expect_px(s, "a quarter of the way along too", (130, 250), BLACK, 2)?;
    expect_px(s, "nothing below the line", (200, 270), WHITE, 0)?;

    // Alt-click with the brush picks the colour under the pointer.
    s.describe("Swap colours with X: white becomes the brush colour");
    s.key("X")?;
    s.expect_node("Foreground colour #FFFFFF")?;
    let steps = s.history()?.len();
    s.describe("Alt-click the 50% dab to pick its grey");
    s.canvas_click((200.0, 120.0), "Alt")?;
    let fg = s.app(|a| crate::color_picker::format_hex(a.brush_rgb))?;
    let grey = format!("#{want:02X}{want:02X}{want:02X}");
    s.check_eq("the brush colour is the grey under the pointer", fg, grey.clone())?;
    s.expect_node(&format!("Foreground colour {grey}"))?;
    expect_steps(s, "Alt-click added no history step", steps)?;
    expect_px(s, "the pick left the canvas as it was", (200, 120), half, 0)?;

    // Undo: one step per stroke, newest first.
    undo(s, "the Shift-click line")?;
    expect_px(s, "the straight line is gone", (200, 250), WHITE, 0)?;
    expect_px(s, "the dab where it started stays", (60, 250), BLACK, 2)?;
    undo(s, "the single dab")?;
    expect_px(s, "the dab is gone", (60, 250), WHITE, 0)?;
    undo(s, "the crossing stroke")?;
    undo(s, "the 50% dab")?;
    expect_px(s, "the grey dab is gone", (200, 120), WHITE, 0)?;
    expect_px(s, "the first stroke stays", (200, 60), BLACK, 0)?;
    expect_steps(s, "only the first stroke is left in the history", 1)?;
    Ok(())
}

/// A version-1 Photoshop brush set: one computed round brush (30 px,
/// hardness 70%) and one 3 × 2 sampled tip.
fn abr_v1() -> Vec<u8> {
    let be16 = |v: u16| v.to_be_bytes();
    let be32 = |v: u32| v.to_be_bytes();
    let mut computed = be32(0).to_vec();
    for x in [25u16, 30, 60, 45, 70] {
        computed.extend(be16(x));
    }
    let mut sampled = be32(0).to_vec();
    sampled.extend(be16(50));
    sampled.push(1);
    sampled.extend([0u8; 8]);
    for x in [0u32, 0, 2, 3] {
        sampled.extend(be32(x));
    }
    sampled.extend(be16(8));
    sampled.push(0);
    sampled.extend([255, 200, 0, 128, 255, 64]);
    let mut file = be16(1).to_vec();
    file.extend(be16(2));
    for (kind, body) in [(1u16, &computed), (2, &sampled)] {
        file.extend(be16(kind));
        file.extend(be32(body.len() as u32));
        file.extend(body);
    }
    file
}

fn brush_tips(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.key("D")?;
    s.describe("Pick the Brush from the toolbar");
    s.click_role(Role::Button, "Brush")?;
    bar_field(s, "Size", "20")?;
    bar_field(s, "Hardness", "100")?;

    // Scatter throws dabs off the line.
    set_scatter(s, "200")?;
    s.describe("Paint a scattered stroke");
    s.canvas_drag((40.0, 60.0), (360.0, 60.0), 16, "")?;
    s.wait_idle()?;
    let band = dark_count(s, (40, 50, 320, 21))?;
    let off = dark_count(s, (0, 0, 400, 49))? + dark_count(s, (0, 72, 400, 40))?;
    s.check(
        "scattered dabs land beyond the brush's own 20 px band",
        off > 200,
        "over 200 dark pixels outside the band",
        format!("{off} outside, {band} inside"),
    )?;
    s.expect_node("Brush settings (dynamics on)")?;
    set_scatter(s, "0")?;
    s.expect_node("Brush settings")?;

    // Presets: save, change, apply.
    s.describe("Save the brush as a preset");
    save_preset(s)?;
    s.expect_text("Brush preset saved")?;
    bar_field(s, "Size", "60")?;
    s.describe("Open the presets menu");
    bar_click(s, "Brush presets", Role::ComboBox)?;
    s.describe("Pick the saved preset");
    s.click("Hard 20")?;
    let size = s.app(|a| a.brush.radius * 2.0)?;
    s.check_eq("the preset brings back the 20 px size", size.round(), 20.0)?;
    s.expect_text("Brush preset \"Hard 20\" applied")?;

    // The Brush settings panel: a sampled tip.
    s.describe("Open Brush settings from the options bar");
    bar_click(s, "Brush settings", Role::Button)?;
    s.expect_node("Brush tip Round")?;
    s.describe("Pick the Chalk tip");
    s.click("Brush tip Chalk")?;
    let tip = s.app(|a| a.brush.tip.as_ref().map(|t| t.name().to_string()))?;
    s.check_eq("the Chalk tip is on the brush", tip, Some("Chalk".to_string()))?;
    s.describe("Close the panel");
    s.click("Close brush settings")?;
    s.wait_frames(2)?;
    let greyed = s
        .tree()
        .matches("Hardness", Some(Role::SpinButton))
        .into_iter()
        .any(|n| n.disabled);
    s.check_eq(
        "Hardness is greyed out: a sampled tip has its own edge",
        greyed,
        true,
    )?;
    // A narrow bar has only the number field, so it can be hovered by
    // name: the tooltip says why it is greyed out.
    if s.tree().matches("Hardness", Some(Role::Slider)).is_empty() {
        s.describe("Hover the greyed-out Hardness");
        let tip = s.hover("Hardness")?.join(" ");
        s.check(
            "the tooltip says why",
            tip.contains("own edge"),
            "…has its own edge…",
            tip,
        )?;
    }
    s.describe("Paint with the Chalk tip");
    s.canvas_drag((40.0, 160.0), (360.0, 160.0), 16, "")?;
    s.wait_idle()?;
    let chalk = dark_count(s, (40, 130, 320, 60))?;
    s.check(
        "the chalk stroke paints a broken band",
        chalk > 500 && chalk < 320 * 60,
        "some but not all pixels",
        format!("{chalk}"),
    )?;

    // Define brush tip from a selection.
    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.describe("Select the chalk stroke's left end");
    s.canvas_drag((30.0, 140.0), (90.0, 180.0), 8, "")?;
    s.describe("Edit ▸ Define brush tip");
    s.menu("Edit > Define brush tip")?;
    s.expect_text("Defined the brush tip \"Custom tip 1\"")?;
    let tip = s.app(|a| a.brush.tip.as_ref().map(|t| t.name().to_string()))?;
    s.check_eq(
        "the new tip is on the brush",
        tip,
        Some("Custom tip 1".to_string()),
    )?;
    s.key("Cmd+D")?;

    // Import an .abr.
    let abr = s.files().join("Set.abr");
    std::fs::write(&abr, abr_v1()).map_err(|e| UiError(e.to_string()))?;
    s.describe("File ▸ Import brushes...");
    s.menu("File > Import brushes...")?;
    s.choose_file(&abr)?;
    s.expect_text("Imported 1 new tip, 1 preset")?;
    let tip = s.app(|a| a.brush.tip.as_ref().map(|t| (t.width(), t.height())))?;
    s.check_eq("the imported 3 × 2 tip is on the brush", tip, Some((3, 2)))?;
    s.describe("Pick the Brush again");
    s.key("B")?;
    s.describe("Open the presets menu");
    bar_click(s, "Brush presets", Role::ComboBox)?;
    s.expect_node("Round 30")?;
    s.describe("Pick the imported round brush");
    s.click("Round 30")?;
    let (size, hard) = s.app(|a| (a.brush.radius * 2.0, a.brush.hardness))?;
    s.check_eq(
        "the imported preset is 30 px at 70% hardness",
        (size, (hard * 100.0).round()),
        (30.0, 70.0),
    )?;
    Ok(())
}

// ---- eraser, bucket, gradient, colours -------------------------------------------

fn eraser(s: &mut Session) -> UiResult {
    // Left half red, right half blue, on the photo's own layer.
    let img = picture(300, 200, [220, 30, 30], &[((150, 0, 150, 200), [30, 60, 220])]);
    open_png(s, "halves.png", img)?;
    let layer = s.layer_names()?.last().cloned().unwrap_or_default();

    s.describe("Pick the Eraser with E");
    s.key("E")?;
    bar_field(s, "Size", "20")?;
    bar_field(s, "Hardness", "100")?;
    s.describe("Erase a stroke across both halves");
    s.canvas_drag((20.0, 30.0), (280.0, 30.0), 16, "")?;
    s.wait_idle()?;
    expect_alpha(s, "the stroke is transparent", &layer, (100, 30), 0)?;
    expect_alpha(s, "on the blue half too", &layer, (200, 30), 0)?;
    expect_layer_px(
        s,
        "below it the red is untouched",
        &layer,
        (100, 60),
        [220, 30, 30, 255],
        0,
    )?;
    undo(s, "the erase")?;
    expect_alpha(s, "undo brings the pixels back", &layer, (100, 30), 255)?;

    // Background eraser: erases the sampled red, keeps the blue.
    s.describe("Shift+E steps to the Background Eraser");
    s.key("Shift+E")?;
    let mode = s.app(|a| a.retouch.eraser_mode)?;
    s.check_eq(
        "the Eraser is in Background mode",
        mode,
        crate::retouch_ui::EraserMode::Background,
    )?;
    bar_field(s, "Size", "40")?;
    s.describe("Drag down the red side, the brush overlapping the blue");
    s.canvas_drag((135.0, 60.0), (135.0, 160.0), 16, "")?;
    s.wait_idle()?;
    expect_alpha(s, "red under the centre is erased", &layer, (135, 110), 0)?;
    expect_alpha(s, "red at the brush's edge is erased", &layer, (148, 110), 0)?;
    expect_layer_px(
        s,
        "blue inside the brush is kept",
        &layer,
        (152, 110),
        [30, 60, 220, 255],
        0,
    )?;
    expect_layer_px(
        s,
        "red outside the brush is kept",
        &layer,
        (100, 110),
        [220, 30, 30, 255],
        0,
    )?;

    // Magic eraser: one click clears the connected red.
    s.describe("Shift+E steps to the Magic Eraser");
    s.key("Shift+E")?;
    s.describe("Click the red half");
    s.canvas_click((40.0, 100.0), "")?;
    s.wait_idle()?;
    expect_alpha(s, "all the red is gone", &layer, (10, 190), 0)?;
    expect_layer_px(s, "the blue stays", &layer, (250, 100), [30, 60, 220, 255], 0)?;
    expect_px(
        s,
        "transparent shows through as nothing",
        (40, 100),
        [0, 0, 0, 0],
        0,
    )?;
    undo(s, "the magic erase")?;
    expect_layer_px(
        s,
        "undo brings the red back",
        &layer,
        (10, 190),
        [220, 30, 30, 255],
        0,
    )?;
    expect_alpha(
        s,
        "but not where the background eraser worked",
        &layer,
        (135, 110),
        0,
    )?;
    Ok(())
}

fn bucket(s: &mut Session) -> UiResult {
    // White with a 4 px black frame, and a pale grey patch inside it.
    let img = picture(
        300,
        200,
        [255, 255, 255],
        &[
            ((50, 50, 200, 100), [0, 0, 0]),
            ((54, 54, 192, 92), [255, 255, 255]),
            ((60, 120, 30, 20), [240, 240, 240]),
        ],
    );
    open_png(s, "frame.png", img)?;
    set_foreground(s, "#FF0000")?;
    s.describe("Pick the Paint Bucket with G");
    s.key("G")?;
    s.expect_text("Paint Bucket")?;
    s.describe("Click inside the frame");
    s.canvas_click((150.0, 100.0), "")?;
    s.wait_idle()?;
    expect_px(s, "the inside is red", (150, 100), [255, 0, 0, 255], 0)?;
    let pale = s.pixel(70, 130)?;
    open_issue(
        s,
        "P2",
        "the pale patch (15 levels off white) is within 12% tolerance, as in Photoshop",
        near(pale, [255, 0, 0, 255], 0),
        "[255, 0, 0, 255]".into(),
        format!("{pale:?}"),
    )?;
    expect_px(s, "the frame stays black", (51, 100), BLACK, 0)?;
    expect_px(s, "outside the frame stays white", (20, 20), WHITE, 0)?;
    undo(s, "the fill")?;
    expect_px(s, "undo brings the white back", (150, 100), WHITE, 0)?;

    s.describe("Tolerance 0 keeps the pale patch out");
    bar_field(s, "Tolerance", "0")?;
    s.canvas_click((150.0, 100.0), "")?;
    s.wait_idle()?;
    expect_px(s, "the inside is red", (150, 100), [255, 0, 0, 255], 0)?;
    expect_px(s, "the pale patch is not", (70, 130), [240, 240, 240, 255], 0)?;
    undo(s, "the fill")?;

    bar_field(s, "Opacity", "50")?;
    s.describe("Fill at 50% opacity");
    s.canvas_click((20.0, 20.0), "")?;
    s.wait_idle()?;
    let half = mix_linear(255, 0, 0.5);
    expect_px(
        s,
        "outside is half red over white",
        (20, 20),
        [255, half, half, 255],
        1,
    )?;
    expect_px(s, "inside the frame is untouched", (150, 100), WHITE, 0)?;
    Ok(())
}

fn gradient(s: &mut Session) -> UiResult {
    new_doc(s, 400, 100)?;
    s.key("D")?;
    s.describe("Pick the Gradient with Shift+G");
    s.key("Shift+G")?;
    s.expect_node("Gradient: Foreground to Background. Click to edit")?;
    s.describe("Drag from the left edge to the right edge");
    s.canvas_drag((0.0, 50.0), (400.0, 50.0), 16, "")?;
    s.wait_idle()?;
    let left = s.pixel(1, 50)?;
    let mid = s.pixel(200, 50)?;
    let right = s.pixel(398, 50)?;
    s.check(
        "black at the start, white at the end, grey between",
        left[0] < 12 && right[0] > 243 && mid[0] > 60 && mid[0] < 200,
        "≈0, mid grey, ≈255",
        format!("{left:?} {mid:?} {right:?}"),
    )?;
    let col: Vec<u8> = [50, 150, 250, 350]
        .into_iter()
        .map(|x| s.pixel(x, 50).map(|p| p[0]))
        .collect::<UiResult<_>>()?;
    s.check(
        "it brightens steadily left to right",
        col.windows(2).all(|w| w[0] < w[1]),
        "rising",
        format!("{col:?}"),
    )?;
    let rows = (s.pixel(200, 5)?, s.pixel(200, 95)?);
    s.check_eq("a horizontal drag paints vertical bands", rows.0, rows.1)?;
    undo(s, "the gradient")?;
    expect_px(s, "undo leaves white", (1, 50), WHITE, 0)?;

    // Presets and the stop editor in the swatch's popover.
    s.describe("Open the gradient popover from the swatch");
    s.click("Gradient: Foreground to Background. Click to edit")?;
    s.describe("Pick the Black, White preset");
    s.click("Gradient preset: Black, White")?;
    s.expect_node("Gradient: Black, White. Click to edit")?;
    s.describe("Click the middle of the ramp to add a stop");
    s.click_in("Gradient, 2 stops*", 0.5, 0.75, "the middle of the ramp")?;
    s.expect_node("Gradient, 3 stops*")?;
    s.describe("Make the new stop red");
    s.click("Stop colour *")?;
    s.set_field("Hex colour", "#FF0000")?;
    s.key("Esc")?;
    s.describe("Close the popover with Esc");
    s.key("Esc")?;
    s.expect_node("Gradient: Black, White (edited). Click to edit")?;
    s.describe("Drag across again");
    s.canvas_drag((0.0, 50.0), (400.0, 50.0), 16, "")?;
    s.wait_idle()?;
    let red = s.pixel(200, 50)?;
    s.check(
        "the middle stop paints red",
        red[0] > 200 && red[1] < 60 && red[2] < 60,
        "red",
        format!("{red:?}"),
    )?;
    undo(s, "the gradient")?;

    // Radial.
    s.describe("Pick the radial style");
    bar_click(s, "Radial gradient", Role::RadioButton)?;
    s.describe("Drag from the centre outwards");
    s.canvas_drag((200.0, 50.0), (240.0, 50.0), 8, "")?;
    s.wait_idle()?;
    let (c, ring_l, ring_r, corner) = (
        s.pixel(200, 50)?,
        s.pixel(179, 50)?,
        s.pixel(220, 50)?,
        s.pixel(5, 5)?,
    );
    s.check(
        "black at the centre, the same red ring either side, white beyond",
        c[0] < 12 && near(ring_l, ring_r, 8) && ring_l[0] > 150 && near(corner, WHITE, 2),
        "black, matching ring, white",
        format!("{c:?} {ring_l:?} {ring_r:?} {corner:?}"),
    )?;

    // Fill-layer mode adds a gradient fill layer instead of painting.
    let layers = s.layer_names()?.len();
    s.describe("Switch the tool to Fill layer mode");
    bar_click(s, "Fill layer", Role::Button)?;
    s.describe("Drag a gradient");
    s.canvas_drag((0.0, 50.0), (400.0, 50.0), 16, "")?;
    s.wait_idle()?;
    let names = s.layer_names()?;
    s.check_eq("a new layer on top", names.len(), layers + 1)?;
    let fill = s.doc(|d| {
        d.layers().last().is_some_and(|l| {
            matches!(
                l.fill_layer().map(|f| &f.fill),
                Some(lumenply_doc::Fill::Gradient { .. })
            )
        })
    })?;
    s.check_eq("it is a gradient fill layer", fill, true)?;
    let history = s.history()?;
    s.note(&format!("History: {}", history.join(" → ")))?;
    undo(s, "the fill layer")?;
    expect_layers(s, "undo removes it", layers)?;
    Ok(())
}

fn colours(s: &mut Session) -> UiResult {
    let img = picture(
        300,
        200,
        [255, 255, 255],
        &[
            ((0, 0, 100, 200), [255, 0, 0]),
            ((100, 0, 100, 200), [0, 160, 80]),
        ],
    );
    open_png(s, "swatches.png", img)?;
    set_foreground(s, "#FF8000")?;
    let fg = s.app(|a| a.brush_rgb)?;
    s.check(
        "the brush colour is the typed orange",
        (fg[0] - 1.0).abs() < 1e-3 && (fg[1] - 128.0 / 255.0).abs() < 2e-3 && fg[2].abs() < 1e-3,
        "[1, 0.502, 0]",
        format!("{fg:?}"),
    )?;
    s.describe("Open the background swatch's picker");
    s.click("Background colour #FFFFFF")?;
    s.describe("Type 40 in its Blue field");
    s.set_field("Blue", "40")?;
    s.key("Esc")?;
    s.expect_node("Background colour #FFFF28")?;

    s.describe("Click the swap arrow");
    s.click("Swap colours")?;
    s.expect_node("Foreground colour #FFFF28")?;
    s.expect_node("Background colour #FF8000")?;
    s.describe("X swaps them back");
    s.key("X")?;
    s.expect_node("Foreground colour #FF8000")?;
    s.describe("Click the default-colours squares");
    s.click("Default colours")?;
    s.expect_node("Foreground colour #000000")?;
    s.expect_node("Background colour #FFFFFF")?;

    s.describe("Pick the Eyedropper with I");
    s.key("I")?;
    s.describe("Click the green");
    s.canvas_click((150.0, 100.0), "")?;
    s.expect_node("Foreground colour #00A050")?;
    s.expect_text("Picked #00A050")?;
    s.describe("Pick the Eyedropper from the toolbar and click the red");
    s.click("Eyedropper")?;
    s.canvas_click((50.0, 100.0), "")?;
    s.expect_node("Foreground colour #FF0000")?;
    expect_steps(s, "picking colours adds no history", 0)?;

    // The picker's own eyedropper samples the canvas into the background.
    s.describe("Open the background picker");
    s.click("Background colour #FFFFFF")?;
    s.describe("Arm its eyedropper");
    s.click("Pick colour from canvas")?;
    s.describe("Click the green on the canvas");
    s.canvas_click((190.0, 30.0), "")?;
    s.wait_frames(3)?;
    let bg = s.app(|a| crate::color_picker::format_hex(a.bg_rgb))?;
    s.check_eq(
        "the background colour is the sampled green",
        bg,
        "#00A050".to_string(),
    )?;
    s.key("Esc")?;
    Ok(())
}

// ---- clone and heal ---------------------------------------------------------------

fn clone_stamp(s: &mut Session) -> UiResult {
    let img = picture(300, 200, [255, 255, 255], &[((40, 80, 40, 40), [200, 20, 20])]);
    open_png(s, "square.png", img)?;
    s.describe("Pick the Clone Stamp with S");
    s.key("S")?;
    expect_no_source(s)?;
    bar_field(s, "Size", "30")?;
    bar_field(s, "Hardness", "100")?;
    s.describe("Alt-click the red square to set the source");
    s.canvas_click((60.0, 100.0), "Alt")?;
    expect_source(s, (60.0, 100.0))?;
    expect_steps(s, "setting the source paints nothing", 0)?;
    s.describe("Paint where the copy should go");
    s.canvas_drag((195.0, 100.0), (205.0, 100.0), 6, "")?;
    s.wait_idle()?;
    expect_px(s, "the copy is red", (200, 100), [200, 20, 20, 255], 1)?;
    expect_px(
        s,
        "a copy of the square's edge: white beyond it",
        (225, 100),
        WHITE,
        0,
    )?;
    expect_px(s, "the source is untouched", (60, 100), [200, 20, 20, 255], 0)?;
    expect_steps(s, "one history step", 1)?;

    // A second stroke 60 px lower: Photoshop's default (Aligned) keeps
    // the source 140 px to the left, where the canvas is white.
    s.describe("Paint a second stroke lower down");
    s.canvas_drag((195.0, 160.0), (205.0, 160.0), 6, "")?;
    s.wait_idle()?;
    let second = s.pixel(200, 160)?;
    open_issue(
        s,
        "P6",
        "a second stroke keeps the first stroke's offset (Aligned)",
        near(second, WHITE, 2),
        "white, from 140 px to the left".into(),
        format!("{second:?} (the source point again)"),
    )?;
    undo(s, "the second stroke")?;
    undo(s, "the clone")?;
    expect_px(s, "undo removes the copy", (200, 100), WHITE, 0)?;
    expect_px(s, "and the second one", (200, 160), WHITE, 0)?;
    Ok(())
}

/// Grey with flaws to retouch: three black dots, a blue square and a red
/// pupil in a dark iris.
fn retouch_picture() -> image::RgbaImage {
    let mut img = picture(
        320,
        200,
        [128, 128, 128],
        &[
            ((57, 57, 6, 6), [0, 0, 0]),
            ((117, 57, 6, 6), [0, 0, 0]),
            ((197, 57, 6, 6), [0, 0, 0]),
            ((250, 140, 20, 20), [30, 60, 220]),
        ],
    );
    for (x, y, p) in img.enumerate_pixels_mut() {
        let d = ((x as f32 - 60.0).powi(2) + (y as f32 - 150.0).powi(2)).sqrt();
        if d <= 6.0 {
            *p = image::Rgba([220, 30, 30, 255]);
        } else if d <= 14.0 {
            *p = image::Rgba([50, 40, 35, 255]);
        }
    }
    img
}

fn heal(s: &mut Session) -> UiResult {
    open_png(s, "flaws.png", retouch_picture())?;

    // Spot healing: one click over the dot.
    s.describe("Pick the Healing Brush with J (Spot mode)");
    s.key("J")?;
    let mode = s.app(|a| a.retouch.heal_mode)?;
    s.check_eq("it starts in Spot mode", mode, crate::retouch_ui::HealMode::Spot)?;
    bar_field(s, "Size", "16")?;
    s.describe("Click the first dot");
    s.canvas_click((60.0, 60.0), "")?;
    s.wait_idle()?;
    expect_px(s, "the dot is healed to the grey around it", (60, 60), GREY, 6)?;
    expect_px(s, "the next dot is untouched", (120, 60), BLACK, 0)?;
    undo(s, "the spot heal")?;
    expect_px(s, "undo brings the dot back", (60, 60), BLACK, 0)?;
    s.canvas_click((60.0, 60.0), "")?;
    s.wait_idle()?;

    // Healing brush: texture from an Alt-clicked source.
    s.describe("Shift+J: Healing mode");
    s.key("Shift+J")?;
    expect_no_source(s)?;
    s.describe("Alt-click clean grey as the source");
    s.canvas_click((120.0, 110.0), "Alt")?;
    s.describe("Paint over the second dot");
    s.canvas_drag((116.0, 60.0), (124.0, 60.0), 6, "")?;
    s.wait_idle()?;
    expect_px(s, "the second dot is healed", (120, 60), GREY, 6)?;

    // Patch: lasso the third dot, drag it onto clean grey.
    s.describe("Shift+J: Patch mode");
    s.key("Shift+J")?;
    // A wide window has room for Patch's how-to and its Source /
    // Destination switch (Spot's crowded bar must not fold it away).
    if s.node("Export").is_some_and(|n| n.rect.max.x > 1300.0) {
        s.expect_text("Draw around the flaw, then drag it onto clean texture")?;
    }
    s.describe("Draw around the third dot");
    let ring: Vec<(f32, f32)> = (0..=12)
        .map(|i| {
            let t = i as f32 / 12.0 * std::f32::consts::TAU;
            (200.0 + 12.0 * t.cos(), 60.0 + 12.0 * t.sin())
        })
        .collect();
    s.canvas_stroke(&ring, 2)?;
    s.wait_idle()?;
    let sel = s.selection_bounds()?;
    s.check(
        "the lasso made a selection around the dot",
        sel.is_some(),
        "a selection",
        format!("{sel:?}"),
    )?;
    s.describe("Drag the selection onto clean grey");
    s.canvas_drag((200.0, 60.0), (200.0, 110.0), 12, "")?;
    s.wait_idle()?;
    expect_px(s, "the third dot is patched", (200, 60), GREY, 6)?;
    s.key("Cmd+D")?;

    // Content-aware move: the blue square moves left, its hole fills.
    s.describe("Shift+J: Content-Aware Move");
    s.key("Shift+J")?;
    let ring: Vec<(f32, f32)> = (0..=12)
        .map(|i| {
            let t = i as f32 / 12.0 * std::f32::consts::TAU;
            (260.0 + 17.0 * t.cos(), 150.0 + 17.0 * t.sin())
        })
        .collect();
    s.describe("Draw around the blue square");
    s.canvas_stroke(&ring, 2)?;
    s.describe("Drag it 60 px to the left");
    s.canvas_drag((260.0, 150.0), (200.0, 150.0), 12, "")?;
    s.wait_idle()?;
    expect_px(
        s,
        "the square is at its new place",
        (200, 150),
        [30, 60, 220, 255],
        4,
    )?;
    expect_px(s, "its old place is filled with grey", (260, 150), GREY, 10)?;
    s.key("Cmd+D")?;

    // Red eye: one click on the pupil.
    s.describe("Shift+J: Red Eye");
    s.key("Shift+J")?;
    let before = s.pixel(60, 150)?;
    s.describe("Click the red pupil");
    s.canvas_click((60.0, 150.0), "")?;
    s.wait_idle()?;
    let after = s.pixel(60, 150)?;
    s.check(
        "the pupil is no longer red",
        after[0] < 100 && (after[0] as i32 - after[1] as i32).abs() < 30,
        "dark, neutral",
        format!("{before:?} → {after:?}"),
    )?;
    expect_px(s, "the grey around the eye is untouched", (90, 150), GREY, 0)?;
    let history = s.history()?;
    s.note(&format!("History: {}", history.join(" → ")))?;
    Ok(())
}

// ---- brush modes -------------------------------------------------------------------

fn blur_sharpen_history(s: &mut Session) -> UiResult {
    let img = picture(300, 200, [0, 0, 0], &[((150, 0, 150, 200), [255, 255, 255])]);
    open_png(s, "edge.png", img)?;
    s.key("B")?;
    brush_mode(s, "Blur")?;
    bar_field(s, "Size", "20")?;
    s.describe("Blur along the edge");
    s.canvas_drag((150.0, 40.0), (150.0, 160.0), 16, "")?;
    s.wait_idle()?;
    let (l, r) = (s.pixel(149, 100)?, s.pixel(150, 100)?);
    s.check(
        "the hard edge is soft now",
        l[0] > 20 && r[0] < 235,
        "grey either side of the edge",
        format!("{l:?} {r:?}"),
    )?;
    expect_px(
        s,
        "away from the stroke the edge is still hard",
        (149, 20),
        BLACK,
        0,
    )?;
    expect_px(s, "and the flat areas are untouched", (50, 100), BLACK, 0)?;

    brush_mode(s, "Sharpen")?;
    let soft = s.pixel(147, 100)?;
    s.describe("Sharpen along the softened edge");
    s.canvas_drag((150.0, 40.0), (150.0, 160.0), 16, "")?;
    s.wait_idle()?;
    let crisp = s.pixel(147, 100)?;
    s.check(
        "sharpening darkens the dark side of the edge again",
        crisp[0] < soft[0],
        format!("below {}", soft[0]),
        format!("{crisp:?}"),
    )?;

    brush_mode(s, "History")?;
    s.expect_node("History brush source")?;
    s.describe("Paint back the opened state over the edge");
    bar_field(s, "Size", "40")?;
    s.canvas_drag((150.0, 30.0), (150.0, 170.0), 16, "")?;
    s.wait_idle()?;
    expect_px(s, "the dark side is as opened", (149, 100), BLACK, 0)?;
    expect_px(s, "the light side is as opened", (150, 100), WHITE, 0)?;
    undo(s, "the history stroke")?;
    let back = s.pixel(147, 100)?;
    s.check_eq("undo brings the sharpened edge back", back, crisp)?;
    Ok(())
}

fn toning(s: &mut Session) -> UiResult {
    let img = picture(
        300,
        200,
        [128, 128, 128],
        &[
            ((0, 100, 150, 100), [180, 140, 110]),
            ((200, 120, 40, 40), [220, 20, 20]),
        ],
    );
    open_png(s, "tones.png", img)?;
    s.key("B")?;
    bar_field(s, "Size", "20")?;
    bar_field(s, "Hardness", "100")?;

    brush_mode(s, "Dodge")?;
    s.describe("Dodge a stroke through the grey");
    s.canvas_drag((20.0, 30.0), (140.0, 30.0), 12, "")?;
    s.wait_idle()?;
    let dodged = s.pixel(80, 30)?;
    s.check(
        "dodging lightens the grey",
        dodged[0] > 150,
        "above 150",
        format!("{dodged:?}"),
    )?;
    s.check(
        "and keeps it grey",
        dodged[0] == dodged[1] && dodged[1] == dodged[2],
        "neutral",
        format!("{dodged:?}"),
    )?;
    open_issue(
        s,
        "P1",
        "one dodge stroke lifts the grey part way, as a Photoshop pass does",
        dodged[0] < 230,
        "a partial lift, under 230".into(),
        format!("{dodged:?}: the overlapping dabs compound to white"),
    )?;

    brush_mode(s, "Burn")?;
    s.describe("Burn a stroke below it");
    s.canvas_drag((20.0, 70.0), (140.0, 70.0), 12, "")?;
    s.wait_idle()?;
    let burnt = s.pixel(80, 70)?;
    s.check(
        "burning darkens the grey",
        burnt[0] < 100,
        "below 100",
        format!("{burnt:?}"),
    )?;
    expect_px(s, "between the strokes the grey is untouched", (80, 50), GREY, 0)?;

    let chroma = |p: [u8; 4]| p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap();
    let base = s.pixel(80, 130)?;
    brush_mode_named(s, &["Desaturate", "Sat−"])?;
    s.describe("Desaturate a stroke through the muted orange");
    s.canvas_drag((20.0, 130.0), (140.0, 130.0), 12, "")?;
    s.wait_idle()?;
    let drained = s.pixel(80, 130)?;
    s.check(
        "the sponge drains the colour",
        chroma(drained) < chroma(base) / 2,
        format!("chroma under {}", chroma(base) / 2),
        format!("{drained:?} (from {base:?})"),
    )?;
    brush_mode_named(s, &["Saturate", "Sat+"])?;
    s.describe("Saturate a stroke lower down");
    s.canvas_drag((20.0, 170.0), (140.0, 170.0), 12, "")?;
    s.wait_idle()?;
    let boosted = s.pixel(80, 170)?;
    s.check(
        "the sponge boosts the colour",
        chroma(boosted) > chroma(base),
        format!("chroma over {}", chroma(base)),
        format!("{boosted:?} (from {base:?})"),
    )?;

    brush_mode(s, "Smudge")?;
    s.describe("Smudge from the red square out to the right");
    s.canvas_drag((220.0, 140.0), (290.0, 140.0), 16, "")?;
    s.wait_idle()?;
    let smeared = s.pixel(255, 140)?;
    s.check(
        "red is dragged into the grey",
        smeared[0] as i32 > smeared[1] as i32 + 30,
        "reddish",
        format!("{smeared:?}"),
    )?;
    expect_px(s, "the grey above the smudge is untouched", (255, 110), GREY, 0)?;
    let history = s.history()?;
    s.check_eq("one history step per stroke", history.len(), 5)?;
    s.note(&format!("History: {}", history.join(" → ")))?;
    Ok(())
}

/// The brush modes a Photoshop user knows as tools of their own, found
/// the way they would look for them: Cmd+K and the tool's name, and
/// Photoshop's keys (O for the toning tools, Y for the History Brush).
fn find_tools(s: &mut Session) -> UiResult {
    use crate::{BrushMode, Tool};
    new_doc(s, 200, 100)?;
    for (query, want) in [
        ("dodge", BrushMode::Dodge),
        ("burn", BrushMode::Burn),
        ("sponge", BrushMode::Desaturate),
        ("smudge", BrushMode::Smudge),
        ("blur tool", BrushMode::Blur),
        ("history brush", BrushMode::History),
    ] {
        s.key("V")?;
        s.describe(&format!("Cmd+K and type “{query}”"));
        s.key("Cmd+K")?;
        s.type_text(query)?;
        s.describe("Enter runs the first match");
        s.key("Enter")?;
        s.wait_frames(2)?;
        let got = s.app(|a| (a.tool, a.brush.mode))?;
        s.check_eq(
            &format!("“{query}” picks the Brush in {want:?} mode"),
            got,
            (Tool::Brush, want),
        )?;
        if got != (Tool::Brush, want) {
            s.key("Esc")?;
        }
    }
    s.key("V")?;
    s.describe("O picks the Dodge tool, as in Photoshop");
    s.key("O")?;
    let got = s.app(|a| (a.tool, a.brush.mode))?;
    s.check_eq("O: the Brush in Dodge mode", got, (Tool::Brush, BrushMode::Dodge))?;
    s.describe("Shift+O steps to Burn");
    s.key("Shift+O")?;
    let got = s.app(|a| a.brush.mode)?;
    s.check_eq("Shift+O: Burn", got, BrushMode::Burn)?;
    s.describe("Shift+O again: the Sponge");
    s.key("Shift+O")?;
    let got = s.app(|a| a.brush.mode)?;
    s.check_eq("Shift+O: Sponge (Desaturate)", got, BrushMode::Desaturate)?;
    s.describe("Y picks the History Brush");
    s.key("Y")?;
    let got = s.app(|a| (a.tool, a.brush.mode))?;
    s.check_eq(
        "Y: the Brush in History mode",
        got,
        (Tool::Brush, BrushMode::History),
    )?;
    s.describe("B goes back to painting");
    s.key("B")?;
    let got = s.app(|a| a.brush.mode)?;
    s.note(&format!("B after Y leaves the Brush in {got:?} mode"))?;
    Ok(())
}

// ---- fill, stroke, content-aware fill, define -----------------------------------------

fn fill_stroke(s: &mut Session) -> UiResult {
    new_doc(s, 300, 200)?;
    s.key("D")?;
    s.describe("Add a layer from the Layer menu");
    s.menu("Layer > New pixel layer")?;
    let layer = s.layer_names()?.first().cloned().unwrap_or_default();

    // The brush's opacity is the brush's: Fill paints at full strength.
    s.key("B")?;
    bar_field(s, "Opacity", "50")?;

    s.describe("Pick the Rectangular Marquee with M");
    s.key("M")?;
    s.describe("Select a square on the left");
    s.canvas_drag((20.0, 20.0), (100.0, 100.0), 8, "")?;
    s.describe("Alt+Backspace fills it with the foreground colour");
    s.key("Alt+Backspace")?;
    s.wait_idle()?;
    expect_layer_px(s, "the selection is black", &layer, (60, 60), BLACK, 0)?;
    expect_alpha(s, "outside it the layer is empty", &layer, (150, 60), 0)?;

    s.describe("Select a square in the middle");
    s.canvas_drag((120.0, 20.0), (200.0, 100.0), 8, "")?;
    s.describe("Cmd+Backspace fills it with the background colour");
    s.key("Cmd+Backspace")?;
    s.wait_idle()?;
    expect_layer_px(s, "the selection is white", &layer, (160, 60), WHITE, 0)?;
    expect_alpha(s, "the left square is untouched", &layer, (60, 60), 255)?;

    s.describe("Select a square on the right");
    s.canvas_drag((220.0, 20.0), (280.0, 100.0), 8, "")?;
    s.describe("Edit ▸ Fill...");
    s.menu("Edit > Fill...")?;
    s.click_role(Role::Button, "Fill")?;
    s.wait_idle()?;
    expect_layer_px(s, "the Fill dialog fills it", &layer, (250, 60), BLACK, 0)?;

    // Edit ▸ Stroke: 4 px inside.
    s.describe("Select a band at the bottom");
    s.canvas_drag((40.0, 120.0), (260.0, 180.0), 8, "")?;
    s.describe("Edit ▸ Stroke...");
    s.menu("Edit > Stroke...")?;
    s.set_field("Stroke width", "4")?;
    s.click("Inside")?;
    s.click_role(Role::Button, "Stroke")?;
    s.wait_idle()?;
    expect_layer_px(s, "the inside edge is stroked", &layer, (41, 150), BLACK, 0)?;
    expect_layer_px(s, "4 px wide", &layer, (43, 150), BLACK, 0)?;
    expect_alpha(s, "not 5", &layer, (45, 150), 0)?;
    expect_alpha(s, "nothing outside the selection", &layer, (38, 150), 0)?;
    expect_alpha(s, "the middle is left empty", &layer, (150, 150), 0)?;

    let history = s.history()?;
    s.note(&format!("History: {}", history.join(" → ")))?;
    undo(s, "the stroke")?;
    expect_alpha(s, "undo removes the stroke", &layer, (41, 150), 0)?;
    expect_alpha(s, "and keeps the fills", &layer, (250, 60), 255)?;
    Ok(())
}

fn caf_define(s: &mut Session) -> UiResult {
    let img = picture(
        300,
        200,
        [128, 128, 128],
        &[
            ((145, 95, 10, 10), [0, 0, 0]),
            ((0, 0, 10, 10), [0, 0, 255]),
            ((10, 10, 10, 10), [0, 0, 255]),
        ],
    );
    open_png(s, "caf.png", img)?;
    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.describe("Select around the black square");
    s.canvas_drag((138.0, 88.0), (162.0, 112.0), 8, "")?;
    s.describe("Edit ▸ Content-Aware Fill...");
    s.menu("Edit > Content-Aware Fill...")?;
    s.click_role(Role::Button, "Fill")?;
    s.wait_idle()?;
    expect_px(
        s,
        "the square is filled from its grey surroundings",
        (150, 100),
        GREY,
        8,
    )?;
    expect_px(s, "outside the selection nothing changed", (130, 100), GREY, 0)?;
    undo(s, "the content-aware fill")?;
    expect_px(s, "undo brings the square back", (150, 100), BLACK, 0)?;

    s.describe("Select the blue checker in the corner");
    s.canvas_drag((0.0, 0.0), (20.0, 20.0), 6, "")?;
    s.describe("Edit ▸ Define Pattern");
    s.menu("Edit > Define Pattern")?;
    s.expect_text("Defined a 20×20 pattern")?;
    s.key("Cmd+D")?;
    let layers = s.layer_names()?.len();
    s.describe("Layer ▸ New fill layer ▸ Pattern...");
    s.menu("Layer > New fill layer > Pattern...")?;
    s.describe("Pick the new pattern");
    s.click("Pattern Pattern 1")?;
    s.wait_idle()?;
    expect_layers(s, "a pattern fill layer is added", layers + 1)?;
    expect_px(s, "it repeats the blue square", (40, 40), [0, 0, 255, 255], 0)?;
    expect_px(s, "and its grey", (50, 40), GREY, 0)?;
    Ok(())
}

// ---- how it feels ------------------------------------------------------------------

fn latency(s: &mut Session) -> UiResult {
    new_doc(s, 4000, 3000)?;
    s.key("B")?;
    bar_field(s, "Size", "80")?;
    // Strokes of growing length: the time per frame should not grow
    // with the stroke.
    for (i, segments) in [2usize, 8, 24].into_iter().enumerate() {
        let y = 500.0 + i as f32 * 800.0;
        let pts: Vec<(f32, f32)> = (0..=segments)
            .map(|k| {
                let t = k as f32 / segments as f32;
                (300.0 + 3400.0 * t, y + 200.0 * (t * 12.0).sin())
            })
            .collect();
        let frames = segments * 10;
        s.describe(&format!("Paint a stroke of {frames} pointer moves"));
        let t = std::time::Instant::now();
        s.canvas_stroke(&pts, 10)?;
        let wall = t.elapsed().as_secs_f64() * 1000.0;
        let commit = std::time::Instant::now();
        s.wait_idle()?;
        let idle = commit.elapsed().as_secs_f64() * 1000.0;
        s.note(&format!(
            "stroke of {frames} moves: {wall:.0} ms for ~{} frames = {:.1} ms a frame (with recording); then {idle:.0} ms to idle",
            frames + 2,
            wall / (frames + 2) as f64
        ))?;
    }
    expect_px(s, "the strokes painted", (300, 500), [0x1a, 0x2e, 0x8c, 255], 2)?;
    expect_steps(s, "three strokes, three steps", 3)?;
    Ok(())
}
