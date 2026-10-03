//! Sparse, copy-on-write tile storage for raster layers.
//!
//! A layer's pixels live in 256×256 [`Tile`]s addressed by [`TileCoord`].
//! Tiles are shared behind `Arc`, so cloning a [`TileStore`] (or a whole
//! document) is cheap: only the map of pointers is copied. Writing to a
//! tile that is shared copies that one tile first (`Arc::make_mut`). This is
//! what makes snapshot-based undo affordable on very large images.
//!
//! Pixels are stored as premultiplied, linear-light `f32` RGBA. Narrower
//! storage formats (8- and 16-bit) will be added behind the same API later.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Edge length of a tile in pixels.
pub const TILE_SIZE: usize = 256;
/// Number of pixels in one tile.
pub const TILE_PIXELS: usize = TILE_SIZE * TILE_SIZE;

/// Premultiplied, linear-light RGBA colour.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba::new(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);

    /// Build from already premultiplied components.
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Rgba { r, g, b, a }
    }

    /// Build from straight (non-premultiplied) components.
    pub fn from_straight(r: f32, g: f32, b: f32, a: f32) -> Self {
        Rgba::new(r * a, g * a, b * a, a)
    }

    /// Straight components `[r, g, b, a]`; fully transparent pixels yield black.
    pub fn to_straight(self) -> [f32; 4] {
        if self.a <= 0.0 {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [self.r / self.a, self.g / self.a, self.b / self.a, self.a]
        }
    }

    /// Multiply every component (including alpha) by `k`.
    #[inline]
    pub fn scale(self, k: f32) -> Self {
        Rgba::new(self.r * k, self.g * k, self.b * k, self.a * k)
    }

    #[inline]
    pub fn is_transparent(self) -> bool {
        self.a <= 0.0
    }

    /// Source-over compositing: `self` on top of `below`.
    #[inline]
    pub fn over(self, below: Rgba) -> Rgba {
        let k = 1.0 - self.a;
        Rgba::new(
            self.r + below.r * k,
            self.g + below.g * k,
            self.b + below.b * k,
            self.a + below.a * k,
        )
    }
}

/// Integer coordinate of a tile in the tile grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    pub x: i32,
    pub y: i32,
}

impl TileCoord {
    pub const fn new(x: i32, y: i32) -> Self {
        TileCoord { x, y }
    }

    /// The tile containing the pixel at `(px, py)`. Works for negative pixels.
    pub fn containing(px: i32, py: i32) -> Self {
        let t = TILE_SIZE as i32;
        TileCoord::new(px.div_euclid(t), py.div_euclid(t))
    }

    /// Pixel position of this tile's top-left corner.
    pub fn origin(self) -> (i32, i32) {
        let t = TILE_SIZE as i32;
        (self.x * t, self.y * t)
    }

    pub fn rect(self) -> Rect {
        let (x, y) = self.origin();
        Rect::new(x, y, TILE_SIZE as u32, TILE_SIZE as u32)
    }
}

/// An axis-aligned pixel rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        if r <= x || b <= y {
            Rect::default()
        } else {
            Rect::new(x, y, (r - x) as u32, (b - y) as u32)
        }
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let r = self.right().max(other.right());
        let b = self.bottom().max(other.bottom());
        Rect::new(x, y, (r - x) as u32, (b - y) as u32)
    }

    /// Every tile that overlaps this rectangle.
    pub fn tiles(&self) -> Vec<TileCoord> {
        if self.is_empty() {
            return Vec::new();
        }
        let first = TileCoord::containing(self.x, self.y);
        let last = TileCoord::containing(self.right() - 1, self.bottom() - 1);
        let mut out = Vec::with_capacity(((last.x - first.x + 1) * (last.y - first.y + 1)).max(0) as usize);
        for ty in first.y..=last.y {
            for tx in first.x..=last.x {
                out.push(TileCoord::new(tx, ty));
            }
        }
        out
    }
}

/// A 2-D affine transform: `x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn translate(tx: f32, ty: f32) -> Self {
        Affine {
            tx,
            ty,
            ..Affine::IDENTITY
        }
    }

    /// The six coefficients, for serialization: `[a, b, c, d, tx, ty]`.
    pub fn coeffs(&self) -> [f32; 6] {
        [self.a, self.b, self.c, self.d, self.tx, self.ty]
    }

    pub fn from_coeffs([a, b, c, d, tx, ty]: [f32; 6]) -> Affine {
        Affine { a, b, c, d, tx, ty }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Affine {
            a: sx,
            d: sy,
            ..Affine::IDENTITY
        }
    }

    /// Horizontal shear: x' = x + k·y. `k = tan(angle)` skews by `angle`.
    pub fn shear_x(k: f32) -> Self {
        Affine {
            c: k,
            ..Affine::IDENTITY
        }
    }

    /// Counter-clockwise rotation (in screen coordinates, y down) by `radians`.
    pub fn rotate(radians: f32) -> Self {
        let (s, c) = radians.sin_cos();
        Affine {
            a: c,
            b: s,
            c: -s,
            d: c,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Scale by `(sx, sy)` and rotate by `radians` around `(cx, cy)`.
    pub fn around(cx: f32, cy: f32, sx: f32, sy: f32, radians: f32) -> Self {
        Affine::translate(-cx, -cy)
            .then(&Affine::rotate(radians))
            .then(&Affine::scale(sx, sy))
            .then(&Affine::translate(cx, cy))
    }

    /// `self` applied first, then `next`.
    pub fn then(&self, next: &Affine) -> Affine {
        Affine {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            tx: next.a * self.tx + next.c * self.ty + next.tx,
            ty: next.b * self.tx + next.d * self.ty + next.ty,
        }
    }

    #[inline]
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }

    pub fn determinant(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.determinant();
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine {
            a,
            b,
            c,
            d,
            tx: -(a * self.tx + c * self.ty),
            ty: -(b * self.tx + d * self.ty),
        })
    }

    /// True when this is a translation by whole pixels (no resampling needed).
    pub fn integer_translation(&self) -> Option<(i32, i32)> {
        let lin = (self.a - 1.0).abs() < 1e-6 && (self.d - 1.0).abs() < 1e-6;
        let shear = self.b.abs() < 1e-6 && self.c.abs() < 1e-6;
        let whole = (self.tx - self.tx.round()).abs() < 1e-4 && (self.ty - self.ty.round()).abs() < 1e-4;
        (lin && shear && whole).then(|| (self.tx.round() as i32, self.ty.round() as i32))
    }

    /// True when the transform only permutes/flips axes and translates by
    /// whole pixels (90° rotations, mirrors), so pixels can be remapped
    /// exactly without resampling.
    pub fn is_pixel_exact(&self) -> bool {
        let unit = |v: f32| (v.abs() - 1.0).abs() < 1e-6 || v.abs() < 1e-6;
        let whole = |v: f32| (v - v.round()).abs() < 1e-4;
        unit(self.a)
            && unit(self.b)
            && unit(self.c)
            && unit(self.d)
            && (self.a.abs() > 0.5) != (self.b.abs() > 0.5)
            && (self.c.abs() > 0.5) != (self.d.abs() > 0.5)
            && self.determinant().abs() > 0.5
            && whole(self.tx)
            && whole(self.ty)
    }

    /// Axis-aligned bounds of a transformed rectangle.
    pub fn transform_rect(&self, r: Rect) -> Rect {
        let corners = [
            self.apply(r.x as f32, r.y as f32),
            self.apply(r.right() as f32, r.y as f32),
            self.apply(r.x as f32, r.bottom() as f32),
            self.apply(r.right() as f32, r.bottom() as f32),
        ];
        // A small epsilon keeps float noise (cos(π/2) ≠ 0) from adding a row.
        const EPS: f32 = 1e-3;
        let x0 = (corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min) + EPS).floor() as i32;
        let y0 = (corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min) + EPS).floor() as i32;
        let x1 = (corners.iter().map(|c| c.0).fold(f32::NEG_INFINITY, f32::max) - EPS).ceil() as i32;
        let y1 = (corners.iter().map(|c| c.1).fold(f32::NEG_INFINITY, f32::max) - EPS).ceil() as i32;
        Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
    }
}

/// How a tile stores its pixels. All arithmetic happens in `f32`; the
/// 16-bit form exists to halve memory for layers at rest.
#[derive(Clone, PartialEq)]
enum TileData {
    /// Premultiplied linear f32 RGBA (16 bytes/pixel). Tiles being edited.
    F32(Box<[Rgba]>),
    /// Premultiplied linear u16 RGBA (8 bytes/pixel). Lossless for any
    /// 8-bit sRGB source; tiles at rest after a command finishes.
    U16(Box<[[u16; 4]]>),
}

/// A fixed-size block of pixels.
#[derive(Clone)]
pub struct Tile {
    data: TileData,
}

#[inline]
fn to_u16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

#[inline]
fn from_u16(v: u16) -> f32 {
    v as f32 / 65535.0
}

impl Tile {
    pub fn new() -> Self {
        Tile::filled(Rgba::TRANSPARENT)
    }

    pub fn filled(c: Rgba) -> Self {
        Tile {
            data: TileData::F32(vec![c; TILE_PIXELS].into_boxed_slice()),
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> Rgba {
        let i = y * TILE_SIZE + x;
        match &self.data {
            TileData::F32(p) => p[i],
            TileData::U16(p) => {
                let q = p[i];
                Rgba::new(from_u16(q[0]), from_u16(q[1]), from_u16(q[2]), from_u16(q[3]))
            }
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, p: Rgba) {
        self.pixels_mut()[y * TILE_SIZE + x] = p;
    }

    /// Read access to the pixels as f32. Borrows directly for f32 tiles and
    /// converts for compact ones.
    pub fn pixels(&self) -> std::borrow::Cow<'_, [Rgba]> {
        match &self.data {
            TileData::F32(p) => std::borrow::Cow::Borrowed(p),
            TileData::U16(p) => std::borrow::Cow::Owned(
                p.iter()
                    .map(|q| Rgba::new(from_u16(q[0]), from_u16(q[1]), from_u16(q[2]), from_u16(q[3])))
                    .collect(),
            ),
        }
    }

    /// Mutable f32 access; a compact tile is expanded first.
    pub fn pixels_mut(&mut self) -> &mut [Rgba] {
        if let TileData::U16(p) = &self.data {
            let f: Vec<Rgba> = p
                .iter()
                .map(|q| Rgba::new(from_u16(q[0]), from_u16(q[1]), from_u16(q[2]), from_u16(q[3])))
                .collect();
            self.data = TileData::F32(f.into_boxed_slice());
        }
        match &mut self.data {
            TileData::F32(p) => p,
            TileData::U16(_) => unreachable!(),
        }
    }

    /// Store this tile compactly (16-bit). Values outside `[0, 1]` are
    /// clamped, so only call this on tiles at rest, not on HDR scratch data.
    pub fn compact(&mut self) {
        if let TileData::F32(p) = &self.data {
            let q: Vec<[u16; 4]> = p
                .iter()
                .map(|c| [to_u16(c.r), to_u16(c.g), to_u16(c.b), to_u16(c.a)])
                .collect();
            self.data = TileData::U16(q.into_boxed_slice());
        }
    }

    pub fn is_compact(&self) -> bool {
        matches!(self.data, TileData::U16(_))
    }

    /// Bytes used by the pixel data.
    pub fn byte_size(&self) -> usize {
        match &self.data {
            TileData::F32(_) => TILE_PIXELS * 16,
            TileData::U16(_) => TILE_PIXELS * 8,
        }
    }

    /// The stored pixel data as raw bytes (native endianness), with a tag
    /// that tells the two storage formats apart: 0 for f32, 1 for 16-bit.
    /// Equal tags and bytes mean equal tiles; content hashing relies on it.
    pub fn raw_bytes(&self) -> (u8, &[u8]) {
        match &self.data {
            // SAFETY: Rgba is repr(C) of four f32 and [u16; 4] is plain old
            // data; both have no padding, so every byte is initialised.
            TileData::F32(p) => (0, unsafe {
                std::slice::from_raw_parts(p.as_ptr() as *const u8, std::mem::size_of_val(&p[..]))
            }),
            TileData::U16(p) => (1, unsafe {
                std::slice::from_raw_parts(p.as_ptr() as *const u8, std::mem::size_of_val(&p[..]))
            }),
        }
    }

    /// The inverse of [`Tile::raw_bytes`]: a tile in the storage format
    /// `format` names (0: f32, 1: 16-bit) from its bytes in native
    /// endianness. `None` for an unknown format or a wrong length.
    pub fn from_raw_bytes(format: u8, bytes: &[u8]) -> Option<Tile> {
        let data = match format {
            0 if bytes.len() == TILE_PIXELS * 16 => TileData::F32(
                bytes
                    .chunks_exact(16)
                    .map(|c| {
                        let f = |i: usize| f32::from_ne_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
                        Rgba::new(f(0), f(4), f(8), f(12))
                    })
                    .collect(),
            ),
            1 if bytes.len() == TILE_PIXELS * 8 => TileData::U16(
                bytes
                    .chunks_exact(8)
                    .map(|c| std::array::from_fn(|i| u16::from_ne_bytes([c[2 * i], c[2 * i + 1]])))
                    .collect(),
            ),
            _ => return None,
        };
        Some(Tile { data })
    }

    /// True when every pixel is fully transparent.
    pub fn is_blank(&self) -> bool {
        match &self.data {
            TileData::F32(p) => p.iter().all(|p| p.is_transparent()),
            TileData::U16(p) => p.iter().all(|q| q[3] == 0),
        }
    }
}

impl PartialEq for Tile {
    fn eq(&self, other: &Self) -> bool {
        match (&self.data, &other.data) {
            (TileData::F32(a), TileData::F32(b)) => a == b,
            (TileData::U16(a), TileData::U16(b)) => a == b,
            _ => *self.pixels() == *other.pixels(),
        }
    }
}

impl Default for Tile {
    fn default() -> Self {
        Tile::new()
    }
}

impl fmt::Debug for Tile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tile({}x{})", TILE_SIZE, TILE_SIZE)
    }
}

/// A sparse grid of shared tiles.
#[derive(Clone, Default)]
pub struct TileStore {
    tiles: HashMap<TileCoord, Arc<Tile>>,
}

impl TileStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn tile(&self, c: TileCoord) -> Option<&Tile> {
        self.tiles.get(&c).map(|t| t.as_ref())
    }

    pub fn tile_arc(&self, c: TileCoord) -> Option<&Arc<Tile>> {
        self.tiles.get(&c)
    }

    /// Mutable access to a tile, allocating a blank one if needed. If the
    /// tile is shared with another store (an undo snapshot, say) it is copied
    /// first, so the other store is never affected.
    pub fn tile_mut(&mut self, c: TileCoord) -> &mut Tile {
        Arc::make_mut(self.tiles.entry(c).or_insert_with(|| Arc::new(Tile::new())))
    }

    pub fn insert(&mut self, c: TileCoord, tile: Arc<Tile>) {
        self.tiles.insert(c, tile);
    }

    pub fn remove(&mut self, c: TileCoord) -> Option<Arc<Tile>> {
        self.tiles.remove(&c)
    }

    pub fn coords(&self) -> impl Iterator<Item = TileCoord> + '_ {
        self.tiles.keys().copied()
    }

    pub fn get_pixel(&self, px: i32, py: i32) -> Rgba {
        let c = TileCoord::containing(px, py);
        match self.tiles.get(&c) {
            Some(t) => {
                let (ox, oy) = c.origin();
                t.get((px - ox) as usize, (py - oy) as usize)
            }
            None => Rgba::TRANSPARENT,
        }
    }

    pub fn set_pixel(&mut self, px: i32, py: i32, p: Rgba) {
        let c = TileCoord::containing(px, py);
        let (ox, oy) = c.origin();
        self.tile_mut(c).set((px - ox) as usize, (py - oy) as usize, p);
    }

    /// Pixel rectangle covering every allocated tile, or `None` when empty.
    pub fn bounds(&self) -> Option<Rect> {
        self.tiles.keys().map(|c| c.rect()).reduce(|a, b| a.union(&b))
    }

    /// Tight rectangle around every non-transparent pixel, or `None`.
    /// Scans pixels, so it is O(allocated tiles × tile area).
    pub fn content_bounds(&self) -> Option<Rect> {
        let mut acc: Option<Rect> = None;
        for (c, tile) in &self.tiles {
            let (ox, oy) = c.origin();
            let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for (i, p) in tile.pixels().iter().enumerate() {
                if p.a > 0.0 {
                    let (x, y) = ((i % TILE_SIZE) as i32, (i / TILE_SIZE) as i32);
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
            if x1 >= x0 {
                let r = Rect::new(ox + x0, oy + y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
                acc = Some(acc.map_or(r, |a| a.union(&r)));
            }
        }
        acc
    }

    /// Compact every tile this store owns outright (tiles shared with
    /// snapshots are already at rest and left alone).
    pub fn compact(&mut self) {
        for tile in self.tiles.values_mut() {
            if let Some(t) = Arc::get_mut(tile) {
                t.compact();
            }
        }
    }

    /// Bytes of pixel data referenced by this store (shared tiles count once here).
    pub fn byte_size(&self) -> usize {
        self.tiles.values().map(|t| t.byte_size()).sum()
    }

    /// Drop tiles that are fully transparent.
    pub fn prune_blank(&mut self) {
        self.tiles.retain(|_, t| !t.is_blank());
    }

    /// A copy shifted by whole pixels. Tile-aligned shifts re-key the shared
    /// tiles without touching pixels.
    pub fn translated(&self, dx: i32, dy: i32) -> TileStore {
        let t = TILE_SIZE as i32;
        let mut out = TileStore::new();
        if dx % t == 0 && dy % t == 0 {
            for (c, tile) in &self.tiles {
                out.insert(TileCoord::new(c.x + dx / t, c.y + dy / t), tile.clone());
            }
            return out;
        }
        for (c, tile) in &self.tiles {
            let (ox, oy) = c.origin();
            let px = tile.pixels();
            for row in 0..TILE_SIZE {
                let sy = oy + row as i32 + dy;
                let dst_c_y = sy.div_euclid(t);
                let dy_in = sy.rem_euclid(t) as usize;
                let src_row = &px[row * TILE_SIZE..(row + 1) * TILE_SIZE];
                // The shifted row spans at most two destination tiles.
                let first_x = ox + dx;
                let mut x = first_x;
                let end = first_x + t;
                while x < end {
                    let dst_c = TileCoord::new(x.div_euclid(t), dst_c_y);
                    let (dox, _) = dst_c.origin();
                    let start_in = (x - dox) as usize;
                    let len = (t as usize - start_in).min((end - x) as usize);
                    let src_off = (x - first_x) as usize;
                    let dst_tile = out.tile_mut(dst_c);
                    let base = dy_in * TILE_SIZE + start_in;
                    dst_tile.pixels_mut()[base..base + len].copy_from_slice(&src_row[src_off..src_off + len]);
                    x += len as i32;
                }
            }
        }
        out.prune_blank();
        out
    }

    /// Copy a dense raster in, with its top-left corner at `(x0, y0)`.
    pub fn from_raster(r: &Raster, x0: i32, y0: i32) -> Self {
        let mut store = TileStore::new();
        let rect = Rect::new(x0, y0, r.width, r.height);
        for c in rect.tiles() {
            let tile = store.tile_mut(c);
            let (ox, oy) = c.origin();
            let sub = rect.intersect(&c.rect());
            for py in sub.y..sub.bottom() {
                for px in sub.x..sub.right() {
                    let p = r.get((px - x0) as u32, (py - y0) as u32);
                    tile.set((px - ox) as usize, (py - oy) as usize, p);
                }
            }
        }
        store.prune_blank();
        store
    }

    /// Read a rectangle out into a dense raster.
    pub fn to_raster(&self, rect: Rect) -> Raster {
        let mut out = Raster::new(rect.w, rect.h);
        for c in rect.tiles() {
            let Some(tile) = self.tile(c) else { continue };
            let (ox, oy) = c.origin();
            let sub = rect.intersect(&c.rect());
            for py in sub.y..sub.bottom() {
                for px in sub.x..sub.right() {
                    let p = tile.get((px - ox) as usize, (py - oy) as usize);
                    out.set((px - rect.x) as u32, (py - rect.y) as u32, p);
                }
            }
        }
        out
    }
}

impl fmt::Debug for TileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TileStore({} tiles)", self.tiles.len())
    }
}

/// A dense, row-major image. Used for I/O, tests and small scratch buffers.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
}

impl Raster {
    pub fn new(width: u32, height: u32) -> Self {
        Raster::filled(width, height, Rgba::TRANSPARENT)
    }

    pub fn filled(width: u32, height: u32, c: Rgba) -> Self {
        Raster {
            width,
            height,
            pixels: vec![c; width as usize * height as usize],
        }
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Rgba {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, p: Rgba) {
        self.pixels[y as usize * self.width as usize + x as usize] = p;
    }

    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_floor_divides_negative_pixels() {
        assert_eq!(TileCoord::containing(0, 0), TileCoord::new(0, 0));
        assert_eq!(TileCoord::containing(255, 255), TileCoord::new(0, 0));
        assert_eq!(TileCoord::containing(256, 0), TileCoord::new(1, 0));
        assert_eq!(TileCoord::containing(-1, -1), TileCoord::new(-1, -1));
        assert_eq!(TileCoord::containing(-256, 0), TileCoord::new(-1, 0));
    }

    #[test]
    fn rect_tiles_cover_edges() {
        let r = Rect::new(250, 0, 10, 300);
        let t = r.tiles();
        assert_eq!(t.len(), 4);
        assert!(t.contains(&TileCoord::new(0, 0)));
        assert!(t.contains(&TileCoord::new(1, 1)));
        assert!(Rect::default().tiles().is_empty());
    }

    #[test]
    fn clone_is_copy_on_write() {
        let mut a = TileStore::new();
        a.set_pixel(10, 10, Rgba::WHITE);
        let snapshot = a.clone();
        assert!(Arc::ptr_eq(
            a.tile_arc(TileCoord::new(0, 0)).unwrap(),
            snapshot.tile_arc(TileCoord::new(0, 0)).unwrap()
        ));
        a.set_pixel(10, 10, Rgba::BLACK);
        assert_eq!(a.get_pixel(10, 10), Rgba::BLACK);
        assert_eq!(snapshot.get_pixel(10, 10), Rgba::WHITE);
    }

    #[test]
    fn raw_bytes_round_trip_in_both_storage_formats() {
        let mut t = Tile::new();
        t.set(3, 4, Rgba::new(0.25, 0.5, 2.5, 1.0));
        let (f, bytes) = t.raw_bytes();
        assert_eq!((f, bytes.len()), (0, TILE_PIXELS * 16));
        let back = Tile::from_raw_bytes(f, bytes).unwrap();
        assert!(!back.is_compact());
        assert_eq!(back.get(3, 4), Rgba::new(0.25, 0.5, 2.5, 1.0), "HDR survives f32");
        assert_eq!(back.raw_bytes(), t.raw_bytes());

        t.compact();
        let (f, bytes) = t.raw_bytes();
        assert_eq!((f, bytes.len()), (1, TILE_PIXELS * 8));
        let back = Tile::from_raw_bytes(f, bytes).unwrap();
        assert!(back.is_compact());
        assert_eq!(
            back.get(3, 4),
            Rgba::new(16384.0 / 65535.0, 32768.0 / 65535.0, 1.0, 1.0)
        );
        assert_eq!(back.raw_bytes(), t.raw_bytes());

        assert!(Tile::from_raw_bytes(1, &bytes[1..]).is_none(), "wrong length");
        assert!(Tile::from_raw_bytes(2, bytes).is_none(), "unknown format");
    }

    #[test]
    fn raster_round_trip_with_offset() {
        let mut r = Raster::new(300, 20);
        r.set(299, 19, Rgba::WHITE);
        r.set(0, 0, Rgba::BLACK);
        let store = TileStore::from_raster(&r, -100, 500);
        assert_eq!(store.get_pixel(199, 519), Rgba::WHITE);
        assert_eq!(store.get_pixel(-100, 500), Rgba::BLACK);
        let back = store.to_raster(Rect::new(-100, 500, 300, 20));
        assert_eq!(back, r);
    }

    #[test]
    fn content_bounds_is_tight() {
        let mut s = TileStore::new();
        assert_eq!(s.content_bounds(), None);
        s.set_pixel(300, 10, Rgba::WHITE);
        s.set_pixel(-5, 40, Rgba::WHITE);
        assert_eq!(s.content_bounds(), Some(Rect::new(-5, 10, 306, 31)));
        assert_eq!(s.bounds(), Some(Rect::new(-256, 0, 768, 256)));
    }

    #[test]
    fn shear_moves_x_by_ky_and_inverts() {
        let t = Affine::shear_x(0.5);
        assert_eq!(t.apply(10.0, 4.0), (12.0, 4.0));
        let inv = t.inverse().unwrap();
        let (x, y) = inv.apply(12.0, 4.0);
        assert!((x - 10.0).abs() < 1e-5 && (y - 4.0).abs() < 1e-5);
    }

    #[test]
    fn translated_matches_pixel_shift() {
        let mut r = Raster::new(300, 70);
        for y in 0..70 {
            for x in 0..300 {
                r.set(x, y, Rgba::new(x as f32 / 300.0, y as f32 / 70.0, 0.5, 1.0));
            }
        }
        let store = TileStore::from_raster(&r, 0, 0);
        for (dx, dy) in [(256, -512), (17, 3), (-40, 300), (255, 255)] {
            let moved = store.translated(dx, dy);
            assert_eq!(moved.get_pixel(dx, dy), r.get(0, 0), "({dx},{dy}) origin");
            assert_eq!(
                moved.get_pixel(299 + dx, 69 + dy),
                r.get(299, 69),
                "({dx},{dy}) corner"
            );
            assert_eq!(
                moved.get_pixel(150 + dx, 35 + dy),
                r.get(150, 35),
                "({dx},{dy}) middle"
            );
            assert_eq!(moved.get_pixel(dx - 1, dy), Rgba::TRANSPARENT);
            assert_eq!(moved.get_pixel(300 + dx, dy), Rgba::TRANSPARENT);
        }
        // Aligned shifts share tiles.
        let aligned = store.translated(256, 0);
        assert!(Arc::ptr_eq(
            store.tile_arc(TileCoord::new(0, 0)).unwrap(),
            aligned.tile_arc(TileCoord::new(1, 0)).unwrap()
        ));
    }

    #[test]
    fn affine_compose_and_invert() {
        let t = Affine::around(10.0, 20.0, 2.0, 2.0, std::f32::consts::FRAC_PI_2);
        let (x, y) = t.apply(10.0, 20.0);
        assert!(
            (x - 10.0).abs() < 1e-4 && (y - 20.0).abs() < 1e-4,
            "centre stays put"
        );
        let (x, y) = t.apply(11.0, 20.0);
        assert!(
            (x - 10.0).abs() < 1e-4 && (y - 22.0).abs() < 1e-4,
            "rotated 90° and doubled: {x},{y}"
        );
        let inv = t.inverse().unwrap();
        let (bx, by) = inv.apply(x, y);
        assert!((bx - 11.0).abs() < 1e-4 && (by - 20.0).abs() < 1e-4);
        assert_eq!(Affine::translate(3.0, -4.0).integer_translation(), Some((3, -4)));
        assert_eq!(Affine::translate(3.5, 0.0).integer_translation(), None);
        assert!(Affine::scale(0.0, 1.0).inverse().is_none());
        assert_eq!(
            Affine::rotate(std::f32::consts::FRAC_PI_2).transform_rect(Rect::new(0, 0, 10, 4)),
            Rect::new(-4, 0, 4, 10)
        );
    }

    #[test]
    fn compact_tiles_round_trip_8bit_values_and_halve_memory() {
        let mut t = Tile::new();
        // Every 8-bit sRGB level, converted to linear, must survive 16-bit storage.
        for v in 0..=255u8 {
            let c = v as f32 / 255.0;
            let lin = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            t.set(v as usize, 0, Rgba::new(lin, lin * 0.5, 1.0 - lin, 1.0));
        }
        let before = t.pixels().to_vec();
        assert_eq!(t.byte_size(), TILE_PIXELS * 16);
        t.compact();
        assert!(t.is_compact());
        assert_eq!(t.byte_size(), TILE_PIXELS * 8);
        for (a, b) in before.iter().zip(t.pixels().iter()) {
            for (x, y) in [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)] {
                assert!((x - y).abs() <= 1.0 / 65535.0, "{x} vs {y}");
            }
        }
        // Writing expands again transparently; equality compares values.
        let mut u = t.clone();
        u.set(3, 3, Rgba::WHITE);
        assert!(!u.is_compact());
        assert_eq!(u.get(3, 3), Rgba::WHITE);
        assert_eq!(u.get(7, 0), t.get(7, 0));
        let mut v = t.clone();
        v.pixels_mut();
        assert_eq!(v, t, "expanded and compact tiles with equal values compare equal");

        // A store compacts only tiles it owns outright.
        let mut store = TileStore::new();
        store.set_pixel(0, 0, Rgba::WHITE);
        store.set_pixel(300, 0, Rgba::WHITE);
        let snapshot = store.clone(); // shares both tiles
        store.set_pixel(1, 0, Rgba::BLACK); // tile 0 becomes unique via copy-on-write
        store.compact();
        assert!(store.tile(TileCoord::new(0, 0)).unwrap().is_compact());
        assert!(
            !store.tile(TileCoord::new(1, 0)).unwrap().is_compact(),
            "shared tile untouched"
        );
        assert!(
            !snapshot.tile(TileCoord::new(0, 0)).unwrap().is_compact(),
            "snapshot keeps its own copy"
        );
        assert_eq!(store.get_pixel(0, 0), Rgba::WHITE);
    }

    #[test]
    fn over_is_associative_enough() {
        let top = Rgba::from_straight(1.0, 0.0, 0.0, 0.5);
        let bottom = Rgba::WHITE;
        let out = top.over(bottom);
        let s = out.to_straight();
        assert!((s[0] - 1.0).abs() < 1e-6);
        assert!((s[1] - 0.5).abs() < 1e-6);
        assert!((s[3] - 1.0).abs() < 1e-6);
    }
}
