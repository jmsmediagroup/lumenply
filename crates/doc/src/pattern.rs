//! Patterns: small images tiled across the canvas by pattern fill layers,
//! shape fills and the Pattern Overlay effect (ADR 0020).
//!
//! A document carries every pattern it uses in [`crate::Document::patterns`]
//! (saved with projects and in PSD's global `Patt` block), so a file is
//! self-contained. Fills and effects name a pattern by [`PatternRef`] — its
//! id plus a display name, as Photoshop does — and hold a derived pointer to
//! the pixels, resolved by [`crate::Document::resolve_patterns`].

use std::sync::Arc;

use lumenply_tiles::{Raster, Rgba};
use serde::{Deserialize, Serialize};

/// Largest side a pattern may have; bigger sources are cropped.
pub const MAX_PATTERN_SIDE: u32 = 1024;

/// A named, tileable image (premultiplied linear RGBA, like every raster).
#[derive(Clone, Debug)]
pub struct Pattern {
    /// Unique id; Photoshop uses a UUID string and so do we.
    pub id: String,
    pub name: String,
    pub image: Arc<Raster>,
}

impl PartialEq for Pattern {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id
            && self.name == o.name
            && (Arc::ptr_eq(&self.image, &o.image) || self.image == o.image)
    }
}

impl Pattern {
    /// A pattern from `image`, cropped to [`MAX_PATTERN_SIDE`] and never
    /// empty (an empty source becomes one transparent pixel).
    pub fn new(id: impl Into<String>, name: impl Into<String>, image: Raster) -> Self {
        Pattern {
            id: id.into(),
            name: name.into(),
            image: Arc::new(clamp_size(image)),
        }
    }

    /// The reference fills and effects store.
    pub fn reference(&self) -> PatternRef {
        PatternRef {
            id: self.id.clone(),
            name: self.name.clone(),
            image: Some(self.image.clone()),
        }
    }

    pub fn width(&self) -> u32 {
        self.image.width
    }

    pub fn height(&self) -> u32 {
        self.image.height
    }
}

fn clamp_size(image: Raster) -> Raster {
    if image.width == 0 || image.height == 0 || image.pixels.len() != (image.width * image.height) as usize {
        return Raster::new(1, 1);
    }
    if image.width <= MAX_PATTERN_SIDE && image.height <= MAX_PATTERN_SIDE {
        return image;
    }
    let (w, h) = (
        image.width.min(MAX_PATTERN_SIDE),
        image.height.min(MAX_PATTERN_SIDE),
    );
    let mut out = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.set(x, y, image.get(x, y));
        }
    }
    out
}

/// A fresh pattern id: a UUID-shaped string (Photoshop's form), made from
/// the clock, a counter and `seed` (e.g. a hash of the pixels).
pub fn new_pattern_id(seed: u64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // splitmix64 over the three inputs.
    let mix = |mut z: u64| {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let a = mix(nanos ^ seed.rotate_left(17));
    let b = mix(a ^ n.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ seed);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0xfff) as u16,
        ((b >> 48) as u16 & 0x3fff) | 0x8000,
        b & 0xffff_ffff_ffff
    )
}

/// A cheap content hash of a raster (8-bit quantised), to recognise the
/// same pixels defined twice.
pub fn raster_hash(r: &Raster) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ ((r.width as u64) << 32 | r.height as u64);
    for p in &r.pixels {
        for v in [p.r, p.g, p.b, p.a] {
            h ^= (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// How a fill or effect names a pattern: the id first, the name as a
/// fallback (and for display). The pixels are derived state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PatternRef {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Derived: the document pattern this reference resolved to. Never
    /// saved; set by [`crate::Document::resolve_patterns`].
    #[serde(skip)]
    pub image: Option<Arc<Raster>>,
}

impl PartialEq for PatternRef {
    /// Compares what is saved; the pixels are derived.
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id && self.name == o.name
    }
}

impl PatternRef {
    /// The display name: Photoshop's preset names read
    /// `$$$/Presets/…/Key=Display`; the part after `=` is shown.
    pub fn display_name(&self) -> &str {
        display_name(&self.name)
    }
}

/// The readable part of a (possibly localisation-keyed) pattern name.
pub fn display_name(name: &str) -> &str {
    let n = name.trim_end_matches('\0');
    if n.starts_with("$$$/") {
        if let Some((_, shown)) = n.split_once('=') {
            return shown;
        }
    }
    n
}

/// Settings of the Pattern Overlay layer effect: the pattern tiled from
/// the canvas origin (plus `offset`) over the layer's coverage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatternOverlayFx {
    pub pattern: PatternRef,
    /// 1.0 = 100%.
    pub scale: f32,
    pub opacity: f32,
    #[serde(default)]
    pub blend: crate::BlendMode,
    /// Phase in canvas pixels (Photoshop's `phase`).
    #[serde(default)]
    pub offset: [f32; 2],
    /// Counter-clockwise rotation in degrees.
    #[serde(default)]
    pub angle: f32,
    /// Photoshop's "Link with Layer"; kept for round trips.
    #[serde(default = "yes")]
    pub link: bool,
}

fn yes() -> bool {
    true
}

impl PatternOverlayFx {
    pub fn new(pattern: PatternRef) -> Self {
        PatternOverlayFx {
            pattern,
            scale: 1.0,
            opacity: 1.0,
            blend: crate::BlendMode::Normal,
            offset: [0.0, 0.0],
            angle: 0.0,
            link: true,
        }
    }

    /// The overlay's pattern as a fill, to sample it.
    pub fn as_fill(&self) -> crate::Fill {
        crate::Fill::Pattern {
            pattern: self.pattern.clone(),
            scale: self.scale,
            offset: self.offset,
            angle: self.angle,
        }
    }
}

/// Samples a pattern tiled across the canvas: canvas pixel centre `p`
/// maps to pattern space by `inv · (p − offset)`, bilinear with
/// wrap-around, so 100% shows the pixels exactly and other scales blend
/// neighbours.
#[derive(Clone, Debug)]
pub struct PatternSampler {
    pub image: Option<Arc<Raster>>,
    /// Canvas → pattern-space linear map (inverse of scale · rotation).
    pub inv: [[f32; 2]; 2],
    pub offset: [f32; 2],
}

impl PatternSampler {
    pub fn new(pattern: &PatternRef, scale: f32, offset: [f32; 2], angle: f32) -> Self {
        let s = if scale.is_finite() {
            scale.clamp(0.01, 100.0)
        } else {
            1.0
        };
        let a = if angle.is_finite() {
            angle.to_radians()
        } else {
            0.0
        };
        // Rotating counter-clockwise on screen (y down) is a clockwise
        // matrix rotation; invert it: p_pattern = R(-a)·p / s.
        let (sin, cos) = a.sin_cos();
        let inv = [[cos / s, -sin / s], [sin / s, cos / s]];
        let offset = offset.map(|v| if v.is_finite() { v } else { 0.0 });
        PatternSampler {
            image: pattern.image.clone(),
            inv,
            offset,
        }
    }

    #[inline]
    pub fn sample(&self, x: i32, y: i32) -> Rgba {
        let Some(img) = &self.image else {
            return Rgba::TRANSPARENT;
        };
        let (px, py) = (x as f32 + 0.5 - self.offset[0], y as f32 + 0.5 - self.offset[1]);
        let u = self.inv[0][0] * px + self.inv[0][1] * py - 0.5;
        let v = self.inv[1][0] * px + self.inv[1][1] * py - 0.5;
        sample_wrapped(img, u, v)
    }
}

/// Bilinear sample of `img` at `(u, v)` (pixel centres on integers), the
/// image repeating in both directions.
#[inline]
pub fn sample_wrapped(img: &Raster, u: f32, v: f32) -> Rgba {
    let (w, h) = (img.width as i64, img.height as i64);
    if w == 0 || h == 0 {
        return Rgba::TRANSPARENT;
    }
    let (fu, fv) = (u.floor(), v.floor());
    let (tx, ty) = (u - fu, v - fv);
    let x0 = (fu as i64).rem_euclid(w);
    let y0 = (fv as i64).rem_euclid(h);
    let x1 = (x0 + 1) % w;
    let y1 = (y0 + 1) % h;
    let at = |x: i64, y: i64| img.pixels[(y * w + x) as usize];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    if tx == 0.0 && ty == 0.0 {
        return a;
    }
    let lerp = |p: Rgba, q: Rgba, t: f32| {
        Rgba::new(
            p.r + (q.r - p.r) * t,
            p.g + (q.g - p.g) * t,
            p.b + (q.b - p.b) * t,
            p.a + (q.a - p.a) * t,
        )
    };
    lerp(lerp(a, b, tx), lerp(c, d, tx), ty)
}

fn same(a: &Option<Arc<Raster>>, b: &Option<Arc<Raster>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl crate::Document {
    /// The document pattern a reference names: by id, else by name.
    pub fn find_pattern(&self, r: &PatternRef) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == r.id).or_else(|| {
            self.patterns
                .iter()
                .find(|p| !r.name.is_empty() && p.name == r.name)
        })
    }

    /// Add `p`, or replace the pattern with its id. Returns its index.
    pub fn upsert_pattern(&mut self, p: Pattern) -> usize {
        match self.patterns.iter().position(|q| q.id == p.id) {
            Some(i) => {
                self.patterns[i] = p;
                i
            }
            None => {
                self.patterns.push(p);
                self.patterns.len() - 1
            }
        }
    }

    /// Point every pattern reference (pattern fill layers, shape fills,
    /// Pattern Overlay effects) at the document's pixels. A reference that
    /// carries pixels the document lacks adds them as a pattern, so the
    /// document stays self-contained. Caches drawn with other pixels are
    /// dropped (the fill refresh redraws them). Returns whether anything
    /// changed.
    pub fn resolve_patterns(&mut self) -> bool {
        let mut known = self.patterns.clone();
        let mut adopted: Vec<Pattern> = Vec::new();
        let mut changed = false;
        let mut fix = |r: &mut PatternRef| -> bool {
            let found = known
                .iter()
                .find(|p| p.id == r.id)
                .or_else(|| known.iter().find(|p| !r.name.is_empty() && p.name == r.name));
            let want = match (found, &r.image) {
                (Some(p), _) => Some(p.image.clone()),
                (None, Some(img)) if !r.id.is_empty() => {
                    let p = Pattern {
                        id: r.id.clone(),
                        name: r.name.clone(),
                        image: img.clone(),
                    };
                    known.push(p.clone());
                    adopted.push(p);
                    Some(img.clone())
                }
                (None, _) => None,
            };
            if same(&r.image, &want) {
                false
            } else {
                r.image = want;
                true
            }
        };
        self.for_each_layer_mut(|l| {
            match &mut l.content {
                crate::LayerContent::Fill(f) => {
                    if let crate::Fill::Pattern { pattern, .. } = &mut f.fill {
                        if fix(pattern) {
                            f.cache = None;
                            changed = true;
                        }
                    }
                }
                crate::LayerContent::Shape(s) => {
                    if let Some(crate::Fill::Pattern { pattern, .. }) = &mut s.fill {
                        if fix(pattern) {
                            s.cache = None;
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
            if let Some(po) = &mut l.effects.pattern_overlay {
                changed |= fix(&mut po.pattern);
            }
        });
        changed |= !adopted.is_empty();
        self.patterns.extend(adopted);
        changed
    }

    /// Ids of the patterns some layer uses, in first-use order.
    pub fn used_pattern_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        let mut add = |r: &PatternRef| {
            if !ids.contains(&r.id) {
                ids.push(r.id.clone());
            }
        };
        self.for_each_layer(|l| {
            match &l.content {
                crate::LayerContent::Fill(f) => {
                    if let crate::Fill::Pattern { pattern, .. } = &f.fill {
                        add(pattern);
                    }
                }
                crate::LayerContent::Shape(s) => {
                    if let Some(crate::Fill::Pattern { pattern, .. }) = &s.fill {
                        add(pattern);
                    }
                }
                _ => {}
            }
            if let Some(po) = &l.effects.pattern_overlay {
                add(&po.pattern);
            }
        });
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4×2 pattern whose red channel is the pixel index / 8.
    fn ramp() -> PatternRef {
        let mut r = Raster::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                let i = (y * 4 + x) as f32;
                r.set(x, y, Rgba::new(i / 8.0, 0.0, 0.0, 1.0));
            }
        }
        Pattern::new("p", "Ramp", r).reference()
    }

    #[test]
    fn tiles_with_the_pattern_period_from_the_canvas_origin() {
        let s = PatternSampler::new(&ramp(), 1.0, [0.0, 0.0], 0.0);
        assert_eq!(s.sample(0, 0).r, 0.0);
        assert_eq!(s.sample(3, 0).r, 3.0 / 8.0);
        assert_eq!(s.sample(1, 1).r, 5.0 / 8.0);
        // One period later, and one before the origin.
        assert_eq!(s.sample(4, 0).r, 0.0);
        assert_eq!(s.sample(7, 3).r, 7.0 / 8.0);
        assert_eq!(s.sample(-1, -1).r, 7.0 / 8.0);
        assert_eq!(s.sample(-4, 2).r, 0.0);
    }

    #[test]
    fn offset_shifts_the_phase_in_canvas_pixels() {
        let s = PatternSampler::new(&ramp(), 1.0, [1.0, 1.0], 0.0);
        // Canvas (1, 1) is the pattern's (0, 0).
        assert_eq!(s.sample(1, 1).r, 0.0);
        assert_eq!(s.sample(0, 0).r, 7.0 / 8.0);
        assert_eq!(s.sample(2, 1).r, 1.0 / 8.0);
    }

    #[test]
    fn scale_200_doubles_the_period_and_50_halves_it() {
        let s = PatternSampler::new(&ramp(), 2.0, [0.0, 0.0], 0.0);
        // Period 8: canvas x 8 repeats canvas x 0.
        assert!((s.sample(0, 0).r - s.sample(8, 0).r).abs() < 1e-6);
        // Canvas pixel 1 at 200% sits at u = 1.5 / 2 − 0.5 = 0.25 (a quarter
        // of the way from pattern x 0 to 1) and v = −0.25 (a quarter into the
        // wrapped last row, whose red is 4/8 higher): 0.25/8 + 0.25 · 0.5.
        assert!((s.sample(1, 0).r - 0.15625).abs() < 1e-6, "{}", s.sample(1, 0).r);
        // Canvas 2 → u = 0.75: 0.75/8 + 0.125.
        assert!((s.sample(2, 0).r - 0.21875).abs() < 1e-6, "{}", s.sample(2, 0).r);
        let h = PatternSampler::new(&ramp(), 0.5, [0.0, 0.0], 0.0);
        // Period 2: canvas 0 is u = v = 0.5, the mean of the 2×2 block
        // (0, 1, 4, 5)/8 = 2.5/8; canvas 1 is the block (2, 3, 6, 7)/8.
        assert!((h.sample(0, 0).r - 0.3125).abs() < 1e-6, "{}", h.sample(0, 0).r);
        assert!((h.sample(1, 0).r - 0.5625).abs() < 1e-6, "{}", h.sample(1, 0).r);
        assert!((h.sample(2, 0).r - h.sample(0, 0).r).abs() < 1e-6);
    }

    #[test]
    fn rotation_by_90_turns_rows_into_columns() {
        let s = PatternSampler::new(&ramp(), 1.0, [0.0, 0.0], 90.0);
        // Rotated counter-clockwise: walking up the canvas walks along a
        // pattern row.
        let a = s.sample(0, -1).r;
        let b = s.sample(0, -2).r;
        assert!((b - a - 1.0 / 8.0).abs() < 1e-4, "{a} {b}");
    }

    #[test]
    fn missing_pixels_sample_transparent_and_refs_compare_saved_fields() {
        let mut r = ramp();
        r.image = None;
        let s = PatternSampler::new(&r, 1.0, [0.0, 0.0], 0.0);
        assert_eq!(s.sample(0, 0), Rgba::TRANSPARENT);
        assert_eq!(r, ramp(), "the pixels are derived");
        let json = serde_json::to_string(&ramp()).unwrap();
        assert_eq!(json, r#"{"id":"p","name":"Ramp"}"#);
    }

    #[test]
    fn display_names_strip_photoshop_localisation_keys() {
        assert_eq!(display_name("$$$/Patterns/Defaults/Water=Water\0"), "Water");
        assert_eq!(display_name("Bricks"), "Bricks");
    }

    #[test]
    fn ids_are_uuid_shaped_and_unique() {
        let a = new_pattern_id(1);
        let b = new_pattern_id(1);
        assert_eq!(a.len(), 36);
        assert_eq!(a.matches('-').count(), 4);
        assert_ne!(a, b);
    }

    #[test]
    fn resolving_points_refs_at_document_pixels_and_adopts_strays() {
        use crate::{Document, Fill, Layer};
        let mut doc = Document::new(8, 8);
        let p = Pattern::new("a", "A", Raster::filled(2, 2, Rgba::WHITE));
        doc.patterns.push(p.clone());
        let unresolved = PatternRef {
            id: "a".into(),
            name: "A".into(),
            image: None,
        };
        let id = doc.alloc_id();
        let fill = Fill::Pattern {
            pattern: unresolved,
            scale: 1.0,
            offset: [0.0, 0.0],
            angle: 0.0,
        };
        doc.add_layer(Layer::fill(id, fill));
        // A stray reference carrying its own pixels.
        let stray = Pattern::new("b", "B", Raster::filled(3, 1, Rgba::BLACK));
        let id2 = doc.alloc_id();
        let mut l = Layer::pixel(id2, "fx");
        l.effects.pattern_overlay = Some(PatternOverlayFx::new(stray.reference()));
        doc.add_layer(l);
        assert!(doc.resolve_patterns());
        let Fill::Pattern { pattern, .. } = &doc.layer(id).unwrap().fill_layer().unwrap().fill else {
            panic!()
        };
        assert!(Arc::ptr_eq(pattern.image.as_ref().unwrap(), &p.image));
        assert_eq!(doc.patterns.len(), 2, "the stray pattern was adopted");
        assert_eq!(doc.patterns[1].id, "b");
        assert_eq!(doc.used_pattern_ids(), vec!["a".to_string(), "b".to_string()]);
        assert!(!doc.resolve_patterns(), "a second pass changes nothing");
        // Replacing a pattern's pixels re-points the reference.
        doc.upsert_pattern(Pattern::new("a", "A", Raster::filled(2, 2, Rgba::BLACK)));
        assert!(doc.resolve_patterns());
        assert!(doc.layer(id).unwrap().fill_layer().unwrap().cache.is_none());
    }

    #[test]
    fn oversized_sources_are_cropped() {
        let p = Pattern::new("x", "big", Raster::new(2000, 10));
        assert_eq!((p.width(), p.height()), (1024, 10));
        let e = Pattern::new("y", "empty", Raster::new(0, 0));
        assert_eq!((e.width(), e.height()), (1, 1));
    }
}
