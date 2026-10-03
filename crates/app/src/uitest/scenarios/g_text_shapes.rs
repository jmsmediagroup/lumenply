//! Text, shapes and paths: the Text tool (typing, paragraph boxes,
//! selecting, styling, committing and editing again), the Shape tool
//! (every kind, fill and stroke, Properties, transforming, rasterizing)
//! and the Pen with the Paths panel.

use crate::uitest::prelude::*;
use lumenply_doc::shape::{CustomShape, ShapeGeometry, ShapeLayer, StrokeAlign};
use lumenply_doc::{Document, Fill, LayerContent, PathNode, TextLayer, VectorPath};

scenario_list! {
    "text-click-to-type" => text_click_to_type: "Click with the Text tool, type, edit with the keyboard, commit, undo",
    "text-paragraph-box" => text_paragraph_box: "Drag a text box, type until it wraps, justify, resize the box",
    "text-style-a-word" => text_style_a_word: "Select one word on the canvas and give it its own size, weight and colour",
    "text-font-and-align" => text_font_and_align: "Pick a font by searching, centre the text, change its size",
    "text-cancel-and-edit-again" => text_cancel_and_edit_again: "Cancel an edit, edit again with a click and with Enter, move the text",
    "text-properties" => text_properties: "Edit text in Properties: content, leading, paragraph, rename, rasterize",
    "text-keyboard-styles" => text_keyboard_styles: "Select with the keyboard and the mouse, style with Photoshop's Cmd+Shift keys",
    "find-tools-in-the-palette" => find_tools_in_the_palette: "Find the Text and Shape tools in Cmd+K by Photoshop's words",
    "shape-every-kind" => shape_every_kind: "Draw every kind of shape, a Shift-constrained square and an Alt-centred circle",
    "shape-fill-and-stroke" => shape_fill_and_stroke: "Shapes without fill, with a stroke, a gradient fill and picked colours",
    "shape-edit-in-properties" => shape_edit_in_properties: "Edit a shape's corners, stroke, dashes, fill and sides in Properties",
    "shape-transform-and-rasterize" => shape_transform_and_rasterize: "Free-transform a shape, then rasterize it",
    "pen-draw-a-path" => pen_draw_a_path: "Draw corners and a curve with the Pen, close the path, undo",
    "pen-edit-anchors" => pen_edit_anchors: "Move an anchor, pull a handle, delete an anchor",
    "paths-panel" => paths_panel: "Save, rename, fill, stroke and load a path; a work path from a selection",
    "path-to-shape-and-back" => path_to_shape_and_back: "A shape layer from the work path, and a work path from a shape",
}

// ---- helpers ------------------------------------------------------------------------------

/// File ▸ New at `w` × `h` (white background).
fn new_doc(s: &mut Session, w: u32, h: u32) -> UiResult {
    s.describe(&format!("Make a new {w} × {h} image from File ▸ New"));
    s.menu("File > New...")?;
    s.set_field("Width", &w.to_string())?;
    s.set_field("Height", &h.to_string())?;
    s.click("Create")?;
    s.wait_idle()
}

/// The text layer called `name`, or else the one holding the text `name`,
/// without its derived raster cache.
fn text_layer(d: &Document, name: &str) -> Option<TextLayer> {
    let t = match find_layer(d, name).map(|l| &l.content) {
        Some(LayerContent::Text(t)) => t,
        _ => d
            .layers()
            .iter()
            .rev()
            .find_map(|l| l.text_layer().filter(|t| t.text == name))?,
    };
    Some(TextLayer {
        cache: None,
        ..t.clone()
    })
}

/// Every text layer, top first: (layer name, text).
fn texts(d: &Document) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for l in d.layers().iter().rev() {
        if let LayerContent::Text(t) = &l.content {
            out.push((l.name.clone(), t.text.clone()));
        }
    }
    out
}

/// The tool options bar's control called `name` (Properties repeats many
/// of them under the same names): the first of `names` found there.
fn bar_node(s: &mut Session, names: &[&str]) -> UiResult<(String, Pos2)> {
    // The bar is named only while it overflows; else it is the strip
    // under the menu bar.
    let band = match s.node("Tool options") {
        Some(n) => n.rect,
        None => eframe::egui::Rect::from_min_max(pos2(0.0, 40.0), pos2(10_000.0, 84.0)),
    };
    for (i, name) in names.iter().enumerate() {
        let hit = s
            .tree()
            .matches(name, None)
            .into_iter()
            .find(|n| band.contains(n.rect.center()))
            .map(|n| (n.name.clone(), n.rect.center()));
        if let Some(hit) = hit {
            if i > 0 {
                s.note(&format!(
                    "“{}” is not in the options bar; found it as “{name}”",
                    names[0]
                ))?;
            }
            return Ok(hit);
        }
    }
    Err(UiError(format!("no “{}” in the options bar", names[0])))
}

/// Click a control in the options bar, trying `names` in turn.
fn bar_click(s: &mut Session, names: &[&str]) -> UiResult {
    let (name, at) = bar_node(s, names)?;
    s.click_at(at, &name)
}

/// Type `text` into a number field of the options bar, then Enter.
fn bar_field(s: &mut Session, name: &str, text: &str) -> UiResult {
    bar_click(s, &[name])?;
    s.key("Cmd+A")?;
    s.type_text(text)?;
    s.key("Enter")
}

/// Scroll the Properties panel until `name` shows in it, then run `f`
/// looking only there. A short window leaves Properties only a few rows.
fn props(s: &mut Session, name: &str, f: impl FnOnce(&mut Session) -> UiResult) -> UiResult {
    for _ in 0..12 {
        let Some(area) = s.node("Properties") else {
            break;
        };
        let target = s
            .tree()
            .matches(name, None)
            .into_iter()
            .filter(|n| area.rect.x_range().contains(n.rect.center().x))
            .map(|n| n.rect)
            .next_back();
        let Some(r) = target else { break };
        if area.rect.contains_rect(r) {
            break;
        }
        let dy = (r.center().y - area.rect.center().y).clamp(-240.0, 240.0);
        s.describe(&format!("Scroll Properties to “{name}”"));
        s.scroll("Properties", dy)?;
    }
    s.within("Properties", f)
}

/// View ▸ Snap off, so drags land exactly where they are aimed.
fn snap_off(s: &mut Session) -> UiResult {
    let on = s.app(|a| a.prefs.snap)?;
    if on {
        s.describe("Turn snapping off: View ▸ Snap");
        s.menu("View > Snap")?;
    }
    let on = s.app(|a| a.prefs.snap)?;
    s.check_eq("snapping is off", on, false)?;
    Ok(())
}

/// Click the first of `names` that is on screen (a control's better name
/// first, then what it was called before), noting a fallback.
fn click_first(s: &mut Session, names: &[&str]) -> UiResult {
    for (i, n) in names.iter().enumerate() {
        if s.has_node(n) {
            if i > 0 {
                s.note(&format!("“{}” is not on screen; found it as “{n}”", names[0]))?;
            }
            return s.click(n);
        }
    }
    s.click(names[0])
}

fn text_of(s: &mut Session, name: &str) -> UiResult<Option<TextLayer>> {
    let name = name.to_string();
    s.doc(move |d| text_layer(d, &name))
}

// ---- text ---------------------------------------------------------------------------------

fn text_click_to_type(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    s.describe("Pick the Text tool from the toolbar");
    s.click_role(Role::Button, "Text")?;
    s.dump_tree("text-tool");
    s.describe("Click on the canvas where the text should start");
    s.canvas_click((100.0, 200.0), "")?;
    s.describe("Type a line; the tool letters V, B and T type as letters");
    s.type_text("Vote Big Today")?;
    let t = text_of(s, "Vote Big Today")?;
    s.check(
        "a text layer holds what was typed",
        t.as_ref()
            .is_some_and(|t| t.text == "Vote Big Today" && t.x == 100.0 && t.y == 200.0),
        "\"Vote Big Today\" anchored at (100, 200)",
        format!("{:?}", t.as_ref().map(|t| (&t.text, t.x, t.y))),
    )?;
    let tool = s.app(|a| a.tool.name())?;
    s.check_eq("typing V, B and T did not switch tools", tool, "Text")?;
    s.screenshot("typing");

    s.describe("Backspace the last word and its space");
    for _ in 0..6 {
        s.key("Backspace")?;
    }
    s.describe("Move the caret to the start with Cmd+Left and type there");
    s.key("Cmd+Left")?;
    s.type_text("Go ")?;
    s.describe("Select the next word with Shift+Alt+Right and type over it");
    s.key("Alt+Right")?;
    s.key("Shift+Alt+Right")?;
    s.type_text(" Large")?;
    let doc_texts = s.doc(texts)?;
    s.check_eq(
        "the text reads as edited",
        doc_texts,
        vec![("Go Vote Large".to_string(), "Go Vote Large".to_string())],
    )?;
    let steps_before = s.history()?;
    s.describe("Press Esc to commit");
    s.key("Esc")?;
    let editing = s.app(|a| a.text_editing())?;
    s.check_eq("the edit is committed", editing, false)?;
    s.expect_text("Text committed")?;
    let steps = s.history()?;
    s.note(&format!("History: {}", steps.join(" → ")))?;
    s.check_eq(
        "the typing session is one history step",
        steps.len(),
        steps_before.len(),
    )?;
    s.describe("Press V now: the shortcut picks the Move tool again");
    s.key("V")?;
    let tool = s.app(|a| a.tool.name())?;
    s.check_eq("V switches to Move after committing", tool, "Move")?;
    s.describe("Undo with Cmd+Z");
    s.key("Cmd+Z")?;
    let doc_texts = s.doc(texts)?;
    s.check_eq("undo removes the new text in one step", doc_texts, vec![])?;
    s.describe("Redo with Cmd+Shift+Z");
    s.key("Cmd+Shift+Z")?;
    let doc_texts = s.doc(texts)?;
    s.check_eq(
        "redo brings it back",
        doc_texts,
        vec![("Go Vote Large".to_string(), "Go Vote Large".to_string())],
    )?;
    let steps = s.history()?.len();
    s.describe("Pick the Text tool again with T");
    s.key("T")?;
    s.describe("Click empty canvas, then change your mind with Esc");
    s.canvas_click((400.0, 400.0), "")?;
    s.key("Esc")?;
    let n = s.doc(texts)?.len();
    s.check_eq("empty new text leaves no layer", n, 1)?;
    let after = s.history()?.len();
    s.check_eq("and no history step", after, steps)?;
    Ok(())
}

/// A document point in the middle of character `i` of `t`.
fn char_point(t: &TextLayer, i: usize) -> (f32, f32) {
    let lay = lumenply_render::text_layout::layout(t);
    let a = lay.caret(i);
    let b = lay.caret(lumenply_render::text_layout::next_char(&t.text, i));
    ((a.x + b.x) / 2.0, (a.top + a.bottom) / 2.0)
}

/// The canvas selection of the open text session, if any.
fn text_selection(s: &mut Session) -> UiResult<Option<String>> {
    s.app(|a| a.typer.session.as_ref().map(|t| t.buf.selected().to_string()))
}

/// New point text typed at (x, y) and committed with Esc.
fn type_new_text(s: &mut Session, at: (f32, f32), text: &str) -> UiResult {
    s.describe("Pick the Text tool from the toolbar");
    s.click_role(Role::Button, "Text")?;
    s.describe("Click on the canvas where the text starts");
    s.canvas_click(at, "")?;
    s.type_text(text)?;
    s.describe("Commit with Esc");
    s.key("Esc")
}

fn text_paragraph_box(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    s.describe("Pick the Text tool from the toolbar");
    s.click_role(Role::Button, "Text")?;
    s.describe("Drag a text box on the canvas");
    s.canvas_drag((100.0, 100.0), (400.0, 300.0), 12, "")?;
    let words = "The quick brown fox jumps over the lazy dog";
    s.type_text(words)?;
    let t = text_of(s, words)?;
    let lines = t
        .as_ref()
        .map(|t| lumenply_render::text_layout::layout(t).lines.len())
        .unwrap_or(0);
    s.check(
        "the text sits in a 300 × 200 box at (100, 100)",
        t.as_ref()
            .is_some_and(|t| t.box_size == Some([300.0, 200.0]) && (t.x, t.y) == (100.0, 100.0)),
        "box [300, 200] at (100, 100)",
        format!("{:?}", t.as_ref().map(|t| (t.box_size, t.x, t.y))),
    )?;
    s.check(
        "the words wrap onto several lines",
        lines >= 2,
        "2 or more lines",
        lines.to_string(),
    )?;
    s.describe("Justify it from the options bar");
    bar_click(s, &["Justify (last line left)"])?;
    let align = text_of(s, words)?.map(|t| t.align);
    s.check_eq(
        "the paragraph is justified",
        align,
        Some(lumenply_doc::TextAlign::Justify),
    )?;
    s.describe("Drag the box's right handle 100 px to the right");
    s.canvas_drag((400.0, 200.0), (500.0, 200.0), 10, "")?;
    let t = text_of(s, words)?;
    s.check_eq(
        "the box is 400 px wide now, same place",
        t.as_ref()
            .map(|t| (t.box_size.map(|b| b.map(f32::round)), t.x, t.y)),
        Some((Some([400.0, 200.0]), 100.0, 100.0)),
    )?;
    s.describe("Click outside the box to commit");
    s.canvas_click((650.0, 420.0), "")?;
    let editing = s.app(|a| a.text_editing())?;
    s.check_eq("clicking outside commits", editing, false)?;
    let n = s.doc(texts)?.len();
    s.check_eq("and starts no second text", n, 1)?;
    let hist = s.history()?;
    s.note(&format!("History: {}", hist.join(" → ")))?;
    s.describe("Undo until the text is gone");
    let mut undos = 0;
    while !s.doc(texts)?.is_empty() && undos < 6 {
        s.key("Cmd+Z")?;
        undos += 1;
    }
    s.check_eq("the whole box edit undoes in one step", undos, 1)?;
    Ok(())
}

fn text_style_a_word(s: &mut Session) -> UiResult {
    new_doc(s, 900, 400)?;
    type_new_text(s, (60.0, 200.0), "Hello brave world")?;
    let t = text_of(s, "Hello brave world")?.ok_or(UiError("no text layer".into()))?;
    let base_size = t.size;
    let word = char_point(&t, 8);
    s.describe("Double-click the word “brave” to select it");
    s.canvas_multi_click(word, 2, "")?;
    let sel = text_selection(s)?;
    s.check_eq("the word is selected", sel, Some("brave".to_string()))?;
    s.describe("Make it bold in the options bar");
    bar_click(s, &["Bold"])?;
    s.describe("Type a size of 60 px in the options bar");
    bar_field(s, "Font size", "60")?;
    let sel = text_selection(s)?;
    s.check_eq("the word is still selected", sel, Some("brave".to_string()))?;
    s.describe("Make it red with the colour swatch");
    bar_click(s, &["Text colour*", "#1A2E8C"])?;
    s.set_field("Hex colour", "#FF0000")?;
    s.describe("Close the picker with Esc");
    s.key("Esc")?;
    let editing = s.app(|a| a.text_editing())?;
    s.check_eq("Esc closed the picker and the edit goes on", editing, true)?;
    let t = text_of(s, "Hello brave world")?.ok_or(UiError("no text layer".into()))?;
    let (first, word_st) = (t.style_at(0), t.style_at(8));
    s.check(
        "only “brave” is bold, 60 px and red",
        word_st.bold && word_st.size == 60.0 && word_st.color[0] > 0.99 && word_st.color[1] < 0.01,
        "bold, 60 px, red",
        format!("{word_st:?}"),
    )?;
    s.check(
        "the rest keeps the layer's style",
        !first.bold && first.size == base_size && first.color[0] < 0.5 && !t.style_at(14).bold,
        format!("regular, {base_size} px"),
        format!("{first:?}"),
    )?;
    s.describe("Select everything with Cmd+A");
    s.key("Cmd+A")?;
    s.describe("Grow it all by 2 px with Cmd+Shift+>");
    s.key("Cmd+Shift+Period")?;
    let t = text_of(s, "Hello brave world")?.ok_or(UiError("no text layer".into()))?;
    s.check_eq(
        "every character grew from its first character's size",
        (t.style_at(0).size, t.style_at(8).size),
        (base_size + 2.0, base_size + 2.0),
    )?;
    s.key("Esc")?;
    let hist = s.history()?;
    s.note(&format!("History: {}", hist.join(" → ")))?;
    s.describe("Undo the editing session");
    s.key("Cmd+Z")?;
    let t = text_of(s, "Hello brave world")?.ok_or(UiError("no text layer".into()))?;
    s.check_eq(
        "undo takes back the whole editing session: sizes, bold and colour",
        (
            t.style_at(0).size,
            t.style_at(8).size,
            t.style_at(8).bold,
            t.runs.len(),
        ),
        (base_size, base_size, false, 0),
    )?;
    Ok(())
}

fn text_font_and_align(s: &mut Session) -> UiResult {
    new_doc(s, 800, 400)?;
    type_new_text(s, (400.0, 200.0), "Centred")?;
    s.describe("Open the font picker in the options bar");
    bar_click(s, &["Font family*", "Font *"])?;
    s.describe("Search for Georgia");
    s.type_text("Georgia")?;
    s.describe("Pick the first match with Enter");
    s.key("Enter")?;
    let t = text_of(s, "Centred")?;
    s.check_eq(
        "the text uses Georgia",
        t.as_ref().map(|t| t.font.clone()),
        Some("Georgia".to_string()),
    )?;
    let before = t.as_ref().map(|t| t.size).unwrap_or(0.0);
    s.describe("Centre it");
    bar_click(s, &["Align centre"])?;
    let t = text_of(s, "Centred")?.ok_or(UiError("no text layer".into()))?;
    let bounds = s.doc(|d| {
        find_layer(d, "Centred")
            .and_then(|l| l.raster_store()?.content_bounds())
            .map(|b| (b.x, b.right()))
    })?;
    s.check(
        "the anchor is the middle of the line now",
        t.align == lumenply_doc::TextAlign::Center
            && t.x == 400.0
            && bounds.is_some_and(|(l, r)| ((l + r) / 2 - 400).abs() <= 3),
        "centred on x = 400",
        format!("{:?} at {}, glyphs {bounds:?}", t.align, t.x),
    )?;
    s.describe("Type a size of 96 px");
    bar_field(s, "Font size", "96")?;
    let size = text_of(s, "Centred")?.map(|t| t.size);
    s.check_eq("the whole text is 96 px", size, Some(96.0))?;
    let hist = s.history()?;
    s.note(&format!("History: {}", hist.join(" → ")))?;
    s.describe("Undo the size and the alignment");
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    let t = text_of(s, "Centred")?;
    s.check_eq(
        "back to left-aligned Georgia",
        t.map(|t| (t.align, t.font, t.size)),
        Some((lumenply_doc::TextAlign::Left, "Georgia".to_string(), before)),
    )?;
    Ok(())
}

fn text_cancel_and_edit_again(s: &mut Session) -> UiResult {
    new_doc(s, 800, 400)?;
    type_new_text(s, (100.0, 200.0), "Hello")?;
    let t = text_of(s, "Hello")?.ok_or(UiError("no text layer".into()))?;
    let steps = s.history()?.len();
    s.describe("Click inside the word, after “He”");
    let he = {
        let lay = lumenply_render::text_layout::layout(&t);
        let c = lay.caret(2);
        (c.x, (c.top + c.bottom) / 2.0)
    };
    s.canvas_click(he, "")?;
    s.type_text("XX")?;
    let now = text_of(s, "HeXXllo")?.map(|t| t.text);
    s.check_eq("typing lands at the click", now, Some("HeXXllo".to_string()))?;
    s.describe("Cancel the edit from the options bar");
    bar_click(s, &["Cancel"])?;
    let back = s.doc(texts)?;
    s.check_eq(
        "the text is back as it was",
        back,
        vec![("Hello".to_string(), "Hello".to_string())],
    )?;
    let n = s.history()?.len();
    s.check_eq("cancel leaves no history step", n, steps)?;

    s.describe("Press Enter to edit the active text again, caret at the end");
    s.key("Enter")?;
    s.type_text(" again")?;
    s.describe("Cmd-drag the text 100 px right and 50 px down while editing");
    s.canvas_drag((130.0, 190.0), (230.0, 240.0), 12, "Cmd")?;
    let t = text_of(s, "Hello again")?;
    s.check_eq(
        "the text moved with its anchor",
        t.as_ref().map(|t| (t.text.clone(), t.x, t.y)),
        Some(("Hello again".to_string(), 200.0, 250.0)),
    )?;
    s.describe("Commit with Cmd+Enter");
    s.key("Cmd+Enter")?;
    s.describe("Pick the Move tool and drag the text back left");
    s.click_role(Role::Button, "Move")?;
    s.canvas_drag((230.0, 240.0), (130.0, 240.0), 12, "")?;
    let t = text_of(s, "Hello again")?;
    // Smart Guides may snap the glyphs a few pixels vertically.
    s.check(
        "the Move tool moves the text layer 100 px left",
        t.as_ref()
            .is_some_and(|t| t.x == 100.0 && (t.y - 250.0).abs() <= 6.0),
        "x 100, y 250 (± a snap)",
        format!("{:?}", t.as_ref().map(|t| (t.x, t.y))),
    )?;
    s.describe("Undo the move");
    s.key("Cmd+Z")?;
    let t = text_of(s, "Hello again")?;
    s.check_eq(
        "the text is back where the Cmd-drag left it",
        t.as_ref().map(|t| (t.x, t.y)),
        Some((200.0, 250.0)),
    )?;
    Ok(())
}

fn text_properties(s: &mut Session) -> UiResult {
    new_doc(s, 800, 400)?;
    type_new_text(s, (100.0, 150.0), "First line")?;
    s.describe("Pick the Move tool so the text is not being edited");
    s.click_role(Role::Button, "Move")?;
    s.describe("Replace the text in Properties");
    props(s, "Text", |s| s.click_role(Role::MultilineTextInput, "Text"))?;
    s.key("Cmd+A")?;
    s.type_text("One\ntwo")?;
    let t0 = text_of(s, "One")?;
    s.check_eq(
        "the layer holds both lines, named after the first",
        t0.as_ref().map(|t| t.text.clone()),
        Some("One\ntwo".to_string()),
    )?;
    let size = t0.as_ref().map(|t| t.size).unwrap_or(0.0);
    s.describe("Drag Leading to its end to space the lines");
    props(s, "Leading", |s| s.drag_slider("Leading", 1.0))?;
    let lh = text_of(s, "One")?.map(|t| t.line_height);
    s.check(
        "leading went up to 4× the size",
        lh.is_some_and(|v| (v - 4.0).abs() < 0.05),
        "line height 4.0",
        format!("{lh:?} at {size} px"),
    )?;
    s.describe("Turn it into paragraph text");
    props(s, "Paragraph", |s| s.click("Paragraph"))?;
    let b = text_of(s, "One")?.and_then(|t| t.box_size);
    s.check("it has a box now", b.is_some(), "a box size", format!("{b:?}"))?;

    s.describe("Rename the layer by double-clicking it");
    s.double_click("Layer One")?;
    s.set_field("Layer name", "Heading")?;
    s.describe("Make it bold in Properties");
    props(s, "Bold", |s| s.click("Bold"))?;
    let names = s.layer_names()?;
    s.check(
        "the layer keeps the name it was given",
        names.first().map(String::as_str) == Some("Heading"),
        "Heading on top",
        format!("{names:?}"),
    )?;
    let bold = s.doc(|d| d.layers().iter().find_map(|l| l.text_layer().map(|t| t.bold)))?;
    s.check_eq("and is bold", bold, Some(true))?;

    s.describe("Right-click the layer and pick Rasterize");
    s.right_click("Layer Heading")?;
    s.click("Rasterize")?;
    let kind = s.doc(|d| {
        d.layers()
            .last()
            .map(|l| matches!(l.content, LayerContent::Pixel(_)))
    })?;
    s.check_eq("the top layer is pixels now", kind, Some(true))?;
    s.describe("Undo the rasterize");
    s.key("Cmd+Z")?;
    let kind = s.doc(|d| d.layers().last().map(|l| l.text_layer().is_some()))?;
    s.check_eq("it is text again", kind, Some(true))?;
    Ok(())
}

fn text_keyboard_styles(s: &mut Session) -> UiResult {
    new_doc(s, 900, 400)?;
    s.describe("Pick the Text tool and click on the canvas");
    s.click_role(Role::Button, "Text")?;
    s.canvas_click((60.0, 200.0), "")?;
    s.type_text("Make it bold")?;
    s.describe("Select the last word with Shift+Alt+Left");
    s.key("Shift+Alt+Left")?;
    let sel = text_selection(s)?;
    s.check_eq("the last word is selected", sel, Some("bold".to_string()))?;
    s.describe("Bold it with Cmd+Shift+B, underline it with Cmd+Shift+U");
    s.key("Cmd+Shift+B")?;
    s.key("Cmd+Shift+U")?;
    let t = text_of(s, "Make it bold")?.ok_or(UiError("no text layer".into()))?;
    let (word, rest) = (t.style_at(9), t.style_at(0));
    s.check(
        "only the word is bold and underlined",
        word.bold && word.underline && !rest.bold && !rest.underline,
        "bold + underline on “bold” only",
        format!("word {word:?}, rest {rest:?}"),
    )?;
    s.describe("Triple-click the line to select all of it");
    let t = text_of(s, "Make it bold")?.ok_or(UiError("no text layer".into()))?;
    s.canvas_multi_click(char_point(&t, 2), 3, "")?;
    let sel = text_selection(s)?;
    s.check_eq(
        "the whole line is selected",
        sel,
        Some("Make it bold".to_string()),
    )?;
    s.describe("Italicise it all with Cmd+Shift+I");
    s.key("Cmd+Shift+I")?;
    let t = text_of(s, "Make it bold")?.ok_or(UiError("no text layer".into()))?;
    s.check(
        "every letter is italic",
        (0..t.text.len()).all(|i| t.style_at(i).italic),
        "italic throughout",
        format!("{:?}", t.runs),
    )?;
    let text = t.text.clone();
    s.check_eq(
        "no letters were typed by the shortcuts",
        text,
        "Make it bold".to_string(),
    )?;
    s.describe("Right-align with Cmd+Shift+R: the anchor becomes the right end of the line");
    s.key("Cmd+Shift+R")?;
    let align = text_of(s, "Make it bold")?.map(|t| t.align);
    s.check_eq("right-aligned", align, Some(lumenply_doc::TextAlign::Right))?;
    s.describe("Commit with Esc");
    s.key("Esc")?;
    let hist = s.history()?;
    s.check_eq(
        "the whole edit is one history step",
        hist,
        vec!["Add text".to_string()],
    )?;
    Ok(())
}

fn find_tools_in_the_palette(s: &mut Session) -> UiResult {
    new_doc(s, 600, 400)?;
    for (word, tool) in [("type", "Text"), ("rectangle", "Shape"), ("ellipse", "Shape")] {
        s.describe(&format!("Open the command palette and type “{word}”"));
        s.key("Cmd+K")?;
        s.type_text(word)?;
        s.describe("Take the first match with Enter");
        s.key("Enter")?;
        let got = s.app(|a| a.tool.name())?;
        s.check_eq(&format!("“{word}” finds the {tool} tool"), got, tool)?;
    }
    Ok(())
}

// ---- shapes -------------------------------------------------------------------------------

/// The top layer's shape (without its derived cache) and its name.
fn top_shape(d: &Document) -> Option<(String, ShapeLayer)> {
    let l = d.layers().last()?;
    let sh = l.shape_layer()?;
    Some((
        l.name.clone(),
        ShapeLayer {
            cache: None,
            ..sh.clone()
        },
    ))
}

fn shape_now(s: &mut Session) -> UiResult<Option<ShapeLayer>> {
    Ok(s.doc(top_shape)?.map(|(_, sh)| sh))
}

/// The active layer's shape, without its cache.
fn active_shape(s: &mut Session) -> UiResult<Option<ShapeLayer>> {
    s.app(|a| {
        a.active_layer()
            .and_then(|l| l.shape_layer())
            .map(|sh| ShapeLayer {
                cache: None,
                ..sh.clone()
            })
    })
}

/// Pick a kind in the Shape tool's options bar.
fn pick_kind(s: &mut Session, kind: &str) -> UiResult {
    s.describe(&format!("Pick “{kind}” in the shape kind list"));
    s.click("Shape kind")?;
    s.click(kind)
}

/// The geometry with every coordinate rounded to a whole pixel (the
/// pointer maps to fractions of a pixel a hair off the integers).
fn rounded(g: ShapeGeometry) -> ShapeGeometry {
    let r = |v: [f32; 4]| v.map(f32::round);
    match g {
        ShapeGeometry::Rectangle { rect, radius } => ShapeGeometry::Rectangle {
            rect: r(rect),
            radius,
        },
        ShapeGeometry::Ellipse { rect } => ShapeGeometry::Ellipse { rect: r(rect) },
        ShapeGeometry::Polygon { rect, sides } => ShapeGeometry::Polygon { rect: r(rect), sides },
        ShapeGeometry::Custom { rect, shape } => ShapeGeometry::Custom { rect: r(rect), shape },
        ShapeGeometry::Line {
            from,
            to,
            weight,
            arrow_start,
            arrow_end,
        } => ShapeGeometry::Line {
            from: from.map(f32::round),
            to: to.map(f32::round),
            weight,
            arrow_start,
            arrow_end,
        },
        other => other,
    }
}

/// Within `tol` of `want` on every colour channel.
fn near(got: [u8; 4], want: [u8; 3], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

fn shape_every_kind(s: &mut Session) -> UiResult {
    new_doc(s, 900, 700)?;
    snap_off(s)?;
    s.describe("Pick the Shape tool from the toolbar");
    s.click_role(Role::Button, "Shape")?;
    let kinds = [
        "Rectangle",
        "Rounded Rectangle",
        "Ellipse",
        "Polygon",
        "Star",
        "Arrow",
        "Heart",
        "Speech Bubble",
        // Last, so no later drag snaps to its stroke's edge.
        "Line",
    ];
    for (i, kind) in kinds.iter().enumerate() {
        let (cx, cy) = ((i % 3) as f32 * 300.0, (i / 3) as f32 * 200.0);
        let (a, b) = ((cx + 50.0, cy + 40.0), (cx + 250.0, cy + 160.0));
        pick_kind(s, kind)?;
        s.describe(&format!("Drag a {kind}"));
        s.canvas_drag(a, b, 10, "")?;
        let got = shape_now(s)?.map(|sh| rounded(sh.geometry));
        let rect = [a.0, a.1, 200.0, 120.0];
        let want = match *kind {
            "Rectangle" => ShapeGeometry::Rectangle { rect, radius: 0.0 },
            "Rounded Rectangle" => ShapeGeometry::Rectangle { rect, radius: 20.0 },
            "Ellipse" => ShapeGeometry::Ellipse { rect },
            "Polygon" => ShapeGeometry::Polygon { rect, sides: 5 },
            "Line" => ShapeGeometry::Line {
                from: [a.0, a.1],
                to: [b.0, b.1],
                weight: 4.0,
                arrow_start: false,
                arrow_end: false,
            },
            other => ShapeGeometry::Custom {
                rect,
                shape: CustomShape::ALL
                    .into_iter()
                    .find(|c| c.name() == other)
                    .unwrap_or(CustomShape::Star),
            },
        };
        s.check_eq(&format!("the {kind} is where it was dragged"), got, Some(want))?;
    }
    let blue = [0x3d, 0x85, 0xeb];
    let inside = s.pixel(450, 100)?;
    s.check(
        "the ellipse is filled with the fill colour",
        near(inside, blue, 2),
        format!("{blue:?}"),
        format!("{inside:?}"),
    )?;
    let n = s.layer_names()?.len();
    s.check_eq("nine shape layers over the background", n, 10)?;

    pick_kind(s, "Rectangle")?;
    s.describe("Shift-drag a square");
    s.canvas_drag((380.0, 625.0), (440.0, 655.0), 10, "Shift")?;
    let got = shape_now(s)?.map(|sh| rounded(sh.geometry));
    s.check_eq(
        "Shift makes it square",
        got,
        Some(ShapeGeometry::Rectangle {
            rect: [380.0, 625.0, 60.0, 60.0],
            radius: 0.0,
        }),
    )?;
    pick_kind(s, "Ellipse")?;
    s.describe("Alt-drag a circle from its centre with Shift too");
    s.canvas_drag((700.0, 640.0), (730.0, 650.0), 10, "Shift+Alt")?;
    let got = shape_now(s)?.map(|sh| rounded(sh.geometry));
    s.check_eq(
        "Shift+Alt draws a circle round the press point",
        got,
        Some(ShapeGeometry::Ellipse {
            rect: [670.0, 610.0, 60.0, 60.0],
        }),
    )?;
    s.describe("Undo the circle");
    s.key("Cmd+Z")?;
    let n = s.layer_names()?.len();
    s.check_eq("undo removes one shape", n, 11)?;
    Ok(())
}

fn shape_fill_and_stroke(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    s.describe("Pick the Shape tool from the toolbar");
    s.click_role(Role::Button, "Shape")?;
    s.describe("No fill: pick None in the fill list");
    s.click("Shape fill")?;
    s.click("None")?;
    s.describe("Turn the stroke on");
    s.click("Stroke")?;
    s.set_field("Width", "10")?;
    let in_bar = s.has_node("Stroke alignment");
    if in_bar {
        s.describe("Centre the stroke on the outline");
        s.click("Stroke alignment")?;
        s.click("Center")?;
    }
    s.describe("Drag a rectangle");
    s.canvas_drag((100.0, 100.0), (300.0, 250.0), 10, "")?;
    if !in_bar {
        s.note("A narrow options bar has no stroke alignment: set it in Properties")?;
        s.describe("Centre the stroke in Properties");
        props(s, "Center", |s| s.click("Center"))?;
    }
    let sh = shape_now(s)?;
    s.check(
        "an empty rectangle with a 10 px centred stroke",
        sh.as_ref().is_some_and(|sh| {
            sh.fill.is_none()
                && sh
                    .stroke
                    .as_ref()
                    .is_some_and(|st| st.width == 10.0 && st.align == StrokeAlign::Center)
        }),
        "no fill, stroke 10 px centre",
        format!("{:?}", sh.as_ref().map(|sh| (&sh.fill, &sh.stroke))),
    )?;
    let (inside, edge) = (s.pixel(200, 175)?, s.pixel(100, 175)?);
    s.check(
        "white inside, black on the outline",
        near(inside, [255, 255, 255], 0) && near(edge, [0, 0, 0], 2),
        "white / black",
        format!("{inside:?} / {edge:?}"),
    )?;

    s.describe("Pick the Background layer so Properties shows no shape");
    s.click("Layer Background")?;
    s.describe("Fill with a gradient now");
    s.click("Shape fill")?;
    // By name the harness would find the Gradient tool in the toolbar:
    // the list's third row, under the box.
    let fill_box = s.node("Shape fill").ok_or(UiError("no Shape fill list".into()))?;
    s.click_at(
        fill_box.rect.min + vec2(40.0, 101.0),
        "Gradient, the list's third row",
    )?;
    let tool = s.app(|a| a.tool.name())?;
    s.check_eq("the Shape tool is still up", tool, "Shape")?;
    s.describe("Make the fill colour red");
    click_first(s, &["Fill colour*", "#3D85EB"])?;
    s.set_field("Hex colour", "#FF0000")?;
    s.key("Esc")?;
    s.describe("Drag an ellipse-sized rectangle");
    s.canvas_drag((400.0, 100.0), (700.0, 400.0), 10, "")?;
    // New layers go above the active one (here the Background).
    let sh = active_shape(s)?;
    let grad = match sh.as_ref().and_then(|sh| sh.fill.clone()) {
        Some(Fill::Gradient { gradient, .. }) => {
            let st = gradient.sorted();
            Some((st[0].color, st[st.len() - 1].color))
        }
        _ => None,
    };
    s.check(
        "a gradient from red to the background colour",
        grad.is_some_and(|(a, b)| a[0] > 0.99 && a[1] < 0.01 && b == [1.0, 1.0, 1.0]),
        "red to white",
        format!("{grad:?}"),
    )?;
    s.describe("Undo it");
    s.key("Cmd+Z")?;
    let n = s.layer_names()?.len();
    s.check_eq("one shape left", n, 2)?;
    Ok(())
}

fn shape_edit_in_properties(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    s.describe("Pick the Shape tool from the toolbar");
    s.click_role(Role::Button, "Shape")?;
    s.describe("Drag a rectangle");
    s.canvas_drag((100.0, 100.0), (400.0, 300.0), 10, "")?;
    let steps = s.history()?.len();
    s.describe("Round its corners with Corners in Properties");
    props(s, "Corners", |s| s.drag_slider("Corners", 0.5))?;
    let g = shape_now(s)?.map(|sh| sh.geometry);
    s.check(
        "the corners are rounded by about 50 px (half of the 100 px maximum)",
        matches!(g, Some(ShapeGeometry::Rectangle { radius, .. }) if (radius - 50.0).abs() < 3.0),
        "radius 50",
        format!("{g:?}"),
    )?;
    s.describe("Add a stroke in Properties");
    props(s, "Stroke", |s| s.click("Stroke"))?;
    s.describe("Dash it");
    props(s, "Dashed", |s| s.click("Dashed"))?;
    let st = shape_now(s)?.and_then(|sh| sh.stroke);
    s.check(
        "a dashed stroke",
        st.as_ref().is_some_and(|st| st.dash.is_some()),
        "dash [2, 2]",
        format!("{st:?}"),
    )?;
    s.describe("Switch the fill to a gradient");
    props(s, "Gradient", |s| s.click("Gradient"))?;
    let gradient = shape_now(s)?.is_some_and(|sh| matches!(sh.fill, Some(Fill::Gradient { .. })));
    s.check_eq("the fill is a gradient", gradient, true)?;
    let n = s.history()?.len();
    s.check_eq("four Properties edits, four history steps", n, steps + 4)?;
    s.describe("Undo twice");
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    let sh = shape_now(s)?;
    s.check(
        "solid fill and an undashed stroke again",
        sh.as_ref().is_some_and(|sh| {
            matches!(sh.fill, Some(Fill::Solid { .. }))
                && sh.stroke.as_ref().is_some_and(|st| st.dash.is_none())
        }),
        "solid, undashed",
        format!("{:?}", sh.map(|sh| (sh.fill, sh.stroke))),
    )?;

    s.describe("Draw a polygon and give it 8 sides in Properties");
    pick_kind(s, "Polygon")?;
    s.canvas_drag((450.0, 100.0), (700.0, 350.0), 10, "")?;
    props(s, "Sides", |s| s.drag_slider("Sides", 0.24))?;
    let g = shape_now(s)?.map(|sh| sh.geometry);
    s.check(
        "the polygon has more sides",
        matches!(g, Some(ShapeGeometry::Polygon { sides, .. }) if sides >= 7),
        "7 or more sides",
        format!("{g:?}"),
    )?;
    Ok(())
}

fn shape_transform_and_rasterize(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    s.describe("Pick the Shape tool from the toolbar");
    s.click_role(Role::Button, "Shape")?;
    pick_kind(s, "Ellipse")?;
    s.describe("Drag an ellipse");
    s.canvas_drag((100.0, 100.0), (300.0, 200.0), 10, "")?;
    s.describe("Free Transform with Cmd+T");
    s.key("Cmd+T")?;
    s.describe("Type 150 % width and 200 % height in the options bar");
    s.set_field("Width", "150")?;
    s.set_field("Height", "200")?;
    s.describe("Press Enter to apply");
    s.key("Enter")?;
    let sh = shape_now(s)?;
    let b = sh.as_ref().and_then(|sh| sh.bounds());
    s.check(
        "the ellipse is 300 × 200 now, about its centre",
        b.is_some_and(|b| {
            (b[0] - 50.0).abs() < 2.0
                && (b[1] - 50.0).abs() < 2.0
                && (b[2] - 350.0).abs() < 2.0
                && (b[3] - 250.0).abs() < 2.0
        }),
        "[50, 50, 350, 250]",
        format!("{b:?}"),
    )?;
    let edge = (s.pixel(60, 150)?, s.pixel(40, 150)?);
    s.check(
        "drawn crisply at the new size: fill inside the new edge, white outside",
        near(edge.0, [0x3d, 0x85, 0xeb], 2) && near(edge.1, [255, 255, 255], 0),
        "fill / white",
        format!("{edge:?}"),
    )?;
    let still_shape = sh.is_some();
    s.check_eq("it is still a vector shape", still_shape, true)?;
    s.describe("Rasterize it from Properties");
    props(s, "Rasterize", |s| s.click("Rasterize"))?;
    let pixel = s.doc(|d| {
        d.layers()
            .last()
            .map(|l| matches!(l.content, LayerContent::Pixel(_)))
    })?;
    s.check_eq("the shape is pixels now", pixel, Some(true))?;
    let inside = s.pixel(250, 200)?;
    s.check(
        "and looks the same",
        near(inside, [0x3d, 0x85, 0xeb], 2),
        "the fill colour",
        format!("{inside:?}"),
    )?;
    s.describe("Undo the rasterize");
    s.key("Cmd+Z")?;
    let shape = shape_now(s)?.is_some();
    s.check_eq("a shape again", shape, true)?;
    Ok(())
}

// ---- pen and paths ------------------------------------------------------------------------

fn work_path(s: &mut Session) -> UiResult<Option<VectorPath>> {
    s.doc(|d| d.work_path.clone())
}

fn points(p: &Option<VectorPath>) -> Vec<(Vec<(f32, f32)>, bool)> {
    p.iter()
        .flat_map(|p| p.subpaths.iter())
        .map(|sp| (sp.nodes.iter().map(|n| round2(n.point)).collect(), sp.closed))
        .collect()
}

fn round2(p: (f32, f32)) -> (f32, f32) {
    (p.0.round(), p.1.round())
}

/// A node with its point and handles rounded to whole pixels.
fn round_node(n: PathNode) -> PathNode {
    PathNode {
        point: round2(n.point),
        handle_in: round2(n.handle_in),
        handle_out: round2(n.handle_out),
    }
}

/// Pen: a closed four-node path with a curve at its third node.
fn draw_pen_path(s: &mut Session) -> UiResult {
    s.describe("Pick the Pen from the toolbar");
    s.click_role(Role::Button, "Pen")?;
    s.describe("Click a corner");
    s.canvas_click((100.0, 100.0), "")?;
    s.describe("Click a second corner");
    s.canvas_click((300.0, 100.0), "")?;
    s.describe("Drag at the third point to pull out a curve");
    s.canvas_drag((300.0, 300.0), (380.0, 300.0), 10, "")?;
    s.describe("Click a fourth corner");
    s.canvas_click((100.0, 300.0), "")?;
    s.describe("Click the first point to close the path");
    s.canvas_click((100.0, 100.0), "")
}

fn pen_draw_a_path(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    draw_pen_path(s)?;
    let p = work_path(s)?;
    s.check_eq(
        "one closed path through the four points",
        points(&p),
        vec![(
            vec![(100.0, 100.0), (300.0, 100.0), (300.0, 300.0), (100.0, 300.0)],
            true,
        )],
    )?;
    let curve = p
        .as_ref()
        .and_then(|p| p.subpaths.first()?.nodes.get(2).copied().map(round_node));
    s.check_eq(
        "the dragged point is smooth, its handles mirrored",
        curve,
        Some(PathNode {
            point: (300.0, 300.0),
            handle_in: (220.0, 300.0),
            handle_out: (380.0, 300.0),
        }),
    )?;
    let hist = s.history()?;
    s.note(&format!("History: {}", hist.join(" → ")))?;
    s.check_eq(
        "History names each step",
        hist,
        [
            "Add anchor",
            "Add anchor",
            "Add anchor",
            "Add anchor",
            "Close path",
        ]
        .map(String::from)
        .to_vec(),
    )?;
    s.describe("Undo once");
    s.key("Cmd+Z")?;
    let p = work_path(s)?;
    s.check_eq(
        "undo takes back the last click (closing the path)",
        points(&p),
        vec![(
            vec![(100.0, 100.0), (300.0, 100.0), (300.0, 300.0), (100.0, 300.0)],
            false,
        )],
    )?;
    s.describe("Redo");
    s.key("Cmd+Shift+Z")?;
    let p = work_path(s)?;
    s.check_eq(
        "redo closes it again",
        points(&p).first().map(|p| p.1),
        Some(true),
    )?;
    s.describe("Start a second, open path: three clicks");
    s.canvas_click((500.0, 100.0), "")?;
    s.canvas_click((650.0, 100.0), "")?;
    s.canvas_click((650.0, 250.0), "")?;
    s.describe("Press Esc to stop drawing");
    s.key("Esc")?;
    let p = work_path(s)?;
    s.check_eq(
        "Esc ends the open path and keeps it",
        points(&p).get(1).cloned(),
        Some((vec![(500.0, 100.0), (650.0, 100.0), (650.0, 250.0)], false)),
    )?;
    s.describe("Click elsewhere: a new subpath starts, the old one stays open");
    s.canvas_click((500.0, 400.0), "")?;
    let p = work_path(s)?;
    s.check_eq(
        "a third subpath with one anchor",
        points(&p).iter().map(|sp| sp.0.len()).collect::<Vec<_>>(),
        vec![4, 3, 1],
    )?;
    Ok(())
}

fn pen_edit_anchors(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    draw_pen_path(s)?;
    s.describe("Drag the second anchor up and right");
    s.canvas_drag((300.0, 100.0), (340.0, 60.0), 10, "")?;
    let p = work_path(s)?;
    s.check_eq(
        "the anchor moved",
        points(&p).first().map(|sp| sp.0[1]),
        Some((340.0, 60.0)),
    )?;
    s.describe("Click the curved anchor to show its handles");
    s.canvas_click((300.0, 300.0), "")?;
    s.describe("Drag its outgoing handle down");
    s.canvas_drag((380.0, 300.0), (380.0, 360.0), 10, "")?;
    let n = work_path(s)?.and_then(|p| p.subpaths.first()?.nodes.get(2).copied().map(round_node));
    s.check_eq(
        "the handle moved and its twin mirrors it",
        n.map(|n| (n.handle_out, n.handle_in)),
        Some(((380.0, 360.0), (220.0, 240.0))),
    )?;
    s.describe("Click the fourth anchor and press Backspace to delete it");
    s.canvas_click((100.0, 300.0), "")?;
    s.key("Backspace")?;
    let p = work_path(s)?;
    s.check_eq(
        "three anchors left",
        points(&p),
        vec![(vec![(100.0, 100.0), (340.0, 60.0), (300.0, 300.0)], true)],
    )?;
    s.describe("Undo the delete");
    s.key("Cmd+Z")?;
    let n = points(&work_path(s)?).first().map(|sp| sp.0.len());
    s.check_eq("four anchors again", n, Some(4))?;
    Ok(())
}

fn paths_panel(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    draw_pen_path(s)?;
    s.describe("Open the Paths panel");
    s.click("Paths panel")?;
    s.dump_tree("paths-panel");
    s.describe("Save the work path");
    s.click("Save the work path as a new path")?;
    let names = s.doc(|d| d.saved_paths.iter().map(|p| p.name.clone()).collect::<Vec<_>>())?;
    s.check_eq("a saved Path 1", names, vec!["Path 1".to_string()])?;
    s.describe("Double-click it to rename it");
    s.double_click("Path Path 1")?;
    s.set_field("Path name", "Outline")?;
    let names = s.doc(|d| d.saved_paths.iter().map(|p| p.name.clone()).collect::<Vec<_>>())?;
    s.check_eq("it is called Outline", names, vec!["Outline".to_string()])?;
    s.describe("Fill the path with the brush colour (the Background layer is active)");
    s.click("Fill path with the brush colour")?;
    let inside = s.pixel(200, 200)?;
    let ink = [0x1a, 0x2e, 0x8c];
    s.check(
        "the path's inside is painted",
        near(inside, ink, 2),
        format!("{ink:?}"),
        format!("{inside:?}"),
    )?;
    let outside = s.pixel(500, 400)?;
    s.check(
        "outside stays white",
        near(outside, [255, 255, 255], 0),
        "white",
        format!("{outside:?}"),
    )?;
    s.describe("Undo the fill");
    s.key("Cmd+Z")?;
    s.describe("Load the path as a selection");
    s.click("Load path as selection")?;
    let b = s.selection_bounds()?;
    s.check(
        "the selection covers the path",
        b.is_some_and(|(x, y, w, h)| {
            (99..=100).contains(&x) && y == 100 && w >= 200 && (200..=201).contains(&h)
        }),
        "from (100, 100) ± an edge pixel, 200+ wide, 200 tall",
        format!("{b:?}"),
    )?;
    s.describe("Deselect, then select a rectangle with the marquee");
    s.key("Cmd+D")?;
    s.click("Rectangular Marquee")?;
    s.canvas_drag((450.0, 150.0), (650.0, 350.0), 10, "")?;
    s.describe("Make a work path from the selection");
    s.click("Make work path from selection")?;
    let p = work_path(s)?;
    let pts = points(&p);
    s.check(
        "the work path is the selection's outline",
        pts.len() == 1
            && pts[0].1
            && pts[0].0.contains(&(450.0, 150.0))
            && pts[0].0.contains(&(650.0, 350.0)),
        "one closed subpath round (450, 150)-(650, 350)",
        format!("{pts:?}"),
    )?;
    s.describe("Pick the saved path and delete it");
    s.click("Path Outline")?;
    s.click("Delete path")?;
    let n = s.doc(|d| d.saved_paths.len())?;
    s.check_eq("no saved paths left", n, 0)?;
    s.describe("Undo the delete");
    s.key("Cmd+Z")?;
    let n = s.doc(|d| d.saved_paths.len())?;
    s.check_eq("the saved path is back", n, 1)?;
    Ok(())
}

fn path_to_shape_and_back(s: &mut Session) -> UiResult {
    new_doc(s, 800, 500)?;
    draw_pen_path(s)?;
    s.describe("Make a shape layer from it: Layer ▸ New shape from path");
    s.menu("Layer > New shape from path")?;
    let sh = shape_now(s)?;
    let path_nodes = match sh.as_ref().map(|sh| &sh.geometry) {
        Some(ShapeGeometry::Path { path }) => path.subpaths.first().map(|sp| sp.nodes.len()),
        _ => None,
    };
    s.check_eq("a path shape with the four anchors", path_nodes, Some(4))?;
    let inside = s.pixel(200, 200)?;
    s.check(
        "filled with the Shape tool's colour",
        near(inside, [0x3d, 0x85, 0xeb], 2),
        "#3D85EB",
        format!("{inside:?}"),
    )?;
    s.describe("Clear the work path from the Pen's options bar");
    s.click("Clear path")?;
    let p = work_path(s)?;
    s.check_eq("no work path", p.is_none(), true)?;
    s.describe("Pick the Shape tool and draw a star");
    s.click_role(Role::Button, "Shape")?;
    pick_kind(s, "Star")?;
    s.canvas_drag((450.0, 100.0), (650.0, 300.0), 10, "")?;
    s.describe("Make a work path from the star in Properties");
    props(s, "Make work path", |s| s.click("Make work path"))?;
    let n = points(&work_path(s)?).first().map(|sp| sp.0.len());
    s.check_eq("the work path is the star's ten points", n, Some(10))?;
    Ok(())
}
