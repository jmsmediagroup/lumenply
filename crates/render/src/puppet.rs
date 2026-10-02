//! Puppet Warp: an as-rigid-as-possible deformation of a triangle mesh laid
//! over a layer's opaque area, driven by pins.
//!
//! **Mesh.** A regular grid is laid over the layer's content bounds grown by
//! the Expansion; every cell that comes within Expansion pixels of a pixel
//! with alpha above [`COVER_ALPHA`] is kept and split into two triangles
//! (diagonals alternate, so the mesh has no preferred direction). Every
//! opaque pixel therefore lies inside the mesh, with an Expansion-wide
//! transparent margin around it. Density sets the cell size. To follow the
//! outline rather than whole-cell stairs, edge cells drop a triangle that
//! holds nothing (picking the diagonal that allows it), and outline
//! vertices are then pulled in to Expansion + ½ px from the shape wherever
//! a check shows no opaque pixel or margin would leave the mesh.
//!
//! **Deformation.** Igarashi, Moscovich & Hughes (2005), "As-rigid-as-
//! possible shape manipulation", in its two closed-form steps:
//! 1. *similarity*: each triangle wants its third vertex where its first
//!    two put it under the rest shape's local frame, so triangles may
//!    rotate and scale uniformly but not shear;
//! 2. *scale adjustment*: each triangle of step 1 is fitted with a rotated
//!    copy of its rest shape (the mode decides how much of step 1's scale
//!    the copy keeps: none for Rigid, half for Normal, all for Distort) and
//!    the mesh is solved so its edges match the fitted edges.
//!
//! Pins are barycentric points of the rest mesh held at their targets by a
//! stiff penalty. Both steps are sparse symmetric positive-definite systems
//! whose matrices depend only on the mesh and *where* the pins sit, so they
//! are factored (banded Cholesky: the grid numbering keeps the bandwidth
//! near one grid row) once per pin set; dragging a pin only re-solves.
//! A connected part of the mesh with no pin stays where it is, one with a
//! single pin follows that pin's translation.
//!
//! **Rendering.** Each deformed triangle maps back to its rest triangle by
//! one affine map; destination pixel centres inside it sample the source
//! bilinearly (premultiplied, so edges against transparency are right).
//! Shared edges belong to exactly one triangle (an exact tie rule on
//! canonically evaluated edge functions), and where the mesh folds over
//! itself triangles are composited "over" in draw order, which pin depth
//! controls.

use std::sync::Arc;

use lumenply_tiles::{Raster, Rect, Rgba, Tile, TileStore, TILE_SIZE};
use rayon::prelude::*;

/// A point in canvas pixels (pixel `(x, y)` covers `[x, x+1] × [y, y+1]`).
pub type Pt = [f64; 2];

/// Alpha above which a pixel belongs to the shape the mesh must cover.
pub const COVER_ALPHA: f32 = 0.5 / 255.0;

/// How stiff the mesh is (Photoshop's Mode).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PuppetMode {
    /// Every triangle keeps its size: limbs bend without swelling.
    Rigid,
    /// Triangles may grow or shrink a little.
    #[default]
    Normal,
    /// Triangles scale freely (still without shear): stretchy.
    Distort,
}

impl PuppetMode {
    pub const ALL: [PuppetMode; 3] = [PuppetMode::Rigid, PuppetMode::Normal, PuppetMode::Distort];

    pub fn name(self) -> &'static str {
        match self {
            PuppetMode::Rigid => "Rigid",
            PuppetMode::Normal => "Normal",
            PuppetMode::Distort => "Distort",
        }
    }

    /// How much of a triangle's step-one scale its fitted copy keeps.
    fn scale_keep(self) -> f64 {
        match self {
            PuppetMode::Rigid => 0.0,
            PuppetMode::Normal => 0.5,
            PuppetMode::Distort => 1.0,
        }
    }
}

/// How many mesh points (Photoshop's Density).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PuppetDensity {
    Fewer,
    #[default]
    Normal,
    More,
}

impl PuppetDensity {
    pub const ALL: [PuppetDensity; 3] = [PuppetDensity::Fewer, PuppetDensity::Normal, PuppetDensity::More];

    pub fn name(self) -> &'static str {
        match self {
            PuppetDensity::Fewer => "Fewer points",
            PuppetDensity::Normal => "Normal",
            PuppetDensity::More => "More points",
        }
    }

    /// Cells along the longer side of the meshed area.
    fn cells(self) -> f64 {
        match self {
            PuppetDensity::Fewer => 14.0,
            PuppetDensity::Normal => 24.0,
            PuppetDensity::More => 38.0,
        }
    }
}

/// What the mesh is built from, besides the pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshParams {
    pub density: PuppetDensity,
    /// Margin around the opaque area, in pixels (0..=100).
    pub expansion: f32,
}

impl Default for MeshParams {
    fn default() -> Self {
        MeshParams {
            density: PuppetDensity::Normal,
            expansion: 2.0,
        }
    }
}

/// Smallest cell size, in pixels.
const MIN_STEP: i32 = 4;
/// Most cells a mesh may have; denser requests get bigger cells.
const MAX_CELLS: usize = 6000;

/// One pin: the rest point it holds (`from`, canvas pixels on the unwarped
/// layer), where it holds it (`to`), and its depth (higher draws on top
/// where the mesh folds over itself).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PuppetPin {
    pub from: Pt,
    pub to: Pt,
    pub depth: i32,
}

impl PuppetPin {
    /// A pin that holds `at` where it is.
    pub fn at(at: Pt) -> PuppetPin {
        PuppetPin {
            from: at,
            to: at,
            depth: 0,
        }
    }
}

/// The triangle mesh over a layer's opaque area.
#[derive(Clone, Debug)]
pub struct PuppetMesh {
    /// Rest positions of the vertices, canvas pixels.
    pub rest: Vec<Pt>,
    /// Triangles as vertex indices, all wound with positive area (y down).
    pub tris: Vec<[u32; 3]>,
    origin: (i32, i32),
    step: i32,
    cols: usize,
    rows: usize,
    /// First triangle and triangle count of each grid cell (`u32::MAX`
    /// outside the mesh).
    cell_tri: Vec<(u32, u8)>,
}

/// Where a point sits in a mesh: a triangle and barycentric weights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshPoint {
    pub tri: u32,
    pub bary: [f64; 3],
}

#[inline]
fn cross(a: Pt, b: Pt) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

#[inline]
fn sub(a: Pt, b: Pt) -> Pt {
    [a[0] - b[0], a[1] - b[1]]
}

/// Barycentric weights of `p` in triangle `(a, b, c)`; `None` if degenerate.
fn barycentric(p: Pt, a: Pt, b: Pt, c: Pt) -> Option<[f64; 3]> {
    let area = cross(sub(b, a), sub(c, a));
    if area.abs() < 1e-12 {
        return None;
    }
    let wa = cross(sub(c, b), sub(p, b)) / area;
    let wb = cross(sub(a, c), sub(p, c)) / area;
    Some([wa, wb, 1.0 - wa - wb])
}

const INSIDE_EPS: f64 = 1e-9;

/// Which pixels of an area are opaque enough to need meshing.
struct OpaqueMap {
    area: Rect,
    bits: Vec<bool>,
}

impl OpaqueMap {
    fn new(store: &TileStore, area: Rect) -> OpaqueMap {
        let mut bits = vec![false; area.w as usize * area.h as usize];
        for c in area.tiles() {
            let Some(tile) = store.tile(c) else { continue };
            let px = tile.pixels(); // once per tile
            let (ox, oy) = c.origin();
            let r = c.rect().intersect(&area);
            for y in r.y..r.bottom() {
                let row = (y - area.y) as usize * area.w as usize;
                for x in r.x..r.right() {
                    bits[row + (x - area.x) as usize] =
                        px[(y - oy) as usize * TILE_SIZE + (x - ox) as usize].a > COVER_ALPHA;
                }
            }
        }
        OpaqueMap { area, bits }
    }

    /// The nearest point of any opaque pixel's square to `p` within `r`,
    /// with its distance.
    fn nearest(&self, p: Pt, r: f64) -> Option<(f64, Pt)> {
        let mut best: Option<(f64, Pt)> = None;
        let x0 = ((p[0] - r).floor() as i32).max(self.area.x);
        let x1 = ((p[0] + r).ceil() as i32).min(self.area.right() - 1);
        let y0 = ((p[1] - r).floor() as i32).max(self.area.y);
        let y1 = ((p[1] + r).ceil() as i32).min(self.area.bottom() - 1);
        for y in y0..=y1 {
            let row = (y - self.area.y) as usize * self.area.w as usize;
            for x in x0..=x1 {
                if !self.bits[row + (x - self.area.x) as usize] {
                    continue;
                }
                let q = [
                    p[0].clamp(x as f64, x as f64 + 1.0),
                    p[1].clamp(y as f64, y as f64 + 1.0),
                ];
                let d = (q[0] - p[0]).hypot(q[1] - p[1]);
                if d <= r && best.is_none_or(|b| d < b.0) {
                    best = Some((d, q));
                }
            }
        }
        best
    }

    /// Whether `f` holds for some opaque pixel in the inclusive ranges.
    fn any_in(&self, (x0, x1): (i32, i32), (y0, y1): (i32, i32), f: impl Fn(i32, i32) -> bool) -> bool {
        let (x0, x1) = (x0.max(self.area.x), x1.min(self.area.right() - 1));
        let (y0, y1) = (y0.max(self.area.y), y1.min(self.area.bottom() - 1));
        for y in y0..=y1 {
            let row = (y - self.area.y) as usize * self.area.w as usize;
            for x in x0..=x1 {
                if self.bits[row + (x - self.area.x) as usize] && f(x, y) {
                    return true;
                }
            }
        }
        false
    }

    /// Whether triangle `t` meets the square of some opaque pixel grown by
    /// `e` on every side (separating-axis test; touching counts).
    fn touches(&self, t: [Pt; 3], e: f64) -> bool {
        let lo_x = t.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let hi_x = t.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let lo_y = t.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let hi_y = t.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        let x0 = ((lo_x - e - 1.0).floor() as i32).max(self.area.x);
        let x1 = ((hi_x + e).ceil() as i32).min(self.area.right() - 1);
        let y0 = ((lo_y - e - 1.0).floor() as i32).max(self.area.y);
        let y1 = ((hi_y + e).ceil() as i32).min(self.area.bottom() - 1);
        if x0 > x1 || y0 > y1 {
            return false;
        }
        // The triangle's edge normals and its extent along each.
        let axes: [(Pt, f64, f64); 3] = std::array::from_fn(|k| {
            let (p, q, r) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            let n = [q[1] - p[1], p[0] - q[0]];
            let (a, b) = (n[0] * p[0] + n[1] * p[1], n[0] * r[0] + n[1] * r[1]);
            (n, a.min(b), a.max(b))
        });
        for y in y0..=y1 {
            let row = (y - self.area.y) as usize * self.area.w as usize;
            let (by0, by1) = (y as f64 - e, y as f64 + 1.0 + e);
            if by1 < lo_y || by0 > hi_y {
                continue;
            }
            for x in x0..=x1 {
                if !self.bits[row + (x - self.area.x) as usize] {
                    continue;
                }
                let (bx0, bx1) = (x as f64 - e, x as f64 + 1.0 + e);
                if bx1 < lo_x || bx0 > hi_x {
                    continue;
                }
                let separated = axes.iter().any(|&(n, lo, hi)| {
                    let proj =
                        [[bx0, by0], [bx1, by0], [bx0, by1], [bx1, by1]].map(|c| n[0] * c[0] + n[1] * c[1]);
                    let cmin = proj.iter().copied().fold(f64::INFINITY, f64::min);
                    let cmax = proj.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    cmax < lo || cmin > hi
                });
                if !separated {
                    return true;
                }
            }
        }
        false
    }
}

impl PuppetMesh {
    /// The mesh over `store`'s opaque pixels, or `None` for an empty layer.
    pub fn build(store: &TileStore, params: MeshParams) -> Option<PuppetMesh> {
        let b = store.content_bounds()?;
        let e = params.expansion.clamp(0.0, 100.0).ceil() as i32;
        let (gw, gh) = (b.w as i32 + 2 * e, b.h as i32 + 2 * e);
        let long = gw.max(gh) as f64;
        let mut step = ((long / params.density.cells()).round() as i32).max(MIN_STEP);
        let dims = |s: i32| {
            (
                ((gw + s - 1) / s).max(1) as usize,
                ((gh + s - 1) / s).max(1) as usize,
            )
        };
        while dims(step).0 * dims(step).1 > MAX_CELLS {
            step += 1;
        }
        let (cols, rows) = dims(step);
        let origin = (b.x - e, b.y - e);
        // The cells a pixel's square, grown by `e`, overlaps: along one
        // axis, pixel `x` reaches cells floor((x-e-o)/s) ..= floor((x+e-o)/s).
        let span = |x: i32, o: i32, n: usize| {
            let lo = (x - e - o).div_euclid(step).clamp(0, n as i32 - 1) as usize;
            let hi = (x + e - o).div_euclid(step).clamp(0, n as i32 - 1) as usize;
            (lo, hi)
        };
        let xs: Vec<(usize, usize)> = (b.x..b.right()).map(|x| span(x, origin.0, cols)).collect();
        let ys: Vec<(usize, usize)> = (b.y..b.bottom()).map(|y| span(y, origin.1, rows)).collect();
        let marks: Vec<Vec<bool>> = b
            .tiles()
            .into_par_iter()
            .filter_map(|c| {
                let tile = store.tile(c)?;
                let px = tile.pixels(); // once per tile (compact tiles convert)
                let (ox, oy) = c.origin();
                let r = c.rect().intersect(&b);
                let mut cell = vec![false; cols * rows];
                for y in r.y..r.bottom() {
                    let (j0, j1) = ys[(y - b.y) as usize];
                    let row = &px[(y - oy) as usize * TILE_SIZE..];
                    let mut last = (usize::MAX, usize::MAX);
                    for x in r.x..r.right() {
                        if row[(x - ox) as usize].a <= COVER_ALPHA {
                            continue;
                        }
                        let (i0, i1) = xs[(x - b.x) as usize];
                        if (i0, i1) == last {
                            continue;
                        }
                        last = (i0, i1);
                        for j in j0..=j1 {
                            cell[j * cols + i0..=j * cols + i1].fill(true);
                        }
                    }
                }
                Some(cell)
            })
            .collect();
        let mut kept = vec![false; cols * rows];
        for m in &marks {
            for (k, v) in kept.iter_mut().zip(m) {
                *k |= *v;
            }
        }
        // Where the outline crosses a cell, the triangle that holds no
        // opaque pixel (grown by `e`) is left out, so the mesh follows the
        // shape in 45° steps instead of whole-cell stairs.
        let opaque = OpaqueMap::new(store, b);
        let needed = |t: [Pt; 3]| opaque.touches(t, e as f64);
        let mut mesh = Self::from_cells(origin, step, cols, rows, &kept, needed);
        mesh.shrink_wrap(&opaque, e as f64);
        Some(mesh)
    }

    /// Pulls outline vertices in towards the shape, to `e` + ½ px from the
    /// nearest opaque pixel, wherever that keeps every opaque pixel and its
    /// `e` margin inside the mesh and no triangle collapses. A vertex
    /// moves less than one cell in all, so [`PuppetMesh::locate`] finds it
    /// among the neighbouring cells.
    fn shrink_wrap(&mut self, opaque: &OpaqueMap, e: f64) {
        let n = self.rest.len();
        let mut edges: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
        let mut incident: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (t, tv) in self.tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (tv[k], tv[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                incident[tv[k] as usize].push(t);
            }
        }
        let mut outline = vec![false; n];
        for (&(a, b), &c) in &edges {
            if c == 1 {
                outline[a as usize] = true;
                outline[b as usize] = true;
            }
        }
        let step = self.step as f64;
        let want = e + 0.5;
        let home = self.rest.clone();
        for _pass in 0..2 {
            for v in 0..n {
                if !outline[v] {
                    continue;
                }
                let p = self.rest[v];
                // Already snug (a small search settles it for most).
                if opaque.nearest(p, want + 0.25).is_some() {
                    continue;
                }
                let Some((d, q)) = opaque.nearest(p, 1.5 * step + want) else {
                    continue;
                };
                if d <= want + 0.25 {
                    continue;
                }
                let len = (d - want).min(0.75 * step);
                let np = [p[0] + (q[0] - p[0]) / d * len, p[1] + (q[1] - p[1]) / d * len];
                let h = home[v];
                if (np[0] - h[0]).abs().max((np[1] - h[1]).abs()) > 0.9 * step {
                    continue;
                }
                if self.can_move(v, np, &incident[v], opaque, e) {
                    self.rest[v] = np;
                }
            }
        }
    }

    /// Whether moving vertex `v` to `np` keeps its triangles from
    /// collapsing and every opaque pixel's `e`-grown square that the mesh
    /// held still held (checked at its centre, corners and edge middles).
    fn can_move(&self, v: usize, np: Pt, inc: &[usize], opaque: &OpaqueMap, e: f64) -> bool {
        let p = self.rest[v];
        let tri = |t: usize, at: Pt| {
            self.tris[t].map(|w| {
                if w as usize == v {
                    at
                } else {
                    self.rest[w as usize]
                }
            })
        };
        let area = |t: [Pt; 3]| cross(sub(t[1], t[0]), sub(t[2], t[0]));
        let old: Vec<[Pt; 3]> = inc.iter().map(|&t| tri(t, p)).collect();
        let new: Vec<[Pt; 3]> = inc.iter().map(|&t| tri(t, np)).collect();
        if old.iter().zip(&new).any(|(o, n)| area(*n) < 0.3 * area(*o)) {
            return false;
        }
        let inside = |ts: &[[Pt; 3]], q: Pt| {
            ts.iter().any(|t| {
                barycentric(q, t[0], t[1], t[2]).is_some_and(|w| w.iter().all(|&x| x >= -INSIDE_EPS))
            })
        };
        // What the move takes away lies in the slivers between the old and
        // new spot and each neighbour.
        let mut slivers: Vec<[Pt; 3]> = Vec::new();
        for o in &old {
            for &w in o {
                if w != p && !slivers.iter().any(|s| s[2] == w) {
                    slivers.push([p, np, w]);
                }
            }
        }
        let offs = [-e, 0.5, 1.0 + e];
        for s in &slivers {
            let lo_x = s.iter().map(|q| q[0]).fold(f64::INFINITY, f64::min);
            let hi_x = s.iter().map(|q| q[0]).fold(f64::NEG_INFINITY, f64::max);
            let lo_y = s.iter().map(|q| q[1]).fold(f64::INFINITY, f64::min);
            let hi_y = s.iter().map(|q| q[1]).fold(f64::NEG_INFINITY, f64::max);
            let hit = opaque.any_in(
                ((lo_x - 1.0 - e).floor() as i32, (hi_x + e).ceil() as i32),
                ((lo_y - 1.0 - e).floor() as i32, (hi_y + e).ceil() as i32),
                |x, y| {
                    offs.iter().any(|&dy| {
                        offs.iter().any(|&dx| {
                            let q = [x as f64 + dx, y as f64 + dy];
                            q[0] >= lo_x
                                && q[0] <= hi_x
                                && q[1] >= lo_y
                                && q[1] <= hi_y
                                && inside(std::slice::from_ref(s), q)
                                && inside(&old, q)
                                && !inside(&new, q)
                        })
                    })
                },
            );
            if hit {
                return false;
            }
        }
        true
    }

    /// The mesh of the kept cells of a grid (row-major `cols × rows`).
    /// Cells on the mesh's edge keep only the triangles `needed` says
    /// hold something, choosing the diagonal that lets one go.
    fn from_cells(
        origin: (i32, i32),
        step: i32,
        cols: usize,
        rows: usize,
        kept: &[bool],
        needed: impl Fn([Pt; 3]) -> bool,
    ) -> PuppetMesh {
        let vcols = cols + 1;
        let gpos = |g: usize| {
            let (i, j) = ((g % vcols) as i32, (g / vcols) as i32);
            [(origin.0 + i * step) as f64, (origin.1 + j * step) as f64]
        };
        let is_kept = |i: isize, j: isize| {
            i >= 0
                && j >= 0
                && (i as usize) < cols
                && (j as usize) < rows
                && kept[j as usize * cols + i as usize]
        };
        // Triangles over grid vertex ids, cell by cell.
        let mut gtris: Vec<[usize; 3]> = Vec::new();
        let mut cell_tri = vec![(u32::MAX, 0u8); cols * rows];
        for j in 0..rows {
            for i in 0..cols {
                if !kept[j * cols + i] {
                    continue;
                }
                let g = |di: usize, dj: usize| (j + dj) * vcols + i + di;
                let (a, b, c, d) = (g(0, 0), g(1, 0), g(1, 1), g(0, 1));
                let diag_ac = [[a, b, c], [a, c, d]];
                let diag_bd = [[a, b, d], [b, c, d]];
                let even = (i + j) % 2 == 0;
                let (si, sj) = (i as isize, j as isize);
                let inner =
                    is_kept(si - 1, sj) && is_kept(si + 1, sj) && is_kept(si, sj - 1) && is_kept(si, sj + 1);
                let pick: Vec<[usize; 3]> = if inner {
                    (if even { diag_ac } else { diag_bd }).to_vec()
                } else {
                    let need = |tt: [[usize; 3]; 2]| tt.map(|t| needed(t.map(gpos)));
                    let (n_ac, n_bd) = (need(diag_ac), need(diag_bd));
                    let (drop_ac, drop_bd) = (!(n_ac[0] && n_ac[1]), !(n_bd[0] && n_bd[1]));
                    let use_ac = match (drop_ac, drop_bd) {
                        (true, false) => true,
                        (false, true) => false,
                        _ => even,
                    };
                    let (tt, nn) = if use_ac { (diag_ac, n_ac) } else { (diag_bd, n_bd) };
                    let keep: Vec<[usize; 3]> = tt
                        .into_iter()
                        .zip(nn)
                        .filter(|(_, n)| *n)
                        .map(|(t, _)| t)
                        .collect();
                    // A kept cell always holds something; never empty it.
                    if keep.is_empty() {
                        tt.to_vec()
                    } else {
                        keep
                    }
                };
                cell_tri[j * cols + i] = (gtris.len() as u32, pick.len() as u8);
                gtris.extend(pick);
            }
        }
        // Row-major numbering of the vertices in use keeps every triangle
        // within about one grid row of indices, which bounds the solver's
        // bandwidth.
        let mut id = vec![u32::MAX; vcols * (rows + 1)];
        for t in &gtris {
            for &g in t {
                id[g] = 0;
            }
        }
        let mut rest = Vec::new();
        for (g, slot) in id.iter_mut().enumerate() {
            if *slot == 0 {
                *slot = rest.len() as u32;
                rest.push(gpos(g));
            }
        }
        let tris = gtris.iter().map(|t| t.map(|g| id[g])).collect();
        PuppetMesh {
            rest,
            tris,
            origin,
            step,
            cols,
            rows,
            cell_tri,
        }
    }

    /// Grid cell size in pixels.
    pub fn step(&self) -> i32 {
        self.step
    }

    /// The area the rest mesh spans.
    pub fn rest_bounds(&self) -> Rect {
        Rect::new(
            self.origin.0,
            self.origin.1,
            (self.cols as i32 * self.step) as u32,
            (self.rows as i32 * self.step) as u32,
        )
    }

    /// Where rest point `p` lies in the mesh, if inside it.
    pub fn locate(&self, p: Pt) -> Option<MeshPoint> {
        let s = self.step as f64;
        if !(p[0].is_finite() && p[1].is_finite()) {
            return None;
        }
        let gi = ((p[0] - self.origin.0 as f64) / s).floor() as i64;
        let gj = ((p[1] - self.origin.1 as f64) / s).floor() as i64;
        // The cell holding `p` and its neighbours: outline vertices may sit
        // up to a cell inwards (shrink-wrap), and a point on a cell edge
        // belongs to either side.
        let mut best: Option<MeshPoint> = None;
        for j in gj - 1..=gj + 1 {
            for i in gi - 1..=gi + 1 {
                if i < 0 || j < 0 || i >= self.cols as i64 || j >= self.rows as i64 {
                    continue;
                }
                let (first, n) = self.cell_tri[j as usize * self.cols + i as usize];
                if first == u32::MAX {
                    continue;
                }
                for t in first..first + n as u32 {
                    let [a, b, c] = self.tris[t as usize].map(|v| self.rest[v as usize]);
                    let Some(w) = barycentric(p, a, b, c) else {
                        continue;
                    };
                    let worst = w[0].min(w[1]).min(w[2]);
                    if best.is_none_or(|bp| worst > bp.bary[0].min(bp.bary[1]).min(bp.bary[2])) {
                        best = Some(MeshPoint { tri: t, bary: w });
                    }
                }
            }
        }
        best.filter(|bp| bp.bary.iter().all(|&w| w >= -INSIDE_EPS))
    }

    /// Where `p` lies in the mesh deformed to `pos`, looking at the
    /// triangles from the top of `order` down (the one drawn on top wins).
    pub fn locate_deformed(&self, pos: &[Pt], order: &[u32], p: Pt) -> Option<MeshPoint> {
        order.iter().rev().find_map(|&t| {
            let [a, b, c] = self.tris[t as usize].map(|v| pos[v as usize]);
            let w = barycentric(p, a, b, c)?;
            w.iter()
                .all(|&x| x >= -INSIDE_EPS)
                .then_some(MeshPoint { tri: t, bary: w })
        })
    }

    /// `m`'s position in the mesh deformed to `pos`.
    pub fn point_in(&self, pos: &[Pt], m: MeshPoint) -> Pt {
        let t = self.tris[m.tri as usize];
        let mut out = [0.0; 2];
        for (k, &v) in t.iter().enumerate() {
            out[0] += m.bary[k] * pos[v as usize][0];
            out[1] += m.bary[k] * pos[v as usize][1];
        }
        out
    }

    /// Where rest point `p` lies, or for points off the mesh the nearest
    /// vertex (so a pin outside a thinner remesh still holds something).
    pub fn locate_or_nearest(&self, p: Pt) -> MeshPoint {
        if let Some(m) = self.locate(p) {
            return m;
        }
        let d2 = |q: Pt| (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2);
        let v = (0..self.rest.len())
            .min_by(|&a, &b| d2(self.rest[a]).total_cmp(&d2(self.rest[b])))
            .unwrap_or(0) as u32;
        let (tri, k) = self
            .tris
            .iter()
            .enumerate()
            .find_map(|(t, tv)| tv.iter().position(|&x| x == v).map(|k| (t as u32, k)))
            .unwrap_or((0, 0));
        let mut bary = [0.0; 3];
        bary[k] = 1.0;
        MeshPoint { tri, bary }
    }

    /// Connected parts: a component id per vertex, and how many there are.
    fn components(&self) -> (Vec<u32>, usize) {
        let n = self.rest.len();
        let mut parent: Vec<u32> = (0..n as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        for t in &self.tris {
            for k in 1..3 {
                let (a, b) = (find(&mut parent, t[0]), find(&mut parent, t[k]));
                if a != b {
                    parent[a.max(b) as usize] = a.min(b);
                }
            }
        }
        let mut label = vec![u32::MAX; n];
        let mut comp = vec![0u32; n];
        let mut count = 0;
        for v in 0..n as u32 {
            let r = find(&mut parent, v) as usize;
            if label[r] == u32::MAX {
                label[r] = count;
                count += 1;
            }
            comp[v as usize] = label[r];
        }
        (comp, count as usize)
    }

    /// Triangle draw order: by the depth of the nearest pin (rest
    /// distance), lower first; mesh order breaks ties.
    pub fn draw_order(&self, pins: &[PuppetPin]) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.tris.len() as u32).collect();
        if pins
            .iter()
            .all(|p| p.depth == pins.first().map_or(0, |f| f.depth))
        {
            return order;
        }
        let depth: Vec<i32> = self
            .tris
            .iter()
            .map(|t| {
                let c = t.iter().fold([0.0, 0.0], |acc, &v| {
                    let r = self.rest[v as usize];
                    [acc[0] + r[0] / 3.0, acc[1] + r[1] / 3.0]
                });
                pins.iter()
                    .min_by(|a, b| {
                        let da = (a.from[0] - c[0]).powi(2) + (a.from[1] - c[1]).powi(2);
                        let db = (b.from[0] - c[0]).powi(2) + (b.from[1] - c[1]).powi(2);
                        da.total_cmp(&db)
                    })
                    .map_or(0, |p| p.depth)
            })
            .collect();
        order.sort_by_key(|&t| depth[t as usize]);
        order
    }
}

/// A symmetric positive-definite band matrix and, after [`Band::factor`],
/// its Cholesky factor `L` (lower, same band).
#[derive(Clone, Debug)]
struct Band {
    n: usize,
    /// Half bandwidth: entry (i, j) is zero when |i − j| > b.
    b: usize,
    /// Row i holds columns i−b ..= i at `i*(b+1) + (i − j)`.
    data: Vec<f64>,
}

impl Band {
    fn new(n: usize, b: usize) -> Band {
        Band {
            n,
            b,
            data: vec![0.0; n * (b + 1)],
        }
    }

    #[inline]
    fn at(&self, i: usize, j: usize) -> f64 {
        self.data[i * (self.b + 1) + (i - j)]
    }

    /// Adds `v` at (i, j) when i ≥ j; the mirrored call fills the rest.
    #[inline]
    fn add_lower(&mut self, i: usize, j: usize, v: f64) {
        if i >= j {
            debug_assert!(i - j <= self.b);
            self.data[i * (self.b + 1) + (i - j)] += v;
        }
    }

    /// In-place banded Cholesky, O(n·b²). `false` if not positive definite.
    fn factor(&mut self) -> bool {
        let (n, b, w) = (self.n, self.b, self.b + 1);
        for j in 0..n {
            let k0 = j.saturating_sub(b);
            let mut s = self.data[j * w];
            for k in k0..j {
                let l = self.data[j * w + (j - k)];
                s -= l * l;
            }
            if s <= 0.0 || !s.is_finite() {
                return false;
            }
            let d = s.sqrt();
            self.data[j * w] = d;
            for i in j + 1..(j + b + 1).min(n) {
                let mut s = self.data[i * w + (i - j)];
                for k in i.saturating_sub(b)..j {
                    s -= self.data[i * w + (i - k)] * self.data[j * w + (j - k)];
                }
                self.data[i * w + (i - j)] = s / d;
            }
        }
        true
    }

    /// Solves `L Lᵀ x = rhs` in place.
    fn solve(&self, x: &mut [f64]) {
        let (n, b) = (self.n, self.b);
        for i in 0..n {
            let k0 = i.saturating_sub(b);
            let s: f64 = (k0..i).zip(&x[k0..i]).map(|(k, xk)| self.at(i, k) * xk).sum();
            x[i] = (x[i] - s) / self.at(i, i);
        }
        for i in (0..n).rev() {
            let k1 = (i + b + 1).min(n);
            let s: f64 = (i + 1..k1)
                .zip(&x[i + 1..k1])
                .map(|(k, xk)| self.at(k, i) * xk)
                .sum();
            x[i] = (x[i] - s) / self.at(i, i);
        }
    }
}

/// Penalty weight holding pins to their targets (the mesh terms are O(1)).
const PIN_WEIGHT: f64 = 1e5;
/// Tiny pull towards the baseline, so degenerate pin sets stay solvable.
const RIDGE: f64 = 1e-9;

/// The factored ARAP systems for one mesh and one set of pin positions.
/// Build it when pins are added or removed; [`PuppetSolver::solve`] is
/// cheap enough to run on every drag event.
#[derive(Clone, Debug)]
pub struct PuppetSolver {
    mesh: Arc<PuppetMesh>,
    pins: Vec<MeshPoint>,
    /// The rest point each pin holds (barycentric in the mesh).
    from: Vec<Pt>,
    comp: Vec<u32>,
    /// Pins per component.
    comp_pins: Vec<Vec<usize>>,
    /// Free (solved) index of each vertex, or `u32::MAX` when its part has
    /// fewer than two pins and simply follows them.
    free: Vec<u32>,
    /// Triangles whose vertices are free.
    free_tris: Vec<u32>,
    /// Step one, x/y interleaved (2 unknowns per free vertex).
    step1: Option<Band>,
    /// Step two, one coordinate at a time.
    step2: Option<Band>,
}

/// The local frame of triangle corner `k` relative to edge `i → j`:
/// `k − i = x (j − i) + y R(j − i)` with `R(v) = (v.y, −v.x)`.
fn local_frame(pi: Pt, pj: Pt, pk: Pt) -> (f64, f64) {
    let e = sub(pj, pi);
    let f = sub(pk, pi);
    let l2 = e[0] * e[0] + e[1] * e[1];
    let re = [e[1], -e[0]];
    (
        (f[0] * e[0] + f[1] * e[1]) / l2,
        (f[0] * re[0] + f[1] * re[1]) / l2,
    )
}

impl PuppetSolver {
    /// Factors the systems for pins holding the rest points `from`.
    pub fn new(mesh: Arc<PuppetMesh>, from: &[Pt]) -> PuppetSolver {
        let pins: Vec<MeshPoint> = from.iter().map(|&p| mesh.locate_or_nearest(p)).collect();
        let from: Vec<Pt> = pins.iter().map(|&m| mesh.point_in(&mesh.rest, m)).collect();
        let (comp, ncomp) = mesh.components();
        let mut comp_pins = vec![Vec::new(); ncomp];
        for (k, m) in pins.iter().enumerate() {
            let v = mesh.tris[m.tri as usize][0];
            comp_pins[comp[v as usize] as usize].push(k);
        }
        let mut free = vec![u32::MAX; mesh.rest.len()];
        let mut nfree = 0u32;
        for (v, f) in free.iter_mut().enumerate() {
            if comp_pins[comp[v] as usize].len() >= 2 {
                *f = nfree;
                nfree += 1;
            }
        }
        let free_tris: Vec<u32> = (0..mesh.tris.len() as u32)
            .filter(|&t| free[mesh.tris[t as usize][0] as usize] != u32::MAX)
            .collect();
        let mut solver = PuppetSolver {
            mesh,
            pins,
            from,
            comp,
            comp_pins,
            free,
            free_tris,
            step1: None,
            step2: None,
        };
        if nfree > 0 {
            solver.factor(nfree as usize);
        }
        solver
    }

    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    pub fn mesh(&self) -> &Arc<PuppetMesh> {
        &self.mesh
    }

    fn factor(&mut self, n: usize) {
        let mesh = &*self.mesh;
        let f = |v: u32| self.free[v as usize] as usize;
        let mut band = 0;
        for &t in &self.free_tris {
            let tv = mesh.tris[t as usize];
            for a in tv {
                for b in tv {
                    band = band.max(f(a).abs_diff(f(b)));
                }
            }
        }
        let mut g = Band::new(2 * n, 2 * band + 1);
        let mut l = Band::new(n, band);
        for &t in &self.free_tris {
            let tv = mesh.tris[t as usize];
            for r in 0..3 {
                let (i, j, k) = (tv[r], tv[(r + 1) % 3], tv[(r + 2) % 3]);
                let (x, y) = local_frame(
                    mesh.rest[i as usize],
                    mesh.rest[j as usize],
                    mesh.rest[k as usize],
                );
                // Residual of corner k (see `local_frame`), as two rows
                // over the dofs (ix, iy, jx, jy, kx, ky).
                let dofs = [
                    2 * f(i),
                    2 * f(i) + 1,
                    2 * f(j),
                    2 * f(j) + 1,
                    2 * f(k),
                    2 * f(k) + 1,
                ];
                let rows = [[-1.0 + x, y, -x, -y, 1.0, 0.0], [-y, -1.0 + x, y, -x, 0.0, 1.0]];
                for row in rows {
                    for (a, &da) in dofs.iter().enumerate() {
                        for (b, &db) in dofs.iter().enumerate() {
                            g.add_lower(da, db, row[a] * row[b]);
                        }
                    }
                }
                // Step two: the edge i → j.
                let (fi, fj) = (f(i), f(j));
                l.add_lower(fi, fi, 1.0);
                l.add_lower(fj, fj, 1.0);
                l.add_lower(fi.max(fj), fi.min(fj), -1.0);
            }
        }
        for m in &self.pins {
            let tv = mesh.tris[m.tri as usize];
            if self.free[tv[0] as usize] == u32::MAX {
                continue;
            }
            for (a, &va) in tv.iter().enumerate() {
                for (b, &vb) in tv.iter().enumerate() {
                    let w = PIN_WEIGHT * m.bary[a] * m.bary[b];
                    g.add_lower(2 * f(va), 2 * f(vb), w);
                    g.add_lower(2 * f(va) + 1, 2 * f(vb) + 1, w);
                    l.add_lower(f(va), f(vb), w);
                }
            }
        }
        for i in 0..2 * n {
            g.data[i * (g.b + 1)] += RIDGE;
        }
        for i in 0..n {
            l.data[i * (l.b + 1)] += RIDGE;
        }
        self.step1 = g.factor().then_some(g);
        self.step2 = l.factor().then_some(l);
    }

    /// The deformed vertex positions for pins at `to` (one per pin, in the
    /// order given to [`PuppetSolver::new`]).
    pub fn solve(&self, to: &[Pt], mode: PuppetMode) -> Vec<Pt> {
        let mesh = &*self.mesh;
        let mut out = mesh.rest.clone();
        // Per part: a similarity fitted to its pins, as a baseline (exact
        // for moves, turns and uniform scales of all pins); parts with one
        // pin just translate.
        let mut sim: Vec<Option<([f64; 2], Pt, Pt)>> = vec![None; self.comp_pins.len()];
        for (c, pins) in self.comp_pins.iter().enumerate() {
            match pins.len() {
                0 => {}
                1 => sim[c] = Some(([1.0, 0.0], self.from[pins[0]], to[pins[0]])),
                _ => sim[c] = Some(fit_similarity(pins.iter().map(|&k| (self.from[k], to[k])))),
            }
        }
        for (v, p) in out.iter_mut().enumerate() {
            if let Some((a, pc, qc)) = sim[self.comp[v] as usize] {
                let r = sub(mesh.rest[v], pc);
                *p = [
                    qc[0] + a[0] * r[0] - a[1] * r[1],
                    qc[1] + a[1] * r[0] + a[0] * r[1],
                ];
            }
        }
        let (Some(g), Some(l)) = (&self.step1, &self.step2) else {
            return out;
        };
        let f = |v: u32| self.free[v as usize] as usize;
        // Step one: (G + W BᵀB) δ = W Bᵀ (q − B v0), as G v0 = 0 for a
        // similarity. Residuals are taken as displacements, so pins that
        // did not move contribute exactly nothing.
        let mut rhs = vec![0.0; g.n];
        for (k, m) in self.pins.iter().enumerate() {
            let tv = mesh.tris[m.tri as usize];
            if self.free[tv[0] as usize] == u32::MAX {
                continue;
            }
            let mut res = sub(to[k], self.from[k]);
            for (a, &v) in tv.iter().enumerate() {
                let d = sub(out[v as usize], mesh.rest[v as usize]);
                res[0] -= m.bary[a] * d[0];
                res[1] -= m.bary[a] * d[1];
            }
            for (a, &v) in tv.iter().enumerate() {
                rhs[2 * f(v)] += PIN_WEIGHT * m.bary[a] * res[0];
                rhs[2 * f(v) + 1] += PIN_WEIGHT * m.bary[a] * res[1];
            }
        }
        g.solve(&mut rhs);
        for (v, p) in out.iter_mut().enumerate() {
            if self.free[v] != u32::MAX {
                let i = self.free[v] as usize;
                p[0] += rhs[2 * i];
                p[1] += rhs[2 * i + 1];
            }
        }
        // Step two: fit each triangle with a rotated (and, by mode,
        // partly scaled) copy of its rest shape, then match the edges.
        let keep = mode.scale_keep();
        let (mut bx, mut by) = (vec![0.0; l.n], vec![0.0; l.n]);
        for &t in &self.free_tris {
            let tv = mesh.tris[t as usize];
            let r = tv.map(|v| mesh.rest[v as usize]);
            let d = tv.map(|v| out[v as usize]);
            let fit = fit_triangle(r, d, keep);
            for e in 0..3 {
                let (a, b) = (e, (e + 1) % 3);
                let gx = (fit[a][0] - fit[b][0]) - (d[a][0] - d[b][0]);
                let gy = (fit[a][1] - fit[b][1]) - (d[a][1] - d[b][1]);
                let (fa, fb) = (f(tv[a]), f(tv[b]));
                bx[fa] += gx;
                by[fa] += gy;
                bx[fb] -= gx;
                by[fb] -= gy;
            }
        }
        for (k, m) in self.pins.iter().enumerate() {
            let tv = mesh.tris[m.tri as usize];
            if self.free[tv[0] as usize] == u32::MAX {
                continue;
            }
            let mut res = sub(to[k], self.from[k]);
            for (a, &v) in tv.iter().enumerate() {
                let d = sub(out[v as usize], mesh.rest[v as usize]);
                res[0] -= m.bary[a] * d[0];
                res[1] -= m.bary[a] * d[1];
            }
            for (a, &v) in tv.iter().enumerate() {
                bx[f(v)] += PIN_WEIGHT * m.bary[a] * res[0];
                by[f(v)] += PIN_WEIGHT * m.bary[a] * res[1];
            }
        }
        l.solve(&mut bx);
        l.solve(&mut by);
        for (v, p) in out.iter_mut().enumerate() {
            if self.free[v] != u32::MAX {
                let i = self.free[v] as usize;
                p[0] += bx[i];
                p[1] += by[i];
            }
        }
        out
    }
}

/// Least-squares similarity taking the `from` points onto the `to` points:
/// `(a, p̄, q̄)` with `x ↦ q̄ + a·(x − p̄)` in complex notation.
fn fit_similarity(pairs: impl Iterator<Item = (Pt, Pt)> + Clone) -> ([f64; 2], Pt, Pt) {
    let n = pairs.clone().count() as f64;
    let (mut pc, mut qc) = ([0.0; 2], [0.0; 2]);
    for (p, q) in pairs.clone() {
        pc = [pc[0] + p[0] / n, pc[1] + p[1] / n];
        qc = [qc[0] + q[0] / n, qc[1] + q[1] / n];
    }
    let (mut re, mut im, mut den) = (0.0, 0.0, 0.0);
    for (p, q) in pairs {
        let (r, s) = (sub(p, pc), sub(q, qc));
        // conj(r) · s
        re += r[0] * s[0] + r[1] * s[1];
        im += r[0] * s[1] - r[1] * s[0];
        den += r[0] * r[0] + r[1] * r[1];
    }
    if den < 1e-12 {
        return ([1.0, 0.0], pc, qc);
    }
    ([re / den, im / den], pc, qc)
}

/// The rest triangle `r` rotated (and scaled by `s^keep`, `s` being its
/// best uniform scale) onto the deformed triangle `d`, about `d`'s centroid.
fn fit_triangle(r: [Pt; 3], d: [Pt; 3], keep: f64) -> [Pt; 3] {
    let rc = [
        (r[0][0] + r[1][0] + r[2][0]) / 3.0,
        (r[0][1] + r[1][1] + r[2][1]) / 3.0,
    ];
    let dc = [
        (d[0][0] + d[1][0] + d[2][0]) / 3.0,
        (d[0][1] + d[1][1] + d[2][1]) / 3.0,
    ];
    let (mut re, mut im, mut den) = (0.0, 0.0, 0.0);
    for k in 0..3 {
        let (a, b) = (sub(r[k], rc), sub(d[k], dc));
        re += a[0] * b[0] + a[1] * b[1];
        im += a[0] * b[1] - a[1] * b[0];
        den += a[0] * a[0] + a[1] * a[1];
    }
    let mag = (re * re + im * im).sqrt();
    let (c, s) = if mag > 1e-300 {
        (re / mag, im / mag)
    } else {
        (1.0, 0.0)
    };
    let scale = if den > 0.0 { (mag / den).powf(keep) } else { 1.0 };
    let (c, s) = (c * scale, s * scale);
    r.map(|p| {
        let a = sub(p, rc);
        [dc[0] + c * a[0] - s * a[1], dc[1] + s * a[0] + c * a[1]]
    })
}

/// One triangle ready to rasterise.
struct DrawTri {
    v: [u32; 3],
    area2: f64,
    /// Pixel index range whose centres may fall inside (inclusive).
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

/// Edge function of directed edge `u → v` at `p`, evaluated the same way
/// (bit for bit) from either direction, so a pixel centre on a shared
/// edge gets exactly opposite signs in the two triangles.
#[inline]
fn edge(pu: Pt, pv: Pt, u: u32, v: u32, p: Pt) -> f64 {
    if u < v {
        cross(sub(pv, pu), sub(p, pu))
    } else {
        -cross(sub(pu, pv), sub(p, pv))
    }
}

/// Draws mesh triangles from `src` (positions `from`) to their deformed
/// positions `to`. All coordinates share one pixel space; `src`'s pixel
/// (0, 0) sits at `src_org`.
pub struct MeshPainter<'a> {
    src: &'a Raster,
    src_org: (i32, i32),
    from: &'a [Pt],
    to: &'a [Pt],
    tris: Vec<DrawTri>,
}

impl<'a> MeshPainter<'a> {
    pub fn new(
        src: &'a Raster,
        src_org: (i32, i32),
        from: &'a [Pt],
        to: &'a [Pt],
        mesh_tris: &[[u32; 3]],
        order: &[u32],
    ) -> MeshPainter<'a> {
        let tris = order
            .iter()
            .filter_map(|&t| {
                let v = mesh_tris[t as usize];
                let [a, b, c] = v.map(|i| to[i as usize]);
                let area2 = cross(sub(b, a), sub(c, a));
                if area2.abs() < 1e-12 || !area2.is_finite() {
                    return None;
                }
                let (lo_x, hi_x) = (a[0].min(b[0]).min(c[0]), a[0].max(b[0]).max(c[0]));
                let (lo_y, hi_y) = (a[1].min(b[1]).min(c[1]), a[1].max(b[1]).max(c[1]));
                Some(DrawTri {
                    v,
                    area2,
                    x0: (lo_x - 0.5).ceil() as i32,
                    x1: (hi_x - 0.5).floor() as i32,
                    y0: (lo_y - 0.5).ceil() as i32,
                    y1: (hi_y - 0.5).floor() as i32,
                })
            })
            .collect();
        MeshPainter {
            src,
            src_org,
            from,
            to,
            tris,
        }
    }

    /// The pixels any triangle may touch, or `None` if there are none.
    pub fn bounds(&self) -> Option<Rect> {
        let x0 = self.tris.iter().map(|t| t.x0).min()?;
        let y0 = self.tris.iter().map(|t| t.y0).min()?;
        let x1 = self.tris.iter().map(|t| t.x1).max()?;
        let y1 = self.tris.iter().map(|t| t.y1).max()?;
        (x1 >= x0 && y1 >= y0).then(|| Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32))
    }

    /// Bilinear source sample at continuous point `p` (pixel centres at
    /// `+0.5`); transparent off the source.
    #[inline]
    fn sample(&self, p: Pt) -> Rgba {
        let x = p[0] - 0.5 - self.src_org.0 as f64;
        let y = p[1] - 0.5 - self.src_org.1 as f64;
        let (fx0, fy0) = (x.floor(), y.floor());
        let (fx, fy) = ((x - fx0) as f32, (y - fy0) as f32);
        let (ix, iy) = (fx0 as i64, fy0 as i64);
        let (w, h) = (self.src.width as i64, self.src.height as i64);
        let get = |x: i64, y: i64| {
            if x < 0 || y < 0 || x >= w || y >= h {
                Rgba::TRANSPARENT
            } else {
                self.src.pixels[(y * w + x) as usize]
            }
        };
        let p00 = get(ix, iy);
        if fx == 0.0 && fy == 0.0 {
            return p00;
        }
        let (p10, p01, p11) = (get(ix + 1, iy), get(ix, iy + 1), get(ix + 1, iy + 1));
        let (w00, w10, w01, w11) = ((1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy);
        Rgba::new(
            p00.r * w00 + p10.r * w10 + p01.r * w01 + p11.r * w11,
            p00.g * w00 + p10.g * w10 + p01.g * w01 + p11.g * w11,
            p00.b * w00 + p10.b * w10 + p01.b * w01 + p11.b * w11,
            p00.a * w00 + p10.a * w10 + p01.a * w01 + p11.a * w11,
        )
    }

    /// Paints every triangle into `out`, a window onto pixel area `win`
    /// with row stride `stride`, compositing over what is there. Returns
    /// whether anything was painted.
    pub fn paint(&self, out: &mut [Rgba], stride: usize, win: Rect) -> bool {
        let mut any = false;
        for t in &self.tris {
            let (x0, x1) = (t.x0.max(win.x), t.x1.min(win.right() - 1));
            let (y0, y1) = (t.y0.max(win.y), t.y1.min(win.bottom() - 1));
            if x0 > x1 || y0 > y1 {
                continue;
            }
            let [ia, ib, ic] = t.v;
            let [a, b, c] = t.v.map(|i| self.to[i as usize]);
            let [sa, sb, sc] = t.v.map(|i| self.from[i as usize]);
            let pos = t.area2 > 0.0;
            let sign = if pos { 1.0 } else { -1.0 };
            // A zero edge value counts as inside for exactly one of the
            // two triangles sharing that edge.
            let tie = |u: u32, v: u32| (u < v) == pos;
            let (tab, tbc, tca) = (tie(ia, ib), tie(ib, ic), tie(ic, ia));
            for y in y0..=y1 {
                let py = y as f64 + 0.5;
                let row = (y - win.y) as usize * stride;
                for x in x0..=x1 {
                    let p = [x as f64 + 0.5, py];
                    let e_ab = edge(a, b, ia, ib, p);
                    let s_ab = e_ab * sign;
                    if s_ab < 0.0 || (s_ab == 0.0 && !tab) {
                        continue;
                    }
                    let e_bc = edge(b, c, ib, ic, p);
                    let s_bc = e_bc * sign;
                    if s_bc < 0.0 || (s_bc == 0.0 && !tbc) {
                        continue;
                    }
                    let e_ca = edge(c, a, ic, ia, p);
                    let s_ca = e_ca * sign;
                    if s_ca < 0.0 || (s_ca == 0.0 && !tca) {
                        continue;
                    }
                    let (la, lb) = (e_bc / t.area2, e_ca / t.area2);
                    let lc = 1.0 - la - lb;
                    let q = [
                        la * sa[0] + lb * sb[0] + lc * sc[0],
                        la * sa[1] + lb * sb[1] + lc * sc[1],
                    ];
                    let s = self.sample(q);
                    if s.a <= 0.0 {
                        continue;
                    }
                    let d = &mut out[row + (x - win.x) as usize];
                    *d = s.over(*d);
                    any = true;
                }
            }
        }
        any
    }

    /// Paints into a dense raster whose pixel (0, 0) sits at `org`, in
    /// parallel row bands.
    pub fn paint_raster(&self, out: &mut Raster, org: (i32, i32)) {
        const BAND: usize = 32;
        let w = out.width as usize;
        if w == 0 {
            return;
        }
        out.pixels
            .par_chunks_mut(w * BAND)
            .enumerate()
            .for_each(|(k, chunk)| {
                let rows = chunk.len() / w;
                let win = Rect::new(org.0, org.1 + (k * BAND) as i32, w as u32, rows as u32);
                self.paint(chunk, w, win);
            });
    }

    /// Paints into fresh tiles over `clip`, in parallel.
    pub fn paint_store(&self, clip: Rect) -> TileStore {
        let mut out = TileStore::new();
        let Some(b) = self.bounds().map(|b| b.intersect(&clip)) else {
            return out;
        };
        if b.is_empty() {
            return out;
        }
        let tiles: Vec<_> = b
            .tiles()
            .into_par_iter()
            .filter_map(|c| {
                let win = c.rect().intersect(&b);
                let mut tile = Tile::new();
                let (ox, oy) = c.origin();
                let px = tile.pixels_mut();
                let off = (win.y - oy) as usize * TILE_SIZE + (win.x - ox) as usize;
                self.paint(&mut px[off..], TILE_SIZE, win).then_some((c, tile))
            })
            .collect();
        for (c, t) in tiles {
            out.insert(c, Arc::new(t));
        }
        out.prune_blank();
        out
    }
}

/// Warps a whole tile store through the mesh: rest positions → `to`,
/// triangles in `order`; output limited to `clip`.
pub fn puppet_store(src: &TileStore, mesh: &PuppetMesh, to: &[Pt], order: &[u32], clip: Rect) -> TileStore {
    let rb = mesh.rest_bounds();
    let read = Rect::new(rb.x - 1, rb.y - 1, rb.w + 2, rb.h + 2);
    let raster = src.to_raster(read);
    MeshPainter::new(&raster, (read.x, read.y), &mesh.rest, to, &mesh.tris, order).paint_store(clip)
}

/// Warps a mask's coverage through the same mesh as its layer (the
/// complement trick of `transform_mask` keeps a reveal-all default right).
pub fn puppet_mask(
    mask: &lumenply_doc::Mask,
    mesh: &PuppetMesh,
    to: &[Pt],
    order: &[u32],
    clip: Rect,
) -> lumenply_doc::Mask {
    let warp = |s: &TileStore| puppet_store(s, mesh, to, order, clip);
    let tiles = if mask.default == 0.0 {
        warp(&mask.tiles)
    } else {
        complement(&warp(&complement(&mask.tiles)))
    };
    let mut out = lumenply_doc::Mask {
        tiles,
        default: mask.default,
        enabled: mask.enabled,
    };
    out.prune_uniform();
    out
}

fn complement(src: &TileStore) -> TileStore {
    let mut out = TileStore::new();
    for c in src.coords() {
        let Some(tile) = src.tile(c) else { continue };
        let px = tile.pixels();
        let mut nt = Tile::new();
        for (p, s) in nt.pixels_mut().iter_mut().zip(px.iter()) {
            let v = 1.0 - s.a;
            *p = Rgba::new(v, v, v, v);
        }
        out.insert(c, Arc::new(nt));
    }
    out
}

/// The bounding box of deformed positions, in whole pixels.
pub fn points_bounds(pts: &[Pt]) -> Option<Rect> {
    let x0 = pts.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let y0 = pts.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let x1 = pts.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
    let y1 = pts.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return None;
    }
    let (x0, y0) = (x0.floor() as i32, y0.floor() as i32);
    let (x1, y1) = (x1.ceil() as i32, y1.ceil() as i32);
    Some(Rect::new(
        x0,
        y0,
        (x1 - x0).max(1) as u32,
        (y1 - y0).max(1) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disc of radius `r` at `(cx, cy)`, antialiased, with a colour
    /// gradient so any misplaced sample shows.
    fn disc(cx: f32, cy: f32, r: f32) -> TileStore {
        let size = (cx + r + 4.0) as u32;
        let mut img = Raster::new(size, (cy + r + 4.0) as u32);
        for y in 0..img.height {
            for x in 0..img.width {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a = (r - d + 0.5).clamp(0.0, 1.0);
                if a > 0.0 {
                    let (u, v) = (x as f32 / size as f32, y as f32 / img.height as f32);
                    img.set(x, y, Rgba::from_straight(u, v, 1.0 - u, a));
                }
            }
        }
        TileStore::from_raster(&img, 0, 0)
    }

    /// A `w × h` bar with its top-left at `(x, y)`, colour varying along it.
    fn bar(x: i32, y: i32, w: u32, h: u32) -> TileStore {
        let mut img = Raster::new(w, h);
        for j in 0..h {
            for i in 0..w {
                let u = i as f32 / w as f32;
                img.set(i, j, Rgba::from_straight(u, 1.0 - u, (j % 4) as f32 / 4.0, 1.0));
            }
        }
        TileStore::from_raster(&img, x, y)
    }

    fn worst_diff(a: &TileStore, b: &TileStore, r: Rect) -> f32 {
        let (ra, rb) = (a.to_raster(r), b.to_raster(r));
        ra.pixels.iter().zip(&rb.pixels).fold(0.0f32, |m, (p, q)| {
            m.max((p.r - q.r).abs())
                .max((p.g - q.g).abs())
                .max((p.b - q.b).abs())
                .max((p.a - q.a).abs())
        })
    }

    fn solve(store: &TileStore, pins: &[PuppetPin], mode: PuppetMode) -> (Arc<PuppetMesh>, Vec<Pt>) {
        let mesh = Arc::new(PuppetMesh::build(store, MeshParams::default()).unwrap());
        let from: Vec<Pt> = pins.iter().map(|p| p.from).collect();
        let to: Vec<Pt> = pins.iter().map(|p| p.to).collect();
        let solver = PuppetSolver::new(mesh.clone(), &from);
        let pos = solver.solve(&to, mode);
        (mesh, pos)
    }

    const BIG: Rect = Rect::new(-1000, -1000, 4000, 4000);

    #[test]
    fn banded_cholesky_solves_a_known_system() {
        // [[4,1,0],[1,4,1],[0,1,4]] x = [1,2,3]  →  x = (5/28, 2/7, 19/28).
        let mut m = Band::new(3, 1);
        for i in 0..3 {
            m.add_lower(i, i, 4.0);
        }
        m.add_lower(1, 0, 1.0);
        m.add_lower(2, 1, 1.0);
        assert!(m.factor());
        let mut x = vec![1.0, 2.0, 3.0];
        m.solve(&mut x);
        for (got, want) in x.iter().zip([5.0 / 28.0, 2.0 / 7.0, 19.0 / 28.0]) {
            assert!((got - want).abs() < 1e-12, "{x:?}");
        }
        // Not positive definite: refused.
        let mut bad = Band::new(2, 1);
        bad.add_lower(0, 0, 1.0);
        bad.add_lower(1, 1, 1.0);
        bad.add_lower(1, 0, 2.0);
        assert!(!bad.factor());
    }

    #[test]
    fn the_mesh_covers_every_opaque_pixel_and_the_expansion() {
        let store = disc(60.0, 50.0, 30.3);
        for e in [0.0, 2.0, 9.0] {
            let mesh = PuppetMesh::build(
                &store,
                MeshParams {
                    density: PuppetDensity::More,
                    expansion: e,
                },
            )
            .unwrap();
            let b = store.content_bounds().unwrap();
            let mut covered = 0;
            for y in b.y..b.bottom() {
                for x in b.x..b.right() {
                    if store.get_pixel(x, y).a > COVER_ALPHA {
                        let c = [x as f64 + 0.5, y as f64 + 0.5];
                        assert!(mesh.locate(c).is_some(), "pixel ({x}, {y}) off the mesh at e={e}");
                        // The expansion margin is meshed too.
                        let out = [c[0] + e as f64 * 0.99, c[1]];
                        assert!(mesh.locate(out).is_some(), "margin at ({x}, {y}) e={e}");
                        covered += 1;
                    }
                }
            }
            assert!(covered > 2800, "{covered}");
            // Far corners of the bounds (outside the disc) are not meshed.
            assert!(mesh.locate([b.x as f64 + 0.5, b.y as f64 + 0.5]).is_none() || e > 5.0);
        }
        // Every triangle has positive area.
        let mesh = PuppetMesh::build(&store, MeshParams::default()).unwrap();
        for t in &mesh.tris {
            let [a, b, c] = t.map(|v| mesh.rest[v as usize]);
            assert!(cross(sub(b, a), sub(c, a)) > 0.0);
        }
        // Normal density on a 62 px disc + 2·2 px margin: 66/24 → 3 px,
        // raised to the 4 px minimum.
        assert_eq!(mesh.step(), 4);
        assert!(PuppetMesh::build(&TileStore::new(), MeshParams::default()).is_none());
    }

    #[test]
    fn the_outline_is_pulled_in_to_the_shape() {
        // A 200 px disc, 9 px cells, 2 px expansion: outline vertices end
        // e + ½ = 2.5 px from the disc wherever the move is safe.
        let store = disc(110.0, 110.0, 100.0);
        let mesh = PuppetMesh::build(&store, MeshParams::default()).unwrap();
        let b = store.content_bounds().unwrap();
        let opaque = OpaqueMap::new(&store, b);
        let mut edges = std::collections::HashMap::new();
        for t in &mesh.tris {
            for k in 0..3 {
                let (u, v) = (t[k].min(t[(k + 1) % 3]), t[k].max(t[(k + 1) % 3]));
                *edges.entry((u, v)).or_insert(0) += 1;
            }
        }
        let mut outline: Vec<u32> = edges
            .iter()
            .filter(|e| *e.1 == 1)
            .flat_map(|e| [e.0 .0, e.0 .1])
            .collect();
        outline.sort_unstable();
        outline.dedup();
        let dist: Vec<f64> = outline
            .iter()
            .map(|&v| opaque.nearest(mesh.rest[v as usize], 40.0).map_or(40.0, |d| d.0))
            .collect();
        let snug = dist.iter().filter(|d| (**d - 2.5).abs() < 1e-6).count();
        let mean = dist.iter().sum::<f64>() / dist.len() as f64;
        assert_eq!((outline.len(), snug), (72, 22));
        // Measured: 4.95 px on average, against 5.67 px on the bare grid.
        assert!((mean - 4.946).abs() < 0.01, "{mean}");
    }

    #[test]
    fn edge_cells_drop_their_empty_triangle() {
        // A 200 px disc on a 24-cell grid (9 px cells): the round outline
        // cuts many edge cells, which keep one triangle where the other
        // holds nothing, so the mesh follows the outline in 45° steps.
        let store = disc(110.0, 110.0, 100.0);
        let mesh = PuppetMesh::build(&store, MeshParams::default()).unwrap();
        let cells = mesh.cell_tri.iter().filter(|c| c.0 != u32::MAX).count();
        let halves = mesh.cell_tri.iter().filter(|c| c.1 == 1).count();
        assert_eq!(mesh.step(), 9);
        assert_eq!((cells, halves), (449, 20));
        assert_eq!(mesh.tris.len(), 2 * cells - halves);
        // A lone square of pixels has no slanted edge to follow.
        let square = TileStore::from_raster(&Raster::filled(40, 40, Rgba::WHITE), 0, 0);
        let m = PuppetMesh::build(
            &square,
            MeshParams {
                density: PuppetDensity::Normal,
                expansion: 0.0,
            },
        )
        .unwrap();
        assert_eq!((m.step(), m.tris.len(), m.rest.len()), (4, 200, 121));
    }

    #[test]
    fn pins_left_in_place_reproduce_the_layer_exactly() {
        let store = disc(70.0, 64.0, 40.0);
        let pins: Vec<PuppetPin> = [[50.0, 60.0], [95.5, 70.25], [70.0, 40.0]]
            .into_iter()
            .map(PuppetPin::at)
            .collect();
        for mode in PuppetMode::ALL {
            let (mesh, pos) = solve(&store, &pins, mode);
            let worst = pos
                .iter()
                .zip(&mesh.rest)
                .map(|(p, r)| (p[0] - r[0]).abs().max((p[1] - r[1]).abs()))
                .fold(0.0, f64::max);
            assert!(worst < 1e-9, "{mode:?}: vertices moved by {worst}");
            let order = mesh.draw_order(&pins);
            let out = puppet_store(&store, &mesh, &pos, &order, BIG);
            let w = worst_diff(&out, &store, Rect::new(0, 0, 120, 110));
            assert!(w < 1e-4, "{mode:?}: pixels differ by {w}");
        }
    }

    #[test]
    fn moving_every_pin_by_the_same_offset_translates_exactly() {
        let store = disc(70.0, 64.0, 40.0);
        let (dx, dy) = (13.0, -7.0);
        let pins: Vec<PuppetPin> = [[50.0, 60.0], [95.5, 70.25], [70.0, 40.0], [72.0, 90.0]]
            .into_iter()
            .map(|p| PuppetPin {
                from: p,
                to: [p[0] + dx, p[1] + dy],
                depth: 0,
            })
            .collect();
        let (mesh, pos) = solve(&store, &pins, PuppetMode::Rigid);
        for (p, r) in pos.iter().zip(&mesh.rest) {
            assert!((p[0] - r[0] - dx).abs() < 1e-7 && (p[1] - r[1] - dy).abs() < 1e-7);
        }
        let out = puppet_store(&store, &mesh, &pos, &mesh.draw_order(&pins), BIG);
        let moved = store.translated(13, -7);
        let w = worst_diff(&out, &moved, Rect::new(0, -20, 140, 130));
        assert!(w < 1e-4, "translation differs by {w}");
    }

    #[test]
    fn a_single_pin_drags_its_part_along() {
        let store = disc(40.0, 40.0, 20.0);
        let pins = [PuppetPin {
            from: [40.0, 40.0],
            to: [45.0, 38.0],
            depth: 0,
        }];
        let (mesh, pos) = solve(&store, &pins, PuppetMode::Normal);
        for (p, r) in pos.iter().zip(&mesh.rest) {
            assert_eq!(*p, [r[0] + 5.0, r[1] - 2.0]);
        }
    }

    /// Rest and deformed lengths of every mesh edge: the worst ratio.
    fn worst_stretch(mesh: &PuppetMesh, pos: &[Pt]) -> f64 {
        let len = |a: Pt, b: Pt| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let mut worst: f64 = 0.0;
        for t in &mesh.tris {
            for e in 0..3 {
                let (u, v) = (t[e] as usize, t[(e + 1) % 3] as usize);
                let r = len(mesh.rest[u], mesh.rest[v]);
                worst = worst.max((len(pos[u], pos[v]) / r - 1.0).abs());
            }
        }
        worst
    }

    /// Mean |deformed area / rest area − 1| over the triangles.
    fn area_change(mesh: &PuppetMesh, pos: &[Pt]) -> f64 {
        let area = |p: [Pt; 3]| cross(sub(p[1], p[0]), sub(p[2], p[0]));
        let sum: f64 = mesh
            .tris
            .iter()
            .map(|t| {
                let r = area(t.map(|v| mesh.rest[v as usize]));
                (area(t.map(|v| pos[v as usize])) / r - 1.0).abs()
            })
            .sum();
        sum / mesh.tris.len() as f64
    }

    #[test]
    fn two_pins_turn_a_bar_rigidly() {
        // A 200 × 24 bar centred on (150, 100), pinned near both ends; the
        // pins turn 30° about the centre.
        let store = bar(50, 88, 200, 24);
        let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
        let turn = |p: Pt| {
            let (x, y) = (p[0] - 150.0, p[1] - 100.0);
            [150.0 + c * x - s * y, 100.0 + s * x + c * y]
        };
        let pins: Vec<PuppetPin> = [[60.0, 100.0], [240.0, 100.0]]
            .into_iter()
            .map(|p| PuppetPin {
                from: p,
                to: turn(p),
                depth: 0,
            })
            .collect();
        for mode in PuppetMode::ALL {
            let (mesh, pos) = solve(&store, &pins, mode);
            let stretch = worst_stretch(&mesh, &pos);
            assert!(stretch < 0.02, "{mode:?}: an edge changed length by {stretch}");
            // Every vertex lands where the rigid turn puts it.
            for (p, r) in pos.iter().zip(&mesh.rest) {
                let q = turn(*r);
                assert!((p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3);
            }
            // And so does the bar's colour: the far end's pixel follows.
            let out = puppet_store(&store, &mesh, &pos, &mesh.draw_order(&pins), BIG);
            let end = turn([245.5, 100.5]);
            let got = out.get_pixel(end[0].floor() as i32, end[1].floor() as i32);
            let want = store.get_pixel(245, 100);
            assert!(
                (got.r - want.r).abs() < 0.03 && got.a > 0.99,
                "{got:?} vs {want:?}"
            );
        }
    }

    #[test]
    fn rigid_bends_keep_lengths_better_than_distort() {
        // Ends held, the middle pushed down 30 px: the bar must bend.
        let store = bar(50, 88, 200, 24);
        let pins = [
            PuppetPin::at([55.0, 100.0]),
            PuppetPin::at([245.0, 100.0]),
            PuppetPin {
                from: [150.0, 100.0],
                to: [150.0, 130.0],
                depth: 0,
            },
        ];
        let (mesh, rigid) = solve(&store, &pins, PuppetMode::Rigid);
        let (_, normal) = solve(&store, &pins, PuppetMode::Normal);
        let (_, distort) = solve(&store, &pins, PuppetMode::Distort);
        let (r, n, d) = (
            area_change(&mesh, &rigid),
            area_change(&mesh, &normal),
            area_change(&mesh, &distort),
        );
        // Measured: rigid 0.069, normal 0.136, distort 0.211.
        assert!(r < n && n < d, "rigid {r} normal {n} distort {d}");
        assert!(r < 0.08 && d > 0.18, "rigid {r} distort {d}");
        // The pins hold: the middle pin's point sits at its target.
        let solver = PuppetSolver::new(mesh.clone(), &pins.map(|p| p.from));
        let pos = solver.solve(&pins.map(|p| p.to), PuppetMode::Normal);
        let m = mesh.locate([150.0, 100.0]).unwrap();
        let at = mesh.point_in(&pos, m);
        assert!(
            (at[0] - 150.0).abs() < 0.01 && (at[1] - 130.0).abs() < 0.01,
            "{at:?}"
        );
        // Points between the pins sag between 0 and 30 px.
        let q = mesh.point_in(&pos, mesh.locate([100.0, 100.0]).unwrap());
        assert!(q[1] > 105.0 && q[1] < 130.0, "{q:?}");
    }

    #[test]
    fn deeper_pins_draw_their_part_on_top() {
        // A red bar and a blue bar 40 px apart; the blue one's mesh is slid
        // 100 px left onto the red one. Where they overlap, the part
        // nearer the deeper pin shows.
        let mut img = Raster::new(160, 16);
        for y in 0..16 {
            for x in 0..60 {
                img.set(x, y, Rgba::new(1.0, 0.0, 0.0, 1.0));
                img.set(x + 100, y, Rgba::new(0.0, 0.0, 1.0, 1.0));
            }
        }
        let store = TileStore::from_raster(&img, 0, 0);
        let mesh = PuppetMesh::build(&store, MeshParams::default()).unwrap();
        assert_eq!(mesh.components().1, 2);
        let pos: Vec<Pt> = mesh
            .rest
            .iter()
            .map(|r| if r[0] > 80.0 { [r[0] - 100.0, r[1]] } else { *r })
            .collect();
        let left = PuppetPin::at([20.0, 8.0]);
        let right = PuppetPin::at([140.0, 8.0]);
        let paint = |pins: &[PuppetPin]| {
            puppet_store(&store, &mesh, &pos, &mesh.draw_order(pins), BIG).get_pixel(30, 8)
        };
        let blue = paint(&[PuppetPin { depth: 0, ..left }, PuppetPin { depth: 1, ..right }]);
        assert_eq!(blue, Rgba::new(0.0, 0.0, 1.0, 1.0));
        let red = paint(&[PuppetPin { depth: 1, ..left }, PuppetPin { depth: 0, ..right }]);
        assert_eq!(red, Rgba::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn shared_edges_are_painted_once() {
        // A uniformly half-transparent square through a sheared mesh: a
        // pixel painted by two triangles would come out denser.
        let mut img = Raster::new(64, 64);
        for p in &mut img.pixels {
            *p = Rgba::new(0.25, 0.25, 0.25, 0.5);
        }
        let store = TileStore::from_raster(&img, 0, 0);
        let mesh = PuppetMesh::build(&store, MeshParams::default()).unwrap();
        let pos: Vec<Pt> = mesh.rest.iter().map(|r| [r[0] + 0.25 * r[1], r[1]]).collect();
        let out = puppet_store(&store, &mesh, &pos, &mesh.draw_order(&[]), BIG);
        for y in 4..60 {
            for x in 20..60 {
                let a = out.get_pixel(x, y).a;
                assert!(a <= 0.5 + 1e-5, "({x}, {y}) alpha {a}");
            }
        }
    }

    #[test]
    fn pins_placed_on_a_warped_mesh_find_their_rest_point() {
        let store = bar(50, 88, 200, 24);
        let pins = [
            PuppetPin::at([60.0, 100.0]),
            PuppetPin {
                from: [240.0, 100.0],
                to: [230.0, 140.0],
                depth: 0,
            },
        ];
        let (mesh, pos) = solve(&store, &pins, PuppetMode::Rigid);
        let order = mesh.draw_order(&pins);
        let rest = [200.0, 95.0];
        let shown = mesh.point_in(&pos, mesh.locate(rest).unwrap());
        let found = mesh.locate_deformed(&pos, &order, shown).unwrap();
        let back = mesh.point_in(&mesh.rest, found);
        assert!(
            (back[0] - rest[0]).abs() < 1e-6 && (back[1] - rest[1]).abs() < 1e-6,
            "{back:?}"
        );
        assert!(mesh.locate_deformed(&pos, &order, [0.0, 0.0]).is_none());
    }

    #[test]
    fn masks_follow_the_layer() {
        let store = bar(50, 88, 200, 24);
        let pins: Vec<PuppetPin> = [[60.0, 100.0], [240.0, 100.0]]
            .into_iter()
            .map(|p| PuppetPin {
                from: p,
                to: [p[0] + 10.0, p[1] + 20.0],
                depth: 0,
            })
            .collect();
        let (mesh, pos) = solve(&store, &pins, PuppetMode::Normal);
        let order = mesh.draw_order(&pins);
        let mut m = lumenply_doc::Mask::reveal_all();
        m.fill_rect(Rect::new(100, 90, 20, 10), 0.0);
        let w = puppet_mask(&m, &mesh, &pos, &order, BIG);
        assert_eq!(w.default, 1.0);
        assert!(w.value(115, 115) < 1e-4, "hidden rect moved with the layer");
        assert!((w.value(105, 95) - 1.0).abs() < 1e-4, "its old place shows again");
        assert_eq!(w.value(3000, 3000), 1.0);
    }

    #[test]
    #[ignore = "timing; cargo test --release -p lumenply-render puppet_timing -- --ignored --nocapture"]
    fn puppet_timing() {
        let (w, h) = (1800u32, 1205u32);
        let mut img = Raster::new(w, h);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let v = (i % 251) as f32 / 251.0;
            *p = Rgba::new(v, 0.5, 1.0 - v, 1.0);
        }
        let store = TileStore::from_raster(&img, 0, 0);
        for density in PuppetDensity::ALL {
            let t = std::time::Instant::now();
            let mesh = Arc::new(
                PuppetMesh::build(
                    &store,
                    MeshParams {
                        density,
                        expansion: 2.0,
                    },
                )
                .unwrap(),
            );
            let build = t.elapsed();
            let from = [[200.0, 200.0], [1600.0, 200.0], [900.0, 1000.0], [300.0, 1000.0]];
            let t = std::time::Instant::now();
            let solver = PuppetSolver::new(mesh.clone(), &from);
            let factor = t.elapsed();
            let to = [[220.0, 180.0], [1600.0, 260.0], [800.0, 1100.0], [300.0, 1000.0]];
            let t = std::time::Instant::now();
            let n = 20;
            let mut pos = Vec::new();
            for _ in 0..n {
                pos = solver.solve(&to, PuppetMode::Normal);
            }
            let solve = t.elapsed() / n;
            let t = std::time::Instant::now();
            let out = puppet_store(&store, &mesh, &pos, &mesh.draw_order(&[]), BIG);
            let bake = t.elapsed();
            println!(
                "{density:?}: {} vertices, build {build:?}, factor {factor:?}, solve {solve:?}, bake {bake:?} ({} tiles)",
                mesh.rest.len(),
                out.len()
            );
        }
    }
}
