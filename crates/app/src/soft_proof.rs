//! View ▸ Proof Colors (Cmd+Y) and View ▸ Gamut Warning (Shift+Cmd+Y):
//! the canvas shows the document as it would print on U.S. Web Coated
//! (SWOP) v2 (see `lumenply_io::proof`). A view setting like zoom, kept
//! per window rather than in the document or the preferences (as in
//! Photoshop): the document's pixels never change, only the texture the
//! canvas draws.

use lumenply_io::proof::ProofLut;

use super::*;

/// The palette / menu / key ids.
pub(crate) const PROOF_COLORS: &str = "proof-colors";
pub(crate) const GAMUT_WARNING: &str = "gamut-warning";

/// What the status bar says while proofing.
pub(crate) const PROOF_NOTE: &str = "Proof: U.S. Web Coated (SWOP)";

/// How the view shows colours: the lookup, whether colours are proofed,
/// and whether out-of-gamut colours show as warning grey.
pub(crate) type ProofView = (&'static ProofLut, bool, bool);

impl App {
    /// The proof the canvas texture goes through; `None` when the canvas
    /// shows the document's own colours.
    pub(crate) fn proof_view(&self) -> Option<ProofView> {
        (self.proof_colors || self.gamut_warning)
            .then(|| (ProofLut::swop(), self.proof_colors, self.gamut_warning))
    }

    /// Run a proof toggle; false for any other id.
    pub(crate) fn run_proof_action(&mut self, id: &str) -> bool {
        let (flag, name) = match id {
            PROOF_COLORS => (&mut self.proof_colors, "Proof colors"),
            GAMUT_WARNING => (&mut self.gamut_warning, "Gamut warning"),
            _ => return false,
        };
        *flag = !*flag;
        let on = *flag;
        self.status = format!("{name} {}", if on { "on (U.S. Web Coated SWOP)" } else { "off" });
        // The whole texture changes (and the Navigator's); the document
        // does not.
        self.mark(None);
        self.panels.nav_stale = true;
        true
    }
}

/// One display colour through the proof (the Navigator's thumbnail).
pub(crate) fn proof_color32(c: Color32, proof: Option<ProofView>) -> Color32 {
    let Some((lut, colors, warn)) = proof else {
        return c;
    };
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let [r, g, b] = lut.apply([r, g, b], colors, warn);
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// [`raster_to_image`] through the proof, when there is one: the
/// straight 8-bit sRGB of each pixel is proofed, alpha kept.
pub(crate) fn display_image(flat: &Raster, proof: Option<ProofView>) -> egui::ColorImage {
    let Some((lut, colors, warn)) = proof else {
        return crate::canvas::raster_to_image(flat);
    };
    let enc = crate::canvas::srgb_lut();
    let enc = |v: f32| enc[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize];
    use rayon::prelude::*;
    egui::ColorImage {
        size: [flat.width as usize, flat.height as usize],
        pixels: flat
            .pixels
            .par_iter()
            .map(|p| {
                let [r, g, b, a] = p.to_straight();
                let [r, g, b] = lut.apply([enc(r), enc(g), enc(b)], colors, warn);
                Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    fn raster(px: &[[u8; 3]]) -> Raster {
        let mut r = Raster::new(px.len() as u32, 1);
        for (i, c) in px.iter().enumerate() {
            let [cr, cg, cb] = c.map(lumenply_io::srgb_to_linear);
            r.set(i as u32, 0, Rgba::new(cr, cg, cb, 1.0));
        }
        r
    }

    #[test]
    fn the_proof_dulls_blue_and_keeps_greys_and_the_document() {
        let flat = raster(&[[0, 0, 255], [128, 128, 128], [224, 172, 140]]);
        let plain = display_image(&flat, None);
        assert_eq!(plain.pixels[0], Color32::from_rgb(0, 0, 255));
        let lut = ProofLut::swop();
        let proofed = display_image(&flat, Some((lut, true, false)));
        let [r, g, b, a] = proofed.pixels[0].to_array();
        assert!(
            (r as i32 - 35).abs() <= 1 && (g as i32 - 75).abs() <= 1,
            "{r} {g} {b}"
        );
        assert!((b as i32 - 165).abs() <= 1 && a == 255, "{r} {g} {b} {a}");
        assert_eq!(proofed.pixels[1], Color32::from_rgb(128, 128, 128));
        let warned = display_image(&flat, Some((lut, true, true)));
        assert_eq!(warned.pixels[0], Color32::from_rgb(128, 128, 128));
        assert_eq!(warned.pixels[2], proofed.pixels[2]);
        // Gamut warning alone greys blue and leaves the rest untouched.
        let only = display_image(&flat, Some((lut, false, true)));
        assert_eq!(only.pixels[0], Color32::from_rgb(128, 128, 128));
        assert_eq!(only.pixels[1..], plain.pixels[1..]);
        // The Navigator's single-colour path agrees with the canvas.
        assert_eq!(
            proof_color32(plain.pixels[0], Some((lut, true, false))),
            proofed.pixels[0]
        );
        assert_eq!(proof_color32(plain.pixels[0], None), plain.pixels[0]);
    }

    #[test]
    fn toggling_proof_colors_changes_the_view_not_the_document() {
        let mut app = crate::a11y_tests::launch(&[]);
        // The welcome screen has nothing to proof.
        app.run_menu_action(PROOF_COLORS);
        assert!(!app.proof_colors);
        let mut doc = Document::new(8, 4);
        doc.add_pixel_layer("Photo");
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        let undo = app.editor.can_undo();
        assert!(app.proof_view().is_none());
        app.run_menu_action(PROOF_COLORS);
        assert!(app.proof_colors && app.dirty);
        assert!(app.proof_view().is_some_and(|(_, colors, warn)| colors && !warn));
        app.run_menu_action(GAMUT_WARNING);
        assert!(app.proof_view().is_some_and(|(_, colors, warn)| colors && warn));
        app.run_menu_action(PROOF_COLORS);
        app.run_menu_action(GAMUT_WARNING);
        assert!(app.proof_view().is_none());
        assert_eq!(app.editor.can_undo(), undo, "a view toggle is not an edit");
        assert_eq!(app.editor.doc().layer_count(), 1);
    }
}
