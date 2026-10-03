//! Edit ▸ Content-Aware Scale: resize a pixel layer by seam carving
//! (`lumenply_render::seam_carve`), so flat areas give way while detailed
//! or protected content keeps its shape. One command, one undo step.

use lumenply_doc::{Document, LayerContent, LayerId, Mask};
use lumenply_render::seam_carve::{content_aware_scale_carry, CarveOptions};
use lumenply_tiles::{Rect, TileStore};

use crate::{Command, EditError, EditResult, Motion};

/// The largest side Content-Aware Scale may produce.
pub const MAX_SCALE_SIDE: u32 = 16_384;

/// Scale the painted bounds of a pixel layer to `new_w × new_h`.
///
/// - `amount` (0..=1) is the share of the size change done by carving;
///   the rest is a plain Lanczos resample (Photoshop's Amount / 100).
/// - `protect` is canvas-space coverage (the selection's, or a saved
///   channel's): seams avoid it.
/// - `skin` also protects skin-coloured pixels.
/// - `origin` is where the scaled bounds' top-left lands; `None` keeps the
///   old top-left.
///
/// The layer mask is carved along the same seams, so it stays registered.
/// Text, smart objects, shapes and fills are refused with a reason
/// (rasterize first), as the perspective and warp transforms do.
#[derive(Clone)]
pub struct ContentAwareScale {
    pub layer: LayerId,
    pub new_w: u32,
    pub new_h: u32,
    pub origin: Option<(i32, i32)>,
    pub amount: f32,
    pub protect: Option<Mask>,
    pub skin: bool,
}

impl ContentAwareScale {
    /// What gets scaled: the layer's painted bounds.
    pub fn source_rect(doc: &Document, layer: LayerId) -> Option<Rect> {
        doc.layer(layer)?.pixels()?.content_bounds()
    }

    /// Where the result lands, for the current document.
    pub fn result_rect(&self, doc: &Document) -> Option<Rect> {
        let b = Self::source_rect(doc, self.layer)?;
        let (x, y) = self.origin.unwrap_or((b.x, b.y));
        Some(Rect::new(x, y, self.new_w.max(1), self.new_h.max(1)))
    }
}

impl Command for ContentAwareScale {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        Motion::Reshape
    }

    fn label(&self) -> String {
        "Content-Aware Scale".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let old = Self::source_rect(doc, self.layer)?;
        let new = self.result_rect(doc)?;
        Some(old.union(&new).intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.new_w == 0 || self.new_h == 0 {
            return Err(EditError::Invalid("the new size must be at least 1 px".into()));
        }
        if self.new_w > MAX_SCALE_SIDE || self.new_h > MAX_SCALE_SIDE {
            return Err(EditError::Invalid(format!(
                "Content-Aware Scale goes up to {MAX_SCALE_SIDE} px a side"
            )));
        }
        if !self.amount.is_finite() {
            return Err(EditError::Invalid("the amount is not a number".into()));
        }
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let reason = match &l.content {
            LayerContent::Pixel(_) => None,
            LayerContent::Text(_) => Some("rasterize the text layer before Content-Aware Scale"),
            LayerContent::Smart(_) => {
                Some("smart objects keep affine transforms only; rasterize before Content-Aware Scale")
            }
            LayerContent::Shape(_) => Some("rasterize the shape before Content-Aware Scale"),
            LayerContent::Fill(_) => Some("rasterize the fill layer before Content-Aware Scale"),
            _ => return Err(EditError::NotPixel(self.layer)),
        };
        if let Some(r) = reason {
            return Err(EditError::Invalid(r.into()));
        }
        let LayerContent::Pixel(store) = &mut l.content else {
            return Err(EditError::NotPixel(self.layer));
        };
        let b = store
            .content_bounds()
            .ok_or_else(|| EditError::Invalid("the layer has no pixels to scale".into()))?;
        let src = store.to_raster(b);
        let protect = self.protect.as_ref().map(|m| m.to_dense(b));
        let carry = l.mask.as_ref().map(|m| m.to_dense(b));
        let opts = CarveOptions {
            amount: self.amount,
            protect: protect.as_deref(),
            skin: self.skin,
        };
        let (out, carried) = content_aware_scale_carry(&src, carry.as_deref(), self.new_w, self.new_h, &opts);
        let (ox, oy) = self.origin.unwrap_or((b.x, b.y));
        let LayerContent::Pixel(store) = &mut l.content else {
            unreachable!("checked above");
        };
        *store = TileStore::from_raster(&out, ox, oy);
        if let (Some(m), Some(c)) = (l.mask.as_mut(), carried) {
            // Old and new areas: the default everywhere, then the carved
            // mask where the pixels now are.
            let new = Rect::new(ox, oy, out.width, out.height);
            let all = b.union(&new);
            let mut buf = vec![m.default; all.w as usize * all.h as usize];
            for y in 0..new.h as usize {
                let row = (y + (new.y - all.y) as usize) * all.w as usize + (new.x - all.x) as usize;
                let src = y * new.w as usize;
                buf[row..row + new.w as usize].copy_from_slice(&c[src..src + new.w as usize]);
            }
            m.set_dense(all, &buf);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddMask, AddPixelLayer, AddTextLayer, ConvertToSmartObject};
    use crate::Editor;
    use lumenply_doc::TextLayer;
    use lumenply_tiles::{Raster, Rgba};

    fn noise(i: u32) -> f32 {
        let mut x = i.wrapping_mul(0x9E37_79B9) ^ 0xC0FFEE;
        x ^= x >> 15;
        x = x.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 13;
        (x & 0xFFFF) as f32 / 65535.0
    }

    /// 120 × 60: flat grey left half, a detailed "object" right half.
    fn editor_with_photo() -> (Editor, LayerId, Raster) {
        let mut r = Raster::filled(120, 60, Rgba::new(0.4, 0.4, 0.4, 1.0));
        for y in 0..60 {
            for x in 60..120 {
                let v = noise(y * 977 + x);
                r.set(x, y, Rgba::new(v, 1.0 - v, noise(x * 31 + y), 1.0));
            }
        }
        let mut ed = Editor::new(Document::new(120, 60));
        ed.execute(&AddPixelLayer::from_raster("Photo", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        // Read back what the document holds (tiles rest as 16-bit).
        let held = ed
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .to_raster(Rect::new(0, 0, 120, 60));
        (ed, id, held)
    }

    fn cas(layer: LayerId, w: u32, h: u32) -> ContentAwareScale {
        ContentAwareScale {
            layer,
            new_w: w,
            new_h: h,
            origin: None,
            amount: 1.0,
            protect: None,
            skin: false,
        }
    }

    fn close(a: Rgba, b: Rgba) -> bool {
        (a.r - b.r).abs() < 1e-4
            && (a.g - b.g).abs() < 1e-4
            && (a.b - b.b).abs() < 1e-4
            && (a.a - b.a).abs() < 1e-4
    }

    #[test]
    fn narrowing_takes_the_flat_part_and_is_one_undo_step() {
        let (mut ed, id, held) = editor_with_photo();
        let steps = ed.history().len();
        let cmd = cas(id, 84, 60);
        assert_eq!(cmd.affected(ed.doc()), Some(Rect::new(0, 0, 120, 60)));
        ed.execute(&cmd).unwrap();
        assert_eq!(ed.history().len(), steps + 1);
        assert_eq!(ed.history().last().copied(), Some("Content-Aware Scale"));
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.content_bounds(), Some(Rect::new(0, 0, 84, 60)));
        // The object moved left by 36 px, unchanged.
        for y in 0..60 {
            for x in 60..120 {
                assert!(
                    close(px.get_pixel(x - 36, y), held.get(x as u32, y as u32)),
                    "({x}, {y})"
                );
            }
        }
        assert!(close(px.get_pixel(10, 30), Rgba::new(0.4, 0.4, 0.4, 1.0)));
        ed.undo();
        let back = ed.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(back.content_bounds(), Some(Rect::new(0, 0, 120, 60)));
    }

    #[test]
    fn a_saved_channel_protects_and_origin_places_the_result() {
        let (ed, id, held) = editor_with_photo();
        // Protect the left 30 columns of the flat half: the 24 seams must
        // then come from columns 30..60.
        let mut protect = Mask::hide_all();
        protect.fill_rect(Rect::new(0, 0, 30, 60), 1.0);
        // Mark the protected part so its identity can be checked.
        let mut cmd = cas(id, 96, 60);
        cmd.protect = Some(protect);
        cmd.origin = Some((24, 0));
        let mut doc = ed.doc().clone();
        {
            let s = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
            for y in 0..60 {
                for x in 0..30 {
                    s.set_pixel(x, y, Rgba::new(0.4, 0.4, 0.4 + (x as f32) * 1e-3, 1.0));
                }
            }
        }
        let marked = doc.layer(id).unwrap().pixels().unwrap().clone();
        cmd.apply(&mut doc).unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.content_bounds(), Some(Rect::new(24, 0, 96, 60)));
        for y in 0..60 {
            for x in 0..30 {
                assert!(
                    close(px.get_pixel(24 + x, y), marked.get_pixel(x, y)),
                    "protected ({x}, {y})"
                );
            }
            for x in 60..120 {
                assert!(
                    close(px.get_pixel(x, y), held.get(x as u32, y as u32)),
                    "object ({x}, {y})"
                );
            }
        }
        assert_eq!(
            ed.doc().layer(id).unwrap().pixels().unwrap().content_bounds(),
            Some(Rect::new(0, 0, 120, 60))
        );
    }

    #[test]
    fn widening_inserts_seams_to_the_exact_size() {
        let (mut ed, id, held) = editor_with_photo();
        ed.execute(&cas(id, 150, 72)).unwrap();
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.content_bounds(), Some(Rect::new(0, 0, 150, 72)));
        // The flat half took all 30 new columns; the object only grew taller
        // (its rows are not checked), so check a row-identical case instead.
        let (mut ed2, id2, _) = editor_with_photo();
        ed2.execute(&cas(id2, 150, 60)).unwrap();
        let px2 = ed2.doc().layer(id2).unwrap().pixels().unwrap();
        for y in 0..60 {
            for x in 60..120 {
                assert!(
                    close(px2.get_pixel(x + 30, y), held.get(x as u32, y as u32)),
                    "({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn the_layer_mask_is_carved_with_the_pixels() {
        let (mut ed, id, _) = editor_with_photo();
        ed.execute(&AddMask { layer: id }).unwrap();
        // Hide the object half in the mask.
        let mut doc = ed.doc().clone();
        doc.layer_mut(id)
            .unwrap()
            .mask
            .as_mut()
            .unwrap()
            .fill_rect(Rect::new(60, 0, 60, 60), 0.0);
        let mut ed = Editor::new(doc);
        ed.execute(&cas(id, 84, 60)).unwrap();
        let m = ed.doc().layer(id).unwrap().mask.clone().unwrap();
        let v = |x, y| m.to_dense(Rect::new(x, y, 1, 1))[0];
        assert_eq!(v(23, 30), 1.0, "flat part still shown");
        assert_eq!(v(24, 30), 0.0, "the hidden object starts at 60 - 36");
        assert_eq!(v(83, 30), 0.0);
        assert_eq!(v(100, 30), 1.0, "beyond the new edge: the mask's default");
    }

    #[test]
    fn amount_zero_matches_a_plain_resample() {
        let (mut ed, id, held) = editor_with_photo();
        let mut cmd = cas(id, 77, 41);
        cmd.amount = 0.0;
        ed.execute(&cmd).unwrap();
        let px = ed
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .to_raster(Rect::new(0, 0, 77, 41));
        let plain = lumenply_render::resample::resample(&held, 77, 41);
        let worst = px
            .pixels
            .iter()
            .zip(&plain.pixels)
            .map(|(a, b)| (a.r - b.r).abs().max((a.b - b.b).abs()))
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn non_pixel_layers_and_bad_sizes_are_refused_with_a_reason() {
        let (mut ed, id, _) = editor_with_photo();
        assert!(ed.execute(&cas(id, 0, 60)).is_err());
        assert!(ed.execute(&cas(id, MAX_SCALE_SIDE + 1, 60)).is_err());
        let mut nan = cas(id, 50, 60);
        nan.amount = f32::NAN;
        assert!(ed.execute(&nan).is_err());
        ed.execute(&AddTextLayer {
            text: TextLayer::new("Hi", 5.0, 5.0, 12.0, [1.0; 4]),
            above: None,
        })
        .unwrap();
        let tid = ed.doc().layers().last().unwrap().id;
        let err = ed.execute(&cas(tid, 50, 20)).unwrap_err().to_string();
        assert!(err.contains("rasterize the text layer"), "{err}");
        ed.execute(&ConvertToSmartObject { layer: id }).unwrap();
        let err = ed.execute(&cas(id, 50, 20)).unwrap_err().to_string();
        assert!(err.contains("smart objects"), "{err}");
    }

    /// Times the demo photo (1800 × 1205) narrowed to 70 %. Run with
    /// `cargo test --release -p lumenply-core -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn timing_on_the_demo_photo() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../app/assets/demo-photo.jpg");
        let img = image::open(path).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        let mut r = Raster::new(w, h);
        for (x, y, p) in img.enumerate_pixels() {
            let f = |v: u8| lumenply_doc::adjust::srgb_decode(v as f32 / 255.0);
            r.set(x, y, Rgba::new(f(p[0]), f(p[1]), f(p[2]), 1.0));
        }
        let mut ed = Editor::new(Document::new(w, h));
        ed.execute(&AddPixelLayer::from_raster("Photo", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        // LUMENPLY_CAS_OUT=dir also writes each result as a PNG.
        let out_dir = std::env::var("LUMENPLY_CAS_OUT").ok();
        for (tw, th) in [
            (w * 7 / 10, h),
            (w * 65 / 100, h),
            (w, h * 8 / 10),
            (w * 13 / 10, h),
        ] {
            let t = std::time::Instant::now();
            let mut doc = ed.doc().clone();
            cas(id, tw, th).apply(&mut doc).unwrap();
            println!(
                "{w}x{h} -> {tw}x{th}: {:.0} ms",
                t.elapsed().as_secs_f64() * 1000.0
            );
            if let Some(dir) = &out_dir {
                let r = doc
                    .layer(id)
                    .unwrap()
                    .pixels()
                    .unwrap()
                    .to_raster(Rect::new(0, 0, tw, th));
                let mut img = image::RgbaImage::new(tw, th);
                for (x, y, p) in img.enumerate_pixels_mut() {
                    let q = r.get(x, y);
                    let f = |v: f32| (lumenply_doc::adjust::srgb_encode(v) * 255.0).round() as u8;
                    *p = image::Rgba([f(q.r), f(q.g), f(q.b), 255]);
                }
                img.save(format!("{dir}/cas-{tw}x{th}.png")).unwrap();
            }
        }
    }
}
