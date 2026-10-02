//! PSD blocks for the adjustments added after the original codec:
//! Gradient Map (`grdm`), Channel Mixer (`mixr`), Photo Filter (`phfl`)
//! and Selective Color (`selc`). All four have documented binary layouts;
//! the writers follow psd-tools' reader (and ag-psd's for `mixr`'s four
//! records), and every writer is cross-checked with psd-tools in tests.
//!
//! Colours travel as 16-bit sRGB (Photoshop's colour space 0, RGB);
//! gradient locations are 0..4096, percentages are whole numbers.

use lumenply_doc::{Adjustment, Fill, Gradient, GradientStop, GradientStyle};

use super::{put_u16, put_u32, Rd};
use crate::{linear_to_srgb_f, srgb_to_linear_f};

/// A gradient location in Photoshop units (0..4096).
const LOC: f32 = 4096.0;

fn put_i16(d: &mut Vec<u8>, v: i16) {
    d.extend_from_slice(&v.to_be_bytes());
}

/// Percent of a fraction, rounded and clamped to `±limit`.
fn pct(v: f32, limit: i32) -> i16 {
    let v = if v.is_finite() { v } else { 0.0 };
    ((v * 100.0).round() as i32).clamp(-limit, limit) as i16
}

/// A straight linear channel as a 16-bit sRGB sample.
fn srgb16(v: f32) -> u16 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    (linear_to_srgb_f(v) * 65535.0).round() as u16
}

fn linear_of16(v: u16) -> f32 {
    srgb_to_linear_f(v as f32 / 65535.0)
}

/// The tagged-block key and payload for one of the newer adjustments.
pub(super) fn adjustment_block(adj: &Adjustment) -> Option<(&'static [u8; 4], Vec<u8>)> {
    let mut d = Vec::new();
    let key: &'static [u8; 4] = match adj {
        Adjustment::GradientMap { gradient, reverse } => {
            put_u16(&mut d, 1); // version
            d.push(u8::from(*reverse));
            d.push(0); // dithered
            let name: Vec<u16> = "Custom\0".encode_utf16().collect();
            put_u32(&mut d, name.len() as u32);
            for u in name {
                put_u16(&mut d, u);
            }
            let stops = gradient.sorted();
            put_u16(&mut d, stops.len().min(u16::MAX as usize) as u16);
            for s in &stops {
                put_u32(&mut d, (s.pos * LOC).round() as u32);
                put_u32(&mut d, 50); // midpoint, percent
                put_u16(&mut d, 0); // colour space: RGB
                for c in s.color {
                    put_u16(&mut d, srgb16(c));
                }
                put_u16(&mut d, 0); // fourth component
                put_u16(&mut d, 0); // padding
            }
            put_u16(&mut d, 2); // transparency stops: opaque at both ends
            for loc in [0u32, LOC as u32] {
                put_u32(&mut d, loc);
                put_u32(&mut d, 50);
                put_u16(&mut d, 100);
            }
            put_u16(&mut d, 2); // expansion
            put_u16(&mut d, 0); // interpolation (smoothness): linear, like ours
            put_u16(&mut d, 32); // length
            put_u16(&mut d, 0); // mode
            put_u32(&mut d, 0); // random seed
            put_u16(&mut d, 0); // showing transparency
            put_u16(&mut d, 0); // using vector colour
            put_u32(&mut d, 0); // roughness
            put_u16(&mut d, 0); // colour model
            for _ in 0..4 {
                put_u16(&mut d, 0); // minimum colour
            }
            for _ in 0..4 {
                put_u16(&mut d, u16::MAX); // maximum colour
            }
            put_u16(&mut d, 0); // dummy
            while d.len() % 4 != 0 {
                d.push(0);
            }
            b"grdm"
        }
        Adjustment::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
            gray,
        } => {
            // Four records (red, green, blue, gray outputs), each R, G, B,
            // an unused CMYK slot and the constant, in percent.
            put_u16(&mut d, 1);
            put_u16(&mut d, u16::from(*monochrome));
            let rec = |d: &mut Vec<u8>, w: &[f32; 4]| {
                for v in &w[..3] {
                    put_i16(d, pct(*v, 200));
                }
                put_i16(d, 0);
                put_i16(d, pct(w[3], 200));
            };
            if *monochrome {
                // Photoshop puts the gray row first when monochrome.
                rec(&mut d, gray);
                rec(&mut d, red);
                rec(&mut d, green);
                rec(&mut d, blue);
            } else {
                rec(&mut d, red);
                rec(&mut d, green);
                rec(&mut d, blue);
                rec(&mut d, gray);
            }
            b"mixr"
        }
        Adjustment::PhotoFilter {
            color,
            density,
            preserve_luminosity,
        } => {
            put_u16(&mut d, 2); // version 2: an RGB colour record
            put_u16(&mut d, 0); // colour space: RGB
            for c in color {
                put_u16(&mut d, srgb16(*c));
            }
            put_u16(&mut d, 0);
            put_u32(&mut d, pct(*density, 100).max(0) as u32);
            d.push(u8::from(*preserve_luminosity));
            while d.len() % 4 != 0 {
                d.push(0);
            }
            b"phfl"
        }
        Adjustment::SelectiveColor { colors, absolute } => {
            put_u16(&mut d, 1);
            put_u16(&mut d, u16::from(*absolute));
            for _ in 0..4 {
                put_i16(&mut d, 0); // record 0 is reserved
            }
            for fam in colors {
                for v in fam {
                    put_i16(&mut d, pct(*v, 100));
                }
            }
            b"selc"
        }
        _ => return None,
    };
    Some((key, d))
}

/// Decode a newer adjustment block; `None` when the key is not one of
/// them or the data does not parse.
pub(super) fn parse_adjustment(key: &[u8], data: &[u8]) -> Option<Adjustment> {
    let mut d = Rd::new(data);
    let pc = |v: i16| v as f32 / 100.0;
    Some(match key {
        b"grdm" => {
            let version = d.u16().ok()?;
            if version != 1 && version != 3 {
                return None;
            }
            let reverse = d.u8().ok()? != 0;
            d.u8().ok()?; // dithered
            if version == 3 {
                d.skip(4).ok()?; // interpolation method
            }
            let n = d.u32().ok()? as usize;
            d.skip(n.min(1 << 16) * 2).ok()?; // name
            let count = d.u16().ok()? as usize;
            let mut stops = Vec::with_capacity(count.min(256));
            for _ in 0..count {
                let loc = d.u32().ok()?;
                let _mid = d.u32().ok()?;
                let space = d.u16().ok()?;
                let c = [d.u16().ok()?, d.u16().ok()?, d.u16().ok()?, d.u16().ok()?];
                d.skip(2).ok()?;
                let color = match space {
                    0 => [linear_of16(c[0]), linear_of16(c[1]), linear_of16(c[2])],
                    // Greyscale (8): one component in 0..10000.
                    8 => [srgb_to_linear_f((c[0] as f32 / 10000.0).min(1.0)); 3],
                    // Other spaces (HSB, CMYK, Lab) are not decoded: grey.
                    _ => [srgb_to_linear_f(0.5); 3],
                };
                stops.push(GradientStop::new((loc as f32 / LOC).clamp(0.0, 1.0), color));
            }
            if stops.is_empty() {
                return None;
            }
            Adjustment::GradientMap {
                gradient: Gradient { stops },
                reverse,
            }
        }
        b"mixr" => {
            if d.u16().ok()? != 1 {
                return None;
            }
            let monochrome = d.u16().ok()? != 0;
            let mut rec = || -> Option<[f32; 4]> {
                let r = d.i16().ok()?;
                let g = d.i16().ok()?;
                let b = d.i16().ok()?;
                d.skip(2).ok()?;
                let k = d.i16().ok()?;
                Some([pc(r), pc(g), pc(b), pc(k)])
            };
            let first = rec()?;
            let ident = |i: usize| {
                let mut w = [0.0; 4];
                w[i] = 1.0;
                w
            };
            let (red, green, blue, gray) = if monochrome {
                let r = rec().unwrap_or(ident(0));
                let g = rec().unwrap_or(ident(1));
                let b = rec().unwrap_or(ident(2));
                // Zeroed colour rows (as some writers leave them) read
                // as the identity, so unticking Monochrome shows colour.
                let fix = |w: [f32; 4], i| if w == [0.0; 4] { ident(i) } else { w };
                (fix(r, 0), fix(g, 1), fix(b, 2), first)
            } else {
                let g = rec()?;
                let b = rec()?;
                let gray = rec().unwrap_or([0.4, 0.4, 0.2, 0.0]);
                (first, g, b, gray)
            };
            Adjustment::ChannelMixer {
                red,
                green,
                blue,
                monochrome,
                gray,
            }
        }
        b"phfl" => {
            let version = d.u16().ok()?;
            let color = match version {
                2 => {
                    let space = d.u16().ok()?;
                    let c = [d.u16().ok()?, d.u16().ok()?, d.u16().ok()?, d.u16().ok()?];
                    photoshop_color(space, c)?
                }
                // Version 3 stores L*a*b* (D50) × 100 as 32-bit integers.
                3 => {
                    let l = d.i32().ok()? as f32 / 100.0;
                    let a = d.i32().ok()? as f32 / 100.0;
                    let b = d.i32().ok()? as f32 / 100.0;
                    lab_to_linear_srgb(l, a, b)
                }
                _ => return None,
            };
            let density = (d.u32().ok()?.min(100) as f32) / 100.0;
            let preserve = d.u8().ok()? != 0;
            Adjustment::PhotoFilter {
                color,
                density,
                preserve_luminosity: preserve,
            }
        }
        b"selc" => {
            if d.u16().ok()? != 1 {
                return None;
            }
            let absolute = d.u16().ok()? == 1;
            d.skip(8).ok()?; // reserved record
            let mut colors = [[0f32; 4]; 9];
            for fam in colors.iter_mut() {
                for v in fam.iter_mut() {
                    *v = pc(d.i16().ok()?).clamp(-1.0, 1.0);
                }
            }
            Adjustment::SelectiveColor { colors, absolute }
        }
        _ => return None,
    })
}

// ---- Action descriptors (fill layers) ----------------------------------------------
//
// Solid Color (`SoCo`) and Gradient (`GdFl`) fill layers store their
// settings as version-16 action descriptors. This is a small, complete
// codec for the value types those blocks use; unknown types end a read.

/// One descriptor value.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Val {
    Long(i32),
    Doub(f64),
    Bool(bool),
    Text(String),
    /// Enumerated: (type id, value id).
    Enum(Vec<u8>, Vec<u8>),
    /// Unit float: (unit, value), e.g. `#Ang`, `#Prc`.
    Unit([u8; 4], f64),
    Obj(Desc),
    List(Vec<Val>),
}

/// A descriptor: class id and keyed items, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Desc {
    pub class: Vec<u8>,
    pub items: Vec<(Vec<u8>, Val)>,
}

impl Desc {
    pub(super) fn new(class: &[u8]) -> Self {
        Desc {
            class: class.to_vec(),
            items: Vec::new(),
        }
    }

    pub(super) fn with(mut self, key: &[u8], v: Val) -> Self {
        self.items.push((key.to_vec(), v));
        self
    }

    pub(super) fn get(&self, key: &[u8]) -> Option<&Val> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub(super) fn num(&self, key: &[u8]) -> Option<f64> {
        match self.get(key)? {
            Val::Long(v) => Some(*v as f64),
            Val::Doub(v) | Val::Unit(_, v) => Some(*v),
            _ => None,
        }
    }

    pub(super) fn obj(&self, key: &[u8]) -> Option<&Desc> {
        match self.get(key)? {
            Val::Obj(d) => Some(d),
            _ => None,
        }
    }
}

fn put_key(d: &mut Vec<u8>, key: &[u8]) {
    // Four-character ids are written with a zero length.
    put_u32(d, if key.len() == 4 { 0 } else { key.len() as u32 });
    d.extend_from_slice(key);
}

fn put_unicode(d: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    put_u32(d, units.len() as u32);
    for u in units {
        put_u16(d, u);
    }
}

fn put_desc(d: &mut Vec<u8>, desc: &Desc) {
    put_unicode(d, "");
    put_key(d, &desc.class);
    put_u32(d, desc.items.len() as u32);
    for (k, v) in &desc.items {
        put_key(d, k);
        put_val(d, v);
    }
}

fn put_val(d: &mut Vec<u8>, v: &Val) {
    match v {
        Val::Long(x) => {
            d.extend_from_slice(b"long");
            d.extend_from_slice(&x.to_be_bytes());
        }
        Val::Doub(x) => {
            d.extend_from_slice(b"doub");
            d.extend_from_slice(&x.to_be_bytes());
        }
        Val::Bool(b) => {
            d.extend_from_slice(b"bool");
            d.push(u8::from(*b));
        }
        Val::Text(s) => {
            d.extend_from_slice(b"TEXT");
            put_unicode(d, s);
        }
        Val::Enum(t, e) => {
            d.extend_from_slice(b"enum");
            put_key(d, t);
            put_key(d, e);
        }
        Val::Unit(u, x) => {
            d.extend_from_slice(b"UntF");
            d.extend_from_slice(u);
            d.extend_from_slice(&x.to_be_bytes());
        }
        Val::Obj(o) => {
            d.extend_from_slice(b"Objc");
            put_desc(d, o);
        }
        Val::List(items) => {
            d.extend_from_slice(b"VlLs");
            put_u32(d, items.len() as u32);
            for it in items {
                put_val(d, it);
            }
        }
    }
}

/// A version-16 descriptor block body.
pub(super) fn descriptor_block(desc: &Desc) -> Vec<u8> {
    let mut d = Vec::new();
    put_u32(&mut d, 16);
    put_desc(&mut d, desc);
    d
}

fn read_key(d: &mut Rd) -> Option<Vec<u8>> {
    let n = d.u32().ok()? as usize;
    let n = if n == 0 { 4 } else { n.min(1024) };
    Some(d.bytes(n).ok()?.to_vec())
}

fn read_unicode(d: &mut Rd) -> Option<String> {
    let n = (d.u32().ok()? as usize).min(1 << 16);
    let mut units = Vec::with_capacity(n);
    for _ in 0..n {
        units.push(d.u16().ok()?);
    }
    while units.last() == Some(&0) {
        units.pop();
    }
    Some(String::from_utf16_lossy(&units))
}

fn read_desc(d: &mut Rd, depth: usize) -> Option<Desc> {
    if depth > 16 {
        return None;
    }
    read_unicode(d)?;
    let class = read_key(d)?;
    let count = d.u32().ok()? as usize;
    let mut items = Vec::with_capacity(count.min(64));
    for _ in 0..count.min(4096) {
        let key = read_key(d)?;
        let ty: [u8; 4] = d.bytes(4).ok()?.try_into().ok()?;
        items.push((key, read_val(d, &ty, depth)?));
    }
    Some(Desc { class, items })
}

fn read_val(d: &mut Rd, ty: &[u8; 4], depth: usize) -> Option<Val> {
    Some(match ty {
        b"long" => Val::Long(d.i32().ok()?),
        b"doub" => Val::Doub(f64::from_be_bytes(d.bytes(8).ok()?.try_into().ok()?)),
        b"bool" => Val::Bool(d.u8().ok()? != 0),
        b"TEXT" => Val::Text(read_unicode(d)?),
        b"enum" => Val::Enum(read_key(d)?, read_key(d)?),
        b"UntF" => {
            let u: [u8; 4] = d.bytes(4).ok()?.try_into().ok()?;
            Val::Unit(u, f64::from_be_bytes(d.bytes(8).ok()?.try_into().ok()?))
        }
        b"Objc" | b"GlbO" => Val::Obj(read_desc(d, depth + 1)?),
        b"VlLs" => {
            let n = d.u32().ok()? as usize;
            let mut v = Vec::with_capacity(n.min(256));
            for _ in 0..n.min(4096) {
                let t: [u8; 4] = d.bytes(4).ok()?.try_into().ok()?;
                v.push(read_val(d, &t, depth + 1)?);
            }
            Val::List(v)
        }
        // Large integer, unit-float list and class references (layer
        // effects use them): kept in the nearest value we model.
        b"comp" => Val::Doub(d.u64().ok()? as i64 as f64),
        b"UnFl" => {
            let u: [u8; 4] = d.bytes(4).ok()?.try_into().ok()?;
            let n = d.u32().ok()? as usize;
            let mut v = Vec::with_capacity(n.min(256));
            for _ in 0..n.min(4096) {
                v.push(Val::Unit(
                    u,
                    f64::from_be_bytes(d.bytes(8).ok()?.try_into().ok()?),
                ));
            }
            Val::List(v)
        }
        b"type" | b"GlbC" => {
            read_unicode(d)?;
            Val::Text(String::from_utf8_lossy(&read_key(d)?).into_owned())
        }
        // Raw data: skipped, kept as an empty text.
        b"tdta" => {
            let n = d.u32().ok()? as usize;
            d.skip(n).ok()?;
            Val::Text(String::new())
        }
        _ => return None,
    })
}

/// Parse a version-16 descriptor block body.
pub(super) fn parse_descriptor(data: &[u8]) -> Option<Desc> {
    let mut d = Rd::new(data);
    if d.u32().ok()? != 16 {
        return None;
    }
    read_desc(&mut d, 0)
}

/// An `RGBC` colour object (0..255 doubles) from straight linear RGB.
pub(super) fn rgbc(c: [f32; 3]) -> Val {
    let v = |x: f32| Val::Doub((linear_to_srgb_f(x.clamp(0.0, 1.0)) * 255.0) as f64);
    Val::Obj(
        Desc::new(b"RGBC")
            .with(b"Rd  ", v(c[0]))
            .with(b"Grn ", v(c[1]))
            .with(b"Bl  ", v(c[2])),
    )
}

/// Straight linear RGB from a colour object (RGB or greyscale; other
/// models read as mid grey).
pub(super) fn color_of(o: &Desc) -> [f32; 3] {
    let lin = |v: f64| srgb_to_linear_f((v / 255.0).clamp(0.0, 1.0) as f32);
    match &o.class[..] {
        b"RGBC" => [
            lin(o.num(b"Rd  ").unwrap_or(0.0)),
            lin(o.num(b"Grn ").unwrap_or(0.0)),
            lin(o.num(b"Bl  ").unwrap_or(0.0)),
        ],
        b"Grsc" => {
            // Grey as ink percent: 0 is white.
            let k = 1.0 - (o.num(b"Gry ").unwrap_or(50.0) / 100.0).clamp(0.0, 1.0);
            [srgb_to_linear_f(k as f32); 3]
        }
        _ => [srgb_to_linear_f(0.5); 3],
    }
}

fn style_key(s: GradientStyle) -> &'static [u8] {
    match s {
        GradientStyle::Linear => b"Lnr ",
        GradientStyle::Radial => b"Rdl ",
        GradientStyle::Angle => b"Angl",
        GradientStyle::Reflected => b"Rflc",
        GradientStyle::Diamond => b"Dmnd",
    }
}

/// The tagged-block key and payload of a fill layer.
pub(super) fn fill_block(fill: &Fill) -> (&'static [u8; 4], Vec<u8>) {
    match fill {
        Fill::Solid { color } => (
            b"SoCo",
            descriptor_block(&Desc::new(b"null").with(b"Clr ", rgbc(*color))),
        ),
        Fill::Gradient {
            gradient,
            style,
            angle,
            scale,
            reverse,
            offset,
        } => {
            let stops = gradient.sorted();
            let loc = |p: f32| Val::Long((p * LOC).round() as i32);
            let colors = stops
                .iter()
                .map(|s| {
                    Val::Obj(
                        Desc::new(b"Clrt")
                            .with(b"Clr ", rgbc(s.color))
                            .with(b"Type", Val::Enum(b"Clry".to_vec(), b"UsrS".to_vec()))
                            .with(b"Lctn", loc(s.pos))
                            .with(b"Mdpn", Val::Long(50)),
                    )
                })
                .collect();
            let alphas = stops
                .iter()
                .map(|s| {
                    Val::Obj(
                        Desc::new(b"TrnS")
                            .with(b"Opct", Val::Unit(*b"#Prc", (s.alpha * 100.0).round() as f64))
                            .with(b"Lctn", loc(s.pos))
                            .with(b"Mdpn", Val::Long(50)),
                    )
                })
                .collect();
            let grad = Desc::new(b"Grdn")
                .with(b"Nm  ", Val::Text("Custom".into()))
                .with(b"GrdF", Val::Enum(b"GrdF".to_vec(), b"CstS".to_vec()))
                // Smoothness 0: straight interpolation, as we render it.
                .with(b"Intr", Val::Doub(0.0))
                .with(b"Clrs", Val::List(colors))
                .with(b"Trns", Val::List(alphas));
            let desc = Desc::new(b"null")
                .with(b"Grad", Val::Obj(grad))
                .with(b"Angl", Val::Unit(*b"#Ang", *angle as f64))
                .with(b"Type", Val::Enum(b"GrdT".to_vec(), style_key(*style).to_vec()))
                .with(b"Rvrs", Val::Bool(*reverse))
                .with(b"Dthr", Val::Bool(false))
                .with(b"Algn", Val::Bool(true))
                .with(b"Scl ", Val::Unit(*b"#Prc", (*scale * 100.0) as f64))
                .with(
                    b"Ofst",
                    Val::Obj(
                        Desc::new(b"Pnt ")
                            .with(b"Hrzn", Val::Unit(*b"#Prc", (offset[0] * 100.0) as f64))
                            .with(b"Vrtc", Val::Unit(*b"#Prc", (offset[1] * 100.0) as f64)),
                    ),
                );
            (b"GdFl", descriptor_block(&desc))
        }
    }
}

/// Decode a `SoCo` / `GdFl` block into a fill.
pub(super) fn parse_fill(key: &[u8], data: &[u8]) -> Option<Fill> {
    let desc = parse_descriptor(data)?;
    match key {
        b"SoCo" => Some(Fill::Solid {
            color: color_of(desc.obj(b"Clr ")?),
        }),
        b"GdFl" => {
            let grad = desc.obj(b"Grad")?;
            let list = |k: &[u8]| match grad.get(k) {
                Some(Val::List(v)) => v
                    .iter()
                    .filter_map(|x| match x {
                        Val::Obj(o) => Some(o.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let pos = |o: &Desc| (o.num(b"Lctn").unwrap_or(0.0) as f32 / LOC).clamp(0.0, 1.0);
            let mut stops: Vec<GradientStop> = list(b"Clrs")
                .iter()
                .map(|o| {
                    let color = match o.get(b"Type") {
                        // Foreground / background stops: black and white.
                        Some(Val::Enum(_, e)) if e == b"FrgC" => [0.0; 3],
                        Some(Val::Enum(_, e)) if e == b"BckC" => [1.0; 3],
                        _ => o.obj(b"Clr ").map_or([0.5; 3], color_of),
                    };
                    GradientStop::new(pos(o), color)
                })
                .collect();
            if stops.is_empty() {
                stops = Gradient::default().stops;
            }
            // Opacity stops live on their own positions: give each colour
            // stop the opacity there, and add a stop wherever opacity
            // alone changes, so the transparency keeps its shape.
            let mut alphas: Vec<(f32, f32)> = list(b"Trns")
                .iter()
                .map(|o| {
                    (
                        pos(o),
                        (o.num(b"Opct").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0) as f32,
                    )
                })
                .collect();
            alphas.sort_by(|a, b| a.0.total_cmp(&b.0));
            if !alphas.is_empty() {
                let colour_ramp = Gradient { stops: stops.clone() };
                // Opacity interpolates linearly between its own stops.
                let a_at = |p: f32| {
                    let (first, last) = (alphas[0], alphas[alphas.len() - 1]);
                    if p <= first.0 {
                        return first.1;
                    }
                    if p >= last.0 {
                        return last.1;
                    }
                    let i = alphas
                        .iter()
                        .position(|(q, _)| *q >= p)
                        .unwrap_or(alphas.len() - 1);
                    let (a, b) = (alphas[i - 1], alphas[i]);
                    let span = (b.0 - a.0).max(1e-6);
                    a.1 + (b.1 - a.1) * (p - a.0) / span
                };
                for s in &mut stops {
                    s.alpha = a_at(s.pos);
                }
                for (p, a) in &alphas {
                    if !stops.iter().any(|s| (s.pos - p).abs() < 1e-4) {
                        let c = colour_ramp.eval_gamma(*p);
                        stops.push(GradientStop {
                            pos: *p,
                            color: [c[0], c[1], c[2]].map(srgb_to_linear_f),
                            alpha: *a,
                        });
                    }
                }
                stops.sort_by(|a, b| a.pos.total_cmp(&b.pos));
            }
            let style = match desc.get(b"Type") {
                Some(Val::Enum(_, e)) => match &e[..] {
                    b"Rdl " => GradientStyle::Radial,
                    b"Angl" => GradientStyle::Angle,
                    b"Rflc" => GradientStyle::Reflected,
                    b"Dmnd" => GradientStyle::Diamond,
                    _ => GradientStyle::Linear,
                },
                _ => GradientStyle::Linear,
            };
            let off = desc.obj(b"Ofst");
            let pct = |o: Option<&Desc>, k: &[u8]| o.and_then(|o| o.num(k)).unwrap_or(0.0) as f32 / 100.0;
            Some(Fill::Gradient {
                gradient: Gradient { stops },
                style,
                angle: desc.num(b"Angl").unwrap_or(90.0) as f32,
                scale: (desc.num(b"Scl ").unwrap_or(100.0) as f32 / 100.0).clamp(0.1, 10.0),
                reverse: matches!(desc.get(b"Rvrs"), Some(Val::Bool(true))),
                offset: [pct(off, b"Hrzn"), pct(off, b"Vrtc")],
            })
        }
        _ => None,
    }
}

/// CIE L*a*b* (D50, Photoshop's Lab) to straight linear sRGB, clipped.
/// A Photoshop colour structure (space id + four 16-bit components) as
/// linear sRGB: RGB, HSB, CMYK (uncalibrated), Lab and Grayscale.
fn photoshop_color(space: u16, c: [u16; 4]) -> Option<[f32; 3]> {
    let unit = |v: u16, max: f32| (v as f32 / max).clamp(0.0, 1.0);
    let gamma = match space {
        0 => [unit(c[0], 65535.0), unit(c[1], 65535.0), unit(c[2], 65535.0)],
        1 => {
            // Hue in hundredths of a degree, saturation and brightness 0-10000.
            let (h, s, v) = (c[0] as f32 / 100.0, unit(c[1], 10000.0), unit(c[2], 10000.0));
            let f = |n: f32| {
                let k = (n + h / 60.0).rem_euclid(6.0);
                v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
            };
            [f(5.0), f(3.0), f(1.0)]
        }
        // 65535 is no ink.
        2 => {
            let k = 1.0 - unit(c[3], 65535.0);
            [0, 1, 2].map(|i| unit(c[i], 65535.0) * (1.0 - k))
        }
        7 => {
            return Some(lab_to_linear_srgb(
                c[0] as f32 / 100.0,
                c[1] as i16 as f32 / 100.0,
                c[2] as i16 as f32 / 100.0,
            ))
        }
        8 => [1.0 - unit(c[0], 10000.0); 3],
        _ => return None,
    };
    Some(gamma.map(crate::srgb_to_linear_f))
}

pub(super) fn lab_to_linear_srgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.964_22 * inv(fx), inv(fy), 0.825_21 * inv(fz));
    // Bradford-adapted XYZ(D50) → linear sRGB.
    let r = 3.133_856 * x - 1.616_867 * y - 0.490_615 * z;
    let g = -0.978_768 * x + 1.916_142 * y + 0.033_454 * z;
    let bl = 0.071_945 * x - 0.228_991 * y + 1.405_243 * z;
    [r, g, bl].map(|v| v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    fn round_trip(adj: &Adjustment) -> Adjustment {
        let (key, data) = adjustment_block(adj).expect("encodes");
        parse_adjustment(key, &data).expect("decodes")
    }

    #[test]
    fn gradient_maps_round_trip_with_exact_layout() {
        let adj = Adjustment::GradientMap {
            gradient: Gradient {
                stops: vec![
                    GradientStop::srgb8(0.0, [0, 0, 0]),
                    GradientStop::srgb8(0.25, [255, 0, 0]),
                    GradientStop::srgb8(1.0, [255, 255, 255]),
                ],
            },
            reverse: true,
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!(key, b"grdm");
        // version, reversed, dithered, name "Custom\0" (7 units), 3 stops.
        assert_eq!(&data[..4], &[0, 1, 1, 0]);
        assert_eq!(u32::from_be_bytes(data[4..8].try_into().unwrap()), 7);
        assert_eq!(u16::from_be_bytes(data[22..24].try_into().unwrap()), 3);
        // Second stop: location 1024 of 4096, midpoint 50, RGB, 65535/0/0.
        let s1 = &data[24 + 20..24 + 40];
        assert_eq!(u32::from_be_bytes(s1[0..4].try_into().unwrap()), 1024);
        assert_eq!(u32::from_be_bytes(s1[4..8].try_into().unwrap()), 50);
        assert_eq!(&s1[8..14], &[0, 0, 0xFF, 0xFF, 0, 0]);
        assert_eq!(data.len() % 4, 0);
        let back = round_trip(&adj);
        let Adjustment::GradientMap { gradient, reverse } = back else {
            panic!()
        };
        assert!(reverse);
        assert_eq!(gradient.stops.len(), 3);
        assert!(close(gradient.stops[1].pos, 0.25) && close(gradient.stops[1].color[0], 1.0));
        assert!(close(gradient.stops[2].color[1], 1.0) && close(gradient.stops[0].color[2], 0.0));
    }

    #[test]
    fn channel_mixers_round_trip_in_percent() {
        let adj = Adjustment::ChannelMixer {
            red: [1.2, -0.1, 0.0, 0.05],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.3, 0.7, -0.2],
            monochrome: false,
            gray: [0.4, 0.4, 0.2, 0.0],
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!((key, data.len()), (b"mixr", 44));
        // Red row: 120, -10, 0, (unused), 5.
        let row: Vec<i16> = data[4..14]
            .chunks(2)
            .map(|c| i16::from_be_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(row, vec![120, -10, 0, 0, 5]);
        assert_eq!(round_trip(&adj), adj);
        let mono = Adjustment::ChannelMixer {
            monochrome: true,
            gray: [-0.7, 2.0, -0.3, 0.0],
            red: [1.0, 0.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.0, 1.0, 0.0],
        };
        assert_eq!(round_trip(&mono), mono);
    }

    #[test]
    fn photo_filters_round_trip_and_read_lab_version_3() {
        let adj = Adjustment::PhotoFilter {
            color: [1.0, srgb_to_linear_f(138.0 / 255.0), 0.0],
            density: 0.25,
            preserve_luminosity: true,
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!(key, b"phfl");
        assert_eq!(&data[..4], &[0, 2, 0, 0]);
        assert_eq!(u16::from_be_bytes([data[6], data[7]]), 138 * 257);
        assert_eq!(u32::from_be_bytes(data[12..16].try_into().unwrap()), 25);
        let Adjustment::PhotoFilter {
            color,
            density,
            preserve_luminosity,
        } = round_trip(&adj)
        else {
            panic!()
        };
        assert!(
            close(color[1], srgb_to_linear_f(138.0 / 255.0)) && close(density, 0.25) && preserve_luminosity
        );

        // Version 3: L*a*b* white (100, 0, 0) is white.
        let mut v3 = vec![0, 3];
        for v in [10000i32, 0, 0] {
            v3.extend_from_slice(&v.to_be_bytes());
        }
        v3.extend_from_slice(&40u32.to_be_bytes());
        v3.push(0);
        let Some(Adjustment::PhotoFilter { color, density, .. }) = parse_adjustment(b"phfl", &v3) else {
            panic!()
        };
        assert!(color.iter().all(|c| close(*c, 1.0)), "{color:?}");
        assert!(close(density, 0.4));

        // Version 2 also names other colour spaces: Lab (7) mid grey,
        // HSB (1) pure red, Grayscale (8) 0% = white.
        for (space, comps, want) in [
            (7u16, [5000u16, 0, 0, 0], srgb_to_linear_f(119.0 / 255.0)),
            (1, [0, 10000, 10000, 0], 1.0),
            (8, [0, 0, 0, 0], 1.0),
        ] {
            let mut v2 = vec![0, 2];
            v2.extend_from_slice(&space.to_be_bytes());
            for c in comps {
                v2.extend_from_slice(&c.to_be_bytes());
            }
            v2.extend_from_slice(&25u32.to_be_bytes());
            v2.push(1);
            let Some(Adjustment::PhotoFilter { color, .. }) = parse_adjustment(b"phfl", &v2) else {
                panic!("space {space}")
            };
            assert!((color[0] - want).abs() < 0.01, "space {space}: {color:?}");
        }
    }

    /// Writes `<tmp>/lumenply-psd-extra/adjustments.psd` (checked with
    /// psd-tools as well) and reads it back through the full codec.
    #[test]
    fn a_psd_with_every_new_adjustment_round_trips() {
        use lumenply_doc::{Document, LayerContent};
        let mut doc = Document::new(8, 8);
        let bg = doc.add_pixel_layer("Background");
        doc.layer_mut(bg).unwrap().pixels_mut().unwrap().set_pixel(
            1,
            1,
            lumenply_tiles::Rgba::new(0.5, 0.2, 0.1, 1.0),
        );
        let mut sel = [[0f32; 4]; 9];
        sel[0] = [-0.3, 0.2, 0.6, 0.0];
        let adjs = vec![
            Adjustment::GradientMap {
                gradient: Gradient::presets()[1].1.clone(),
                reverse: false,
            },
            Adjustment::ChannelMixer {
                red: [1.0, 0.0, 0.0, 0.0],
                green: [0.0, 1.0, 0.0, 0.0],
                blue: [0.0, 0.0, 1.0, 0.0],
                monochrome: true,
                gray: [-0.7, 2.0, -0.3, 0.0],
            },
            Adjustment::photo_filter_default(),
            Adjustment::SelectiveColor {
                colors: sel,
                absolute: false,
            },
        ];
        for a in &adjs {
            doc.add_adjustment(a.clone());
        }
        let dir = std::env::temp_dir().join("lumenply-psd-extra");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adjustments.psd");
        let report = crate::psd::save(&path, &doc).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let back = crate::psd::load(&path).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let got: Vec<&Adjustment> = back
            .value
            .layers()
            .iter()
            .filter_map(|l| match &l.content {
                LayerContent::Adjustment(a) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(got.len(), 4);
        assert!(matches!(got[0], Adjustment::GradientMap { gradient, .. } if gradient.stops.len() == 3));
        assert_eq!(got[1], &adjs[1]);
        assert!(matches!(got[2], Adjustment::PhotoFilter { density, .. } if close(*density, 0.25)));
        assert_eq!(got[3], &adjs[3]);
    }

    #[test]
    fn selective_color_round_trips_every_family() {
        let mut colors = [[0f32; 4]; 9];
        colors[0] = [0.5, -0.25, 0.0, 0.1];
        colors[8] = [0.0, 0.0, 0.0, -1.0];
        let adj = Adjustment::SelectiveColor {
            colors,
            absolute: true,
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!((key, data.len()), (b"selc", 4 + 80));
        assert_eq!(&data[..4], &[0, 1, 0, 1]);
        // Reds follow the reserved record: 50, -25, 0, 10.
        assert_eq!(&data[12..20], &[0, 50, 0xFF, 0xE7, 0, 0, 0, 10]);
        assert_eq!(round_trip(&adj), adj);
    }
}
