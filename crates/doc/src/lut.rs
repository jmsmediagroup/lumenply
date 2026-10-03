//! Colour lookup tables for the Color Lookup adjustment (Photoshop's
//! "3DLUT" lookups; `.cube` / `.3dl` files).
//!
//! A [`Lut3D`] is an `N³` lattice of RGB outputs over a cubic input domain,
//! sampled with **tetrahedral** interpolation (the standard in Resolve and
//! OCIO: smoother than trilinear along the neutral axis, and exact on
//! anything linear inside a cell). An optional per-channel 1D "shaper"
//! runs first, which is how 1D `.cube` files (and Resolve's combined
//! 1D + 3D files) are represented without blowing a long 1D table up into
//! a coarse cube.
//!
//! LUT files assume display-referred, gamma-encoded RGB, so the adjustment
//! feeds them sRGB-encoded values like every other gamma-space adjustment
//! (ADR 0005); this module itself is colour-space agnostic.
//!
//! The parsers and writers for the file formats live in `lumenply-io`; the
//! built-in looks are generated in code in [`looks`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub mod looks;

/// Smallest and largest lattice a 3D table may have.
pub const LUT3D_SIZES: std::ops::RangeInclusive<usize> = 2..=65;
/// Largest 1D table (entries per channel).
pub const MAX_LUT1D: usize = 65_536;

/// A per-channel 1D table: channel `c` of the input looks up column `c`.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1D {
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// At least two entries, evenly spaced over the domain.
    pub data: Vec<[f32; 3]>,
}

impl Lut1D {
    /// Look each channel up in its own column, linearly interpolated; inputs
    /// outside the domain clamp to its ends.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n1 = (self.data.len() - 1) as f32;
        std::array::from_fn(|c| {
            let span = self.domain_max[c] - self.domain_min[c];
            let p = ((rgb[c] - self.domain_min[c]) / span).clamp(0.0, 1.0) * n1;
            // NaN clamps to NaN; `as usize` turns it into 0.
            let i = (p as usize).min(self.data.len() - 2);
            let t = (p - i as f32).clamp(0.0, 1.0);
            let (a, b) = (self.data[i][c], self.data[i + 1][c]);
            a + (b - a) * t
        })
    }

    fn validate(&self) -> Result<(), String> {
        if !(2..=MAX_LUT1D).contains(&self.data.len()) {
            return Err(format!(
                "1D table has {} entries (2..={MAX_LUT1D})",
                self.data.len()
            ));
        }
        check_domain(self.domain_min, self.domain_max)?;
        if !self.data.iter().flatten().all(|v| v.is_finite()) {
            return Err("1D table has a non-finite value".into());
        }
        Ok(())
    }
}

/// A 3D colour lookup table, plus an optional 1D shaper applied first.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "LutRepr", into = "LutRepr")]
pub struct Lut3D {
    /// The file's `TITLE`, if it had one.
    pub title: String,
    /// Lattice points per axis, in [`LUT3D_SIZES`].
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// `size³` outputs, red varying fastest: the entry for lattice point
    /// `(r, g, b)` sits at `r + g·size + b·size²` (the `.cube` order).
    pub data: Vec<[f32; 3]>,
    pub shaper: Option<Lut1D>,
}

impl PartialEq for Lut3D {
    fn eq(&self, other: &Self) -> bool {
        // Tables are shared through `Arc`s; comparing one with itself (an
        // unchanged adjustment, every UI frame) needn't walk 36k entries.
        std::ptr::eq(self, other)
            || (self.size == other.size
                && self.title == other.title
                && self.domain_min == other.domain_min
                && self.domain_max == other.domain_max
                && self.shaper == other.shaper
                && self.data == other.data)
    }
}

fn check_domain(min: [f32; 3], max: [f32; 3]) -> Result<(), String> {
    for c in 0..3 {
        if !(min[c].is_finite() && max[c].is_finite() && max[c] > min[c]) {
            return Err(format!("domain {min:?}..{max:?} is empty or not finite"));
        }
    }
    Ok(())
}

impl Lut3D {
    /// The table that changes nothing, with `size` points per axis.
    pub fn identity(size: usize) -> Lut3D {
        let n = size.clamp(*LUT3D_SIZES.start(), *LUT3D_SIZES.end());
        Lut3D::from_fn(n, |rgb| rgb)
    }

    /// Sample `f` at every lattice point of a `[0, 1]³` cube.
    pub fn from_fn(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> Lut3D {
        let n = size.clamp(*LUT3D_SIZES.start(), *LUT3D_SIZES.end());
        let s = (n - 1) as f32;
        let mut data = Vec::with_capacity(n * n * n);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    data.push(f([r as f32 / s, g as f32 / s, b as f32 / s]));
                }
            }
        }
        Lut3D {
            title: String::new(),
            size: n,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data,
            shaper: None,
        }
    }

    /// A 1D table on its own: a shaper over the identity cube.
    pub fn from_1d(shaper: Lut1D) -> Lut3D {
        Lut3D {
            shaper: Some(shaper),
            ..Lut3D::identity(2)
        }
    }

    /// Check sizes, domains and values; parsers and loaders call this so a
    /// table in use is always well formed.
    pub fn validate(&self) -> Result<(), String> {
        if !LUT3D_SIZES.contains(&self.size) {
            return Err(format!("3D table size {} is outside 2..=65", self.size));
        }
        let want = self.size * self.size * self.size;
        if self.data.len() != want {
            return Err(format!(
                "3D table of size {} needs {want} entries, has {}",
                self.size,
                self.data.len()
            ));
        }
        check_domain(self.domain_min, self.domain_max)?;
        if !self.data.iter().flatten().all(|v| v.is_finite()) {
            return Err("3D table has a non-finite value".into());
        }
        if let Some(s) = &self.shaper {
            s.validate()?;
        }
        Ok(())
    }

    /// Whether the table maps every input in `[0, 1]` to itself (to 1e-6).
    /// Cheap enough for every UI frame: no table is built.
    pub fn is_identity(&self) -> bool {
        let n = self.size;
        if self.shaper.is_some()
            || self.domain_min != [0.0; 3]
            || self.domain_max != [1.0; 3]
            || n < 2
            || self.data.len() != n * n * n
        {
            return false;
        }
        let s = (n - 1) as f32;
        self.data.iter().enumerate().all(|(i, c)| {
            let want = [
                (i % n) as f32 / s,
                ((i / n) % n) as f32 / s,
                (i / (n * n)) as f32 / s,
            ];
            (0..3).all(|k| (c[k] - want[k]).abs() < 1e-6)
        })
    }

    pub fn with_title(mut self, title: &str) -> Lut3D {
        self.title = title.to_string();
        self
    }

    #[inline]
    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + self.size * (g + self.size * b)]
    }

    /// Run a colour through the shaper (if any) and the cube, then clamp
    /// to `[0, 1]`. Inputs outside the domain clamp to its faces.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let rgb = match &self.shaper {
            Some(s) => s.apply(rgb),
            None => rgb,
        };
        self.sample(rgb).map(|v| v.clamp(0.0, 1.0))
    }

    /// Tetrahedral interpolation of the cube (no shaper, no clamp). The
    /// lattice cell is split into six tetrahedra along its main diagonal;
    /// the ordering of the fractional coordinates picks one, and the colour
    /// is the barycentric mix of its four corners.
    #[inline]
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n1 = (self.size - 1) as f32;
        let mut i0 = [0usize; 3];
        let mut f = [0f32; 3];
        for c in 0..3 {
            let span = self.domain_max[c] - self.domain_min[c];
            let p = ((rgb[c] - self.domain_min[c]) / span).clamp(0.0, 1.0) * n1;
            let p = if p.is_nan() { 0.0 } else { p };
            let i = (p as usize).min(self.size - 2);
            i0[c] = i;
            f[c] = p - i as f32;
        }
        let [x, y, z] = i0;
        let [fx, fy, fz] = f;
        let c000 = self.at(x, y, z);
        let c111 = self.at(x + 1, y + 1, z + 1);
        // (weight of c000, corner 1, its weight, corner 2, its weight, weight of c111)
        let (w0, a, wa, b, wb, w1) = if fx > fy {
            if fy > fz {
                (
                    1.0 - fx,
                    self.at(x + 1, y, z),
                    fx - fy,
                    self.at(x + 1, y + 1, z),
                    fy - fz,
                    fz,
                )
            } else if fx > fz {
                (
                    1.0 - fx,
                    self.at(x + 1, y, z),
                    fx - fz,
                    self.at(x + 1, y, z + 1),
                    fz - fy,
                    fy,
                )
            } else {
                (
                    1.0 - fz,
                    self.at(x, y, z + 1),
                    fz - fx,
                    self.at(x + 1, y, z + 1),
                    fx - fy,
                    fy,
                )
            }
        } else if fz > fy {
            (
                1.0 - fz,
                self.at(x, y, z + 1),
                fz - fy,
                self.at(x, y + 1, z + 1),
                fy - fx,
                fx,
            )
        } else if fz > fx {
            (
                1.0 - fy,
                self.at(x, y + 1, z),
                fy - fz,
                self.at(x, y + 1, z + 1),
                fz - fx,
                fx,
            )
        } else {
            (
                1.0 - fy,
                self.at(x, y + 1, z),
                fy - fx,
                self.at(x + 1, y + 1, z),
                fx - fz,
                fz,
            )
        };
        std::array::from_fn(|c| w0 * c000[c] + wa * a[c] + wb * b[c] + w1 * c111[c])
    }

    /// The same mapping as one pure 3D table of `size` points: the shaper
    /// folded in (Photoshop's `.cube` reader takes 1D or 3D, not both).
    pub fn baked(&self, size: usize) -> Lut3D {
        let mut out = Lut3D::from_fn(size, |rgb| {
            // Lattice points in [0, 1] map onto this table's input domain.
            let x = std::array::from_fn(|c| {
                self.domain_min[c] + rgb[c] * (self.domain_max[c] - self.domain_min[c])
            });
            self.apply(x)
        });
        out.domain_min = self.domain_min;
        out.domain_max = self.domain_max;
        out.title = self.title.clone();
        out
    }

    /// A stable 64-bit FNV-1a hash of everything the table holds, so equal
    /// tables can share one copy in a project file.
    pub fn content_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        eat(self.title.as_bytes());
        eat(&(self.size as u64).to_le_bytes());
        let floats = |v: &[[f32; 3]], eat: &mut dyn FnMut(&[u8])| {
            for e in v {
                for x in e {
                    eat(&x.to_bits().to_le_bytes());
                }
            }
        };
        floats(&[self.domain_min, self.domain_max], &mut eat);
        floats(&self.data, &mut eat);
        if let Some(s) = &self.shaper {
            eat(b"shaper");
            floats(&[s.domain_min, s.domain_max], &mut eat);
            floats(&s.data, &mut eat);
        }
        h
    }
}

/// The JSON shape of a table when an [`crate::Adjustment`] is serialised
/// on its own (a `.lumen` project stores tables as `.cube` files instead).
#[derive(Serialize, Deserialize)]
struct LutRepr {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,
    size: usize,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    /// Flattened RGB triples.
    data: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shaper: Option<ShaperRepr>,
}

#[derive(Serialize, Deserialize)]
struct ShaperRepr {
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    data: Vec<f32>,
}

fn triples(v: &[f32]) -> Result<Vec<[f32; 3]>, String> {
    if v.len() % 3 != 0 {
        return Err("table data is not RGB triples".into());
    }
    Ok(v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

impl TryFrom<LutRepr> for Lut3D {
    type Error = String;

    fn try_from(r: LutRepr) -> Result<Self, String> {
        let lut = Lut3D {
            title: r.title,
            size: r.size,
            domain_min: r.domain_min,
            domain_max: r.domain_max,
            data: triples(&r.data)?,
            shaper: match r.shaper {
                Some(s) => Some(Lut1D {
                    domain_min: s.domain_min,
                    domain_max: s.domain_max,
                    data: triples(&s.data)?,
                }),
                None => None,
            },
        };
        lut.validate()?;
        Ok(lut)
    }
}

impl From<Lut3D> for LutRepr {
    fn from(l: Lut3D) -> Self {
        LutRepr {
            title: l.title,
            size: l.size,
            domain_min: l.domain_min,
            domain_max: l.domain_max,
            data: l.data.into_iter().flatten().collect(),
            shaper: l.shaper.map(|s| ShaperRepr {
                domain_min: s.domain_min,
                domain_max: s.domain_max,
                data: s.data.into_iter().flatten().collect(),
            }),
        }
    }
}

/// Serde helpers for an `Arc<Lut3D>` field.
pub mod shared {
    use super::*;

    pub fn serialize<S: serde::Serializer>(lut: &Arc<Lut3D>, s: S) -> Result<S::Ok, S::Error> {
        lut.as_ref().serialize(s)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Arc<Lut3D>, D::Error> {
        Lut3D::deserialize(d).map(Arc::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() <= tol)
    }

    #[test]
    fn identity_tables_change_nothing() {
        for n in [2, 17, 33] {
            let id = Lut3D::identity(n);
            assert!(id.validate().is_ok() && id.is_identity());
            for rgb in [
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 1.0],
                [0.3, 0.6, 0.9],
                [0.91, 0.05, 0.47],
            ] {
                assert!(
                    near(id.apply(rgb), rgb, 1e-6),
                    "{n}: {rgb:?} -> {:?}",
                    id.apply(rgb)
                );
            }
        }
        // Out-of-range inputs clamp to the cube's faces.
        assert_eq!(Lut3D::identity(5).apply([-0.5, 1.5, f32::NAN]), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn a_swap_table_swaps_red_and_blue() {
        let swap = Lut3D::from_fn(9, |[r, g, b]| [b, g, r]);
        assert!(!swap.is_identity());
        assert!(near(swap.apply([0.9, 0.5, 0.1]), [0.1, 0.5, 0.9], 1e-6));
        assert!(near(swap.apply([0.2, 0.3, 0.77]), [0.77, 0.3, 0.2], 1e-6));
    }

    #[test]
    fn tetrahedral_hits_lattice_points_and_a_known_interior_point() {
        // f = r·g·b is not linear, so interpolation shows between points.
        let f = |[r, g, b]: [f32; 3]| [r * g * b, r, 1.0 - b];
        let lut = Lut3D::from_fn(5, f);
        // Exact at every lattice point.
        for (r, g, b) in [(0.25, 0.5, 0.75), (1.0, 0.0, 0.5), (0.75, 0.75, 1.0)] {
            assert!(near(lut.sample([r, g, b]), f([r, g, b]), 1e-6));
        }
        // On a 2³ cube, (0.5, 0.25, 0.75) has fz > fx > fy: the tetrahedron
        // c000, c001, c101, c111 with weights 0.25 each, so r·g·b reads
        // 0.25 (trilinear would give 0.5·0.25·0.75 = 0.09375).
        let two = Lut3D::from_fn(2, f);
        let v = two.sample([0.5, 0.25, 0.75]);
        assert!(near(v, [0.25, 0.5, 0.25], 1e-6), "{v:?}");
        // Every one of the six orderings reproduces a linear function.
        let lin = Lut3D::from_fn(3, |[r, g, b]| [0.2 * r + 0.5 * g + 0.3 * b, r, g]);
        for p in [
            [0.9, 0.6, 0.3],
            [0.9, 0.3, 0.6],
            [0.6, 0.3, 0.9],
            [0.3, 0.6, 0.9],
            [0.3, 0.9, 0.6],
            [0.6, 0.9, 0.3],
        ] {
            let want = [0.2 * p[0] + 0.5 * p[1] + 0.3 * p[2], p[0], p[1]];
            assert!(near(lin.sample(p), want, 1e-6), "{p:?}");
        }
    }

    #[test]
    fn a_domain_rescales_the_input() {
        // Domain 0..2: an input of 1.0 sits in the middle of the cube.
        let mut lut = Lut3D::from_fn(3, |[r, _, _]| [r, r, r]);
        lut.domain_max = [2.0; 3];
        assert!(near(lut.apply([1.0, 0.0, 0.0]), [0.5, 0.5, 0.5], 1e-6));
    }

    #[test]
    fn a_shaper_runs_before_the_cube() {
        // 1D: square each channel (3 points: 0, 0.25, 1), identity cube.
        let lut = Lut3D::from_1d(Lut1D {
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data: vec![[0.0; 3], [0.25; 3], [1.0; 3]],
        });
        assert!(lut.validate().is_ok() && !lut.is_identity());
        // 0.5 → 0.25 exactly; 0.75 halfway between 0.25 and 1 → 0.625.
        assert!(near(lut.apply([0.5, 0.75, 1.0]), [0.25, 0.625, 1.0], 1e-6));
        let baked = lut.baked(5);
        assert!(baked.shaper.is_none());
        assert!(near(baked.apply([0.5, 0.75, 1.0]), [0.25, 0.625, 1.0], 1e-6));
    }

    #[test]
    fn validation_rejects_bad_tables() {
        let mut l = Lut3D::identity(3);
        l.data.pop();
        assert!(l.validate().is_err());
        let mut l = Lut3D::identity(3);
        l.data[4][1] = f32::NAN;
        assert!(l.validate().is_err());
        let mut l = Lut3D::identity(3);
        l.domain_max = [1.0, 0.0, 1.0];
        assert!(l.validate().is_err());
        let l = Lut3D {
            size: 66,
            ..Lut3D::identity(2)
        };
        assert!(l.validate().is_err());
    }

    #[test]
    fn tables_round_trip_through_json_and_hash_stably() {
        let lut = Lut3D::from_fn(4, |[r, g, b]| [g, b, r * 0.5]).with_title("Rotate");
        let json = serde_json::to_string(&lut).unwrap();
        let back: Lut3D = serde_json::from_str(&json).unwrap();
        assert_eq!(back, lut);
        assert_eq!(back.content_hash(), lut.content_hash());
        assert_ne!(Lut3D::identity(4).content_hash(), lut.content_hash());
        // A short data array is refused, not accepted half-filled.
        let bad = r#"{"size":2,"domain_min":[0,0,0],"domain_max":[1,1,1],"data":[0,0,0]}"#;
        assert!(serde_json::from_str::<Lut3D>(bad).is_err());
    }
}
