//! B. Layers: the Layers panel and every layer command, used the way a
//! Photoshop user would (docs/testing/user-journeys.md, area B).

use crate::uitest::prelude::*;
use lumenply_doc::{BlendMode, Document, Layer};

scenario_list! {
    "layers-add-rename-delete" => add_rename_delete: "Add layers three ways, rename them, delete and undo",
    "layers-duplicate-group" => duplicate_group: "Duplicate a layer, group two, collapse, ungroup, undo",
    "layers-reorder" => reorder: "Drag a layer in the panel, Bring to Front, Send to Back, move up and down",
    "layers-visibility-blend" => visibility_blend: "The eye, Alt-click solo, opacity, fill and blend mode",
    "layers-locks" => locks: "Lock transparency, pixels, position and all; what each refuses and how it says so",
    "layers-masks" => masks: "A mask from a selection, painted, disabled, applied and removed",
    "layers-clip-merge" => clip_merge: "Clipping masks, layer via copy and cut, merge, stamp and flatten",
    "layers-fill-layers" => fill_layers: "Solid, gradient and pattern fill layers, edited in Properties",
    "layers-smart-objects" => smart_objects: "Convert, transform twice, edit contents, replace contents, rasterize",
    "layers-align-filter-menu" => align_filter_menu: "Align and distribute, the layer filter, the context menu, thumbnails",
}

/// The foreground colour a fresh session starts with (#1A2E8C).
const BLUE: [u8; 4] = [0x1a, 0x2e, 0x8c, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Within `tol` of `want` on every channel.
fn near(got: [u8; 4], want: [u8; 4], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

/// sRGB (0..255) to linear light and back, for expected blends.
fn lin(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb(l: f32) -> u8 {
    let c = if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `top` at `alpha` over opaque `bottom`, mixed in linear light.
fn over(top: [u8; 4], bottom: [u8; 4], alpha: f32) -> [u8; 4] {
    let m = |i: usize| srgb(lin(top[i]) * alpha + lin(bottom[i]) * (1.0 - alpha));
    [m(0), m(1), m(2), 255]
}

/// The layer tree as the panel shows it, top first; children are
/// written "Group/child".
fn outline(d: &Document) -> Vec<String> {
    fn walk(list: &[Layer], prefix: &str, out: &mut Vec<String>) {
        for l in list.iter().rev() {
            out.push(format!("{prefix}{}", l.name));
            if let Some(c) = l.children() {
                walk(c, &format!("{prefix}{}/", l.name), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(d.layers(), "", &mut out);
    out
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn expect_layers(s: &mut Session, what: &str, want: &[&str]) -> UiResult<bool> {
    let got = s.layer_names()?;
    s.check_eq(what, got, names(want))
}

fn expect_tree(s: &mut Session, what: &str, want: &[&str]) -> UiResult<bool> {
    let got = tree(s)?;
    s.check_eq(what, got, names(want))
}

fn expect_last_step(s: &mut Session, what: &str, want: &str) -> UiResult<bool> {
    let got = s.history()?.last().cloned().unwrap_or_default();
    s.check_eq(what, got, want.to_string())
}

fn expect_pixel(s: &mut Session, what: &str, at: (u32, u32), want: [u8; 4]) -> UiResult<bool> {
    let got = s.pixel(at.0, at.1)?;
    s.check_eq(what, got, want)
}

fn tree(s: &mut Session) -> UiResult<Vec<String>> {
    s.doc(outline)
}

/// File ▸ New from the welcome screen at `w` × `h`.
fn new_doc(s: &mut Session, w: u32, h: u32) -> UiResult {
    s.describe(&format!("Make a new {w} × {h} image from the welcome screen"));
    s.click("New image…")?;
    s.set_field("Width", &w.to_string())?;
    s.set_field("Height", &h.to_string())?;
    s.click("Create")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("the new document has that size", size, (w, h))?;
    Ok(())
}

/// Select a rectangle with the marquee and fill it with the brush colour
/// on the active layer, then deselect.
fn fill_rect(s: &mut Session, from: (f32, f32), to: (f32, f32)) -> UiResult {
    s.describe("Pick the Rectangular Marquee");
    s.click("Rectangular Marquee")?;
    s.describe("Select a rectangle");
    s.canvas_drag(from, to, 10, "")?;
    s.describe("Fill it with Edit ▸ Fill with brush colour");
    s.menu("Edit > Fill with brush colour")?;
    s.describe("Deselect");
    s.key("Cmd+D")?;
    s.wait_idle()?;
    Ok(())
}

/// Whether a layer row is wholly visible inside the Layers list, above
/// the panel's footer buttons.
fn row_fully_visible(s: &mut Session, layer: &str) -> (bool, String) {
    // The list is a named scroll view only while it scrolls; when every
    // row fits, the footer is the edge to stay above.
    let row = s.node(&format!("Layer {layer}"));
    let list = s.node("Layers");
    let footer = s.node("New layer");
    match (row, footer) {
        (Some(r), Some(f)) => {
            let in_list = list.as_ref().is_none_or(|l| {
                r.rect.top() >= l.rect.top() - 0.5 && r.rect.bottom() <= l.rect.bottom() + 0.5
            });
            let ok = in_list && r.rect.bottom() <= f.rect.top() + 0.5;
            (
                ok,
                format!(
                    "row {:.0}–{:.0}, list {}, footer from {:.0}",
                    r.rect.top(),
                    r.rect.bottom(),
                    list.map_or("not scrolling".to_string(), |l| format!(
                        "{:.0}–{:.0}",
                        l.rect.top(),
                        l.rect.bottom()
                    )),
                    f.rect.top()
                ),
            )
        }
        (r, f) => (false, format!("row {}, footer {}", r.is_some(), f.is_some())),
    }
}

/// Open the Layer menu, then click `path` in it. (The harness's `menu`
/// takes a History step of the same name for an open menu item.)
fn layer_menu(s: &mut Session, path: &str) -> UiResult {
    s.describe(&format!(
        "Open the Layer menu and pick {}",
        path.replace('>', "▸")
    ));
    s.click_role(Role::Button, "Layer")?;
    // Down into the menu first, as a hand does, so the pointer never
    // crosses the next menu title on its way to the right column.
    s.hover("New pixel layer")?;
    // A History step or a Properties button can share the item's name
    // (and lie under the menu on a narrow window): aim at the one inside
    // the open menu.
    if !path.contains('>') {
        let menu = ["New pixel layer", "Flatten image", "Flip layer vertical"]
            .iter()
            .filter_map(|n| s.node(n))
            .map(|n| n.rect)
            .reduce(|a, b| a.union(b));
        let item = menu.and_then(|m| {
            s.tree()
                .matches(path, Some(Role::Button))
                .into_iter()
                .find(|n| !n.disabled && m.contains(n.rect.center()))
                .cloned()
        });
        if let Some(n) = item {
            return s.click_at(n.rect.center(), &format!("“{path}” in the Layer menu"));
        }
    }
    // Rest on a submenu's title first, so a sibling submenu the pointer
    // brushed on its way (Align, above Distribute) closes.
    if let Some((first, last)) = path.split_once('>') {
        let (first, last) = (first.trim(), last.trim());
        s.hover(first)?;
        s.wait_frames(5)?;
        // The item in the submenu that just opened: the one of that name
        // nearest the submenu's title (a tool or a Properties button can
        // share the name), reached along the title's row as a hand does.
        if let Some(parent) = s.node(first) {
            let at = parent.rect.right_center();
            let item = s
                .tree()
                .matches(last, Some(Role::Button))
                .into_iter()
                .filter(|n| !n.disabled)
                .min_by(|a, b| {
                    a.rect
                        .center()
                        .distance(at)
                        .total_cmp(&b.rect.center().distance(at))
                })
                .cloned();
            if let Some(n) = item.filter(|_| !last.contains('>')) {
                let y = at.y.clamp(n.rect.top() - 200.0, n.rect.bottom() + 200.0);
                s.move_to(pos2(n.rect.left() + 12.0, y), &format!("the {first} submenu"))?;
                return s.click_at(n.rect.center(), &format!("“{last}” in the {first} submenu"));
            }
        }
    }
    s.menu(&format!("Layer > {path}"))
}

fn active_name(s: &mut Session) -> UiResult<Option<String>> {
    s.app(|a| a.active_layer().map(|l| l.name.clone()))
}

/// The platform's word for the command key, as menus and tooltips show it.
fn cmd_word() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

fn add_rename_delete(s: &mut Session) -> UiResult {
    new_doc(s, 800, 600)?;

    s.describe("Add a layer with the + button under the Layers panel");
    s.click("New layer")?;
    expect_layers(s, "Layer 2 sits above the Background", &["Layer 2", "Background"])?;
    let active = s.app(|a| a.active_layer().map(|l| l.name.clone()))?;
    s.check_eq("the new layer is the active one", active, Some("Layer 2".into()))?;
    let (ok, rows) = row_fully_visible(s, "Background");
    s.check(
        "both rows show whole above the panel's buttons",
        ok,
        "the Background row inside the list, above the footer",
        rows,
    )?;

    // Photoshop selects the whole name on a double-click, so typing
    // replaces it.
    s.describe("Double-click the new layer's name and type a new one");
    s.double_click("Layer Layer 2")?;
    s.type_text("Sky")?;
    s.key("Enter")?;
    expect_layers(s, "typing replaced the old name", &["Sky", "Background"])?;
    expect_last_step(s, "the rename is one history step", "Rename layer")?;

    s.describe("Rename it again from Layer ▸ Rename, then change your mind with Esc");
    layer_menu(s, "Rename")?;
    s.type_text("Oops")?;
    s.key("Esc")?;
    expect_layers(s, "Esc keeps the old name", &["Sky", "Background"])?;

    s.describe("Add another layer from Layer ▸ New pixel layer");
    layer_menu(s, "New pixel layer")?;
    s.describe("And one more with Shift+Cmd+N");
    s.key("Cmd+Shift+N")?;
    expect_layers(
        s,
        "four layers, the newest on top",
        &["Layer 4", "Layer 3", "Sky", "Background"],
    )?;

    // The eye at the left of each row is a control of its own.
    s.check(
        "the eye has a name a screen reader says",
        s.has_node("Show Sky"),
        "a checkbox “Show Sky”",
        format!("{:?}", s.texts_matching("Sky")),
    )?;

    s.describe("Right-click Layer 3 and rename it from the context menu");
    s.right_click("Layer Layer 3")?;
    s.click("Rename")?;
    s.type_text("Clouds")?;
    s.key("Enter")?;
    expect_layers(
        s,
        "renamed from the context menu",
        &["Layer 4", "Clouds", "Sky", "Background"],
    )?;

    s.describe("Select Layer 4 and delete it with the bin button");
    s.click("Layer Layer 4")?;
    s.click("Delete layer")?;
    expect_layers(s, "Layer 4 is gone", &["Clouds", "Sky", "Background"])?;
    let active = s.app(|a| a.active_layer().map(|l| l.name.clone()))?;
    s.check_eq(
        "the layer below becomes active, as in Photoshop",
        active,
        Some("Clouds".into()),
    )?;
    s.describe("Undo the delete");
    s.key("Cmd+Z")?;
    expect_layers(s, "Layer 4 is back", &["Layer 4", "Clouds", "Sky", "Background"])?;
    s.describe("Delete the active layer with the Delete key... after picking Clouds");
    s.click("Layer Clouds")?;
    layer_menu(s, "Delete layer")?;
    expect_layers(
        s,
        "Clouds deleted from the Layer menu",
        &["Layer 4", "Sky", "Background"],
    )?;
    Ok(())
}

fn duplicate_group(s: &mut Session) -> UiResult {
    new_doc(s, 600, 400)?;
    s.describe("Add a layer and paint a blue block on it");
    s.click("New layer")?;
    fill_rect(s, (100.0, 100.0), (300.0, 250.0))?;
    let painted = s.layer_pixel("Layer 2", 200, 150)?;
    s.check_eq("the block is painted", painted, Some(BLUE))?;

    s.describe("Duplicate it with Cmd+J");
    s.key("Cmd+J")?;
    expect_layers(
        s,
        "a copy sits right above",
        &["Layer 2 copy", "Layer 2", "Background"],
    )?;
    let copy = s.layer_pixel("Layer 2 copy", 200, 150)?;
    s.check_eq("the copy has the same pixels", copy, Some(BLUE))?;
    let active = s.app(|a| a.active_layer().map(|l| l.name.clone()))?;
    s.check_eq("the copy is active", active, Some("Layer 2 copy".into()))?;

    s.describe("Duplicate again from Layer ▸ Duplicate layer");
    layer_menu(s, "Duplicate layer")?;
    expect_layers(
        s,
        "a second copy, numbered as Photoshop does",
        &["Layer 2 copy 2", "Layer 2 copy", "Layer 2", "Background"],
    )?;

    // The Group button's tooltip says how to pick several layers.
    let group_tip = s
        .tree()
        .nodes
        .iter()
        .find(|n| n.name.starts_with("Group selected layers"))
        .map(|n| n.name.clone())
        .unwrap_or_default();
    s.check(
        "the Group button names this platform's modifier",
        group_tip.contains(&format!("{}+click", cmd_word())),
        format!("“{}+click” in its tooltip", cmd_word()),
        group_tip.clone(),
    )?;

    s.describe("Pick Layer 2, then Cmd-click Layer 2 copy to add it");
    s.click("Layer Layer 2")?;
    s.click_with("Layer Layer 2 copy", "Cmd")?;
    let renaming = s.app(|a| a.renaming.is_some())?;
    s.check_eq(
        "two quick clicks on different rows start no rename",
        renaming,
        false,
    )?;
    s.describe("Group the two with the folder button");
    s.click(&group_tip)?;
    s.wait_idle()?;
    let t = tree(s)?;
    s.check_eq(
        "both layers are inside a new group, in their order",
        t.clone(),
        names(&[
            "Layer 2 copy 2",
            "Group 4",
            "Group 4/Layer 2 copy",
            "Group 4/Layer 2",
            "Background",
        ]),
    )?;
    let picture = s.pixel(200, 150)?;
    s.check_eq("grouping changes nothing on the canvas", picture, BLUE)?;

    s.describe("Collapse the group with its triangle");
    s.click_offset("Layer Group 4", 38.0, 19.0, "the disclosure triangle")?;
    let collapsed = s.doc(|d| find_layer(d, "Group 4").map(|l| l.collapsed))?;
    s.check_eq("the group is collapsed", collapsed, Some(true))?;
    s.check(
        "its children's rows are hidden",
        !s.has_node("Layer Layer 2 copy"),
        "no “Layer 2 copy” row",
        format!("{}", s.has_node("Layer Layer 2 copy")),
    )?;

    s.describe("Ungroup with Layer ▸ Ungroup");
    s.click("Layer Group 4")?;
    layer_menu(s, "Ungroup")?;
    expect_tree(
        s,
        "the layers are back at the top level",
        &["Layer 2 copy 2", "Layer 2 copy", "Layer 2", "Background"],
    )?;
    s.describe("Undo the ungroup");
    s.key("Cmd+Z")?;
    let back = tree(s)?;
    s.check_eq("the group is back", back, t)?;
    s.describe("Ungroup with Shift+Cmd+G, as in Photoshop");
    s.click("Layer Group 4")?;
    s.key("Cmd+Shift+G")?;
    expect_tree(
        s,
        "Shift+Cmd+G ungroups",
        &["Layer 2 copy 2", "Layer 2 copy", "Layer 2", "Background"],
    )?;
    s.describe("Group the top layer with Cmd+G");
    s.click("Layer Layer 2 copy 2")?;
    s.key("Cmd+G")?;
    let t = tree(s)?;
    s.check(
        "Cmd+G puts the active layer in a group",
        t.iter().any(|n| n.ends_with("/Layer 2 copy 2")),
        "“Layer 2 copy 2” inside a group",
        format!("{t:?}"),
    )?;
    Ok(())
}

fn reorder(s: &mut Session) -> UiResult {
    new_doc(s, 600, 400)?;
    s.describe("Add three layers");
    s.click("New layer")?;
    s.click("New layer")?;
    s.click("New layer")?;
    expect_layers(
        s,
        "three layers above the Background",
        &["Layer 4", "Layer 3", "Layer 2", "Background"],
    )?;

    // Drag Layer 4 down to sit between Layer 3 and Layer 2.
    let from = s.point_in("Layer Layer 4", 0.6, 0.5)?;
    let to = s.point_in("Layer Layer 2", 0.6, 0.2)?;
    s.describe("Drag Layer 4 down below Layer 3");
    s.drag(from, to, 14, "")?;
    s.wait_idle()?;
    expect_layers(
        s,
        "Layer 4 moved down one place",
        &["Layer 3", "Layer 4", "Layer 2", "Background"],
    )?;
    expect_last_step(s, "one history step for the drag", "Move layer")?;

    s.describe("Pick Layer 2 and Bring to Front with Shift+Cmd+]");
    s.click("Layer Layer 2")?;
    s.key("Cmd+Shift+]")?;
    expect_layers(
        s,
        "Layer 2 is on top",
        &["Layer 2", "Layer 3", "Layer 4", "Background"],
    )?;
    s.describe("Send it to the back from Layer ▸ Send to back");
    layer_menu(s, "Send to back")?;
    expect_layers(
        s,
        "Layer 2 is at the bottom",
        &["Layer 3", "Layer 4", "Background", "Layer 2"],
    )?;
    s.describe("Move it up one with the arrow button");
    s.click("Move layer up")?;
    expect_layers(
        s,
        "one place up",
        &["Layer 3", "Layer 4", "Layer 2", "Background"],
    )?;
    s.describe("Move it up again with Cmd+]");
    s.key("Cmd+]")?;
    expect_layers(
        s,
        "Cmd+] moves up one place",
        &["Layer 3", "Layer 2", "Layer 4", "Background"],
    )?;
    s.describe("And down with Cmd+[");
    s.key("Cmd+[")?;
    expect_layers(
        s,
        "Cmd+[ moves down one place",
        &["Layer 3", "Layer 4", "Layer 2", "Background"],
    )?;
    s.describe("Pick the top layer: Move layer up is greyed out, and says why");
    s.click("Layer Layer 3")?;
    s.expect_disabled("Move layer up", "Already at the top")?;
    s.describe("Undo twice");
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    expect_layers(
        s,
        "two moves undone",
        &["Layer 3", "Layer 4", "Layer 2", "Background"],
    )?;
    Ok(())
}

fn visibility_blend(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.describe("Add a layer and paint a blue block on it");
    s.click("New layer")?;
    fill_rect(s, (100.0, 50.0), (300.0, 250.0))?;
    expect_pixel(s, "the block shows", (200, 150), BLUE)?;

    s.describe("Hide Layer 2 with its eye");
    s.click_offset("Layer Layer 2", 14.0, 19.0, "the eye")?;
    let vis = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.visible))?;
    s.check_eq("Layer 2 is hidden", vis, Some(false))?;
    expect_pixel(s, "the canvas is white there", (200, 150), WHITE)?;
    s.describe("Show it again");
    s.click_offset("Layer Layer 2", 14.0, 19.0, "the eye")?;
    expect_pixel(s, "the block is back", (200, 150), BLUE)?;

    s.describe("Alt-click Layer 2's eye to show it alone");
    let eye = s.point_in("Layer Layer 2", 0.0, 0.5)?;
    let eye = pos2(eye.x + 14.0, eye.y);
    s.drag(eye, eye, 2, "Alt")?;
    let vis = s.doc(|d| find_layer(d, "Background").map(|l| l.visible))?;
    s.check_eq("the Background is hidden", vis, Some(false))?;
    let corner = s.pixel(20, 20)?;
    s.check_eq("outside the block is transparent", corner[3], 0)?;
    s.describe("Alt-click it again to bring the others back");
    s.drag(eye, eye, 2, "Alt")?;
    let vis = s.doc(|d| find_layer(d, "Background").map(|l| l.visible))?;
    s.check_eq("the Background shows again", vis, Some(true))?;

    s.describe("Type 50 % into Opacity in Properties");
    s.within("Properties", |s| s.set_field("Opacity", "50"))?;
    let o = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.opacity))?;
    s.check_eq("the layer is at 50 % opacity", o, Some(0.5))?;
    let want = over(BLUE, WHITE, 0.5);
    let got = s.pixel(200, 150)?;
    s.check(
        "the block is half see-through",
        near(got, want, 2),
        format!("{want:?}"),
        format!("{got:?}"),
    )?;
    s.expect_text("50%")?;

    s.describe("Back to 100 %, then Fill to 25 %");
    s.within("Properties", |s| s.set_field("Opacity", "100"))?;
    s.within("Properties", |s| s.set_field("Fill", "25"))?;
    let f = s.doc(|d| find_layer(d, "Layer 2").map(|l| (l.opacity, l.fill_opacity)))?;
    s.check_eq("opacity 100 %, fill 25 %", f, Some((1.0, 0.25)))?;
    let want = over(BLUE, WHITE, 0.25);
    let got = s.pixel(200, 150)?;
    s.check(
        "the block shows at a quarter",
        near(got, want, 2),
        format!("{want:?}"),
        format!("{got:?}"),
    )?;
    s.within("Properties", |s| s.set_field("Fill", "100"))?;

    s.describe("Pick Screen in the Blend mode list");
    s.click_role(Role::ComboBox, "Blend mode")?;
    s.click("Screen")?;
    let b = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.blend))?;
    s.check_eq("the layer screens", b, Some(BlendMode::Screen))?;
    expect_pixel(s, "blue screened over white is white", (200, 150), WHITE)?;
    s.describe("Undo the blend mode");
    s.key("Cmd+Z")?;
    let b = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.blend))?;
    s.check_eq("Normal again", b, Some(BlendMode::Normal))?;
    expect_pixel(s, "the block is back", (200, 150), BLUE)?;
    Ok(())
}

fn locks(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.describe("Add a layer and paint a blue block on it");
    s.click("New layer")?;
    fill_rect(s, (100.0, 100.0), (200.0, 200.0))?;

    s.describe("Lock its transparent pixels with the first lock toggle");
    s.click("Lock transparent pixels")?;
    let l = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.locks.transparency))?;
    s.check_eq("transparency is locked", l, Some(true))?;
    s.expect_text("Locked: ")?;
    s.describe("Swap the colours (white in front) and paint across the block's edge");
    s.click("Swap colours")?;
    s.click_role(Role::Button, "Brush")?;
    s.canvas_drag((150.0, 150.0), (300.0, 150.0), 12, "")?;
    s.wait_idle()?;
    let inside = s.layer_pixel("Layer 2", 160, 150)?;
    let outside = s.layer_pixel("Layer 2", 260, 150)?.map(|p| p[3]);
    s.check_eq("the block took the white paint", inside, Some(WHITE))?;
    s.check_eq("the transparent area stayed empty", outside, Some(0))?;
    s.describe("Undo the stroke");
    s.key("Cmd+Z")?;

    s.describe("Unlock transparency, lock image pixels instead");
    s.click("Lock transparent pixels")?;
    s.click("Lock image pixels")?;
    s.describe("Try to paint");
    s.canvas_drag((150.0, 120.0), (190.0, 120.0), 8, "")?;
    s.wait_idle()?;
    let px = s.layer_pixel("Layer 2", 170, 120)?;
    s.check_eq("the pixels didn't change", px, Some(BLUE))?;
    s.expect_text("pixels are locked")?;
    s.describe("Edit ▸ Fill is greyed out and says why");
    s.click("Edit")?;
    s.screenshot("edit-menu-pixels-locked");
    let why = s.app(|a| {
        (
            a.action_block("fill"),
            a.action_block("fill-bg"),
            a.action_block("fill-dialog"),
        )
    })?;
    s.check_eq(
        "the Edit menu's fills are greyed out with the reason",
        why,
        (
            Some("The layer's pixels are locked"),
            Some("The layer's pixels are locked"),
            Some("The layer's pixels are locked"),
        ),
    )?;
    s.key("Esc")?;
    s.describe("The Move tool still moves a pixel-locked layer");
    s.click_role(Role::Button, "Move")?;
    s.canvas_drag((150.0, 150.0), (170.0, 150.0), 8, "")?;
    s.wait_idle()?;
    let moved = s.layer_pixel("Layer 2", 215, 150)?;
    s.check_eq("the block moved 20 px right", moved, Some(BLUE))?;

    s.describe("Lock position too, and try to move it again");
    s.click("Lock position")?;
    s.canvas_drag((170.0, 150.0), (250.0, 150.0), 8, "")?;
    s.wait_idle()?;
    let still = s.layer_pixel("Layer 2", 215, 150)?;
    s.check_eq("the block stayed put", still, Some(BLUE))?;
    s.expect_text("position is locked")?;

    s.describe("Lock all: the other toggles show as covered");
    s.click_role(Role::CheckBox, "Lock all")?;
    let l = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.locks))?;
    s.check(
        "Lock all is on",
        l.is_some_and(|l| l.all),
        "all locked",
        format!("{l:?}"),
    )?;
    let tip = s.hover("Lock position")?;
    s.check(
        "the position toggle says Lock all covers it",
        tip.iter().any(|t| t.contains("Lock all")),
        "a tooltip naming Lock all",
        format!("{tip:?}"),
    )?;
    s.describe("Opacity, Fill and Blend are greyed out in Properties, and say why");
    let props = s.node("Properties").map(|n| n.rect);
    let off = ["Opacity", "Fill", "Blend mode"].map(|name| {
        let controls: Vec<_> = s
            .tree()
            .matches(name, None)
            .into_iter()
            // In the Properties column, scrolled into view or not.
            .filter(|n| {
                let c = n.rect.center();
                n.role != Role::Label && props.is_some_and(|p| p.x_range().contains(c.x) && c.y >= p.top())
            })
            .collect();
        !controls.is_empty() && controls.iter().all(|n| n.disabled)
    });
    s.check_eq("the three are disabled", off, [true, true, true])?;
    s.expect_text("The layer is locked")?;
    let o = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.opacity))?;
    s.check_eq("opacity unchanged", o, Some(1.0))?;
    s.describe("Unlock everything with Lock all and one by one");
    s.click_role(Role::CheckBox, "Lock all")?;
    s.click("Lock position")?;
    s.click("Lock image pixels")?;
    let l = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.locks.is_empty()))?;
    s.check_eq("no locks left", l, Some(true))?;
    s.expect_text("Layer unlocked")?;
    Ok(())
}

fn masks(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.describe("Add a layer and fill all of it with blue");
    s.click("New layer")?;
    fill_rect(s, (0.0, 0.0), (400.0, 300.0))?;
    s.describe("Select the middle with the marquee");
    s.canvas_drag((100.0, 100.0), (300.0, 200.0), 10, "")?;

    // Photoshop: Layer ▸ Layer Mask ▸ Reveal Selection.
    mask_menu(s, "Reveal selection", Some("Select > Layer mask from selection"))?;
    s.wait_idle()?;
    let m = (s.mask_at("Layer 2", 50, 50)?, s.mask_at("Layer 2", 200, 150)?);
    s.check_eq(
        "hidden outside the selection, shown inside",
        m,
        (Some(0.0), Some(1.0)),
    )?;
    let sel = s.selection_bounds()?;
    s.check_eq(
        "the selection is used up (deselected), as in Photoshop",
        sel,
        None,
    )?;
    expect_pixel(s, "the corner shows the white background", (50, 50), WHITE)?;
    expect_pixel(s, "the middle stays blue", (200, 150), BLUE)?;
    let editing = s.app(|a| a.editing_mask)?;
    s.check_eq("the mask is the paint target", editing, true)?;

    s.describe("Reset to black and white with the Default colours button");
    s.click("Default colours")?;
    s.describe("Paint black on the mask across the middle");
    s.click_role(Role::Button, "Brush")?;
    s.canvas_drag((120.0, 150.0), (280.0, 150.0), 12, "")?;
    s.wait_idle()?;
    let painted = s.mask_at("Layer 2", 200, 150)?;
    s.check(
        "the stroke hid the layer there",
        painted.is_some_and(|v| v < 0.05),
        "about 0",
        format!("{painted:?}"),
    )?;
    let px = s.layer_pixel("Layer 2", 200, 150)?;
    s.check_eq("the layer's own pixels are untouched", px, Some(BLUE))?;

    s.describe("Disable the mask from the row's right-click menu");
    s.right_click("Layer Layer 2")?;
    s.click("Disable mask")?;
    let on = s.doc(|d| find_layer(d, "Layer 2").and_then(|l| l.mask.as_ref().map(|m| m.enabled)))?;
    s.check_eq("the mask is off", on, Some(false))?;
    expect_pixel(s, "the whole layer shows", (50, 50), BLUE)?;
    mask_menu(s, "Enable mask", Some("Layer > Enable mask"))?;
    expect_pixel(s, "the corner is hidden again", (50, 50), WHITE)?;

    // Photoshop: Shift-click the mask thumbnail to switch it off and on.
    let thumb = s.point_in("Layer Layer 2", 0.0, 0.5)?;
    let thumb = pos2(thumb.x + 100.0, thumb.y);
    s.describe("Shift-click the mask thumbnail to disable it");
    s.drag(thumb, thumb, 2, "Shift")?;
    let on = s.doc(|d| find_layer(d, "Layer 2").and_then(|l| l.mask.as_ref().map(|m| m.enabled)))?;
    s.check_eq("Shift-click disabled the mask", on, Some(false))?;
    s.describe("Shift-click it again to enable it");
    s.drag(thumb, thumb, 2, "Shift")?;
    let on = s.doc(|d| find_layer(d, "Layer 2").and_then(|l| l.mask.as_ref().map(|m| m.enabled)))?;
    s.check_eq("and on again", on, Some(true))?;

    s.describe("Apply the mask: its holes become the layer's own transparency");
    if mask_menu(s, "Apply mask", None)? {
        let gone = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.mask.is_none()))?;
        s.check_eq("the mask is gone", gone, Some(true))?;
        let a = (
            s.layer_pixel("Layer 2", 50, 50)?.map(|p| p[3]),
            s.layer_pixel("Layer 2", 150, 120)?.map(|p| p[3]),
        );
        s.check_eq("the layer is cut where the mask hid it", a, (Some(0), Some(255)))?;
        expect_pixel(s, "the picture didn't change", (50, 50), WHITE)?;
        s.describe("Undo the apply");
        s.key("Cmd+Z")?;
        let back = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.mask.is_some()))?;
        s.check_eq("the mask is back", back, Some(true))?;
    }

    mask_menu(s, "Delete mask", Some("Layer > Remove mask"))?;
    let gone = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.mask.is_none()))?;
    s.check_eq("no mask", gone, Some(true))?;
    expect_pixel(s, "the whole blue layer shows", (50, 50), BLUE)?;

    s.describe("Select the middle again");
    s.click("Rectangular Marquee")?;
    s.canvas_drag((100.0, 100.0), (300.0, 200.0), 10, "")?;
    s.describe("Open the Layer menu and close it again with Esc");
    s.click_role(Role::Button, "Layer")?;
    s.key("Esc")?;
    let closed = !s.has_node("New pixel layer");
    s.check_eq("Esc closed the menu", closed, true)?;
    let sel = s.selection_bounds()?;
    s.check_eq("and left the selection alone", sel, Some((100, 100, 200, 100)))?;
    if mask_menu(s, "Hide selection", None)? {
        let m = (s.mask_at("Layer 2", 50, 50)?, s.mask_at("Layer 2", 200, 150)?);
        s.check_eq("shown outside, hidden inside", m, (Some(1.0), Some(0.0)))?;
        s.describe("Undo it");
        s.key("Cmd+Z")?;
    }
    s.describe("Alt-click the mask button under the panel: a mask hiding the selection");
    let button = s
        .tree()
        .nodes
        .iter()
        .find(|n| n.name.starts_with("Add mask"))
        .map(|n| n.name.clone())
        .unwrap_or_else(|| "Add mask".into());
    s.click_with(&button, "Alt")?;
    let m = (s.mask_at("Layer 2", 50, 50)?, s.mask_at("Layer 2", 200, 150)?);
    s.check_eq(
        "Alt-click hides the selection, as in Photoshop",
        m,
        (Some(1.0), Some(0.0)),
    )?;
    Ok(())
}

/// Layer ▸ Layer mask ▸ `item`, as Photoshop has it. When the app has no
/// such item the failure is logged and `fallback` (a menu path) does the
/// same job where there is one; false when nothing ran.
fn mask_menu(s: &mut Session, item: &str, fallback: Option<&str>) -> UiResult<bool> {
    s.describe(&format!("Open the Layer menu and pick Layer mask ▸ {item}"));
    s.click_role(Role::Button, "Layer")?;
    s.hover("New pixel layer")?;
    let there = s.has_node("Layer mask");
    s.check(
        &format!("Layer ▸ Layer mask ▸ {item} is there"),
        there,
        "a Layer mask submenu",
        "no Layer mask submenu",
    )?;
    if there {
        s.menu(&format!("Layer > Layer mask > {item}"))?;
        return Ok(true);
    }
    s.key("Esc")?;
    match fallback {
        Some(path) => {
            s.describe(&format!("Use {} instead", path.replace('>', "▸")));
            if path.starts_with("Layer >") {
                s.click_role(Role::Button, "Layer")?;
                s.hover("New pixel layer")?;
            }
            s.menu(path)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn clip_merge(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.describe("Layer 2: a small blue block");
    s.click("New layer")?;
    fill_rect(s, (100.0, 100.0), (200.0, 200.0))?;
    s.describe("Layer 3: blue over the whole canvas");
    s.click("New layer")?;
    s.key("Cmd+A")?;
    s.menu("Edit > Fill with brush colour")?;
    s.key("Cmd+D")?;
    expect_pixel(s, "blue everywhere", (300, 250), BLUE)?;

    s.describe("Clip Layer 3 to the block with Alt+Cmd+G");
    s.key("Cmd+Alt+G")?;
    let clip = s.doc(|d| find_layer(d, "Layer 3").map(|l| l.clip))?;
    s.check_eq("Layer 3 is clipped", clip, Some(true))?;
    expect_pixel(s, "outside the block it is white again", (300, 250), WHITE)?;
    expect_pixel(s, "inside the block blue", (150, 150), BLUE)?;
    s.describe("Release it from the row's right-click menu");
    s.right_click("Layer Layer 3")?;
    s.click("Release clip")?;
    expect_pixel(s, "blue everywhere again", (300, 250), BLUE)?;
    layer_menu(s, "Clip to layer below")?;
    expect_pixel(s, "clipped again", (300, 250), WHITE)?;

    s.describe("Pick the block: the Layer menu offers Merge clipping mask");
    s.click("Layer Layer 2")?;
    s.click_role(Role::Button, "Layer")?;
    s.expect_node("Merge clipping mask")?;
    s.key("Esc")?;
    s.describe("Merge it with Cmd+E");
    s.key("Cmd+E")?;
    expect_layers(s, "one layer holds both", &["Layer 2", "Background"])?;
    expect_pixel(s, "the picture is unchanged outside", (300, 250), WHITE)?;
    expect_pixel(s, "the block is still blue", (150, 150), BLUE)?;

    s.describe("Select part of the block, Layer via copy with Cmd+J");
    s.click("Rectangular Marquee")?;
    s.canvas_drag((100.0, 100.0), (150.0, 200.0), 8, "")?;
    s.key("Cmd+J")?;
    expect_layers(s, "a copy above", &["Layer 2 copy", "Layer 2", "Background"])?;
    let c = (
        s.layer_pixel("Layer 2 copy", 120, 150)?.map(|p| p[3]),
        s.layer_pixel("Layer 2 copy", 180, 150)?.map(|p| p[3]),
    );
    s.check_eq("only the selected half was copied", c, (Some(255), Some(0)))?;
    let active = active_name(s)?;
    s.check_eq("the new layer is active", active, Some("Layer 2 copy".into()))?;

    s.describe("Pick Layer 2, select its other half and Layer via cut (Shift+Cmd+J)");
    s.click("Layer Layer 2")?;
    s.canvas_drag((150.0, 100.0), (200.0, 200.0), 8, "")?;
    s.key("Cmd+Shift+J")?;
    let t = s.layer_names()?;
    s.check(
        "a new layer above Layer 2",
        t.len() == 4 && t[2] == "Layer 2",
        "4 layers, Layer 2 third",
        format!("{t:?}"),
    )?;
    let hole = s.layer_pixel("Layer 2", 180, 150)?.map(|p| p[3]);
    s.check_eq("the cut left a hole in Layer 2", hole, Some(0))?;
    expect_pixel(s, "the picture is unchanged", (180, 150), BLUE)?;
    s.key("Cmd+D")?;

    s.describe("Stamp visible with Shift+Alt+Cmd+E");
    s.key("Cmd+Shift+Alt+E")?;
    let t = s.layer_names()?;
    s.check(
        "a stamp layer on top",
        t.len() == 5 && t[0].starts_with("Stamp"),
        "5 layers, a Stamp on top",
        format!("{t:?}"),
    )?;
    let top = t.first().cloned().unwrap_or_default();
    let p = s.layer_pixel(&top, 300, 250)?;
    s.check_eq("the stamp holds the white background too", p, Some(WHITE))?;
    s.describe("Undo the stamp");
    s.key("Cmd+Z")?;

    s.describe("Pick the top layer, Cmd-click Layer 2 copy, merge them with Cmd+E");
    let t = s.layer_names()?;
    let top = t.first().cloned().unwrap_or_default();
    s.click(&format!("Layer {top}"))?;
    s.click_with("Layer Layer 2 copy", "Cmd")?;
    s.key("Cmd+E")?;
    let n = s.layer_names()?.len();
    s.check_eq("two layers became one", n, 3)?;
    expect_pixel(s, "the picture is unchanged", (120, 150), BLUE)?;

    s.describe("Merge visible with Shift+Cmd+E");
    s.key("Cmd+Shift+E")?;
    let n = s.layer_names()?.len();
    s.check_eq("everything visible in one layer", n, 1)?;
    expect_pixel(s, "the picture is unchanged", (150, 150), BLUE)?;
    s.describe("Undo the merge");
    s.key("Cmd+Z")?;
    layer_menu(s, "Flatten image")?;
    expect_layers(s, "one Background layer", &["Background"])?;
    expect_pixel(s, "the picture is unchanged", (150, 150), BLUE)?;
    expect_pixel(s, "and the white stays", (300, 250), WHITE)?;
    Ok(())
}

fn fill_layers(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    let dock_left = s.node("Layers panel").map(|n| n.rect.left());
    layer_menu(s, "New fill layer > Solid color")?;
    s.wait_idle()?;
    expect_layers(s, "a Color Fill layer on top", &["Color Fill", "Background"])?;
    expect_pixel(s, "the canvas is the foreground blue", (200, 150), BLUE)?;
    s.expect_text("COLOR FILL")?;
    let now = s.node("Layers panel").map(|n| n.rect.left());
    s.check_eq(
        "the fill's settings fit: the dock keeps its width",
        now,
        dock_left,
    )?;

    s.describe("Switch it to a gradient in Properties");
    s.click("Gradient")?;
    s.wait_idle()?;
    expect_layers(
        s,
        "a fill still named by its kind follows it",
        &["Gradient Fill", "Background"],
    )?;
    let kind =
        s.doc(|d| find_layer(d, "Gradient Fill").and_then(|l| l.fill_layer().map(|f| f.fill.name())))?;
    s.check_eq("it is a gradient fill now", kind, Some("Gradient Fill"))?;
    let (l, r) = (s.pixel(200, 2)?, s.pixel(200, 297)?);
    s.check(
        "the gradient runs from blue to white, top to bottom (90°)",
        (near(l, BLUE, 12) && near(r, WHITE, 12)) || (near(r, BLUE, 12) && near(l, WHITE, 12)),
        "blue at one side, white at the other",
        format!("{l:?} … {r:?}"),
    )?;
    s.describe("Undo the switch");
    s.key("Cmd+Z")?;
    expect_pixel(s, "solid blue again", (395, 150), BLUE)?;

    // (With the Background picked, Properties shows no Gradient button
    // of its own under the menu on a narrow window.)
    s.describe("Pick the Background, then add a gradient fill layer above it");
    s.click("Layer Background")?;
    layer_menu(s, "New fill layer > Gradient")?;
    s.wait_idle()?;
    expect_layers(
        s,
        "a Gradient Fill right above the Background",
        &["Color Fill", "Gradient Fill", "Background"],
    )?;

    layer_menu(s, "New fill layer > Pattern...")?;
    s.wait_idle()?;
    s.dump_tree("pattern picker");
    s.screenshot("pattern-picker");
    let first = s
        .tree()
        .nodes
        .iter()
        .find(|n| n.name.starts_with("Pattern "))
        .map(|n| n.name.clone());
    s.check(
        "the picker lists patterns",
        first.is_some(),
        "a “Pattern …” button",
        format!("{first:?}"),
    )?;
    if let Some(p) = first {
        s.describe("Pick the first pattern");
        s.click(&p)?;
        s.wait_idle()?;
    }
    expect_layers(
        s,
        "a Pattern Fill above the gradient",
        &["Color Fill", "Pattern Fill", "Gradient Fill", "Background"],
    )?;

    s.describe("Rasterize the pattern fill from its right-click menu");
    s.right_click("Layer Pattern Fill")?;
    s.click("Rasterize")?;
    let pixel = s.doc(|d| find_layer(d, "Pattern Fill").map(|l| l.pixels().is_some()))?;
    s.check_eq("it is a pixel layer now", pixel, Some(true))?;
    Ok(())
}

/// A 60 × 40 red PNG in the session's files.
fn write_red(path: &std::path::Path) -> UiResult {
    let img = image::RgbaImage::from_pixel(60, 40, image::Rgba([220, 30, 30, 255]));
    img.save(path)
        .map_err(|e| UiError(format!("writing {}: {e}", path.display())))
}

fn smart_objects(s: &mut Session) -> UiResult {
    new_doc(s, 400, 300)?;
    s.describe("Add a layer with a blue block");
    s.click("New layer")?;
    fill_rect(s, (100.0, 100.0), (200.0, 160.0))?;

    layer_menu(s, "Convert to smart object")?;
    let smart = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.smart_layer().is_some()))?;
    s.check_eq("Layer 2 is a smart object", smart, Some(true))?;
    s.expect_text("Smart")?;

    for (pct, label) in [
        ("50", "Shrink it to half with Free Transform (Cmd+T)"),
        ("200", "Grow it back to full size"),
    ] {
        s.describe(label);
        s.key("Cmd+T")?;
        s.set_field("Width", pct)?;
        s.set_field("Height", pct)?;
        s.key("Enter")?;
        s.wait_idle()?;
    }
    let edge = (
        raster_px(s, "Layer 2", 100, 130)?,
        raster_px(s, "Layer 2", 99, 130)?.map(|p| p[3]),
        raster_px(s, "Layer 2", 199, 159)?,
    );
    s.check_eq(
        "after half and double the block is crisp, as it started",
        edge,
        (Some(BLUE), Some(0), Some(BLUE)),
    )?;

    layer_menu(s, "Edit smart object contents")?;
    s.wait_idle()?;
    let size = s.doc(|d| (d.width, d.height))?;
    s.check_eq("a tab with just the block's 100 × 60 pixels", size, (100, 60))?;
    s.describe("Make the contents white: swap colours, Select All, fill");
    s.click("Swap colours")?;
    s.key("Cmd+A")?;
    s.menu("Edit > Fill with brush colour")?;
    s.describe("Save with Cmd+S to update the smart object");
    s.key("Cmd+S")?;
    s.expect_text("Updated smart object")?;
    s.describe("Close the contents tab");
    s.menu("File > Close document")?;
    s.wait_idle()?;
    let px = raster_px(s, "Layer 2", 150, 130)?;
    s.check_eq("the smart object shows the new contents", px, Some(WHITE))?;

    let red = s.files().join("red.png");
    write_red(&red)?;
    layer_menu(s, "Replace smart object contents...")?;
    s.choose_file(&red)?;
    s.wait_idle()?;
    let px = raster_px(s, "Layer 2", 130, 120)?;
    s.check_eq("the red image took its place", px, Some([220, 30, 30, 255]))?;

    layer_menu(s, "Rasterize")?;
    let pixel = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.pixels().is_some()))?;
    s.check_eq("Layer 2 is plain pixels now", pixel, Some(true))?;
    s.describe("Undo the rasterize");
    s.key("Cmd+Z")?;
    let smart = s.doc(|d| find_layer(d, "Layer 2").map(|l| l.smart_layer().is_some()))?;
    s.check_eq("a smart object again", smart, Some(true))?;
    Ok(())
}

/// A layer's rendered pixel (smart objects, fills and shapes included),
/// straight sRGB 0 to 255.
fn raster_px(s: &mut Session, layer: &str, x: i32, y: i32) -> UiResult<Option<[u8; 4]>> {
    let layer = layer.to_string();
    s.doc(move |d| {
        let p = find_layer(d, &layer)?.raster_store()?.get_pixel(x, y);
        let [r, g, b, a] = p.to_straight();
        Some([
            lumenply_io::linear_to_srgb(r),
            lumenply_io::linear_to_srgb(g),
            lumenply_io::linear_to_srgb(b),
            (a.clamp(0.0, 1.0) * 255.0).round() as u8,
        ])
    })
}

/// Left edge of a layer's painted pixels.
fn left_of(s: &mut Session, name: &str) -> UiResult<Option<i32>> {
    let name = name.to_string();
    s.doc(move |d| {
        find_layer(d, &name)
            .and_then(|l| l.raster_store())
            .and_then(|r| r.content_bounds())
            .map(|b| b.x)
    })
}

fn align_filter_menu(s: &mut Session) -> UiResult {
    new_doc(s, 600, 400)?;
    for (i, (x, y)) in [(50.0, 50.0), (200.0, 150.0), (420.0, 250.0)]
        .into_iter()
        .enumerate()
    {
        s.describe(&format!("Add layer {} with a 60 px block", i + 2));
        s.click("New layer")?;
        fill_rect(s, (x, y), (x + 60.0, y + 60.0))?;
    }
    s.describe("Pick Layer 2 and Cmd-click Layers 3 and 4");
    s.click("Layer Layer 2")?;
    s.click_with("Layer Layer 3", "Cmd")?;
    s.click_with("Layer Layer 4", "Cmd")?;
    layer_menu(s, "Align > Left edges")?;
    let xs = (
        left_of(s, "Layer 2")?,
        left_of(s, "Layer 3")?,
        left_of(s, "Layer 4")?,
    );
    s.check_eq("all three start at x = 50", xs, (Some(50), Some(50), Some(50)))?;
    s.describe("Undo the align");
    s.key("Cmd+Z")?;
    layer_menu(s, "Distribute > Horizontal centres")?;
    let xs = (
        left_of(s, "Layer 2")?,
        left_of(s, "Layer 3")?,
        left_of(s, "Layer 4")?,
    );
    s.check_eq(
        "evenly spread: 50, 235, 420",
        xs,
        (Some(50), Some(235), Some(420)),
    )?;

    s.describe("Type “3” into Filter layers");
    s.set_field("Filter layers by name", "3")?;
    let only = s.has_node("Layer Layer 3") && !s.has_node("Layer Layer 2") && !s.has_node("Layer Background");
    s.check(
        "only Layer 3 is listed",
        only,
        "the Layer 3 row alone",
        format!("{:?}", s.texts_matching("Layer ")),
    )?;
    s.describe("Clear the filter");
    s.click("Filter layers by name")?;
    s.key("Cmd+A")?;
    s.key("Backspace")?;
    s.key("Enter")?;
    s.expect_node("Layer Background")?;

    s.describe("Right-click Layer 3 for its context menu");
    s.right_click("Layer Layer 3")?;
    s.screenshot("context-menu");
    for item in [
        "Rename",
        "Duplicate layer",
        "Merge down",
        "Add mask",
        "Convert to smart object",
        "Delete layer",
    ] {
        s.expect_node(item)?;
    }
    s.describe("Pick Duplicate layer");
    s.click("Duplicate layer")?;
    let t = s.layer_names()?;
    s.check_eq(
        "Layer 3 copy above Layer 3",
        t.get(1).cloned(),
        Some("Layer 3 copy".into()),
    )?;

    let thumbs = s.app(|a| {
        let mut ids = Vec::new();
        a.editor.doc().for_each_layer(|l| ids.push(l.id));
        ids.iter().filter(|id| a.thumbs.contains_key(id)).count() == ids.len()
    })?;
    s.check_eq("every row has a thumbnail", thumbs, true)?;
    s.screenshot("thumbnails");
    Ok(())
}
