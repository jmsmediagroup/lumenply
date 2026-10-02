//! Layer masks as Photoshop stores them. A layer can carry a pixel ("user")
//! mask, a vector mask, or both. With both, the pixel mask moves to channel
//! −3 with its own "real" rectangle and default, and channel −2 holds the
//! vector mask rendered to pixels. Either mask may have a density and a
//! feather applied non-destructively (mask parameters, flag bit 4).
//!
//! Lumenply keeps one mask per layer, so the reader applies density and
//! feather and multiplies the two masks, as Photoshop composites them.
//! Vector masks are rasterised from their path, with Photoshop's path
//! operations (combine, subtract, intersect, exclude) between components.

use lumenply_doc::{Mask, PathNode, SubPath, VectorPath};
use lumenply_tiles::{Rect, Rgba, Tile, TileStore};

use super::Rd;

const FIXED: f32 = (1u32 << 24) as f32;

/// A path component's operation and its flattened outlines.
type Component = (i16, Vec<Vec<(f32, f32)>>);

/// The layer record's mask data.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MaskRecord {
    pub rect: Rect,
    pub default: u8,
    pub flags: u8,
    /// The pixel mask's rectangle, default and flags when the layer has
    /// both masks (channel −3).
    pub real: Option<(Rect, u8, u8)>,
    pub user_density: Option<u8>,
    pub user_feather: Option<f64>,
    pub vector_density: Option<u8>,
    pub vector_feather: Option<f64>,
}

impl MaskRecord {
    /// Channel −2 is the vector mask rendered, not a pixel mask.
    pub fn rendered_vector(&self) -> bool {
        self.flags & 0x08 != 0
    }
}

/// Parse the mask data block (its length already stripped). `has_real`
/// says whether the layer stores a −3 channel, the only reliable sign
/// that the "real" fields are present. `None` for an empty block.
pub(super) fn read_record(
    data: &[u8],
    has_real: bool,
    rect: impl Fn(i32, i32, i32, i32) -> Option<Rect>,
) -> Option<MaskRecord> {
    if data.len() < 18 {
        return None;
    }
    let mut d = Rd::new(data);
    let (t, l, b, r) = (d.i32().ok()?, d.i32().ok()?, d.i32().ok()?, d.i32().ok()?);
    let mut rec = MaskRecord {
        rect: rect(l, t, r, b)?,
        default: d.u8().ok()?,
        flags: d.u8().ok()?,
        ..Default::default()
    };
    if has_real && data.len() >= 36 {
        let flags = d.u8().ok()?;
        let default = d.u8().ok()?;
        let (t, l, b, r) = (d.i32().ok()?, d.i32().ok()?, d.i32().ok()?, d.i32().ok()?);
        rec.real = Some((rect(l, t, r, b)?, default, flags));
    }
    if rec.flags & 0x10 != 0 {
        if let Ok(p) = d.u8() {
            if p & 1 != 0 {
                rec.user_density = d.u8().ok();
            }
            if p & 2 != 0 {
                rec.user_feather = d
                    .bytes(8)
                    .ok()
                    .map(|b| f64::from_be_bytes(b.try_into().expect("8")));
            }
            if p & 4 != 0 {
                rec.vector_density = d.u8().ok();
            }
            if p & 8 != 0 {
                rec.vector_feather = d
                    .bytes(8)
                    .ok()
                    .map(|b| f64::from_be_bytes(b.try_into().expect("8")));
            }
        }
    }
    Some(rec)
}

/// Everything a layer stores about its masks.
#[derive(Default)]
pub(super) struct LayerMasks {
    pub record: Option<MaskRecord>,
    /// Channel −2 and −3 samples (full range).
    pub minus2: Option<Vec<u16>>,
    pub minus3: Option<Vec<u16>>,
    /// The `vmsk` / `vsms` body.
    pub vector: Option<Vec<u8>>,
    /// The layer is a fill clipped by its vector mask (a shape), so its
    /// stored pixels are already the clipped rendering.
    pub shaped: bool,
}

impl LayerMasks {
    /// The layer's mask: the pixel mask times the vector mask, each with
    /// its density and feather. `shape` layers clip by their own outline,
    /// so their vector mask is left out.
    pub fn build(&self, width: u32, height: u32, shape: bool) -> Option<Mask> {
        let shape = shape || self.shaped;
        let rec = self.record.as_ref();
        // The pixel mask.
        let user = match (rec, &self.minus3, &self.minus2) {
            (Some(r), Some(p), _) if r.real.is_some() => {
                let (rect, default, flags) = r.real.expect("checked");
                Some(plane_mask(rect, p, default, flags))
            }
            (Some(r), _, p) if !(r.rendered_vector() && (self.vector.is_some() || shape)) => {
                let p = p.as_deref().unwrap_or(&[]);
                Some(plane_mask(r.rect, p, r.default, r.flags))
            }
            _ => None,
        }
        .map(|m| {
            refine(
                m,
                rec.and_then(|r| r.user_density),
                rec.and_then(|r| r.user_feather),
            )
        });
        // The vector mask, unless the layer is a shape.
        let vector = if shape {
            None
        } else {
            // Drawn from the path; Photoshop's rendering of it (−2) is
            // not always the vector mask alone, so it is only a fallback.
            let drawn = self.vector.as_deref().and_then(|v| rasterize(v, width, height));
            let rendered = || match (rec, &self.minus2) {
                (Some(r), Some(p)) if r.rendered_vector() && self.vector.is_none() => {
                    Some(plane_mask(r.rect, p, r.default, r.flags & !0x02))
                }
                _ => None,
            };
            drawn.or_else(rendered).map(|m| {
                refine(
                    m,
                    rec.and_then(|r| r.vector_density),
                    rec.and_then(|r| r.vector_feather),
                )
            })
        };
        match (user, vector) {
            (Some(u), Some(v)) if u.enabled => Some(multiply(&u, &v)),
            (Some(u), Some(v)) => {
                // A disabled pixel mask leaves the vector mask in charge.
                let _ = u;
                Some(v)
            }
            (u, v) => u.or(v),
        }
    }
}

/// A mask from a decoded plane covering `rect`; elsewhere `default`.
fn plane_mask(rect: Rect, plane: &[u16], default: u8, flags: u8) -> Mask {
    let d = default as f32 / 255.0;
    let vals: Vec<f32> = if plane.len() == (rect.w * rect.h) as usize {
        plane.iter().map(|&s| s as f32 / 65535.0).collect()
    } else {
        Vec::new()
    };
    let mut m = if vals.is_empty() {
        Mask {
            tiles: TileStore::new(),
            default: d,
            enabled: true,
        }
    } else {
        from_dense(rect, &vals, d)
    };
    m.enabled = flags & 0x02 == 0;
    m
}

/// Density lifts the hidden parts towards visible (100% = as painted);
/// feather blurs the edge.
fn refine(mut m: Mask, density: Option<u8>, feather: Option<f64>) -> Mask {
    if let Some(d) = density.filter(|&d| d < 255) {
        let d = d as f32 / 255.0;
        let f = |v: f32| 1.0 - d * (1.0 - v);
        let region = m.tiles.bounds();
        let vals = region.map(|r| dense(&m, r));
        let default = f(m.default);
        let enabled = m.enabled;
        m = match (region, vals) {
            (Some(r), Some(v)) => from_dense(r, &v.into_iter().map(f).collect::<Vec<_>>(), default),
            _ => Mask {
                tiles: TileStore::new(),
                default,
                enabled,
            },
        };
        m.enabled = enabled;
    }
    if let Some(r) = feather.filter(|r| r.is_finite() && *r > 0.0) {
        m.feather(r.min(1000.0) as f32);
    }
    m
}

/// Coverage of `m` over `region`, one value per pixel.
fn dense(m: &Mask, region: Rect) -> Vec<f32> {
    let mut out = vec![m.default; (region.w * region.h) as usize];
    for c in region.tiles() {
        let Some(tile) = m.tiles.tile(c) else { continue };
        let px = tile.pixels();
        let (ox, oy) = c.origin();
        let sub = region.intersect(&c.rect());
        for y in sub.y..sub.bottom() {
            for x in sub.x..sub.right() {
                let src = ((y - oy) as usize) * lumenply_tiles::TILE_SIZE + (x - ox) as usize;
                out[((y - region.y) as u32 * region.w + (x - region.x) as u32) as usize] = px[src].a;
            }
        }
    }
    out
}

/// A mask holding `vals` over `region` and `default` everywhere else.
fn from_dense(region: Rect, vals: &[f32], default: f32) -> Mask {
    let mut m = Mask {
        tiles: TileStore::new(),
        default,
        enabled: true,
    };
    for c in region.tiles() {
        let (ox, oy) = c.origin();
        let sub = region.intersect(&c.rect());
        let mut tile = Tile::filled(Rgba::new(default, default, default, default));
        let mut differs = false;
        for y in sub.y..sub.bottom() {
            for x in sub.x..sub.right() {
                let v =
                    vals[((y - region.y) as u32 * region.w + (x - region.x) as u32) as usize].clamp(0.0, 1.0);
                differs |= v != default;
                tile.set((x - ox) as usize, (y - oy) as usize, Rgba::new(v, v, v, v));
            }
        }
        if differs {
            m.tiles.insert(c, std::sync::Arc::new(tile));
        }
    }
    m
}

/// Both masks at once: their product.
fn multiply(a: &Mask, b: &Mask) -> Mask {
    let region = match (a.tiles.bounds(), b.tiles.bounds()) {
        (Some(x), Some(y)) => Some(x.union(&y)),
        (x, y) => x.or(y),
    };
    let default = a.default * b.default;
    match region {
        Some(r) => {
            let (va, vb) = (dense(a, r), dense(b, r));
            let vals: Vec<f32> = va.iter().zip(&vb).map(|(x, y)| x * y).collect();
            from_dense(r, &vals, default)
        }
        None => Mask {
            tiles: TileStore::new(),
            default,
            enabled: true,
        },
    }
}

/// Rasterise a `vmsk` / `vsms` body over a `width × height` canvas.
/// `None` when it is disabled or unreadable.
pub(super) fn rasterize(data: &[u8], width: u32, height: u32) -> Option<Mask> {
    let mut d = Rd::new(data);
    if d.u32().ok()? != 3 {
        return None;
    }
    let flags = d.u32().ok()?;
    if flags & 0b100 != 0 {
        return None;
    }
    let invert = flags & 1 != 0;
    // Components: a subpath with operation −1 joins the one before.
    let mut comps: Vec<(i16, VectorPath)> = Vec::new();
    let mut initial_fill = false;
    while d.pos + 26 <= data.len() {
        let sel = d.u16().ok()?;
        match sel {
            0 | 3 => {
                let _count = d.u16().ok()?;
                let op = d.i16().ok()?;
                d.skip(20).ok()?;
                let sp = SubPath {
                    nodes: Vec::new(),
                    closed: true,
                };
                match comps.last_mut() {
                    Some((_, p)) if op == -1 => p.subpaths.push(sp),
                    _ => comps.push((if op == -1 { 1 } else { op }, VectorPath { subpaths: vec![sp] })),
                }
            }
            1 | 2 | 4 | 5 => {
                let mut pts = [(0.0f32, 0.0f32); 3];
                for p in &mut pts {
                    let y = d.i32().ok()?;
                    let x = d.i32().ok()?;
                    *p = (x as f32 / FIXED * width as f32, y as f32 / FIXED * height as f32);
                }
                let sp = comps.last_mut()?.1.subpaths.last_mut()?;
                if sp.nodes.len() < 100_000 {
                    sp.nodes.push(PathNode {
                        handle_in: pts[0],
                        point: pts[1],
                        handle_out: pts[2],
                    });
                }
            }
            8 => {
                initial_fill = d.u16().ok()? == 1;
                d.skip(22).ok()?;
            }
            _ => d.skip(24).ok()?,
        }
    }
    let canvas = Rect::new(0, 0, width, height);
    let polys: Vec<Component> = comps
        .iter()
        .map(|(op, p)| (*op, p.flatten().into_iter().map(|(pts, _)| pts).collect()))
        .collect();
    // Where any path reaches; outside it every component is empty.
    let mut region: Option<Rect> = None;
    for (x, y) in polys.iter().flat_map(|(_, ps)| ps.iter().flatten()) {
        let r = Rect::new(x.floor() as i32 - 1, y.floor() as i32 - 1, 3, 3);
        region = Some(region.map_or(r, |g| g.union(&r)));
    }
    let region = region
        .map(|r| r.intersect(&canvas))
        .filter(|r| r.w > 0 && r.h > 0);
    // Subtract or intersect first means "from everything", as Photoshop.
    let start = match polys.first() {
        Some((op, _)) if *op == 2 || *op == 3 => 1.0,
        None if initial_fill => 1.0,
        _ => 0.0,
    };
    let combine = |m: f32, p: f32, op: i16| match op {
        0 => m + p - 2.0 * m * p,
        2 => (m - p).max(0.0),
        3 => m * p,
        _ => m + p - m * p,
    };
    let finish = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if invert {
            1.0 - v
        } else {
            v
        }
    };
    let outside = finish(polys.iter().fold(start, |m, (op, _)| combine(m, 0.0, *op)));
    let Some(region) = region else {
        return Some(Mask {
            tiles: TileStore::new(),
            default: outside,
            enabled: true,
        });
    };
    let mut acc = vec![start; (region.w * region.h) as usize];
    for (op, ps) in &polys {
        let mut plane = Mask::hide_all();
        plane.fill_polygons(ps, 1.0);
        let p = dense(&plane, region);
        for (m, v) in acc.iter_mut().zip(p) {
            *m = combine(*m, v, *op);
        }
    }
    let vals: Vec<f32> = acc.into_iter().map(finish).collect();
    Some(from_dense(region, &vals, outside))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(v: f32, dim: u32) -> i32 {
        (v / dim as f32 * FIXED).round() as i32
    }

    /// A `vmsk` body with closed rectangles, each `(op, x, y, w, h)`.
    fn vmsk(rects: &[(i16, f32, f32, f32, f32)], flags: u32, w: u32, h: u32) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&3u32.to_be_bytes());
        d.extend_from_slice(&flags.to_be_bytes());
        for &(op, x, y, rw, rh) in rects {
            d.extend_from_slice(&0u16.to_be_bytes());
            d.extend_from_slice(&4u16.to_be_bytes());
            d.extend_from_slice(&op.to_be_bytes());
            d.extend_from_slice(&[0; 20]);
            for (px, py) in [(x, y), (x + rw, y), (x + rw, y + rh), (x, y + rh)] {
                d.extend_from_slice(&1u16.to_be_bytes());
                for _ in 0..3 {
                    d.extend_from_slice(&fixed(py, h).to_be_bytes());
                    d.extend_from_slice(&fixed(px, w).to_be_bytes());
                }
            }
        }
        d
    }

    #[test]
    fn vector_masks_rasterise_with_path_operations() {
        let (w, h) = (100, 80);
        let union = rasterize(&vmsk(&[(1, 10.0, 10.0, 40.0, 40.0)], 0, w, h), w, h).unwrap();
        assert_eq!(union.value(30, 30), 1.0);
        assert_eq!(union.value(5, 5), 0.0);
        assert_eq!(union.value(80, 70), 0.0);
        // Intersect: only the overlap of the two squares stays.
        let both = [(1, 10.0, 10.0, 40.0, 40.0), (3, 30.0, 30.0, 40.0, 40.0)];
        let inter = rasterize(&vmsk(&both, 0, w, h), w, h).unwrap();
        assert_eq!(inter.value(40, 40), 1.0);
        assert_eq!(inter.value(15, 15), 0.0);
        assert_eq!(inter.value(60, 60), 0.0);
        // Subtract.
        let sub = [(1, 10.0, 10.0, 40.0, 40.0), (2, 30.0, 30.0, 40.0, 40.0)];
        let sub = rasterize(&vmsk(&sub, 0, w, h), w, h).unwrap();
        assert_eq!(sub.value(15, 15), 1.0);
        assert_eq!(sub.value(40, 40), 0.0);
        // Exclude.
        let xor = [(1, 10.0, 10.0, 40.0, 40.0), (0, 30.0, 30.0, 40.0, 40.0)];
        let xor = rasterize(&vmsk(&xor, 0, w, h), w, h).unwrap();
        assert_eq!(
            (xor.value(15, 15), xor.value(40, 40), xor.value(60, 60)),
            (1.0, 0.0, 1.0)
        );
        // Inverted, and disabled.
        let inv = rasterize(&vmsk(&[(1, 10.0, 10.0, 40.0, 40.0)], 1, w, h), w, h).unwrap();
        assert_eq!(
            (inv.value(30, 30), inv.value(5, 5), inv.value(90, 75)),
            (0.0, 1.0, 1.0)
        );
        assert!(rasterize(&vmsk(&[(1, 10.0, 10.0, 40.0, 40.0)], 4, w, h), w, h).is_none());
        // A first subtract cuts from everything.
        let first = rasterize(&vmsk(&[(2, 10.0, 10.0, 40.0, 40.0)], 0, w, h), w, h).unwrap();
        assert_eq!(
            (first.value(30, 30), first.value(5, 5), first.value(90, 75)),
            (0.0, 1.0, 1.0)
        );
    }

    #[test]
    fn mask_records_read_real_masks_and_parameters() {
        let rect = |l: i32, t: i32, r: i32, b: i32| Some(Rect::new(l, t, (r - l) as u32, (b - t) as u32));
        let mut d = Vec::new();
        for v in [5i32, 4, 25, 44] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        d.extend_from_slice(&[0, 0x08 | 0x10]);
        // Real flags, default, rect.
        d.extend_from_slice(&[0, 255]);
        for v in [1i32, 2, 11, 22] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        // Parameters: user density 128, vector feather 2.5.
        d.push(1 | 8);
        d.push(128);
        d.extend_from_slice(&2.5f64.to_be_bytes());
        let rec = read_record(&d, true, rect).unwrap();
        assert_eq!(rec.rect, Rect::new(4, 5, 40, 20));
        assert!(rec.rendered_vector());
        assert_eq!(rec.real, Some((Rect::new(2, 1, 20, 10), 255, 0)));
        assert_eq!((rec.user_density, rec.vector_feather), (Some(128), Some(2.5)));
        assert_eq!((rec.user_feather, rec.vector_density), (None, None));
        // Without a −3 channel the same bytes are not a real mask.
        let mut short = d[..18].to_vec();
        short.extend_from_slice(&[0, 0]);
        let rec = read_record(&short, false, rect).unwrap();
        assert_eq!(rec.real, None);
    }

    #[test]
    fn density_lifts_hidden_areas_and_both_masks_multiply() {
        // A pixel mask hiding the left half of a 4×2 rect, at 50% density.
        let plane = [0u16, 0, 65535, 65535, 0, 0, 65535, 65535];
        let masks = LayerMasks {
            record: Some(MaskRecord {
                rect: Rect::new(0, 0, 4, 2),
                default: 0,
                flags: 0x10,
                user_density: Some(128),
                ..Default::default()
            }),
            minus2: Some(plane.to_vec()),
            ..Default::default()
        };
        let m = masks.build(10, 10, false).unwrap();
        let half = 1.0 - 128.0 / 255.0;
        assert!((m.value(0, 0) - half).abs() < 1e-3, "{}", m.value(0, 0));
        assert_eq!(m.value(3, 1), 1.0);
        assert!((m.value(8, 8) - half).abs() < 1e-3, "the default lifts too");
        // Adding a vector mask that keeps only x < 3: the product.
        let masks = LayerMasks {
            vector: Some(vmsk(&[(1, 0.0, 0.0, 3.0, 10.0)], 0, 10, 10)),
            record: Some(MaskRecord {
                rect: Rect::new(0, 0, 4, 2),
                ..Default::default()
            }),
            minus2: Some(plane.to_vec()),
            ..Default::default()
        };
        let m = masks.build(10, 10, false).unwrap();
        assert_eq!((m.value(0, 0), m.value(2, 0)), (0.0, 1.0));
        assert!(m.value(3, 0) < 1e-6, "outside the path");
        // A shape layer leaves its vector mask out.
        let m = masks.build(10, 10, true).unwrap();
        assert_eq!(m.value(3, 0), 1.0);
    }
}
