//! Shape layers in PSD: a fill layer (`SoCo` / `GdFl`) clipped by a
//! vector mask (`vmsk`) and outlined by a vector stroke (`vstk`), which
//! is how Photoshop stores its own shape layers. The rendered pixels go
//! alongside, so readers without vector support still see the shape.
//!
//! Vector mask records are 26 bytes: a selector, then for a knot three
//! points (the handle before, the anchor, the handle after), each as
//! (y, x) in signed 8.24 fixed point relative to the canvas height and
//! width. Subpaths after the first are written with operation −1
//! ("merged component"), which fills them together by the even-odd rule,
//! as Lumenply fills them.

use lumenply_doc::shape::{ShapeGeometry, ShapeLayer, ShapeStroke, StrokeAlign};
use lumenply_doc::{Fill, PathNode, SubPath, VectorPath};

use super::extra::{color_of, descriptor_block, parse_descriptor, rgbc, Desc, Val};
use super::{put_u16, put_u32, Rd};

const FIXED: f64 = (1u32 << 24) as f64;

fn put_i32(d: &mut Vec<u8>, v: i32) {
    d.extend_from_slice(&v.to_be_bytes());
}

/// A coordinate as 8.24 fixed point of `dim`.
fn fixed(v: f32, dim: u32) -> i32 {
    let f = (v as f64 / dim.max(1) as f64 * FIXED).round();
    f.clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

fn unfixed(v: i32, dim: u32) -> f32 {
    (v as f64 / FIXED * dim as f64) as f32
}

/// The `vmsk` body for an outline on a `w × h` canvas.
pub(super) fn vector_mask_block(path: &VectorPath, w: u32, h: u32) -> Vec<u8> {
    let mut d = Vec::new();
    put_u32(&mut d, 3); // version
    put_u32(&mut d, 0); // flags: not inverted, linked, enabled
                        // Path fill rule record, then the initial fill rule: start empty.
    put_u16(&mut d, 6);
    d.extend_from_slice(&[0; 24]);
    put_u16(&mut d, 8);
    put_u16(&mut d, 0);
    d.extend_from_slice(&[0; 22]);
    let mut first = true;
    for sp in path.subpaths.iter().filter(|sp| !sp.nodes.is_empty()) {
        put_u16(&mut d, if sp.closed { 0 } else { 3 });
        put_u16(&mut d, sp.nodes.len().min(u16::MAX as usize) as u16);
        // Operation: union for the first component, then merged with it.
        d.extend_from_slice(&(if first { 1i16 } else { -1i16 }).to_be_bytes());
        put_u16(&mut d, 1);
        put_u32(&mut d, 0);
        put_u32(&mut d, 0);
        d.extend_from_slice(&[0; 10]);
        first = false;
        for n in sp.nodes.iter().take(u16::MAX as usize) {
            put_u16(&mut d, if sp.closed { 2 } else { 5 }); // unlinked knots
            for p in [n.handle_in, n.point, n.handle_out] {
                put_i32(&mut d, fixed(p.1, h));
                put_i32(&mut d, fixed(p.0, w));
            }
        }
    }
    d
}

/// Read a `vmsk` / `vsms` body. `None` when it is malformed, inverted or
/// disabled (such a mask can't be expressed as a shape outline).
pub(super) fn parse_vector_mask(data: &[u8], w: u32, h: u32) -> Option<VectorPath> {
    let mut d = Rd::new(data);
    if d.u32().ok()? != 3 {
        return None;
    }
    let flags = d.u32().ok()?;
    if flags & 0b101 != 0 {
        return None;
    }
    let mut path = VectorPath::default();
    while d.pos + 26 <= data.len() {
        let sel = d.u16().ok()?;
        match sel {
            0 | 3 => {
                path.subpaths.push(SubPath {
                    nodes: Vec::new(),
                    closed: sel == 0,
                });
                d.skip(24).ok()?;
            }
            1 | 2 | 4 | 5 => {
                let mut pts = [(0.0f32, 0.0f32); 3];
                for p in &mut pts {
                    let y = d.i32().ok()?;
                    let x = d.i32().ok()?;
                    *p = (unfixed(x, w), unfixed(y, h));
                }
                let sp = path.subpaths.last_mut()?;
                if sp.nodes.len() < 100_000 {
                    sp.nodes.push(PathNode {
                        handle_in: pts[0],
                        point: pts[1],
                        handle_out: pts[2],
                    });
                }
            }
            _ => d.skip(24).ok()?,
        }
    }
    path.subpaths.retain(|sp| !sp.nodes.is_empty());
    (!path.subpaths.is_empty()).then_some(path)
}

fn align_key(a: StrokeAlign) -> &'static [u8] {
    match a {
        StrokeAlign::Inside => b"strokeStyleAlignInside",
        StrokeAlign::Center => b"strokeStyleAlignCenter",
        StrokeAlign::Outside => b"strokeStyleAlignOutside",
    }
}

/// The `vstk` body: the stroke (or a disabled one) and whether the fill
/// is on. Lumenply's distance-field strokes have round joins outside the
/// outline and round dash caps; an inside stroke keeps the corners sharp.
pub(super) fn stroke_block(stroke: Option<&ShapeStroke>, fill_enabled: bool) -> Vec<u8> {
    let s = stroke.copied().unwrap_or_default();
    let e = |ty: &[u8], v: &[u8]| Val::Enum(ty.to_vec(), v.to_vec());
    let cap: &[u8] = if s.dash.is_some() {
        b"strokeStyleRoundCap"
    } else {
        b"strokeStyleButtCap"
    };
    let join: &[u8] = if s.align == StrokeAlign::Inside {
        b"strokeStyleMiterJoin"
    } else {
        b"strokeStyleRoundJoin"
    };
    let dashes = s
        .dash
        .map(|[on, off]| vec![Val::Unit(*b"#Nne", on as f64), Val::Unit(*b"#Nne", off as f64)])
        .unwrap_or_default();
    let desc = Desc::new(b"strokeStyle")
        .with(b"strokeStyleVersion", Val::Long(2))
        .with(b"strokeEnabled", Val::Bool(stroke.is_some()))
        .with(b"fillEnabled", Val::Bool(fill_enabled))
        .with(
            b"strokeStyleLineWidth",
            Val::Unit(*b"#Pxl", s.sane_width() as f64),
        )
        .with(b"strokeStyleLineDashOffset", Val::Unit(*b"#Pnt", 0.0))
        .with(b"strokeStyleMiterLimit", Val::Doub(100.0))
        .with(b"strokeStyleLineCapType", e(b"strokeStyleLineCapType", cap))
        .with(b"strokeStyleLineJoinType", e(b"strokeStyleLineJoinType", join))
        .with(
            b"strokeStyleLineAlignment",
            e(b"strokeStyleLineAlignment", align_key(s.align)),
        )
        .with(b"strokeStyleScaleLock", Val::Bool(false))
        .with(b"strokeStyleStrokeAdjust", Val::Bool(false))
        .with(b"strokeStyleLineDashSet", Val::List(dashes))
        .with(b"strokeStyleBlendMode", e(b"BlnM", b"Nrml"))
        .with(b"strokeStyleOpacity", Val::Unit(*b"#Prc", 100.0))
        .with(
            b"strokeStyleContent",
            Val::Obj(Desc::new(b"solidColorLayer").with(b"Clr ", rgbc(s.color))),
        )
        .with(b"strokeStyleResolution", Val::Doub(72.0));
    descriptor_block(&desc)
}

/// Read a `vstk` body: the stroke when enabled, and whether the fill is.
pub(super) fn parse_stroke(data: &[u8]) -> Option<(Option<ShapeStroke>, bool)> {
    let desc = parse_descriptor(data)?;
    let flag = |k: &[u8], default: bool| match desc.get(k) {
        Some(Val::Bool(b)) => *b,
        _ => default,
    };
    let fill_enabled = flag(b"fillEnabled", true);
    if !flag(b"strokeEnabled", false) {
        return Some((None, fill_enabled));
    }
    let align = match desc.get(b"strokeStyleLineAlignment") {
        Some(Val::Enum(_, v)) if v == b"strokeStyleAlignCenter" => StrokeAlign::Center,
        Some(Val::Enum(_, v)) if v == b"strokeStyleAlignOutside" => StrokeAlign::Outside,
        _ => StrokeAlign::Inside,
    };
    let dash = match desc.get(b"strokeStyleLineDashSet") {
        Some(Val::List(v)) if v.len() >= 2 => {
            let n = |x: &Val| match x {
                Val::Unit(_, f) | Val::Doub(f) => Some(*f as f32),
                Val::Long(l) => Some(*l as f32),
                _ => None,
            };
            Some([n(&v[0])?, n(&v[1])?])
        }
        _ => None,
    };
    let color = desc
        .obj(b"strokeStyleContent")
        .and_then(|c| c.obj(b"Clr "))
        .map_or([0.0; 3], color_of);
    Some((
        Some(ShapeStroke {
            color,
            width: desc.num(b"strokeStyleLineWidth").unwrap_or(1.0) as f32,
            align,
            dash,
        }),
        fill_enabled,
    ))
}

/// A shape layer from its parsed fill and its raw `vmsk` and `vstk`
/// blocks, or `None` when the outline is unreadable.
pub(super) fn parse_shape(
    fill: Fill,
    vmsk: &[u8],
    vstk: Option<&[u8]>,
    w: u32,
    h: u32,
) -> Option<ShapeLayer> {
    let path = parse_vector_mask(vmsk, w, h)?;
    let (stroke, fill_on) = vstk.and_then(parse_stroke).unwrap_or((None, true));
    let shape = ShapeLayer::new(ShapeGeometry::Path { path }, fill_on.then_some(fill), stroke);
    shape.geometry.is_finite().then_some(shape)
}

/// Set a layer record's "pixel data irrelevant" flag (bit 4, with bit 3
/// saying bit 4 is meaningful), as Photoshop does for shape layers: the
/// pixels are a preview, the vectors are the layer.
pub(super) fn mark_pixel_data_irrelevant(record: &mut [u8]) {
    let Some(n) = record
        .get(16..18)
        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    else {
        return;
    };
    // Bounds, channel count, 6 bytes per channel, "8BIM", blend key,
    // opacity, clipping, then the flags.
    if let Some(flags) = record.get_mut(18 + 6 * n + 10) {
        *flags |= 0x18;
    }
}

/// The blocks a shape layer carries: its fill settings (a disabled fill
/// still needs a colour), the stroke, and the outline.
pub(super) fn shape_blocks(s: &ShapeLayer, w: u32, h: u32) -> Vec<(&'static [u8; 4], Vec<u8>)> {
    let fill = s.fill.clone().unwrap_or(Fill::Solid {
        color: s.stroke.map_or([0.0; 3], |st| st.color),
    });
    let (key, data) = super::extra::fill_block(&fill);
    vec![
        (key, data),
        (b"vstk", stroke_block(s.stroke.as_ref(), s.fill.is_some())),
        (b"vmsk", vector_mask_block(&s.outline(), w, h)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::shape::CustomShape;

    #[test]
    fn vector_masks_round_trip_in_fixed_point() {
        let s = ShapeLayer::new(
            ShapeGeometry::Custom {
                rect: [10.0, 20.0, 64.0, 32.0],
                shape: CustomShape::Heart,
            },
            None,
            None,
        );
        let path = s.outline();
        let block = vector_mask_block(&path, 128, 64);
        // Header, two fill-rule records, one length record, six knots.
        assert_eq!(block.len(), 8 + 26 * (2 + 1 + 6));
        assert_eq!(&block[..8], &[0, 0, 0, 3, 0, 0, 0, 0]);
        // The first knot's anchor: the heart's notch at (42, 28.96) → x
        // 42/128 and y 28.96/64 of 2^24.
        let knot = &block[8 + 26 * 3..];
        assert_eq!(&knot[..2], &[0, 2], "closed, unlinked");
        let y = i32::from_be_bytes(knot[10..14].try_into().unwrap());
        let x = i32::from_be_bytes(knot[14..18].try_into().unwrap());
        assert_eq!(x, (42.0 / 128.0 * FIXED).round() as i32);
        assert_eq!(y, (28.96f64 / 64.0 * FIXED).round() as i32);
        let back = parse_vector_mask(&block, 128, 64).unwrap();
        assert_eq!(back.subpaths.len(), 1);
        assert!(back.subpaths[0].closed);
        for (a, b) in back.subpaths[0].nodes.iter().zip(&path.subpaths[0].nodes) {
            for (p, q) in [
                (a.point, b.point),
                (a.handle_in, b.handle_in),
                (a.handle_out, b.handle_out),
            ] {
                assert!(
                    (p.0 - q.0).abs() < 1e-4 && (p.1 - q.1).abs() < 1e-4,
                    "{p:?} {q:?}"
                );
            }
        }
        // Inverted masks are not shapes.
        let mut inv = block.clone();
        inv[7] = 1;
        assert!(parse_vector_mask(&inv, 128, 64).is_none());
    }

    #[test]
    fn strokes_round_trip_through_vstk() {
        let st = ShapeStroke {
            color: [1.0, 0.0, 0.0],
            width: 7.5,
            align: StrokeAlign::Outside,
            dash: Some([3.0, 1.5]),
        };
        let (back, fill_on) = parse_stroke(&stroke_block(Some(&st), false)).unwrap();
        assert!(!fill_on);
        let back = back.unwrap();
        assert_eq!(
            (back.width, back.align, back.dash),
            (7.5, StrokeAlign::Outside, Some([3.0, 1.5]))
        );
        assert!((back.color[0] - 1.0).abs() < 1e-3 && back.color[1] < 1e-3);
        // A disabled stroke reads as none, the fill flag survives.
        assert_eq!(parse_stroke(&stroke_block(None, true)), Some((None, true)));
    }
}
