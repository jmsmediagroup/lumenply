//! The ICC device link Photoshop stores beside an embedded LUT (`profile`
//! in a `clrL` descriptor). Its layout copies Photoshop's own: an ICC v4
//! `link` profile RGB → RGB with `desc`, `cprt`, `pseq` and an `A2B0`
//! `mAB` whose curves and matrix are identities around one 16-bit CLUT
//! holding the table (first input slowest, as ICC orders grids). Checked
//! against a Photoshop CS6 file: its CLUT equals the embedded `.cube` read
//! red-fastest, value for value.

use lumenply_doc::Lut3D;

fn u16be(d: &mut Vec<u8>, v: u16) {
    d.extend_from_slice(&v.to_be_bytes());
}

fn u32be(d: &mut Vec<u8>, v: u32) {
    d.extend_from_slice(&v.to_be_bytes());
}

fn pad4(d: &mut Vec<u8>) {
    while d.len() % 4 != 0 {
        d.push(0);
    }
}

/// A one-record (`enUS`) multi-localised Unicode tag.
fn mluc(text: &str) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"mluc");
    u32be(&mut d, 0);
    u32be(&mut d, 1); // records
    u32be(&mut d, 12); // record size
    d.extend_from_slice(b"enUS");
    let units: Vec<u16> = text.encode_utf16().collect();
    u32be(&mut d, (units.len() * 2) as u32);
    u32be(&mut d, 28); // string offset from the tag start
    for u in units {
        u16be(&mut d, u);
    }
    d
}

/// An empty `mluc` (the profile sequence's manufacturer / model text).
fn mluc_empty() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"mluc");
    u32be(&mut d, 0);
    u32be(&mut d, 0);
    u32be(&mut d, 12);
    d
}

/// The identity `curv` (no entries).
fn curv_identity(d: &mut Vec<u8>) {
    d.extend_from_slice(b"curv");
    u32be(d, 0);
    u32be(d, 0);
}

fn pseq() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"pseq");
    u32be(&mut d, 0);
    u32be(&mut d, 2);
    for _ in 0..2 {
        d.extend_from_slice(&[0; 20]); // manufacturer, model, attributes, technology
        d.extend(mluc_empty());
        d.extend(mluc_empty());
    }
    d
}

/// The `mAB` tag: B curves, matrix, M curves, CLUT, A curves.
fn m_ab(lut: &Lut3D) -> Vec<u8> {
    // A pure table over [0, 1] goes in as is; anything else (a domain, a
    // shaper) is sampled on a 33-point grid over the encoded inputs.
    let plain = lut.shaper.is_none() && lut.domain_min == [0.0; 3] && lut.domain_max == [1.0; 3];
    let n = if plain { lut.size } else { 33 };
    let s = (n - 1) as f32;
    let mut d = Vec::new();
    d.extend_from_slice(b"mAB ");
    u32be(&mut d, 0);
    d.extend_from_slice(&[3, 3, 0, 0]);
    let offsets_at = d.len();
    d.extend_from_slice(&[0; 20]);
    let b_at = d.len();
    for _ in 0..3 {
        curv_identity(&mut d);
    }
    let matrix_at = d.len();
    for v in [1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0] {
        u32be(&mut d, (v as u32) << 16); // s15Fixed16
    }
    let m_at = d.len();
    for _ in 0..3 {
        curv_identity(&mut d);
    }
    let clut_at = d.len();
    let mut grid = [0u8; 16];
    grid[..3].fill(n as u8);
    d.extend_from_slice(&grid);
    d.extend_from_slice(&[2, 0, 0, 0]); // 16-bit precision
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                let c = if plain {
                    lut.data[r + n * (g + n * b)].map(|v| v.clamp(0.0, 1.0))
                } else {
                    lut.apply([r as f32 / s, g as f32 / s, b as f32 / s])
                };
                for v in c {
                    u16be(&mut d, q(v));
                }
            }
        }
    }
    pad4(&mut d);
    let a_at = d.len();
    for _ in 0..3 {
        curv_identity(&mut d);
    }
    for (i, at) in [b_at, matrix_at, m_at, clut_at, a_at].into_iter().enumerate() {
        d[offsets_at + 4 * i..offsets_at + 4 * i + 4].copy_from_slice(&(at as u32).to_be_bytes());
    }
    d
}

/// A device link profile applying `lut` to encoded RGB.
pub(super) fn device_link(lut: &Lut3D, name: &str) -> Vec<u8> {
    let tags: [(&[u8; 4], Vec<u8>); 4] = [
        (b"desc", mluc(name)),
        (b"cprt", mluc("Created by Lumenply")),
        (b"pseq", pseq()),
        (b"A2B0", m_ab(lut)),
    ];
    let mut d = vec![0u8; 128];
    d[4..8].copy_from_slice(b"lmnp");
    d[8..12].copy_from_slice(&[4, 0, 0, 0]); // version 4.0
    d[12..16].copy_from_slice(b"link");
    d[16..20].copy_from_slice(b"RGB ");
    d[20..24].copy_from_slice(b"RGB ");
    for (i, v) in [2026u16, 1, 1, 0, 0, 0].into_iter().enumerate() {
        d[24 + 2 * i..26 + 2 * i].copy_from_slice(&v.to_be_bytes());
    }
    d[36..40].copy_from_slice(b"acsp");
    // D50 illuminant, s15Fixed16.
    for (i, v) in [0x0000_f6d6u32, 0x0001_0000, 0x0000_d32d].into_iter().enumerate() {
        d[68 + 4 * i..72 + 4 * i].copy_from_slice(&v.to_be_bytes());
    }
    d[80..84].copy_from_slice(b"lmnp");
    u32be(&mut d, tags.len() as u32);
    let table_at = d.len();
    d.resize(table_at + 12 * tags.len(), 0);
    for (i, (sig, body)) in tags.iter().enumerate() {
        pad4(&mut d);
        let at = d.len();
        d.extend_from_slice(body);
        let e = table_at + 12 * i;
        d[e..e + 4].copy_from_slice(*sig);
        d[e + 4..e + 8].copy_from_slice(&(at as u32).to_be_bytes());
        d[e + 8..e + 12].copy_from_slice(&(body.len() as u32).to_be_bytes());
    }
    pad4(&mut d);
    let size = d.len() as u32;
    d[0..4].copy_from_slice(&size.to_be_bytes());
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(d: &[u8], at: usize) -> usize {
        u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize
    }

    /// Find a tag's body by signature.
    fn tag<'a>(p: &'a [u8], sig: &[u8]) -> &'a [u8] {
        let n = be32(p, 128);
        for i in 0..n {
            let e = 132 + 12 * i;
            if &p[e..e + 4] == sig {
                let (at, len) = (be32(p, e + 4), be32(p, e + 8));
                return &p[at..at + len];
            }
        }
        panic!("no {sig:?} tag");
    }

    #[test]
    fn the_clut_holds_the_table_first_input_slowest() {
        let lut = Lut3D::from_fn(3, |[r, g, b]| [b, g * 0.5, r]);
        let p = device_link(&lut, "Swap");
        assert_eq!(be32(&p, 0), p.len());
        assert_eq!(&p[12..24], b"linkRGB RGB ");
        assert_eq!(&p[36..40], b"acsp");
        let a2b = tag(&p, b"A2B0");
        assert_eq!(&a2b[..4], b"mAB ");
        let clut = &a2b[be32(a2b, 24)..];
        assert_eq!(&clut[..4], &[3, 3, 3, 0]);
        assert_eq!(clut[16], 2);
        let at = |r: usize, g: usize, b: usize, c: usize| {
            let i = ((r * 3 + g) * 3 + b) * 3 + c;
            u16::from_be_bytes([clut[20 + 2 * i], clut[21 + 2 * i]])
        };
        // Lattice (r=2, g=1, b=0) → (b, g/2, r) = (0, 0.25, 1).
        assert_eq!(
            [at(2, 1, 0, 0), at(2, 1, 0, 1), at(2, 1, 0, 2)],
            [0, 16384, 65535]
        );
        // The description names the table.
        let desc = tag(&p, b"desc");
        let text: Vec<u16> = desc[28..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&text).unwrap(), "Swap");
    }
}
