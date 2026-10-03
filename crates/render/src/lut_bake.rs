//! Bake a document's adjustment layers into one 3D LUT — Photoshop's
//! File ▸ Export ▸ Color Lookup Tables.
//!
//! Every lattice colour of an `N³` cube becomes one pixel of a scratch
//! document; the visible adjustment layers are stacked above it in their
//! order, with their opacity and blend mode, and the reference compositor
//! renders the result, so the table is exactly what the adjustments do.
//! Masks, clipping and every pixel, filter or fill layer are left out:
//! they depend on where a pixel is, which a colour table cannot express.

use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::{Document, Layer, LayerContent, Lut3D};
use lumenply_tiles::Rgba;

/// The visible adjustment layers, bottom to top, inside visible groups
/// too, as unmasked, unclipped copies.
fn adjustment_layers(layers: &[Layer], out: &mut Vec<Layer>) {
    for l in layers {
        if !l.visible {
            continue;
        }
        match &l.content {
            LayerContent::Adjustment(_) => {
                let mut c = l.clone();
                c.mask = None;
                c.clip = false;
                out.push(c);
            }
            LayerContent::Group(children) => adjustment_layers(children, out),
            _ => {}
        }
    }
}

/// The document's adjustments as a `size`³ table on gamma-encoded RGB
/// (what LUT files expect), titled `title`; also how many adjustment
/// layers went into it (none means the table is the identity).
pub fn bake_adjustments(doc: &Document, size: usize, title: &str) -> (Lut3D, usize) {
    let mut adjustments = Vec::new();
    adjustment_layers(doc.layers(), &mut adjustments);
    let lattice = Lut3D::identity(size);
    let n = lattice.size;
    // One pixel per lattice point: x = r + n·g, y = b (red fastest).
    let (w, h) = ((n * n) as u32, n as u32);
    let mut scratch = Document::new(w, h);
    let base = scratch.add_pixel_layer("Lattice");
    if let Some(LayerContent::Pixel(store)) = scratch.layer_mut(base).map(|l| &mut l.content) {
        for (i, c) in lattice.data.iter().enumerate() {
            let [r, g, b] = c.map(srgb_decode);
            store.set_pixel(
                (i % (n * n)) as i32,
                (i / (n * n)) as i32,
                Rgba::new(r, g, b, 1.0),
            );
        }
    }
    let count = adjustments.len();
    for mut l in adjustments {
        l.id = scratch.alloc_id();
        scratch.add_layer(l);
    }
    let out = crate::composite(&scratch);
    let mut lut = lattice;
    for (i, c) in lut.data.iter_mut().enumerate() {
        let [r, g, b, _] = out
            .get_pixel((i % (n * n)) as i32, (i / (n * n)) as i32)
            .to_straight();
        *c = [r, g, b].map(srgb_encode);
    }
    lut.title = title.to_string();
    (lut, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::{Adjustment, BlendMode, Mask};

    fn near(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() < tol)
    }

    #[test]
    fn invert_and_opacity_bake_into_the_table() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("Photo");
        let id = doc.alloc_id();
        doc.add_layer(Layer::adjustment(id, Adjustment::Invert));
        let (lut, count) = bake_adjustments(&doc, 17, "Inverted");
        assert_eq!((count, lut.size, lut.title.as_str()), (1, 17, "Inverted"));
        // Lattice (r=4, g=8, b=16)/16 = (0.25, 0.5, 1) → (0.75, 0.5, 0).
        assert!(near(lut.data[4 + 17 * (8 + 17 * 16)], [0.75, 0.5, 0.0], 1e-3));
        assert!(near(lut.apply([0.25, 0.5, 1.0]), [0.75, 0.5, 0.0], 1e-3));
        // At 50% the mix runs in linear light: sRGB 0.25 and 0.75 meet at
        // linear (0.050876 + 0.522522) / 2 = 0.286699, sRGB 0.571874.
        doc.layer_mut(id).unwrap().opacity = 0.5;
        let (lut, _) = bake_adjustments(&doc, 17, "");
        let v = lut.data[4];
        assert!((v[0] - 0.571874).abs() < 1e-3, "{v:?}");
    }

    #[test]
    fn hidden_layers_and_masks_are_left_out() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("Photo");
        let id = doc.alloc_id();
        let mut inv = Layer::adjustment(id, Adjustment::Invert);
        inv.visible = false;
        doc.add_layer(inv);
        let (lut, count) = bake_adjustments(&doc, 5, "");
        assert_eq!(count, 0);
        assert!(lut.is_identity());
        // A masked Threshold counts in full: masks are spatial.
        let id = doc.alloc_id();
        let mut t = Layer::adjustment(id, Adjustment::Threshold { level: 0.5 });
        t.mask = Some(Mask::hide_all());
        t.blend = BlendMode::Normal;
        doc.add_layer(t);
        let (lut, count) = bake_adjustments(&doc, 5, "");
        assert_eq!(count, 1);
        assert!(near(lut.apply([0.75, 0.75, 0.75]), [1.0; 3], 1e-3));
        assert!(near(lut.apply([0.25, 0.25, 0.25]), [0.0; 3], 1e-3));
    }
}
