//! Photoshop layer effects (layer styles, the `lfx2` / `lmfx` blocks) and
//! Fill opacity (`iOpa`), both ways.
//!
//! Effects are version-16 action descriptors: one object per effect kind
//! (`DrSh`, `IrSh`, `OrGl`, `IrGl`, `ebbl`, `SoFi`, `GrFl`, `FrFX`, plus
//! `ChFX` satin and `patternFill`, which we cannot render). `lmfx` adds
//! lists for the kinds Photoshop allows several of; we take the first
//! enabled one of each kind and say so when there were more.
//!
//! Photoshop's "Size" is the distance an effect fades over; our effects
//! blur with three box passes whose reach is about √3 × the blur radius,
//! so `blur = size / √3` (and back on export).

use lumenply_doc::{
    BevelFx, BlendMode, ColorOverlayFx, Fill, GlowFx, GradientOverlayFx, Layer, LayerEffects, ShadowFx,
    StrokeAlign, StrokeFx,
};

use super::extra::{color_of, descriptor_block, fill_block, parse_descriptor, parse_fill, rgbc, Desc, Val};
use super::{additional_block, put_u32};

/// Photoshop's global light (image resources 1037 and 1049), used by
/// effects with "Use Global Light" on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GlobalLight {
    pub angle: f32,
    pub altitude: f32,
}

impl Default for GlobalLight {
    fn default() -> Self {
        // Photoshop's defaults for a new document.
        GlobalLight {
            angle: 120.0,
            altitude: 30.0,
        }
    }
}

/// The global light in an image-resources section body. Missing or
/// malformed resources leave Photoshop's defaults.
pub(super) fn global_light(res: &[u8]) -> GlobalLight {
    let mut light = GlobalLight::default();
    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize| -> Option<&[u8]> {
        let s = res.get(*pos..pos.checked_add(n)?)?;
        *pos += n;
        Some(s)
    };
    while take(&mut pos, 4).is_some() {
        let Some(id) = take(&mut pos, 2).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
            break;
        };
        let Some(name_len) = take(&mut pos, 1).map(|b| b[0] as usize) else {
            break;
        };
        if take(&mut pos, (1 + name_len).next_multiple_of(2) - 1).is_none() {
            break;
        }
        let Some(size) = take(&mut pos, 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            break;
        };
        let Some(data) = take(&mut pos, size) else { break };
        if size % 2 == 1 {
            pos += 1;
        }
        if data.len() >= 4 {
            let v = i32::from_be_bytes([data[0], data[1], data[2], data[3]]) as f32;
            match id {
                1037 => light.angle = v,
                1049 => light.altitude = v,
                _ => {}
            }
        }
    }
    light
}

/// Our blur radius for a Photoshop effect size in pixels.
pub(crate) fn blur_of_size(size: f32) -> f32 {
    size.max(0.0) / 3f32.sqrt()
}

/// The Photoshop effect size for one of our blur radii.
pub(crate) fn size_of_blur(blur: f32) -> f32 {
    blur.max(0.0) * 3f32.sqrt()
}

/// Descriptor blend-mode ids (`BlnM`) and our blend-mode names. Modes we
/// don't have yet parse to `None` (normal, with a warning).
const MODES: &[(&[u8], &str)] = &[
    (b"Nrml", "normal"),
    (b"Dslv", "dissolve"),
    (b"Drkn", "darken"),
    (b"Mltp", "multiply"),
    (b"CBrn", "color-burn"),
    (b"linearBurn", "linear-burn"),
    (b"darkerColor", "darker-color"),
    (b"Lghn", "lighten"),
    (b"Scrn", "screen"),
    (b"CDdg", "color-dodge"),
    (b"linearDodge", "add"),
    (b"lighterColor", "lighter-color"),
    (b"Ovrl", "overlay"),
    (b"SftL", "soft-light"),
    (b"HrdL", "hard-light"),
    (b"vividLight", "vivid-light"),
    (b"linearLight", "linear-light"),
    (b"pinLight", "pin-light"),
    (b"hardMix", "hard-mix"),
    (b"Dfrn", "difference"),
    (b"Xclu", "exclusion"),
    (b"blendSubtraction", "subtract"),
    (b"blendDivide", "divide"),
    (b"H   ", "hue"),
    (b"Strt", "saturation"),
    (b"Clr ", "color"),
    (b"Lmns", "luminosity"),
];

fn mode_of(id: &[u8]) -> Option<BlendMode> {
    if let Some((_, name)) = MODES.iter().find(|(k, _)| *k == id) {
        return name.parse().ok();
    }
    // Newer files spell modes out in camel case ("normal", "softLight").
    let mut name = String::new();
    for c in String::from_utf8_lossy(id).trim().chars() {
        if c.is_ascii_uppercase() {
            name.push('-');
        }
        name.push(c.to_ascii_lowercase());
    }
    match name.as_str() {
        "linear-dodge" => Some(BlendMode::Add),
        _ => name.parse().ok(),
    }
}

fn mode_id(m: BlendMode) -> &'static [u8] {
    MODES
        .iter()
        .find(|(_, n)| *n == m.name())
        .map_or(b"Nrml".as_slice(), |(k, _)| k)
}

fn enum_val(d: &Desc, key: &[u8]) -> Option<Vec<u8>> {
    match d.get(key)? {
        Val::Enum(_, e) => Some(e.clone()),
        _ => None,
    }
}

fn flag(d: &Desc, key: &[u8]) -> Option<bool> {
    match d.get(key)? {
        Val::Bool(b) => Some(*b),
        _ => None,
    }
}

/// Reads one effect object, reporting what could not be carried over.
struct Reader<'a> {
    layer: &'a str,
    light: GlobalLight,
    warnings: &'a mut Vec<String>,
}

impl Reader<'_> {
    fn warn(&mut self, what: String) {
        let w = format!("layer '{}': {what}", self.layer);
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    fn blend(&mut self, d: &Desc, key: &[u8], fx: &str) -> BlendMode {
        let Some(id) = enum_val(d, key) else {
            return BlendMode::Normal;
        };
        match mode_of(&id) {
            Some(m) => m,
            None => {
                self.warn(format!(
                    "{fx} blend mode '{}' is not supported yet, using normal",
                    String::from_utf8_lossy(&id).trim()
                ));
                BlendMode::Normal
            }
        }
    }

    fn color(&mut self, d: &Desc, key: &[u8], fx: &str) -> [f32; 3] {
        let Some(o) = d.obj(key) else {
            return [0.0; 3];
        };
        let gamma = |v: f64| crate::srgb_to_linear_f(v.clamp(0.0, 1.0) as f32);
        match &o.class[..] {
            b"RGBC" if o.get(b"Rd  ").is_none() => {
                // Newer files: 0..1 floats.
                let f = |k: &[u8]| gamma(o.num(k).unwrap_or(0.0));
                [f(b"redFloat"), f(b"greenFloat"), f(b"blueFloat")]
            }
            b"RGBC" | b"Grsc" => color_of(o),
            b"HSBC" => {
                let h = o.num(b"H   ").unwrap_or(0.0).rem_euclid(360.0) / 60.0;
                let s = o.num(b"Strt").unwrap_or(0.0) / 100.0;
                let v = o.num(b"Brgh").unwrap_or(0.0) / 100.0;
                let c = v * s;
                let x = c * (1.0 - (h % 2.0 - 1.0).abs());
                let (r, g, b) = match h as i32 {
                    0 => (c, x, 0.0),
                    1 => (x, c, 0.0),
                    2 => (0.0, c, x),
                    3 => (0.0, x, c),
                    4 => (x, 0.0, c),
                    _ => (c, 0.0, x),
                };
                let m = v - c;
                [gamma(r + m), gamma(g + m), gamma(b + m)]
            }
            b"LbCl" => super::extra::lab_to_linear_srgb(
                o.num(b"Lmnc").unwrap_or(50.0) as f32,
                o.num(b"A   ").unwrap_or(0.0) as f32,
                o.num(b"B   ").unwrap_or(0.0) as f32,
            ),
            b"CMYC" => {
                let k = |key: &[u8]| (o.num(key).unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
                let black = k(b"Blck");
                [b"Cyn ", b"Mgnt", b"Ylw "].map(|c| gamma((1.0 - k(c)) * (1.0 - black)))
            }
            other => {
                self.warn(format!(
                    "{fx} colour ({}) is not supported, using grey",
                    String::from_utf8_lossy(other).trim()
                ));
                [crate::srgb_to_linear_f(0.5); 3]
            }
        }
    }

    fn pct(d: &Desc, key: &[u8], default: f64) -> f32 {
        (d.num(key).unwrap_or(default) / 100.0).clamp(0.0, 1.0) as f32
    }

    fn px(&self, d: &Desc, key: &[u8], default: f64) -> f32 {
        let v = d.num(key).unwrap_or(default) as f32;
        if v.is_finite() {
            v.clamp(0.0, 1000.0)
        } else {
            0.0
        }
    }

    fn angle(&self, d: &Desc) -> f32 {
        if flag(d, b"uglg").unwrap_or(false) {
            self.light.angle
        } else {
            d.num(b"lagl").unwrap_or(self.light.angle as f64) as f32
        }
    }

    /// Drop or inner shadow: offset away from the light.
    fn shadow(&mut self, d: &Desc, fx: &str) -> ShadowFx {
        let a = self.angle(d).to_radians();
        let dist = self.px(d, b"Dstn", 0.0);
        let size = self.px(d, b"blur", 0.0);
        // Spread (choke) is a percentage of the size that stays solid.
        let spread = Self::pct(d, b"Ckmt", 0.0);
        ShadowFx {
            dx: -a.cos() * dist,
            dy: a.sin() * dist,
            blur: blur_of_size(size * (1.0 - spread)),
            color: self.color(d, b"Clr ", fx),
            opacity: Self::pct(d, b"Opct", 75.0),
            spread: size * spread,
            blend: self.blend(d, b"Md  ", fx),
            knockout: flag(d, b"layerConceals").unwrap_or(true),
        }
    }

    fn glow(&mut self, d: &Desc, fx: &str) -> GlowFx {
        let size = self.px(d, b"blur", 0.0);
        let spread = Self::pct(d, b"Ckmt", 0.0);
        let color = if d.obj(b"Clr ").is_some() {
            self.color(d, b"Clr ", fx)
        } else {
            // A gradient glow: its first colour.
            self.warn(format!("{fx} uses a gradient, rendered in its first colour"));
            gradient_of(d)
                .and_then(|f| match f {
                    Fill::Gradient { gradient, .. } => gradient.sorted().first().map(|s| s.color),
                    Fill::Solid { color } => Some(color),
                })
                .unwrap_or([1.0; 3])
        };
        GlowFx {
            blur: blur_of_size(size * (1.0 - spread)),
            color,
            opacity: Self::pct(d, b"Opct", 75.0),
            spread: size * spread,
            blend: self.blend(d, b"Md  ", fx),
        }
    }
}

/// A gradient effect's settings (`Grad`, `Angl`, `Type`, …) as a fill.
fn gradient_of(d: &Desc) -> Option<Fill> {
    let mut g = Desc::new(b"null");
    for k in [b"Grad", b"Angl", b"Type", b"Rvrs", b"Scl ", b"Ofst"] {
        if let Some(v) = d.get(k) {
            g = g.with(k, v.clone());
        }
    }
    parse_fill(b"GdFl", &descriptor_block(&g))
}

/// The enabled effect objects of one kind: the single key, then the
/// `lmfx` list.
fn enabled<'a>(root: &'a Desc, single: &[u8], multi: &[u8]) -> Vec<&'a Desc> {
    let mut out: Vec<&Desc> = Vec::new();
    if let Some(d) = root.obj(single) {
        out.push(d);
    }
    if let Some(Val::List(items)) = root.get(multi) {
        out.extend(items.iter().filter_map(|v| match v {
            Val::Obj(o) => Some(o),
            _ => None,
        }));
    }
    out.retain(|d| flag(d, b"enab").unwrap_or(true));
    out
}

/// Decode an `lfx2` or `lmfx` block into our effects. `None` when the block
/// is unreadable or holds nothing we render.
pub(super) fn parse_effects(
    data: &[u8],
    light: GlobalLight,
    layer: &str,
    warnings: &mut Vec<String>,
) -> Option<LayerEffects> {
    // Object-effects version, then the descriptor.
    let root = parse_descriptor(data.get(4..)?)?;
    // The style's `Scl ` is informational: sizes are stored as rendered
    // (checked against Photoshop's own composites).
    let mut r = Reader {
        layer,
        light,
        warnings,
    };
    if flag(&root, b"masterFXSwitch") == Some(false) {
        r.warn("layer effects are turned off and were not imported".into());
        return None;
    }
    let mut fx = LayerEffects::default();
    let first = |r: &mut Reader, single: &[u8], multi: &[u8], what: &str| -> Option<Desc> {
        let list = enabled(&root, single, multi);
        if list.len() > 1 {
            r.warn(format!("{} {what} effects; only the first is used", list.len()));
        }
        list.first().map(|d| (*d).clone())
    };
    if let Some(d) = first(&mut r, b"DrSh", b"dropShadowMulti", "drop shadow") {
        fx.drop_shadow = Some(r.shadow(&d, "drop shadow"));
    }
    if let Some(d) = first(&mut r, b"IrSh", b"innerShadowMulti", "inner shadow") {
        let mut s = r.shadow(&d, "inner shadow");
        s.knockout = false;
        fx.inner_shadow = Some(s);
    }
    if let Some(d) = first(&mut r, b"OrGl", b"outerGlowMulti", "outer glow") {
        fx.outer_glow = Some(r.glow(&d, "outer glow"));
    }
    if let Some(d) = first(&mut r, b"IrGl", b"innerGlowMulti", "inner glow") {
        if enum_val(&d, b"glwS").as_deref() == Some(b"SrcC") {
            r.warn("inner glow from the centre is rendered from the edge".into());
        }
        fx.inner_glow = Some(r.glow(&d, "inner glow"));
    }
    if let Some(d) = first(&mut r, b"SoFi", b"solidFillMulti", "colour overlay") {
        fx.color_overlay = Some(ColorOverlayFx {
            color: r.color(&d, b"Clr ", "colour overlay"),
            opacity: Reader::pct(&d, b"Opct", 100.0),
            blend: r.blend(&d, b"Md  ", "colour overlay"),
        });
    }
    if let Some(d) = first(&mut r, b"GrFl", b"gradientFillMulti", "gradient overlay") {
        match gradient_of(&d) {
            Some(fill) => {
                let (start, end, angle) = match &fill {
                    Fill::Gradient { gradient, angle, .. } => {
                        let s = gradient.sorted();
                        (
                            s.first().map_or([0.0; 3], |s| s.color),
                            s.last().map_or([1.0; 3], |s| s.color),
                            *angle,
                        )
                    }
                    Fill::Solid { color } => (*color, *color, 90.0),
                };
                fx.gradient_overlay = Some(GradientOverlayFx {
                    start,
                    end,
                    angle,
                    opacity: Reader::pct(&d, b"Opct", 100.0),
                    blend: r.blend(&d, b"Md  ", "gradient overlay"),
                    fill: Some(fill),
                });
            }
            None => r.warn("gradient overlay settings are not readable".into()),
        }
    }
    if let Some(d) = first(&mut r, b"FrFX", b"frameFXMulti", "stroke") {
        let position = match enum_val(&d, b"Styl").as_deref() {
            Some(b"InsF") => StrokeAlign::Inside,
            Some(b"CtrF") => StrokeAlign::Center,
            _ => StrokeAlign::Outside,
        };
        let color = match enum_val(&d, b"PntT").as_deref() {
            Some(b"GrFl") => {
                r.warn("gradient stroke is rendered in a single colour".into());
                gradient_of(&d)
                    .and_then(|f| match f {
                        Fill::Gradient { gradient, .. } => gradient.sorted().first().map(|s| s.color),
                        Fill::Solid { color } => Some(color),
                    })
                    .unwrap_or([0.0; 3])
            }
            Some(b"Ptrn") => {
                r.warn("pattern stroke is rendered in grey".into());
                [crate::srgb_to_linear_f(0.5); 3]
            }
            _ => r.color(&d, b"Clr ", "stroke"),
        };
        fx.stroke = Some(StrokeFx {
            size: r.px(&d, b"Sz  ", 3.0),
            color,
            opacity: Reader::pct(&d, b"Opct", 100.0),
            position,
            blend: r.blend(&d, b"Md  ", "stroke"),
        });
    }
    if let Some(d) = first(&mut r, b"ebbl", b"bevelEmbossMulti", "bevel") {
        match enum_val(&d, b"bvlS").as_deref() {
            Some(b"InrB") | None => {}
            Some(other) => r.warn(format!(
                "bevel style '{}' is rendered as an inner bevel",
                String::from_utf8_lossy(other).trim()
            )),
        }
        let mut angle = r.angle(&d);
        if enum_val(&d, b"bvlD").as_deref() == Some(b"Out ") {
            // Direction down: the light comes from the opposite side.
            angle += 180.0;
        }
        let (hi, lo) = (Reader::pct(&d, b"hglO", 75.0), Reader::pct(&d, b"sdwO", 75.0));
        fx.bevel = Some(BevelFx {
            size: blur_of_size(r.px(&d, b"blur", 5.0)),
            depth: (d.num(b"srgR").unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 10.0),
            angle,
            highlight: r.color(&d, b"hglC", "bevel highlight"),
            shadow: r.color(&d, b"sdwC", "bevel shadow"),
            opacity: hi,
            shadow_opacity: (lo != hi).then_some(lo),
        });
    }
    if !enabled(&root, b"ChFX", b"satinMulti").is_empty() {
        r.warn("satin effect is not supported yet and was not imported".into());
    }
    if !enabled(&root, b"patternFill", b"patternFillMulti").is_empty() {
        r.warn("pattern overlay effect is not supported yet and was not imported".into());
    }
    (!fx.is_empty()).then_some(fx)
}

/// Fill opacity from an `iOpa` block.
pub(super) fn parse_fill_opacity(data: &[u8]) -> Option<f32> {
    data.first().map(|&b| b as f32 / 255.0)
}

// ---- export ---------------------------------------------------------------------------------

fn unit(u: &[u8; 4], v: f32) -> Val {
    Val::Unit(*u, v as f64)
}

fn mode(m: BlendMode) -> Val {
    Val::Enum(b"BlnM".to_vec(), mode_id(m).to_vec())
}

/// Photoshop's linear contour, which every effect object carries.
fn linear_contour() -> Val {
    let pt = |v: f64| {
        Val::Obj(
            Desc::new(b"CrPt")
                .with(b"Hrzn", Val::Doub(v))
                .with(b"Vrtc", Val::Doub(v)),
        )
    };
    Val::Obj(
        Desc::new(b"ShpC")
            .with(b"Nm  ", Val::Text("Linear".into()))
            .with(b"Crv ", Val::List(vec![pt(0.0), pt(255.0)])),
    )
}

fn head(class: &[u8]) -> Desc {
    Desc::new(class)
        .with(b"enab", Val::Bool(true))
        .with(b"present", Val::Bool(true))
        .with(b"showInDialog", Val::Bool(true))
}

fn pct(v: f32) -> Val {
    unit(b"#Prc", (v * 100.0).clamp(0.0, 100.0))
}

fn shadow_desc(class: &[u8], s: &ShadowFx, drop: bool) -> Desc {
    let dist = s.dx.hypot(s.dy);
    // The light shines from opposite the offset.
    let angle = if dist > 0.0 {
        s.dy.atan2(-s.dx).to_degrees()
    } else {
        120.0
    };
    let blur = lumenply_doc::sane_radius(s.blur);
    let grow = lumenply_doc::sane_radius(s.spread);
    let size = size_of_blur(blur) + grow;
    let spread = if size > 0.0 { grow / size } else { 0.0 };
    let mut d = head(class)
        .with(b"Md  ", mode(s.blend))
        .with(b"Clr ", rgbc(s.color))
        .with(b"Opct", pct(s.opacity))
        .with(b"uglg", Val::Bool(false))
        .with(b"lagl", unit(b"#Ang", angle))
        .with(b"Dstn", unit(b"#Pxl", dist))
        .with(b"Ckmt", unit(b"#Pxl", (spread * 100.0).round()))
        .with(b"blur", unit(b"#Pxl", size))
        .with(b"Nose", unit(b"#Prc", 0.0))
        .with(b"AntA", Val::Bool(false))
        .with(b"TrnS", linear_contour());
    if drop {
        d = d.with(b"layerConceals", Val::Bool(s.knockout));
    }
    d
}

fn glow_desc(class: &[u8], g: &GlowFx, inner: bool) -> Desc {
    let blur = lumenply_doc::sane_radius(g.blur);
    let grow = lumenply_doc::sane_radius(g.spread);
    let size = size_of_blur(blur) + grow;
    let spread = if size > 0.0 { grow / size } else { 0.0 };
    let mut d = head(class)
        .with(b"Md  ", mode(g.blend))
        .with(b"Clr ", rgbc(g.color))
        .with(b"Opct", pct(g.opacity))
        .with(b"GlwT", Val::Enum(b"BETE".to_vec(), b"SfBL".to_vec()))
        .with(b"Ckmt", unit(b"#Pxl", (spread * 100.0).round()))
        .with(b"blur", unit(b"#Pxl", size))
        .with(b"Nose", unit(b"#Prc", 0.0))
        .with(b"ShdN", unit(b"#Prc", 0.0))
        .with(b"AntA", Val::Bool(false))
        .with(b"TrnS", linear_contour())
        .with(b"Inpr", unit(b"#Prc", 50.0));
    if inner {
        d = d.with(b"glwS", Val::Enum(b"IGSr".to_vec(), b"SrcE".to_vec()));
    }
    d
}

/// The effects as an `lfx2` descriptor (`None` when there are none).
fn effects_desc(fx: &LayerEffects) -> Option<Desc> {
    if fx.is_empty() {
        return None;
    }
    let mut root = Desc::new(b"null")
        .with(b"Scl ", unit(b"#Prc", 100.0))
        .with(b"masterFXSwitch", Val::Bool(true));
    if let Some(s) = &fx.drop_shadow {
        root = root.with(b"DrSh", Val::Obj(shadow_desc(b"DrSh", s, true)));
    }
    if let Some(s) = &fx.inner_shadow {
        root = root.with(b"IrSh", Val::Obj(shadow_desc(b"IrSh", s, false)));
    }
    if let Some(g) = &fx.outer_glow {
        root = root.with(b"OrGl", Val::Obj(glow_desc(b"OrGl", g, false)));
    }
    if let Some(g) = &fx.inner_glow {
        root = root.with(b"IrGl", Val::Obj(glow_desc(b"IrGl", g, true)));
    }
    if let Some(b) = &fx.bevel {
        root = root.with(
            b"ebbl",
            Val::Obj(
                head(b"ebbl")
                    .with(b"hglM", mode(BlendMode::Screen))
                    .with(b"hglC", rgbc(b.highlight))
                    .with(b"hglO", pct(b.opacity))
                    .with(b"sdwM", mode(BlendMode::Multiply))
                    .with(b"sdwC", rgbc(b.shadow))
                    .with(b"sdwO", pct(b.shadow_opacity.unwrap_or(b.opacity)))
                    .with(b"bvlT", Val::Enum(b"bvlT".to_vec(), b"SfBL".to_vec()))
                    .with(b"bvlS", Val::Enum(b"BESl".to_vec(), b"InrB".to_vec()))
                    .with(b"uglg", Val::Bool(false))
                    .with(b"lagl", unit(b"#Ang", b.angle))
                    .with(b"Lald", unit(b"#Ang", 30.0))
                    .with(b"srgR", unit(b"#Prc", (b.depth * 100.0).clamp(1.0, 1000.0)))
                    .with(
                        b"blur",
                        unit(b"#Pxl", size_of_blur(lumenply_doc::sane_radius(b.size))),
                    )
                    .with(b"bvlD", Val::Enum(b"BESs".to_vec(), b"In  ".to_vec()))
                    .with(b"TrnS", linear_contour())
                    .with(b"antialiasGloss", Val::Bool(false))
                    .with(b"Sftn", unit(b"#Pxl", 0.0))
                    .with(b"useShape", Val::Bool(false))
                    .with(b"useTexture", Val::Bool(false)),
            ),
        );
    }
    if let Some(c) = &fx.color_overlay {
        root = root.with(
            b"SoFi",
            Val::Obj(
                head(b"SoFi")
                    .with(b"Md  ", mode(c.blend))
                    .with(b"Clr ", rgbc(c.color))
                    .with(b"Opct", pct(c.opacity)),
            ),
        );
    }
    if let Some(g) = &fx.gradient_overlay {
        let fill = g.fill.clone().unwrap_or_else(|| Fill::Gradient {
            gradient: lumenply_doc::Gradient {
                stops: vec![
                    lumenply_doc::GradientStop::new(0.0, g.start),
                    lumenply_doc::GradientStop::new(1.0, g.end),
                ],
            },
            style: lumenply_doc::GradientStyle::Linear,
            angle: g.angle,
            scale: 1.0,
            reverse: false,
            offset: [0.0, 0.0],
        });
        let mut d = head(b"GrFl")
            .with(b"Md  ", mode(g.blend))
            .with(b"Opct", pct(g.opacity));
        // The gradient fill layer's descriptor carries the same keys.
        if let Some(src) = parse_descriptor(&fill_block(&fill).1) {
            for (k, v) in src.items {
                d = d.with(&k, v);
            }
        }
        root = root.with(b"GrFl", Val::Obj(d));
    }
    if let Some(s) = &fx.stroke {
        let styl: &[u8] = match s.position {
            StrokeAlign::Outside => b"OutF",
            StrokeAlign::Inside => b"InsF",
            StrokeAlign::Center => b"CtrF",
        };
        root = root.with(
            b"FrFX",
            Val::Obj(
                head(b"FrFX")
                    .with(b"Styl", Val::Enum(b"FStl".to_vec(), styl.to_vec()))
                    .with(b"PntT", Val::Enum(b"FrFl".to_vec(), b"SClr".to_vec()))
                    .with(b"Md  ", mode(s.blend))
                    .with(b"Opct", pct(s.opacity))
                    .with(b"Sz  ", unit(b"#Pxl", lumenply_doc::sane_radius(s.size)))
                    .with(b"Clr ", rgbc(s.color))
                    .with(b"overprint", Val::Bool(false)),
            ),
        );
    }
    Some(root)
}

/// The extra tagged blocks for a layer's effects (`lfx2`) and fill
/// opacity (`iOpa`).
pub(super) fn layer_blocks(l: &Layer) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if l.fill_opacity < 1.0 {
        let v = (l.fill_opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        out.push(additional_block(b"iOpa", &[v, 0, 0, 0]));
    }
    if let Some(desc) = effects_desc(&l.effects) {
        let mut data = Vec::new();
        put_u32(&mut data, 0); // object-effects version
        data.extend_from_slice(&descriptor_block(&desc));
        out.push(additional_block(b"lfx2", &data));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn rgb(r: f64, g: f64, b: f64) -> Val {
        Val::Obj(
            Desc::new(b"RGBC")
                .with(b"Rd  ", Val::Doub(r))
                .with(b"Grn ", Val::Doub(g))
                .with(b"Bl  ", Val::Doub(b)),
        )
    }

    fn block(root: Desc) -> Vec<u8> {
        let mut data = vec![0, 0, 0, 0];
        data.extend_from_slice(&descriptor_block(&root));
        data
    }

    fn en(id: &[u8]) -> Val {
        Val::Enum(b"BlnM".to_vec(), id.to_vec())
    }

    #[test]
    fn drop_shadow_reads_angle_distance_size_spread_and_mode() {
        let ds = Desc::new(b"DrSh")
            .with(b"enab", Val::Bool(true))
            .with(b"Md  ", en(b"Mltp"))
            .with(b"Clr ", rgb(255.0, 0.0, 0.0))
            .with(b"Opct", Val::Unit(*b"#Prc", 50.0))
            .with(b"uglg", Val::Bool(true))
            .with(b"lagl", Val::Unit(*b"#Ang", 45.0))
            .with(b"Dstn", Val::Unit(*b"#Pxl", 10.0))
            .with(b"Ckmt", Val::Unit(*b"#Pxl", 25.0))
            .with(b"blur", Val::Unit(*b"#Pxl", 12.0))
            .with(b"layerConceals", Val::Bool(false));
        let root = Desc::new(b"null")
            .with(b"Scl ", Val::Unit(*b"#Prc", 100.0))
            .with(b"masterFXSwitch", Val::Bool(true))
            .with(b"DrSh", Val::Obj(ds));
        let mut w = Vec::new();
        // "Use global light" picks the document's 90° over the effect's 45°.
        let light = GlobalLight {
            angle: 90.0,
            altitude: 30.0,
        };
        let fx = parse_effects(&block(root), light, "L", &mut w).expect("effects");
        assert!(w.is_empty(), "{w:?}");
        let s = fx.drop_shadow.expect("shadow");
        // Light from above: the shadow falls 10 px straight down.
        assert!(close(s.dx, 0.0) && close(s.dy, 10.0), "{s:?}");
        // 25% of the 12 px size is solid (3 px), the rest (9 px) blurs.
        assert!(close(s.spread, 3.0));
        assert!(close(s.blur, 9.0 / 3f32.sqrt()));
        assert_eq!(s.color, [1.0, 0.0, 0.0]);
        assert!(close(s.opacity, 0.5));
        assert_eq!(s.blend, BlendMode::Multiply);
        assert!(!s.knockout);
        assert!(fx.stroke.is_none() && fx.outer_glow.is_none());
    }

    #[test]
    fn disabled_effects_master_switch_and_unsupported_kinds() {
        let off = Desc::new(b"SoFi")
            .with(b"enab", Val::Bool(false))
            .with(b"Clr ", rgb(0.0, 0.0, 0.0));
        let stroke = |size: f64, styl: &[u8]| {
            Val::Obj(
                Desc::new(b"FrFX")
                    .with(b"enab", Val::Bool(true))
                    .with(b"Styl", Val::Enum(b"FStl".to_vec(), styl.to_vec()))
                    .with(b"PntT", Val::Enum(b"FrFl".to_vec(), b"SClr".to_vec()))
                    .with(b"Md  ", en(b"Nrml"))
                    .with(b"Opct", Val::Unit(*b"#Prc", 100.0))
                    .with(b"Sz  ", Val::Unit(*b"#Pxl", size))
                    .with(b"Clr ", rgb(0.0, 0.0, 255.0)),
            )
        };
        let root = Desc::new(b"null")
            .with(b"Scl ", Val::Unit(*b"#Prc", 200.0))
            .with(b"SoFi", Val::Obj(off))
            .with(
                b"frameFXMulti",
                Val::List(vec![stroke(4.0, b"InsF"), stroke(9.0, b"OutF")]),
            )
            .with(
                b"ChFX",
                Val::Obj(Desc::new(b"ChFX").with(b"enab", Val::Bool(true))),
            );
        let mut w = Vec::new();
        let fx = parse_effects(&block(root.clone()), GlobalLight::default(), "L", &mut w).unwrap();
        assert!(fx.color_overlay.is_none(), "disabled overlay skipped");
        let st = fx.stroke.unwrap();
        // The first enabled stroke, at its stored size (the style's 200%
        // scale is informational).
        assert_eq!(st.position, StrokeAlign::Inside);
        assert!(close(st.size, 4.0));
        assert_eq!(st.color, [0.0, 0.0, 1.0]);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("2 stroke effects"));
        assert!(w[1].contains("satin"));
        // Master switch off: nothing, one warning.
        let mut w = Vec::new();
        let off = root.with(b"masterFXSwitch", Val::Bool(false));
        assert!(parse_effects(&block(off), GlobalLight::default(), "L", &mut w).is_none());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn global_light_reads_resources_1037_and_1049() {
        let mut res = Vec::new();
        for (id, v) in [(1037u16, 75i32), (1049, 42)] {
            res.extend_from_slice(b"8BIM");
            res.extend_from_slice(&id.to_be_bytes());
            res.extend_from_slice(&[0, 0, 0, 0, 0, 4]);
            res.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(
            global_light(&res),
            GlobalLight {
                angle: 75.0,
                altitude: 42.0
            }
        );
        assert_eq!(global_light(&[]), GlobalLight::default());
    }

    #[test]
    fn every_effect_round_trips_through_lfx2() {
        let fx = LayerEffects {
            drop_shadow: Some(ShadowFx {
                dx: 3.0,
                dy: 4.0,
                blur: 6.0,
                spread: 2.0,
                blend: BlendMode::Multiply,
                ..ShadowFx::default()
            }),
            inner_shadow: Some(ShadowFx {
                knockout: false,
                ..ShadowFx::default()
            }),
            outer_glow: Some(GlowFx {
                blend: BlendMode::Screen,
                ..GlowFx::default()
            }),
            inner_glow: Some(GlowFx::default()),
            color_overlay: Some(ColorOverlayFx::default()),
            gradient_overlay: Some(GradientOverlayFx::default()),
            bevel: Some(BevelFx {
                shadow_opacity: Some(0.4),
                ..BevelFx::default()
            }),
            stroke: Some(StrokeFx {
                position: StrokeAlign::Center,
                ..StrokeFx::default()
            }),
        };
        let mut l = Layer::pixel(1, "L");
        l.effects = fx.clone();
        l.fill_opacity = 0.2;
        let blocks = layer_blocks(&l);
        assert_eq!(blocks.len(), 2);
        // iOpa: signature, key, length 4, then 20% of 255 = 51.
        assert_eq!(&blocks[0][4..8], b"iOpa");
        assert_eq!(blocks[0][12], 51);
        assert_eq!(&blocks[1][4..8], b"lfx2");
        let mut w = Vec::new();
        let back = parse_effects(&blocks[1][12..], GlobalLight::default(), "L", &mut w).unwrap();
        assert!(w.is_empty(), "{w:?}");
        let (a, b) = (fx.drop_shadow.unwrap(), back.drop_shadow.unwrap());
        assert!(close(a.dx, b.dx) && close(a.dy, b.dy), "{a:?} {b:?}");
        // Spread and blur survive the size/percent conversion closely.
        assert!(
            (a.spread - b.spread).abs() < 0.2 && (a.blur - b.blur).abs() < 0.2,
            "{a:?} {b:?}"
        );
        assert_eq!(b.blend, BlendMode::Multiply);
        assert!(b.knockout);
        assert!(!back.inner_shadow.unwrap().knockout);
        assert_eq!(back.outer_glow.unwrap().blend, BlendMode::Screen);
        assert!(back.inner_glow.is_some());
        let co = back.color_overlay.unwrap();
        assert!(co
            .color
            .iter()
            .zip(ColorOverlayFx::default().color)
            .all(|(x, y)| (x - y).abs() < 0.01));
        let go = back.gradient_overlay.unwrap();
        assert!(go.fill.is_some());
        assert!(close(go.angle, 90.0));
        let bv = back.bevel.unwrap();
        assert!(close(bv.opacity, 0.75) && bv.shadow_opacity.is_some_and(|o| close(o, 0.4)));
        assert!((bv.size - BevelFx::default().size).abs() < 0.01);
        let st = back.stroke.unwrap();
        assert_eq!(st.position, StrokeAlign::Center);
        assert!(close(st.size, 3.0));
    }

    #[test]
    fn blend_modes_map_by_name_both_ways() {
        assert_eq!(mode_of(b"Scrn"), Some(BlendMode::Screen));
        assert_eq!(mode_of(b"linearDodge"), Some(BlendMode::Add));
        assert_eq!(mode_of(b"normal"), Some(BlendMode::Normal));
        assert_eq!(mode_of(b"softLight"), Some(BlendMode::SoftLight));
        assert_eq!(mode_id(BlendMode::HardLight), b"HrdL");
        assert_eq!(mode_id(BlendMode::Normal), b"Nrml");
    }
}
