//! Photoshop artboards (`artb` / `artd` / `abdd` on a group). Lumenply has
//! no artboards, so one comes in as what it looks like: a group clipped to
//! the artboard's rectangle, with its background colour as an editable
//! solid fill layer at the bottom.

use lumenply_doc::{Document, Fill, Layer, Selection};
use lumenply_tiles::Rect;

use super::extra::{color_of, parse_descriptor};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Artboard {
    pub rect: Rect,
    /// Straight linear RGB; `None` is transparent.
    pub background: Option<[f32; 3]>,
}

/// Read an artboard block's descriptor.
pub(super) fn parse(data: &[u8]) -> Option<Artboard> {
    let d = parse_descriptor(data)?;
    let r = d.obj(b"artboardRect")?;
    let (top, left) = (r.num(b"Top ")?, r.num(b"Left")?);
    let (bottom, right) = (r.num(b"Btom")?, r.num(b"Rght")?);
    if !(top.is_finite() && left.is_finite() && bottom > top && right > left) {
        return None;
    }
    let size = |a: f64, b: f64| (b - a).round().clamp(1.0, 300_000.0) as u32;
    let rect = Rect::new(
        left.round() as i32,
        top.round() as i32,
        size(left, right),
        size(top, bottom),
    );
    // Background type: 1 white, 2 black, 3 transparent, 4 the given colour.
    let background = match d.num(b"artboardBackgroundType").map(|v| v as i32) {
        Some(1) => Some([1.0; 3]),
        Some(2) => Some([0.0; 3]),
        Some(4) => d.obj(b"Clr ").map(color_of),
        _ => None,
    };
    Some(Artboard { rect, background })
}

/// Turn the group `g` into the artboard's look: clipped to its rectangle,
/// over its background colour.
pub(super) fn apply(g: &mut Layer, a: &Artboard, doc: &mut Document) {
    if g.mask.is_none() {
        g.mask = Some(Selection::rect(a.rect).to_mask());
    }
    if let Some(color) = a.background {
        let mut bg = Layer::fill(doc.alloc_id(), Fill::Solid { color });
        bg.name = "Artboard background".into();
        if let Some(children) = g.children_mut() {
            children.insert(0, bg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::extra::{descriptor_block, Desc, Val};
    use super::*;

    fn block(kind: Option<i32>, color: Option<[f64; 3]>) -> Vec<u8> {
        let rect = Desc::new(b"classFloatRect")
            .with(b"Top ", Val::Doub(10.0))
            .with(b"Left", Val::Doub(20.0))
            .with(b"Btom", Val::Doub(110.0))
            .with(b"Rght", Val::Doub(220.0));
        let mut d = Desc::new(b"artboard").with(b"artboardRect", Val::Obj(rect));
        if let Some(k) = kind {
            d = d.with(b"artboardBackgroundType", Val::Long(k));
        }
        if let Some([r, g, b]) = color {
            d = d.with(
                b"Clr ",
                Val::Obj(
                    Desc::new(b"RGBC")
                        .with(b"Rd  ", Val::Doub(r))
                        .with(b"Grn ", Val::Doub(g))
                        .with(b"Bl  ", Val::Doub(b)),
                ),
            );
        }
        descriptor_block(&d)
    }

    #[test]
    fn artboards_become_clipped_groups_over_their_background() {
        let a = parse(&block(Some(4), Some([255.0, 0.0, 0.0]))).unwrap();
        assert_eq!(a.rect, Rect::new(20, 10, 200, 100));
        assert_eq!(a.background, Some([1.0, 0.0, 0.0]));
        assert_eq!(parse(&block(Some(1), None)).unwrap().background, Some([1.0; 3]));
        assert_eq!(parse(&block(Some(3), None)).unwrap().background, None);
        assert_eq!(parse(&block(None, None)).unwrap().background, None);

        let mut doc = Document::new(300, 200);
        let id = doc.alloc_id();
        let mut g = Layer::group(id, "Artboard 1");
        apply(&mut g, &a, &mut doc);
        let m = g.mask.as_ref().unwrap();
        assert_eq!(
            (
                m.value(20, 10),
                m.value(219, 109),
                m.value(19, 10),
                m.value(220, 50)
            ),
            (1.0, 1.0, 0.0, 0.0)
        );
        let kids = g.children().unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "Artboard background");
    }
}
