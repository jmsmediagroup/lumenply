//! Adobe Photoshop (`.psd`) import and export, 8-bit RGB.
//!
//! Supported both ways: pixel layers with their bounds and alpha, layer
//! groups (section dividers), layer masks, opacity, visibility, blend modes
//! (mapped to the closest of ours), Unicode layer names, and a flattened
//! composite so other programs show a preview. Adjustment layers are not a
//! PSD concept we model yet: on export they are baked into nothing (skipped,
//! with a warning returned to the caller); on import Photoshop's own
//! adjustment layers are skipped the same way.
//!
//! Layout written: header → empty colour-mode data → image resources (just
//! the guides, resource 1032, when there are any; see `psd_guides`) →
//! layer-and-mask section (RLE channels) → RLE composite.

use std::path::Path;

use lumenply_doc::{Adjustment, BlendMode, Document, Layer, LayerContent, LayerLocks, Mask};
use lumenply_tiles::{Raster, Rect, TileStore};

use crate::{linear_to_srgb, IoError};

mod color_modes;
mod extra;
mod masks;
mod shape;

#[derive(Debug, thiserror::Error)]
pub enum PsdError {
    #[error("not a PSD file: {0}")]
    NotPsd(String),
    #[error("unsupported PSD: {0}")]
    Unsupported(String),
    #[error("corrupt PSD: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] IoError),
}

impl From<std::io::Error> for PsdError {
    fn from(e: std::io::Error) -> Self {
        PsdError::Io(IoError::Io(e))
    }
}

/// Result of an import or export plus anything that could not be carried over.
pub struct Report<T> {
    pub value: T,
    pub warnings: Vec<String>,
}

// ---- blend mode keys -----------------------------------------------------------

fn blend_key(m: BlendMode) -> &'static [u8; 4] {
    match m {
        BlendMode::Normal => b"norm",
        BlendMode::Multiply => b"mul ",
        BlendMode::Screen => b"scrn",
        BlendMode::Overlay => b"over",
        BlendMode::Darken => b"dark",
        BlendMode::Lighten => b"lite",
        BlendMode::Difference => b"diff",
        BlendMode::Add => b"lddg",
        BlendMode::HardLight => b"hLit",
        BlendMode::SoftLight => b"sLit",
    }
}

fn blend_from_key(k: &[u8]) -> (BlendMode, bool) {
    match k {
        b"norm" | b"pass" => (BlendMode::Normal, true),
        b"mul " => (BlendMode::Multiply, true),
        b"scrn" => (BlendMode::Screen, true),
        b"over" => (BlendMode::Overlay, true),
        b"dark" => (BlendMode::Darken, true),
        b"lite" => (BlendMode::Lighten, true),
        b"diff" => (BlendMode::Difference, true),
        b"lddg" => (BlendMode::Add, true),
        b"hLit" => (BlendMode::HardLight, true),
        b"sLit" => (BlendMode::SoftLight, true),
        _ => (BlendMode::Normal, false),
    }
}

// ---- byte helpers ----------------------------------------------------------------

struct Rd<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Rd { buf, pos: 0 }
    }
    fn need(&self, n: usize) -> Result<(), PsdError> {
        if self.pos + n > self.buf.len() {
            Err(PsdError::Corrupt(format!(
                "unexpected end of file at {}",
                self.pos
            )))
        } else {
            Ok(())
        }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], PsdError> {
        self.need(n)?;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, PsdError> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, PsdError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16, PsdError> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, PsdError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, PsdError> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64, PsdError> {
        let b = self.bytes(8)?;
        Ok(u64::from_be_bytes(b.try_into().expect("8 bytes")))
    }
    /// A section length: 4 bytes in PSD, 8 in PSB.
    fn len_of(&mut self, psb: bool) -> Result<usize, PsdError> {
        if psb {
            let v = self.u64()?;
            usize::try_from(v).map_err(|_| PsdError::Corrupt(format!("section length {v} overflows")))
        } else {
            Ok(self.u32()? as usize)
        }
    }
    fn skip(&mut self, n: usize) -> Result<(), PsdError> {
        self.need(n)?;
        self.pos += n;
        Ok(())
    }
}

fn put_u16(w: &mut Vec<u8>, v: u16) {
    w.extend_from_slice(&v.to_be_bytes());
}
fn put_i32(w: &mut Vec<u8>, v: i32) {
    w.extend_from_slice(&v.to_be_bytes());
}
fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_be_bytes());
}

/// PackBits (RLE) encode one scanline.
fn packbits(row: &[u8], out: &mut Vec<u8>) {
    let n = row.len();
    let mut i = 0;
    while i < n {
        // Run of identical bytes?
        let mut run = 1;
        while i + run < n && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 2 {
            out.push((257 - run as i32) as u8);
            out.push(row[i]);
            i += run;
            continue;
        }
        // Literal run until the next repeat of length >= 3 (or 128 bytes).
        let start = i;
        let mut lit = 0;
        while i < n && lit < 128 {
            if i + 2 < n && row[i] == row[i + 1] && row[i] == row[i + 2] {
                break;
            }
            i += 1;
            lit += 1;
        }
        out.push((lit - 1) as u8);
        out.extend_from_slice(&row[start..start + lit]);
    }
}

fn unpackbits(src: &[u8], expected: usize) -> Result<Vec<u8>, PsdError> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < src.len() && out.len() < expected {
        let h = src[i] as i8;
        i += 1;
        if h >= 0 {
            let n = h as usize + 1;
            if i + n > src.len() {
                return Err(PsdError::Corrupt("RLE literal overruns row".into()));
            }
            out.extend_from_slice(&src[i..i + n]);
            i += n;
        } else if h != -128 {
            let n = (1 - h as i32) as usize;
            if i >= src.len() {
                return Err(PsdError::Corrupt("RLE repeat missing byte".into()));
            }
            out.extend(std::iter::repeat_n(src[i], n));
            i += 1;
        }
    }
    if out.len() < expected {
        return Err(PsdError::Corrupt(format!(
            "RLE row too short: {} of {}",
            out.len(),
            expected
        )));
    }
    out.truncate(expected);
    Ok(out)
}

/// Encode a channel (rows of `w` bytes) as PSD RLE: row byte counts, then data.
fn encode_channel_rle(plane: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut counts = Vec::with_capacity(h * 2);
    let mut data = Vec::new();
    for y in 0..h {
        let start = data.len();
        packbits(&plane[y * w..(y + 1) * w], &mut data);
        put_u16(&mut counts, (data.len() - start) as u16);
    }
    let mut out = Vec::with_capacity(2 + counts.len() + data.len());
    put_u16(&mut out, 1); // compression = RLE
    out.extend_from_slice(&counts);
    out.extend_from_slice(&data);
    out
}

/// Raw plane bytes → samples scaled to the full u16 range (8-bit values
/// multiply by 257, so 255 → 65535 exactly; 16-bit values are big-endian).
fn bytes_to_samples(bytes: &[u8], depth_bytes: usize) -> Vec<u16> {
    if depth_bytes == 4 {
        // 32-bit floats (transparency, masks): clamped to 0..1.
        return bytes
            .chunks_exact(4)
            .map(|p| {
                let v = f32::from_be_bytes([p[0], p[1], p[2], p[3]]);
                (if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } * 65535.0).round() as u16
            })
            .collect();
    }
    if depth_bytes == 2 {
        bytes
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect()
    } else {
        bytes.iter().map(|&b| b as u16 * 257).collect()
    }
}

/// Inflate a zlib stream to at most `cap` bytes (a lying stream must not
/// size an unbounded allocation).
fn inflate(src: &[u8], cap: usize) -> Result<Vec<u8>, PsdError> {
    use std::io::Read;
    let mut out = Vec::with_capacity(cap.min(1 << 20));
    let mut dec = flate2::read::ZlibDecoder::new(src).take(cap as u64 + 1);
    dec.read_to_end(&mut out)
        .map_err(|e| PsdError::Corrupt(format!("zip channel: {e}")))?;
    if out.len() > cap {
        return Err(PsdError::Corrupt("zip channel inflates past its plane".into()));
    }
    Ok(out)
}

fn decode_channel(
    rd: &mut Rd,
    w: usize,
    h: usize,
    depth_bytes: usize,
    psb: bool,
) -> Result<Vec<u16>, PsdError> {
    if depth_bytes == 4 {
        // 32-bit transparency and masks: floats, clamped to 0..1.
        let bytes = color_modes::decode_plane_bytes(rd, w, h, 32, psb)?;
        return Ok(bytes_to_samples(&bytes, 4));
    }
    let compression = rd.u16()?;
    let plane_len = w * h * depth_bytes;
    match compression {
        0 => Ok(bytes_to_samples(rd.bytes(plane_len)?, depth_bytes)),
        1 => {
            let mut counts = Vec::with_capacity(h);
            for _ in 0..h {
                // RLE row byte counts are 2 bytes in PSD, 4 in PSB.
                counts.push(if psb {
                    rd.u32()? as usize
                } else {
                    rd.u16()? as usize
                });
            }
            // Don't let a lying header size the allocation: RLE cannot
            // expand the remaining input by more than 128×.
            let remaining = rd.buf.len().saturating_sub(rd.pos);
            let mut out = Vec::with_capacity(plane_len.min(remaining.saturating_mul(128)));
            for c in counts {
                let row = rd.bytes(c)?;
                out.extend(unpackbits(row, w * depth_bytes)?);
            }
            Ok(bytes_to_samples(&out, depth_bytes))
        }
        // ZIP, without (2) and with (3) per-row delta prediction —
        // Photoshop's usual coding for 16-bit layer channels.
        2 | 3 => {
            let rest = &rd.buf[rd.pos..];
            rd.pos = rd.buf.len();
            let bytes = inflate(rest, plane_len)?;
            if bytes.len() != plane_len {
                return Err(PsdError::Corrupt(format!(
                    "zip channel inflated to {} of {plane_len} bytes",
                    bytes.len()
                )));
            }
            let mut samples = bytes_to_samples(&bytes, depth_bytes);
            if compression == 3 {
                // Undo the delta coding along each row, on the sample type
                // the file stores (bytes for 8-bit, u16 for 16-bit).
                for row in samples.chunks_exact_mut(w) {
                    for i in 1..row.len() {
                        if depth_bytes == 2 {
                            row[i] = row[i].wrapping_add(row[i - 1]);
                        } else {
                            let v = ((row[i] / 257) as u8).wrapping_add((row[i - 1] / 257) as u8);
                            row[i] = v as u16 * 257;
                        }
                    }
                }
            }
            Ok(samples)
        }
        other => Err(PsdError::Unsupported(format!("compression method {other}"))),
    }
}

fn pad_even(n: usize) -> usize {
    n + (n & 1)
}

// ---- export ----------------------------------------------------------------------------

/// Flatten a layer's pixels (sRGB 8-bit, straight alpha) over its content bounds.
fn layer_planes(store: &TileStore) -> Option<(Rect, [Vec<u8>; 4])> {
    let b = store.content_bounds()?;
    let r = store.to_raster(b);
    let n = (b.w * b.h) as usize;
    let mut planes = [vec![0u8; n], vec![0u8; n], vec![0u8; n], vec![0u8; n]];
    for (i, p) in r.pixels.iter().enumerate() {
        let [cr, cg, cb, a] = p.to_straight();
        planes[0][i] = linear_to_srgb(cr);
        planes[1][i] = linear_to_srgb(cg);
        planes[2][i] = linear_to_srgb(cb);
        planes[3][i] = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    Some((b, planes))
}

/// 16-bit variant of [`layer_planes`]: sRGB u16 samples, raw-encoded later.
fn layer_planes16(store: &TileStore) -> Option<(Rect, [Vec<u16>; 4])> {
    let b = store.content_bounds()?;
    let r = store.to_raster(b);
    let n = (b.w * b.h) as usize;
    let mut planes = [vec![0u16; n], vec![0u16; n], vec![0u16; n], vec![0u16; n]];
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    for (i, p) in r.pixels.iter().enumerate() {
        let [cr, cg, cb, a] = p.to_straight();
        planes[0][i] = q(lumenply_doc::adjust::srgb_encode(cr));
        planes[1][i] = q(lumenply_doc::adjust::srgb_encode(cg));
        planes[2][i] = q(lumenply_doc::adjust::srgb_encode(cb));
        planes[3][i] = q(a);
    }
    Some((b, planes))
}

/// Encode a 16-bit plane as a raw (compression 0) channel.
fn encode_channel_raw16(plane: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + plane.len() * 2);
    put_u16(&mut out, 0); // compression = raw
    for &v in plane {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

fn mask_plane(mask: &Mask, canvas: Rect) -> (Rect, Vec<u8>, u8) {
    // Masks are written over the canvas; pixels outside painted tiles take the default.
    let r = canvas;
    let mut plane = vec![0u8; (r.w * r.h) as usize];
    for y in 0..r.h as i32 {
        for x in 0..r.w as i32 {
            plane[(y as u32 * r.w + x as u32) as usize] =
                (mask.value(r.x + x, r.y + y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    let default = (mask.default.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    (r, plane, default)
}

struct LayerRecordOut {
    record: Vec<u8>,
    channels: Vec<Vec<u8>>, // channel image data, one per channel in record order
}

fn pascal_name(name: &str) -> Vec<u8> {
    let bytes: Vec<u8> = name.bytes().take(255).collect();
    let mut out = vec![bytes.len() as u8];
    out.extend_from_slice(&bytes);
    // Pascal string padded to a multiple of 4 (including the length byte).
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

fn unicode_name_block(name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut data = Vec::new();
    put_u32(&mut data, units.len() as u32);
    for u in units {
        put_u16(&mut data, u);
    }
    additional_block(b"luni", &data)
}

/// Photoshop's layer protection flags (the `lspf` block): bit 0 locks
/// transparent pixels, bit 1 image pixels, bit 2 position; bit 31 is
/// "lock all".
const LSPF_TRANSPARENCY: u32 = 1;
const LSPF_PIXELS: u32 = 2;
const LSPF_POSITION: u32 = 4;
const LSPF_ALL: u32 = 0x8000_0000;

fn lock_flags(l: &LayerLocks) -> u32 {
    let mut f = 0;
    for (on, bit) in [
        (l.transparency, LSPF_TRANSPARENCY),
        (l.pixels, LSPF_PIXELS),
        (l.position, LSPF_POSITION),
        (l.all, LSPF_ALL),
    ] {
        if on {
            f |= bit;
        }
    }
    f
}

fn locks_from_flags(f: u32) -> LayerLocks {
    LayerLocks {
        transparency: f & LSPF_TRANSPARENCY != 0,
        pixels: f & LSPF_PIXELS != 0,
        position: f & LSPF_POSITION != 0,
        all: f & LSPF_ALL != 0,
    }
}

/// The `lspf` block for a locked layer (none for an unlocked one).
fn lock_blocks(l: &Layer) -> Vec<Vec<u8>> {
    if l.locks.is_empty() {
        return Vec::new();
    }
    vec![additional_block(b"lspf", &lock_flags(&l.locks).to_be_bytes())]
}

fn additional_block(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(key);
    // The length counts the pad byte ("rounded up to an even byte
    // count"); readers such as psd-tools rely on it.
    put_u32(&mut out, (data.len() + data.len() % 2) as u32);
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn i16_of(v: f32, scale: f32, lo: i32, hi: i32) -> i16 {
    ((v * scale).round() as i32).clamp(lo, hi) as i16
}

/// Encode an adjustment as Photoshop's tagged block, if the type has a
/// documented binary layout (descriptor-only types are not handled).
fn adjustment_block(adj: &Adjustment) -> Option<Vec<u8>> {
    let mut d = Vec::new();
    let key: &[u8; 4] = match adj {
        Adjustment::Invert => b"nvrt",
        Adjustment::Threshold { level } => {
            put_u16(&mut d, i16_of(*level, 255.0, 1, 255) as u16);
            put_u16(&mut d, 0);
            b"thrs"
        }
        Adjustment::Posterize { levels } => {
            put_u16(&mut d, (*levels).clamp(2, 255) as u16);
            put_u16(&mut d, 0);
            b"post"
        }
        Adjustment::BrightnessContrast { brightness, contrast } => {
            d.extend_from_slice(&i16_of(*brightness, 100.0, -100, 100).to_be_bytes());
            d.extend_from_slice(&i16_of(*contrast, 100.0, -100, 100).to_be_bytes());
            d.extend_from_slice(&0i16.to_be_bytes()); // mean value
            d.push(0); // lab colour only
            d.push(0);
            b"brit"
        }
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
            colorize,
        } => {
            put_u16(&mut d, 2); // version
            d.push(*colorize as u8);
            d.push(0);
            // Colorization hue/sat/light, then the master hue/sat/light.
            let light = i16_of(*lightness, 100.0, -100, 100);
            let (colorization, master) = if *colorize {
                (
                    [
                        i16_of(hue.rem_euclid(360.0), 1.0, 0, 360),
                        i16_of(*saturation, 100.0, 0, 100),
                        light,
                    ],
                    [0, 0, 0],
                )
            } else {
                (
                    [0, 25, 0],
                    [
                        i16_of(*hue, 1.0, -180, 180),
                        i16_of(*saturation, 100.0, -100, 100),
                        light,
                    ],
                )
            };
            for v in colorization.into_iter().chain(master) {
                d.extend_from_slice(&v.to_be_bytes());
            }
            // Six hextant records: ranges then settings (all neutral).
            let ranges: [[i16; 4]; 6] = [
                [315, 345, 15, 45],
                [15, 45, 75, 105],
                [75, 105, 135, 165],
                [135, 165, 195, 225],
                [195, 225, 255, 285],
                [255, 285, 315, 345],
            ];
            for r in ranges {
                for v in r {
                    d.extend_from_slice(&v.to_be_bytes());
                }
                for _ in 0..3 {
                    d.extend_from_slice(&0i16.to_be_bytes());
                }
            }
            b"hue2"
        }
        Adjustment::Levels {
            in_black,
            in_white,
            gamma,
            out_black,
            out_white,
            channels,
        } => {
            put_u16(&mut d, 2); // version
            let rec = |d: &mut Vec<u8>, ib: i16, iw: i16, ob: i16, ow: i16, g: i16| {
                for v in [ib, iw, ob, ow, g] {
                    d.extend_from_slice(&v.to_be_bytes());
                }
            };
            rec(
                &mut d,
                i16_of(*in_black, 255.0, 0, 253),
                i16_of(*in_white, 255.0, 2, 255),
                i16_of(*out_black, 255.0, 0, 255),
                i16_of(*out_white, 255.0, 0, 255),
                i16_of(*gamma, 100.0, 10, 999),
            );
            // Records 1-3 are the R, G and B channels.
            for ch in channels {
                rec(
                    &mut d,
                    i16_of(ch.in_black, 255.0, 0, 253),
                    i16_of(ch.in_white, 255.0, 2, 255),
                    i16_of(ch.out_black, 255.0, 0, 255),
                    i16_of(ch.out_white, 255.0, 0, 255),
                    i16_of(ch.gamma, 100.0, 10, 999),
                );
            }
            for _ in 4..29 {
                rec(&mut d, 0, 255, 0, 255, 100);
            }
            b"levl"
        }
        Adjustment::Curves { points, channels } => {
            // Channel 0 is the composite, 1-3 are R, G and B; straight
            // channel curves are left out.
            let mut curves: Vec<(u16, Vec<[f32; 2]>)> = vec![(0, points.clone())];
            for (i, c) in channels.iter().enumerate() {
                if !lumenply_doc::adjust::curve_is_identity(c) {
                    curves.push((i as u16 + 1, c.clone()));
                }
            }
            for (_, pts) in &mut curves {
                if pts.is_empty() {
                    *pts = vec![[0.0, 0.0], [1.0, 1.0]];
                }
            }
            let write_points = |d: &mut Vec<u8>, pts: &[[f32; 2]]| {
                put_u16(d, pts.len().min(19) as u16);
                for p in pts.iter().take(19) {
                    put_u16(d, i16_of(p[1], 255.0, 0, 255) as u16); // output
                    put_u16(d, i16_of(p[0], 255.0, 0, 255) as u16); // input
                }
            };
            d.push(0); // not a lookup map
            put_u16(&mut d, 1); // version
            put_u32(&mut d, curves.iter().fold(0, |m, (id, _)| m | 1 << id));
            for (_, pts) in &curves {
                write_points(&mut d, pts);
            }
            // Trailing "Crv " section that newer readers expect.
            d.extend_from_slice(b"Crv ");
            put_u16(&mut d, 4);
            put_u32(&mut d, curves.len() as u32);
            for (id, pts) in &curves {
                put_u16(&mut d, *id);
                write_points(&mut d, pts);
            }
            b"curv"
        }
        Adjustment::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => {
            for tone in [shadows, midtones, highlights] {
                for v in tone {
                    d.extend_from_slice(&i16_of(*v, 100.0, -100, 100).to_be_bytes());
                }
            }
            d.push(u8::from(*preserve_luminosity));
            d.push(0);
            b"blnc"
        }
        Adjustment::Exposure {
            exposure,
            offset,
            gamma,
        } => {
            put_u16(&mut d, 1); // version
            for v in [*exposure, *offset, *gamma] {
                d.extend_from_slice(&v.to_be_bytes());
            }
            b"expA"
        }
        Adjustment::Vibrance { vibrance, saturation } => {
            put_u32(&mut d, 16); // descriptor version
            put_u32(&mut d, 1); // unicode name: one code unit (the terminator)
            put_u16(&mut d, 0);
            desc_key(&mut d, b"null"); // class id
            put_u32(&mut d, 2); // item count
            desc_long(&mut d, b"vibrance", i16_of(*vibrance, 100.0, -100, 100) as i32);
            desc_long(&mut d, b"Strt", i16_of(*saturation, 100.0, -100, 100) as i32);
            b"vibA"
        }
        Adjustment::BlackWhite { red, green, blue } => {
            // Photoshop's six channel weights in percent; the in-between
            // channels interpolate ours so the grey mix stays close.
            let pc = |v: f32| (v.clamp(0.0, 3.0) * 100.0 + 0.5) as i32;
            put_u32(&mut d, 16); // descriptor version
            put_u32(&mut d, 1);
            put_u16(&mut d, 0);
            desc_key(&mut d, b"null");
            put_u32(&mut d, 8); // item count
            desc_long(&mut d, b"Rd  ", pc(*red));
            desc_long(&mut d, b"Yllw", pc((*red + *green) / 2.0));
            desc_long(&mut d, b"Grn ", pc(*green));
            desc_long(&mut d, b"Cyn ", pc((*green + *blue) / 2.0));
            desc_long(&mut d, b"Bl  ", pc(*blue));
            desc_long(&mut d, b"Mgnt", pc((*red + *blue) / 2.0));
            desc_bool(&mut d, b"useTint", false);
            desc_long(&mut d, b"bwPresetKind", 3); // custom
            b"blwh"
        }
        other => {
            let (key, data) = extra::adjustment_block(other)?;
            return Some(additional_block(key, &data));
        }
    };
    Some(additional_block(key, &d))
}

fn desc_key(d: &mut Vec<u8>, key: &[u8]) {
    if key.len() == 4 {
        put_u32(d, 0);
    } else {
        put_u32(d, key.len() as u32);
    }
    d.extend_from_slice(key);
}

fn desc_long(d: &mut Vec<u8>, key: &[u8], v: i32) {
    desc_key(d, key);
    d.extend_from_slice(b"long");
    put_i32(d, v);
}

fn desc_bool(d: &mut Vec<u8>, key: &[u8], v: bool) {
    desc_key(d, key);
    d.extend_from_slice(b"bool");
    d.push(u8::from(v));
}

/// The modern Brightness/Contrast settings block (`CgEd`, a descriptor).
fn cged_block(brightness: i32, contrast: i32) -> Vec<u8> {
    let mut d = Vec::new();
    put_u32(&mut d, 16); // descriptor version
    put_u32(&mut d, 1); // unicode name: one code unit (the terminator)
    put_u16(&mut d, 0);
    desc_key(&mut d, b"null"); // class id
    put_u32(&mut d, 7); // item count
    desc_long(&mut d, b"Vrsn", 1);
    desc_long(&mut d, b"Brgh", brightness);
    desc_long(&mut d, b"Cntr", contrast);
    desc_long(&mut d, b"means", 127);
    desc_bool(&mut d, b"Lab ", false);
    desc_bool(&mut d, b"useLegacy", true);
    desc_bool(&mut d, b"Auto", false);
    additional_block(b"CgEd", &d)
}

fn desc_read_key(d: &mut Rd) -> Option<Vec<u8>> {
    let len = d.u32().ok()? as usize;
    let len = if len == 0 { 4 } else { len.min(256) };
    Some(d.bytes(len).ok()?.to_vec())
}

/// Walk one descriptor body (after the version word): collects `long`/
/// `bool`/`doub` items, skips `TEXT`/`enum`/`UntF` and nested `Objc`
/// descriptors (their numeric items land in the same flat list, which is
/// fine for the keys we look up), and gives up on anything else.
fn walk_descriptor(d: &mut Rd, out: &mut Vec<(Vec<u8>, f64)>, depth: usize) -> Option<()> {
    if depth > 8 {
        return None;
    }
    let n = d.u32().ok()? as usize; // unicode name length, in code units
    d.skip(n.min(1 << 20) * 2).ok()?;
    desc_read_key(d)?; // class id
    let count = d.u32().ok()? as usize;
    for _ in 0..count.min(1024) {
        let key = desc_read_key(d)?;
        let ty = d.bytes(4).ok()?.to_vec();
        match &ty[..] {
            b"long" => out.push((key, d.i32().ok()? as f64)),
            b"bool" => out.push((key, d.u8().ok()? as f64)),
            b"doub" => out.push((key, f64::from_be_bytes(d.bytes(8).ok()?.try_into().ok()?))),
            b"TEXT" => {
                let n = d.u32().ok()? as usize;
                d.skip(n.min(1 << 20) * 2).ok()?;
            }
            b"enum" => {
                desc_read_key(d)?;
                desc_read_key(d)?;
            }
            b"UntF" => {
                d.bytes(4).ok()?;
                d.bytes(8).ok()?;
            }
            b"Objc" => walk_descriptor(d, out, depth + 1)?,
            _ => return None,
        }
    }
    Some(())
}

/// Minimal descriptor reader: returns `(key, value)` for the numeric items
/// (`long`/`bool`/`doub`), or `None` if the structure uses a type the
/// walker doesn't know.
fn parse_simple_descriptor(data: &[u8]) -> Option<Vec<(Vec<u8>, f64)>> {
    let mut d = Rd::new(data);
    let version = d.u32().ok()?;
    if version != 16 {
        return None;
    }
    let mut out = Vec::new();
    walk_descriptor(&mut d, &mut out, 0)?;
    Some(out)
}

/// Decode a Photoshop adjustment block we understand.
fn parse_adjustment(key: &[u8], data: &[u8]) -> Result<Option<Adjustment>, PsdError> {
    let mut d = Rd::new(data);
    let f = |v: i16, scale: f32| v as f32 / scale;
    Ok(Some(match key {
        b"nvrt" => Adjustment::Invert,
        b"thrs" => Adjustment::Threshold {
            level: d.u16()? as f32 / 255.0,
        },
        b"post" => Adjustment::Posterize {
            levels: d.u16()?.clamp(2, 256) as u32,
        },
        b"brit" => {
            let b = d.i16()?;
            let c = d.i16()?;
            Adjustment::BrightnessContrast {
                brightness: f(b, 100.0),
                contrast: f(c, 100.0),
            }
        }
        b"CgEd" => {
            let Some(items) = parse_simple_descriptor(data) else {
                return Ok(None);
            };
            let get = |k: &[u8]| items.iter().find(|(key, _)| key == k).map(|(_, v)| *v as f32);
            Adjustment::BrightnessContrast {
                brightness: get(b"Brgh").unwrap_or(0.0) / 100.0,
                contrast: get(b"Cntr").unwrap_or(0.0) / 100.0,
            }
        }
        b"hue2" => {
            let version = d.u16()?;
            if version != 2 {
                return Ok(None);
            }
            let colorize = d.u8()? != 0;
            d.skip(1)?;
            let (ch, cs, cl) = (d.i16()?, d.i16()?, d.i16()?);
            let (h, s, l) = (d.i16()?, d.i16()?, d.i16()?);
            if colorize {
                Adjustment::HueSaturation {
                    hue: ch as f32,
                    saturation: f(cs, 100.0),
                    lightness: f(cl, 100.0),
                    colorize: true,
                }
            } else {
                Adjustment::HueSaturation {
                    hue: h as f32,
                    saturation: f(s, 100.0),
                    lightness: f(l, 100.0),
                    colorize: false,
                }
            }
        }
        b"levl" => {
            let version = d.u16()?;
            if version != 2 {
                return Ok(None);
            }
            let mut rec = || -> Result<[f32; 5], PsdError> {
                let ib = d.i16()?;
                let iw = d.i16()?;
                let ob = d.i16()?;
                let ow = d.i16()?;
                let g = d.i16()?;
                Ok([
                    f(ib, 255.0),
                    f(iw, 255.0),
                    f(g, 100.0),
                    f(ob, 255.0),
                    f(ow, 255.0),
                ])
            };
            let m = rec()?;
            // Records 1-3 are the R, G and B channels when present.
            let mut channels = [lumenply_doc::adjust::LevelsChannel::default(); 3];
            for ch in &mut channels {
                let Ok(v) = rec() else { break };
                *ch = lumenply_doc::adjust::LevelsChannel {
                    in_black: v[0],
                    in_white: v[1],
                    gamma: v[2],
                    out_black: v[3],
                    out_white: v[4],
                };
            }
            Adjustment::Levels {
                in_black: m[0],
                in_white: m[1],
                gamma: m[2],
                out_black: m[3],
                out_white: m[4],
                channels,
            }
        }
        b"curv" => {
            // The undocumented lookup-table form is not supported.
            if d.u8()? != 0 {
                return Ok(None);
            }
            if d.u16()? != 1 {
                return Ok(None);
            }
            let mask = d.u32()?;
            let read_points = |d: &mut Rd| -> Result<Vec<[f32; 2]>, PsdError> {
                let n = (d.u16()? as usize).min(256);
                let mut points = Vec::with_capacity(n);
                for _ in 0..n {
                    let out = d.u16()? as f32 / 255.0;
                    let input = d.u16()? as f32 / 255.0;
                    points.push([input, out]);
                }
                Ok(points)
            };
            // Channel 0 is the composite, 1-3 are R, G and B.
            let mut curves: [Vec<[f32; 2]>; 4] = Default::default();
            for id in 0..32u32 {
                if mask & (1 << id) != 0 {
                    let pts = read_points(&mut d)?;
                    if let Some(slot) = curves.get_mut(id as usize) {
                        *slot = pts;
                    }
                }
            }
            // The "Crv " section names each curve's channel explicitly.
            if d.pos + 10 <= data.len() && d.bytes(4)? == b"Crv " {
                let _version = d.u16()?;
                let count = d.u32()?.min(64);
                let mut extra: [Vec<[f32; 2]>; 4] = Default::default();
                let mut ok = true;
                for _ in 0..count {
                    let Ok(id) = d.u16() else {
                        ok = false;
                        break;
                    };
                    let Ok(pts) = read_points(&mut d) else {
                        ok = false;
                        break;
                    };
                    if let Some(slot) = extra.get_mut(id as usize) {
                        *slot = pts;
                    }
                }
                if ok {
                    curves = extra;
                }
            }
            let [points, r, g, b] = curves;
            Adjustment::Curves {
                points: if points.is_empty() {
                    vec![[0.0, 0.0], [1.0, 1.0]]
                } else {
                    points
                },
                channels: [r, g, b],
            }
        }
        b"blnc" => {
            let mut tones = [[0f32; 3]; 3];
            for tone in tones.iter_mut() {
                for v in tone.iter_mut() {
                    *v = f(d.i16()?, 100.0);
                }
            }
            let preserve = d.u8()? != 0;
            Adjustment::ColorBalance {
                shadows: tones[0],
                midtones: tones[1],
                highlights: tones[2],
                preserve_luminosity: preserve,
            }
        }
        b"expA" => {
            let version = d.u16()?;
            if version != 1 {
                return Ok(None);
            }
            let mut v = [0f32; 3];
            for x in &mut v {
                *x = f32::from_be_bytes(d.bytes(4)?.try_into().expect("4 bytes"));
            }
            if !v.iter().all(|x| x.is_finite()) {
                return Ok(None);
            }
            Adjustment::Exposure {
                exposure: v[0].clamp(-20.0, 20.0),
                offset: v[1].clamp(-0.5, 0.5),
                gamma: v[2].clamp(0.01, 10.0),
            }
        }
        b"vibA" => {
            let Some(items) = parse_simple_descriptor(data) else {
                return Ok(None);
            };
            let get = |k: &[u8]| items.iter().find(|(key, _)| key == k).map(|(_, v)| *v as f32);
            Adjustment::Vibrance {
                vibrance: (get(b"vibrance").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
                saturation: (get(b"Strt").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            }
        }
        b"blwh" => {
            let Some(items) = parse_simple_descriptor(data) else {
                return Ok(None);
            };
            let get = |k: &[u8]| items.iter().find(|(key, _)| key == k).map(|(_, v)| *v as f32);
            // Only the primary channels map onto our three weights.
            let w = |k: &[u8], dflt: f32| (get(k).map_or(dflt, |v| v / 100.0)).clamp(0.0, 3.0);
            Adjustment::BlackWhite {
                red: w(b"Rd  ", 0.4),
                green: w(b"Grn ", 0.4),
                blue: w(b"Bl  ", 0.2),
            }
        }
        other => return Ok(extra::parse_adjustment(other, data)),
    }))
}

fn section_divider_block(kind: u32, key: &[u8; 4]) -> Vec<u8> {
    let mut data = Vec::new();
    put_u32(&mut data, kind);
    data.extend_from_slice(b"8BIM");
    data.extend_from_slice(key);
    additional_block(b"lsct", &data)
}

/// Build one PSD layer record (and channel data) from a flat description.
#[allow(clippy::too_many_arguments)]
fn layer_record(
    name: &str,
    bounds: Rect,
    channels: Vec<(i16, Vec<u8>)>, // (channel id, encoded data)
    blend: BlendMode,
    pass_through: bool,
    clip: bool,
    opacity: f32,
    visible: bool,
    mask: Option<(Rect, Vec<u8>, u8)>, // (rect, encoded data, default colour)
    mask_enabled: bool,
    extra_blocks: Vec<Vec<u8>>,
) -> LayerRecordOut {
    let mut rec = Vec::new();
    put_i32(&mut rec, bounds.y);
    put_i32(&mut rec, bounds.x);
    put_i32(&mut rec, bounds.bottom());
    put_i32(&mut rec, bounds.right());
    let mut all_channels: Vec<(i16, Vec<u8>)> = channels;
    if let Some((_, data, _)) = &mask {
        all_channels.push((-2, data.clone()));
    }
    put_u16(&mut rec, all_channels.len() as u16);
    let mut channel_data = Vec::new();
    for (id, data) in &all_channels {
        rec.extend_from_slice(&(*id).to_be_bytes());
        put_u32(&mut rec, data.len() as u32);
        channel_data.push(data.clone());
    }
    rec.extend_from_slice(b"8BIM");
    rec.extend_from_slice(if pass_through { b"pass" } else { blend_key(blend) });
    rec.push((opacity.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    rec.push(u8::from(clip));
    let mut flags = 0u8;
    if !visible {
        flags |= 0x02;
    }
    flags |= 0x08; // bit 3 set means bit 4 is meaningful
    rec.push(flags);
    rec.push(0); // filler

    let mut extra = Vec::new();
    // Layer mask data.
    match &mask {
        Some((r, _, default)) => {
            let mut m = Vec::new();
            put_i32(&mut m, r.y);
            put_i32(&mut m, r.x);
            put_i32(&mut m, r.bottom());
            put_i32(&mut m, r.right());
            m.push(*default);
            m.push(if mask_enabled { 0 } else { 0x02 }); // bit 1: mask disabled
            put_u16(&mut m, 0); // padding
            put_u32(&mut extra, m.len() as u32);
            extra.extend_from_slice(&m);
        }
        None => put_u32(&mut extra, 0),
    }
    put_u32(&mut extra, 0); // blending ranges: none
    extra.extend_from_slice(&pascal_name(name));
    extra.extend_from_slice(&unicode_name_block(name));
    for b in extra_blocks {
        extra.extend_from_slice(&b);
    }
    put_u32(&mut rec, extra.len() as u32);
    rec.extend_from_slice(&extra);
    LayerRecordOut {
        record: rec,
        channels: channel_data,
    }
}

fn empty_channels() -> Vec<(i16, Vec<u8>)> {
    // Zero-area layers still carry channel entries; Photoshop writes 2-byte raw headers.
    [0i16, 1, 2, -1].iter().map(|id| (*id, vec![0, 0])).collect()
}

/// Walk the tree bottom-to-top, emitting PSD records in file order
/// (bottom layer first; a group is "close divider, children, open divider").
fn collect_records(
    layers: &[Layer],
    canvas: Rect,
    out: &mut Vec<LayerRecordOut>,
    warnings: &mut Vec<String>,
    deep: bool,
) {
    for l in layers {
        let mask = l.mask.as_ref().map(|m| {
            let (r, plane, default) = mask_plane(m, canvas);
            let enc = if deep {
                let wide: Vec<u16> = plane.iter().map(|&v| v as u16 * 257).collect();
                encode_channel_raw16(&wide)
            } else {
                encode_channel_rle(&plane, r.w as usize, r.h as usize)
            };
            (r, enc, default)
        });
        let mask_enabled = l.mask.as_ref().is_some_and(|m| m.enabled);
        match &l.content {
            LayerContent::Pixel(_)
            | LayerContent::Text(_)
            | LayerContent::Smart(_)
            | LayerContent::Fill(_)
            | LayerContent::Shape(_) => {
                let owned_store;
                // A fill layer carries its settings block next to its
                // rendered pixels, so readers without fills still see it.
                let mut blocks = Vec::new();
                let store: &TileStore = match &l.content {
                    LayerContent::Fill(f) => {
                        let (key, data) = extra::fill_block(&f.fill);
                        blocks.push(additional_block(key, &data));
                        owned_store = f
                            .cache
                            .clone()
                            .filter(|_| f.cache_canvas == (canvas.w, canvas.h))
                            .unwrap_or_else(|| lumenply_render::fill::render_fill(&f.fill, canvas, false));
                        &owned_store
                    }
                    LayerContent::Pixel(s) => s,
                    LayerContent::Text(t) => {
                        warnings.push(format!(
                            "text layer '{}' was exported as pixels (font: {}, {} px)",
                            l.name,
                            lumenply_render::text::font_label(t),
                            t.size.round()
                        ));
                        owned_store = t
                            .cache
                            .clone()
                            .unwrap_or_else(|| lumenply_render::text::rasterize(t));
                        &owned_store
                    }
                    LayerContent::Smart(s) => {
                        warnings.push(format!("smart object '{}' was exported as pixels", l.name));
                        owned_store = s
                            .cache
                            .clone()
                            .unwrap_or_else(|| lumenply_render::transform_store(&s.source, &s.transform));
                        &owned_store
                    }
                    LayerContent::Shape(sh) => {
                        // Photoshop's own shape layer: fill settings, vector
                        // stroke and vector mask, beside the rendered pixels.
                        for (key, data) in shape::shape_blocks(sh, canvas.w, canvas.h) {
                            blocks.push(additional_block(key, &data));
                        }
                        owned_store = sh
                            .cache
                            .clone()
                            .filter(|_| sh.cache_canvas == (canvas.w, canvas.h))
                            .unwrap_or_else(|| lumenply_render::shape::render_shape(sh, canvas, false));
                        &owned_store
                    }
                    _ => unreachable!(),
                };
                // PSD smart filters are not written (ADR 0011): the layer
                // carries its filtered pixels instead, without fill or
                // shape settings that would make readers re-render it.
                let sf_baked;
                let store: &TileStore = if l.smart_filters.is_active() {
                    warnings.push(format!(
                        "layer '{}': smart filters were baked into its pixels",
                        l.name
                    ));
                    blocks.clear();
                    sf_baked = l.smart_filters.filtered().cloned().unwrap_or_else(|| {
                        lumenply_render::smart_filters::bake(store, &l.smart_filters, canvas, false)
                    });
                    &sf_baked
                } else {
                    store
                };
                let (bounds, chans) = if deep {
                    match layer_planes16(store) {
                        Some((b, planes)) => (
                            b,
                            vec![
                                (0i16, encode_channel_raw16(&planes[0])),
                                (1, encode_channel_raw16(&planes[1])),
                                (2, encode_channel_raw16(&planes[2])),
                                (-1, encode_channel_raw16(&planes[3])),
                            ],
                        ),
                        None => (Rect::new(0, 0, 0, 0), empty_channels()),
                    }
                } else {
                    match layer_planes(store) {
                        Some((b, planes)) => {
                            let (w, h) = (b.w as usize, b.h as usize);
                            (
                                b,
                                vec![
                                    (0i16, encode_channel_rle(&planes[0], w, h)),
                                    (1, encode_channel_rle(&planes[1], w, h)),
                                    (2, encode_channel_rle(&planes[2], w, h)),
                                    (-1, encode_channel_rle(&planes[3], w, h)),
                                ],
                            )
                        }
                        None => (Rect::new(0, 0, 0, 0), empty_channels()),
                    }
                };
                out.push(layer_record(
                    &l.name,
                    bounds,
                    chans,
                    l.blend,
                    false,
                    l.clip,
                    l.opacity,
                    l.visible,
                    mask,
                    mask_enabled,
                    {
                        blocks.extend(lock_blocks(l));
                        blocks
                    },
                ));
                if let (LayerContent::Shape(_), Some(rec), false) =
                    (&l.content, out.last_mut(), l.smart_filters.is_active())
                {
                    shape::mark_pixel_data_irrelevant(&mut rec.record);
                }
            }
            LayerContent::Group(children) => {
                // Closing divider comes first in file order (it is the bottom-most record).
                out.push(layer_record(
                    "</Layer group>",
                    Rect::new(0, 0, 0, 0),
                    empty_channels(),
                    BlendMode::Normal,
                    false,
                    false,
                    1.0,
                    true,
                    None,
                    true,
                    vec![section_divider_block(3, blend_key(BlendMode::Normal))],
                ));
                collect_records(children, canvas, out, warnings, deep);
                out.push(layer_record(
                    &l.name,
                    Rect::new(0, 0, 0, 0),
                    empty_channels(),
                    l.blend,
                    l.pass_through,
                    l.clip,
                    l.opacity,
                    l.visible,
                    mask,
                    mask_enabled,
                    [
                        vec![section_divider_block(
                            if l.collapsed { 2 } else { 1 },
                            if l.pass_through {
                                b"pass"
                            } else {
                                blend_key(l.blend)
                            },
                        )],
                        lock_blocks(l),
                    ]
                    .concat(),
                ));
            }
            LayerContent::Filter(f) => warnings.push(format!(
                "live filter layer '{}' ({}) has no PSD equivalent and was not exported",
                l.name,
                f.name()
            )),
            LayerContent::Adjustment(a) => match adjustment_block(a) {
                Some(block) => {
                    let mut blocks = vec![block];
                    blocks.extend(lock_blocks(l));
                    if let Adjustment::BrightnessContrast { brightness, contrast } = a {
                        blocks.push(cged_block(
                            i16_of(*brightness, 100.0, -100, 100) as i32,
                            i16_of(*contrast, 100.0, -100, 100) as i32,
                        ));
                    }
                    out.push(layer_record(
                        &l.name,
                        Rect::new(0, 0, 0, 0),
                        empty_channels(),
                        l.blend,
                        false,
                        l.clip,
                        l.opacity,
                        l.visible,
                        mask,
                        mask_enabled,
                        blocks,
                    ))
                }
                None => warnings.push(format!(
                    "adjustment layer '{}' ({}) has no PSD equivalent and was not exported",
                    l.name,
                    a.name()
                )),
            },
        }
    }
}

/// Write `doc` as an 8-bit RGB PSD.
pub fn save(path: impl AsRef<Path>, doc: &Document) -> Result<Report<()>, PsdError> {
    save_depth(path, doc, false)
}

/// Write `doc` as a 16-bit RGB PSD (raw channels, full precision).
pub fn save_16(path: impl AsRef<Path>, doc: &Document) -> Result<Report<()>, PsdError> {
    save_depth(path, doc, true)
}

fn save_depth(path: impl AsRef<Path>, doc: &Document, deep: bool) -> Result<Report<()>, PsdError> {
    let mut warnings = Vec::new();
    doc.for_each_layer(|l| {
        if !l.effects.is_empty() {
            warnings.push(format!(
                "layer '{}': layer effects are not written to PSD yet",
                l.name
            ));
        }
    });
    let canvas = doc.canvas();
    let (w, h) = (doc.width as usize, doc.height as usize);

    let mut file = Vec::new();
    file.extend_from_slice(b"8BPS");
    put_u16(&mut file, 1); // version
    file.extend_from_slice(&[0; 6]);
    // RGB, the composite's transparency, then one alpha channel per saved
    // selection.
    put_u16(&mut file, 4 + doc.saved_selections.len() as u16);
    put_u32(&mut file, doc.height);
    put_u32(&mut file, doc.width);
    put_u16(&mut file, if deep { 16 } else { 8 }); // depth
    put_u16(&mut file, 3); // RGB
    put_u32(&mut file, 0); // colour mode data
                           // Guides (1032) and the alpha channels' names (1006, 1045).
    file.extend_from_slice(&crate::psd_channels::resources_section(
        &crate::psd_guides::image_resources(doc),
        doc,
    ));

    // Layer and mask information.
    let mut records = Vec::new();
    collect_records(doc.layers(), canvas, &mut records, &mut warnings, deep);
    let mut layer_info = Vec::new();
    // Negative: the composite's fourth channel is its transparency, not an
    // alpha channel Photoshop should list in the Channels panel.
    let count = records.len() as i16;
    put_u16(&mut layer_info, if count > 0 { (-count) as u16 } else { 0 });
    for r in &records {
        layer_info.extend_from_slice(&r.record);
    }
    for r in &records {
        for c in &r.channels {
            layer_info.extend_from_slice(c);
        }
    }
    let mut lm = Vec::new();
    put_u32(&mut lm, pad_even(layer_info.len()) as u32);
    lm.extend_from_slice(&layer_info);
    if layer_info.len() % 2 == 1 {
        lm.push(0);
    }
    put_u32(&mut lm, 0); // global layer mask info: none
    put_u32(&mut file, lm.len() as u32);
    file.extend_from_slice(&lm);

    // Composite image data: 8-bit writes RLE (all channels share one count
    // table), 16-bit writes raw planes of big-endian samples.
    let flat = lumenply_render::composite_raster(doc);
    if deep {
        put_u16(&mut file, 0);
        let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        for ch in 0..4usize {
            for p in &flat.pixels {
                let [cr, cg, cb, a] = p.to_straight();
                let v = match ch {
                    0 => q(lumenply_doc::adjust::srgb_encode(cr)),
                    1 => q(lumenply_doc::adjust::srgb_encode(cg)),
                    2 => q(lumenply_doc::adjust::srgb_encode(cb)),
                    _ => q(a),
                };
                file.extend_from_slice(&v.to_be_bytes());
            }
        }
        for sel in &doc.saved_selections {
            for v in crate::psd_channels::plane(sel, canvas) {
                file.extend_from_slice(&q(v).to_be_bytes());
            }
        }
    } else {
        let mut planes = vec![
            vec![0u8; w * h],
            vec![0u8; w * h],
            vec![0u8; w * h],
            vec![0u8; w * h],
        ];
        for sel in &doc.saved_selections {
            let plane = crate::psd_channels::plane(sel, canvas);
            planes.push(
                plane
                    .iter()
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
                    .collect(),
            );
        }
        for (i, p) in flat.pixels.iter().enumerate() {
            let [cr, cg, cb, a] = p.to_straight();
            planes[0][i] = linear_to_srgb(cr);
            planes[1][i] = linear_to_srgb(cg);
            planes[2][i] = linear_to_srgb(cb);
            planes[3][i] = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        put_u16(&mut file, 1);
        let mut counts = Vec::new();
        let mut data = Vec::new();
        for plane in &planes {
            for y in 0..h {
                let start = data.len();
                packbits(&plane[y * w..(y + 1) * w], &mut data);
                put_u16(&mut counts, (data.len() - start) as u16);
            }
        }
        file.extend_from_slice(&counts);
        file.extend_from_slice(&data);
    }

    std::fs::write(path, file)?;
    Ok(Report { value: (), warnings })
}

// ---- import -------------------------------------------------------------------------------

struct RawLayer {
    name: String,
    bounds: Rect,
    channels: Vec<(i16, Vec<u16>)>, // decoded planes, full-range samples
    blend: BlendMode,
    blend_known: bool,
    pass_through: bool,
    clip: bool,
    opacity: f32,
    visible: bool,
    /// The pixel and vector masks as stored (combined by `build_mask`).
    masks: masks::LayerMasks,
    section: u32, // 0 none, 1/2 group open, 3 close
    is_adjustment: bool,
    adjustment: Option<Adjustment>,
    /// From the `lspf` block; unlocked when absent.
    locks: LayerLocks,
    /// A Solid Color or Gradient fill layer's settings.
    fill: Option<lumenply_doc::Fill>,
    /// A shape layer: a fill with a readable vector mask (and stroke).
    shape: Option<lumenply_doc::ShapeLayer>,
    /// Colour planes of a 32-bit file, as floats (`channels` keeps the
    /// transparency).
    float_channels: Vec<(i16, Vec<f32>)>,
}

/// Largest dimension the PSD v1 format allows for the canvas, a layer or a
/// mask; PSB (v2) raises it tenfold. Anything bigger is corrupt.
const MAX_DIM: u32 = 30_000;
const MAX_DIM_PSB: u32 = 300_000;

/// Validate a bounds rectangle read from the file.
fn checked_rect(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    max: u32,
    what: &str,
) -> Result<Rect, PsdError> {
    let w = right.saturating_sub(left).max(0) as u32;
    let h = bottom.saturating_sub(top).max(0) as u32;
    let m = max as i32;
    if w > max || h > max || !(-m..=m).contains(&left) || !(-m..=m).contains(&top) {
        return Err(PsdError::Corrupt(format!(
            "{what} bounds ({left}, {top})–({right}, {bottom}) are outside the format's limits"
        )));
    }
    Ok(Rect::new(left, top, w, h))
}

/// Read a PSD into a document.
pub fn load(path: impl AsRef<Path>) -> Result<Report<Document>, PsdError> {
    let buf = std::fs::read(path)?;
    let mut rd = Rd::new(&buf);
    if rd.bytes(4)? != b"8BPS" {
        return Err(PsdError::NotPsd("bad signature".into()));
    }
    let version = rd.u16()?;
    if version != 1 && version != 2 {
        return Err(PsdError::Unsupported(format!("version {version}")));
    }
    // Version 2 is PSB ("large document"): the same format with 8-byte
    // section/channel lengths, 4-byte RLE row counts and a 300k dim cap.
    let psb = version == 2;
    rd.skip(6)?;
    let channels = rd.u16()?;
    let height = rd.u32()?;
    let width = rd.u32()?;
    let depth = rd.u16()?;
    let mode = rd.u16()?;
    // PSD v1 caps dimensions at 30,000 (PSB at 300,000) and channels at 56;
    // values beyond that are corrupt and would otherwise size huge
    // allocations.
    let max_dim = if psb { MAX_DIM_PSB } else { MAX_DIM };
    if width == 0 || height == 0 || width > max_dim || height > max_dim {
        return Err(PsdError::Corrupt(format!(
            "canvas {width}×{height} is outside the format's limits"
        )));
    }
    // Colour modes other than RGB and 32-bit depth convert on import.
    let mut cm = color_modes::ColorMode::from_header(mode, depth, channels)?;
    let depth_bytes = (depth as usize / 8).max(1);
    let cmd_len = rd.u32()? as usize;
    let mode_data = rd.bytes(cmd_len)?;
    let res_len = rd.u32()? as usize;
    let resources = rd.bytes(res_len)?;
    cm.read_sections(mode_data, resources);
    let guides = crate::psd_guides::read_guides(resources);
    let alpha_names = crate::psd_channels::read_names(resources);

    let mut warnings: Vec<String> = cm.warning().into_iter().collect();
    let lm_len = rd.len_of(psb)?;
    let lm_start = rd.pos;
    let mut raw: Vec<RawLayer> = Vec::new();
    if lm_len > 0 {
        let mut li_len = rd.len_of(psb)?;
        if li_len == 0 {
            // 16/32-bit Photoshop files keep their layers in Lr16/Lr32.
            if let Some((at, len)) = color_modes::deep_layer_info(&buf, rd.pos, lm_start + lm_len, psb) {
                (rd.pos, li_len) = (at, len);
            }
        }
        if li_len > 0 {
            let li_start = rd.pos;
            let count = rd.i16()?;
            let count = count.unsigned_abs() as usize; // negative = first alpha is transparency
            let mut heads = Vec::with_capacity(count);
            for _ in 0..count {
                let top = rd.i32()?;
                let left = rd.i32()?;
                let bottom = rd.i32()?;
                let right = rd.i32()?;
                let nch = rd.u16()? as usize;
                if nch > 56 {
                    return Err(PsdError::Corrupt(format!("layer with {nch} channels")));
                }
                let mut chans = Vec::with_capacity(nch);
                for _ in 0..nch {
                    let id = rd.i16()?;
                    let len = rd.len_of(psb)?;
                    chans.push((id, len));
                }
                if rd.bytes(4)? != b"8BIM" {
                    return Err(PsdError::Corrupt("layer record signature".into()));
                }
                let mut key = rd.bytes(4)?.to_vec();
                let opacity = rd.u8()? as f32 / 255.0;
                let clipping = rd.u8()?;
                let flags = rd.u8()?;
                let _filler = rd.u8()?;
                let extra_len = rd.u32()? as usize;
                let extra_end = rd.pos + extra_len;

                let mask_len = rd.u32()? as usize;
                // The "real" pixel-mask fields are there only with a −3 channel.
                let has_real = chans.iter().any(|&(id, _)| id == -3);
                let mut layer_masks = masks::LayerMasks {
                    record: masks::read_record(rd.bytes(mask_len)?, has_real, |l, t, r, b| {
                        checked_rect(l, t, r, b, max_dim, "mask").ok()
                    }),
                    ..Default::default()
                };
                let ranges_len = rd.u32()? as usize;
                rd.skip(ranges_len)?;
                let plen = rd.u8()? as usize;
                let pname = String::from_utf8_lossy(rd.bytes(plen)?).into_owned();
                let pad = (4 - (plen + 1) % 4) % 4;
                rd.skip(pad)?;

                let mut name = pname;
                let mut section = 0u32;
                let mut is_adjustment = false;
                let mut adjustment = None;
                let mut locks = LayerLocks::NONE;
                let mut fill = None;
                let (mut vector_mask, mut pattern) = (false, false);
                let (mut vmsk, mut vstk): (Option<Vec<u8>>, Option<Vec<u8>>) = (None, None);
                while rd.pos + 12 <= extra_end {
                    let sig = rd.bytes(4)?;
                    if sig != b"8BIM" && sig != b"8B64" {
                        break;
                    }
                    let k = rd.bytes(4)?.to_vec();
                    // In PSB a handful of block keys carry 8-byte lengths.
                    let wide = psb
                        && matches!(
                            &k[..],
                            b"LMsk"
                                | b"Lr16"
                                | b"Lr32"
                                | b"Layr"
                                | b"Mt16"
                                | b"Mt32"
                                | b"Mtrn"
                                | b"Alph"
                                | b"FMsk"
                                | b"lnk2"
                                | b"FEid"
                                | b"FXid"
                                | b"PxSD"
                        );
                    let len = rd.len_of(wide)?;
                    let data = rd.bytes(len)?;
                    if len % 2 == 1 && rd.pos < extra_end {
                        rd.skip(1)?;
                    }
                    match &k[..] {
                        b"luni" => {
                            let mut d = Rd::new(data);
                            // The count cannot exceed what the block holds.
                            let n = (d.u32()? as usize).min(data.len().saturating_sub(4) / 2);
                            let mut units = Vec::with_capacity(n);
                            for _ in 0..n {
                                units.push(d.u16()?);
                            }
                            name = String::from_utf16_lossy(&units);
                        }
                        b"lsct" => {
                            let mut d = Rd::new(data);
                            section = d.u32()?;
                            // A group's real blend mode (Pass Through
                            // included) lives here; the record says "norm".
                            if data.len() >= 12 && &data[4..8] == b"8BIM" {
                                key = data[8..12].to_vec();
                            }
                        }
                        b"lspf" => {
                            let mut d = Rd::new(data);
                            locks = locks_from_flags(d.u32()?);
                        }
                        b"SoCo" | b"GdFl" => {
                            layer_masks.shaped = true;
                            match extra::parse_fill(&k, data) {
                                Some(f) => fill = Some(f),
                                // An unreadable fill keeps its rendered pixels.
                                None => warnings.push(format!("fill layer '{name}': settings not readable")),
                            }
                        }
                        // A vector mask makes a fill a shape layer.
                        b"vmsk" | b"vsms" => {
                            vector_mask = true;
                            vmsk = Some(data.to_vec());
                        }
                        b"vstk" => vstk = Some(data.to_vec()),
                        // Shape content in newer files: pixels come clipped.
                        b"vscg" => layer_masks.shaped = true,
                        // Pattern fills keep their rendered pixels.
                        b"PtFl" => {
                            pattern = true;
                            layer_masks.shaped = true;
                        }
                        b"levl" | b"curv" | b"brit" | b"CgEd" | b"hue2" | b"hue " | b"blnc" | b"blwh"
                        | b"expA" | b"vibA" | b"thrs" | b"post" | b"nvrt" | b"phfl" | b"mixr" | b"clrL"
                        | b"grdm" | b"selc" => {
                            is_adjustment = true;
                            if adjustment.is_none() {
                                adjustment = cm.adjustment(&k, data)?;
                            }
                        }
                        _ => {}
                    }
                }
                rd.pos = extra_end;
                // Shape layers (a fill clipped by vector paths) and pattern
                // fills come in as the pixels Photoshop rendered for them.
                let mut shape_layer = None;
                layer_masks.vector = vmsk.clone();
                layer_masks.shaped &= vector_mask;
                if let Some(f) = fill.take_if(|_| vector_mask) {
                    // A feathered or faded vector mask isn't a crisp outline.
                    let soft = layer_masks.record.as_ref().is_some_and(|r| {
                        r.vector_density.is_some_and(|d| d < 255) || r.vector_feather.is_some_and(|f| f > 0.0)
                    });
                    if !soft {
                        shape_layer = vmsk
                            .as_deref()
                            .and_then(|m| shape::parse_shape(f.clone(), m, vstk.as_deref(), width, height));
                    }
                    if shape_layer.is_none() {
                        let stroked = vstk
                            .as_deref()
                            .and_then(shape::parse_stroke)
                            .is_some_and(|(stroke, _)| stroke.is_some());
                        let drawable = vmsk
                            .as_deref()
                            .is_some_and(|m| masks::rasterize(m, width, height).is_some());
                        if stroked || !drawable {
                            warnings.push(format!(
                                "shape layer '{name}' was imported as pixels (its outline uses path operations a Lumenply shape can't hold yet)"
                            ));
                        } else {
                            // Still editable: the fill clipped by the vector
                            // mask, drawn from its path (Photoshop may not
                            // have stored pixels at all).
                            fill = Some(f);
                            layer_masks.shaped = false;
                        }
                    }
                }
                if pattern {
                    warnings.push(format!("pattern fill layer '{name}' was imported as pixels"));
                }
                let (blend, known) = blend_from_key(&key);
                heads.push((
                    RawLayer {
                        pass_through: &key[..] == b"pass",
                        clip: clipping == 1,
                        name,
                        bounds: checked_rect(left, top, right, bottom, max_dim, "layer")?,
                        channels: Vec::new(),
                        blend,
                        blend_known: known,
                        opacity,
                        visible: flags & 0x02 == 0,
                        masks: layer_masks,
                        section,
                        is_adjustment,
                        adjustment,
                        locks,
                        fill,
                        shape: shape_layer,
                        float_channels: Vec::new(),
                    },
                    chans,
                ));
            }
            // Channel image data follows, in the same order.
            for (mut layer, chans) in heads {
                for (id, len) in chans {
                    // Bounds-checked: a declared length past the end of the
                    // file is an error, not a slice panic.
                    let data = rd.bytes(len)?;
                    let rec = layer.masks.record.as_ref();
                    let (w, h) = match id {
                        -2 => rec.map_or((0, 0), |m| (m.rect.w as usize, m.rect.h as usize)),
                        -3 => rec
                            .and_then(|m| m.real)
                            .map_or((0, 0), |(r, ..)| (r.w as usize, r.h as usize)),
                        _ => (layer.bounds.w as usize, layer.bounds.h as usize),
                    };
                    if w > 0 && h > 0 && len >= 2 {
                        let mut sub = Rd::new(data);
                        if depth == 32 && id >= 0 {
                            // 32-bit colour keeps its floats (HDR values).
                            match color_modes::decode_f32(&mut sub, w, h, psb) {
                                Ok(plane) => layer.float_channels.push((id, plane)),
                                Err(e) => warnings.push(format!("layer '{}' channel {id}: {e}", layer.name)),
                            }
                            continue;
                        }
                        match decode_channel(&mut sub, w, h, depth_bytes, psb) {
                            Ok(plane) => match id {
                                -2 => layer.masks.minus2 = Some(plane),
                                -3 => layer.masks.minus3 = Some(plane),
                                _ => layer.channels.push((id, plane)),
                            },
                            Err(e) => warnings.push(format!("layer '{}' channel {id}: {e}", layer.name)),
                        }
                    }
                }
                raw.push(layer);
            }
            rd.pos = li_start + pad_even(li_len);
        }
    }
    rd.pos = lm_start + lm_len;

    // Composite: the image itself when the file has no layers, and the
    // alpha channels (saved selections) when it names any.
    let composite = if (raw.is_empty() || !alpha_names.is_empty()) && rd.pos + 2 <= buf.len() {
        let (w, h) = (width as usize, height as usize);
        let nch = channels as usize; // header-validated: 1..=56
        Some(color_modes::read_composite(&mut rd, w, h, nch, depth, psb)?)
    } else {
        None
    };

    let mut doc = Document::new(width, height);
    doc.guides = guides;
    // 32-bit files are linear float: keep values above 1 (HDR).
    doc.float_mode = cm.is_float();
    // The named alpha channels are the composite's last planes.
    let n_alpha = composite
        .as_ref()
        .map_or(0, |p| alpha_names.len().min(p.len().saturating_sub(cm.n_color())));
    if let Some(planes) = &composite {
        let start = planes.len() - n_alpha;
        for (k, name) in alpha_names.iter().take(n_alpha).enumerate() {
            doc.saved_selections.push(crate::psd_channels::from_plane(
                name.clone(),
                &planes[start + k].to_u16(),
                width,
                height,
            ));
        }
    }
    if let Some(planes) = composite.filter(|_| raw.is_empty()) {
        let r = cm.composite_raster(&planes, n_alpha, width, height);
        let id = doc.alloc_id();
        let mut l = Layer::pixel(id, "Background");
        *l.pixels_mut().expect("pixel") = TileStore::from_raster(&r, 0, 0);
        doc.add_layer(l);
        return Ok(Report { value: doc, warnings });
    }

    // Rebuild the tree: records are bottom-to-top; a close divider (3) opens
    // a pending group that the next open divider (1/2) names and finishes.
    let mut stack: Vec<Vec<Layer>> = vec![Vec::new()];
    for rl in raw {
        match rl.section {
            3 => stack.push(Vec::new()),
            1 | 2 => {
                // Pop only a frame a close divider opened — a stray open
                // divider must not consume the root of the tree.
                let children = if stack.len() > 1 {
                    stack.pop().unwrap_or_default()
                } else {
                    warnings.push(format!("group '{}' had no matching start marker", rl.name));
                    Vec::new()
                };
                let id = doc.alloc_id();
                let mut g = Layer::group(id, rl.name.clone());
                *g.children_mut().expect("group") = children;
                g.blend = rl.blend;
                g.pass_through = rl.pass_through;
                g.clip = rl.clip;
                g.opacity = rl.opacity;
                g.visible = rl.visible;
                g.collapsed = rl.section == 2;
                g.mask = build_mask(&rl, width, height);
                g.locks = rl.locks;
                if !rl.blend_known {
                    warnings.push(format!(
                        "group '{}': unsupported blend mode, using normal",
                        rl.name
                    ));
                }
                stack.last_mut().expect("root").push(g);
            }
            _ => {
                if rl.is_adjustment {
                    match rl.adjustment.clone() {
                        Some(adj) => {
                            let id = doc.alloc_id();
                            let mut l = Layer::adjustment(id, adj);
                            l.name = rl.name.clone();
                            l.blend = rl.blend;
                            l.clip = rl.clip;
                            l.opacity = rl.opacity;
                            l.visible = rl.visible;
                            l.mask = build_mask(&rl, width, height);
                            l.locks = rl.locks;
                            stack.last_mut().expect("root").push(l);
                        }
                        None => warnings.push(format!(
                            "adjustment layer '{}' is of a type not supported yet and was skipped",
                            rl.name
                        )),
                    }
                    continue;
                }
                if let Some(sh) = rl.shape.clone() {
                    // Rendered below with the fills, once every layer is in.
                    let id = doc.alloc_id();
                    let mut l = Layer::shape(id, sh);
                    l.name = rl.name.clone();
                    l.blend = rl.blend;
                    l.clip = rl.clip;
                    l.opacity = rl.opacity;
                    l.visible = rl.visible;
                    l.mask = build_mask(&rl, width, height);
                    l.locks = rl.locks;
                    stack.last_mut().expect("root").push(l);
                    continue;
                }
                if let Some(fill) = rl.fill.clone() {
                    // The cache renders below, once every layer is in.
                    let id = doc.alloc_id();
                    let mut l = Layer::fill(id, fill);
                    l.name = rl.name.clone();
                    l.blend = rl.blend;
                    l.clip = rl.clip;
                    l.opacity = rl.opacity;
                    l.visible = rl.visible;
                    l.mask = build_mask(&rl, width, height);
                    stack.last_mut().expect("root").push(l);
                    continue;
                }
                let id = doc.alloc_id();
                let mut l = Layer::pixel(id, rl.name.clone());
                l.locks = rl.locks;
                l.blend = rl.blend;
                l.clip = rl.clip;
                l.opacity = rl.opacity;
                l.visible = rl.visible;
                if !rl.blend_known {
                    warnings.push(format!(
                        "layer '{}': unsupported blend mode, using normal",
                        rl.name
                    ));
                }
                let b = rl.bounds;
                if b.w > 0 && b.h > 0 {
                    let raster = cm.layer_raster(&rl.channels, &rl.float_channels, b.w, b.h);
                    *l.pixels_mut().expect("pixel") = TileStore::from_raster(&raster, b.x, b.y);
                }
                l.mask = build_mask(&rl, width, height);
                stack.last_mut().expect("root").push(l);
            }
        }
    }
    while stack.len() > 1 {
        // Unterminated groups: flatten their contents into the parent.
        let orphans = stack.pop().unwrap_or_default();
        stack.last_mut().expect("root").extend(orphans);
        warnings.push("a layer group was missing its header and was flattened".into());
    }
    let top = stack.pop().unwrap_or_default();
    for l in top {
        doc.add_layer(l);
    }
    if cm.is_float() {
        color_modes::linear_descriptor_colors(&mut doc);
    }
    lumenply_render::fill::refresh_stale(&mut doc);
    Ok(Report { value: doc, warnings })
}

fn build_mask(rl: &RawLayer, width: u32, height: u32) -> Option<Mask> {
    rl.masks.build(width, height, rl.shape.is_some())
}

/// Convenience for callers that only need a flat raster of a PSD.
pub fn load_flat(path: impl AsRef<Path>) -> Result<Raster, PsdError> {
    let doc = load(path)?.value;
    Ok(lumenply_render::composite_raster(&doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("nge-psd-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// Minimal PSD bytes: a header plus a layer-info section holding
    /// `records` (already encoded) followed by `channel_data`.
    fn craft_psd(channels: u16, w: u32, h: u32, records: &[u8], channel_data: &[u8]) -> Vec<u8> {
        craft_psd_depth(8, channels, w, h, records, channel_data)
    }

    fn craft_psd_depth(
        depth: u16,
        channels: u16,
        w: u32,
        h: u32,
        records: &[u8],
        channel_data: &[u8],
    ) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(b"8BPS");
        put_u16(&mut f, 1); // version
        f.extend_from_slice(&[0; 6]);
        put_u16(&mut f, channels);
        put_u32(&mut f, h);
        put_u32(&mut f, w);
        put_u16(&mut f, depth);
        put_u16(&mut f, 3); // RGB
        put_u32(&mut f, 0); // colour mode data
        put_u32(&mut f, 0); // resources
        if records.is_empty() {
            put_u32(&mut f, 0); // no layer+mask section
        } else {
            let li_len = 2 + records.len() + channel_data.len();
            put_u32(&mut f, 4 + pad_even(li_len) as u32); // layer+mask section
            put_u32(&mut f, li_len as u32); // layer info
            put_u16(&mut f, 1); // one layer record
            f.extend_from_slice(records);
            f.extend_from_slice(channel_data);
            if li_len % 2 == 1 {
                f.push(0);
            }
        }
        f
    }

    /// One layer record with the given channel list and an optional
    /// `lsct` section divider.
    fn craft_record(chans: &[(i16, u32)], section: Option<u32>, bounds: (i32, i32, i32, i32)) -> Vec<u8> {
        let (top, left, bottom, right) = bounds;
        let mut r = Vec::new();
        put_i32(&mut r, top);
        put_i32(&mut r, left);
        put_i32(&mut r, bottom);
        put_i32(&mut r, right);
        put_u16(&mut r, chans.len() as u16);
        for (id, len) in chans {
            put_u16(&mut r, *id as u16);
            put_u32(&mut r, *len);
        }
        r.extend_from_slice(b"8BIM");
        r.extend_from_slice(b"norm");
        r.extend_from_slice(&[255, 0, 0, 0]); // opacity, clipping, flags, filler
        let mut extra = Vec::new();
        put_u32(&mut extra, 0); // no mask
        put_u32(&mut extra, 0); // no blending ranges
        extra.extend_from_slice(&[0, 0, 0, 0]); // empty pascal name + pad
        if let Some(s) = section {
            extra.extend_from_slice(b"8BIM");
            extra.extend_from_slice(b"lsct");
            put_u32(&mut extra, 4);
            put_u32(&mut extra, s);
        }
        put_u32(&mut r, extra.len() as u32);
        r.extend_from_slice(&extra);
        r
    }

    fn load_bytes(name: &str, bytes: &[u8]) -> Result<Report<Document>, PsdError> {
        let path = temp(name);
        std::fs::write(&path, bytes).unwrap();
        load(&path)
    }

    /// A 2×2 layer of opaque red pixels carrying the given tagged blocks.
    fn record_with_blocks(blocks: &[(&[u8; 4], Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let mut r = Vec::new();
        for v in [0, 0, 2, 2] {
            put_i32(&mut r, v); // top, left, bottom, right
        }
        put_u16(&mut r, 4);
        for id in [0i16, 1, 2, -1] {
            put_u16(&mut r, id as u16);
            put_u32(&mut r, 6); // raw compression word + 4 samples
        }
        r.extend_from_slice(b"8BIMnorm");
        r.extend_from_slice(&[255, 0, 0, 0]);
        let mut extra = Vec::new();
        put_u32(&mut extra, 0);
        put_u32(&mut extra, 0);
        extra.extend_from_slice(&[0, 0, 0, 0]);
        for (key, data) in blocks {
            extra.extend_from_slice(&additional_block(key, data));
        }
        put_u32(&mut r, extra.len() as u32);
        r.extend_from_slice(&extra);
        let mut chans = Vec::new();
        for v in [255u8, 0, 0, 255] {
            chans.extend_from_slice(&[0, 0, v, v, v, v]);
        }
        (r, chans)
    }

    #[test]
    fn fills_import_as_fill_layers_but_shapes_and_patterns_as_pixels() {
        let (key, soco) = extra::fill_block(&lumenply_doc::Fill::Solid {
            color: [0.0, 1.0, 0.0],
        });
        let (rec, chans) = record_with_blocks(&[(key, soco.clone())]);
        let doc = load_bytes("fill-soco.psd", &craft_psd(3, 4, 4, &rec, &chans)).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let l = &doc.value.layers()[0];
        assert!(matches!(
            l.fill_layer().map(|f| &f.fill),
            Some(lumenply_doc::Fill::Solid { color }) if color[1] > 0.99 && color[0] < 0.01
        ));
        // Rendered over the whole 4×4 canvas, not the 2×2 record bounds.
        let p = l.raster_store().unwrap().get_pixel(3, 3);
        assert!(p.g > 0.99 && p.a == 1.0, "{p:?}");

        // With a vector mask it is a shape: its pixels, with a warning.
        let (rec, chans) = record_with_blocks(&[(key, soco), (b"vmsk", vec![0; 8])]);
        let doc = load_bytes("fill-shape.psd", &craft_psd(3, 4, 4, &rec, &chans)).unwrap();
        assert!(
            doc.warnings.iter().any(|w| w.contains("shape layer")),
            "{:?}",
            doc.warnings
        );
        let l = &doc.value.layers()[0];
        let p = l.pixels().expect("pixel layer").get_pixel(1, 1);
        assert!(p.r > 0.99 && p.g < 0.01, "the rendered red shape: {p:?}");
        assert_eq!(l.pixels().unwrap().get_pixel(3, 3).a, 0.0);

        // An outline with path operations stays an editable fill layer,
        // clipped by the vector mask drawn from its path: here two squares
        // intersected leave only x = 1 of the 4×4 canvas.
        let fixed = |v: f32| ((v / 4.0) * (1 << 24) as f32) as i32;
        let mut vmsk = Vec::new();
        put_u32(&mut vmsk, 3);
        put_u32(&mut vmsk, 0);
        for x0 in [0.0f32, 1.0] {
            put_u16(&mut vmsk, 0);
            put_u16(&mut vmsk, 4);
            vmsk.extend_from_slice(&3i16.to_be_bytes()); // intersect
            vmsk.extend_from_slice(&[0; 20]);
            for (x, y) in [(x0, 0.0), (x0 + 2.0, 0.0), (x0 + 2.0, 4.0), (x0, 4.0)] {
                put_u16(&mut vmsk, 1);
                for _ in 0..3 {
                    put_i32(&mut vmsk, fixed(y));
                    put_i32(&mut vmsk, fixed(x));
                }
            }
        }
        let (rec, chans) = record_with_blocks(&[
            (
                key,
                extra::fill_block(&lumenply_doc::Fill::Solid {
                    color: [0.0, 1.0, 0.0],
                })
                .1,
            ),
            (b"vmsk", vmsk),
        ]);
        let doc = load_bytes("fill-ops.psd", &craft_psd(3, 4, 4, &rec, &chans)).unwrap();
        let l = &doc.value.layers()[0];
        assert!(l.fill_layer().is_some(), "an editable fill layer");
        let m = l.mask.as_ref().expect("the vector mask");
        assert_eq!(
            (m.value(0, 2), m.value(1, 2), m.value(2, 2), m.value(3, 2)),
            (0.0, 1.0, 0.0, 0.0)
        );

        // Pattern fills keep their pixels too.
        let (rec, chans) = record_with_blocks(&[(b"PtFl", vec![0; 8])]);
        let doc = load_bytes("fill-pattern.psd", &craft_psd(3, 4, 4, &rec, &chans)).unwrap();
        assert!(
            doc.warnings.iter().any(|w| w.contains("pattern fill")),
            "{:?}",
            doc.warnings
        );
        assert!(doc.value.layers()[0].pixels().is_some());
    }

    #[test]
    fn psb_files_load_with_wide_lengths_and_rle_counts() {
        // A hand-built 4×4 PSB: version 2, 8-byte section and channel
        // lengths, one layer whose channels are RLE with 4-byte row
        // counts, plus a raw composite so strict readers accept the file.
        let (w, h) = (4usize, 4usize);
        let put_u64 = |f: &mut Vec<u8>, v: u64| f.extend_from_slice(&v.to_be_bytes());

        // Channel planes: red ramps 0..255 by pixel index, green 0, blue 0,
        // alpha 255.
        let red: Vec<u8> = (0..w * h).map(|i| (i * 17) as u8).collect();
        let flat = |v: u8| vec![v; w * h];
        let rle = |plane: &[u8]| -> Vec<u8> {
            let mut counts = Vec::new();
            let mut data = Vec::new();
            for y in 0..h {
                let start = data.len();
                packbits(&plane[y * w..(y + 1) * w], &mut data);
                counts.extend_from_slice(&((data.len() - start) as u32).to_be_bytes());
            }
            let mut out = vec![0, 1]; // compression = RLE
            out.extend(counts);
            out.extend(data);
            out
        };
        let chans: Vec<(i16, Vec<u8>)> = vec![
            (0, rle(&red)),
            (1, rle(&flat(0))),
            (2, rle(&flat(0))),
            (-1, rle(&flat(255))),
        ];

        // Layer record with 8-byte channel lengths.
        let mut rec = Vec::new();
        for v in [0i32, 0, h as i32, w as i32] {
            rec.extend_from_slice(&v.to_be_bytes());
        }
        put_u16(&mut rec, chans.len() as u16);
        for (id, data) in &chans {
            put_u16(&mut rec, *id as u16);
            put_u64(&mut rec, data.len() as u64);
        }
        rec.extend_from_slice(b"8BIM");
        rec.extend_from_slice(b"norm");
        rec.extend_from_slice(&[255, 0, 0, 0]);
        let mut extra = Vec::new();
        put_u32(&mut extra, 0); // no mask
        put_u32(&mut extra, 0); // no blending ranges
        extra.extend_from_slice(&[0, 0, 0, 0]); // empty name + pad
        put_u32(&mut rec, extra.len() as u32);
        rec.extend_from_slice(&extra);

        let mut f = Vec::new();
        f.extend_from_slice(b"8BPS");
        put_u16(&mut f, 2); // version: PSB
        f.extend_from_slice(&[0; 6]);
        put_u16(&mut f, 3);
        put_u32(&mut f, h as u32);
        put_u32(&mut f, w as u32);
        put_u16(&mut f, 8);
        put_u16(&mut f, 3); // RGB
        put_u32(&mut f, 0); // colour mode data
        put_u32(&mut f, 0); // resources
        let chan_bytes: usize = chans.iter().map(|(_, d)| d.len()).sum();
        let li_len = 2 + rec.len() + chan_bytes;
        put_u64(&mut f, (8 + pad_even(li_len) + 4) as u64); // layer+mask section
        put_u64(&mut f, li_len as u64); // layer info
        put_u16(&mut f, 1); // one record
        f.extend_from_slice(&rec);
        for (_, d) in &chans {
            f.extend_from_slice(d);
        }
        if li_len % 2 == 1 {
            f.push(0);
        }
        put_u32(&mut f, 0); // global layer mask info
        put_u16(&mut f, 0); // composite: raw
        for v in [None, Some(0u8), Some(0)] {
            match v {
                None => f.extend_from_slice(&red),
                Some(c) => f.extend_from_slice(&flat(c)),
            }
        }
        let doc = load_bytes("wide.psb", &f).unwrap().value;
        assert_eq!((doc.width, doc.height), (4, 4));
        assert_eq!(doc.layer_count(), 1);
        let px = doc.layers()[0].pixels().unwrap();
        let got = px.get_pixel(3, 2).to_straight(); // index 11 → 187
        let want = crate::srgb_to_linear_f(187.0 / 255.0);
        assert!((got[0] - want).abs() < 1e-3, "PSB RLE channel decodes: {got:?}");
        assert!((px.get_pixel(0, 0).to_straight()[3] - 1.0).abs() < 1e-4);

        // Unknown versions still refuse.
        let mut bad = f.clone();
        bad[4..6].copy_from_slice(&3u16.to_be_bytes());
        assert!(load_bytes("v3.psb", &bad).is_err());
    }

    #[test]
    fn sixteen_bit_export_round_trips_deep_values() {
        // A value that needs 16 bits: linear for the sRGB code 300/65535.
        let fine = crate::srgb_to_linear_f(300.0 / 65535.0);
        let mut doc = Document::new(4, 4);
        let id = doc.add_pixel_layer("L");
        for y in 0..4 {
            for x in 0..4 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(fine, 0.25, 1.0, 1.0),
                );
            }
        }
        let path = temp("deep.psd");
        save_16(&path, &doc).unwrap();
        let back = load(&path).unwrap().value;
        let got = back.layers()[0].pixels().unwrap().get_pixel(1, 1).to_straight();
        assert!(
            (got[0] - fine).abs() < 1e-5,
            "16-bit red survives: {} vs {fine}",
            got[0]
        );
        // An 8-bit save of the same value lands on a coarser code.
        let path8 = temp("deep8.psd");
        save(&path8, &doc).unwrap();
        let back8 = load(&path8).unwrap().value;
        let got8 = back8.layers()[0].pixels().unwrap().get_pixel(1, 1).to_straight();
        assert!(
            (got8[0] - fine).abs() > (got[0] - fine).abs(),
            "the deep file is strictly more precise: 8-bit {} vs 16-bit {}",
            got8[0],
            got[0]
        );
    }

    #[test]
    fn sixteen_bit_psds_load_at_full_precision() {
        // Flat file: header depth 16, no layers, raw composite. The red
        // plane ramps through exact 16-bit values no 8-bit file can hold.
        let reds: [u16; 4] = [0, 16384, 32768, 65535];
        let mut f = craft_psd_depth(16, 3, 2, 2, &[], &[]);
        put_u16(&mut f, 0); // composite compression: raw
        for &v in &reds {
            f.extend_from_slice(&v.to_be_bytes());
        }
        for _ in 0..2 {
            // green and blue planes: zero
            f.extend_from_slice(&[0u8; 8]);
        }
        let doc = load_bytes("flat16.psd", &f).unwrap().value;
        let px = doc.layers()[0].pixels().unwrap();
        for (i, &v) in reds.iter().enumerate() {
            let got = px.get_pixel(i as i32 % 2, i as i32 / 2).to_straight()[0];
            let want = crate::srgb_to_linear_f(v as f32 / 65535.0);
            assert!((got - want).abs() < 2e-4, "16-bit sample {v}: {got} vs {want}");
        }

        // Layered file: one 2x2 layer whose channels are ZIP-with-
        // prediction coded, Photoshop's usual choice for 16-bit layers.
        let zip_pred = |samples: &[u16], w: usize| -> Vec<u8> {
            let mut bytes = Vec::new();
            for row in samples.chunks(w) {
                let mut prev = 0u16;
                for (i, &s) in row.iter().enumerate() {
                    let d = if i == 0 { s } else { s.wrapping_sub(prev) };
                    bytes.extend_from_slice(&d.to_be_bytes());
                    prev = s;
                }
            }
            use std::io::Write;
            let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&bytes).unwrap();
            let mut out = vec![0, 3]; // compression = 3 (zip with prediction)
            out.extend(enc.finish().unwrap());
            out
        };
        let zero = zip_pred(&[0, 0, 0, 0], 2);
        let red = zip_pred(&reds, 2);
        let mut data = Vec::new();
        let mut chans = Vec::new();
        for (id, payload) in [(0i16, &red), (1, &zero), (2, &zero)] {
            chans.push((id, payload.len() as u32));
            data.extend_from_slice(payload);
        }
        let rec = craft_record(&chans, None, (0, 0, 2, 2));
        let mut psd = craft_psd_depth(16, 3, 2, 2, &rec, &data);
        // A trailing raw composite keeps the file valid for strict readers.
        put_u16(&mut psd, 0);
        psd.extend_from_slice(&[0u8; 2 * 2 * 2 * 3]);
        let doc = load_bytes("layered16.psd", &psd).unwrap().value;
        let px = doc.layers()[0].pixels().unwrap();
        let got = px.get_pixel(0, 1).to_straight()[0];
        let want = crate::srgb_to_linear_f(32768.0 / 65535.0);
        assert!(
            (got - want).abs() < 2e-4,
            "zip-predicted 16-bit channel: {got} vs {want}"
        );
        assert!(px.get_pixel(1, 1).to_straight()[0] > 0.999, "65535 is white");
        assert!(px.get_pixel(0, 0).to_straight()[1] < 1e-4, "green stays 0");
    }

    #[test]
    fn malformed_psds_error_instead_of_panicking() {
        // A layer channel whose declared length runs past the end of the file.
        let rec = craft_record(&[(0, 50_000)], None, (0, 0, 10, 10));
        let psd = craft_psd(3, 10, 10, &rec, &[0u8; 8]);
        assert!(load_bytes("truncated-channel.psd", &psd).is_err());

        // A lone "group open" divider with no earlier close: must not pop the
        // root of the tree stack.
        let rec = craft_record(&[], Some(1), (0, 0, 0, 0));
        let psd = craft_psd(3, 10, 10, &rec, &[]);
        let report = load_bytes("lone-divider.psd", &psd).unwrap();
        assert_eq!(report.value.layer_count(), 1, "one empty group");

        // A composite that claims only 2 channels cannot be RGB.
        let mut psd = craft_psd(2, 4, 4, &[], &[]);
        put_u16(&mut psd, 0); // raw compression
        psd.extend_from_slice(&[128u8; 32]); // 2 planes of 16 bytes
        assert!(load_bytes("two-channel.psd", &psd).is_err());

        // Absurd canvas and layer dimensions are rejected, not allocated.
        let psd = craft_psd(3, 0xFFFF_FFFF, 10, &[], &[]);
        assert!(load_bytes("huge-canvas.psd", &psd).is_err());
        let rec = craft_record(&[(0, 4)], None, (0, 0, 0x7FFF_FFFF, 0x7FFF_FFFF));
        let psd = craft_psd(3, 10, 10, &rec, &[0u8; 4]);
        assert!(load_bytes("huge-layer.psd", &psd).is_err());
    }

    #[test]
    fn packbits_round_trip() {
        for row in [
            vec![1u8, 1, 1, 1, 2, 3, 4, 4, 4, 4, 4, 5],
            vec![0u8; 300],
            (0..=255u8).collect::<Vec<_>>(),
            vec![7u8],
        ] {
            let mut enc = Vec::new();
            packbits(&row, &mut enc);
            assert_eq!(unpackbits(&enc, row.len()).unwrap(), row);
        }
    }

    #[test]
    fn psd_round_trip_keeps_tree_pixels_and_masks() {
        let mut doc = Document::new(300, 200);
        let bg = doc.add_pixel_layer("Background");
        let fill = Raster::filled(300, 200, Rgba::from_straight(0.9, 0.9, 0.85, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);

        let g = doc.add_group("Shapes ünïcode");
        let a_id = doc.alloc_id();
        let mut a = Layer::pixel(a_id, "Red square");
        let sq = Raster::filled(50, 40, Rgba::from_straight(1.0, 0.0, 0.0, 0.5));
        *a.pixels_mut().unwrap() = TileStore::from_raster(&sq, 20, 30);
        a.opacity = 0.6;
        a.blend = BlendMode::Multiply;
        let mut mask = Mask::reveal_all();
        mask.set_value(25, 35, 0.25);
        a.mask = Some(mask);
        let b_id = doc.alloc_id();
        let mut b = Layer::pixel(b_id, "Hidden");
        b.visible = false;
        b.pixels_mut().unwrap().set_pixel(250, 150, Rgba::WHITE);
        let grp = doc.layer_mut(g).unwrap();
        grp.children_mut().unwrap().push(a);
        grp.children_mut().unwrap().push(b);
        grp.children_mut().unwrap()[1].clip = true;
        grp.pass_through = true;
        doc.add_adjustment(lumenply_doc::Adjustment::Invert);

        let path = temp("rt.psd");
        let rep = save(&path, &doc).unwrap();
        assert!(
            rep.warnings.is_empty(),
            "Invert is exportable: {:?}",
            rep.warnings
        );

        let back = load(&path).unwrap().value;
        assert_eq!((back.width, back.height), (300, 200));
        let names: Vec<&str> = back.layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Background", "Shapes ünïcode", "Invert"]);
        assert!(matches!(
            back.layers()[2].content,
            LayerContent::Adjustment(Adjustment::Invert)
        ));
        let grp = &back.layers()[1];
        assert!(grp.pass_through, "the 'pass' blend key round-trips");
        // Photoshop writes "norm" in a group's record and its real mode in
        // the section divider block; the divider wins.
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes
            .windows(8)
            .position(|w| w == b"8BIMpass")
            .expect("record key");
        bytes[at + 4..at + 8].copy_from_slice(b"norm");
        assert!(
            bytes.windows(8).any(|w| w == b"8BIMpass"),
            "the divider still says pass"
        );
        std::fs::write(&path, &bytes).unwrap();
        assert!(load(&path).unwrap().value.layers()[1].pass_through);
        let kids: Vec<&str> = grp.children().unwrap().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(kids, ["Red square", "Hidden"]);
        let red = &grp.children().unwrap()[0];
        assert_eq!(red.blend, BlendMode::Multiply);
        assert!(!red.clip);
        assert!((red.opacity - 0.6).abs() < 0.01);
        let p = red.pixels().unwrap().get_pixel(30, 40).to_straight();
        assert!(
            (p[0] - 1.0).abs() < 0.02 && p[1] < 0.02 && (p[3] - 0.5).abs() < 0.01,
            "{p:?}"
        );
        assert!(red.pixels().unwrap().get_pixel(10, 10).is_transparent());
        let m = red.mask.as_ref().unwrap();
        assert!((m.value(25, 35) - 0.25).abs() < 0.01 && (m.value(100, 100) - 1.0).abs() < 0.01);
        let hidden = &grp.children().unwrap()[1];
        assert!(!hidden.visible);
        assert!(hidden.clip, "the clipping byte round-trips");
        assert_eq!(hidden.pixels().unwrap().get_pixel(250, 150), Rgba::WHITE);

        // The composite of the imported document matches the original, Invert included.
        let o = lumenply_render::composite_raster(&doc);
        let n = lumenply_render::composite_raster(&back);
        let mut maxdiff = 0.0f32;
        let mut at = (0, [0.0; 4], [0.0; 4]);
        for (i, (x, y)) in o.pixels.iter().zip(n.pixels.iter()).enumerate() {
            let (sa, sb) = (x.to_straight(), y.to_straight());
            for (a, b) in sa.iter().zip(sb.iter()) {
                if (a - b).abs() > maxdiff {
                    maxdiff = (a - b).abs();
                    at = (i, sa, sb);
                }
            }
        }
        assert!(
            maxdiff < 0.02,
            "composites differ by {maxdiff} at ({}, {}): {:?} vs {:?}",
            at.0 % 300,
            at.0 / 300,
            at.1,
            at.2
        );
    }

    #[test]
    fn colorize_survives_the_trip() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("bg");
        doc.add_adjustment(Adjustment::HueSaturation {
            hue: 200.0,
            saturation: 0.4,
            lightness: -0.1,
            colorize: true,
        });
        let path = temp("colorize.psd");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap().value;
        let _ = std::fs::remove_file(&path);
        let LayerContent::Adjustment(Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
            colorize,
        }) = back.layers()[1].content
        else {
            panic!("hue/saturation lost");
        };
        assert!(colorize);
        assert_eq!((hue, saturation, lightness), (200.0, 0.4, -0.1));
    }

    #[test]
    fn curves_keep_their_channel_curves_both_ways() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("bg");
        let curves = Adjustment::Curves {
            points: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            channels: [
                vec![[0.0, 0.0], [0.4, 0.2], [1.0, 1.0]],
                vec![],
                vec![[0.0, 0.1], [1.0, 0.9]],
            ],
        };
        doc.add_adjustment(curves);
        let path = temp("curves-rgb.psd");
        save(&path, &doc).unwrap();
        // For checking with an independent reader (psd-tools).
        if let Ok(dir) = std::env::var("LUMENPLY_KEEP_PSD") {
            let _ = std::fs::copy(&path, format!("{dir}/curves-rgb.psd"));
        }
        let back = load(&path).unwrap().value;
        let _ = std::fs::remove_file(&path);
        let LayerContent::Adjustment(Adjustment::Curves { points, channels }) = &back.layers()[1].content
        else {
            panic!("curves lost");
        };
        let q = |v: f32| (v * 255.0).round() / 255.0;
        assert_eq!(points, &vec![[0.0, 0.0], [q(0.5), q(0.6)], [1.0, 1.0]]);
        assert_eq!(channels[0], vec![[0.0, 0.0], [q(0.4), q(0.2)], [1.0, 1.0]]);
        assert!(channels[1].is_empty(), "a straight green stays out");
        assert_eq!(channels[2], vec![[0.0, q(0.1)], [1.0, q(0.9)]]);
        // Photoshop's own layout for channel-only curves: no composite bit.
        let mut d = vec![0u8];
        d.extend_from_slice(&1u16.to_be_bytes());
        d.extend_from_slice(&0b1110u32.to_be_bytes());
        // R straight, G lifted to 51 at black, B down to 204 at white;
        // each point is (output, input).
        for curve in [
            [(0u16, 0u16), (255, 255)],
            [(51, 0), (255, 255)],
            [(0, 0), (204, 255)],
        ] {
            d.extend_from_slice(&2u16.to_be_bytes());
            for (out, inp) in curve {
                d.extend_from_slice(&out.to_be_bytes());
                d.extend_from_slice(&inp.to_be_bytes());
            }
        }
        let Some(Adjustment::Curves { points, channels }) = parse_adjustment(b"curv", &d).unwrap() else {
            panic!("channel-only curves are supported");
        };
        assert_eq!(points, vec![[0.0, 0.0], [1.0, 1.0]]);
        assert_eq!(channels[0], vec![[0.0, 0.0], [1.0, 1.0]]);
        assert_eq!(channels[1], vec![[0.0, 0.2], [1.0, 1.0]]);
        assert_eq!(channels[2], vec![[0.0, 0.0], [1.0, 0.8]]);
    }

    #[test]
    fn adjustment_layers_survive_the_trip() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("bg");
        let adjs = vec![
            Adjustment::BrightnessContrast {
                brightness: 0.25,
                contrast: -0.5,
            },
            Adjustment::HueSaturation {
                hue: 40.0,
                saturation: 0.3,
                lightness: -0.2,
                colorize: false,
            },
            Adjustment::Levels {
                in_black: 0.1,
                in_white: 0.9,
                gamma: 1.5,
                out_black: 0.05,
                out_white: 0.95,
                channels: [
                    lumenply_doc::LevelsChannel {
                        in_black: 0.2,
                        gamma: 0.8,
                        ..Default::default()
                    },
                    Default::default(),
                    lumenply_doc::LevelsChannel {
                        out_white: 0.9,
                        ..Default::default()
                    },
                ],
            },
            Adjustment::Curves {
                points: vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]],
                channels: Default::default(),
            },
            Adjustment::ColorBalance {
                shadows: [0.1, 0.0, -0.2],
                midtones: [0.0, 0.3, 0.0],
                highlights: [-0.4, 0.0, 0.5],
                preserve_luminosity: false,
            },
            Adjustment::Threshold { level: 0.6 },
            Adjustment::Posterize { levels: 5 },
            Adjustment::Invert,
            Adjustment::black_white_default(),
            Adjustment::Exposure {
                exposure: 1.5,
                offset: -0.1,
                gamma: 1.25,
            },
            Adjustment::Vibrance {
                vibrance: 0.45,
                saturation: -0.3,
            },
        ];
        for a in &adjs {
            doc.add_adjustment(a.clone());
        }
        let path = temp("adj.psd");
        let rep = save(&path, &doc).unwrap();
        assert_eq!(
            rep.warnings.len(),
            0,
            "every adjustment exports: {:?}",
            rep.warnings
        );
        let back = load(&path).unwrap().value;
        assert_eq!(back.layer_count(), 12);
        let got: Vec<Adjustment> = back.layers()[1..]
            .iter()
            .map(|l| match &l.content {
                LayerContent::Adjustment(a) => a.clone(),
                _ => panic!("not an adjustment"),
            })
            .collect();
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        match (&got[0], &adjs[0]) {
            (
                Adjustment::BrightnessContrast {
                    brightness: b1,
                    contrast: c1,
                },
                Adjustment::BrightnessContrast {
                    brightness: b2,
                    contrast: c2,
                },
            ) => assert!(close(*b1, *b2) && close(*c1, *c2)),
            _ => panic!("brightness/contrast lost"),
        }
        match &got[1] {
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
                colorize: false,
            } => {
                assert!(close(*hue, 40.0) && close(*saturation, 0.3) && close(*lightness, -0.2))
            }
            _ => panic!("hue/sat lost"),
        }
        match &got[2] {
            Adjustment::Levels {
                in_black,
                gamma,
                out_white,
                channels,
                ..
            } => {
                assert!(close(*in_black, 0.1) && close(*gamma, 1.5) && close(*out_white, 0.95));
                assert!(
                    close(channels[0].in_black, 0.2) && close(channels[0].gamma, 0.8),
                    "red channel levels lost: {channels:?}"
                );
                assert!(channels[1].is_identity(), "green stays identity");
                assert!(close(channels[2].out_white, 0.9), "blue channel levels lost");
            }
            _ => panic!("levels lost"),
        }
        match &got[3] {
            Adjustment::Curves { points, .. } => {
                assert_eq!(points.len(), 4);
                assert!(close(points[1][0], 0.25) && close(points[1][1], 0.15));
            }
            _ => panic!("curves lost"),
        }
        match &got[4] {
            Adjustment::ColorBalance {
                highlights,
                preserve_luminosity,
                ..
            } => {
                assert!(close(highlights[2], 0.5) && !preserve_luminosity)
            }
            _ => panic!("color balance lost"),
        }
        assert!(matches!(got[5], Adjustment::Threshold { level } if close(level, 0.6)));
        assert!(matches!(got[6], Adjustment::Posterize { levels: 5 }));
        assert!(matches!(got[7], Adjustment::Invert));
        match &got[8] {
            Adjustment::BlackWhite { red, green, blue } => {
                // The luminance defaults, through percent and back.
                assert!(
                    close(*red, 0.21) && close(*green, 0.72) && close(*blue, 0.07),
                    "{got:?}"
                );
            }
            _ => panic!("black & white lost"),
        }
        match &got[9] {
            Adjustment::Exposure {
                exposure,
                offset,
                gamma,
            } => {
                assert!(close(*exposure, 1.5) && close(*offset, -0.1) && close(*gamma, 1.25));
            }
            _ => panic!("exposure lost"),
        }
        match &got[10] {
            Adjustment::Vibrance { vibrance, saturation } => {
                assert!(close(*vibrance, 0.45) && close(*saturation, -0.3));
            }
            _ => panic!("vibrance lost"),
        }
    }

    #[test]
    fn rejects_non_psd() {
        let path = temp("nope.psd");
        std::fs::write(&path, b"hello").unwrap();
        assert!(matches!(load(&path), Err(PsdError::NotPsd(_))));
    }

    #[test]
    fn layer_locks_round_trip_through_lspf() {
        let mut doc = Document::new(64, 64);
        let a = doc.add_pixel_layer("Locked pixels");
        doc.layer_mut(a)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(3, 3, Rgba::WHITE);
        let b = doc.add_pixel_layer("Free");
        doc.layer_mut(b)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(4, 4, Rgba::WHITE);
        let g = doc.add_group("Locked group");
        let tp = LayerLocks {
            transparency: true,
            position: true,
            ..LayerLocks::NONE
        };
        let all = LayerLocks {
            all: true,
            ..LayerLocks::NONE
        };
        let px = LayerLocks {
            pixels: true,
            ..LayerLocks::NONE
        };
        doc.layer_mut(a).unwrap().locks = tp;
        doc.layer_mut(g).unwrap().locks = all;
        let adj = doc.add_adjustment(Adjustment::Invert);
        doc.layer_mut(adj).unwrap().locks = px;
        assert_eq!(lock_flags(&tp), 5);
        assert_eq!(lock_flags(&all), 0x8000_0000);
        assert_eq!(locks_from_flags(2), px);
        // Kept for an independent reader (psd-tools, see ADR 0003).
        let path = temp("locks.psd");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap().value;
        let by_name = |n: &str| {
            let mut found = None;
            back.for_each_layer(|l| {
                if l.name == n {
                    found = Some(l.locks);
                }
            });
            found.unwrap()
        };
        assert_eq!(by_name("Locked pixels"), tp);
        assert_eq!(by_name("Free"), LayerLocks::NONE);
        assert_eq!(by_name("Locked group"), all);
        assert_eq!(by_name("Invert"), px);
    }
}
