//! The Lumenply brand: the "Lit L" mark drawn vectorially (crisp at any
//! size), the rasterized window icon, the About dialog content and the
//! startup splash. Geometry mirrors the design reference in
//! `img/logo/LitL.dc.html` (a 64-unit viewBox: ink bar 10,8 15×48 r5;
//! accent crossbar 10,41 42×15 r5 with a 2.5-unit background ring;
//! accent dot at 45,20 r8).

use super::*;

/// Paint the mark into `rect` (any size; the 64-unit grid scales to fit).
pub(crate) fn paint_mark(p: &egui::Painter, rect: egui::Rect, ink: Color32, accent: Color32, bg: Color32) {
    let s = rect.width().min(rect.height()) / 64.0;
    let o = rect.min;
    let r = |x: f32, y: f32, w: f32, h: f32| {
        egui::Rect::from_min_size(o + egui::vec2(x * s, y * s), egui::vec2(w * s, h * s))
    };
    // Vertical bar.
    p.rect_filled(r(10.0, 8.0, 15.0, 48.0), 5.0 * s, ink);
    // Crossbar ring (paint-order: stroke): background colour, 2.5 units out.
    p.rect_filled(r(7.5, 38.5, 47.0, 20.0), 7.5 * s, bg);
    // Crossbar.
    p.rect_filled(r(10.0, 41.0, 42.0, 15.0), 5.0 * s, accent);
    // The lumen.
    p.circle_filled(o + egui::vec2(45.0 * s, 20.0 * s), 8.0 * s, accent);
}

/// Rasterize the icon tile (dark rounded square + mark) as straight RGBA.
/// Pure math — no asset pipeline — with 3×3 supersampling.
pub(crate) fn icon_rgba(size: usize) -> Vec<u8> {
    // Work in the tile's 84-unit space (viewBox -10..74).
    let ground = [0x14u8, 0x16, 0x19];
    let ink = [0xECu8, 0xEE, 0xF1];
    let accent = [0xFFu8, 0xB5, 0x47];

    let rounded = |x: f32, y: f32, cx: f32, cy: f32, w: f32, h: f32, r: f32| -> f32 {
        // Signed distance to a rounded rect centred at (cx, cy).
        let dx = (x - cx).abs() - (w / 2.0 - r);
        let dy = (y - cy).abs() - (h / 2.0 - r);
        let ox = dx.max(0.0);
        let oy = dy.max(0.0);
        (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0) - r
    };
    let circle = |x: f32, y: f32, cx: f32, cy: f32, r: f32| -> f32 {
        ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
    };

    let mut out = vec![0u8; size * size * 4];
    let unit = 84.0 / size as f32;
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..3 {
                for sx in 0..3 {
                    let x = -10.0 + (px as f32 + (sx as f32 + 0.5) / 3.0) * unit;
                    let y = -10.0 + (py as f32 + (sy as f32 + 0.5) / 3.0) * unit;
                    let aa = |d: f32| (0.5 - d / unit).clamp(0.0, 1.0);

                    let tile = aa(rounded(x, y, 32.0, 32.0, 84.0, 84.0, 19.0));
                    let vbar = aa(rounded(x, y, 17.5, 32.0, 15.0, 48.0, 5.0));
                    let ring = aa(rounded(x, y, 31.0, 48.5, 47.0, 20.0, 7.5));
                    let cross = aa(rounded(x, y, 31.0, 48.5, 42.0, 15.0, 5.0));
                    let dot = aa(circle(x, y, 45.0, 20.0, 8.0));

                    // Composite in order: ground, ink bar, ring (ground
                    // colour), crossbar, dot — all clipped by the tile.
                    let mut c = [ground[0] as f32, ground[1] as f32, ground[2] as f32];
                    let mix = |c: &mut [f32; 3], col: [u8; 3], a: f32| {
                        for k in 0..3 {
                            c[k] += (col[k] as f32 - c[k]) * a;
                        }
                    };
                    mix(&mut c, ink, vbar);
                    mix(&mut c, ground, ring);
                    mix(&mut c, accent, cross);
                    mix(&mut c, accent, dot);
                    acc[0] += c[0] * tile;
                    acc[1] += c[1] * tile;
                    acc[2] += c[2] * tile;
                    acc[3] += 255.0 * tile;
                }
            }
            let i = (py * size + px) * 4;
            for k in 0..4 {
                out[i + k] = (acc[k] / 9.0 + 0.5) as u8;
            }
        }
    }
    out
}

pub(crate) const SPLASH_PNG: &[u8] = include_bytes!("../assets/splash.png");

impl App {
    /// The startup splash: the branded card centred on the ground, fading
    /// out after a moment. Skipped entirely for `--screenshot` runs.
    pub(crate) fn splash_ui(&mut self, ctx: &egui::Context) {
        let Some(until) = self.splash_until else { return };
        let now = std::time::Instant::now();
        if now >= until {
            self.splash_until = None;
            return;
        }
        let remaining = (until - now).as_secs_f32();
        let alpha = (remaining / 0.25).clamp(0.0, 1.0); // fade the last 250 ms
        if self.splash_tex.is_none() {
            if let Ok(img) = image::load_from_memory(SPLASH_PNG) {
                let mut rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                // The PDF rasterises with white page corners outside the
                // card's rounded corners; mask them off.
                let radius = w as f32 * 0.026;
                for y in 0..h {
                    for x in 0..w {
                        let dx = (x as f32 - (w as f32 - 1.0) / 2.0).abs() - (w as f32 / 2.0 - radius);
                        let dy = (y as f32 - (h as f32 - 1.0) / 2.0).abs() - (h as f32 / 2.0 - radius);
                        if dx > 0.0 && dy > 0.0 {
                            let d = (dx * dx + dy * dy).sqrt() - radius;
                            if d > 0.0 {
                                rgba.get_pixel_mut(x, y).0[3] = 0;
                            } else if d > -1.5 {
                                let a = (-d / 1.5).clamp(0.0, 1.0);
                                let p = rgba.get_pixel_mut(x, y);
                                p.0[3] = (p.0[3] as f32 * a) as u8;
                            }
                        }
                    }
                }
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
                self.splash_tex = Some(ctx.load_texture("splash", ci, egui::TextureOptions::LINEAR));
            }
        }
        let screen = ctx.screen_rect();
        let p = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("splash"),
        ));
        let tint = Color32::from_white_alpha((alpha * 255.0) as u8);
        p.rect_filled(screen, 0.0, GROUND.gamma_multiply(alpha));
        if let Some(tex) = &self.splash_tex {
            let size = tex.size_vec2();
            let scale = (screen.width() * 0.55 / size.x)
                .min(screen.height() * 0.6 / size.y)
                .min(1.0);
            let rect = egui::Rect::from_center_size(screen.center(), size * scale);
            p.image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                tint,
            );
        }
        ctx.request_repaint();
    }
}
