//! Edit ▸ Cut / Copy / Copy merged / Paste / Paste in place for pixels.
//!
//! Copies keep full precision internally and also go to the system
//! clipboard as an 8-bit sRGB image (with a fingerprint), so pasting into
//! other apps works; a paste prefers the system clipboard's image unless it
//! is the one we put there, in which case the precise copy is used.
//! egui swallows Cmd+V when the clipboard holds no text, so on macOS an
//! AppKit key monitor reports it.

use super::*;
use lumenply_core::paste::{copy_region, PasteLayer};
use lumenply_tiles::{Raster, Rgba, TileStore};
use std::sync::atomic::{AtomicU8, Ordering};

/// Set by the macOS key monitor: 1 = Cmd+V, 2 = Shift+Cmd+V.
static PASTE_KEY: AtomicU8 = AtomicU8::new(0);

/// What Copy last took, at full precision.
pub(crate) struct Clip {
    raster: Raster,
    x: i32,
    y: i32,
    /// Fingerprint of the 8-bit image we put on the system clipboard.
    print: u64,
}

fn rgba8(r: &Raster) -> Vec<u8> {
    let mut out = Vec::with_capacity(r.pixels.len() * 4);
    for p in &r.pixels {
        let [cr, cg, cb, a] = p.to_straight();
        out.extend_from_slice(&[
            lumenply_io::linear_to_srgb(cr),
            lumenply_io::linear_to_srgb(cg),
            lumenply_io::linear_to_srgb(cb),
            (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        ]);
    }
    out
}

fn fingerprint(w: usize, h: usize, bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut s = std::collections::hash_map::DefaultHasher::new();
    (w, h).hash(&mut s);
    bytes.hash(&mut s);
    s.finish()
}

fn from_rgba8(w: usize, h: usize, bytes: &[u8]) -> Raster {
    let mut r = Raster::new(w as u32, h as u32);
    for (p, c) in r.pixels.iter_mut().zip(bytes.chunks_exact(4)) {
        *p = Rgba::from_straight(
            lumenply_io::srgb_to_linear(c[0]),
            lumenply_io::srgb_to_linear(c[1]),
            lumenply_io::srgb_to_linear(c[2]),
            c[3] as f32 / 255.0,
        );
    }
    r
}

/// The system clipboard's image, if any (never touched by unit tests).
fn system_image() -> Option<(usize, usize, Vec<u8>)> {
    if cfg!(test) {
        return None;
    }
    let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
    Some((img.width, img.height, img.bytes.into_owned()))
}

fn set_system_image(w: usize, h: usize, bytes: &[u8]) {
    if cfg!(test) {
        return;
    }
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_image(arboard::ImageData {
            width: w,
            height: h,
            bytes: std::borrow::Cow::Borrowed(bytes),
        });
    }
}

impl App {
    /// Cmd+C (merged: Shift+Cmd+C): the selected pixels of the active
    /// layer, or of the whole visible image.
    pub(crate) fn copy_pixels(&mut self, merged: bool) -> bool {
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let sel = doc.selection.as_ref();
        let got = if merged {
            let flat = lumenply_render::composite_raster(doc);
            copy_region(&TileStore::from_raster(&flat, 0, 0), sel, canvas)
        } else {
            match self.active_layer().and_then(|l| l.raster_store()) {
                Some(store) => copy_region(store, sel, canvas),
                None => {
                    self.status = "Select a layer with pixels to copy".into();
                    return false;
                }
            }
        };
        let Some((raster, x, y)) = got else {
            self.status = "Nothing to copy there".into();
            return false;
        };
        let bytes = rgba8(&raster);
        let (w, h) = (raster.width as usize, raster.height as usize);
        let print = fingerprint(w, h, &bytes);
        set_system_image(w, h, &bytes);
        self.status = format!("Copied {w} × {h} px{}", if merged { " (merged)" } else { "" });
        self.clip = Some(Clip { raster, x, y, print });
        true
    }

    /// Cmd+X: copy, then clear the selected pixels of the active layer.
    pub(crate) fn cut_pixels(&mut self) {
        if !self.active_is_pixel() {
            self.status = "Cut works on a pixel layer".into();
            return;
        }
        if self.copy_pixels(false) {
            self.run_menu_action("clear");
            self.status = "Cut to the clipboard".into();
        }
    }

    /// Cmd+V (Shift+Cmd+V: in place): a new layer above the active one.
    /// Our own copies land where they came from; images from other apps
    /// land centred on the canvas.
    pub(crate) fn paste_pixels(&mut self, in_place: bool) {
        let _ = in_place; // Our copies always return to their own place.
        let canvas = self.editor.doc().canvas();
        let (raster, x, y) = match system_image() {
            Some((w, h, bytes))
                if self
                    .clip
                    .as_ref()
                    .is_none_or(|c| c.print != fingerprint(w, h, &bytes)) =>
            {
                let r = from_rgba8(w, h, &bytes);
                let x = canvas.x + (canvas.w as i32 - w as i32) / 2;
                let y = canvas.y + (canvas.h as i32 - h as i32) / 2;
                (r, x, y)
            }
            _ => match &self.clip {
                Some(c) => (c.raster.clone(), c.x, c.y),
                None => {
                    self.status = "The clipboard holds no image".into();
                    return;
                }
            },
        };
        let id = self.editor.doc().next_id();
        let (w, h) = (raster.width, raster.height);
        self.run(&PasteLayer {
            name: "Pasted".into(),
            raster,
            x,
            y,
            above: self.active,
        });
        if self.editor.doc().layer(id).is_some() {
            self.set_active(Some(id));
            self.status = format!("Pasted {w} × {h} px as a new layer");
        }
    }

    /// Copy / Cut / Paste keys, unless a text field has them.
    pub(crate) fn clipboard_keys(&mut self, ctx: &egui::Context) {
        let mac_paste = PASTE_KEY.swap(0, Ordering::Relaxed);
        if ctx.wants_keyboard_input() {
            return;
        }
        let (copy, cut, paste, shift) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.events.iter().any(|e| matches!(e, egui::Event::Cut)),
                i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                i.modifiers.shift,
            )
        });
        if copy {
            self.copy_pixels(shift);
        }
        if cut {
            self.cut_pixels();
        }
        // On macOS the key monitor reports every Cmd+V (egui only does when
        // the clipboard holds text); elsewhere the egui event is all there is.
        if mac_paste > 0 {
            self.paste_pixels(mac_paste == 2);
        } else if paste && !cfg!(target_os = "macos") {
            self.paste_pixels(shift);
        }
    }
}

/// Starts the macOS Cmd+V monitor (main thread, after the app exists).
#[cfg(target_os = "macos")]
pub(crate) fn install() {
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
    use std::ptr::NonNull;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let block = block2::RcBlock::new(|ev: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the monitor a live key event.
            let e = unsafe { ev.as_ref() };
            let flags = e.modifierFlags();
            if flags.contains(NSEventModifierFlags::Command)
                && !flags.contains(NSEventModifierFlags::Option)
                && !flags.contains(NSEventModifierFlags::Control)
                && e.charactersIgnoringModifiers()
                    .is_some_and(|c| c.to_string().eq_ignore_ascii_case("v"))
            {
                let shift = flags.contains(NSEventModifierFlags::Shift);
                PASTE_KEY.store(if shift { 2 } else { 1 }, Ordering::Relaxed);
            }
            ev.as_ptr()
        });
        // SAFETY: the block returns the event it was given, never null.
        let monitor =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) };
        std::mem::forget(monitor);
    });
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_then_paste_makes_a_layer_with_the_same_pixels_in_place() {
        let mut doc = Document::new(30, 20);
        let id = doc.add_pixel_layer("photo");
        for y in 2..8 {
            for x in 3..9 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(0.25, 0.5, 0.125, 1.0),
                );
            }
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        app.run(&SetSelection {
            selection: Some(lumenply_doc::Selection::rect(lumenply_tiles::Rect::new(
                4, 3, 3, 3,
            ))),
        });
        assert!(app.copy_pixels(false));
        app.paste_pixels(false);
        let pasted = app.active.unwrap();
        assert_ne!(pasted, id);
        let px = app.editor.doc().layer(pasted).unwrap().pixels().unwrap();
        // Exact up to the 16-bit storage tiles rest in.
        let p = px.get_pixel(4, 3);
        assert!(
            (p.r - 0.25).abs() < 1e-4 && (p.g - 0.5).abs() < 1e-4 && (p.b - 0.125).abs() < 1e-4 && p.a == 1.0,
            "in place: {p:?}"
        );
        assert_eq!(px.get_pixel(7, 3).a, 0.0, "only the selection");
        // Cut clears the source under the selection.
        app.set_active(Some(id));
        app.cut_pixels();
        let src = app.editor.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(src.get_pixel(5, 4).a, 0.0);
        assert_eq!(src.get_pixel(3, 2).a, 1.0, "outside the selection stays");
    }

    #[test]
    fn eight_bit_round_trip_is_close() {
        let mut r = Raster::new(2, 1);
        r.pixels[0] = Rgba::from_straight(0.2, 0.4, 0.6, 1.0);
        r.pixels[1] = Rgba::from_straight(1.0, 0.0, 0.0, 0.5);
        let back = from_rgba8(2, 1, &rgba8(&r));
        for (a, b) in r.pixels.iter().zip(&back.pixels) {
            assert!(
                (a.r - b.r).abs() < 0.01 && (a.a - b.a).abs() < 0.01,
                "{a:?} vs {b:?}"
            );
        }
    }
}
