//! Photoshop type layers (`TySh`, the "type tool object setting") both ways.
//!
//! The block holds a transform, a `TxLr` descriptor whose `EngineData`
//! (see [`super::engine_data`]) carries the characters, style runs,
//! paragraph runs and font list, a warp descriptor and a bounds box.
//!
//! **Import** maps the first paragraph's alignment and the style that
//! covers most characters onto the layer, and the other style runs onto
//! [`TextRun`]s (colour, size, bold, italic, underline, strikethrough).
//! Fonts arrive as PostScript names and map through
//! [`lumenply_render::font_names`]. Layers our model cannot draw in place
//! — rotated, skewed, flipped or stretched transforms, warps, vertical or
//! on-path text — return `None` so the caller keeps Photoshop's pixels.
//!
//! **Export** writes the same structure Photoshop CS6 does (the layer's
//! rendered pixels stay beside it as the preview other readers show), so
//! Photoshop opens the layer as editable type.

use lumenply_doc::text_runs::{CharStyle, TextRun};
use lumenply_doc::{TextAlign, TextLayer};
use lumenply_render::font_names;
use lumenply_tiles::Rect;

use super::engine_data::{self, dict, Value};
use super::extra::{read_descriptor, Desc, Val};
use super::{put_u16, put_u32, Rd};
use crate::{linear_to_srgb_f, srgb_to_linear_f};

/// Photoshop's `AutoLeading` default: line height 120% of the font size.
const AUTO_LEADING: f32 = 1.2;

/// Style runs read per layer; real documents stay far below this.
const MAX_RUNS: usize = 20_000;

/// Photoshop sets a paragraph box's first baseline the height of a "d"
/// below the box top (Adobe's "Ascent" first baseline); our layout puts it
/// the font's full ascent below (ADR 0013), a few pixels lower. Import
/// raises the box by the difference and export lowers it back, so the
/// text lands where Photoshop draws it and the bottom edge stays put.
/// Whole 1/32 px, so adding and removing it is exact, and EngineData's
/// five decimals write it exactly.
fn box_baseline_gap(t: &TextLayer) -> f32 {
    let font = lumenply_render::text::font_for(t);
    if font.lookup_glyph_index('d') == 0 || t.size.is_nan() || t.size <= 0.0 {
        return 0.0;
    }
    let b = font.metrics('d', t.size).bounds;
    let d_top = b.ymin + b.height;
    let ascent = lumenply_render::text_layout::layout(t).ascent;
    if d_top.is_nan() || d_top <= 0.0 || !ascent.is_finite() {
        return 0.0;
    }
    ((ascent - d_top) * 32.0).round() / 32.0
}

// ---- import -------------------------------------------------------------------------------

/// The parts of a `TySh` block the importer uses.
struct TypeTool {
    /// xx, xy, yx, yy, tx, ty.
    transform: [f64; 6],
    text: Desc,
    warp: Option<Desc>,
}

fn read_tysh(data: &[u8]) -> Option<TypeTool> {
    let mut d = Rd::new(data);
    let _version = d.u16().ok()?;
    let mut transform = [0.0; 6];
    for v in &mut transform {
        *v = f64::from_be_bytes(d.bytes(8).ok()?.try_into().ok()?);
    }
    let _text_version = d.u16().ok()?;
    let text = read_descriptor(&mut d)?;
    // The warp may be missing in truncated files; the text still reads.
    let warp = d.u16().ok().and_then(|_| read_descriptor(&mut d));
    Some(TypeTool {
        transform,
        text,
        warp,
    })
}

/// A Photoshop colour (`<< /Type 1 /Values [ a r g b ] >>`, 0–1 sRGB) as
/// straight linear RGBA.
fn color_of(v: &Value) -> Option<[f32; 4]> {
    let vals = v.get("Values")?.nums();
    if vals.len() < 4 {
        return None;
    }
    let c = |x: f64| srgb_to_linear_f((x as f32).clamp(0.0, 1.0));
    Some([
        c(vals[1]),
        c(vals[2]),
        c(vals[3]),
        (vals[0] as f32).clamp(0.0, 1.0),
    ])
}

/// Looks a key up in a run's own dictionary, then its defaults.
struct Cascade<'a>(Vec<&'a Value>);

impl<'a> Cascade<'a> {
    fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.iter().find_map(|d| d.get(key))
    }
    fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::num)
    }
    fn flag(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::bool)
    }
}

/// One Photoshop style run, resolved, in source units (before the
/// transform's scale).
#[derive(Clone, Debug, PartialEq)]
struct PsStyle {
    font: String,
    size: f64,
    faux_bold: bool,
    faux_italic: bool,
    color: [f32; 4],
    tracking: f64,
    auto_leading: bool,
    leading: f64,
    baseline_shift: f64,
    caps: i64,
    underline: bool,
    strikethrough: bool,
    auto_kern: bool,
    h_scale: f64,
    v_scale: f64,
}

fn resolve_style(c: &Cascade, fonts: &[String]) -> PsStyle {
    let font = c
        .get("Font")
        .and_then(Value::int)
        .and_then(|i| fonts.get(usize::try_from(i).ok()?))
        .cloned()
        .unwrap_or_default();
    PsStyle {
        font,
        size: c.num("FontSize").unwrap_or(12.0),
        faux_bold: c.flag("FauxBold").unwrap_or(false),
        faux_italic: c.flag("FauxItalic").unwrap_or(false),
        color: c
            .get("FillColor")
            .and_then(color_of)
            .unwrap_or([0.0, 0.0, 0.0, 1.0]),
        tracking: c.num("Tracking").unwrap_or(0.0),
        auto_leading: c.flag("AutoLeading").unwrap_or(true),
        leading: c.num("Leading").unwrap_or(0.0),
        baseline_shift: c.num("BaselineShift").unwrap_or(0.0),
        caps: c.get("FontCaps").and_then(Value::int).unwrap_or(0),
        underline: c.flag("Underline").unwrap_or(false),
        strikethrough: c.flag("Strikethrough").unwrap_or(false),
        auto_kern: c.flag("AutoKerning").unwrap_or(true),
        h_scale: c.num("HorizontalScale").unwrap_or(1.0),
        v_scale: c.num("VerticalScale").unwrap_or(1.0),
    }
}

/// `(start, end)` in UTF-16 units of each run, from a run-length array.
fn run_spans(runs: &Value) -> Vec<(usize, usize)> {
    let mut at = 0usize;
    runs.get("RunLengthArray")
        .map(Value::nums)
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            let start = at;
            at = at.saturating_add(n.max(0.0) as usize);
            (start, at)
        })
        .collect()
}

fn align_of(justification: i64) -> TextAlign {
    match justification {
        1 => TextAlign::Right,
        2 => TextAlign::Center,
        3..=6 => TextAlign::Justify,
        _ => TextAlign::Left,
    }
}

/// Why a type layer is kept as Photoshop's pixels, or `None` when our
/// text model can draw it where Photoshop does.
fn unsupported(tt: &TypeTool, engine: &Value) -> Option<&'static str> {
    let [xx, xy, yx, yy, _, _] = tt.transform;
    if !tt.transform.iter().all(|v| v.is_finite()) {
        return Some("its transform is not readable");
    }
    let scale = xx.abs().max(yy.abs()).max(1e-9);
    if xy.abs() > scale * 1e-4 || yx.abs() > scale * 1e-4 {
        return Some("it is rotated or skewed");
    }
    if xx <= 0.0 || yy <= 0.0 {
        return Some("it is flipped");
    }
    if (xx / yy - 1.0).abs() > 0.01 {
        return Some("it is stretched");
    }
    if let Some(Val::Enum(_, style)) = tt.warp.as_ref().and_then(|w| w.get(b"warpStyle")) {
        if style != b"warpNone" {
            return Some("it is warped");
        }
    }
    if let Some(Val::Enum(_, o)) = tt.text.get(b"Ornt") {
        if o == b"Vrtc" {
            return Some("it is vertical text");
        }
    }
    let shape = engine
        .at(&["EngineDict", "Rendered", "Shapes", "Children"])
        .and_then(Value::array)
        .and_then(|c| c.first());
    if let Some(kind) = shape.and_then(|s| s.get("ShapeType")).and_then(Value::int) {
        if kind > 1 {
            return Some("it is text on a path");
        }
    }
    None
}

/// Read a `TySh` block into a text layer. `None` (with the reason pushed
/// to `warnings`) when it is unreadable or our model cannot place it; the
/// caller then keeps Photoshop's rendered pixels.
pub(super) fn import(data: &[u8], name: &str, warnings: &mut Vec<String>) -> Option<TextLayer> {
    let pixels = |why: &str, warnings: &mut Vec<String>| {
        warnings.push(format!("text layer '{name}' was imported as pixels ({why})"));
        None
    };
    let Some(tt) = read_tysh(data) else {
        return pixels("its type settings are not readable", warnings);
    };
    let engine = match tt.text.get(b"EngineData") {
        Some(Val::Raw(bytes)) => engine_data::parse(bytes),
        _ => None,
    };
    let Some(engine) = engine else {
        return pixels("its type settings are not readable", warnings);
    };
    if let Some(why) = unsupported(&tt, &engine) {
        return pixels(why, warnings);
    }
    let [xx, _, _, yy, tx, ty] = tt.transform;
    let scale = yy;

    // Characters: Photoshop ends every text with a paragraph break and
    // uses \r between paragraphs and U+0003 for a forced line break.
    let raw: String = engine
        .at(&["EngineDict", "Editor", "Text"])
        .and_then(Value::str)
        .map(str::to_string)
        .or_else(|| match tt.text.get(b"Txt ") {
            Some(Val::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let raw = raw.strip_suffix('\r').unwrap_or(&raw);
    let mut text = String::with_capacity(raw.len());
    // Byte offset of every UTF-16 unit (one past the end included).
    let mut byte_at: Vec<usize> = Vec::with_capacity(raw.len() + 1);
    for ch in raw.chars() {
        let ch = if matches!(ch, '\r' | '\u{3}') { '\n' } else { ch };
        for _ in 0..ch.len_utf16() {
            byte_at.push(text.len());
        }
        text.push(ch);
    }
    byte_at.push(text.len());
    let units = byte_at.len() - 1;
    let byte = |u: usize| byte_at[u.min(units)];

    // Style runs, each resolved through the run's defaults and the
    // document's normal style sheet.
    let res = engine.get("ResourceDict");
    let fonts: Vec<String> = res
        .and_then(|r| r.get("FontSet"))
        .and_then(Value::array)
        .unwrap_or_default()
        .iter()
        .map(|f| f.get("Name").and_then(Value::str).unwrap_or_default().to_string())
        .collect();
    let nth = |set: &str, which: &str| {
        let i = res?.get(which).and_then(Value::int).unwrap_or(0);
        res?.get(set)?.array()?.get(usize::try_from(i).ok()?)
    };
    let normal_style = nth("StyleSheetSet", "TheNormalStyleSheet").and_then(|s| s.get("StyleSheetData"));
    let normal_para = nth("ParagraphSheetSet", "TheNormalParagraphSheet").and_then(|s| s.get("Properties"));
    let style_run = engine.at(&["EngineDict", "StyleRun"]);
    let default_style = style_run.and_then(|r| r.at(&["DefaultRunData", "StyleSheet", "StyleSheetData"]));
    let mut styles: Vec<(usize, usize, PsStyle)> = Vec::new();
    if let Some(sr) = style_run {
        let spans = run_spans(sr);
        let runs = sr.get("RunArray").and_then(Value::array).unwrap_or_default();
        // Bounded: a hostile file cannot make the style grouping below
        // (quadratic in distinct styles) take forever.
        for (run, (a, b)) in runs.iter().zip(spans).take(MAX_RUNS) {
            let own = run.at(&["StyleSheet", "StyleSheetData"]);
            let c = Cascade([own, default_style, normal_style].into_iter().flatten().collect());
            styles.push((a, b, resolve_style(&c, &fonts)));
        }
    }
    if styles.is_empty() {
        let c = Cascade([default_style, normal_style].into_iter().flatten().collect());
        styles.push((0, units + 1, resolve_style(&c, &fonts)));
    }
    // The layer takes the style covering the most characters.
    let base = {
        let mut best: Option<(usize, &PsStyle)> = None;
        for (_, _, s) in &styles {
            let n: usize = styles
                .iter()
                .filter(|(_, _, o)| o == s)
                .map(|(x, y, _)| y.saturating_sub(*x))
                .sum();
            if best.is_none_or(|(m, _)| n > m) {
                best = Some((n, s));
            }
        }
        best.map(|(_, s)| s.clone()).expect("at least one style")
    };

    // Paragraphs: the first one's alignment and auto-leading.
    let para_run = engine.at(&["EngineDict", "ParagraphRun"]);
    let default_para = para_run.and_then(|r| r.at(&["DefaultRunData", "ParagraphSheet", "Properties"]));
    let paras: Vec<Cascade> = para_run
        .and_then(|r| r.get("RunArray"))
        .and_then(Value::array)
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let own = p.at(&["ParagraphSheet", "Properties"]);
            Cascade([own, default_para, normal_para].into_iter().flatten().collect())
        })
        .collect();
    let para0 = match paras.first() {
        Some(p) => Cascade(p.0.clone()),
        None => Cascade([default_para, normal_para].into_iter().flatten().collect()),
    };
    let justification = |p: &Cascade| p.get("Justification").and_then(Value::int).unwrap_or(0);
    let mut notes: Vec<&str> = Vec::new();
    if paras.iter().any(|p| justification(p) != justification(&para0)) {
        notes.push("paragraphs aligned differently");
    }
    if (4..=6).contains(&justification(&para0)) {
        notes.push("last lines of justified paragraphs aligned left");
    }

    let lf = font_names::from_postscript(&base.font);
    let mut t = TextLayer::new(text, tx as f32, ty as f32, (base.size * scale) as f32, base.color);
    t.font = lf.font.clone();
    t.bold = lf.bold || base.faux_bold;
    t.italic = lf.italic || base.faux_italic;
    t.align = align_of(justification(&para0));
    t.tracking = base.tracking as f32;
    t.line_height = if base.auto_leading {
        para0.num("AutoLeading").unwrap_or(AUTO_LEADING as f64) as f32
    } else if base.size > 0.0 {
        (base.leading / base.size) as f32
    } else {
        AUTO_LEADING
    };
    t.baseline_shift = (base.baseline_shift * scale) as f32;
    t.all_caps = base.caps != 0;
    if base.caps == 1 {
        notes.push("small caps shown as all caps");
    }
    t.kerning = base.auto_kern;
    t.underline = base.underline;
    t.strikethrough = base.strikethrough;
    if (base.h_scale - 1.0).abs() > 0.01 || (base.v_scale - 1.0).abs() > 0.01 {
        notes.push("horizontal or vertical character scaling");
    }

    // Placement: point text anchors its first baseline at the transform's
    // origin (plus PointBase); box text's box is BoxBounds in that space.
    let cookie = engine
        .at(&["EngineDict", "Rendered", "Shapes", "Children"])
        .and_then(Value::array)
        .and_then(|c| c.first())
        .and_then(|s| s.at(&["Cookie", "Photoshop"]));
    let shape_type = cookie
        .and_then(|c| c.get("ShapeType"))
        .and_then(Value::int)
        .unwrap_or(0);
    let bounds = cookie
        .and_then(|c| c.get("BoxBounds"))
        .map(Value::nums)
        .unwrap_or_default();
    if shape_type == 1 && bounds.len() == 4 {
        t.x = (tx + xx * bounds[0]) as f32;
        t.y = (ty + yy * bounds[1]) as f32;
        t.box_size = Some([
            ((bounds[2] - bounds[0]) * xx).max(1.0) as f32,
            ((bounds[3] - bounds[1]) * yy).max(1.0) as f32,
        ]);
    } else {
        let pb = cookie
            .and_then(|c| c.get("PointBase"))
            .map(Value::nums)
            .unwrap_or_default();
        if pb.len() == 2 {
            t.x = (tx + xx * pb[0]) as f32;
            t.y = (ty + yy * pb[1]) as f32;
        }
    }

    // Character runs: whatever differs from the layer's own style.
    let mut runs = Vec::new();
    let mut other_family = false;
    let mut other_spacing = false;
    for (a, b, s) in &styles {
        let (start, end) = (byte(*a), byte(*b));
        if start >= end || s == &base {
            continue;
        }
        let f = font_names::from_postscript(&s.font);
        other_family |= f.font != lf.font;
        other_spacing |= s.tracking != base.tracking
            || s.baseline_shift != base.baseline_shift
            || s.caps != base.caps
            || s.auto_leading != base.auto_leading
            || (!s.auto_leading
                && s.size > 0.0
                && (s.leading / s.size - base.leading / base.size).abs() > 0.01);
        let size = (s.size * scale) as f32;
        let bold = f.bold || s.faux_bold;
        let italic = f.italic || s.faux_italic;
        let style = CharStyle {
            color: (s.color != t.color).then_some(s.color),
            size: (size != t.size).then_some(size),
            bold: (bold != t.bold).then_some(bold),
            italic: (italic != t.italic).then_some(italic),
            underline: (s.underline != t.underline).then_some(s.underline),
            strikethrough: (s.strikethrough != t.strikethrough).then_some(s.strikethrough),
        };
        if !style.is_empty() {
            runs.push(TextRun { start, end, style });
        }
    }
    t.runs = runs;
    t.normalize_runs();
    if let Some([w, h]) = t.box_size {
        let gap = box_baseline_gap(&t);
        t.y -= gap;
        t.box_size = Some([w, (h + gap).max(1.0)]);
    }
    if other_family {
        notes.push("several fonts, shown in one");
    }
    if other_spacing {
        notes.push("per-character tracking, leading, shift or caps");
    }
    if !notes.is_empty() {
        warnings.push(format!(
            "text layer '{name}': simplified on import ({})",
            notes.join("; ")
        ));
    }
    Some(t)
}

/// How much our rendering of an imported type layer overlaps the pixels
/// Photoshop stored for it (intersection over union of the two ink
/// rectangles, 0–1). `None` when Photoshop stored no pixels to compare.
pub(super) fn overlap_with_photoshop(t: &TextLayer, photoshop: Rect) -> Option<f32> {
    if photoshop.is_empty() {
        return None;
    }
    let Some(ours) = t.cache.as_ref().and_then(|c| c.content_bounds()) else {
        return Some(0.0);
    };
    let area = |r: Rect| r.w as f32 * r.h as f32;
    let inter = area(ours.intersect(&photoshop));
    Some(inter / (area(ours) + area(photoshop) - inter).max(1.0))
}

/// Below this overlap the text is not where Photoshop draws it — a layout
/// the file keeps elsewhere, such as type on a path (stored in the
/// document-wide `Txt2` block, not in `TySh`) — so Photoshop's pixels are
/// kept instead. Font substitution alone stays far above it.
pub(super) const MIN_OVERLAP: f32 = 0.2;

// ---- export -------------------------------------------------------------------------------

/// Photoshop's Japanese line-breaking sets, written as it writes them.
fn kinsoku_set() -> Value {
    let set = |name: &str, no_start: &str, no_end: &str| {
        dict(vec![
            ("Name", Value::Str(name.into())),
            ("NoStart", Value::Str(no_start.into())),
            ("NoEnd", Value::Str(no_end.into())),
            ("Keep", Value::Str("\u{2015}\u{2025}".into())),
            ("Hanging", Value::Str("\u{3001}\u{3002}.,".into())),
        ])
    };
    Value::Array(vec![
        set(
            "PhotoshopKinsokuHard",
            "\u{3001}\u{3002}\u{ff0c}\u{ff0e}\u{30fb}\u{ff1a}\u{ff1b}\u{ff1f}\u{ff01}\u{30fc}\u{2015}\
             \u{2019}\u{201d}\u{ff09}\u{3015}\u{ff3d}\u{ff5d}\u{3009}\u{300b}\u{300d}\u{300f}\u{3011}\
             \u{30fd}\u{30fe}\u{309d}\u{309e}\u{3005}\u{3041}\u{3043}\u{3045}\u{3047}\u{3049}\u{3063}\
             \u{3083}\u{3085}\u{3087}\u{308e}\u{30a1}\u{30a3}\u{30a5}\u{30a7}\u{30a9}\u{30c3}\u{30e3}\
             \u{30e5}\u{30e7}\u{30ee}\u{30f5}\u{30f6}\u{309b}\u{309c}?!)]},.:;\u{2103}\u{2109}\u{a2}\
             \u{ff05}\u{2030}",
            "\u{2018}\u{201c}\u{ff08}\u{3014}\u{ff3b}\u{ff5b}\u{3008}\u{300a}\u{300c}\u{300e}\u{3010}\
             ([{\u{ffe5}\u{ff04}\u{a3}\u{ff20}\u{a7}\u{3012}\u{ff03}",
        ),
        set(
            "PhotoshopKinsokuSoft",
            "\u{3001}\u{3002}\u{ff0c}\u{ff0e}\u{30fb}\u{ff1a}\u{ff1b}\u{ff1f}\u{ff01}\u{2019}\u{201d}\
             \u{ff09}\u{3015}\u{ff3d}\u{ff5d}\u{3009}\u{300b}\u{300d}\u{300f}\u{3011}\u{30fd}\u{30fe}\
             \u{309d}\u{309e}\u{3005}",
            "\u{2018}\u{201c}\u{ff08}\u{3014}\u{ff3b}\u{ff5b}\u{3008}\u{300a}\u{300c}\u{300e}\u{3010}",
        ),
    ])
}

fn num(v: f32) -> Value {
    Value::Num(v as f64)
}

fn nums(v: &[f64]) -> Value {
    Value::Array(v.iter().map(|&x| Value::Num(x)).collect())
}

fn ps_color(c: [f32; 4]) -> Value {
    let s = |x: f32| (linear_to_srgb_f(x.clamp(0.0, 1.0)) as f64 * 1e5).round() / 1e5;
    dict(vec![
        ("Type", Value::Int(1)),
        ("Values", nums(&[1.0, s(c[0]), s(c[1]), s(c[2])])),
    ])
}

fn paragraph_properties(justification: i64) -> Value {
    dict(vec![
        ("Justification", Value::Int(justification)),
        ("FirstLineIndent", Value::Num(0.0)),
        ("StartIndent", Value::Num(0.0)),
        ("EndIndent", Value::Num(0.0)),
        ("SpaceBefore", Value::Num(0.0)),
        ("SpaceAfter", Value::Num(0.0)),
        ("AutoHyphenate", Value::Bool(true)),
        ("HyphenatedWordSize", Value::Int(6)),
        ("PreHyphen", Value::Int(2)),
        ("PostHyphen", Value::Int(2)),
        ("ConsecutiveHyphens", Value::Int(8)),
        ("Zone", Value::Num(36.0)),
        ("WordSpacing", nums(&[0.8, 1.0, 1.33])),
        ("LetterSpacing", nums(&[0.0, 0.0, 0.0])),
        ("GlyphSpacing", nums(&[1.0, 1.0, 1.0])),
        ("AutoLeading", Value::Num(AUTO_LEADING as f64)),
        ("LeadingType", Value::Int(0)),
        ("Hanging", Value::Bool(false)),
        ("Burasagari", Value::Bool(false)),
        ("KinsokuOrder", Value::Int(0)),
        ("EveryLineComposer", Value::Bool(false)),
    ])
}

/// The full character style Photoshop's normal style sheet spells out.
fn normal_style_data() -> Value {
    let black = dict(vec![
        ("Type", Value::Int(1)),
        ("Values", nums(&[1.0, 0.0, 0.0, 0.0])),
    ]);
    dict(vec![
        ("Font", Value::Int(1)),
        ("FontSize", Value::Num(12.0)),
        ("FauxBold", Value::Bool(false)),
        ("FauxItalic", Value::Bool(false)),
        ("AutoLeading", Value::Bool(true)),
        ("Leading", Value::Num(0.0)),
        ("HorizontalScale", Value::Num(1.0)),
        ("VerticalScale", Value::Num(1.0)),
        ("Tracking", Value::Int(0)),
        ("AutoKerning", Value::Bool(true)),
        ("Kerning", Value::Int(0)),
        ("BaselineShift", Value::Num(0.0)),
        ("FontCaps", Value::Int(0)),
        ("FontBaseline", Value::Int(0)),
        ("Underline", Value::Bool(false)),
        ("Strikethrough", Value::Bool(false)),
        ("Ligatures", Value::Bool(true)),
        ("DLigatures", Value::Bool(false)),
        ("BaselineDirection", Value::Int(2)),
        ("Tsume", Value::Num(0.0)),
        ("StyleRunAlignment", Value::Int(2)),
        ("Language", Value::Int(0)),
        ("NoBreak", Value::Bool(false)),
        ("FillColor", black.clone()),
        ("StrokeColor", black),
        ("FillFlag", Value::Bool(true)),
        ("StrokeFlag", Value::Bool(false)),
        ("FillFirst", Value::Bool(true)),
        ("YUnderline", Value::Int(1)),
        ("OutlineWidth", Value::Num(1.0)),
        ("CharacterDirection", Value::Int(0)),
        ("HindiNumbers", Value::Bool(false)),
        ("Kashida", Value::Int(1)),
        ("DiacriticPos", Value::Int(2)),
    ])
}

fn resources(fonts: &[String]) -> Value {
    // Index 0 is Photoshop's invisible placeholder font; ours follow.
    let font_set = std::iter::once("AdobeInvisFont")
        .chain(fonts.iter().map(String::as_str))
        .enumerate()
        .map(|(i, name)| {
            dict(vec![
                ("Name", Value::Str(name.into())),
                ("Script", Value::Int(0)),
                ("FontType", Value::Int(i64::from(i > 0))),
                ("Synthetic", Value::Int(0)),
            ])
        })
        .collect();
    let moji = (1..=4)
        .map(|i| {
            dict(vec![(
                "InternalName",
                Value::Str(format!("Photoshop6MojiKumiSet{i}")),
            )])
        })
        .collect();
    dict(vec![
        ("KinsokuSet", kinsoku_set()),
        ("MojiKumiSet", Value::Array(moji)),
        ("TheNormalStyleSheet", Value::Int(0)),
        ("TheNormalParagraphSheet", Value::Int(0)),
        (
            "ParagraphSheetSet",
            Value::Array(vec![dict(vec![
                ("Name", Value::Str("Normal RGB".into())),
                ("DefaultStyleSheet", Value::Int(0)),
                ("Properties", paragraph_properties(0)),
            ])]),
        ),
        (
            "StyleSheetSet",
            Value::Array(vec![dict(vec![
                ("Name", Value::Str("Normal RGB".into())),
                ("StyleSheetData", normal_style_data()),
            ])]),
        ),
        ("FontSet", Value::Array(font_set)),
        ("SuperscriptSize", Value::Num(0.583)),
        ("SuperscriptPosition", Value::Num(0.333)),
        ("SubscriptSize", Value::Num(0.583)),
        ("SubscriptPosition", Value::Num(0.333)),
        ("SmallCapSize", Value::Num(0.7)),
    ])
}

/// The layer's text as Photoshop stores it: \r between paragraphs.
fn ps_text(t: &TextLayer) -> String {
    t.text.replace("\r\n", "\n").replace(['\n', '\r'], "\r")
}

/// The EngineData for a text layer, in Photoshop's own layout.
fn engine_data(t: &TextLayer) -> Value {
    let text = ps_text(t);
    let full = format!("{text}\r");
    let boxed = t.box_size.is_some();

    // Style runs: cut at every run edge; the closing \r joins the last.
    let mut cuts: Vec<usize> = vec![0, t.text.len()];
    for r in &t.runs {
        cuts.extend([r.start.min(t.text.len()), r.end.min(t.text.len())]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut fonts: Vec<String> = Vec::new();
    let mut run_array = Vec::new();
    let mut lengths = Vec::new();
    let windows: Vec<(usize, usize)> = if t.text.is_empty() {
        vec![(0, 0)]
    } else {
        cuts.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let last = windows.len() - 1;
    for (i, &(a, b)) in windows.iter().enumerate() {
        let s = t.style_at(a);
        let (name, face_bold, face_italic) = font_names::to_postscript(&t.font, s.bold, s.italic);
        let font = match fonts.iter().position(|n| *n == name) {
            Some(i) => i,
            None => {
                fonts.push(name);
                fonts.len() - 1
            }
        } + 1;
        let auto = (t.line_height - AUTO_LEADING).abs() < 1e-4;
        let data = dict(vec![
            ("Font", Value::Int(font as i64)),
            ("FontSize", num(s.size)),
            ("FauxBold", Value::Bool(s.bold && !face_bold)),
            ("FauxItalic", Value::Bool(s.italic && !face_italic)),
            ("AutoLeading", Value::Bool(auto)),
            ("Leading", num(if auto { 0.0 } else { t.line_height * s.size })),
            ("HorizontalScale", Value::Num(1.0)),
            ("VerticalScale", Value::Num(1.0)),
            ("Tracking", Value::Int(t.tracking.round() as i64)),
            ("AutoKerning", Value::Bool(t.kerning)),
            ("Kerning", Value::Int(0)),
            ("BaselineShift", num(t.baseline_shift)),
            ("FontCaps", Value::Int(if t.all_caps { 2 } else { 0 })),
            ("Underline", Value::Bool(s.underline)),
            ("Strikethrough", Value::Bool(s.strikethrough)),
            ("FillColor", ps_color(s.color)),
        ]);
        run_array.push(dict(vec![("StyleSheet", dict(vec![("StyleSheetData", data)]))]));
        let n = t.text[a..b].encode_utf16().count() + usize::from(i == last);
        lengths.push(Value::Int(n as i64));
    }

    // Paragraph runs: one per paragraph, each with its closing \r.
    let justification = match t.align {
        TextAlign::Left => 0,
        TextAlign::Right => 1,
        TextAlign::Center => 2,
        TextAlign::Justify if boxed => 3,
        TextAlign::Justify => 0,
    };
    let paragraphs: Vec<usize> = full
        .split_inclusive('\r')
        .map(|p| p.encode_utf16().count())
        .collect();
    let para_runs = paragraphs
        .iter()
        .map(|_| {
            dict(vec![
                (
                    "ParagraphSheet",
                    dict(vec![
                        ("DefaultStyleSheet", Value::Int(0)),
                        ("Properties", paragraph_properties(justification)),
                    ]),
                ),
                ("Adjustments", adjustments()),
            ])
        })
        .collect();

    let shape_type = i64::from(boxed);
    let mut photoshop = vec![("ShapeType", Value::Int(shape_type))];
    match t.box_size {
        Some([w, h]) => {
            let h = (h - box_baseline_gap(t)).max(1.0);
            photoshop.push(("BoxBounds", nums(&[0.0, 0.0, w as f64, h as f64])))
        }
        None => photoshop.push(("PointBase", nums(&[0.0, 0.0]))),
    }
    photoshop.push((
        "Base",
        dict(vec![
            ("ShapeType", Value::Int(shape_type)),
            ("TransformPoint0", nums(&[1.0, 0.0])),
            ("TransformPoint1", nums(&[0.0, 1.0])),
            ("TransformPoint2", nums(&[0.0, 0.0])),
        ]),
    ));
    let grid_color = || {
        dict(vec![
            ("Type", Value::Int(1)),
            ("Values", nums(&[0.0, 0.0, 0.0, 1.0])),
        ])
    };
    let editor = dict(vec![
        ("Editor", dict(vec![("Text", Value::Str(full))])),
        (
            "ParagraphRun",
            dict(vec![
                (
                    "DefaultRunData",
                    dict(vec![
                        (
                            "ParagraphSheet",
                            dict(vec![
                                ("DefaultStyleSheet", Value::Int(0)),
                                ("Properties", dict(vec![])),
                            ]),
                        ),
                        ("Adjustments", adjustments()),
                    ]),
                ),
                ("RunArray", Value::Array(para_runs)),
                (
                    "RunLengthArray",
                    Value::Array(paragraphs.iter().map(|&n| Value::Int(n as i64)).collect()),
                ),
                ("IsJoinable", Value::Int(1)),
            ]),
        ),
        (
            "StyleRun",
            dict(vec![
                (
                    "DefaultRunData",
                    dict(vec![("StyleSheet", dict(vec![("StyleSheetData", dict(vec![]))]))]),
                ),
                ("RunArray", Value::Array(run_array)),
                ("RunLengthArray", Value::Array(lengths)),
                ("IsJoinable", Value::Int(2)),
            ]),
        ),
        (
            "GridInfo",
            dict(vec![
                ("GridIsOn", Value::Bool(false)),
                ("ShowGrid", Value::Bool(false)),
                ("GridSize", Value::Num(18.0)),
                ("GridLeading", Value::Num(22.0)),
                ("GridColor", grid_color()),
                ("GridLeadingFillColor", grid_color()),
                ("AlignLineHeightToGridFlags", Value::Bool(false)),
            ]),
        ),
        ("AntiAlias", Value::Int(3)),
        ("UseFractionalGlyphWidths", Value::Bool(true)),
        (
            "Rendered",
            dict(vec![
                ("Version", Value::Int(1)),
                (
                    "Shapes",
                    dict(vec![
                        ("WritingDirection", Value::Int(0)),
                        (
                            "Children",
                            Value::Array(vec![dict(vec![
                                ("ShapeType", Value::Int(shape_type)),
                                ("Procession", Value::Int(0)),
                                (
                                    "Lines",
                                    dict(vec![
                                        ("WritingDirection", Value::Int(0)),
                                        ("Children", Value::Array(vec![])),
                                    ]),
                                ),
                                ("Cookie", dict(vec![("Photoshop", dict(photoshop))])),
                            ])]),
                        ),
                    ]),
                ),
            ]),
        ),
    ]);
    let res = resources(&fonts);
    dict(vec![
        ("EngineDict", editor),
        ("ResourceDict", res.clone()),
        ("DocumentResources", res),
    ])
}

fn adjustments() -> Value {
    dict(vec![("Axis", nums(&[1.0, 0.0, 1.0])), ("XY", nums(&[0.0, 0.0]))])
}

fn enum_val(t: &[u8], v: &[u8]) -> Val {
    Val::Enum(t.to_vec(), v.to_vec())
}

/// The `TySh` block body for a text layer; `index` numbers the document's
/// text layers.
pub(super) fn tysh_block(t: &TextLayer, index: i32) -> Vec<u8> {
    let mut d = Vec::new();
    put_u16(&mut d, 1);
    // Point text anchors its first baseline at the origin; box text's box
    // starts there.
    let y = t.y + t.box_size.map_or(0.0, |_| box_baseline_gap(t));
    for v in [1.0f64, 0.0, 0.0, 1.0, t.x as f64, y as f64] {
        d.extend_from_slice(&v.to_be_bytes());
    }
    put_u16(&mut d, 50);
    let text = Desc::new(b"TxLr")
        .with(b"Txt ", Val::Text(ps_text(t)))
        .with(b"textGridding", enum_val(b"textGridding", b"None"))
        .with(b"Ornt", enum_val(b"Ornt", b"Hrzn"))
        .with(b"AntA", enum_val(b"Annt", b"AnSm"))
        .with(b"TextIndex", Val::Long(index))
        .with(b"EngineData", Val::Raw(engine_data::write(&engine_data(t))));
    d.extend_from_slice(&super::extra::descriptor_block(&text));
    put_u16(&mut d, 1);
    let warp = Desc::new(b"warp")
        .with(b"warpStyle", enum_val(b"warpStyle", b"warpNone"))
        .with(b"warpValue", Val::Doub(0.0))
        .with(b"warpPerspective", Val::Doub(0.0))
        .with(b"warpPerspectiveOther", Val::Doub(0.0))
        .with(b"warpRotate", enum_val(b"Ornt", b"Hrzn"));
    d.extend_from_slice(&super::extra::descriptor_block(&warp));
    for _ in 0..4 {
        put_u32(&mut d, 0);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(t: &TextLayer) -> (TextLayer, Vec<String>) {
        let mut w = Vec::new();
        let back = import(&tysh_block(t, 0), "T", &mut w).expect("imports as text");
        (back, w)
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-4)
    }

    #[test]
    fn point_text_round_trips_every_field() {
        let mut t = TextLayer::new("Hello\nWorld", 34.0, 69.5, 60.0, [0.5, 0.25, 0.75, 1.0]);
        t.font = "NoSuchFontXYZ-Regular".into();
        t.italic = true;
        t.align = TextAlign::Center;
        t.tracking = 50.0;
        t.line_height = 1.5;
        t.baseline_shift = 3.0;
        t.all_caps = true;
        t.kerning = true;
        t.underline = true;
        let (back, w) = round_trip(&t);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back.text, "Hello\nWorld");
        assert_eq!((back.x, back.y, back.size), (34.0, 69.5, 60.0));
        assert_eq!(back.font, "NoSuchFontXYZ-Regular");
        assert_eq!((back.bold, back.italic), (false, true));
        assert_eq!(back.align, TextAlign::Center);
        assert_eq!(back.tracking, 50.0);
        assert!((back.line_height - 1.5).abs() < 1e-6, "{}", back.line_height);
        assert_eq!(back.baseline_shift, 3.0);
        assert!(back.all_caps && back.kerning && back.underline && !back.strikethrough);
        assert!(close(back.color, t.color), "{:?}", back.color);
        assert_eq!(back.box_size, None);
        assert!(back.runs.is_empty());
    }

    #[test]
    fn paragraph_text_and_style_runs_round_trip() {
        let mut t = TextLayer::new("Big red wörds here", 10.0, 20.0, 24.0, [0.0, 0.0, 0.0, 1.0]);
        t.box_size = Some([300.0, 120.0]);
        t.align = TextAlign::Justify;
        // "Big" bold, "red" red, "wörds" 36 px struck through.
        t.apply_style(
            0,
            3,
            CharStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        t.apply_style(
            4,
            7,
            CharStyle {
                color: Some([1.0, 0.0, 0.0, 1.0]),
                ..Default::default()
            },
        );
        t.apply_style(
            8,
            14,
            CharStyle {
                size: Some(36.0),
                strikethrough: Some(true),
                ..Default::default()
            },
        );
        let (back, w) = round_trip(&t);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((back.x, back.y), (10.0, 20.0));
        assert_eq!(back.box_size, Some([300.0, 120.0]));
        assert_eq!(back.align, TextAlign::Justify);
        assert_eq!(back.runs.len(), 3, "{:?}", back.runs);
        assert_eq!((back.runs[0].start, back.runs[0].end), (0, 3));
        assert_eq!(back.runs[0].style.bold, Some(true));
        assert_eq!((back.runs[1].start, back.runs[1].end), (4, 7));
        assert!(close(back.runs[1].style.color.unwrap(), [1.0, 0.0, 0.0, 1.0]));
        // Byte offsets survive the two-byte "ö" (one UTF-16 unit).
        assert_eq!((back.runs[2].start, back.runs[2].end), (8, 14));
        assert_eq!(back.runs[2].style.size, Some(36.0));
        assert_eq!(back.runs[2].style.strikethrough, Some(true));
    }

    #[test]
    fn engine_data_carries_runs_paragraphs_and_fonts_like_photoshop() {
        let mut t = TextLayer::new("ab\ncd", 0.0, 0.0, 30.0, [1.0, 1.0, 1.0, 1.0]);
        t.apply_style(
            1,
            2,
            CharStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let e = engine_data(&t);
        assert_eq!(
            e.at(&["EngineDict", "Editor", "Text"]).and_then(Value::str),
            Some("ab\rcd\r")
        );
        let sr = e.at(&["EngineDict", "StyleRun"]).unwrap();
        // a | b | \rcd\r in UTF-16 units.
        assert_eq!(sr.get("RunLengthArray").unwrap().nums(), vec![1.0, 1.0, 4.0]);
        let pr = e.at(&["EngineDict", "ParagraphRun"]).unwrap();
        assert_eq!(pr.get("RunLengthArray").unwrap().nums(), vec![3.0, 3.0]);
        let fonts: Vec<&str> = e
            .at(&["ResourceDict", "FontSet"])
            .and_then(Value::array)
            .unwrap()
            .iter()
            .filter_map(|f| f.get("Name").and_then(Value::str))
            .collect();
        assert_eq!(fonts, vec!["AdobeInvisFont", "DejaVuSans", "DejaVuSans-Bold"]);
        let run1 = &sr.get("RunArray").unwrap().array().unwrap()[1];
        let data = run1.at(&["StyleSheet", "StyleSheetData"]).unwrap();
        assert_eq!(data.get("Font"), Some(&Value::Int(2)));
        assert_eq!(data.get("FauxBold"), Some(&Value::Bool(false)));
        assert_eq!(data.at(&["FillColor", "Values"]).unwrap().nums(), vec![1.0; 4]);
        // Smooth anti-aliasing, the same as the descriptor's `AntA` AnSm.
        assert_eq!(e.at(&["EngineDict", "AntiAlias"]), Some(&Value::Int(3)));
    }

    #[test]
    fn photoshop_engine_data_maps_onto_the_layer() {
        // Photoshop's own run layout (ag-psd's "text-simple2" values): two
        // paragraphs, centred then left, colour runs, a 2× transform.
        let ps = |c: [f64; 3]| {
            dict(vec![(
                "FillColor",
                dict(vec![
                    ("Type", Value::Int(1)),
                    ("Values", nums(&[1.0, c[0], c[1], c[2]])),
                ]),
            )])
        };
        let run = |data: Value| dict(vec![("StyleSheet", dict(vec![("StyleSheetData", data)]))]);
        let para = |j: i64| {
            dict(vec![(
                "ParagraphSheet",
                dict(vec![("Properties", dict(vec![("Justification", Value::Int(j))]))]),
            )])
        };
        let (pink, blue) = ([0.84886, 0.43897, 0.87451], [0.43899, 0.55174, 0.87448]);
        let engine = dict(vec![
            (
                "EngineDict",
                dict(vec![
                    (
                        "Editor",
                        dict(vec![("Text", Value::Str("Hello\rWorld\r".into()))]),
                    ),
                    (
                        "ParagraphRun",
                        dict(vec![
                            ("RunArray", Value::Array(vec![para(2), para(0)])),
                            ("RunLengthArray", nums(&[6.0, 6.0])),
                        ]),
                    ),
                    (
                        "StyleRun",
                        dict(vec![
                            (
                                "RunArray",
                                Value::Array(vec![run(ps(pink)), run(ps(blue)), run(ps(pink))]),
                            ),
                            ("RunLengthArray", nums(&[1.0, 1.0, 10.0])),
                        ]),
                    ),
                ]),
            ),
            (
                "ResourceDict",
                dict(vec![
                    (
                        "StyleSheetSet",
                        Value::Array(vec![dict(vec![(
                            "StyleSheetData",
                            dict(vec![("Font", Value::Int(1)), ("FontSize", Value::Num(30.0))]),
                        )])]),
                    ),
                    (
                        "FontSet",
                        Value::Array(vec![
                            dict(vec![("Name", Value::Str("AdobeInvisFont".into()))]),
                            dict(vec![("Name", Value::Str("NoSuchFontXYZ-Bold".into()))]),
                        ]),
                    ),
                ]),
            ),
        ]);
        let mut d = Vec::new();
        put_u16(&mut d, 1);
        for v in [2.0f64, 0.0, 0.0, 2.0, 43.0, 81.5] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        put_u16(&mut d, 50);
        let desc = Desc::new(b"TxLr").with(b"EngineData", Val::Raw(engine_data::write(&engine)));
        d.extend_from_slice(&super::super::extra::descriptor_block(&desc));
        let mut w = Vec::new();
        let t = import(&d, "Hello", &mut w).expect("text");
        assert_eq!(t.text, "Hello\nWorld");
        assert_eq!((t.x, t.y), (43.0, 81.5));
        // FontSize 30 under a 2× transform.
        assert_eq!(t.size, 60.0);
        assert_eq!(t.font, "NoSuchFontXYZ-Bold");
        assert!(t.bold && !t.italic);
        assert_eq!(t.align, TextAlign::Center);
        assert_eq!(t.line_height, 1.2);
        assert!(t.kerning, "AutoKerning defaults on");
        let srgb = |v: f64| srgb_to_linear_f(v as f32);
        assert_eq!(t.color, [srgb(pink[0]), srgb(pink[1]), srgb(pink[2]), 1.0]);
        assert_eq!(t.runs.len(), 1);
        assert_eq!((t.runs[0].start, t.runs[0].end), (1, 2));
        assert_eq!(
            t.runs[0].style.color,
            Some([srgb(blue[0]), srgb(blue[1]), srgb(blue[2]), 1.0])
        );
        assert_eq!(
            w,
            vec!["text layer 'Hello': simplified on import (paragraphs aligned differently)"]
        );
    }

    #[test]
    fn box_text_keeps_its_first_baseline_where_photoshop_draws_it() {
        // DejaVu Sans at 30 px: Photoshop's first baseline sits a "d"
        // (1556/2048 em = 22.79 px) below the box top, ours the ascent
        // (1901/2048 em = 27.85 px) below; the gap 5.05 px is written as
        // 162/32 = 5.0625.
        let mut t = TextLayer::new("Box", 10.0, 100.0, 30.0, [0.0, 0.0, 0.0, 1.0]);
        t.box_size = Some([200.0, 50.0]);
        assert_eq!(box_baseline_gap(&t), 5.0625);
        let block = tysh_block(&t, 0);
        let ty = f64::from_be_bytes(block[42..50].try_into().unwrap());
        assert_eq!(ty, 105.0625);
        let ours = lumenply_render::text_layout::layout(&t).lines[0].baseline;
        let photoshop = ty as f32 + 30.0 * 1556.0 / 2048.0;
        assert!((ours - photoshop).abs() < 0.05, "{ours} vs {photoshop}");
        let mut w = Vec::new();
        let back = import(&block, "B", &mut w).unwrap();
        assert_eq!((back.y, back.box_size), (100.0, Some([200.0, 50.0])));
    }

    #[test]
    fn overlap_compares_our_ink_with_photoshops_pixels() {
        let mut t = TextLayer::new("Hi", 10.0, 40.0, 30.0, [0.0, 0.0, 0.0, 1.0]);
        lumenply_render::text::refresh_cache(&mut t);
        let ours = t.cache.as_ref().unwrap().content_bounds().unwrap();
        assert_eq!(overlap_with_photoshop(&t, ours), Some(1.0));
        // Half of it shifted away: 1/3 overlap (intersection 1, union 3).
        let half = Rect::new(ours.x + ours.w as i32 / 2, ours.y, ours.w, ours.h);
        let o = overlap_with_photoshop(&t, half).unwrap();
        let w = ours.w as f32;
        let expected = ((w / 2.0).ceil() * ours.h as f32) / ((w + (w / 2.0).floor()) * ours.h as f32);
        assert!((o - expected).abs() < 1e-6, "{o} vs {expected}");
        // Far away, or no pixels from Photoshop to compare with.
        assert_eq!(overlap_with_photoshop(&t, Rect::new(500, 500, 20, 20)), Some(0.0));
        assert_eq!(overlap_with_photoshop(&t, Rect::new(0, 0, 0, 0)), None);
        // Nothing drawn by us (a box too short for its line) but pixels there.
        t.box_size = Some([100.0, 2.0]);
        lumenply_render::text::refresh_cache(&mut t);
        assert_eq!(overlap_with_photoshop(&t, ours), Some(0.0));
    }

    #[test]
    fn rotated_warped_and_broken_type_stays_pixels() {
        let t = TextLayer::new("x", 5.0, 6.0, 20.0, [0.0, 0.0, 0.0, 1.0]);
        let good = tysh_block(&t, 0);
        // Rotate 90°: xx xy yx yy = 0 1 -1 0.
        let mut rotated = good.clone();
        for (i, v) in [0.0f64, 1.0, -1.0, 0.0].iter().enumerate() {
            rotated[2 + i * 8..10 + i * 8].copy_from_slice(&v.to_be_bytes());
        }
        let mut w = Vec::new();
        assert!(import(&rotated, "R", &mut w).is_none());
        assert_eq!(
            w,
            vec!["text layer 'R' was imported as pixels (it is rotated or skewed)"]
        );
        // Stretched 2:1.
        let mut stretched = good.clone();
        stretched[2..10].copy_from_slice(&2.0f64.to_be_bytes());
        w.clear();
        assert!(import(&stretched, "S", &mut w).is_none());
        assert_eq!(w, vec!["text layer 'S' was imported as pixels (it is stretched)"]);
        // A warp style other than none.
        let at = good.windows(8).position(|s| s == b"warpNone").unwrap();
        let mut warped = good[..at].to_vec();
        warped.extend_from_slice(b"warpArc ");
        warped.extend_from_slice(&good[at + 8..]);
        w.clear();
        assert!(import(&warped, "W", &mut w).is_none());
        assert_eq!(w, vec!["text layer 'W' was imported as pixels (it is warped)"]);
        // Truncated data.
        w.clear();
        assert!(import(&good[..40], "B", &mut w).is_none());
        assert_eq!(
            w,
            vec!["text layer 'B' was imported as pixels (its type settings are not readable)"]
        );
    }
}
