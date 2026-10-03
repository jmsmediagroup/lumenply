//! The composite histogram shown in the Properties dock, and auto
//! contrast, which turns its percentiles into a Levels adjustment layer.

use super::*;

pub(crate) const BINS: usize = 64;

/// Luminance histogram of the composite, binned in the gamma (sRGB)
/// domain — the same scale the Levels endpoints and every familiar
/// histogram use.
pub(crate) fn luminance_histogram(flat: &Raster) -> [u32; BINS] {
    luminance_counts(&flat.pixels)
}

/// The luminance bin of one pixel (`None` for a transparent one).
fn luminance_bin(p: &lumenply_tiles::Rgba) -> Option<usize> {
    if p.a <= 0.0 {
        return None;
    }
    let [r, g, b, _] = p.to_straight();
    let y = lumenply_doc::adjust::srgb_encode(0.2126 * r + 0.7152 * g + 0.0722 * b);
    Some(((y * (BINS - 1) as f32).round() as usize).min(BINS - 1))
}

/// [`luminance_histogram`] of some pixels, counted in parallel (the counts
/// are integers, so the result doesn't depend on how the work is split).
fn luminance_counts(pixels: &[lumenply_tiles::Rgba]) -> [u32; BINS] {
    use rayon::prelude::*;
    pixels
        .par_chunks(1 << 16)
        .map(|chunk| {
            let mut hist = [0u32; BINS];
            for b in chunk.iter().filter_map(luminance_bin) {
                hist[b] += 1;
            }
            hist
        })
        .reduce(
            || [0u32; BINS],
            |mut a, b| {
                for (x, y) in a.iter_mut().zip(b) {
                    *x += y;
                }
                a
            },
        )
}

/// The luminance histogram of the pixels of `flat` inside `r` (which lies
/// within it).
pub(crate) fn luminance_histogram_in(flat: &Raster, r: Rect) -> [u32; BINS] {
    let mut hist = [0u32; BINS];
    for y in r.y..r.bottom() {
        let row = y as usize * flat.width as usize;
        let row = &flat.pixels[row + r.x as usize..row + r.right() as usize];
        for (x, y) in hist.iter_mut().zip(luminance_counts(row)) {
            *x += y;
        }
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

/// What the Histogram panel reports under its graph, on Photoshop's 0–255
/// scale (each of the 64 bins counts at its centre value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HistStats {
    pub mean: f32,
    pub median: u32,
    pub pixels: u64,
}

impl HistStats {
    /// `None` for an empty (fully transparent) image.
    pub(crate) fn of(hist: &[u32; BINS]) -> Option<HistStats> {
        let pixels: u64 = hist.iter().map(|&c| c as u64).sum();
        if pixels == 0 {
            return None;
        }
        let level = |i: usize| i as f64 * 255.0 / (BINS - 1) as f64;
        let sum: f64 = hist.iter().enumerate().map(|(i, &c)| level(i) * c as f64).sum();
        let mut acc = 0u64;
        let mut median = 0;
        for (i, &c) in hist.iter().enumerate() {
            acc += c as u64;
            if acc * 2 >= pixels {
                median = level(i).round() as u32;
                break;
            }
        }
        Some(HistStats {
            mean: (sum / pixels as f64) as f32,
            median,
            pixels,
        })
    }
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
        self.histogram_graph(ui, 56.0);
    }

    /// Window ▸ Histogram: the composite's luminance histogram in a
    /// floating panel, with its mean, median and pixel count. Returns the
    /// panel's height.
    pub(crate) fn histogram_panel_ui(&mut self, ctx: &egui::Context, anchor: Pos2) -> f32 {
        use crate::navigator::{float_header, NAV_W};
        let mut close = false;
        let stats = HistStats::of(&self.histogram);
        let out = egui::Area::new("histogram-panel".into())
            .order(egui::Order::Middle)
            .sense(BACKDROP_SENSE)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                crate::panels::float_frame().show(ui, |ui| {
                    ui.set_width(NAV_W);
                    close = float_header(ui, "HISTOGRAM", "Close Histogram");
                    self.histogram_graph(ui, 90.0);
                    let key = |s: &str| RichText::new(s).color(MUTED);
                    let val = |s: String| RichText::new(s).monospace().color(TEXT);
                    let dash = || "–".to_string();
                    ui.horizontal(|ui| {
                        ui.label(key("Mean"));
                        ui.label(val(stats.map_or_else(dash, |s| format!("{:.1}", s.mean))));
                        ui.label(key("Median"));
                        ui.label(val(stats.map_or_else(dash, |s| s.median.to_string())));
                    });
                    ui.horizontal(|ui| {
                        ui.label(key("Pixels"));
                        ui.label(val(stats.map_or_else(dash, |s| s.pixels.to_string())));
                    });
                });
            });
        if close {
            self.prefs.panels.histogram = false;
            self.save_panel_prefs();
        }
        out.response.rect.height()
    }

    /// The histogram's bars, `height` points tall across the width.
    fn histogram_graph(&self, ui: &mut egui::Ui, height: f32) {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                true,
                "Histogram of the image's luminance",
            )
        });
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
    fn histogram_panel_stats_on_photoshops_scale() {
        let mut h = [0u32; BINS];
        h[0] = 1;
        h[BINS - 1] = 3;
        let s = HistStats::of(&h).unwrap();
        assert_eq!(
            s,
            HistStats {
                mean: 191.25,
                median: 255,
                pixels: 4
            }
        );
        // Bin 21 of 64 sits at 85 of 255.
        let mut h = [0u32; BINS];
        h[21] = 10;
        h[42] = 10;
        let s = HistStats::of(&h).unwrap();
        assert_eq!((s.mean, s.median, s.pixels), (127.5, 85, 20));
        assert_eq!(HistStats::of(&[0; BINS]), None);
    }

    #[test]
    fn window_histogram_toggles_the_panel_and_remembers_it() {
        let mut app = crate::a11y_tests::launch(&["--demo".to_string()]);
        assert!(!app.prefs.panels.histogram);
        assert_eq!(app.action_block("histogram-panel"), None);
        app.run_menu_action("histogram-panel");
        assert!(app.prefs.panels.histogram);
        app.run_menu_action("histogram-panel");
        assert!(!app.prefs.panels.histogram);
    }

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
