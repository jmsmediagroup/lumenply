//! The composite histogram shown in the Properties dock, and auto
//! contrast, which turns its percentiles into a Levels adjustment layer.

use super::*;

pub(crate) const BINS: usize = 64;

/// Luminance histogram of the composite, binned in the gamma (sRGB)
/// domain — the same scale the Levels endpoints and every familiar
/// histogram use.
pub(crate) fn luminance_histogram(flat: &Raster) -> [u32; BINS] {
    let mut hist = [0u32; BINS];
    for p in &flat.pixels {
        if p.a <= 0.0 {
            continue;
        }
        let [r, g, b, _] = p.to_straight();
        let y = lumenply_doc::adjust::srgb_encode(0.2126 * r + 0.7152 * g + 0.0722 * b);
        let bin = ((y * (BINS - 1) as f32).round() as usize).min(BINS - 1);
        hist[bin] += 1;
    }
    hist
}

/// Per-channel histograms of the composite, binned like the luminance one.
pub(crate) fn channel_histograms(flat: &Raster) -> [[u32; BINS]; 3] {
    let mut hist = [[0u32; BINS]; 3];
    for p in &flat.pixels {
        if p.a <= 0.0 {
            continue;
        }
        let s = p.to_straight();
        for ch in 0..3 {
            let v = lumenply_doc::adjust::srgb_encode(s[ch]);
            let bin = ((v * (BINS - 1) as f32).round() as usize).min(BINS - 1);
            hist[ch][bin] += 1;
        }
    }
    hist
}

/// Black and white points that clip `clip` (a fraction, e.g. 0.001) of the
/// pixels at each end. `None` when the image has no tonal range to stretch.
pub(crate) fn auto_contrast_levels(hist: &[u32; BINS], clip: f32) -> Option<(f32, f32)> {
    let total: u64 = hist.iter().map(|&c| c as u64).sum();
    if total == 0 {
        return None;
    }
    let limit = (total as f64 * clip as f64) as u64;
    let mut acc = 0u64;
    let mut lo = 0usize;
    for (i, &c) in hist.iter().enumerate() {
        acc += c as u64;
        if acc > limit {
            lo = i;
            break;
        }
    }
    let mut acc = 0u64;
    let mut hi = BINS - 1;
    for (i, &c) in hist.iter().enumerate().rev() {
        acc += c as u64;
        if acc > limit {
            hi = i;
            break;
        }
    }
    if hi <= lo + 1 {
        return None;
    }
    Some((lo as f32 / (BINS - 1) as f32, hi as f32 / (BINS - 1) as f32))
}

impl App {
    /// Recompute the histogram from the current composite. Called from
    /// refresh(), so it tracks every visible change.
    pub(crate) fn update_histogram(&mut self) {
        if let Some(flat) = &self.last_flat {
            self.histogram = luminance_histogram(flat);
        }
    }

    pub(crate) fn histogram_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "HISTOGRAM");
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 4.0, GROUND);
        let max = self.histogram.iter().copied().max().unwrap_or(0).max(1) as f32;
        let bw = rect.width() / BINS as f32;
        for (i, &c) in self.histogram.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let h = (c as f32 / max).sqrt() * (rect.height() - 4.0);
            let x = rect.min.x + i as f32 * bw;
            p.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, rect.max.y - 2.0 - h),
                    egui::pos2(x + bw.max(1.0), rect.max.y - 2.0),
                ),
                0.0,
                MUTED,
            );
        }
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, LINE));
    }

    /// Add a Levels adjustment above the active layer that stretches the
    /// composite's tonal range, clipping 0.1% at each end.
    pub(crate) fn auto_contrast(&mut self) {
        match auto_contrast_levels(&self.histogram, 0.001) {
            Some((in_black, in_white)) => {
                self.add_adjustment(Adjustment::Levels {
                    in_black,
                    in_white,
                    gamma: 1.0,
                    out_black: 0.0,
                    out_white: 1.0,
                    channels: Default::default(),
                });
                self.status = format!(
                    "Auto contrast: black {:.2}, white {:.2} (as an adjustment layer)",
                    in_black, in_white
                );
            }
            None => self.status = "Auto contrast: the image has no tonal range to stretch".into(),
        }
    }

    /// Add a Levels adjustment that stretches each channel separately
    /// (Photoshop's "auto color" per-channel contrast), neutralising casts.
    pub(crate) fn auto_color(&mut self) {
        let Some(flat) = &self.last_flat else {
            self.status = "Auto color: nothing composited yet".into();
            return;
        };
        let hists = channel_histograms(flat);
        let mut channels = [lumenply_doc::LevelsChannel::default(); 3];
        let mut stretched = false;
        for (ch, hist) in channels.iter_mut().zip(&hists) {
            if let Some((lo, hi)) = auto_contrast_levels(hist, 0.001) {
                if lo > 0.0 || hi < 1.0 {
                    ch.in_black = lo;
                    ch.in_white = hi;
                    stretched = true;
                }
            }
        }
        if !stretched {
            self.status = "Auto color: the channels already span the full range".into();
            return;
        }
        let mut adj = Adjustment::levels_default();
        if let Adjustment::Levels { channels: c, .. } = &mut adj {
            *c = channels;
        }
        self.add_adjustment(adj);
        self.status = "Auto color: per-channel levels (as an adjustment layer)".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    #[test]
    fn auto_contrast_finds_percentile_endpoints() {
        // A low-contrast image: everything between bins 16 and 47.
        let mut hist = [0u32; BINS];
        hist[16..48].fill(100);
        // A couple of outliers that the clip must ignore.
        hist[0] = 1;
        hist[63] = 1;
        let (lo, hi) = auto_contrast_levels(&hist, 0.001).unwrap();
        assert!((lo - 16.0 / 63.0).abs() < 0.02, "lo {lo}");
        assert!((hi - 47.0 / 63.0).abs() < 0.02, "hi {hi}");

        // Flat or empty input has nothing to stretch.
        let mut flat = [0u32; BINS];
        flat[30] = 1000;
        assert!(auto_contrast_levels(&flat, 0.001).is_none());
        assert!(auto_contrast_levels(&[0u32; BINS], 0.001).is_none());
    }

    #[test]
    fn histogram_counts_opaque_pixels_by_luminance() {
        let mut r = Raster::new(4, 1);
        r.set(0, 0, Rgba::from_straight(0.0, 0.0, 0.0, 1.0));
        r.set(1, 0, Rgba::from_straight(1.0, 1.0, 1.0, 1.0));
        r.set(2, 0, Rgba::from_straight(0.5, 0.5, 0.5, 1.0));
        // Transparent pixels are not counted.
        let h = luminance_histogram(&r);
        assert_eq!(h.iter().sum::<u32>(), 3);
        assert_eq!(h[0], 1);
        assert_eq!(h[BINS - 1], 1);
        // Bins are gamma-domain: linear 0.5 sits at sRGB ~0.735.
        let expect = (lumenply_doc::adjust::srgb_encode(0.5) * (BINS - 1) as f32).round() as usize;
        assert_eq!(h[expect], 1);
    }

    #[test]
    fn channel_histograms_find_a_colour_cast() {
        // Green and blue span the full range; red is compressed into the
        // upper half (a warm cast). Auto colour should stretch only red.
        let dec = lumenply_doc::adjust::srgb_decode;
        let mut r = Raster::new(64, 1);
        for i in 0..64 {
            let t = i as f32 / 63.0;
            r.set(i, 0, Rgba::from_straight(dec(0.5 + t * 0.5), dec(t), dec(t), 1.0));
        }
        let h = channel_histograms(&r);
        let red = auto_contrast_levels(&h[0], 0.001).unwrap();
        let green = auto_contrast_levels(&h[1], 0.001).unwrap();
        assert!((red.0 - 0.5).abs() < 0.03, "red floor ~0.5: {red:?}");
        assert!(red.1 > 0.97, "red ceiling ~1: {red:?}");
        assert!(green.0 < 0.03 && green.1 > 0.97, "green full range: {green:?}");
    }
}
