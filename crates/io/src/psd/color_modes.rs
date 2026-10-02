//! PSD colour modes and depths beyond 8/16-bit RGB.
//!
//! The document model is RGB (premultiplied, linear light), so every other
//! Photoshop mode is converted on import, with one warning that names the
//! conversion; saving writes an RGB PSD as before.
//!
//! - **Grayscale** (mode 1): gray → R = G = B through the embedded gray
//!   profile (resource 1039, e.g. "Dot Gain 20%") when qcms reads it, else
//!   as sRGB-gamma gray. **Duotone** (8) opens as its grayscale data.
//! - **32-bit** (RGB or Grayscale): IEEE floats that Photoshop keeps in
//!   linear light, so they are used as they are and the document opens in
//!   float mode, keeping values above 1. ZIP-with-prediction at this depth
//!   is byte-planar per row ([`unpredict32`]).
//! - **CMYK** (4): channels store 255 − ink. Converted through the embedded
//!   CMYK profile with qcms when it has one, else a Yule–Nielsen-modified
//!   Neugebauer model of U.S. Web Coated (SWOP) v2 ([`SWOP_PRIMARIES`]).
//! - **Lab** (9): CIELAB D50 → XYZ → linear sRGB, Bradford-adapted to D65.
//! - **Indexed** (2): the 768-byte palette in the colour mode data section,
//!   transparency index from resource 1047. **Bitmap** (0, 1-bit): set bits
//!   are black. **Multichannel** (7): the first three channels as RGB (one
//!   or two channels: the first as gray).

use std::borrow::Cow;

use lumenply_doc::adjust::LevelsChannel;
use lumenply_doc::Adjustment;
use lumenply_tiles::{Raster, Rgba};

use super::{bytes_to_samples, inflate, unpackbits, PsdError, Rd};
use crate::srgb_to_linear_f;

pub(super) const BITMAP: u16 = 0;
pub(super) const GRAYSCALE: u16 = 1;
pub(super) const INDEXED: u16 = 2;
pub(super) const RGB: u16 = 3;
pub(super) const CMYK: u16 = 4;
pub(super) const MULTICHANNEL: u16 = 7;
pub(super) const DUOTONE: u16 = 8;
pub(super) const LAB: u16 = 9;

/// Image resource ids this module reads.
const RES_ICC: u16 = 1039;
const RES_TRANSPARENT_INDEX: u16 = 1047;

/// Pixels converted per batch (keeps the scratch buffers small however
/// large the image).
const CHUNK: usize = 4096;

/// How the colour channels of this file become linear RGB.
enum Conv {
    /// 8/16-bit RGB: sRGB-encoded samples.
    Rgb,
    /// 32-bit RGB or Grayscale: linear floats, used as they are.
    Linear,
    /// Gray level (0..=255 steps, interpolated between) → linear value.
    Gray(Box<[f32; 256]>),
    /// CMYK through the embedded profile, or the built-in SWOP model.
    Cmyk(Option<qcms::Transform>),
    Lab,
    /// Palette entries (linear RGB) and the transparent index, if any.
    Indexed(Box<[[f32; 3]; 256]>, Option<u8>),
    /// Multichannel with three or more channels: the first three as RGB.
    Multi,
}

/// The colour mode and depth of a PSD being read.
pub(super) struct ColorMode {
    mode: u16,
    depth: u16,
    channels: u16,
    conv: Conv,
    /// What the conversion did, for the import report.
    note: Option<String>,
}

/// One merged-image plane: full-range u16 samples (8/16-bit and bitmap
/// data) or the floats of a 32-bit file.
pub(super) enum Plane {
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl Plane {
    /// Normalised sample `i` (0..1 for integer data; floats as stored).
    fn get(&self, i: usize) -> f32 {
        match self {
            Plane::U16(p) => p[i] as f32 / 65535.0,
            Plane::F32(p) => p[i],
        }
    }

    /// The plane as full-range u16 samples (floats clamped to 0..1).
    pub(super) fn to_u16(&self) -> Cow<'_, [u16]> {
        match self {
            Plane::U16(p) => Cow::Borrowed(p),
            Plane::F32(p) => Cow::Owned(p.iter().map(|&v| float_to_u16(v)).collect()),
        }
    }
}

fn float_to_u16(v: f32) -> u16 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 65535.0).round() as u16
    } else {
        0
    }
}

fn mode_name(mode: u16) -> &'static str {
    match mode {
        BITMAP => "Bitmap",
        GRAYSCALE => "Grayscale",
        INDEXED => "Indexed Color",
        RGB => "RGB",
        CMYK => "CMYK",
        MULTICHANNEL => "Multichannel",
        DUOTONE => "Duotone",
        LAB => "Lab",
        _ => "unknown",
    }
}

impl ColorMode {
    /// Validate the header's mode, depth and channel count.
    pub(super) fn from_header(mode: u16, depth: u16, channels: u16) -> Result<Self, PsdError> {
        let min_channels = match mode {
            BITMAP | GRAYSCALE | INDEXED | DUOTONE | MULTICHANNEL => 1,
            RGB | LAB => 3,
            CMYK => 4,
            other => return Err(PsdError::Unsupported(format!("colour mode {other}"))),
        };
        if !(min_channels..=56).contains(&channels) {
            return Err(PsdError::Corrupt(format!(
                "{channels} channels in a {} file",
                mode_name(mode)
            )));
        }
        let depth_ok = match mode {
            BITMAP => depth == 1,
            INDEXED => depth == 8,
            RGB | GRAYSCALE => matches!(depth, 8 | 16 | 32),
            _ => matches!(depth, 8 | 16),
        };
        if !depth_ok {
            return Err(PsdError::Unsupported(format!(
                "{depth}-bit {} documents",
                mode_name(mode)
            )));
        }
        Ok(ColorMode {
            mode,
            depth,
            channels,
            conv: Conv::Rgb,
            note: None,
        })
    }

    /// Set up the conversion from the colour mode data section (the
    /// palette of indexed files) and the image resources (ICC profile,
    /// transparent index).
    pub(super) fn read_sections(&mut self, mode_data: &[u8], resources: &[u8]) {
        let icc = resource(resources, RES_ICC);
        let save = "; saving writes an RGB PSD";
        let (conv, note) = match (self.mode, self.depth) {
            (RGB, 32) => (
                Conv::Linear,
                Some("32-bit document opened in float (HDR) mode; saving writes an 8- or 16-bit PSD".into()),
            ),
            (RGB, _) => (Conv::Rgb, None),
            (GRAYSCALE, 32) => (
                Conv::Linear,
                Some(format!(
                    "32-bit Grayscale document converted to RGB and opened in float (HDR) mode{save}"
                )),
            ),
            (GRAYSCALE, _) => {
                let (lut, how) = match icc.and_then(gray_icc_lut) {
                    Some(lut) => (lut, "through its embedded gray profile"),
                    None => (srgb_lut(), "as sRGB gray"),
                };
                (
                    Conv::Gray(lut),
                    Some(format!("Grayscale document converted to RGB {how}{save}")),
                )
            }
            (DUOTONE, _) => (
                Conv::Gray(srgb_lut()),
                Some(format!(
                    "Duotone document opened as its grayscale data (ink colours not applied){save}"
                )),
            ),
            (BITMAP, _) => (
                Conv::Gray(srgb_lut()),
                Some(format!("Bitmap (1-bit) document converted to RGB{save}")),
            ),
            (CMYK, _) => {
                let xf = icc.and_then(cmyk_transform);
                let how = if xf.is_some() {
                    "with its embedded CMYK profile"
                } else {
                    "with a built-in U.S. Web Coated (SWOP) approximation"
                };
                (
                    Conv::Cmyk(xf),
                    Some(format!(
                        "CMYK document converted to RGB {how}; blending and adjustments now work in RGB{save}"
                    )),
                )
            }
            (LAB, _) => (
                Conv::Lab,
                Some(format!("Lab document converted to RGB (sRGB){save}")),
            ),
            (INDEXED, _) => {
                let mut pal = Box::new([[0.0f32; 3]; 256]);
                if mode_data.len() >= 768 {
                    for (i, p) in pal.iter_mut().enumerate() {
                        for (c, v) in p.iter_mut().enumerate() {
                            *v = srgb_to_linear_f(mode_data[c * 256 + i] as f32 / 255.0);
                        }
                    }
                }
                let transparent = resource(resources, RES_TRANSPARENT_INDEX)
                    .filter(|d| d.len() >= 2)
                    .and_then(|d| u8::try_from(u16::from_be_bytes([d[0], d[1]])).ok());
                (
                    Conv::Indexed(pal, transparent),
                    Some(format!("Indexed Color document converted to RGB{save}")),
                )
            }
            (MULTICHANNEL, _) => {
                if self.channels >= 3 {
                    (
                        Conv::Multi,
                        Some(format!(
                            "Multichannel document opened with its first three channels as RGB{save}"
                        )),
                    )
                } else {
                    (
                        Conv::Gray(srgb_lut()),
                        Some(format!(
                            "Multichannel document opened with its first channel as gray{save}"
                        )),
                    )
                }
            }
            _ => (Conv::Rgb, None),
        };
        self.conv = conv;
        self.note = note;
    }

    /// The import warning naming the conversion, if there was one.
    pub(super) fn warning(&self) -> Option<String> {
        self.note.clone()
    }

    /// Colour channels per pixel (ids 0..n in layer records); planes past
    /// these in the merged image are transparency and alpha channels.
    pub(super) fn n_color(&self) -> usize {
        match self.mode {
            RGB | LAB => 3,
            CMYK => 4,
            // Every multichannel channel is an ink: none is transparency.
            MULTICHANNEL => self.channels as usize,
            _ => 1,
        }
    }

    /// Colour channels the conversion reads.
    fn n_used(&self) -> usize {
        match self.conv {
            Conv::Multi => 3,
            _ => self.n_color().min(4),
        }
    }

    /// Parse an adjustment block, mapping per-channel settings of other
    /// modes: a Grayscale document's Levels and Curves keep the gray
    /// channel's settings in record / curve 1 (record 0 is the unused
    /// composite), which become the master; per-channel Levels of CMYK,
    /// Lab and Multichannel documents name no RGB channel and are dropped.
    pub(super) fn adjustment(&self, key: &[u8], data: &[u8]) -> Result<Option<Adjustment>, PsdError> {
        let gray = matches!(self.mode, GRAYSCALE | DUOTONE | BITMAP);
        if gray && key == b"curv" {
            if let Some(adj) = gray_curve(data) {
                return Ok(Some(adj));
            }
        }
        let mut adj = super::parse_adjustment(key, data)?;
        if let Some(Adjustment::Levels {
            in_black,
            in_white,
            gamma,
            out_black,
            out_white,
            channels,
        }) = adj.as_mut()
        {
            if gray {
                let master = LevelsChannel {
                    in_black: *in_black,
                    in_white: *in_white,
                    gamma: *gamma,
                    out_black: *out_black,
                    out_white: *out_white,
                };
                if master.is_identity() {
                    let g = channels[0];
                    (*in_black, *in_white, *gamma, *out_black, *out_white) =
                        (g.in_black, g.in_white, g.gamma, g.out_black, g.out_white);
                }
                *channels = Default::default();
            } else if self.mode != RGB {
                *channels = Default::default();
            }
        }
        Ok(adj)
    }

    /// Whether the document should open in float mode.
    pub(super) fn is_float(&self) -> bool {
        self.depth == 32
    }

    /// Convert `n` pixels to premultiplied linear RGBA. `color(c, i)` is
    /// colour channel `c` of pixel `i` (normalised), `alpha(i)` its
    /// transparency.
    fn convert(
        &self,
        n: usize,
        color: &dyn Fn(usize, usize) -> f32,
        alpha: &dyn Fn(usize) -> f32,
        out: &mut [Rgba],
    ) {
        let nc = self.n_used();
        let mut s = vec![0.0f32; CHUNK * nc];
        let mut rgb = vec![[0.0f32; 3]; CHUNK];
        let mut bytes_in = Vec::new();
        let mut bytes_out = Vec::new();
        let mut start = 0;
        while start < n {
            let m = CHUNK.min(n - start);
            for k in 0..m {
                for c in 0..nc {
                    s[k * nc + c] = color(c, start + k);
                }
            }
            let s = &s[..m * nc];
            let rgb = &mut rgb[..m];
            match &self.conv {
                Conv::Rgb => {
                    for (o, p) in rgb.iter_mut().zip(s.chunks_exact(3)) {
                        *o = [
                            srgb_to_linear_f(p[0]),
                            srgb_to_linear_f(p[1]),
                            srgb_to_linear_f(p[2]),
                        ];
                    }
                }
                Conv::Linear => {
                    for (o, p) in rgb.iter_mut().zip(s.chunks_exact(nc)) {
                        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
                        *o = if nc >= 3 {
                            [finite(p[0]), finite(p[1]), finite(p[2])]
                        } else {
                            [finite(p[0]); 3]
                        };
                    }
                }
                Conv::Gray(lut) => {
                    for (o, p) in rgb.iter_mut().zip(s.chunks_exact(nc)) {
                        *o = [lut_lookup(lut, p[0]); 3];
                    }
                }
                Conv::Cmyk(Some(xf)) => {
                    // qcms takes ink amounts (0 = none) as bytes; the file
                    // stores 255 − ink.
                    bytes_in.clear();
                    bytes_in.extend(
                        s.iter()
                            .map(|&v| ((1.0 - v.clamp(0.0, 1.0)) * 255.0).round() as u8),
                    );
                    bytes_out.resize(m * 3, 0u8);
                    xf.convert(&bytes_in, &mut bytes_out);
                    for (o, p) in rgb.iter_mut().zip(bytes_out.chunks_exact(3)) {
                        *o = [
                            crate::srgb_to_linear(p[0]),
                            crate::srgb_to_linear(p[1]),
                            crate::srgb_to_linear(p[2]),
                        ];
                    }
                }
                Conv::Cmyk(None) => {
                    for (o, p) in rgb.iter_mut().zip(s.chunks_exact(4)) {
                        *o = swop_approx([1.0 - p[0], 1.0 - p[1], 1.0 - p[2], 1.0 - p[3]]);
                    }
                }
                Conv::Lab => {
                    for (o, p) in rgb.iter_mut().zip(s.chunks_exact(3)) {
                        let l = p[0] * 100.0;
                        let a = p[1] * 255.0 - 128.0;
                        let b = p[2] * 255.0 - 128.0;
                        *o = lab_to_linear_srgb(l, a, b).map(|v| v.clamp(0.0, 1.0));
                    }
                }
                Conv::Indexed(pal, _) => {
                    for (o, p) in rgb.iter_mut().zip(s.iter()) {
                        *o = pal[palette_index(*p)];
                    }
                }
                Conv::Multi => {
                    for (o, p) in rgb.iter_mut().zip(s.chunks_exact(3)) {
                        *o = [
                            srgb_to_linear_f(p[0]),
                            srgb_to_linear_f(p[1]),
                            srgb_to_linear_f(p[2]),
                        ];
                    }
                }
            }
            for k in 0..m {
                let i = start + k;
                let mut a = alpha(i);
                if let Conv::Indexed(_, Some(t)) = &self.conv {
                    if palette_index(s[k]) == *t as usize {
                        a = 0.0;
                    }
                }
                let [r, g, b] = rgb[k];
                out[i] = Rgba::from_straight(r, g, b, a);
            }
            start += m;
        }
    }

    /// A pixel layer's raster from its decoded channels: `chans` are
    /// full-range u16 planes by channel id (-1 = transparency), `floats`
    /// the colour planes of a 32-bit file. Missing colour channels read as
    /// 0, a missing transparency as opaque.
    pub(super) fn layer_raster(
        &self,
        chans: &[(i16, Vec<u16>)],
        floats: &[(i16, Vec<f32>)],
        w: u32,
        h: u32,
    ) -> Raster {
        let n = (w * h) as usize;
        let mut raster = Raster::new(w, h);
        let plane = |id: i16| -> Option<Plane2<'_>> {
            if let Some((_, p)) = floats.iter().find(|(i, p)| *i == id && p.len() >= n) {
                return Some(Plane2::F32(p));
            }
            chans
                .iter()
                .find(|(i, p)| *i == id && p.len() >= n)
                .map(|(_, p)| Plane2::U16(p))
        };
        let colors: Vec<Option<Plane2>> = (0..self.n_used() as i16).map(plane).collect();
        let a = chans
            .iter()
            .find(|(i, p)| *i == -1 && p.len() >= n)
            .map(|(_, p)| p.as_slice());
        let color = |c: usize, i: usize| colors[c].as_ref().map_or(0.0, |p| p.get(i));
        let alpha = |i: usize| a.map_or(1.0, |p| p[i] as f32 / 65535.0);
        self.convert(n, &color, &alpha, &mut raster.pixels);
        raster
    }

    /// The merged image as a raster: `planes` from [`read_composite`], the
    /// last `n_alpha` of them named alpha channels. A plane after the
    /// colour channels that is not a named one is transparency.
    pub(super) fn composite_raster(&self, planes: &[Plane], n_alpha: usize, w: u32, h: u32) -> Raster {
        let n = (w * h) as usize;
        let mut raster = Raster::new(w, h);
        let nc = self.n_color();
        let alpha_plane = (planes.len() > nc + n_alpha).then(|| &planes[nc]);
        let color = |c: usize, i: usize| planes.get(c).map_or(0.0, |p| p.get(i));
        let alpha = |i: usize| alpha_plane.map_or(1.0, |p| p.get(i).clamp(0.0, 1.0));
        self.convert(n, &color, &alpha, &mut raster.pixels);
        raster
    }
}

/// A borrowed layer plane of either sample type.
enum Plane2<'a> {
    U16(&'a [u16]),
    F32(&'a [f32]),
}

impl Plane2<'_> {
    fn get(&self, i: usize) -> f32 {
        match self {
            Plane2::U16(p) => p[i] as f32 / 65535.0,
            Plane2::F32(p) => p[i],
        }
    }
}

/// A Grayscale document's `curv` block whose only curve is the gray
/// channel's (channel bit 1, no composite curve) as a master Curves.
fn gray_curve(data: &[u8]) -> Option<Adjustment> {
    let mut d = Rd::new(data);
    d.skip(1).ok()?;
    if d.u16().ok()? != 1 {
        return None;
    }
    let mask = d.u32().ok()?;
    if mask & 1 != 0 || mask & 2 == 0 {
        return None;
    }
    let n = d.u16().ok()? as usize;
    let mut points = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        let out = d.u16().ok()? as f32 / 255.0;
        let input = d.u16().ok()? as f32 / 255.0;
        points.push([input, out]);
    }
    Some(Adjustment::Curves { points })
}

fn palette_index(v: f32) -> usize {
    (v * 255.0).round().clamp(0.0, 255.0) as usize
}

/// Gray level → linear through the sRGB curve.
fn srgb_lut() -> Box<[f32; 256]> {
    let mut lut = Box::new([0.0f32; 256]);
    for (i, v) in lut.iter_mut().enumerate() {
        *v = srgb_to_linear_f(i as f32 / 255.0);
    }
    lut
}

/// Gray level → linear through an embedded gray ICC profile (qcms, to
/// sRGB, then decoded). `None` when qcms cannot read or use the profile.
fn gray_icc_lut(icc: &[u8]) -> Option<Box<[f32; 256]>> {
    let src = qcms::Profile::new_from_slice(icc, false)?;
    let dst = qcms::Profile::new_sRGB();
    let xf = qcms::Transform::new_to(
        &src,
        &dst,
        qcms::DataType::Gray8,
        qcms::DataType::RGB8,
        qcms::Intent::Perceptual,
    )?;
    let levels: Vec<u8> = (0..=255u8).collect();
    let mut rgb = vec![0u8; 256 * 3];
    xf.convert(&levels, &mut rgb);
    let mut lut = Box::new([0.0f32; 256]);
    for (v, p) in lut.iter_mut().zip(rgb.chunks_exact(3)) {
        // A neutral input stays neutral; average away rounding.
        let g = (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
        *v = srgb_to_linear_f(g / 255.0);
    }
    Some(lut)
}

/// A gray level (0..1) through a 256-entry table, interpolated so 16-bit
/// data keeps its precision.
fn lut_lookup(lut: &[f32; 256], v: f32) -> f32 {
    let t = v.clamp(0.0, 1.0) * 255.0;
    let i = (t as usize).min(254);
    let f = t - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * f
}

/// A CMYK → sRGB transform from an embedded CMYK profile.
fn cmyk_transform(icc: &[u8]) -> Option<qcms::Transform> {
    let src = qcms::Profile::new_from_slice(icc, false)?;
    let dst = qcms::Profile::new_sRGB();
    qcms::Transform::new_to(
        &src,
        &dst,
        qcms::DataType::CMYK,
        qcms::DataType::RGB8,
        qcms::Intent::Perceptual,
    )
}

/// Linear sRGB of the 16 Neugebauer primaries of U.S. Web Coated (SWOP)
/// v2, indexed `c·8 + m·4 + y·2 + k` (each ink off or solid). Sampled from
/// the profile with LittleCMS (perceptual intent) and refined by a
/// least-squares fit of the model below over 20 000 random CMYK values:
/// mean error 4.6/255 against the profile, where the naive
/// (1 − C)(1 − K) formula is off by 15.5/255.
const SWOP_PRIMARIES: [[f32; 3]; 16] = [
    [1.0, 1.0, 1.0],
    [0.01678, 0.01374, 0.01448],
    [1.0, 0.88792, 0.00037],
    [0.00912, 0.01037, 0.00014],
    [0.8388, 0.00038, 0.26225],
    [0.00752, 0.00011, 0.00055],
    [0.84687, 0.01167, 0.01767],
    [0.00805, 0.00012, 0.00026],
    [0.0, 0.42327, 0.86316],
    [0.0, 0.00166, 0.01767],
    [0.00006, 0.38133, 0.08023],
    [0.00004, 0.00165, 0.00012],
    [0.0273, 0.02959, 0.28744],
    [0.00009, 0.00006, 0.00029],
    [0.03689, 0.03691, 0.04092],
    [0.00008, 0.00005, 0.00019],
];

/// Ink amounts (0..1, 1 = solid) → linear sRGB with the Yule–Nielsen
/// modified Neugebauer model (n = 2): Demichel-weighted square roots of
/// the primaries, squared.
fn swop_approx(ink: [f32; 4]) -> [f32; 3] {
    let ink = ink.map(|v| v.clamp(0.0, 1.0));
    let mut acc = [0.0f32; 3];
    for (idx, prim) in SWOP_PRIMARIES.iter().enumerate() {
        let mut w = 1.0;
        for (ch, &v) in ink.iter().enumerate() {
            let on = idx >> (3 - ch) & 1 == 1;
            w *= if on { v } else { 1.0 - v };
        }
        if w > 0.0 {
            for c in 0..3 {
                acc[c] += w * prim[c].sqrt();
            }
        }
    }
    acc.map(|v| v * v)
}

/// A descriptor's CMYK colour (ink percentages) → linear sRGB, through the
/// built-in SWOP model.
pub(super) fn cmyk_percent_to_linear(ink: [f64; 4]) -> [f32; 3] {
    swop_approx(ink.map(|v| (v / 100.0) as f32))
}

/// In 32-bit documents Photoshop stores the RGB of fill and shape colours
/// as linear 0..255, but the descriptor reader decodes them as sRGB:
/// re-encode them so they mean what they say.
pub(super) fn linear_descriptor_colors(doc: &mut lumenply_doc::Document) {
    use lumenply_doc::{Fill, LayerContent};
    let fix = |c: &mut [f32; 3]| *c = c.map(crate::linear_to_srgb_f);
    let fix_fill = |f: &mut Fill| match f {
        Fill::Solid { color } => fix(color),
        Fill::Gradient { gradient, .. } => gradient.stops.iter_mut().for_each(|s| fix(&mut s.color)),
    };
    doc.for_each_layer_mut(|l| match &mut l.content {
        LayerContent::Fill(f) => fix_fill(&mut f.fill),
        LayerContent::Shape(s) => {
            if let Some(f) = s.fill.as_mut() {
                fix_fill(f);
            }
            if let Some(st) = s.stroke.as_mut() {
                fix(&mut st.color);
            }
        }
        _ => {}
    });
}

/// CIELAB (D50, as Photoshop stores it) → linear sRGB (D65), through XYZ
/// with Bradford chromatic adaptation. Not clamped.
pub(super) fn lab_to_linear_srgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    const D: f32 = 6.0 / 29.0;
    let finv = |t: f32| {
        if t > D {
            t * t * t
        } else {
            3.0 * D * D * (t - 4.0 / 29.0)
        }
    };
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    // D50 reference white.
    let (x, y, z) = (0.964_22 * finv(fx), finv(fy), 0.825_21 * finv(fz));
    // XYZ (D50) → linear sRGB (D65), Bradford-adapted (Lindbloom).
    [
        3.133_856 * x - 1.616_866_7 * y - 0.490_614_6 * z,
        -0.978_768_4 * x + 1.916_141_5 * y + 0.033_454 * z,
        0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z,
    ]
}

/// The data of image resource `id`, if the section has it.
fn resource(res: &[u8], id: u16) -> Option<&[u8]> {
    let mut at = 0usize;
    while at + 12 <= res.len() && &res[at..at + 4] == b"8BIM" {
        let rid = u16::from_be_bytes([res[at + 4], res[at + 5]]);
        // Pascal name: length byte + bytes, padded to an even total.
        let name_total = 1 + res[at + 6] as usize;
        let p = at + 6 + name_total + (name_total % 2);
        let size_bytes = res.get(p..p + 4)?;
        let size = u32::from_be_bytes([size_bytes[0], size_bytes[1], size_bytes[2], size_bytes[3]]) as usize;
        let data = res.get(p + 4..(p + 4).checked_add(size)?)?;
        if rid == id {
            return Some(data);
        }
        at = p + 4 + size + (size % 2);
    }
    None
}

/// Undo 32-bit ZIP prediction: each row's bytes were delta-coded as one
/// run after splitting the row's big-endian floats into four byte planes
/// (all first bytes, then all second bytes, …).
pub(super) fn unpredict32(bytes: &mut [u8], w: usize) {
    let row_len = w * 4;
    if row_len == 0 {
        return;
    }
    let mut planar = vec![0u8; row_len];
    for row in bytes.chunks_exact_mut(row_len) {
        for i in 1..row_len {
            row[i] = row[i].wrapping_add(row[i - 1]);
        }
        planar.copy_from_slice(row);
        for x in 0..w {
            for b in 0..4 {
                row[x * 4 + b] = planar[b * w + x];
            }
        }
    }
}

/// Undo 8/16-bit ZIP prediction (a running sum along each row of
/// samples, wrapping at the sample width).
fn unpredict(bytes: &mut [u8], w: usize, depth: u16) {
    match depth {
        8 if w > 0 => {
            for row in bytes.chunks_exact_mut(w) {
                for i in 1..w {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
            }
        }
        16 if w > 0 => {
            for row in bytes.chunks_exact_mut(w * 2) {
                for i in 1..w {
                    let prev = u16::from_be_bytes([row[2 * i - 2], row[2 * i - 1]]);
                    let cur = u16::from_be_bytes([row[2 * i], row[2 * i + 1]]);
                    row[2 * i..2 * i + 2].copy_from_slice(&cur.wrapping_add(prev).to_be_bytes());
                }
            }
        }
        32 => unpredict32(bytes, w),
        _ => {}
    }
}

/// Bytes in one row of `w` samples at `depth` bits (1-bit rows pad to a
/// whole byte).
fn row_bytes(w: usize, depth: u16) -> usize {
    (w * depth as usize).div_ceil(8)
}

/// Decoded plane bytes → a [`Plane`]: 1-bit rows unpack with set bits
/// black, 8/16-bit samples scale to full u16, 32-bit samples are floats.
fn to_plane(bytes: &[u8], w: usize, h: usize, depth: u16) -> Plane {
    match depth {
        1 => {
            let rb = row_bytes(w, 1);
            let mut out = Vec::with_capacity(w * h);
            for row in bytes.chunks_exact(rb).take(h) {
                for x in 0..w {
                    let bit = row[x / 8] >> (7 - x % 8) & 1;
                    out.push(if bit == 1 { 0 } else { 65535 });
                }
            }
            Plane::U16(out)
        }
        32 => Plane::F32(
            bytes
                .chunks_exact(4)
                .map(|b| f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
        ),
        _ => Plane::U16(bytes_to_samples(bytes, depth as usize / 8)),
    }
}

/// One layer channel's decoded, unpredicted bytes (raw, RLE or ZIP ±
/// prediction) at any depth. ZIP consumes the rest of `rd`, which for a
/// layer channel is exactly its own data.
pub(super) fn decode_plane_bytes(
    rd: &mut Rd,
    w: usize,
    h: usize,
    depth: u16,
    psb: bool,
) -> Result<Vec<u8>, PsdError> {
    let compression = rd.u16()?;
    let rb = row_bytes(w, depth);
    let plane_len = rb * h;
    match compression {
        0 => Ok(rd.bytes(plane_len)?.to_vec()),
        1 => {
            let mut counts = Vec::with_capacity(h);
            for _ in 0..h {
                counts.push(if psb {
                    rd.u32()? as usize
                } else {
                    rd.u16()? as usize
                });
            }
            let remaining = rd.buf.len().saturating_sub(rd.pos);
            let mut out = Vec::with_capacity(plane_len.min(remaining.saturating_mul(128)));
            for c in counts {
                out.extend(unpackbits(rd.bytes(c)?, rb)?);
            }
            Ok(out)
        }
        2 | 3 => {
            let rest = &rd.buf[rd.pos..];
            rd.pos = rd.buf.len();
            let mut bytes = inflate(rest, plane_len)?;
            if bytes.len() != plane_len {
                return Err(PsdError::Corrupt(format!(
                    "zip channel inflated to {} of {plane_len} bytes",
                    bytes.len()
                )));
            }
            if compression == 3 {
                unpredict(&mut bytes, w, depth);
            }
            Ok(bytes)
        }
        other => Err(PsdError::Unsupported(format!("compression method {other}"))),
    }
}

/// A 32-bit layer colour channel as floats.
pub(super) fn decode_f32(rd: &mut Rd, w: usize, h: usize, psb: bool) -> Result<Vec<f32>, PsdError> {
    let bytes = decode_plane_bytes(rd, w, h, 32, psb)?;
    match to_plane(&bytes, w, h, 32) {
        Plane::F32(p) => Ok(p),
        Plane::U16(_) => unreachable!("32-bit planes are floats"),
    }
}

/// Read the merged image section: its compression word, then `nch`
/// planes of `w × h` at `depth` bits — raw, RLE (row counts for every
/// plane first), or one ZIP stream for all planes (± prediction).
pub(super) fn read_composite(
    rd: &mut Rd,
    w: usize,
    h: usize,
    nch: usize,
    depth: u16,
    psb: bool,
) -> Result<Vec<Plane>, PsdError> {
    let compression = rd.u16()?;
    let rb = row_bytes(w, depth);
    let plane_len = rb * h;
    match compression {
        0 => (0..nch)
            .map(|_| rd.bytes(plane_len).map(|b| to_plane(b, w, h, depth)))
            .collect(),
        1 => {
            let mut counts = Vec::with_capacity(h * nch);
            for _ in 0..h * nch {
                counts.push(if psb {
                    rd.u32()? as usize
                } else {
                    rd.u16()? as usize
                });
            }
            let mut planes = Vec::with_capacity(nch);
            for c in 0..nch {
                let remaining = rd.buf.len().saturating_sub(rd.pos);
                let mut plane = Vec::with_capacity(plane_len.min(remaining.saturating_mul(128)));
                for y in 0..h {
                    plane.extend(unpackbits(rd.bytes(counts[c * h + y])?, rb)?);
                }
                planes.push(to_plane(&plane, w, h, depth));
            }
            Ok(planes)
        }
        2 | 3 => {
            let total = plane_len * nch;
            let rest = &rd.buf[rd.pos..];
            rd.pos = rd.buf.len();
            let mut bytes = inflate(rest, total)?;
            if bytes.len() != total {
                return Err(PsdError::Corrupt(format!(
                    "zip composite inflated to {} of {total} bytes",
                    bytes.len()
                )));
            }
            if compression == 3 {
                unpredict(&mut bytes, w, depth);
            }
            Ok(bytes
                .chunks_exact(plane_len.max(1))
                .take(nch)
                .map(|b| to_plane(b, w, h, depth))
                .collect())
        }
        other => Err(PsdError::Unsupported(format!("composite compression {other}"))),
    }
}

/// Photoshop writes the layers of 16- and 32-bit documents into an `Lr16`
/// / `Lr32` block among the global tagged blocks, leaving the regular
/// layer info empty. `pos` is just after that empty layer info, `end` the
/// end of the layer-and-mask section; returns the block's data range.
pub(super) fn deep_layer_info(buf: &[u8], pos: usize, end: usize, psb: bool) -> Option<(usize, usize)> {
    let end = end.min(buf.len());
    let u32_at = |p: usize| -> Option<usize> {
        let b = buf.get(p..p + 4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    // Global layer mask info.
    let mut at = pos.checked_add(4)?.checked_add(u32_at(pos)?)?;
    while at + 12 <= end {
        let sig = &buf[at..at + 4];
        if sig != b"8BIM" && sig != b"8B64" {
            // Tolerate writers that pad blocks to 4 bytes or don't pad.
            at += 1;
            continue;
        }
        let key = &buf[at + 4..at + 8];
        let deep = matches!(key, b"Lr16" | b"Lr32" | b"Layr");
        // In PSB a handful of keys (these among them) carry 8-byte lengths.
        let wide = psb
            && matches!(
                key,
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
        let (len, data) = if wide {
            let b = buf.get(at + 8..at + 16)?;
            let len = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            (usize::try_from(len).ok()?, at + 16)
        } else {
            (u32_at(at + 8)?, at + 12)
        };
        let data_end = data.checked_add(len)?;
        if data_end > end {
            return None;
        }
        if deep {
            return Some((data, len));
        }
        at = data_end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_to_srgb_f;
    use lumenply_doc::Document;

    fn u16s(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn u32s(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    /// A PSD with the given header, colour mode data, resources, layer
    /// and mask section body and merged image section.
    #[allow(clippy::too_many_arguments)]
    fn psd(
        mode: u16,
        depth: u16,
        channels: u16,
        (w, h): (u32, u32),
        mode_data: &[u8],
        resources: &[u8],
        layers: &[u8],
        composite: &[u8],
    ) -> Vec<u8> {
        let mut f = b"8BPS".to_vec();
        u16s(&mut f, 1);
        f.extend_from_slice(&[0; 6]);
        u16s(&mut f, channels);
        u32s(&mut f, h);
        u32s(&mut f, w);
        u16s(&mut f, depth);
        u16s(&mut f, mode);
        u32s(&mut f, mode_data.len() as u32);
        f.extend_from_slice(mode_data);
        u32s(&mut f, resources.len() as u32);
        f.extend_from_slice(resources);
        u32s(&mut f, layers.len() as u32);
        f.extend_from_slice(layers);
        f.extend_from_slice(composite);
        f
    }

    /// An image resource block.
    fn res(id: u16, data: &[u8]) -> Vec<u8> {
        let mut r = b"8BIM".to_vec();
        u16s(&mut r, id);
        r.extend_from_slice(&[0, 0]);
        u32s(&mut r, data.len() as u32);
        r.extend_from_slice(data);
        if data.len() % 2 == 1 {
            r.push(0);
        }
        r
    }

    /// Layer info content (count, one record, channel data) for a layer
    /// covering (0, 0, w, h) with the given (id, encoded data) channels.
    fn one_layer(w: u32, h: u32, chans: &[(i16, Vec<u8>)]) -> Vec<u8> {
        let mut r = Vec::new();
        u16s(&mut r, 1);
        for v in [0, 0, h, w] {
            u32s(&mut r, v); // top, left, bottom, right
        }
        u16s(&mut r, chans.len() as u16);
        for (id, data) in chans {
            u16s(&mut r, *id as u16);
            u32s(&mut r, data.len() as u32);
        }
        r.extend_from_slice(b"8BIMnorm");
        r.extend_from_slice(&[255, 0, 0, 0]);
        u32s(&mut r, 12);
        r.extend_from_slice(&[0; 8]); // no mask, no blending ranges
        r.extend_from_slice(&[0, 0, 0, 0]); // empty name, padded
        for (_, data) in chans {
            r.extend_from_slice(data);
        }
        r
    }

    /// Layer and mask section body with the layers in the regular info.
    fn regular(info: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        let len = info.len() + info.len() % 2;
        u32s(&mut s, len as u32);
        s.extend_from_slice(info);
        s.resize(4 + len, 0);
        s
    }

    /// Layer and mask section body as Photoshop writes 16/32-bit files:
    /// empty layer info, empty global mask, then an `Lr16`/`Lr32` block.
    fn deep(key: &[u8; 4], info: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        u32s(&mut s, 0);
        u32s(&mut s, 0);
        s.extend_from_slice(b"8BIM");
        s.extend_from_slice(key);
        u32s(&mut s, info.len() as u32);
        s.extend_from_slice(info);
        while s.len() % 4 != 0 {
            s.push(0);
        }
        s
    }

    fn raw8(v: &[u8]) -> Vec<u8> {
        let mut d = vec![0, 0];
        d.extend_from_slice(v);
        d
    }

    fn load_bytes(name: &str, bytes: &[u8]) -> super::super::Report<Document> {
        let dir = std::env::temp_dir().join("lumenply-psd-colormodes");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        super::super::load(&path).unwrap()
    }

    fn px(doc: &Document, layer: usize, x: i32, y: i32) -> [f32; 4] {
        doc.layers()[layer]
            .pixels()
            .unwrap()
            .get_pixel(x, y)
            .to_straight()
    }

    fn close(got: f32, want: f32, tol: f32, what: &str) {
        assert!((got - want).abs() <= tol, "{what}: got {got}, want {want}");
    }

    #[test]
    fn grayscale_opens_as_equal_rgb_with_one_warning() {
        // Flat, 2 channels: gray + transparency.
        let mut comp = vec![0, 0];
        comp.extend_from_slice(&[0, 128, 255, 64]);
        comp.extend_from_slice(&[255, 255, 255, 0]);
        let f = psd(GRAYSCALE, 8, 2, (2, 2), &[], &[], &[], &comp);
        let rep = load_bytes("gray-flat.psd", &f);
        assert_eq!(rep.warnings.len(), 1, "{:?}", rep.warnings);
        assert!(rep.warnings[0].starts_with("Grayscale document converted to RGB as sRGB gray"));
        assert!(rep.warnings[0].contains("saving writes an RGB PSD"));
        let p = px(&rep.value, 0, 1, 0);
        for v in &p[..3] {
            close(*v, 0.215_860_5, 1e-5, "gray 128 is sRGB 128 on every channel");
        }
        assert_eq!(px(&rep.value, 0, 0, 1)[3], 1.0);
        assert_eq!(px(&rep.value, 0, 1, 1)[3], 0.0, "transparency plane");

        // Layered: channels -1 and 0.
        let info = one_layer(2, 1, &[(-1, raw8(&[255, 51])), (0, raw8(&[255, 0]))]);
        let f = psd(GRAYSCALE, 8, 1, (2, 1), &[], &[], &regular(&info), &raw8(&[0, 0]));
        let doc = load_bytes("gray-layer.psd", &f).value;
        assert_eq!(px(&doc, 0, 0, 0), [1.0, 1.0, 1.0, 1.0]);
        let p = px(&doc, 0, 1, 0);
        assert_eq!(&p[..3], &[0.0, 0.0, 0.0]);
        close(p[3], 0.2, 1e-4, "51/255 transparency");
    }

    /// A minimal gray ICC profile whose tone curve is a pure gamma.
    fn gray_icc(gamma: f32) -> Vec<u8> {
        let mut p = Vec::new();
        u32s(&mut p, 160);
        u32s(&mut p, 0);
        u32s(&mut p, 0x0210_0000);
        p.extend_from_slice(b"mntrGRAYXYZ ");
        p.extend_from_slice(&[0; 12]);
        p.extend_from_slice(b"acsp");
        p.extend_from_slice(&[0; 24]);
        u32s(&mut p, 0); // intent
        for v in [0x0000_F6D6u32, 0x0001_0000, 0x0000_D32D] {
            u32s(&mut p, v); // D50
        }
        p.resize(128, 0);
        u32s(&mut p, 1);
        p.extend_from_slice(b"kTRC");
        u32s(&mut p, 144);
        u32s(&mut p, 14);
        p.extend_from_slice(b"curv");
        u32s(&mut p, 0);
        u32s(&mut p, 1);
        u16s(&mut p, (gamma * 256.0) as u16);
        p.resize(160, 0);
        p
    }

    #[test]
    fn embedded_gray_profile_sets_the_tone_curve() {
        // Gamma 1.0: gray 128 means 50% linear light, not sRGB's 21.6%.
        let resources = res(RES_ICC, &gray_icc(1.0));
        let f = psd(GRAYSCALE, 8, 1, (1, 1), &[], &resources, &[], &raw8(&[128]));
        let rep = load_bytes("gray-icc.psd", &f);
        assert!(rep.warnings[0].contains("through its embedded gray profile"));
        let p = px(&rep.value, 0, 0, 0);
        // qcms answers in 8-bit sRGB: allow its rounding.
        close(p[0], 128.0 / 255.0, 0.005, "gamma-1.0 gray");
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
        // 16-bit samples interpolate between the 256 table entries.
        let mut comp = vec![0, 0];
        comp.extend_from_slice(&16384u16.to_be_bytes());
        let f = psd(GRAYSCALE, 16, 1, (1, 1), &[], &resources, &[], &comp);
        close(
            px(&load_bytes("gray-icc16.psd", &f).value, 0, 0, 0)[0],
            0.25,
            0.005,
            "16-bit",
        );
    }

    /// ZIP-with-prediction coding of 32-bit floats, as Photoshop writes it.
    fn zip_pred32(samples: &[f32], w: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for row in samples.chunks(w) {
            let mut planar = vec![0u8; w * 4];
            for (x, v) in row.iter().enumerate() {
                for (b, byte) in v.to_be_bytes().iter().enumerate() {
                    planar[b * w + x] = *byte;
                }
            }
            let mut prev = 0u8;
            for (i, b) in planar.iter().enumerate() {
                bytes.push(if i == 0 { *b } else { b.wrapping_sub(prev) });
                prev = *b;
            }
        }
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&bytes).unwrap();
        let mut out = vec![0, 3];
        out.extend(enc.finish().unwrap());
        out
    }

    fn raw32(v: &[f32]) -> Vec<u8> {
        let mut d = vec![0, 0];
        for x in v {
            d.extend_from_slice(&x.to_be_bytes());
        }
        d
    }

    #[test]
    fn thirty_two_bit_files_open_linear_in_float_mode() {
        // Layers in an Lr32 block; red is ZIP-predicted and holds HDR 2.5.
        let reds = [2.5f32, 0.25, 1.0, 0.0];
        let info = one_layer(
            2,
            2,
            &[
                (-1, raw32(&[1.0, 0.5, 1.0, 1.0])),
                (0, zip_pred32(&reds, 2)),
                (1, raw32(&[0.25; 4])),
                (2, raw32(&[0.0; 4])),
            ],
        );
        let mut comp = vec![0, 0];
        comp.extend_from_slice(&[0; 2 * 2 * 4 * 3]);
        let f = psd(RGB, 32, 3, (2, 2), &[], &[], &deep(b"Lr32", &info), &comp);
        let rep = load_bytes("rgb32.psd", &f);
        let doc = &rep.value;
        assert!(doc.float_mode, "32-bit opens in float mode");
        assert_eq!(doc.layers().len(), 1, "the layer came from Lr32");
        assert!(rep.warnings[0].starts_with("32-bit document opened in float (HDR) mode"));
        assert_eq!(px(doc, 0, 0, 0), [2.5, 0.25, 0.0, 1.0], "linear, HDR kept");
        let p = px(doc, 0, 1, 0);
        close(p[0], 0.25, 1e-6, "no sRGB decode");
        close(p[3], 0.5, 1e-4, "float transparency");
        assert_eq!(px(doc, 0, 0, 1)[0], 1.0);

        // Flat 32-bit gray: the float is the linear value.
        let f = psd(GRAYSCALE, 32, 1, (2, 1), &[], &[], &[], &raw32(&[0.18, 4.0]));
        let rep = load_bytes("gray32.psd", &f);
        assert!(rep.value.float_mode);
        assert!(rep.warnings[0].starts_with("32-bit Grayscale document converted to RGB"));
        assert_eq!(px(&rep.value, 0, 0, 0), [0.18, 0.18, 0.18, 1.0]);
        assert_eq!(px(&rep.value, 0, 1, 0), [4.0, 4.0, 4.0, 1.0]);
    }

    #[test]
    fn predicted_32_bit_planes_decode_byte_planar() {
        let v = [1.5f32, -2.0, 0.333, 1e-6, 7.0, 0.5];
        let enc = zip_pred32(&v, 3);
        let mut rd = Rd::new(&enc);
        assert_eq!(decode_f32(&mut rd, 3, 2, false).unwrap(), v.to_vec());
    }

    #[test]
    fn cmyk_without_a_profile_uses_the_swop_model() {
        // 16-bit flat CMYK + transparency; stored values are 65535 − ink.
        let (n, s) = (65535u16, 32768u16);
        let pixels: [[u16; 5]; 4] = [
            [n, n, n, n, n], // no ink
            [0, n, n, n, n], // solid cyan
            [s, n, n, n, n], // 50% cyan
            [n, n, n, 0, 0], // solid black, transparent
        ];
        let mut comp = vec![0, 0];
        for c in 0..5 {
            for p in &pixels {
                comp.extend_from_slice(&p[c].to_be_bytes());
            }
        }
        let f = psd(CMYK, 16, 5, (4, 1), &[], &[], &[], &comp);
        let rep = load_bytes("cmyk16.psd", &f);
        assert_eq!(rep.warnings.len(), 1);
        assert!(
            rep.warnings[0].contains("CMYK document converted to RGB with a built-in U.S. Web Coated (SWOP)")
        );
        assert_eq!(px(&rep.value, 0, 0, 0), [1.0, 1.0, 1.0, 1.0]);
        let cyan = px(&rep.value, 0, 1, 0);
        close(cyan[0], 0.0, 1e-6, "cyan R");
        close(cyan[1], 0.42327, 1e-5, "cyan G");
        close(cyan[2], 0.86316, 1e-5, "cyan B");
        // Yule–Nielsen (n = 2) between paper and solid cyan.
        let half = px(&rep.value, 0, 2, 0);
        close(half[0], 0.250_008, 1e-4, "50% cyan R");
        close(half[1], 0.681_118, 1e-4, "50% cyan G");
        close(half[2], 0.930_323, 1e-4, "50% cyan B");
        assert_eq!(px(&rep.value, 0, 3, 0)[3], 0.0, "fifth plane is transparency");
        // The naive (1 − C)(1 − K) formula would make solid cyan (0, 1, 1).
        assert!(cyan[1] < 0.5);
    }

    #[test]
    fn cmyk_layers_convert_and_adjustments_map_channels() {
        let chans = [
            (-1, raw8(&[255])),
            (0, raw8(&[255])),
            (1, raw8(&[0])),
            (2, raw8(&[255])),
            (3, raw8(&[255])),
        ];
        let info = one_layer(1, 1, &chans);
        let f = psd(
            CMYK,
            8,
            4,
            (1, 1),
            &[],
            &[],
            &regular(&info),
            &raw8(&[0, 0, 0, 0]),
        );
        let doc = load_bytes("cmyk-layer.psd", &f).value;
        let m = px(&doc, 0, 0, 0);
        close(m[0], 0.8388, 1e-5, "solid magenta R");
        close(m[1], 0.00038, 1e-5, "solid magenta G");
        close(m[2], 0.26225, 1e-5, "solid magenta B");

        // Levels: the composite record, then one record per channel.
        let mut levl = Vec::new();
        u16s(&mut levl, 2);
        for rec in [[0i16, 255, 0, 255, 100], [40, 200, 0, 255, 150]] {
            for v in rec {
                levl.extend_from_slice(&v.to_be_bytes());
            }
        }
        for _ in 0..27 {
            for v in [0i16, 255, 0, 255, 100] {
                levl.extend_from_slice(&v.to_be_bytes());
            }
        }
        let cm = ColorMode::from_header(CMYK, 8, 4).unwrap();
        let Some(Adjustment::Levels {
            channels, in_black, ..
        }) = cm.adjustment(b"levl", &levl).unwrap()
        else {
            panic!("levels")
        };
        assert_eq!(in_black, 0.0);
        assert!(
            channels.iter().all(|c| c.is_identity()),
            "cyan levels are not red levels"
        );

        // A Grayscale document's levels live in record 1: they become the master.
        let cm = ColorMode::from_header(GRAYSCALE, 8, 1).unwrap();
        let Some(Adjustment::Levels {
            in_black,
            in_white,
            gamma,
            channels,
            ..
        }) = cm.adjustment(b"levl", &levl).unwrap()
        else {
            panic!("levels")
        };
        close(in_black, 40.0 / 255.0, 1e-6, "gray in black");
        close(in_white, 200.0 / 255.0, 1e-6, "gray in white");
        close(gamma, 1.5, 1e-6, "gray gamma");
        assert!(channels.iter().all(|c| c.is_identity()));

        // A Grayscale curv block with only the gray channel's curve.
        let mut curv = vec![0];
        u16s(&mut curv, 1);
        u32s(&mut curv, 2);
        u16s(&mut curv, 2);
        for v in [0u16, 64, 255, 255] {
            u16s(&mut curv, v); // (out, in) pairs
        }
        let Some(Adjustment::Curves { points }) = cm.adjustment(b"curv", &curv).unwrap() else {
            panic!("curves")
        };
        assert_eq!(points, vec![[64.0 / 255.0, 0.0], [1.0, 1.0]]);
    }

    #[test]
    fn lab_converts_through_d50_xyz_with_bradford() {
        // 16-bit Lab: L 0..65535 → 0..100, a/b offset 128·257.
        let pixels: [[u16; 3]; 3] = [
            [65535, 32896, 32896], // L 100: white
            [32768, 32896, 32896], // L 50: neutral
            [35579, 53664, 50858], // sRGB red's Lab (D50)
        ];
        let mut comp = vec![0, 0];
        for c in 0..3 {
            for p in &pixels {
                comp.extend_from_slice(&p[c].to_be_bytes());
            }
        }
        let f = psd(LAB, 16, 3, (3, 1), &[], &[], &[], &comp);
        let rep = load_bytes("lab16.psd", &f);
        assert!(rep.warnings[0].starts_with("Lab document converted to RGB"));
        for v in &px(&rep.value, 0, 0, 0)[..3] {
            close(*v, 1.0, 2e-4, "white");
        }
        for v in &px(&rep.value, 0, 1, 0)[..3] {
            close(*v, 0.184_193, 1e-4, "L 50 is Y 0.1842");
        }
        let red = px(&rep.value, 0, 2, 0);
        close(red[0], 0.999_92, 1e-3, "red R");
        close(red[1], 0.0, 1e-3, "red G");
        close(red[2], 0.0, 1e-3, "red B");
    }

    #[test]
    fn indexed_bitmap_multichannel_and_duotone() {
        // Indexed: palette (all reds, then greens, then blues), index 2 transparent.
        let mut pal = vec![0u8; 768];
        pal[1] = 255; // entry 1 red
        pal[256 + 2] = 255; // entry 2 green
        pal[512 + 3] = 128; // entry 3 blue 128
        let resources = res(RES_TRANSPARENT_INDEX, &[0, 2]);
        let f = psd(INDEXED, 8, 1, (4, 1), &pal, &resources, &[], &raw8(&[0, 1, 2, 3]));
        let rep = load_bytes("indexed.psd", &f);
        assert!(rep.warnings[0].starts_with("Indexed Color document converted to RGB"));
        let d = &rep.value;
        assert_eq!(px(d, 0, 0, 0), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(px(d, 0, 1, 0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(px(d, 0, 2, 0)[3], 0.0, "transparent index");
        close(
            px(d, 0, 3, 0)[2],
            srgb_to_linear_f(128.0 / 255.0),
            1e-5,
            "palette blue",
        );

        // Bitmap: 10 pixels per row pad to 2 bytes; set bits are black. RLE.
        let rows = [[0b1010_0000u8, 0b0100_0000], [0b0000_0000, 0b1100_0000]];
        let mut comp = vec![0, 1];
        let mut packed = Vec::new();
        for row in &rows {
            let start = packed.len();
            super::super::packbits(row, &mut packed);
            u16s(&mut comp, (packed.len() - start) as u16);
        }
        comp.extend_from_slice(&packed);
        let f = psd(BITMAP, 1, 1, (10, 2), &[], &[], &[], &comp);
        let rep = load_bytes("bitmap.psd", &f);
        assert!(rep.warnings[0].starts_with("Bitmap (1-bit) document converted to RGB"));
        let d = &rep.value;
        let bit = |x, y| px(d, 0, x, y)[0];
        assert_eq!([bit(0, 0), bit(1, 0), bit(2, 0), bit(9, 0)], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!([bit(8, 0), bit(8, 1), bit(9, 1), bit(0, 1)], [1.0, 0.0, 0.0, 1.0]);

        // Multichannel: the first three channels as RGB, none is alpha.
        let mut comp = vec![0, 0];
        comp.extend_from_slice(&[255, 0, 128, 0]);
        let f = psd(MULTICHANNEL, 8, 4, (1, 1), &[], &[], &[], &comp);
        let rep = load_bytes("multi.psd", &f);
        assert!(
            rep.warnings[0].starts_with("Multichannel document opened with its first three channels as RGB")
        );
        let p = px(&rep.value, 0, 0, 0);
        assert_eq!([p[0], p[1], p[3]], [1.0, 0.0, 1.0]);
        close(
            p[2],
            srgb_to_linear_f(128.0 / 255.0),
            1e-5,
            "third channel is blue",
        );

        // Duotone: its gray data, sRGB-encoded.
        let f = psd(DUOTONE, 8, 1, (1, 1), &[0; 4], &[], &[], &raw8(&[64]));
        let rep = load_bytes("duotone.psd", &f);
        assert!(rep.warnings[0].starts_with("Duotone document opened as its grayscale data"));
        close(
            px(&rep.value, 0, 0, 0)[1],
            srgb_to_linear_f(64.0 / 255.0),
            1e-6,
            "duotone gray",
        );
    }

    #[test]
    fn sixteen_bit_layers_in_lr16_are_read() {
        let mut gray = vec![0, 0];
        for v in [0u16, 65535] {
            gray.extend_from_slice(&v.to_be_bytes());
        }
        let info = one_layer(2, 1, &[(0, gray)]);
        let mut comp = vec![0, 0];
        comp.extend_from_slice(&[0x80; 4]);
        let f = psd(GRAYSCALE, 16, 1, (2, 1), &[], &[], &deep(b"Lr16", &info), &comp);
        let doc = load_bytes("gray16-lr16.psd", &f).value;
        assert_eq!(doc.layers().len(), 1);
        assert_eq!(doc.layers()[0].name, "", "the layer, not the merged image");
        assert_eq!(px(&doc, 0, 0, 0), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(px(&doc, 0, 1, 0), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn unsupported_combinations_are_refused() {
        assert!(ColorMode::from_header(CMYK, 32, 4).is_err());
        assert!(ColorMode::from_header(BITMAP, 8, 1).is_err());
        assert!(
            ColorMode::from_header(CMYK, 8, 3).is_err(),
            "CMYK needs 4 channels"
        );
        assert!(ColorMode::from_header(5, 8, 3).is_err(), "no mode 5");
        assert!(ColorMode::from_header(GRAYSCALE, 32, 1).is_ok());
    }

    #[test]
    fn descriptor_colours_of_other_modes() {
        // Photoshop's descriptors: CMYK percentages and D50 Lab.
        assert_eq!(cmyk_percent_to_linear([0.0; 4]), [1.0, 1.0, 1.0]);
        let k = cmyk_percent_to_linear([0.0, 0.0, 0.0, 100.0]);
        close(k[0], 0.01678, 1e-6, "solid black R");
        for v in lab_to_linear_srgb(100.0, 0.0, 0.0) {
            close(v, 1.0, 1e-4, "Lab white");
        }
        // 32-bit documents store linear 0..255: undo the sRGB decode.
        let mut doc = Document::new(4, 4);
        let id = doc.alloc_id();
        let fill = lumenply_doc::Fill::Solid {
            color: [srgb_to_linear_f(0.6745), 0.0, 1.0],
        };
        doc.add_layer(lumenply_doc::Layer::fill(id, fill));
        linear_descriptor_colors(&mut doc);
        let lumenply_doc::LayerContent::Fill(f) = &doc.layers()[0].content else {
            panic!("fill")
        };
        let lumenply_doc::Fill::Solid { color } = f.fill else {
            panic!("solid")
        };
        close(color[0], 0.6745, 1e-5, "linear 172/255 stays linear");
        assert_eq!(color[2], linear_to_srgb_f(1.0));
    }
}
