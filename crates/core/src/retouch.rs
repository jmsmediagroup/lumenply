//! Retouching commands for the Heal tool's Patch and Red Eye modes.
//! Re-exported from [`crate::commands`].
//!
//! Colour maths runs on straight, gamma-encoded RGB (ADR 0005): a patch's
//! lighting correction and a pupil's redness follow what the eye sees, and
//! a correction never pushes shadows below black the way a linear-light
//! offset could.

use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::{Document, LayerId};
use lumenply_tiles::{Raster, Rect, Rgba};

use crate::commands::{dab_coverage, interpolate_dabs, stroke_bounds, Brush, StrokePoint};
use crate::{Command, EditError, EditResult};
use lumenply_render::membrane::membrane;

/// Straight, gamma-encoded RGB and alpha of a premultiplied linear pixel.
fn encoded(p: Rgba) -> ([f32; 3], f32) {
    let [r, g, b, a] = p.to_straight();
    ([srgb_encode(r), srgb_encode(g), srgb_encode(b)], a)
}

fn decoded(c: [f32; 3], a: f32) -> Rgba {
    Rgba::from_straight(
        srgb_decode(c[0].clamp(0.0, 1.0)),
        srgb_decode(c[1].clamp(0.0, 1.0)),
        srgb_decode(c[2].clamp(0.0, 1.0)),
        a,
    )
}

/// Heal ▸ Patch: replace the selected area of a pixel layer with the
/// texture found `offset` pixels away (where the user dragged the patch),
/// healed into place: the colour and lighting difference measured along
/// the patch's rim is spread smoothly across it (a membrane, as in seamless
/// cloning), so the copied detail takes on the destination's tone. A soft
/// selection blends the result in by its coverage. With `content_aware`
/// the area is synthesised by PatchMatch from the dragged-to area and the
/// patch's own surroundings instead.
///
/// With `destination` the roles swap (Photoshop's Destination mode): the
/// selection is the clean texture, it is patched into the area `offset`
/// away, and the selection moves there. Otherwise the selection stays.
///
/// Sampling a composite (`RetouchSample::CurrentAndBelow` or `All`) reads
/// colours from it and lays the healed result onto the layer by coverage,
/// so a patch works on an empty layer above the photo (non-destructive
/// retouching). One undo step.
pub struct PatchHeal {
    pub layer: LayerId,
    /// Source position = destination + offset (canvas pixels); with
    /// `destination`, where the selection is dragged to.
    pub offset: (i32, i32),
    pub sample: RetouchSample,
    pub content_aware: bool,
    pub destination: bool,
}

impl PatchHeal {
    /// The pixels the patch changes, for previews.
    pub fn area(&self, doc: &Document) -> Option<Rect> {
        self.hole(doc).ok()
    }

    /// The patched area's bounds, before the patch is applied.
    fn hole(&self, doc: &Document) -> EditResult<Rect> {
        let none = || EditError::Invalid("draw around the area to patch first".into());
        let sel = doc.selection.as_ref().ok_or_else(none)?;
        let canvas = doc.canvas();
        let b = sel.tight_bounds(canvas);
        let hole = if self.destination {
            Rect::new(b.x + self.offset.0, b.y + self.offset.1, b.w, b.h).intersect(&canvas)
        } else {
            b
        };
        if hole.is_empty() {
            return Err(none());
        }
        Ok(hole)
    }
}

/// What a retouching command reads, as in Photoshop's Sample menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RetouchSample {
    /// The layer being retouched.
    #[default]
    Current,
    /// The composite of that layer and everything under it: retouch on an
    /// empty layer beneath adjustment layers without baking them in. (In a
    /// group, the whole top-level group holding the layer counts.)
    CurrentAndBelow,
    /// The whole composite.
    All,
}

/// `rect` of what a retouching command on `target` reads.
pub(crate) fn read(doc: &Document, target: LayerId, sample: RetouchSample, rect: Rect) -> EditResult<Raster> {
    Ok(match sample {
        RetouchSample::Current => doc
            .layer(target)
            .ok_or(EditError::NoLayer(target))?
            .pixels()
            .ok_or(EditError::NotPixel(target))?
            .to_raster(rect),
        RetouchSample::All => lumenply_render::composite_rect(doc, rect).to_raster(rect),
        RetouchSample::CurrentAndBelow => {
            let mut top = target;
            while let Some(p) = doc.parent_of(top) {
                top = p;
            }
            let i = doc
                .layers()
                .iter()
                .position(|l| l.id == top)
                .ok_or(EditError::NoLayer(target))?;
            lumenply_render::composite_layers(&doc.layers()[..=i], rect, doc.canvas()).to_raster(rect)
        }
    })
}

/// Lay `healed` (covering `area`) onto `store` by `cov`, premultiplied.
fn lay_down(store: &mut lumenply_tiles::TileStore, area: Rect, healed: &Raster, cov: &[f32]) {
    let w = area.w as usize;
    for (i, &k) in cov.iter().enumerate() {
        let k = k.min(1.0);
        if k <= 0.0 {
            continue;
        }
        let (x, y) = (area.x + (i % w) as i32, area.y + (i / w) as i32);
        let (o, f) = (store.get_pixel(x, y), healed.pixels[i]);
        store.set_pixel(
            x,
            y,
            Rgba::new(
                o.r + (f.r - o.r) * k,
                o.g + (f.g - o.g) * k,
                o.b + (f.b - o.b) * k,
                o.a + (f.a - o.a) * k,
            ),
        );
    }
    store.prune_blank();
}

impl Command for PatchHeal {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.content_aware {
            "Patch (content-aware)".into()
        } else {
            "Patch".into()
        }
    }

    /// A Destination patch moves the selection, whose outline only a full
    /// redraw refreshes, so it reports no bounded area (see
    /// [`PatchHeal::area`] for the pixels it changes).
    fn affected(&self, doc: &Document) -> Option<Rect> {
        if self.destination {
            None
        } else {
            self.area(doc)
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let hole = self.hole(doc)?;
        if self.offset == (0, 0) {
            return Err(EditError::Invalid(
                "drag the patch onto the area to copy from".into(),
            ));
        }
        let canvas = doc.canvas();
        let layer = doc.layer(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        layer.pixels().ok_or(EditError::NotPixel(self.layer))?;
        // Destination mode: patch the dragged-to area from the selection,
        // and carry the selection along.
        let mut sel = doc.selection.clone().expect("checked by hole()");
        let (ox, oy) = if self.destination {
            let (dx, dy) = self.offset;
            sel.coverage.tiles = sel.coverage.tiles.translated(dx, dy);
            doc.selection = Some(sel.clone());
            (-dx, -dy)
        } else {
            self.offset
        };
        let area = if self.content_aware {
            let m = (hole.w.max(hole.h) / 2).clamp(16, 96) as i32;
            Rect::new(
                hole.x - m,
                hole.y - m,
                hole.w + 2 * m as u32,
                hole.h + 2 * m as u32,
            )
            .intersect(&canvas)
        } else {
            Rect::new(hole.x - 1, hole.y - 1, hole.w + 2, hole.h + 2).intersect(&canvas)
        };
        // Both are read before anything is written, so overlapping source
        // and destination never feed on the patch itself. The source
        // repeats the canvas edge where it runs off it.
        let dst = read(doc, self.layer, self.sample, area)?;
        let src = {
            let shifted = Rect::new(area.x + ox, area.y + oy, area.w, area.h);
            let inside = shifted.intersect(&canvas);
            if inside.is_empty() {
                return Err(EditError::Invalid("the patch was dragged off the canvas".into()));
            }
            let part = read(doc, self.layer, self.sample, inside)?;
            let mut r = Raster::new(area.w, area.h);
            for y in 0..area.h as i32 {
                for x in 0..area.w as i32 {
                    let sx = (shifted.x + x).clamp(inside.x, inside.right() - 1) - inside.x;
                    let sy = (shifted.y + y).clamp(inside.y, inside.bottom() - 1) - inside.y;
                    r.set(x as u32, y as u32, part.get(sx as u32, sy as u32));
                }
            }
            r
        };
        let cov = sel.coverage.to_dense(area);
        let healed = if self.content_aware {
            content_aware_patch(&dst, &src, &cov, area, |x, y| {
                !canvas.contains(x + ox, y + oy) || sel.value(x + ox, y + oy) > 0.0
            })?
        } else {
            heal_patch(&dst, &src, &cov)?
        };
        let store = doc
            .layer_mut(self.layer)
            .and_then(|l| l.pixels_mut())
            .expect("checked above");
        lay_down(store, area, &healed, &cov);
        Ok(())
    }
}

/// The healed patch over `dst`'s area: `src` (the dragged-to texture,
/// aligned with `dst`) plus the membrane of `dst − src` along the rim,
/// with `dst`'s alpha. Pixels outside the patch come back as they were.
fn heal_patch(dst: &Raster, src: &Raster, cov: &[f32]) -> EditResult<Raster> {
    let (w, h) = (dst.width as usize, dst.height as usize);
    let n = w * h;
    let mut d = Vec::with_capacity(n);
    let mut s = Vec::with_capacity(n);
    let mut alpha = Vec::with_capacity(n);
    for i in 0..n {
        let (dc, da) = encoded(dst.pixels[i]);
        let (sc, _) = encoded(src.pixels[i]);
        d.push(dc);
        s.push(sc);
        alpha.push(da);
    }
    // The rim: unpatched pixels with colour. Transparent ones carry none,
    // so they are interpolated over rather than matched.
    let known: Vec<bool> = (0..n).map(|i| cov[i] <= 0.0 && alpha[i] > 0.0).collect();
    if !known.iter().any(|&k| k) {
        return Err(EditError::Invalid(
            "nothing around the patch to blend it into".into(),
        ));
    }
    let mut diff: Vec<[f32; 3]> = (0..n)
        .map(|i| {
            if known[i] {
                [d[i][0] - s[i][0], d[i][1] - s[i][1], d[i][2] - s[i][2]]
            } else {
                [0.0; 3]
            }
        })
        .collect();
    membrane(&mut diff, &known, w, h);
    let mut out = dst.clone();
    for i in 0..n {
        if cov[i] <= 0.0 || alpha[i] <= 0.0 {
            continue;
        }
        let healed = [s[i][0] + diff[i][0], s[i][1] + diff[i][1], s[i][2] + diff[i][2]];
        out.pixels[i] = decoded(healed, alpha[i]);
    }
    Ok(out)
}

/// Content-aware patch: PatchMatch fills the patch from a canvas made of
/// two blocks side by side — the patch with its surroundings, and the same
/// area at the dragged-to place — so the synthesis prefers texture from
/// where the user pointed. `excluded(x, y)` marks dragged-to pixels that
/// must not be copied (off the canvas, or inside the patch itself).
fn content_aware_patch(
    dst: &Raster,
    src: &Raster,
    cov: &[f32],
    area: Rect,
    excluded: impl Fn(i32, i32) -> bool,
) -> EditResult<Raster> {
    let (w, h) = (dst.width as usize, dst.height as usize);
    let mut synth = Raster::new(2 * w as u32, h as u32);
    let mut hole = vec![false; 2 * w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            synth.set(x as u32, y as u32, dst.pixels[i]);
            synth.set((w + x) as u32, y as u32, src.pixels[i]);
            hole[y * 2 * w + x] = cov[i] > 0.0;
            hole[y * 2 * w + w + x] = excluded(area.x + x as i32, area.y + y as i32);
        }
    }
    let filled = lumenply_render::inpaint::inpaint(&synth, &hole, 0x5EED_0002)
        .map_err(|e| EditError::Invalid(e.to_string()))?;
    let mut out = dst.clone();
    for y in 0..h {
        for x in 0..w {
            out.pixels[y * w + x] = filled.pixels[y * 2 * w + x];
        }
    }
    Ok(out)
}

/// Heal ▸ Spot, Content-Aware: the area a stroke covers is synthesised by
/// PatchMatch from what surrounds it (as Content-Aware Fill does for a
/// selection), then blended in by the stroke's coverage — each pixel's
/// strongest dab coverage times the strength (`brush.color[3]`).
/// Sampling is limited to the stroke's bounds grown by their larger side,
/// 32 to 128 px. Sampling a composite (see [`RetouchSample`]) lays the
/// result onto the layer, so it works on an empty layer above the photo.
/// One undo step.
pub struct SpotHealAware {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
    pub sample: RetouchSample,
}

impl Command for SpotHealAware {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Spot heal (content-aware)".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let bounds = stroke_bounds(&self.brush, &self.points, canvas);
        if bounds.is_empty() {
            return Err(EditError::Invalid("the stroke is off the canvas".into()));
        }
        doc.layer(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?
            .pixels()
            .ok_or(EditError::NotPixel(self.layer))?;
        let m = bounds.w.max(bounds.h).clamp(32, 128) as i32;
        let area = Rect::new(
            bounds.x - m,
            bounds.y - m,
            bounds.w + 2 * m as u32,
            bounds.h + 2 * m as u32,
        )
        .intersect(&canvas);
        let w = area.w as usize;
        let strength = self.brush.color[3].clamp(0.0, 1.0);
        let mut cov = vec![0f32; w * area.h as usize];
        let sel = doc.selection.clone();
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, sel.as_ref(), |x, y, c| {
                if area.contains(x, y) {
                    let i = (y - area.y) as usize * w + (x - area.x) as usize;
                    cov[i] = cov[i].max(c * strength);
                }
            });
        }
        let hole: Vec<bool> = cov.iter().map(|&c| c > 0.0).collect();
        if !hole.iter().any(|&b| b) {
            return Ok(());
        }
        let src = read(doc, self.layer, self.sample, area)?;
        let filled = lumenply_render::inpaint::inpaint(&src, &hole, 0x5EED_0003)
            .map_err(|e| EditError::Invalid(e.to_string()))?;
        let store = doc
            .layer_mut(self.layer)
            .and_then(|l| l.pixels_mut())
            .expect("checked above");
        lay_down(store, area, &filled, &cov);
        Ok(())
    }
}

/// Heal ▸ Spot and Heal ▸ Healing, applied to a whole stroke at once (on
/// release; the per-dab `HealStroke` previews it while dragging). The
/// stroke's coverage — each pixel's strongest dab coverage times the
/// strength — is one region, healed the way Patch heals a selection: with
/// `texture`, the detail found `offset` away plus the membrane of the
/// colour difference along the region's rim (the healing brush); without,
/// the membrane of the rim itself (spot healing by diffusion). One solve
/// over the whole region converges where overlapping per-dab solves
/// smear. One undo step.
pub struct HealRegion {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
    /// Source position = destination + offset (texture only).
    pub offset: (i32, i32),
    pub texture: bool,
    pub sample: RetouchSample,
}

impl Command for HealRegion {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.texture {
            "Healing brush".into()
        } else {
            "Spot heal".into()
        }
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let bounds = stroke_bounds(&self.brush, &self.points, canvas);
        if bounds.is_empty() {
            return Err(EditError::Invalid("the stroke is off the canvas".into()));
        }
        doc.layer(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?
            .pixels()
            .ok_or(EditError::NotPixel(self.layer))?;
        let area = Rect::new(bounds.x - 1, bounds.y - 1, bounds.w + 2, bounds.h + 2).intersect(&canvas);
        let w = area.w as usize;
        let strength = self.brush.color[3].clamp(0.0, 1.0);
        let mut cov = vec![0f32; w * area.h as usize];
        let sel = doc.selection.clone();
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, sel.as_ref(), |x, y, c| {
                if area.contains(x, y) {
                    let i = (y - area.y) as usize * w + (x - area.x) as usize;
                    cov[i] = cov[i].max(c * strength);
                }
            });
        }
        if !cov.iter().any(|&c| c > 0.0) {
            return Ok(());
        }
        let dst = read(doc, self.layer, self.sample, area)?;
        let src = if self.texture {
            let (ox, oy) = self.offset;
            let shifted = Rect::new(area.x + ox, area.y + oy, area.w, area.h);
            let inside = shifted.intersect(&canvas);
            if inside.is_empty() {
                return Err(EditError::Invalid("the healing source is off the canvas".into()));
            }
            let part = read(doc, self.layer, self.sample, inside)?;
            let mut r = Raster::new(area.w, area.h);
            for y in 0..area.h as i32 {
                for x in 0..area.w as i32 {
                    let sx = (shifted.x + x).clamp(inside.x, inside.right() - 1) - inside.x;
                    let sy = (shifted.y + y).clamp(inside.y, inside.bottom() - 1) - inside.y;
                    r.set(x as u32, y as u32, part.get(sx as u32, sy as u32));
                }
            }
            r
        } else {
            // No texture: healing a blank source leaves the membrane of
            // the rim alone.
            Raster::new(area.w, area.h)
        };
        let healed = heal_patch(&dst, &src, &cov)?;
        let store = doc
            .layer_mut(self.layer)
            .and_then(|l| l.pixels_mut())
            .expect("checked above");
        lay_down(store, area, &healed, &cov);
        Ok(())
    }
}

/// Heal ▸ Move (Photoshop's Content-Aware Move): carry the selected pixels
/// of a layer `offset` away. With `fill`, the area they leave is rebuilt
/// by PatchMatch from its surroundings (as Content-Aware Fill does); with
/// `adapt`, the moved copy is healed into its new place (its tone pulled to
/// the new rim, as Patch does), otherwise it lands as it was. A soft
/// selection blends both by its coverage. The selection moves with the
/// pixels. One undo step.
pub struct ContentAwareMove {
    pub layer: LayerId,
    pub offset: (i32, i32),
    pub fill: bool,
    pub adapt: bool,
}

impl ContentAwareMove {
    /// The pixels the move changes (where they leave and where they land),
    /// for previews.
    pub fn area(&self, doc: &Document) -> Option<Rect> {
        let hole = ContentAwareMove::hole(doc).ok()?;
        let (dx, dy) = self.offset;
        Some(
            hole.union(&Rect::new(hole.x + dx, hole.y + dy, hole.w, hole.h))
                .intersect(&doc.canvas()),
        )
    }

    fn hole(doc: &Document) -> EditResult<Rect> {
        let none = || EditError::Invalid("select what to move first".into());
        let hole = doc
            .selection
            .as_ref()
            .ok_or_else(none)?
            .tight_bounds(doc.canvas());
        if hole.is_empty() {
            return Err(none());
        }
        Ok(hole)
    }
}

impl Command for ContentAwareMove {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Content-aware move".into()
    }

    /// The selection moves too, and only a full redraw refreshes its
    /// outline: no bounded area (see [`ContentAwareMove::area`]).
    fn affected(&self, _doc: &Document) -> Option<Rect> {
        None
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let hole = ContentAwareMove::hole(doc)?;
        if self.offset == (0, 0) {
            return Err(EditError::Invalid("drag the selection somewhere else".into()));
        }
        let canvas = doc.canvas();
        let (dx, dy) = self.offset;
        let sel = doc.selection.clone().expect("checked by hole()");
        let store = doc
            .layer_mut(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?
            .pixels_mut()
            .ok_or(EditError::NotPixel(self.layer))?;
        // The layer as it was: the moved pixels come from here.
        let before = store.clone();
        if self.fill {
            let m = hole.w.max(hole.h).max(64) as i32;
            let area = Rect::new(
                hole.x - m,
                hole.y - m,
                hole.w + 2 * m as u32,
                hole.h + 2 * m as u32,
            )
            .intersect(&canvas);
            let src = before.to_raster(area);
            let cov = sel.coverage.to_dense(area);
            let holes: Vec<bool> = cov.iter().map(|&c| c > 0.0).collect();
            let filled = lumenply_render::inpaint::inpaint(&src, &holes, 0x5EED_0004)
                .map_err(|e| EditError::Invalid(e.to_string()))?;
            lay_down(store, area, &filled, &cov);
        }
        // The landing area, one pixel wider so Adapt has a rim to read.
        let dest = Rect::new(hole.x + dx - 1, hole.y + dy - 1, hole.w + 2, hole.h + 2).intersect(&canvas);
        if dest.is_empty() {
            return Err(EditError::Invalid(
                "the selection was dragged off the canvas".into(),
            ));
        }
        let (w, h) = (dest.w as usize, dest.h as usize);
        let mut cov = vec![0f32; w * h];
        let mut moved = Raster::new(dest.w, dest.h);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (dest.x + x as i32, dest.y + y as i32);
                cov[y * w + x] = sel.value(px - dx, py - dy);
                moved.set(x as u32, y as u32, before.get_pixel(px - dx, py - dy));
            }
        }
        let landed = if self.adapt {
            heal_patch(&store.to_raster(dest), &moved, &cov)?
        } else {
            moved
        };
        lay_down(store, dest, &landed, &cov);
        let mut sel = sel;
        sel.coverage.tiles = sel.coverage.tiles.translated(dx, dy);
        doc.selection = Some(sel);
        Ok(())
    }
}

/// Heal ▸ Red Eye: find the red pupil in `area` (the reddest connected
/// patch nearest the box centre) and turn it a dark neutral.
///
/// Redness is `(r − max(g, b)) / r` on gamma-encoded values, so a red
/// pupil scores near 1 and skin about 0.25. `pupil_size` (0–1) lowers the
/// threshold from 0.75 to 0.25, taking in less saturated edge pixels. The
/// pupil becomes `(g + b) / 2 × (1 − darken)` in every channel; edge pixels
/// just below the threshold blend partly, by how red they are.
pub struct RedEye {
    pub layer: LayerId,
    pub area: Rect,
    pub pupil_size: f32,
    pub darken: f32,
}

impl RedEye {
    /// Redness threshold for a pupil size.
    pub fn threshold(pupil_size: f32) -> f32 {
        0.75 - 0.5 * pupil_size.clamp(0.0, 1.0)
    }
}

fn redness(c: [f32; 3], a: f32) -> f32 {
    if a <= 0.0 || c[0] < 0.15 {
        return 0.0;
    }
    ((c[0] - c[1].max(c[2])) / c[0]).max(0.0)
}

impl Command for RedEye {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Red eye".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(self.area.intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let area = self.area.intersect(&doc.canvas());
        if area.is_empty() {
            return Err(EditError::Invalid("the red-eye box is off the canvas".into()));
        }
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let img = store.to_raster(area);
        let (w, h) = (area.w as usize, area.h as usize);
        let px: Vec<([f32; 3], f32)> = img.pixels.iter().map(|&p| encoded(p)).collect();
        let red: Vec<f32> = px.iter().map(|&(c, a)| redness(c, a)).collect();
        let thr = RedEye::threshold(self.pupil_size);
        // Seed: the qualifying pixel nearest the box centre.
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let seed = (0..w * h)
            .filter(|&i| red[i] >= thr)
            .min_by(|&a, &b| {
                let d = |i: usize| {
                    let (x, y) = ((i % w) as f32 + 0.5 - cx, (i / w) as f32 + 0.5 - cy);
                    x * x + y * y
                };
                d(a).total_cmp(&d(b)).then(a.cmp(&b))
            })
            .ok_or_else(|| EditError::Invalid("no red pupil found here".into()))?;
        // The pupil: pixels at or above the threshold, 8-connected to it.
        let mut inside = vec![false; w * h];
        let mut stack = vec![seed];
        inside[seed] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if !inside[j] && red[j] >= thr {
                        inside[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        // Its one-pixel rim blends partly, by how red each pixel is.
        let lower = (thr - 0.2).max(0.0);
        let strength = |i: usize| -> f32 {
            if inside[i] {
                return 1.0;
            }
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let touches = (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx >= 0
                        && ny >= 0
                        && nx < w as i32
                        && ny < h as i32
                        && inside[ny as usize * w + nx as usize]
                })
            });
            if touches {
                ((red[i] - lower) / (thr - lower).max(1e-3)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let dark = 1.0 - self.darken.clamp(0.0, 1.0);
        for (i, &(c, a)) in px.iter().enumerate() {
            let (x, y) = (area.x + (i % w) as i32, area.y + (i / w) as i32);
            let k = strength(i) * sel.as_ref().map_or(1.0, |s| s.value(x, y));
            if k <= 0.0 {
                continue;
            }
            let v = 0.5 * (c[1] + c[2]) * dark;
            let out = [
                c[0] + (v - c[0]) * k,
                c[1] + (v - c[1]) * k,
                c[2] + (v - c[2]) * k,
            ];
            store.set_pixel(x, y, decoded(out, a));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddPixelLayer, SetSelection};
    use crate::Editor;
    use lumenply_doc::Selection;

    /// A pixel from gamma-encoded straight RGB, fully opaque.
    fn g(r: f32, gg: f32, b: f32) -> Rgba {
        decoded([r, gg, b], 1.0)
    }

    fn enc(p: Rgba) -> [f32; 3] {
        encoded(p).0
    }

    /// Gamma grey of the test image: vertical stripes of ±0.1 every 3
    /// pixels over 0.4, brighter by 0.25 from row 40 down.
    fn stripes(x: u32, y: u32) -> f32 {
        let stripe = if x % 6 < 3 { 0.1 } else { -0.1 };
        0.4 + stripe + if y >= 40 { 0.25 } else { 0.0 }
    }

    fn patch_doc() -> (Editor, LayerId) {
        let (w, h) = (64u32, 64u32);
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // A white blemish over the stripes.
                let blemish = (20..28).contains(&x) && (14..22).contains(&y);
                let v = if blemish { 1.0 } else { stripes(x, y) };
                r.set(x, y, g(v, v, v));
            }
        }
        let mut ed = Editor::new(Document::new(w, h));
        ed.execute(&AddPixelLayer::from_raster("photo", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        (ed, id)
    }

    #[test]
    fn a_patch_takes_texture_from_the_source_and_tone_from_its_rim() {
        let (mut ed, id) = patch_doc();
        let patch = PatchHeal {
            layer: id,
            offset: (0, 36),
            sample: RetouchSample::Current,
            content_aware: false,
            destination: false,
        };
        assert!(ed.execute(&patch).is_err(), "needs a selection");
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(16, 10, 16, 16))),
        })
        .unwrap();
        assert_eq!(patch.affected(ed.doc()), Some(Rect::new(16, 10, 16, 16)));
        ed.execute(&patch).unwrap();
        assert_eq!(ed.history().last().copied(), Some("Patch"));
        // The source rows 46..62 carry the same stripes 0.25 brighter; the
        // rim says "0.25 darker" all round, so the patch lands exactly on
        // the stripes that belong here: 0.5 on bright columns, 0.3 on dark.
        let px = ed.doc().layer(id).unwrap().pixels().unwrap().clone();
        let mut worst = 0f32;
        for y in 10..26 {
            for x in 16..32 {
                let c = enc(px.get_pixel(x, y));
                let want = stripes(x as u32, y as u32);
                worst = worst.max((c[0] - want).abs()).max((c[2] - want).abs());
            }
        }
        assert!(worst < 2e-3, "worst {worst}");
        let c = enc(px.get_pixel(24, 18));
        // Inside the old blemish: column 24 is bright (24 % 6 < 3), 27 dark.
        assert!((c[1] - 0.5).abs() < 2e-3, "{c:?}");
        let c = enc(px.get_pixel(27, 18));
        assert!((c[1] - 0.3).abs() < 2e-3, "{c:?}");
        // Untouched outside the selection; the selection stays.
        let c = enc(px.get_pixel(40, 18));
        assert!((c[0] - 0.3).abs() < 1e-3, "{c:?}");
        assert!(ed.doc().selection.is_some());
        ed.undo();
        let back = enc(ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(24, 18));
        assert!((back[0] - 1.0).abs() < 1e-3, "undo brings the blemish back");
    }

    #[test]
    fn a_content_aware_patch_fills_from_the_dragged_area() {
        let (mut ed, id) = patch_doc();
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(18, 12, 12, 12))),
        })
        .unwrap();
        ed.execute(&PatchHeal {
            layer: id,
            offset: (0, -12),
            sample: RetouchSample::Current,
            content_aware: true,
            destination: false,
        })
        .unwrap();
        assert_eq!(ed.history().last().copied(), Some("Patch (content-aware)"));
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let mut worst = 0f32;
        for y in 14..22 {
            for x in 20..28 {
                let c = enc(px.get_pixel(x, y));
                worst = worst.max((c[0] - stripes(x as u32, y as u32)).abs());
            }
        }
        assert!(worst < 0.03, "the stripes run through the patch (worst {worst})");
    }

    #[test]
    fn a_patch_without_a_drag_or_on_a_non_pixel_layer_is_refused() {
        let (mut ed, id) = patch_doc();
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(16, 10, 16, 16))),
        })
        .unwrap();
        let steps = ed.history().len();
        for (layer, offset) in [(id, (0, 0)), (99, (0, 30))] {
            let r = ed.execute(&PatchHeal {
                layer,
                offset,
                sample: RetouchSample::All,
                content_aware: false,
                destination: false,
            });
            assert!(r.is_err());
        }
        assert_eq!(ed.history().len(), steps);
    }

    fn eye_doc() -> (Document, LayerId) {
        let (w, h) = (40u32, 40u32);
        let skin = g(0.85, 0.62, 0.52);
        let pupil = g(0.8, 0.1, 0.1);
        let mut r = Raster::filled(w, h, skin);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let d2 = |cx: i32, cy: i32| (x - cx).pow(2) + (y - cy).pow(2);
                if d2(20, 20) <= 36 || d2(35, 5) <= 4 {
                    r.set(x as u32, y as u32, pupil);
                }
            }
        }
        let mut doc = Document::new(w, h);
        let id = doc.add_pixel_layer("face");
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = lumenply_tiles::TileStore::from_raster(&r, 0, 0);
        (doc, id)
    }

    #[test]
    fn red_eye_darkens_the_connected_pupil_only() {
        let (mut doc, id) = eye_doc();
        RedEye {
            layer: id,
            area: Rect::new(0, 0, 40, 40),
            pupil_size: 0.5,
            darken: 0.5,
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        // (g + b) / 2 = 0.1, halved: 0.05 in every channel.
        for (x, y) in [(20, 20), (14, 20), (20, 25)] {
            let c = enc(px.get_pixel(x, y));
            for v in c {
                assert!((v - 0.05).abs() < 1e-4, "({x}, {y}): {c:?}");
            }
        }
        // Skin right next to the pupil is untouched (redness 0.27 is below
        // the rim's lower bound of 0.3).
        for (x, y) in [(27, 20), (20, 13), (12, 12)] {
            let c = enc(px.get_pixel(x, y));
            assert!(
                (c[0] - 0.85).abs() < 1e-5 && (c[1] - 0.62).abs() < 1e-5,
                "({x}, {y}): {c:?}"
            );
        }
        // The second red spot is not connected to the pupil: still red.
        let c = enc(px.get_pixel(35, 5));
        assert!((c[0] - 0.8).abs() < 1e-5 && (c[1] - 0.1).abs() < 1e-5, "{c:?}");
    }

    /// Worst gamma error against the stripes over the blemish's patch.
    fn stripe_error(px: &lumenply_tiles::TileStore) -> f32 {
        let mut worst = 0f32;
        for y in 10..26 {
            for x in 16..32 {
                let c = enc(px.get_pixel(x, y));
                worst = worst.max((c[0] - stripes(x as u32, y as u32)).abs());
            }
        }
        worst
    }

    #[test]
    fn a_destination_patch_carries_the_clean_selection_onto_the_flaw() {
        let (mut ed, id) = patch_doc();
        // Select clean texture below and drag it up onto the blemish.
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(16, 46, 16, 16))),
        })
        .unwrap();
        let patch = PatchHeal {
            layer: id,
            offset: (0, -36),
            sample: RetouchSample::Current,
            content_aware: false,
            destination: true,
        };
        assert_eq!(patch.area(ed.doc()), Some(Rect::new(16, 10, 16, 16)));
        assert_eq!(patch.affected(ed.doc()), None, "the selection outline moves");
        ed.execute(&patch).unwrap();
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let worst = stripe_error(px);
        assert!(worst < 2e-3, "worst {worst}");
        // The clean area is untouched (0.4 + 0.1 + 0.25) and the selection
        // moved onto the patched area.
        assert!((enc(px.get_pixel(24, 50))[0] - 0.75).abs() < 1e-3);
        let doc = ed.doc();
        let moved = doc.selection.as_ref().unwrap().tight_bounds(doc.canvas());
        assert_eq!(moved, Rect::new(16, 10, 16, 16));
        ed.undo();
        let doc = ed.doc();
        let back = doc.selection.as_ref().unwrap().tight_bounds(doc.canvas());
        assert_eq!(back, Rect::new(16, 46, 16, 16), "undo puts the selection back");
    }

    #[test]
    fn merged_retouching_works_on_an_empty_layer_above_the_photo() {
        let (mut ed, photo) = patch_doc();
        ed.execute(&AddPixelLayer::new("retouch")).unwrap();
        let top = ed.doc().layers().last().unwrap().id;
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(16, 10, 16, 16))),
        })
        .unwrap();
        ed.execute(&PatchHeal {
            layer: top,
            offset: (0, 36),
            sample: RetouchSample::All,
            content_aware: false,
            destination: false,
        })
        .unwrap();
        // The photo keeps its blemish; the new layer holds opaque stripes
        // over the patch and nothing elsewhere, so the composite is clean.
        let doc = ed.doc();
        let base = doc.layer(photo).unwrap().pixels().unwrap();
        assert!((enc(base.get_pixel(24, 18))[0] - 1.0).abs() < 1e-3);
        let over = doc.layer(top).unwrap().pixels().unwrap();
        assert!((over.get_pixel(24, 18).a - 1.0).abs() < 1e-4);
        assert!((enc(over.get_pixel(24, 18))[0] - 0.5).abs() < 2e-3);
        assert!((enc(over.get_pixel(27, 18))[0] - 0.3).abs() < 2e-3);
        assert_eq!(over.get_pixel(40, 18).a, 0.0);
        let flat = lumenply_render::composite(doc);
        let worst = stripe_error(&flat);
        assert!(worst < 2e-3, "composite worst {worst}");

        // Content-aware spot healing on the same empty layer.
        ed.undo(); // the patch
        ed.undo(); // the selection
        ed.execute(&SpotHealAware {
            layer: top,
            brush: Brush {
                radius: 7.0,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, 1.0],
                spacing: 0.2,
                jitter: 0.0,
                mode: crate::commands::BrushMode::Paint,
                ..Brush::default()
            },
            points: vec![
                StrokePoint::new(20.0, 18.0, 1.0),
                StrokePoint::new(28.0, 18.0, 1.0),
            ],
            sample: RetouchSample::All,
        })
        .unwrap();
        let doc = ed.doc();
        let base = doc.layer(photo).unwrap().pixels().unwrap();
        assert!(
            (enc(base.get_pixel(24, 18))[0] - 1.0).abs() < 1e-3,
            "the photo is untouched"
        );
        let flat = lumenply_render::composite(doc);
        let mut worst = 0f32;
        for y in 14..22 {
            for x in 20..28 {
                worst = worst.max((enc(flat.get_pixel(x, y))[0] - stripes(x as u32, y as u32)).abs());
            }
        }
        assert!(worst < 0.03, "composite worst {worst}");
    }

    #[test]
    fn current_and_below_ignores_the_layers_above() {
        // photo, an empty retouch layer, then half-opaque black on top.
        let (mut ed, _photo) = patch_doc();
        ed.execute(&AddPixelLayer::new("retouch")).unwrap();
        let retouch = ed.doc().layers().last().unwrap().id;
        let mut veil = AddPixelLayer::from_raster(
            "veil",
            Raster::filled(64, 64, Rgba::new(0.0, 0.0, 0.0, 1.0)),
            0,
            0,
        );
        veil.opacity = 0.5;
        ed.execute(&veil).unwrap();
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(16, 10, 16, 16))),
        })
        .unwrap();
        let patch = |sample| PatchHeal {
            layer: retouch,
            offset: (0, 36),
            sample,
            content_aware: false,
            destination: false,
        };
        ed.execute(&patch(RetouchSample::CurrentAndBelow)).unwrap();
        let px = ed.doc().layer(retouch).unwrap().pixels().unwrap();
        assert!(stripe_error(px) < 2e-3, "the veil above is not baked in");
        ed.undo();
        ed.execute(&patch(RetouchSample::All)).unwrap();
        let px = ed.doc().layer(retouch).unwrap().pixels().unwrap();
        assert!(stripe_error(px) > 0.1, "All layers samples the veil too");
        // Current: the empty layer has nothing to heal from.
        ed.undo();
        assert!(ed.execute(&patch(RetouchSample::Current)).is_err());
    }

    fn heal_brush() -> Brush {
        Brush {
            radius: 7.0,
            hardness: 1.0,
            color: [0.0, 0.0, 0.0, 1.0],
            spacing: 0.2,
            jitter: 0.0,
            mode: crate::commands::BrushMode::Paint,
            ..Brush::default()
        }
    }

    #[test]
    fn a_whole_stroke_heals_texture_and_tone_exactly() {
        let (mut ed, id) = patch_doc();
        let stroke = HealRegion {
            layer: id,
            brush: heal_brush(),
            points: vec![
                StrokePoint::new(20.0, 18.0, 1.0),
                StrokePoint::new(28.0, 18.0, 1.0),
            ],
            offset: (0, 36),
            texture: true,
            sample: RetouchSample::Current,
        };
        ed.execute(&stroke).unwrap();
        assert_eq!(ed.history().last().copied(), Some("Healing brush"));
        // The source is 0.25 brighter; the rim pulls it back onto the
        // stripes, over the whole blemish.
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let mut worst = 0f32;
        for y in 14..22 {
            for x in 20..28 {
                worst = worst.max((enc(px.get_pixel(x, y))[0] - stripes(x as u32, y as u32)).abs());
            }
        }
        assert!(worst < 2e-3, "worst {worst}");
    }

    #[test]
    fn a_whole_stroke_spot_heal_continues_a_smooth_ramp() {
        // A gamma ramp 0.2 + 0.01·x with a white blemish: diffusion alone
        // restores the ramp, since a linear ramp is harmonic.
        let mut r = Raster::new(64, 40);
        for y in 0..40 {
            for x in 0..64 {
                let v = if (24..34).contains(&x) && (15..25).contains(&y) {
                    1.0
                } else {
                    0.2 + 0.01 * x as f32
                };
                r.set(x, y, g(v, v, v));
            }
        }
        let mut ed = Editor::new(Document::new(64, 40));
        ed.execute(&AddPixelLayer::from_raster("ramp", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        let mut brush = heal_brush();
        brush.radius = 9.0;
        ed.execute(&HealRegion {
            layer: id,
            brush,
            points: vec![
                StrokePoint::new(27.0, 20.0, 1.0),
                StrokePoint::new(31.0, 20.0, 1.0),
            ],
            offset: (0, 0),
            texture: false,
            sample: RetouchSample::Current,
        })
        .unwrap();
        assert_eq!(ed.history().last().copied(), Some("Spot heal"));
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let mut worst = 0f32;
        for y in 15..25 {
            for x in 24..34 {
                worst = worst.max((enc(px.get_pixel(x, y))[0] - (0.2 + 0.01 * x as f32)).abs());
            }
        }
        assert!(worst < 2e-3, "worst {worst}");
        // (30, 20): 0.2 + 0.3 = 0.5.
        assert!((enc(px.get_pixel(30, 20))[1] - 0.5).abs() < 2e-3);
    }

    #[test]
    fn content_aware_move_carries_pixels_and_fills_behind() {
        let (mut ed, id) = patch_doc();
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(20, 14, 8, 8))),
        })
        .unwrap();
        let mv = ContentAwareMove {
            layer: id,
            offset: (24, 0),
            fill: true,
            adapt: false,
        };
        assert_eq!(mv.area(ed.doc()), Some(Rect::new(20, 14, 32, 8)));
        assert_eq!(mv.affected(ed.doc()), None, "the selection outline moves");
        ed.execute(&mv).unwrap();
        assert_eq!(ed.history().last().copied(), Some("Content-aware move"));
        let doc = ed.doc();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        // The white square landed 24 px right, exactly as it was...
        for (x, y) in [(44, 14), (51, 21), (48, 18)] {
            assert!((enc(px.get_pixel(x, y))[0] - 1.0).abs() < 1e-3, "({x}, {y})");
        }
        // ...the stripes grew back where it was, and the selection moved.
        let mut worst = 0f32;
        for y in 14..22 {
            for x in 20..28 {
                worst = worst.max((enc(px.get_pixel(x, y))[0] - stripes(x as u32, y as u32)).abs());
            }
        }
        assert!(worst < 0.03, "worst {worst}");
        assert_eq!(
            doc.selection.as_ref().unwrap().tight_bounds(doc.canvas()),
            Rect::new(44, 14, 8, 8)
        );
    }

    #[test]
    fn an_adapted_move_takes_on_the_tone_of_its_new_place() {
        // Grey 0.3 above row 32, 0.6 below; a 0.45 square at the top.
        let mut r = Raster::new(32, 64);
        for y in 0..64 {
            for x in 0..32 {
                let v = if (8..16).contains(&x) && (8..16).contains(&y) {
                    0.45
                } else if y < 32 {
                    0.3
                } else {
                    0.6
                };
                r.set(x, y, g(v, v, v));
            }
        }
        let run = |adapt: bool| {
            let mut ed = Editor::new(Document::new(32, 64));
            ed.execute(&AddPixelLayer::from_raster("p", r.clone(), 0, 0))
                .unwrap();
            let id = ed.doc().layers()[0].id;
            ed.execute(&SetSelection {
                selection: Some(Selection::rect(Rect::new(8, 8, 8, 8))),
            })
            .unwrap();
            ed.execute(&ContentAwareMove {
                layer: id,
                offset: (0, 32),
                fill: true,
                adapt,
            })
            .unwrap();
            let px = ed.doc().layer(id).unwrap().pixels().unwrap().clone();
            (enc(px.get_pixel(12, 44))[0], enc(px.get_pixel(12, 12))[0])
        };
        // The rim says "0.3 brighter here": 0.45 + 0.3 = 0.75.
        let (landed, behind) = run(true);
        assert!((landed - 0.75).abs() < 2e-3, "{landed}");
        assert!((behind - 0.3).abs() < 2e-3, "{behind}");
        let (landed, _) = run(false);
        assert!((landed - 0.45).abs() < 1e-3, "{landed}");
    }

    #[test]
    fn content_aware_spot_healing_rebuilds_the_stripes_under_the_stroke() {
        let (mut ed, id) = patch_doc();
        let stroke = SpotHealAware {
            layer: id,
            brush: Brush {
                radius: 7.0,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, 1.0],
                spacing: 0.2,
                jitter: 0.0,
                mode: crate::commands::BrushMode::Paint,
                ..Brush::default()
            },
            points: vec![
                StrokePoint::new(20.0, 18.0, 1.0),
                StrokePoint::new(28.0, 18.0, 1.0),
            ],
            sample: RetouchSample::Current,
        };
        assert_eq!(stroke.affected(ed.doc()), Some(Rect::new(11, 9, 26, 18)));
        ed.execute(&stroke).unwrap();
        assert_eq!(ed.history().last().copied(), Some("Spot heal (content-aware)"));
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let mut worst = 0f32;
        for y in 14..22 {
            for x in 20..28 {
                let c = enc(px.get_pixel(x, y));
                worst = worst.max((c[0] - stripes(x as u32, y as u32)).abs());
            }
        }
        assert!(
            worst < 0.03,
            "the blemish is gone, stripes rebuilt (worst {worst})"
        );
        // Far from the stroke nothing changed.
        let c = enc(px.get_pixel(50, 50));
        assert!((c[0] - stripes(50, 50)).abs() < 1e-3);
    }

    #[test]
    fn red_eye_honours_darken_and_refuses_a_box_without_red() {
        let (mut doc, id) = eye_doc();
        RedEye {
            layer: id,
            area: Rect::new(14, 14, 13, 13),
            pupil_size: 0.5,
            darken: 0.0,
        }
        .apply(&mut doc)
        .unwrap();
        let c = enc(doc.layer(id).unwrap().pixels().unwrap().get_pixel(20, 20));
        assert!(c.iter().all(|v| (v - 0.1).abs() < 1e-4), "{c:?}");
        assert_eq!(RedEye::threshold(0.5), 0.5);
        assert_eq!(RedEye::threshold(1.0), 0.25);
        let err = RedEye {
            layer: id,
            area: Rect::new(0, 30, 10, 10),
            pupil_size: 0.5,
            darken: 0.5,
        }
        .apply(&mut doc);
        assert!(err.is_err(), "skin alone is not a pupil");
    }
}
