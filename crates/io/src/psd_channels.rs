//! PSD alpha channels for saved selections: the composite image carries
//! one extra plane per saved selection after RGB and transparency, named
//! by image resources 1006 (Pascal) and 1045 (Unicode).

use lumenply_doc::{Document, Mask, SavedSelection};
use lumenply_tiles::{Rect, TileStore};

const ALPHA_NAMES_PASCAL: u16 = 1006;
const ALPHA_NAMES_UNICODE: u16 = 1045;

fn resource_block(id: u16, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"8BIM");
    b.extend_from_slice(&id.to_be_bytes());
    b.extend_from_slice(&[0, 0]); // empty Pascal name, padded to even
    b.extend_from_slice(&(data.len() as u32).to_be_bytes());
    b.extend_from_slice(data);
    if data.len() % 2 == 1 {
        b.push(0);
    }
    b
}

/// The resource blocks naming `doc`'s saved selections (empty when none).
pub(crate) fn name_blocks(doc: &Document) -> Vec<u8> {
    if doc.saved_selections.is_empty() {
        return Vec::new();
    }
    let mut pascal = Vec::new();
    let mut unicode = Vec::new();
    for s in &doc.saved_selections {
        let bytes: Vec<u8> = s
            .name
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .take(255)
            .collect();
        pascal.push(bytes.len() as u8);
        pascal.extend_from_slice(&bytes);
        let utf16: Vec<u16> = s.name.encode_utf16().collect();
        unicode.extend_from_slice(&(utf16.len() as u32).to_be_bytes());
        for u in utf16 {
            unicode.extend_from_slice(&u.to_be_bytes());
        }
    }
    let mut out = resource_block(ALPHA_NAMES_PASCAL, &pascal);
    out.extend_from_slice(&resource_block(ALPHA_NAMES_UNICODE, &unicode));
    out
}

/// The image resources section: `guides` (a section as psd_guides writes
/// it, length first) merged with the alpha-channel names.
pub(crate) fn resources_section(guides: &[u8], doc: &Document) -> Vec<u8> {
    let mut blocks = guides.get(4..).unwrap_or(&[]).to_vec();
    blocks.extend_from_slice(&name_blocks(doc));
    blocks.extend_from_slice(&crate::resolution::psd_resource_block(doc.resolution));
    let mut out = (blocks.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&blocks);
    out
}

/// Alpha-channel names from an image resources section's contents
/// (Unicode when present, else Pascal).
pub(crate) fn read_names(res: &[u8]) -> Vec<String> {
    let (mut pascal, mut unicode) = (None, None);
    let mut at = 0usize;
    while at + 12 <= res.len() && &res[at..at + 4] == b"8BIM" {
        let id = u16::from_be_bytes([res[at + 4], res[at + 5]]);
        // Pascal name: length byte + bytes, padded to an even total.
        let name_total = 1 + res[at + 6] as usize;
        let p = at + 6 + name_total + (name_total % 2);
        if p + 4 > res.len() {
            break;
        }
        let size = u32::from_be_bytes([res[p], res[p + 1], res[p + 2], res[p + 3]]) as usize;
        let data_start = p + 4;
        let Some(data) = res.get(data_start..data_start + size) else {
            break;
        };
        match id {
            ALPHA_NAMES_PASCAL => pascal = Some(data.to_vec()),
            ALPHA_NAMES_UNICODE => unicode = Some(data.to_vec()),
            _ => {}
        }
        at = data_start + size + (size % 2);
    }
    if let Some(u) = unicode {
        let mut names = Vec::new();
        let mut i = 0;
        while i + 4 <= u.len() {
            let n = u32::from_be_bytes([u[i], u[i + 1], u[i + 2], u[i + 3]]) as usize;
            i += 4;
            let Some(chars) = u.get(i..i + 2 * n) else { break };
            let units: Vec<u16> = chars
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            // Some writers include a trailing NUL.
            names.push(
                String::from_utf16_lossy(&units)
                    .trim_end_matches('\0')
                    .to_string(),
            );
            i += 2 * n;
        }
        return names;
    }
    let mut names = Vec::new();
    if let Some(p) = pascal {
        let mut i = 0;
        while i < p.len() {
            let n = p[i] as usize;
            let Some(b) = p.get(i + 1..i + 1 + n) else { break };
            names.push(String::from_utf8_lossy(b).into_owned());
            i += 1 + n;
        }
    }
    names
}

/// A saved selection's coverage over the canvas, one value per pixel.
pub(crate) fn plane(sel: &SavedSelection, canvas: Rect) -> Vec<f32> {
    let mut out = Vec::with_capacity((canvas.w * canvas.h) as usize);
    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            out.push(sel.mask.value(x, y));
        }
    }
    out
}

/// A saved selection from a decoded plane of full-range samples.
pub(crate) fn from_plane(name: String, samples: &[u16], w: u32, h: u32) -> SavedSelection {
    let mut r = lumenply_tiles::Raster::new(w, h);
    for (p, &s) in r.pixels.iter_mut().zip(samples) {
        let v = s as f32 / 65535.0;
        *p = lumenply_tiles::Rgba::new(v, v, v, v);
    }
    let mut tiles = TileStore::from_raster(&r, 0, 0);
    tiles.prune_blank();
    SavedSelection {
        name,
        mask: Mask {
            tiles,
            default: 0.0,
            enabled: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::Selection;

    #[test]
    fn saved_selections_round_trip_as_named_alpha_channels() {
        let mut doc = Document::new(64, 48);
        let id = doc.add_pixel_layer("photo");
        doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
            3,
            3,
            lumenply_tiles::Rgba::new(1.0, 0.0, 0.0, 1.0),
        );
        let mut soft = Selection::rect(Rect::new(10, 8, 30, 20));
        soft.feather(3.0);
        doc.saved_selections = vec![
            SavedSelection {
                name: "Sky".into(),
                mask: Selection::rect(Rect::new(0, 0, 64, 20)).to_mask(),
            },
            SavedSelection {
                name: "Céline's edge".into(),
                mask: soft.to_mask(),
            },
        ];
        for deep in [false, true] {
            let path = std::env::temp_dir().join(format!("lumenply-alpha-{}-{deep}.psd", std::process::id()));
            if deep {
                crate::psd::save_16(&path, &doc).unwrap();
            } else {
                crate::psd::save(&path, &doc).unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            // For checking with an independent reader (psd-tools).
            if let Ok(dir) = std::env::var("LUMENPLY_KEEP_PSD") {
                let _ = std::fs::write(format!("{dir}/alpha-{deep}.psd"), &bytes);
            }
            assert_eq!(
                u16::from_be_bytes([bytes[12], bytes[13]]),
                6,
                "RGB + transparency + 2"
            );
            let back = crate::psd::load(&path).unwrap().value;
            let _ = std::fs::remove_file(&path);
            let names: Vec<_> = back.saved_selections.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["Sky", "Céline's edge"]);
            assert_eq!(back.saved_selections[0].mask.value(5, 5), 1.0);
            assert_eq!(back.saved_selections[0].mask.value(5, 30), 0.0);
            for (x, y) in [(10, 8), (25, 18), (39, 27), (5, 5)] {
                let want = doc.saved_selections[1].mask.value(x, y);
                let got = back.saved_selections[1].mask.value(x, y);
                let tol = if deep { 1e-4 } else { 3e-3 };
                assert!((got - want).abs() < tol, "({x}, {y}): {got} vs {want}");
            }
            // The layers came through as layers.
            assert_eq!(back.layers().len(), 1);
        }
    }

    #[test]
    fn layered_files_mark_their_fourth_channel_as_transparency() {
        let mut doc = Document::new(8, 8);
        doc.add_pixel_layer("a");
        let path = std::env::temp_dir().join(format!("lumenply-count-{}.psd", std::process::id()));
        crate::psd::save(&path, &doc).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // header 26 + colour mode 4 + resources (4 + their length, the
        // resolution block at least) + layer&mask len 4 + layer info len 4,
        // then the signed layer count.
        let res_len = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
        let at = 26 + 4 + 4 + res_len + 4 + 4;
        assert_eq!(i16::from_be_bytes([bytes[at], bytes[at + 1]]), -1);
    }
}
