//! Photoshop brush sets (`.abr`): tips for the brush engine.
//!
//! Supported: versions 1 and 2 (Photoshop 6 and earlier: computed round
//! brushes and sampled tips) and versions 6, 7 and 10 (Photoshop 7 and
//! later), whose sampled tips live in the `8BIMsamp` section, stored raw
//! or PackBits-compressed, 8 or 16 bits deep. Names come from the
//! `8BIMdesc` preset descriptor when it pairs them with a tip. The
//! descriptor's other settings (dynamics, textures, dual brushes) are not
//! read; a brush that can't be read is skipped with a warning, and a file
//! with nothing readable is an error that says why.

use std::collections::HashMap;
use std::path::Path;

use crate::IoError;

/// What a brush set file holds.
#[derive(Debug, Default)]
pub struct AbrSet {
    pub brushes: Vec<AbrBrush>,
    /// Brushes that were skipped, and why.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbrBrush {
    pub name: String,
    /// Dab spacing as a fraction of the diameter, when the file says.
    pub spacing: Option<f32>,
    /// The preset's brush diameter in pixels, when the file gives one
    /// for a sampled tip (Photoshop 7+ descriptors).
    pub diameter: Option<f32>,
    pub shape: AbrShape,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AbrShape {
    /// A sampled tip: 16-bit coverage, 65535 = full paint (8-bit files
    /// are widened by ×257, so 255 → 65535 exactly).
    Sampled {
        width: u32,
        height: u32,
        /// Bit depth in the file (8 or 16).
        depth: u16,
        gray: Vec<u16>,
    },
    /// Photoshop's computed round brush.
    Computed {
        /// Pixels.
        diameter: f32,
        /// 0–1.
        hardness: f32,
        /// Degrees, counter-clockwise.
        angle: f32,
        /// 0–1.
        roundness: f32,
    },
}

/// Largest tip side read (Photoshop's own limit is 5000; the engine's
/// `MAX_TIP_SIDE` is 8192).
const MAX_SIDE: i64 = 8192;

fn err(msg: impl Into<String>) -> IoError {
    IoError::Codec(format!("ABR: {}", msg.into()))
}

/// Read a brush set from disk.
pub fn load(path: impl AsRef<Path>) -> Result<AbrSet, IoError> {
    parse(&std::fs::read(path)?)
}

/// Read a brush set from its bytes.
pub fn parse(bytes: &[u8]) -> Result<AbrSet, IoError> {
    let mut r = Reader::new(bytes);
    let version = r
        .u16()
        .map_err(|_| err("the file is too short to be a brush set"))?;
    let set = match version {
        1 | 2 => parse_v12(&mut r, version)?,
        6 | 7 | 10 => parse_v6(&mut r, version)?,
        v => {
            return Err(err(format!(
                "version {v} brush sets are not supported (only versions 1, 2, 6, 7 and 10)"
            )))
        }
    };
    if set.brushes.is_empty() {
        let why = set
            .warnings
            .first()
            .cloned()
            .unwrap_or_else(|| "the file lists no brushes".into());
        return Err(err(format!("no brushes could be read: {why}")));
    }
    Ok(set)
}

/// Bounds-checked big-endian reads; every failure is "truncated".
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

type R<T> = Result<T, String>;

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if n > self.remaining() {
            return Err("the data is truncated".into());
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> R<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }

    fn i16(&mut self) -> R<i16> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> R<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn i32(&mut self) -> R<i32> {
        Ok(self.u32()? as i32)
    }
}

fn parse_v12(r: &mut Reader, version: u16) -> Result<AbrSet, IoError> {
    let count = r.u16().map_err(err)?;
    let mut set = AbrSet::default();
    for i in 0..count as usize {
        let n = i + 1;
        let (kind, size) = match (r.u16(), r.u32()) {
            (Ok(k), Ok(s)) => (k, s as usize),
            _ => {
                set.warnings.push(format!("brush {n}: the file ends early"));
                break;
            }
        };
        let body = match r.take(size) {
            Ok(b) => b,
            Err(_) => {
                set.warnings.push(format!("brush {n}: the file ends early"));
                break;
            }
        };
        let mut b = Reader::new(body);
        let read = match kind {
            1 => computed_v12(&mut b),
            2 => sampled_v12(&mut b, version, n),
            k => Err(format!("unknown brush type {k}")),
        };
        match read {
            Ok(brush) => set.brushes.push(brush),
            Err(e) => set.warnings.push(format!("brush {n}: {e}; skipped")),
        }
    }
    Ok(set)
}

/// Version 1/2 computed brush: misc u32, then spacing, diameter,
/// roundness, angle and hardness as 16-bit values (percent / px / °).
fn computed_v12(b: &mut Reader) -> R<AbrBrush> {
    b.u32()?;
    let spacing = b.u16()?;
    let diameter = b.u16()?;
    let roundness = b.u16()?;
    let angle = b.i16()?;
    let hardness = b.u16()?;
    if diameter == 0 {
        return Err("a computed brush with no diameter".into());
    }
    Ok(AbrBrush {
        name: format!("Round {diameter}"),
        spacing: (spacing > 0).then(|| spacing as f32 / 100.0),
        diameter: None,
        shape: AbrShape::Computed {
            diameter: diameter as f32,
            hardness: (hardness as f32 / 100.0).clamp(0.0, 1.0),
            angle: angle as f32,
            roundness: (roundness as f32 / 100.0).clamp(0.01, 1.0),
        },
    })
}

/// Version 1/2 sampled brush: misc u32, spacing u16, (v2) a UCS-2 name,
/// antialias u8, short bounds, long bounds, depth u16, compression u8,
/// then the image.
fn sampled_v12(b: &mut Reader, version: u16, n: usize) -> R<AbrBrush> {
    b.u32()?;
    let spacing = b.u16()?;
    let mut name = String::new();
    if version == 2 {
        let len = b.u32()? as usize;
        if len > 4096 {
            return Err("a brush name longer than 4096 characters".into());
        }
        let units: Vec<u16> = b
            .take(len * 2)?
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        name = String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .trim()
            .to_string();
    }
    b.u8()?; // antialiasing
    b.take(8)?; // the bounds again, as 16-bit values
    let (width, height, depth, gray) = image(b)?;
    Ok(AbrBrush {
        name: if name.is_empty() {
            format!("Sampled brush {n}")
        } else {
            name
        },
        spacing: (spacing > 0).then(|| spacing as f32 / 100.0),
        diameter: None,
        shape: AbrShape::Sampled {
            width,
            height,
            depth,
            gray,
        },
    })
}

fn parse_v6(r: &mut Reader, version: u16) -> Result<AbrSet, IoError> {
    let sub = r.u16().map_err(err)?;
    if sub != 1 && sub != 2 {
        return Err(err(format!(
            "version {version}.{sub} brush sets are not supported (only subversions 1 and 2)"
        )));
    }
    let (mut samp, mut desc) = (None, None);
    while r.remaining() >= 12 {
        let sig = r.take(4).map_err(err)?;
        if sig != b"8BIM" {
            break;
        }
        let key = r.take(4).map_err(err)?;
        let len = r.u32().map_err(err)? as usize;
        let data = r.take(len.min(r.remaining())).map_err(err)?;
        match key {
            b"samp" => samp = Some(data),
            b"desc" => desc = Some(data),
            _ => {}
        }
    }
    let presets = desc.map(descriptor_presets).unwrap_or_default();
    let computed: Vec<&PresetInfo> = presets.iter().filter(|p| p.computed && p.key.is_none()).collect();
    if samp.is_none() && computed.is_empty() {
        return Err(err(
            "this brush set has no sampled tips (no 8BIMsamp section) and no computed brushes",
        ));
    }
    let by_key: HashMap<&str, &PresetInfo> = presets
        .iter()
        .filter_map(|p| p.key.as_deref().map(|k| (k, p)))
        .collect();
    let mut set = AbrSet::default();
    let mut s = Reader::new(samp.unwrap_or_default());
    let mut n = 0;
    while s.remaining() >= 4 {
        n += 1;
        let size = s.u32().map_err(err)? as usize;
        let Ok(body) = s.take(size) else {
            set.warnings.push(format!("tip {n}: the file ends early"));
            break;
        };
        // Entries are padded to a multiple of four bytes.
        let pad = (4 - size % 4) % 4;
        let _ = s.take(pad.min(s.remaining()));
        match sampled_v6(body, sub) {
            Ok((key, width, height, depth, gray)) => {
                let info = by_key.get(key.as_str());
                set.brushes.push(AbrBrush {
                    name: info
                        .and_then(|p| p.name.clone())
                        .unwrap_or_else(|| format!("Sampled brush {n}")),
                    spacing: info.and_then(|p| p.spacing).map(|s| s / 100.0),
                    diameter: info.and_then(|p| p.diameter),
                    shape: AbrShape::Sampled {
                        width,
                        height,
                        depth,
                        gray,
                    },
                })
            }
            Err(e) => set.warnings.push(format!("tip {n}: {e}; skipped")),
        }
    }
    for (i, p) in computed.into_iter().enumerate() {
        let Some(diameter) = p.diameter.filter(|d| *d > 0.0) else {
            set.warnings
                .push(format!("computed brush {}: no diameter; skipped", i + 1));
            continue;
        };
        set.brushes.push(AbrBrush {
            name: p.name.clone().unwrap_or_else(|| format!("Round {diameter:.0}")),
            spacing: p.spacing.map(|s| s / 100.0),
            diameter: None,
            shape: AbrShape::Computed {
                diameter,
                hardness: (p.hardness.unwrap_or(100.0) / 100.0).clamp(0.0, 1.0),
                angle: p.angle.unwrap_or(0.0),
                roundness: (p.roundness.unwrap_or(100.0) / 100.0).clamp(0.01, 1.0),
            },
        });
    }
    Ok(set)
}

/// One `8BIMsamp` entry: a Pascal-string key (the tip's UUID), header
/// bytes that differ by subversion, then bounds, depth, compression and
/// the image.
fn sampled_v6(body: &[u8], sub: u16) -> R<(String, u32, u32, u16, Vec<u16>)> {
    let mut b = Reader::new(body);
    let klen = b.u8()? as usize;
    let key = String::from_utf8_lossy(b.take(klen)?)
        .trim_end_matches('\0')
        .to_string();
    // Subversion 1: short bounds and an unknown short; 2: 264 unknown bytes.
    b.take(if sub == 1 { 10 } else { 264 })?;
    let (w, h, depth, gray) = image(&mut b)?;
    Ok((key, w, h, depth, gray))
}

/// Bounds (top, left, bottom, right as i32), depth u16, compression u8,
/// then raw rows or PackBits rows behind a table of 16-bit row lengths.
fn image(b: &mut Reader) -> R<(u32, u32, u16, Vec<u16>)> {
    let (top, left, bottom, right) = (b.i32()?, b.i32()?, b.i32()?, b.i32()?);
    let depth = b.u16()?;
    let compression = b.u8()?;
    let (w, h) = (right as i64 - left as i64, bottom as i64 - top as i64);
    if w <= 0 || h <= 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("a tip of {w}×{h} px is out of range"));
    }
    if depth != 8 && depth != 16 {
        return Err(format!("{depth}-bit tips are not supported (only 8 and 16)"));
    }
    let (w, h) = (w as usize, h as usize);
    let row = w * (depth as usize / 8);
    let bytes: Vec<u8> = match compression {
        0 => b.take(row * h)?.to_vec(),
        1 => {
            let mut lens = Vec::with_capacity(h);
            for _ in 0..h {
                let len = b.u16()? as usize;
                // PackBits expands at most 64-fold (2 bytes → 128): a row
                // claiming more is corrupt, and would let a tiny file
                // allocate gigabytes.
                if len * 64 < row {
                    return Err("a compressed row is too short for the tip's width".into());
                }
                lens.push(len);
            }
            let mut out = Vec::with_capacity(row * h);
            for len in lens {
                packbits(b.take(len)?, row, &mut out);
            }
            out
        }
        c => return Err(format!("unknown compression {c}")),
    };
    let gray = if depth == 8 {
        bytes.iter().map(|&v| v as u16 * 257).collect()
    } else {
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect()
    };
    Ok((w as u32, h as u32, depth, gray))
}

/// Decode one PackBits row into exactly `row` bytes (short rows are
/// padded with zero, long ones cut).
fn packbits(src: &[u8], row: usize, out: &mut Vec<u8>) {
    let start = out.len();
    let mut i = 0;
    while i < src.len() && out.len() - start < row {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let len = (n as usize + 1).min(src.len() - i);
            out.extend_from_slice(&src[i..i + len]);
            i += len;
        } else if n != -128 {
            if let Some(&v) = src.get(i) {
                out.extend(std::iter::repeat_n(v, (1 - n as isize) as usize));
            }
            i += 1;
        }
    }
    out.resize(start + row, 0);
}

/// What the `8BIMdesc` descriptor says about one brush preset.
#[derive(Debug, Default, Clone, PartialEq)]
struct PresetInfo {
    /// The preset's name (the first `Nm  ` in it; the tip inside carries
    /// a second, less specific one).
    name: Option<String>,
    /// `Dmtr`, pixels.
    diameter: Option<f32>,
    /// `Angl`, degrees.
    angle: Option<f32>,
    /// `Rndn`, percent.
    roundness: Option<f32>,
    /// `Spcn`, percent of the diameter.
    spacing: Option<f32>,
    /// `Hrdn`, percent (computed brushes).
    hardness: Option<f32>,
    /// The brush is a `computedBrush`.
    computed: bool,
    /// `sampledData`: the key of its tip in `8BIMsamp`.
    key: Option<String>,
}

/// Brush presets from the `8BIMdesc` descriptor. Each `brushPreset`
/// object holds a name and a brush object (`sampledBrush` with the tip's
/// key in `sampledData`, or `computedBrush`) with `Dmtr`, `Angl`, `Rndn`,
/// `Spcn` (and `Hrdn`) as unit floats. A scan rather than a full
/// descriptor parse, so unknown keys and types can't derail it; the first
/// value of each key in a preset wins (later ones belong to dynamics).
fn descriptor_presets(d: &[u8]) -> Vec<PresetInfo> {
    let text = |at: usize| -> Option<(String, usize)> {
        let n = u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?) as usize;
        if n > 4096 {
            return None;
        }
        let raw = d.get(at + 4..at + 4 + 2 * n)?;
        let units: Vec<u16> = raw.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string();
        Some((s, 4 + 2 * n))
    };
    // `KeyyUntF` + a 4-byte unit + a big-endian f64.
    let unit_float = |at: usize, key: &[u8; 4]| -> Option<f32> {
        if !d.get(at..)?.starts_with(key) || d.get(at + 4..at + 8)? != b"UntF" {
            return None;
        }
        let v = f64::from_be_bytes(d.get(at + 12..at + 20)?.try_into().ok()?);
        v.is_finite().then_some(v as f32)
    };
    let mut out = Vec::new();
    let mut cur = PresetInfo::default();
    let mut i = 0;
    // Past a preset's `dualBrush` key, tips and values belong to the
    // second brush, not the preset's own.
    let mut dual = false;
    while i + 8 <= d.len() {
        let rest = &d[i..];
        if rest.starts_with(b"brushPreset") {
            if cur != PresetInfo::default() {
                out.push(std::mem::take(&mut cur));
            }
            dual = false;
            i += 11;
            continue;
        }
        if rest.starts_with(b"dualBrush") {
            dual = true;
            i += 9;
            continue;
        }
        if dual {
            i += 1;
            continue;
        }
        if rest.starts_with(b"computedBrush") {
            cur.computed = true;
            i += 13;
            continue;
        }
        if rest.starts_with(b"Nm  TEXT") {
            if let Some((s, used)) = text(i + 8) {
                cur.name.get_or_insert(s);
                i += 8 + used;
                continue;
            }
        }
        if rest.starts_with(b"sampledDataTEXT") {
            if let Some((key, used)) = text(i + 15) {
                cur.key.get_or_insert(key);
                i += 15 + used;
                continue;
            }
        }
        let slots: [(&[u8; 4], &mut Option<f32>); 5] = [
            (b"Dmtr", &mut cur.diameter),
            (b"Angl", &mut cur.angle),
            (b"Rndn", &mut cur.roundness),
            (b"Spcn", &mut cur.spacing),
            (b"Hrdn", &mut cur.hardness),
        ];
        let mut hit = false;
        for (key, slot) in slots {
            if let Some(v) = unit_float(i, key) {
                slot.get_or_insert(v);
                hit = true;
                break;
            }
        }
        i += if hit { 20 } else { 1 };
    }
    if cur != PresetInfo::default() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    fn be32(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    /// Bounds + depth + compression + data for a w×h image.
    fn image_bytes(w: u32, h: u32, depth: u16, compression: u8, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [0, 0, h, w] {
            v.extend(be32(x));
        }
        v.extend(be16(depth));
        v.push(compression);
        v.extend(data);
        v
    }

    fn ucs2(s: &str) -> Vec<u8> {
        let mut v = be32(s.encode_utf16().count() as u32 + 1).to_vec();
        for u in s.encode_utf16().chain([0]) {
            v.extend(be16(u));
        }
        v
    }

    fn v12_entry(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut v = be16(kind).to_vec();
        v.extend(be32(body.len() as u32));
        v.extend(body);
        v
    }

    #[test]
    fn version_1_reads_computed_and_raw_sampled_brushes() {
        let mut computed = be32(0).to_vec();
        for x in [25u16, 19, 50, (-30i16) as u16, 80] {
            computed.extend(be16(x)); // spacing, diameter, roundness, angle, hardness
        }
        let mut sampled = be32(0).to_vec();
        sampled.extend(be16(40)); // spacing
        sampled.push(1); // antialiasing
        sampled.extend([0u8; 8]);
        sampled.extend(image_bytes(3, 2, 8, 0, &[0, 128, 255, 255, 64, 0]));
        let mut file = be16(1).to_vec();
        file.extend(be16(2));
        file.extend(v12_entry(1, &computed));
        file.extend(v12_entry(2, &sampled));
        let set = parse(&file).unwrap();
        assert!(set.warnings.is_empty(), "{:?}", set.warnings);
        assert_eq!(
            set.brushes[0],
            AbrBrush {
                name: "Round 19".into(),
                spacing: Some(0.25),
                diameter: None,
                shape: AbrShape::Computed {
                    diameter: 19.0,
                    hardness: 0.8,
                    angle: -30.0,
                    roundness: 0.5,
                },
            }
        );
        assert_eq!(
            set.brushes[1],
            AbrBrush {
                name: "Sampled brush 2".into(),
                spacing: Some(0.4),
                diameter: None,
                shape: AbrShape::Sampled {
                    width: 3,
                    height: 2,
                    depth: 8,
                    gray: vec![0, 128 * 257, 65535, 65535, 64 * 257, 0],
                },
            }
        );
    }

    #[test]
    fn version_2_reads_names_and_packbits_rows() {
        // Row 1: a run of four 200s (-3 → 4 copies); row 2: two literals
        // then a run of two 9s.
        let rows = [vec![0xFDu8, 200], vec![0x01, 7, 8, 0xFF, 9]];
        let mut data = Vec::new();
        for r in &rows {
            data.extend(be16(r.len() as u16));
        }
        for r in &rows {
            data.extend(r);
        }
        let mut sampled = be32(0).to_vec();
        sampled.extend(be16(10));
        sampled.extend(ucs2("Grainy ✓"));
        sampled.push(0);
        sampled.extend([0u8; 8]);
        sampled.extend(image_bytes(4, 2, 8, 1, &data));
        let mut file = be16(2).to_vec();
        file.extend(be16(1));
        file.extend(v12_entry(2, &sampled));
        let set = parse(&file).unwrap();
        assert_eq!(set.brushes[0].name, "Grainy ✓");
        assert_eq!(set.brushes[0].spacing, Some(0.1));
        let AbrShape::Sampled { gray, .. } = &set.brushes[0].shape else {
            panic!("sampled")
        };
        let bytes: Vec<u16> = [200, 200, 200, 200, 7, 8, 9, 9]
            .iter()
            .map(|&v| v * 257)
            .collect();
        assert_eq!(gray, &bytes);
    }

    fn section(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = b"8BIM".to_vec();
        v.extend(key);
        v.extend(be32(data.len() as u32));
        v.extend(data);
        v
    }

    /// A descriptor unit float: 4-byte key length 0, key, `UntF`, unit, f64.
    fn unit_float(key: &[u8; 4], unit: &[u8; 4], v: f64) -> Vec<u8> {
        let mut out = vec![0u8; 4];
        out.extend(key);
        out.extend(b"UntF");
        out.extend(unit);
        out.extend(v.to_be_bytes());
        out
    }

    fn samp_entry(key: &str, sub: u16, image: &[u8]) -> Vec<u8> {
        let mut body = vec![key.len() as u8];
        body.extend(key.as_bytes());
        body.extend(vec![0u8; if sub == 1 { 10 } else { 264 }]);
        body.extend(image);
        let mut v = be32(body.len() as u32).to_vec();
        v.extend(&body);
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    #[test]
    fn version_6_reads_8_and_16_bit_tips_with_descriptor_names() {
        let raw8 = image_bytes(2, 2, 8, 0, &[255, 0, 0, 255]);
        // 16-bit, PackBits: one row of two values 0x1234, 0xFFFF as a
        // 4-byte literal run.
        let mut rle16 = be16(5).to_vec();
        rle16.extend([0x03, 0x12, 0x34, 0xFF, 0xFF]);
        let img16 = image_bytes(2, 1, 16, 1, &rle16);
        let mut samp = samp_entry("uuid-a", 1, &raw8);
        samp.extend(samp_entry("uuid-b", 1, &img16));
        // A descriptor laid out as Photoshop writes it: a preset's name,
        // then its sampled brush with its own (tip) name, diameter,
        // spacing and the tip key; then a computed brush preset.
        let mut desc =
            b"\0\0\0\x10null....BrshVlLs\0\0\0\x02Objc\0\0\0\x01\0\0\0\0\0\0\0\x0BbrushPreset".to_vec();
        desc.extend(b"\0\0\0\x0E\0\0\0\0Nm  TEXT");
        desc.extend(ucs2("Charcoal 12"));
        desc.extend(b"\0\0\0\0BrshObjc\0\0\0\x01\0\0\0\0\0\0\0\x0CsampledBrush\0\0\0\x09");
        desc.extend(unit_float(b"Dmtr", b"#Pxl", 61.0));
        desc.extend(b"\0\0\0\0Nm  TEXT");
        desc.extend(ucs2("Charcoal"));
        desc.extend(unit_float(b"Spcn", b"#Prc", 10.0));
        desc.extend(b"\0\0\0\x0BsampledDataTEXT");
        desc.extend(ucs2("uuid-b"));
        desc.extend(b"Objc\0\0\0\x01\0\0\0\0\0\0\0\x0BbrushPreset\0\0\0\x02\0\0\0\0Nm  TEXT");
        desc.extend(ucs2("Hard oval"));
        desc.extend(b"\0\0\0\0BrshObjc\0\0\0\x01\0\0\0\0\0\0\0\x0DcomputedBrush\0\0\0\x05");
        desc.extend(unit_float(b"Dmtr", b"#Pxl", 30.0));
        desc.extend(unit_float(b"Hrdn", b"#Prc", 80.0));
        desc.extend(unit_float(b"Angl", b"#Ang", -15.0));
        desc.extend(unit_float(b"Rndn", b"#Prc", 50.0));
        desc.extend(unit_float(b"Spcn", b"#Prc", 25.0));
        // Its dual brush is sampled: that tip and size are not the preset's.
        desc.extend(b"\0\0\0\x09dualBrushObjc\0\0\0\x01\0\0\0\0\0\0\0\x09dualBrush\0\0\0\x02");
        desc.extend(unit_float(b"Dmtr", b"#Pxl", 99.0));
        desc.extend(b"\0\0\0\x0BsampledDataTEXT");
        desc.extend(ucs2("uuid-a"));
        let mut file = be16(6).to_vec();
        file.extend(be16(1));
        file.extend(section(b"samp", &samp));
        file.extend(section(b"patt", &[]));
        file.extend(section(b"desc", &desc));
        let set = parse(&file).unwrap();
        assert_eq!(set.brushes.len(), 3);
        assert_eq!(set.brushes[0].name, "Sampled brush 1");
        assert_eq!((set.brushes[0].spacing, set.brushes[0].diameter), (None, None));
        assert_eq!(set.brushes[1].spacing, Some(0.1));
        assert_eq!(set.brushes[1].diameter, Some(61.0));
        assert_eq!(
            set.brushes[2],
            AbrBrush {
                name: "Hard oval".into(),
                spacing: Some(0.25),
                diameter: None,
                shape: AbrShape::Computed {
                    diameter: 30.0,
                    hardness: 0.8,
                    angle: -15.0,
                    roundness: 0.5,
                },
            }
        );
        // A descriptor-only set of computed brushes reads too.
        let mut only = be16(6).to_vec();
        only.extend(be16(2));
        only.extend(section(b"desc", &desc));
        let only = parse(&only).unwrap();
        assert_eq!(only.brushes.len(), 1);
        assert_eq!(only.brushes[0].name, "Hard oval");
        assert_eq!(
            set.brushes[0].shape,
            AbrShape::Sampled {
                width: 2,
                height: 2,
                depth: 8,
                gray: vec![65535, 0, 0, 65535],
            }
        );
        // The preset's name, not the tip's own inside it.
        assert_eq!(set.brushes[1].name, "Charcoal 12");
        assert_eq!(
            set.brushes[1].shape,
            AbrShape::Sampled {
                width: 2,
                height: 1,
                depth: 16,
                gray: vec![0x1234, 0xFFFF],
            }
        );
    }

    #[test]
    fn version_10_subversion_2_skips_its_longer_header() {
        let img = image_bytes(1, 1, 8, 0, &[77]);
        let mut file = be16(10).to_vec();
        file.extend(be16(2));
        file.extend(section(b"samp", &samp_entry("k", 2, &img)));
        let set = parse(&file).unwrap();
        let AbrShape::Sampled { gray, .. } = &set.brushes[0].shape else {
            panic!("sampled")
        };
        assert_eq!(gray, &vec![77 * 257]);
    }

    #[test]
    fn unsupported_and_broken_files_say_why() {
        let msg = |b: &[u8]| parse(b).unwrap_err().to_string();
        assert_eq!(
            msg(&[0, 3, 0, 0]),
            "codec error: ABR: version 3 brush sets are not supported (only versions 1, 2, 6, 7 and 10)"
        );
        assert!(msg(&[0]).contains("too short"));
        assert!(msg(&[0, 6, 0, 3]).contains("version 6.3"));
        let mut no_samp = be16(6).to_vec();
        no_samp.extend(be16(2));
        no_samp.extend(section(b"desc", b"xx"));
        assert!(msg(&no_samp).contains("no sampled tips"));
        // A 32-bit tip is skipped with a warning; its neighbour loads.
        let mut samp = samp_entry("a", 1, &image_bytes(1, 1, 32, 0, &[0; 4]));
        samp.extend(samp_entry("b", 1, &image_bytes(1, 1, 8, 0, &[9])));
        let mut file = be16(6).to_vec();
        file.extend(be16(1));
        file.extend(section(b"samp", &samp));
        let set = parse(&file).unwrap();
        assert_eq!(set.brushes.len(), 1);
        assert_eq!(
            set.warnings,
            vec!["tip 1: 32-bit tips are not supported (only 8 and 16); skipped"]
        );
        // Nothing readable at all is an error carrying the reason.
        let only_bad = {
            let mut f = be16(6).to_vec();
            f.extend(be16(1));
            f.extend(section(
                b"samp",
                &samp_entry("a", 1, &image_bytes(0, 5, 8, 0, &[])),
            ));
            f
        };
        assert!(msg(&only_bad).contains("no brushes could be read: tip 1: a tip of 0×5 px is out of range"));
        // A decompression bomb: an 8000×8000 16-bit tip whose PackBits
        // rows are all empty is refused before anything is allocated.
        let bomb = {
            let mut f = be16(6).to_vec();
            f.extend(be16(1));
            let img = image_bytes(8000, 8000, 16, 1, &[0u8; 16_000]);
            f.extend(section(b"samp", &samp_entry("a", 1, &img)));
            f
        };
        assert!(
            msg(&bomb).contains("a compressed row is too short"),
            "{}",
            msg(&bomb)
        );
        // Wider than the engine takes.
        let wide = {
            let mut f = be16(6).to_vec();
            f.extend(be16(1));
            f.extend(section(
                b"samp",
                &samp_entry("a", 1, &image_bytes(9000, 1, 8, 0, &[])),
            ));
            f
        };
        assert!(msg(&wide).contains("9000×1 px is out of range"), "{}", msg(&wide));
        // Truncated image data.
        let mut short = be16(1).to_vec();
        short.extend(be16(1));
        let mut body = be32(0).to_vec();
        body.extend(be16(25));
        body.push(0);
        body.extend([0u8; 8]);
        body.extend(image_bytes(4, 4, 8, 0, &[1, 2, 3]));
        short.extend(v12_entry(2, &body));
        assert!(msg(&short).contains("truncated"));
    }
}
