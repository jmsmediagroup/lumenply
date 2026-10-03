//! Turning rendered frames into what a viewer watches: the pointer and a
//! caption strip drawn over each frame (never into the app's own UI), an
//! H.264 video at a constant frame rate, and PNG stills.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use eframe::egui;
use image::{Rgba, RgbaImage};

const FONT: &[u8] = include_bytes!("../../../render/fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../../render/fonts/DejaVuSans-Bold.ttf");

/// Caption strip height in points.
pub(crate) const CAPTION_H: f32 = 34.0;

const STRIP: [u8; 3] = [14, 15, 18];
const ACCENT: [u8; 3] = [240, 162, 60];
const PASS: [u8; 3] = [94, 196, 120];
const FAIL: [u8; 3] = [236, 88, 80];
const INK: [u8; 3] = [236, 236, 240];

/// What to draw over one frame.
pub(crate) struct Overlay<'a> {
    /// The pointer in points (None: not shown).
    pub pointer: Option<egui::Pos2>,
    /// A button held down: a ring around the pointer's tip.
    pub pressed: Option<egui::PointerButton>,
    pub step: usize,
    pub caption: &'a str,
    /// The step's outcome, shown while its last frame is held.
    pub outcome: Option<bool>,
    /// A stand-in for a system file panel: its lines, centred.
    pub dialog: Option<&'a [String]>,
}

pub(crate) struct Painter {
    font: fontdue::Font,
    bold: fontdue::Font,
    glyphs: HashMap<(bool, char, u32), (fontdue::Metrics, Vec<u8>)>,
    ppp: f32,
}

fn blend(img: &mut RgbaImage, x: i32, y: i32, c: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 || a <= 0.0 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let a = a.min(1.0);
    for (dst, src) in p.0.iter_mut().zip(c) {
        *dst = (*dst as f32 * (1.0 - a) + src as f32 * a).round() as u8;
    }
    p.0[3] = 255;
}

fn fill_rect(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3], a: f32) {
    for y in y0.max(0)..y1.min(img.height() as i32) {
        for x in x0.max(0)..x1.min(img.width() as i32) {
            blend(img, x, y, c, a);
        }
    }
}

/// Even-odd point-in-polygon.
fn inside(pts: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut c = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            c = !c;
        }
        j = i;
    }
    c
}

fn seg_dist(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len == 0.0 {
        0.0
    } else {
        (((px - a.0) * dx + (py - a.1) * dy) / len).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

impl Painter {
    pub(crate) fn new(ppp: f32) -> Painter {
        let settings = fontdue::FontSettings::default();
        Painter {
            font: fontdue::Font::from_bytes(FONT, settings).expect("bundled font"),
            bold: fontdue::Font::from_bytes(FONT_BOLD, fontdue::FontSettings::default())
                .expect("bundled font"),
            glyphs: HashMap::new(),
            ppp,
        }
    }

    fn glyph(&mut self, bold: bool, c: char, px: f32) -> &(fontdue::Metrics, Vec<u8>) {
        let key = (bold, c, px.to_bits());
        let font = if bold { &self.bold } else { &self.font };
        self.glyphs.entry(key).or_insert_with(|| font.rasterize(c, px))
    }

    /// Width of `text` at `px` pixels.
    fn measure(&mut self, text: &str, px: f32, bold: bool) -> f32 {
        text.chars()
            .map(|c| self.glyph(bold, c, px).0.advance_width)
            .sum()
    }

    /// Draw `text` with its baseline's start at `(x, y)`; returns the advance.
    fn text(
        &mut self,
        img: &mut RgbaImage,
        (x, y): (f32, f32),
        text: &str,
        px: f32,
        c: [u8; 3],
        bold: bool,
    ) -> f32 {
        let mut pen = x;
        for ch in text.chars() {
            let (m, cov) = self.glyph(bold, ch, px).clone();
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (y - m.ymin as f32 - m.height as f32).round() as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = cov[row * m.width + col] as f32 / 255.0;
                    blend(img, gx + col as i32, gy + row as i32, c, a);
                }
            }
            pen += m.advance_width;
        }
        pen - x
    }

    /// `text` cut to fit `width` pixels, with an ellipsis.
    fn fit(&mut self, text: &str, px: f32, width: f32, bold: bool) -> String {
        if self.measure(text, px, bold) <= width {
            return text.to_string();
        }
        let mut s: Vec<char> = text.chars().collect();
        while !s.is_empty() {
            s.pop();
            let t: String = s.iter().collect::<String>() + "…";
            if self.measure(&t, px, bold) <= width {
                return t;
            }
        }
        String::new()
    }

    /// The frame a viewer sees: the UI with the pointer over it and the
    /// caption strip below it, padded to even dimensions for H.264.
    pub(crate) fn compose(&mut self, ui: &RgbaImage, o: &Overlay) -> RgbaImage {
        let s = self.ppp;
        let cap = (CAPTION_H * s).round() as u32;
        let w = ui.width() + ui.width() % 2;
        let h = ui.height() + cap + (ui.height() + cap) % 2;
        let mut img = RgbaImage::from_pixel(w, h, Rgba([STRIP[0], STRIP[1], STRIP[2], 255]));
        image::imageops::replace(&mut img, ui, 0, 0);

        if let Some(lines) = o.dialog {
            self.dialog_card(&mut img, ui.width(), ui.height(), lines);
        }
        if let Some(p) = o.pointer {
            self.cursor(&mut img, p.x * s, p.y * s, o.pressed, ui.height());
        }

        // Caption strip: an accent rule, the step number, the description,
        // and the outcome once the step is done.
        let top = ui.height() as i32;
        fill_rect(
            &mut img,
            0,
            top,
            w as i32,
            top + (1.5 * s).ceil() as i32,
            ACCENT,
            0.9,
        );
        let px = 15.0 * s;
        let base = top as f32 + cap as f32 / 2.0 + px * 0.36;
        let mut x = 14.0 * s;
        if o.step > 0 {
            let n = format!("{}", o.step);
            let nw = self.measure(&n, 13.0 * s, true);
            let pad = 7.0 * s;
            let (bx0, by0) = (x, top as f32 + cap as f32 / 2.0 - 10.0 * s);
            fill_rect(
                &mut img,
                bx0 as i32,
                by0 as i32,
                (bx0 + nw + 2.0 * pad) as i32,
                (by0 + 20.0 * s) as i32,
                ACCENT,
                1.0,
            );
            self.text(&mut img, (x + pad, base - 0.5 * s), &n, 13.0 * s, STRIP, true);
            x += nw + 2.0 * pad + 10.0 * s;
        }
        let mark_w = 90.0 * s;
        let room = w as f32 - x - mark_w - 14.0 * s;
        let caption = self.fit(o.caption, px, room, false);
        self.text(&mut img, (x, base), &caption, px, INK, false);
        match o.outcome {
            Some(true) => {
                let t = "✓ pass";
                let tw = self.measure(t, px, true);
                self.text(&mut img, (w as f32 - tw - 14.0 * s, base), t, px, PASS, true);
            }
            Some(false) => {
                let t = "✗ fail";
                let tw = self.measure(t, px, true);
                self.text(&mut img, (w as f32 - tw - 14.0 * s, base), t, px, FAIL, true);
            }
            None => {}
        }
        img
    }

    /// An arrow pointer with its tip at (x, y) pixels; a ring while pressed.
    fn cursor(
        &mut self,
        img: &mut RgbaImage,
        x: f32,
        y: f32,
        pressed: Option<egui::PointerButton>,
        ui_h: u32,
    ) {
        let s = self.ppp;
        if let Some(b) = pressed {
            let c = if b == egui::PointerButton::Secondary {
                [120, 170, 255]
            } else {
                ACCENT
            };
            let r = 11.0 * s;
            let t = 2.2 * s;
            for py in (y - r - t) as i32..=(y + r + t) as i32 {
                for px in (x - r - t) as i32..=(x + r + t) as i32 {
                    let d = ((px as f32 + 0.5 - x).powi(2) + (py as f32 + 0.5 - y).powi(2)).sqrt();
                    let a = (t / 2.0 - (d - r).abs() + 0.5).clamp(0.0, 1.0);
                    if py < ui_h as i32 {
                        blend(img, px, py, c, a * 0.85);
                    }
                }
            }
        }
        // The classic arrow, in points from the tip.
        let shape = [
            (0.0, 0.0),
            (0.0, 16.5),
            (4.0, 12.8),
            (6.9, 19.4),
            (9.4, 18.3),
            (6.6, 11.8),
            (11.8, 11.8),
        ];
        let pts: Vec<(f32, f32)> = shape.iter().map(|(px, py)| (x + px * s, y + py * s)).collect();
        let outline = 1.3 * s;
        let x0 = (x - outline - 1.0) as i32;
        let y0 = (y - outline - 1.0) as i32;
        let x1 = (x + 12.0 * s + outline + 2.0) as i32;
        let y1 = (y + 20.0 * s + outline + 2.0) as i32;
        const N: i32 = 4;
        for py in y0..y1 {
            if py >= ui_h as i32 {
                break;
            }
            for px in x0..x1 {
                let (mut fill, mut edge) = (0, 0);
                for sy in 0..N {
                    for sx in 0..N {
                        let fx = px as f32 + (sx as f32 + 0.5) / N as f32;
                        let fy = py as f32 + (sy as f32 + 0.5) / N as f32;
                        let near = (0..pts.len())
                            .map(|i| seg_dist(fx, fy, pts[i], pts[(i + 1) % pts.len()]))
                            .fold(f32::MAX, f32::min);
                        if near <= outline / 2.0 + 0.4 {
                            edge += 1;
                        } else if inside(&pts, fx, fy) {
                            fill += 1;
                        }
                    }
                }
                let n = (N * N) as f32;
                blend(img, px, py, [255, 255, 255], edge as f32 / n);
                blend(img, px, py, [10, 10, 12], fill as f32 / n);
            }
        }
    }

    /// The stand-in for the system's file panel, centred on the UI.
    fn dialog_card(&mut self, img: &mut RgbaImage, ui_w: u32, ui_h: u32, lines: &[String]) {
        let s = self.ppp;
        fill_rect(img, 0, 0, ui_w as i32, ui_h as i32, [0, 0, 0], 0.35);
        let cw = (560.0 * s).min(ui_w as f32 - 40.0 * s);
        let ch = (54.0 + 24.0 * lines.len() as f32) * s;
        let x0 = (ui_w as f32 - cw) / 2.0;
        let y0 = (ui_h as f32 - ch) / 2.0;
        fill_rect(
            img,
            x0 as i32,
            y0 as i32,
            (x0 + cw) as i32,
            (y0 + ch) as i32,
            [236, 236, 238],
            1.0,
        );
        fill_rect(
            img,
            x0 as i32,
            y0 as i32,
            (x0 + cw) as i32,
            (y0 + 4.0 * s) as i32,
            ACCENT,
            1.0,
        );
        let mut y = y0 + 30.0 * s;
        self.text(
            img,
            (x0 + 18.0 * s, y),
            "System file panel (stand-in)",
            15.0 * s,
            [30, 30, 34],
            true,
        );
        for line in lines {
            y += 24.0 * s;
            let t = self.fit(line, 14.0 * s, cw - 36.0 * s, false);
            self.text(img, (x0 + 18.0 * s, y), &t, 14.0 * s, [50, 50, 56], false);
        }
    }
}

/// An ffmpeg process taking raw RGBA frames on stdin.
pub(crate) struct Video {
    child: Child,
    stdin: Option<ChildStdin>,
    pub(crate) path: PathBuf,
    size: [u32; 2],
    pub(crate) frames: u64,
    last: Vec<u8>,
}

/// The ffmpeg to use: `LUMENPLY_FFMPEG`, Homebrew's, or the one on PATH.
pub(crate) fn ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LUMENPLY_FFMPEG") {
        return Some(PathBuf::from(p));
    }
    for p in [
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/usr/bin/ffmpeg",
    ] {
        if Path::new(p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| PathBuf::from("ffmpeg"))
}

impl Video {
    pub(crate) fn start(path: &Path, size: [u32; 2], fps: u32, scale: f32) -> Result<Video, String> {
        let exe = ffmpeg().ok_or("ffmpeg not found (set LUMENPLY_FFMPEG); no video")?;
        let mut cmd = Command::new(exe);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .args(["-s", &format!("{}x{}", size[0], size[1])])
        .args(["-r", &fps.to_string(), "-i", "-", "-an"]);
        if (scale - 1.0).abs() > 1e-3 {
            cmd.args([
                "-vf",
                &format!("scale=trunc(iw*{scale}/2)*2:trunc(ih*{scale}/2)*2:flags=lanczos"),
            ]);
        }
        cmd.args([
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "20",
            "-tune",
            "animation",
        ])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-r",
            &fps.to_string(),
            "-movflags",
            "+faststart",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| format!("could not start ffmpeg: {e}"))?;
        let stdin = child.stdin.take();
        Ok(Video {
            child,
            stdin,
            path: path.to_path_buf(),
            size,
            frames: 0,
            last: Vec::new(),
        })
    }

    /// Append a frame; `dedupe` skips it when nothing changed since the
    /// last one (a still screen while waiting reads as a cut, not a stall).
    pub(crate) fn push(&mut self, frame: &RgbaImage, dedupe: bool) {
        if [frame.width(), frame.height()] != self.size {
            return;
        }
        if dedupe && self.last.as_slice() == frame.as_raw().as_slice() {
            return;
        }
        if let Some(stdin) = self.stdin.as_mut() {
            if stdin.write_all(frame.as_raw()).is_err() {
                self.stdin = None;
                return;
            }
            self.frames += 1;
        }
        self.last.clear();
        self.last.extend_from_slice(frame.as_raw());
    }

    /// Show `frame` for `n` frames.
    pub(crate) fn hold(&mut self, frame: &RgbaImage, n: u32) {
        for _ in 0..n {
            self.push(frame, false);
        }
    }

    pub(crate) fn finish(mut self) -> Result<PathBuf, String> {
        drop(self.stdin.take());
        let status = self.child.wait().map_err(|e| e.to_string())?;
        if status.success() && self.frames > 0 {
            Ok(self.path)
        } else {
            Err(format!("ffmpeg failed ({status}) after {} frames", self.frames))
        }
    }
}

pub(crate) fn save_png(img: &RgbaImage, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    img.save_with_format(path, image::ImageFormat::Png)
        .map_err(|e| e.to_string())
}

/// A file-name-safe short form of a step description.
pub(crate) fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_and_pointer_land_outside_and_over_the_ui() {
        let ui = RgbaImage::from_pixel(201, 120, Rgba([100, 100, 100, 255]));
        let mut p = Painter::new(1.0);
        let frame = p.compose(
            &ui,
            &Overlay {
                pointer: Some(egui::pos2(50.0, 40.0)),
                pressed: Some(egui::PointerButton::Primary),
                step: 3,
                caption: "Click File ▸ New...",
                outcome: Some(true),
                dialog: None,
            },
        );
        // Even dimensions, the strip below the UI.
        assert_eq!((frame.width(), frame.height()), (202, 154));
        // The arrow's body is dark just inside the tip, white at its edge.
        assert!(frame.get_pixel(52, 48).0[0] < 40, "{:?}", frame.get_pixel(52, 48));
        // Far from the pointer the UI is untouched.
        assert_eq!(frame.get_pixel(150, 100).0, [100, 100, 100, 255]);
        assert_eq!(slug("Click “File” ▸ New..."), "click-file-new");
    }
}
