//! A. Documents and files: new documents, opening every kind of file,
//! saving, closing and quitting with unsaved work, crash recovery, tabs,
//! image and canvas size, rotating, and every export format.

use crate::uitest::prelude::*;

scenario_list! {
    "new-document" => new_document: "New documents from presets and a custom size, from the welcome screen and File ▸ New",
    "open-files" => open_files: "Open PNG, JPEG, PSD, camera RAW, HEIC and an old project; place and drop images",
    "save-and-close" => save_and_close: "Save, save again, Save As, the unsaved mark, closing with unsaved changes, Open Recent",
    "quit-with-tabs" => quit_with_tabs: "Quit with three tabs, two unsaved: cancel, then save each one",
    "quit-discarding" => quit_discarding: "Quit with two unsaved documents: don't save one, cancel at the other",
    "autosave-recover" => autosave_and_recover: "Autosave two documents, crash, relaunch and recover both",
    "tabs-and-duplicate" => tabs_and_duplicate: "Switch tabs, duplicate a document, close tabs",
    "image-size" => image_size: "Image Size in pixels, percent, inches (Resample off) and centimetres",
    "canvas-and-rotate" => canvas_and_rotate: "Canvas Size with anchors, Trim, Reveal All, rotate and flip the image",
    "export-formats" => export_formats: "Export PNG, JPEG, PDF, PSD 8/16, ORA, 16-bit PNG/TIFF, EXR, Export As and a LUT",
}

use std::path::Path;
use std::time::Duration;

// ---- helpers ------------------------------------------------------------------

/// `s.check_eq(what, actual, expected)` with `actual` read first (it
/// borrows the session too): `ck!(s, what, actual, expected)?`.
macro_rules! ck {
    ($s:ident, $what:expr, $actual:expr, $want:expr $(,)?) => {{
        let actual = $actual;
        $s.check_eq($what, actual, $want)
    }};
}

/// Within `tol` of `want` on every channel.
fn near(got: [u8; 4], want: [u8; 4], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

/// The open document's (width, height, resolution).
fn doc_size(s: &mut Session) -> UiResult<(u32, u32, f32)> {
    s.doc(|d| (d.width, d.height, d.resolution))
}

/// The tab strip as the user sees it: (title, unsaved) per tab.
fn tabs(s: &mut Session) -> UiResult<Vec<(String, bool)>> {
    s.app(|a| a.tab_infos())
}

/// The status bar's message.
fn status(s: &mut Session) -> UiResult<String> {
    s.app(|a| a.status.clone())
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> UiError {
    UiError(format!("writing {}: {e}", path.display()))
}

/// A photo-like PNG the "user" has on disk: red rises left to right,
/// green top to bottom, blue 128.
fn write_gradient(path: &Path, w: u32, h: u32) -> UiResult {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x * 255 / (w - 1)) as u8, (y * 255 / (h - 1)) as u8, 128, 255])
    });
    img.save(path).map_err(|e| io_err(path, e))
}

/// A flat-colour image in any format `image` writes (by extension).
fn write_solid(path: &Path, w: u32, h: u32, rgba: [u8; 4]) -> UiResult {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    match path.extension().and_then(|e| e.to_str()) {
        Some("jpg") | Some("jpeg") => image::DynamicImage::ImageRgba8(img)
            .to_rgb8()
            .save(path)
            .map_err(|e| io_err(path, e)),
        _ => img.save(path).map_err(|e| io_err(path, e)),
    }
}

/// A JPEG, left half red, right half blue, as a camera might write it.
fn write_jpeg_halves(path: &Path, w: u32, h: u32) -> UiResult {
    let img = image::RgbImage::from_fn(w, h, |x, _| {
        if x < w / 2 {
            image::Rgb([220, 30, 30])
        } else {
            image::Rgb([30, 30, 220])
        }
    });
    img.save(path).map_err(|e| io_err(path, e))
}

/// A HEIC made by macOS's own converter from a flat orange PNG.
fn write_heic(path: &Path, w: u32, h: u32) -> UiResult {
    let png = path.with_extension("src.png");
    write_solid(&png, w, h, [240, 140, 20, 255])?;
    let out = std::process::Command::new("sips")
        .args(["-s", "format", "heic"])
        .arg(&png)
        .arg("--out")
        .arg(path)
        .output()
        .map_err(|e| io_err(path, e))?;
    let _ = std::fs::remove_file(&png);
    if !out.status.success() || !path.exists() {
        return Err(io_err(path, String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// A minimal camera RAW: an uncompressed 16-bit RGGB Bayer DNG whose
/// camera space is linear sRGB (ColorMatrix1 = XYZ→sRGB), so a neutral
/// grey develops to grey. The left half is brighter than the right.
fn write_dng(path: &Path, w: u32, h: u32) -> UiResult {
    let mut data: Vec<u8> = Vec::with_capacity((w * h * 2) as usize);
    for _y in 0..h {
        for x in 0..w {
            let v: u16 = if x < w / 2 { 30000 } else { 6000 };
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    // (tag, type, count, value bytes); types: 1 BYTE, 2 ASCII, 3 SHORT,
    // 4 LONG, 5 RATIONAL, 10 SRATIONAL.
    let short = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let long = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let srat = |v: &[(i32, i32)]| {
        v.iter()
            .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
            .collect::<Vec<u8>>()
    };
    let ascii = |s: &str| {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        b
    };
    let xyz_to_srgb = [
        (32406, 10000),
        (-15372, 10000),
        (-4986, 10000),
        (-9689, 10000),
        (18758, 10000),
        (415, 10000),
        (557, 10000),
        (-2040, 10000),
        (10570, 10000),
    ];
    let mut tags: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (254, 4, 1, long(&[0])),
        (256, 4, 1, long(&[w])),
        (257, 4, 1, long(&[h])),
        (258, 3, 1, short(&[16])),
        (259, 3, 1, short(&[1])),
        (262, 3, 1, short(&[32803])),
        (271, 2, 0, ascii("Lumenply")),
        (272, 2, 0, ascii("Test camera")),
        (273, 4, 1, long(&[0])), // strip offset, patched below
        (274, 3, 1, short(&[1])),
        (277, 3, 1, short(&[1])),
        (278, 4, 1, long(&[h])),
        (279, 4, 1, long(&[w * h * 2])),
        (284, 3, 1, short(&[1])),
        (33421, 3, 2, short(&[2, 2])),
        (33422, 1, 4, vec![0, 1, 1, 2]),
        (50706, 1, 4, vec![1, 4, 0, 0]),
        (50707, 1, 4, vec![1, 1, 0, 0]),
        (50708, 2, 0, ascii("Lumenply Test camera")),
        (50714, 4, 1, long(&[0])),
        (50717, 4, 1, long(&[65535])),
        (50721, 10, 9, srat(&xyz_to_srgb)),
        (50728, 5, 3, long(&[1, 1, 1, 1, 1, 1])),
        (50778, 3, 1, short(&[21])),
    ];
    for t in &mut tags {
        if t.2 == 0 {
            t.2 = t.3.len() as u32;
        }
    }
    let ifd_at = 8u32;
    let ifd_len = 2 + tags.len() as u32 * 12 + 4;
    let mut extra_at = ifd_at + ifd_len;
    let mut extra: Vec<u8> = Vec::new();
    let mut entries: Vec<u8> = Vec::new();
    let data_at = {
        let big: u32 = tags
            .iter()
            .filter(|t| t.3.len() > 4)
            .map(|t| t.3.len() as u32 + 1)
            .sum();
        (extra_at + big + 1) & !1
    };
    for (tag, ty, count, mut bytes) in tags {
        if tag == 273 {
            bytes = long(&[data_at]);
        }
        entries.extend_from_slice(&tag.to_le_bytes());
        entries.extend_from_slice(&ty.to_le_bytes());
        entries.extend_from_slice(&count.to_le_bytes());
        if bytes.len() <= 4 {
            bytes.resize(4, 0);
            entries.extend_from_slice(&bytes);
        } else {
            entries.extend_from_slice(&extra_at.to_le_bytes());
            if bytes.len() % 2 == 1 {
                bytes.push(0);
            }
            extra_at += bytes.len() as u32;
            extra.extend_from_slice(&bytes);
        }
    }
    let mut file = b"II*\0".to_vec();
    file.extend_from_slice(&ifd_at.to_le_bytes());
    file.extend_from_slice(&((ifd_len - 6) / 12).to_le_bytes()[..2]);
    file.extend_from_slice(&entries);
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&extra);
    file.resize(data_at as usize, 0);
    file.extend_from_slice(&data);
    std::fs::write(path, file).map_err(|e| io_err(path, e))
}

/// A project as Lumenply 0.8 saved it (format 1, the layer tree): a green
/// Background and a half-opaque red square on a "Top" layer.
fn write_format1(path: &Path) -> UiResult {
    use lumenply_tiles::{Raster, Rgba, TileStore};
    let mut doc = lumenply_doc::Document::new(300, 200);
    let bg = doc.add_pixel_layer("Background");
    let green = Raster::filled(300, 200, Rgba::from_straight(0.0, 0.5, 0.0, 1.0));
    *doc.layer_mut(bg)
        .and_then(|l| l.pixels_mut())
        .ok_or("no pixels")? = TileStore::from_raster(&green, 0, 0);
    let top = doc.add_pixel_layer("Top");
    let red = Raster::filled(100, 100, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
    let l = doc.layer_mut(top).ok_or("no layer")?;
    *l.pixels_mut().ok_or("no pixels")? = TileStore::from_raster(&red, 50, 50);
    l.opacity = 0.5;
    doc.resolution = 200.0;
    lumenply_io::project::save(path, &doc).map_err(|e| io_err(path, e))
}

/// The format version a project file declares.
fn project_version(path: &Path) -> Option<u32> {
    lumenply_io::graph_project::project_version(path).ok()
}

fn new_document(s: &mut Session) -> UiResult {
    s.expect_text("No document open")?;
    s.describe("Click New image… on the welcome screen");
    s.click("New image…")?;
    s.expect_node("New document")?;
    s.dump_tree("new-dialog");
    s.describe("Open the Preset list");
    s.click("Document preset")?;
    s.describe("Pick A4 at 300 ppi");
    s.click("A4 (300 ppi)")?;
    s.expect_text("2480 × 3508 pixels")?;
    s.describe("Create it");
    s.click("Create")?;
    s.wait_idle()?;
    ck!(s, "an A4 page at 300 ppi", doc_size(s)?, (2480, 3508, 300.0))?;
    ck!(
        s,
        "one white Background layer",
        s.layer_names()?,
        vec!["Background".to_string()],
    )?;
    let px = s.pixel(1240, 1754)?;
    s.check(
        "the paper is white",
        near(px, [255; 4], 0),
        "[255, 255, 255, 255]",
        format!("{px:?}"),
    )?;
    ck!(s, "nothing to undo yet", s.history()?.len(), 0)?;
    ck!(
        s,
        "one tab, Untitled-1, saved",
        tabs(s)?,
        vec![("Untitled-1".to_string(), false)]
    )?;
    s.screenshot("a4");

    s.describe("A second document from File ▸ New, typed in by hand");
    s.menu("File > New...")?;
    s.set_field("Width", "640")?;
    s.set_field("Height", "480")?;
    s.set_field("Resolution", "150")?;
    s.expect_text("640 × 480 pixels")?;
    s.click("Create")?;
    s.wait_idle()?;
    ck!(s, "640 × 480 at 150 ppi", doc_size(s)?, (640, 480, 150.0))?;
    ck!(
        s,
        "a second tab, the new one live",
        tabs(s)?,
        vec![
            ("Untitled-1".to_string(), false),
            ("Untitled-2".to_string(), false)
        ],
    )?;

    s.describe("A third from the 4K preset with Cmd+N");
    s.key("Cmd+N")?;
    s.click("Document preset")?;
    s.click("4K UHD 3840 × 2160 (72 ppi)")?;
    s.click("Create")?;
    s.wait_idle()?;
    ck!(s, "4K at 72 ppi", doc_size(s)?, (3840, 2160, 72.0))?;

    s.describe("And one sized in centimetres");
    s.key("Cmd+N")?;
    s.click("Width unit")?;
    s.click("Centimetres")?;
    s.set_field("Resolution", "300")?;
    s.set_field("Width", "10")?;
    s.set_field("Height", "15")?;
    s.click("Create")?;
    s.wait_idle()?;
    // 10 cm at 300 ppi: 10 / 2.54 × 300 = 1181.1; 15 cm: 1771.7.
    ck!(s, "10 × 15 cm at 300 ppi", doc_size(s)?, (1181, 1772, 300.0))?;

    s.describe("Cancel a New dialog with Esc");
    s.key("Cmd+N")?;
    s.key("Esc")?;
    let n = tabs(s)?.len();
    ck!(s, "no document was made", n, 4)?;
    Ok(())
}

/// Make a new white document of `w` × `h` from File ▸ New.
fn new_doc(s: &mut Session, w: u32, h: u32) -> UiResult {
    s.describe(&format!("Make a new {w} × {h} image with File ▸ New"));
    s.menu("File > New...")?;
    s.set_field("Width", &w.to_string())?;
    s.set_field("Height", &h.to_string())?;
    s.click("Create")?;
    s.wait_idle()
}

/// The brush colour the app starts with, as the composite shows it.
const BRUSH_BLUE: [u8; 4] = [0x1a, 0x2e, 0x8c, 255];

/// Paint a horizontal stroke across row `y` with the Brush.
fn stroke(s: &mut Session, y: f32, x0: f32, x1: f32) -> UiResult {
    s.click_role(Role::Button, "Brush")?;
    s.describe(&format!("Paint a stroke at y = {y}"));
    s.canvas_drag((x0, y), (x1, y), 10, "")?;
    s.wait_idle()
}

fn painted(s: &mut Session, x: u32, y: u32) -> UiResult<bool> {
    Ok(near(s.pixel(x, y)?, BRUSH_BLUE, 3))
}

fn save_and_close(s: &mut Session) -> UiResult {
    let poster = s.files().join("poster.lumen");
    let v2 = s.files().join("poster-v2.lumen");
    new_doc(s, 400, 300)?;
    stroke(s, 60.0, 50.0, 350.0)?;
    ck!(
        s,
        "the tab shows unsaved changes",
        tabs(s)?,
        vec![("Untitled-1".to_string(), true)]
    )?;
    s.check(
        "so does the window title",
        s.title.contains('•'),
        "Untitled-1 • — Lumenply",
        s.title.clone(),
    )?;

    s.describe("Save with Cmd+S: a new document asks where");
    s.key("Cmd+S")?;
    let panel = s
        .open_dialog()
        .map(|d| (d.kind.to_string(), d.title.clone(), d.file_name.clone()));
    ck!(
        s,
        "a save panel for a project, named Untitled",
        panel,
        Some((
            "save".to_string(),
            "Save project".to_string(),
            Some("Untitled-1.lumen".to_string())
        ))
    )?;
    s.save_as(&poster)?;
    ck!(
        s,
        "a format 3 project was written",
        project_version(&poster),
        Some(3)
    )?;
    ck!(
        s,
        "the tab takes the file's name, saved",
        tabs(s)?,
        vec![("poster.lumen".to_string(), false)]
    )?;
    s.check(
        "the title has no unsaved mark",
        !s.title.contains('•'),
        "poster.lumen — Lumenply",
        s.title.clone(),
    )?;
    s.expect_text("Saved")?;

    stroke(s, 120.0, 50.0, 350.0)?;
    ck!(s, "unsaved again", tabs(s)?[0].1, true)?;
    s.describe("Cmd+S again saves in place, without asking");
    s.key("Cmd+S")?;
    ck!(s, "no panel this time", s.open_dialog().is_some(), false)?;
    ck!(s, "saved", tabs(s)?[0].1, false)?;

    s.describe("Undo after saving: the document differs from the file again");
    s.key("Cmd+Z")?;
    ck!(s, "unsaved after the undo", tabs(s)?[0].1, true)?;
    s.describe("Redo: back to what was saved");
    s.key("Cmd+Shift+Z")?;
    ck!(s, "saved state again", tabs(s)?[0].1, false)?;
    s.describe("Undo, then paint something else instead");
    s.key("Cmd+Z")?;
    stroke(s, 200.0, 50.0, 350.0)?;
    ck!(s, "a different edit is unsaved too", tabs(s)?[0].1, true)?;
    s.check(
        "and the title says so",
        s.title.contains('•'),
        "poster.lumen • — Lumenply",
        s.title.clone(),
    )?;

    s.describe("Save a copy under a new name with File ▸ Save as...");
    s.menu("File > Save as...")?;
    s.save_as(&v2)?;
    ck!(
        s,
        "the tab follows the new name",
        tabs(s)?,
        vec![("poster-v2.lumen".to_string(), false)]
    )?;
    ck!(
        s,
        "both files exist",
        (poster.exists(), v2.exists()),
        (true, true)
    )?;

    stroke(s, 250.0, 50.0, 350.0)?;
    s.describe("Close the document with Cmd+W while it has unsaved changes");
    s.key("Cmd+W")?;
    s.expect_text("Save changes to “poster-v2.lumen” before closing?")?;
    s.screenshot("close-warning");
    s.describe("Cancel keeps it open");
    s.click("Cancel")?;
    ck!(
        s,
        "still open, still unsaved",
        tabs(s)?,
        vec![("poster-v2.lumen".to_string(), true)]
    )?;
    s.key("Cmd+W")?;
    s.describe("Esc cancels too");
    s.key("Esc")?;
    ck!(s, "still open", tabs(s)?.len(), 1)?;
    s.key("Cmd+W")?;
    s.describe("Close without saving");
    s.click("Close without saving")?;
    s.expect_text("No document open")?;

    s.describe("Reopen the copy from File ▸ Open recent");
    s.menu("File > Open recent > poster-v2.lumen")?;
    s.wait_idle()?;
    let back = (
        painted(s, 200, 60)?,
        painted(s, 200, 120)?,
        painted(s, 200, 200)?,
        painted(s, 200, 250)?,
    );
    ck!(
        s,
        "the saved strokes are there, the discarded one is not",
        back,
        (true, false, true, false)
    )?;
    stroke(s, 280.0, 50.0, 350.0)?;
    s.key("Cmd+W")?;
    s.describe("Save and close");
    s.click("Save and close")?;
    s.expect_text("No document open")?;

    s.describe("Open the first file from the welcome screen's recent list");
    s.click("Open poster.lumen")?;
    s.wait_idle()?;
    let first = (painted(s, 200, 60)?, painted(s, 200, 120)?, painted(s, 200, 200)?);
    ck!(s, "poster.lumen kept its second save", first, (true, true, false))?;
    s.describe("And the copy, saved on closing");
    s.menu("File > Open recent > poster-v2.lumen")?;
    s.wait_idle()?;
    ck!(
        s,
        "the stroke saved on closing is there",
        painted(s, 200, 280)?,
        true
    )?;
    Ok(())
}

fn quit_with_tabs(s: &mut Session) -> UiResult {
    let photo = s.files().join("photo.png");
    write_gradient(&photo, 240, 160)?;
    let c = s.files().join("c.lumen");
    let b = s.files().join("b.lumen");

    s.describe("Open a photo and leave it untouched");
    s.click("Open…")?;
    s.choose_file(&photo)?;
    new_doc(s, 300, 200)?;
    stroke(s, 50.0, 20.0, 280.0)?;
    new_doc(s, 200, 200)?;
    stroke(s, 40.0, 20.0, 180.0)?;
    s.describe("Save the third document as c.lumen");
    s.key("Cmd+S")?;
    s.save_as(&c)?;
    stroke(s, 150.0, 20.0, 180.0)?;
    ck!(
        s,
        "three tabs, two with unsaved changes",
        tabs(s)?,
        vec![
            ("photo.png".to_string(), false),
            ("Untitled-1".to_string(), true),
            ("c.lumen".to_string(), true),
        ]
    )?;

    s.describe("Quit with File ▸ Quit Lumenply");
    s.menu("File > Quit Lumenply")?;
    ck!(s, "Lumenply is still running", s.has_quit(), false)?;
    s.expect_node("Unsaved changes")?;
    s.screenshot("quit-warning");
    s.describe("Cancel the quit");
    s.click("Cancel")?;
    ck!(
        s,
        "still running, nothing closed",
        (s.has_quit(), tabs(s)?.len()),
        (false, 3)
    )?;

    s.describe("Quit again with the window's close button");
    s.close_window()?;
    s.expect_node("Unsaved changes")?;
    s.expect_text("c.lumen")?;
    s.describe("Save c.lumen");
    s.click("Save")?;
    ck!(
        s,
        "Lumenply waits: another document has unsaved changes",
        s.has_quit(),
        false
    )?;
    s.expect_node("Unsaved changes")?;
    s.expect_text("Untitled-1")?;
    s.screenshot("quit-second-document");
    s.describe("Save Untitled-1 too: it asks where");
    s.click("Save")?;
    s.save_as(&b)?;
    s.wait_frames(4)?;
    ck!(s, "then Lumenply quits", s.has_quit(), true)?;
    ck!(
        s,
        "both files were written",
        (project_version(&b), project_version(&c)),
        (Some(3), Some(3))
    )?;
    // c.lumen holds both strokes: the one saved first and the later one.
    let strokes = lumenply_io::graph_project::load_graph_project(&c)
        .ok()
        .and_then(|p| lumenply_core::Editor::from_graph_project(&p.graph, &p.meta, p.blobs).ok())
        .map(|ed| {
            let flat = lumenply_render::composite_raster(ed.doc());
            let at = |x: u32, y: u32| flat.pixels[(y * flat.width + x) as usize].a;
            (at(100, 40) > 0.99, at(100, 150) > 0.99, at(100, 100) > 0.99)
        });
    ck!(
        s,
        "c.lumen has both strokes on its white page",
        strokes,
        Some((true, true, true))
    )?;
    Ok(())
}

fn quit_discarding(s: &mut Session) -> UiResult {
    new_doc(s, 300, 200)?;
    stroke(s, 50.0, 20.0, 280.0)?;
    new_doc(s, 300, 200)?;
    stroke(s, 80.0, 20.0, 280.0)?;
    s.describe("Quit with two unsaved documents");
    s.menu("File > Quit Lumenply")?;
    s.expect_text("Untitled-2")?;
    s.describe("Don't save the first");
    s.click("Don't save")?;
    ck!(
        s,
        "still running: the other one is asked about",
        s.has_quit(),
        false
    )?;
    s.expect_text("Untitled-1")?;
    s.describe("Cancel at the second: the quit stops there");
    s.click("Cancel")?;
    ck!(
        s,
        "still running, Untitled-1 still open and unsaved",
        tabs(s)?,
        vec![("Untitled-1".to_string(), true)]
    )?;
    s.describe("Quit again and discard it");
    s.close_window()?;
    s.click("Don't save")?;
    s.wait_frames(4)?;
    ck!(s, "Lumenply quits", s.has_quit(), true)?;
    Ok(())
}

fn autosave_and_recover(s: &mut Session) -> UiResult {
    let saved = s.files().join("saved.lumen");
    s.describe("Set autosave to every 15 s in Preferences");
    s.menu("Edit > Preferences...")?;
    s.set_field("Autosave every", "15")?;
    s.click("Save")?;
    let every = s.app(|a| a.prefs.autosave_secs)?;
    ck!(s, "autosave every 15 s", every, 15)?;

    new_doc(s, 300, 200)?;
    stroke(s, 50.0, 20.0, 280.0)?;
    s.key("Cmd+S")?;
    s.save_as(&saved)?;
    stroke(s, 120.0, 20.0, 280.0)?;
    new_doc(s, 200, 100)?;
    stroke(s, 30.0, 20.0, 180.0)?;
    let before = (tabs(s)?, s.pixel(100, 30)?);
    s.wait_until(
        "Lumenply autosaves both documents",
        Duration::from_secs(40),
        |s| Ok(status(s)?.starts_with("Autosaved backups of 2 documents")),
    )?;
    s.expect_text("Autosaved backups of 2 documents")?;
    s.wait_until("the backups are on disk", Duration::from_secs(10), |s| {
        let dir = s.app(|_| crate::session::autosave_dir())?;
        Ok(dir.is_some_and(|d| d.join("1.lumen").exists() && d.join("1.src").exists()))
    })?;

    s.crash_and_relaunch()?;
    s.expect_node("Recover autosaved document")?;
    s.expect_text("autosaved backups of 2 documents")?;
    s.screenshot("recover");
    s.describe("Recover them");
    s.click("Recover")?;
    s.wait_idle()?;
    let after = tabs(s)?;
    s.check(
        "both documents are back, unsaved",
        after.len() == 2 && after.iter().all(|t| t.1) && after.iter().any(|t| t.0 == "saved.lumen"),
        format!("{:?} (both marked unsaved)", before.0),
        format!("{after:?}"),
    )?;
    s.expect_text("Recovered 2 autosaved documents")?;
    // The live tab is the last recovered; find the untitled one.
    let untitled = after.iter().position(|t| t.0 != "saved.lumen").unwrap_or(0);
    s.app(move |a| a.switch_tab(untitled))?;
    s.wait_idle()?;
    let (w, h, _) = doc_size(s)?;
    ck!(s, "the untitled one is 200 × 100", (w, h), (200, 100))?;
    ck!(s, "with its stroke", painted(s, 100, 30)?, true)?;
    let other = 1 - untitled;
    s.app(move |a| a.switch_tab(other))?;
    s.wait_idle()?;
    ck!(
        s,
        "saved.lumen has both strokes, the unsaved one too",
        (painted(s, 150, 50)?, painted(s, 150, 120)?),
        (true, true)
    )?;
    s.describe("Save it: it goes back to its own file");
    s.key("Cmd+S")?;
    ck!(s, "no panel: it knows its file", s.open_dialog().is_some(), false)?;
    ck!(s, "saved", tabs(s)?[other].1, false)?;
    Ok(())
}

fn tabs_and_duplicate(s: &mut Session) -> UiResult {
    let a = s.files().join("a.png");
    let b = s.files().join("b.png");
    write_solid(&a, 120, 80, [200, 40, 40, 255])?;
    write_solid(&b, 90, 60, [40, 40, 200, 255])?;
    s.click("Open…")?;
    s.choose_file(&a)?;
    s.key("Cmd+O")?;
    s.choose_file(&b)?;
    ck!(
        s,
        "two tabs, b.png live",
        (tabs(s)?.len(), doc_size(s)?.0),
        (2, 90)
    )?;
    s.describe("Click the first tab");
    s.click("a.png")?;
    ck!(s, "a.png is live", doc_size(s)?.0, 120)?;

    s.describe("Duplicate the document with Image ▸ Duplicate");
    s.menu("Image > Duplicate")?;
    s.wait_idle()?;
    let t = tabs(s)?;
    ck!(
        s,
        "a third tab, the copy, unsaved",
        t.last().cloned(),
        Some(("a copy".to_string(), true))
    )?;
    ck!(s, "the same pixels", s.pixel(60, 40)?, [200, 40, 40, 255])?;
    stroke(s, 40.0, 10.0, 110.0)?;
    s.describe("Back to the original");
    s.click("a.png")?;
    ck!(
        s,
        "the original is untouched",
        s.pixel(60, 40)?,
        [200, 40, 40, 255]
    )?;

    s.describe("Close the live tab with its × button");
    s.click("Close document")?;
    ck!(
        s,
        "a.png closed; the copy still asks before closing",
        tabs(s)?.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
        vec!["b.png".to_string(), "a copy".to_string()]
    )?;
    s.describe("Switch to the copy and close it from File ▸ Close document");
    s.click("a copy*")?;
    s.menu("File > Close document")?;
    s.expect_text("Save changes to “a copy” before closing?")?;
    s.click("Close without saving")?;
    ck!(
        s,
        "only b.png is left",
        tabs(s)?,
        vec![("b.png".to_string(), false)]
    )?;
    s.key("Cmd+W")?;
    s.expect_text("No document open")?;
    Ok(())
}

/// Opens a 400 × 200 gradient (72 ppi) for the size scenarios.
fn open_gradient(s: &mut Session) -> UiResult {
    let png = s.files().join("gradient.png");
    write_gradient(&png, 400, 200)?;
    s.click("Open…")?;
    s.choose_file(&png)?;
    ck!(s, "400 × 200 at 72 ppi", doc_size(s)?, (400, 200, 72.0))?;
    Ok(())
}

fn image_size(s: &mut Session) -> UiResult {
    open_gradient(s)?;
    s.describe("Image ▸ Image size: halve the width in pixels");
    s.menu("Image > Image size...")?;
    s.expect_node("Image size")?;
    s.set_field("Width", "200")?;
    s.expect_text("200 × 100")?;
    s.click("Apply")?;
    s.wait_idle()?;
    ck!(
        s,
        "200 × 100, the height kept in proportion",
        doc_size(s)?,
        (200, 100, 72.0)
    )?;
    let c = (s.pixel(0, 0)?, s.pixel(199, 99)?);
    s.check(
        "the picture is scaled, not cropped",
        near(c.0, [0, 0, 128, 255], 4) && near(c.1, [255, 255, 128, 255], 4),
        "≈[0, 0, 128] top left, ≈[255, 255, 128] bottom right",
        format!("{c:?}"),
    )?;
    ck!(s, "one history step", s.history()?.len(), 1)?;
    s.key("Cmd+Z")?;
    ck!(s, "undo brings back 400 × 200", doc_size(s)?, (400, 200, 72.0))?;

    s.describe("In percent: 25 %");
    s.menu("Image > Image size...")?;
    s.click("Width unit")?;
    s.click("Percent")?;
    s.set_field("Width", "25")?;
    s.key("Enter")?;
    s.wait_idle()?;
    ck!(s, "Enter applies: 100 × 50", doc_size(s)?, (100, 50, 72.0))?;
    s.key("Cmd+Z")?;

    s.describe("Print size with Resample off: 2 inches wide");
    s.menu("Image > Image size...")?;
    s.click("Resample")?;
    s.click("Width unit")?;
    s.click("Inches")?;
    s.set_field("Width", "2")?;
    s.expect_text("200 ppi")?;
    s.click("Apply")?;
    s.wait_idle()?;
    ck!(s, "the same pixels at 200 ppi", doc_size(s)?, (400, 200, 200.0))?;
    s.key("Cmd+Z")?;
    ck!(s, "undo restores 72 ppi", doc_size(s)?, (400, 200, 72.0))?;

    s.describe("In centimetres with Resample on: 2.54 cm at 72 ppi");
    s.menu("Image > Image size...")?;
    s.dump_tree("image-size-reopened");
    s.click("Width unit")?;
    s.click("Centimetres")?;
    s.set_field("Width", "2.54")?;
    s.click("Apply")?;
    s.wait_idle()?;
    ck!(s, "one inch at 72 ppi: 72 × 36", doc_size(s)?, (72, 36, 72.0))?;

    s.describe("Esc cancels the dialog");
    s.menu("Image > Image size...")?;
    s.set_field("Width", "10")?;
    s.key("Esc")?;
    ck!(s, "nothing changed", doc_size(s)?, (72, 36, 72.0))?;
    Ok(())
}

fn canvas_and_rotate(s: &mut Session) -> UiResult {
    open_gradient(s)?;
    let tl = s.pixel(0, 0)?;
    let br = s.pixel(399, 199)?;

    s.describe("Image ▸ Canvas size: 500 × 300 anchored top left");
    s.menu("Image > Canvas size...")?;
    s.set_field("Width", "500")?;
    s.set_field("Height", "300")?;
    s.click("Anchor top left")?;
    s.click("Apply")?;
    s.wait_idle()?;
    ck!(s, "500 × 300", doc_size(s)?, (500, 300, 72.0))?;
    ck!(
        s,
        "the picture stays at the top left",
        (s.pixel(0, 0)?, s.pixel(399, 199)?),
        (tl, br)
    )?;
    let added = s.pixel(450, 250)?;
    s.note(&format!("The added canvas shows as {added:?}"))?;
    s.key("Cmd+Z")?;
    ck!(s, "undo: 400 × 200", doc_size(s)?, (400, 200, 72.0))?;

    s.describe("Canvas size 600 × 400 from the centre");
    s.menu("Image > Canvas size...")?;
    s.set_field("Width", "600")?;
    s.set_field("Height", "400")?;
    s.click("Apply")?;
    s.wait_idle()?;
    ck!(
        s,
        "centred: the old corner is at (100, 100)",
        s.pixel(100, 100)?,
        tl
    )?;

    s.describe("Trim the transparent border away");
    s.menu("Image > Trim...")?;
    s.click("Transparent pixels")?;
    s.click_role(Role::Button, "Trim")?;
    s.wait_idle()?;
    ck!(
        s,
        "back to the picture's 400 × 200",
        doc_size(s)?,
        (400, 200, 72.0)
    )?;
    s.describe("Shrink the canvas to 200 × 100, cutting into the picture");
    s.menu("Image > Canvas size...")?;
    s.set_field("Width", "200")?;
    s.set_field("Height", "100")?;
    s.click("Apply")?;
    s.wait_idle()?;
    ck!(s, "200 × 100", doc_size(s)?, (200, 100, 72.0))?;
    s.describe("Reveal all brings the hidden picture back");
    s.menu("Image > Reveal all")?;
    s.wait_idle()?;
    ck!(
        s,
        "the whole 400 × 200 picture again",
        (doc_size(s)?, s.pixel(0, 0)?),
        ((400, 200, 72.0), tl)
    )?;
    for _ in 0..4 {
        s.key("Cmd+Z")?;
    }
    ck!(s, "four undos: the opened image", doc_size(s)?, (400, 200, 72.0))?;
    ck!(s, "nothing left to undo", s.history()?.len(), 0)?;

    s.describe("Rotate 90° clockwise");
    s.menu("Image > Rotate 90° clockwise")?;
    s.wait_idle()?;
    ck!(s, "200 × 400", doc_size(s)?, (200, 400, 72.0))?;
    ck!(s, "the top-left corner is now top right", s.pixel(199, 0)?, tl)?;
    s.describe("Flip the image horizontally");
    s.menu("Image > Flip image horizontal")?;
    s.wait_idle()?;
    ck!(s, "and back to the top left", s.pixel(0, 0)?, tl)?;
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;
    ck!(
        s,
        "undone: 400 × 200 as opened",
        (doc_size(s)?, s.pixel(0, 0)?),
        ((400, 200, 72.0), tl)
    )?;

    s.describe("Rotate by 90° counter-clockwise, then 180°");
    s.menu("Image > Rotate 90° counter-clockwise")?;
    s.wait_idle()?;
    ck!(
        s,
        "the top-left corner goes to the bottom left",
        s.pixel(0, 399)?,
        tl
    )?;
    s.menu("Image > Rotate 180°")?;
    s.wait_idle()?;
    ck!(s, "then to the top right", s.pixel(199, 0)?, tl)?;
    s.key("Cmd+Z")?;
    s.key("Cmd+Z")?;

    s.describe("Rotate by angle: 30° clockwise");
    s.menu("Image > Rotate by angle...")?;
    s.set_field("Rotation angle", "30")?;
    s.click("Rotate")?;
    s.wait_idle()?;
    // The bounding box of a 400 × 200 rectangle turned 30°:
    // 400 cos 30 + 200 sin 30 = 446.4, 400 sin 30 + 200 cos 30 = 373.2.
    let (w, h, _) = doc_size(s)?;
    s.check(
        "the canvas grows to fit",
        (446..=448).contains(&w) && (373..=375).contains(&h),
        "≈447 × 374",
        format!("{w} × {h}"),
    )?;
    ck!(s, "the new corners are transparent", s.pixel(1, 1)?[3], 0)?;
    s.key("Cmd+Z")?;
    ck!(s, "undo: 400 × 200", doc_size(s)?, (400, 200, 72.0))?;
    Ok(())
}

/// A 200 × 100 PNG in four quadrants: red, green / blue, half-clear white.
fn write_quadrants(path: &Path) -> UiResult {
    let img = image::RgbaImage::from_fn(200, 100, |x, y| match (x < 100, y < 50) {
        (true, true) => image::Rgba([255, 0, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, 255]),
        (true, false) => image::Rgba([0, 0, 255, 255]),
        (false, false) => image::Rgba([255, 255, 255, 128]),
    });
    img.save(path).map_err(|e| io_err(path, e))
}

/// Pick an item from the Export button at the top right, which lists the
/// same items as File ▸ Export (the harness can't tell that submenu from
/// the button of the same name).
fn export_item(s: &mut Session, item: &str) -> UiResult {
    s.click_role(Role::Button, "Export")?;
    s.click(item)
}

/// Export through an Export item and its save panel.
fn export(s: &mut Session, item: &str, path: &Path) -> UiResult {
    s.describe(&format!("Export ▸ {item}"));
    export_item(s, item)?;
    s.save_as(path)?;
    s.wait_idle()
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len())
}

fn export_formats(s: &mut Session) -> UiResult {
    let dir = s.files();
    let quad = dir.join("quad.png");
    write_quadrants(&quad)?;
    s.click("Open…")?;
    s.choose_file(&quad)?;
    s.describe("Add a layer with a patch on it, for the layered formats");
    let patch = dir.join("patch.png");
    write_solid(&patch, 20, 10, [255, 255, 0, 255])?;
    s.menu("File > Place image as layer...")?;
    s.choose_file(&patch)?;
    s.wait_idle()?;
    ck!(
        s,
        "two layers",
        s.layer_names()?,
        vec!["patch.png".to_string(), "Background".to_string()]
    )?;

    // PNG: the composite, transparency kept.
    let png = dir.join("out.png");
    export(s, "PNG...", &png)?;
    let img = image::open(&png).map(|i| i.to_rgba8()).ok();
    let px = img.as_ref().map(|i| {
        (
            i.dimensions(),
            i.get_pixel(10, 10).0,
            i.get_pixel(150, 80).0,
            i.get_pixel(100, 50).0,
        )
    });
    ck!(
        s,
        "PNG: 200 × 100, red, half-clear white, the yellow patch",
        px,
        Some((
            (200, 100),
            [255, 0, 0, 255],
            [255, 255, 255, 128],
            [255, 255, 0, 255]
        ))
    )?;
    s.expect_text("Exported")?;

    // JPEG at two qualities.
    let lo = dir.join("low.jpg");
    s.describe("File ▸ Export ▸ JPEG at quality 20");
    export_item(s, "JPEG...")?;
    s.save_as(&lo)?;
    s.expect_node("Export JPEG")?;
    s.set_field("Quality", "20")?;
    s.click("Export")?;
    s.wait_idle()?;
    let hi = dir.join("high.jpg");
    export_item(s, "JPEG...")?;
    s.save_as(&hi)?;
    s.set_field("Quality", "95")?;
    s.click("Export")?;
    s.wait_idle()?;
    let (l, h) = (file_len(&lo), file_len(&hi));
    s.check(
        "quality 20 is smaller than quality 95",
        l > 0 && h > l,
        "0 < low < high",
        format!("{l} < {h}"),
    )?;
    let j = image::open(&hi)
        .map(|i| i.to_rgb8())
        .ok()
        .map(|i| (i.dimensions(), i.get_pixel(10, 10).0));
    s.check(
        "JPEG: 200 × 100, red top left",
        j.is_some_and(|(d, p)| d == (200, 100) && p[0] > 240 && p[1] < 20),
        "(200, 100), ≈[255, 0, 0]",
        format!("{j:?}"),
    )?;

    // PDF: one page at print size (72 ppi: 200 × 100 pt).
    let pdf = dir.join("out.pdf");
    export(s, "PDF...", &pdf)?;
    let bytes = std::fs::read(&pdf).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    s.check(
        "PDF: a 200 × 100 pt page",
        bytes.starts_with(b"%PDF") && text.contains("/MediaBox [0 0 200 100]"),
        "%PDF… /MediaBox [0 0 200 100]",
        format!(
            "{} bytes, MediaBox {:?}",
            bytes.len(),
            text.find("/MediaBox")
                .map(|i| text[i..].chars().take(30).collect::<String>())
        ),
    )?;

    // PSD, 8 and 16 bits: layers kept.
    let psd8 = dir.join("out.psd");
    export(s, "Photoshop PSD...", &psd8)?;
    let psd16 = dir.join("out16.psd");
    export(s, "Photoshop PSD (16-bit)...", &psd16)?;
    let depth = |p: &Path| {
        std::fs::read(p)
            .ok()
            .and_then(|b| b.get(22..24).map(|d| u16::from_be_bytes([d[0], d[1]])))
    };
    ck!(
        s,
        "PSD bit depths 8 and 16",
        (depth(&psd8), depth(&psd16)),
        (Some(8), Some(16))
    )?;
    let names = |p: &Path| {
        lumenply_io::psd::load(p).ok().map(|r| {
            r.value
                .layers()
                .iter()
                .map(|l| l.name.clone())
                .collect::<Vec<_>>()
        })
    };
    let want = Some(vec!["Background".to_string(), "patch.png".to_string()]);
    ck!(s, "the 8-bit PSD keeps both layers", names(&psd8), want.clone())?;
    ck!(s, "so does the 16-bit one", names(&psd16), want.clone())?;

    // OpenRaster: layers kept.
    let ora = dir.join("out.ora");
    export(s, "OpenRaster (.ora)...", &ora)?;
    let ora_names = lumenply_io::ora::load(&ora).ok().map(|r| {
        r.value
            .layers()
            .iter()
            .map(|l| l.name.clone())
            .collect::<Vec<_>>()
    });
    ck!(s, "the ORA keeps both layers", ora_names, want)?;

    // 16-bit PNG and TIFF.
    let p16 = dir.join("out16.png");
    s.describe("File ▸ Export ▸ 16-bit PNG");
    export_item(s, "16-bit PNG / TIFF...")?;
    s.save_as(&p16)?;
    let t16 = dir.join("out16.tif");
    s.describe("…and as a 16-bit TIFF");
    export_item(s, "16-bit PNG / TIFF...")?;
    s.save_as(&t16)?;
    let kind = |p: &Path| {
        image::open(p)
            .ok()
            .map(|i| (i.color(), i.width(), i.to_rgba16().get_pixel(10, 10).0))
    };
    ck!(
        s,
        "16-bit PNG: RGBA16, full red",
        kind(&p16),
        Some((image::ColorType::Rgba16, 200, [65535, 0, 0, 65535]))
    )?;
    ck!(
        s,
        "16-bit TIFF: RGBA16, full red",
        kind(&t16),
        Some((image::ColorType::Rgba16, 200, [65535, 0, 0, 65535]))
    )?;

    // OpenEXR, linear float.
    let exr = dir.join("out.exr");
    export(s, "OpenEXR (linear float)...", &exr)?;
    let e = lumenply_io::load(&exr).ok().map(|r| {
        let p = r.pixels[10 * r.width as usize + 10];
        (r.width, r.height, (p.r * 1000.0).round(), (p.g * 1000.0).round())
    });
    ck!(
        s,
        "EXR: 200 × 100, red is 1.0 linear",
        e,
        Some((200, 100, 1000.0, 0.0))
    )?;

    // Export As: JPEG at half size.
    s.describe("File ▸ Export ▸ Export As…");
    export_item(s, "Export As...")?;
    s.wait_idle()?;
    s.dump_tree("export-as");
    s.screenshot("export-as");
    s.click("JPEG")?;
    s.click("50%")?;
    s.wait_idle()?;
    s.expect_text("100 × 50 px")?;
    s.expect_text(" KB")?;
    s.click("Export…")?;
    let small = dir.join("small.jpg");
    s.save_as(&small)?;
    let d = image::open(&small).ok().map(|i| (i.width(), i.height()));
    ck!(s, "Export As wrote a 100 × 50 JPEG", d, Some((100, 50)))?;

    // A LUT needs an adjustment layer to bake.
    let lut = dir.join("look.cube");
    s.describe("A LUT bakes adjustment layers: without one it says so");
    s.click("File")?;
    s.click("Export")?;
    s.expect_disabled("Color Lookup Table (.cube)...", "Add an adjustment layer first")?;
    s.key("Esc")?;
    s.key("Esc")?;
    s.describe("Add an Invert adjustment layer for the LUT");
    s.menu("Layer > New adjustment layer > Invert")?;
    s.wait_idle()?;
    export(s, "Color Lookup Table (.cube)...", &lut)?;
    let cube = std::fs::read_to_string(&lut).unwrap_or_default();
    let first = cube
        .lines()
        .find(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string);
    s.check(
        "a 33³ cube whose first entry (black) maps to white",
        cube.contains("LUT_3D_SIZE 33") && first.as_deref().is_some_and(|f| f.starts_with("1")),
        "LUT_3D_SIZE 33, first entry ≈ 1 1 1",
        format!("{} bytes, first {first:?}", cube.len()),
    )?;
    Ok(())
}

/// The corpus PSD with a visible and a hidden shape layer.
const HIDDEN_LAYER_PSD: &str =
    "/Users/johan/Projects/14_NGE/nge/target/psd-corpus/psd-tools/tests/psd_files/hidden-layer.psd";

fn open_files(s: &mut Session) -> UiResult {
    let dir = s.files();
    let png = dir.join("gradient.png");
    write_gradient(&png, 240, 160)?;
    let jpeg = dir.join("halves.jpg");
    write_jpeg_halves(&jpeg, 200, 100)?;
    let psd = s.copy_in(HIDDEN_LAYER_PSD)?;
    let dng = dir.join("IMG_0001.dng");
    write_dng(&dng, 64, 48)?;
    let heic = dir.join("orange.heic");
    write_heic(&heic, 120, 80)?;
    let v1 = dir.join("old-project.lumen");
    write_format1(&v1)?;
    let broken = dir.join("broken.png");
    std::fs::write(&broken, b"not really a png").map_err(|e| io_err(&broken, e))?;

    s.describe("Click Open… on the welcome screen");
    s.click("Open…")?;
    s.describe("Pick gradient.png in the open panel");
    s.choose_file(&png)?;
    ck!(s, "the PNG opened at its size", doc_size(s)?, (240, 160, 72.0))?;
    let corners = (s.pixel(0, 0)?, s.pixel(239, 159)?);
    s.check(
        "its pixels are the file's",
        near(corners.0, [0, 0, 128, 255], 1) && near(corners.1, [255, 255, 128, 255], 1),
        "[0, 0, 128, 255] and [255, 255, 128, 255]",
        format!("{corners:?}"),
    )?;
    ck!(
        s,
        "the tab is named after the file",
        tabs(s)?,
        vec![("gradient.png".to_string(), false)]
    )?;
    ck!(s, "the opened image is not an undo step", s.history()?.len(), 0)?;

    s.describe("Open a JPEG with Cmd+O");
    s.key("Cmd+O")?;
    s.choose_file(&jpeg)?;
    ck!(s, "the JPEG opened at its size", doc_size(s)?.0, 200)?;
    let halves = (s.pixel(20, 50)?, s.pixel(180, 50)?);
    s.check(
        "red on the left, blue on the right",
        near(halves.0, [220, 30, 30, 255], 4) && near(halves.1, [30, 30, 220, 255], 4),
        "≈[220, 30, 30] and ≈[30, 30, 220]",
        format!("{halves:?}"),
    )?;

    s.describe("Open a Photoshop file from File ▸ Open...");
    s.menu("File > Open...")?;
    s.choose_file(&psd)?;
    s.wait_idle()?;
    ck!(s, "the PSD's size", doc_size(s)?.0, 100)?;
    ck!(
        s,
        "every layer came in, top first",
        s.layer_names()?,
        vec![
            "Shape 2".to_string(),
            "Shape 1".to_string(),
            "Background".to_string()
        ],
    )?;
    let hidden = s.doc(|d| find_layer(d, "Shape 2").map(|l| l.visible))?;
    ck!(s, "the hidden layer stays hidden", hidden, Some(false))?;
    let px = (s.pixel(40, 30)?, s.pixel(50, 66)?);
    s.check(
        "it looks as Photoshop saved it",
        near(px.0, [0, 0, 0, 255], 2) && near(px.1, [255, 255, 255, 255], 2),
        "black inside Shape 1, white where hidden Shape 2 is",
        format!("{px:?}"),
    )?;
    let st = status(s)?;
    s.note(&format!("Status after the PSD: {st}"))?;

    s.describe("Open a camera RAW (DNG)");
    s.key("Cmd+O")?;
    s.choose_file(&dng)?;
    s.wait_idle()?;
    s.screenshot("camera-raw");
    s.describe("Camera Raw opens on it: click Open");
    s.click("Open")?;
    s.wait_idle()?;
    let size = doc_size(s)?;
    ck!(s, "the RAW developed at its size", (size.0, size.1), (64, 48))?;
    let raw = (s.pixel(10, 24)?, s.pixel(54, 24)?);
    s.check(
        "the bright half is brighter, both neutral grey",
        raw.0[0] > raw.1[0] + 40 && raw.0[0].abs_diff(raw.0[1]) < 12 && raw.0[1].abs_diff(raw.0[2]) < 12,
        "left brighter than right, R≈G≈B",
        format!("{raw:?}"),
    )?;
    ck!(
        s,
        "the tab is named after the RAW",
        tabs(s)?.last().map(|t| t.0.clone()),
        Some("IMG_0001.dng".to_string())
    )?;

    s.describe("Open a HEIC photo");
    s.key("Cmd+O")?;
    s.choose_file(&heic)?;
    let size = doc_size(s)?;
    ck!(s, "the HEIC opened at its size", (size.0, size.1), (120, 80))?;
    let o = s.pixel(60, 40)?;
    s.check(
        "it is orange",
        near(o, [240, 140, 20, 255], 6),
        "≈[240, 140, 20]",
        format!("{o:?}"),
    )?;

    s.describe("Open a project saved by an older Lumenply (format 1)");
    s.key("Cmd+O")?;
    s.choose_file(&v1)?;
    ck!(s, "its size and resolution", doc_size(s)?, (300, 200, 200.0))?;
    ck!(
        s,
        "its layers",
        s.layer_names()?,
        vec!["Top".to_string(), "Background".to_string()]
    )?;
    let op = s.doc(|d| find_layer(d, "Top").map(|l| l.opacity))?;
    ck!(s, "the Top layer is still at half opacity", op, Some(0.5))?;
    let mixed = s.pixel(100, 100)?;
    let bg = s.pixel(10, 10)?;
    s.check(
        "green outside, half red over green inside",
        near(bg, [0, 188, 0, 255], 2) && mixed[0] > 150 && mixed[1] > 60,
        "≈[0, 188, 0] outside; red and green mixed inside",
        format!("{bg:?} / {mixed:?}"),
    )?;
    ck!(
        s,
        "the tab shows the project's name, saved",
        tabs(s)?.last().cloned(),
        Some(("old-project.lumen".to_string(), false))
    )?;

    s.describe("Opening a file that is open already brings its tab forward");
    s.key("Cmd+O")?;
    s.choose_file(&png)?;
    ck!(s, "back on gradient.png", doc_size(s)?, (240, 160, 72.0))?;
    ck!(s, "no second copy was opened", tabs(s)?.len(), 6)?;

    s.describe("Place an image as a layer from File ▸ Place");
    let patch = dir.join("patch.png");
    write_solid(&patch, 40, 20, [0, 255, 0, 255])?;
    s.menu("File > Place image as layer...")?;
    s.choose_file(&patch)?;
    s.wait_idle()?;
    ck!(
        s,
        "a new layer named after the file",
        s.layer_names()?.first().cloned(),
        Some("patch.png".to_string())
    )?;
    // Centred: (240 - 40) / 2 = 100, (160 - 20) / 2 = 70.
    let placed = (
        s.layer_pixel("patch.png", 100, 70)?,
        s.layer_pixel("patch.png", 99, 70)?,
    );
    ck!(
        s,
        "centred in the document",
        placed,
        (Some([0, 255, 0, 255]), Some([0, 0, 0, 0]))
    )?;
    s.describe("Undo the place");
    s.key("Cmd+Z")?;
    ck!(
        s,
        "the placed layer is gone",
        s.layer_names()?,
        vec!["Background".to_string()]
    )?;

    s.describe("Drag a PNG from the Finder onto the open document");
    s.drop_files(std::slice::from_ref(&patch))?;
    ck!(
        s,
        "it is placed as a layer",
        s.layer_names()?.first().cloned(),
        Some("patch.png".to_string())
    )?;
    s.describe("Drag the PSD onto the window");
    s.drop_files(std::slice::from_ref(&psd))?;
    ck!(s, "the PSD's tab comes forward", doc_size(s)?.0, 100)?;

    s.describe("Try to open a damaged file");
    s.key("Cmd+O")?;
    s.choose_file(&broken)?;
    let st = status(s)?;
    s.check(
        "it says it could not open it, and why",
        st.starts_with("Could not open") && st.contains("broken.png"),
        "Could not open …broken.png: <reason>",
        st.clone(),
    )?;
    ck!(s, "nothing new opened", tabs(s)?.len(), 6)?;
    s.expect_text("Could not open")?;

    s.describe("Open recent lists what was opened");
    s.click("File")?;
    s.click("Open recent")?;
    s.expect_node("halves.jpg")?;
    s.expect_node("IMG_0001.dng")?;
    s.key("Esc")?;
    Ok(())
}
