//! Guides in PSD files: image resource 1032 ("grid and guides info").
//!
//! Layout (big-endian): version u32 = 1, the grid cycle as two u32s
//! (Photoshop writes 576 = a quarter inch at 72 dpi in 1/32 units), the
//! guide count u32, then per guide a 4-byte location in 1/32 pixel and a
//! direction byte (0 = vertical, 1 = horizontal). The resource sits in the
//! image-resources section as an `8BIM` block with an empty Pascal name.

use lumenply_doc::{Document, Guide};

/// Resource id of the grid-and-guides block.
const GRID_AND_GUIDES: u16 = 1032;

/// The whole image-resources section (its u32 length included) for `doc`:
/// just the length 0 when there are no guides.
pub(crate) fn image_resources(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    if doc.guides.is_empty() {
        out.extend_from_slice(&0u32.to_be_bytes());
        return out;
    }
    let mut data = Vec::new();
    for v in [1u32, 576, 576, doc.guides.len() as u32] {
        data.extend_from_slice(&v.to_be_bytes());
    }
    for g in &doc.guides {
        let loc = (g.pos * 32.0).round().clamp(i32::MIN as f32, i32::MAX as f32) as i32;
        data.extend_from_slice(&loc.to_be_bytes());
        data.push(if g.is_vertical() { 0 } else { 1 });
    }
    let mut block = Vec::new();
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(&GRID_AND_GUIDES.to_be_bytes());
    block.extend_from_slice(&[0, 0]); // empty Pascal name, padded to even
    block.extend_from_slice(&(data.len() as u32).to_be_bytes());
    block.extend_from_slice(&data);
    if data.len() % 2 == 1 {
        block.push(0);
    }
    out.extend_from_slice(&(block.len() as u32).to_be_bytes());
    out.extend_from_slice(&block);
    out
}

/// The guides in an image-resources section body (without its length).
/// Malformed or truncated blocks end the scan; nothing here can fail a load.
pub(crate) fn read_guides(res: &[u8]) -> Vec<Guide> {
    let mut guides = Vec::new();
    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize| -> Option<&[u8]> {
        let s = res.get(*pos..pos.checked_add(n)?)?;
        *pos += n;
        Some(s)
    };
    loop {
        let Some(_sig) = take(&mut pos, 4) else { break };
        let Some(id) = take(&mut pos, 2).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
            break;
        };
        let Some(name_len) = take(&mut pos, 1).map(|b| b[0] as usize) else {
            break;
        };
        // The Pascal string (length byte included) is padded to even.
        let name_total = (1 + name_len).next_multiple_of(2) - 1;
        if take(&mut pos, name_total).is_none() {
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
        if id != GRID_AND_GUIDES || data.len() < 16 {
            continue;
        }
        let count = u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize;
        for g in data[16..].chunks_exact(5).take(count) {
            let loc = i32::from_be_bytes([g[0], g[1], g[2], g[3]]);
            let pos = loc as f32 / 32.0;
            guides.push(if g[4] == 0 {
                Guide::vertical(pos)
            } else {
                Guide::horizontal(pos)
            });
        }
    }
    guides
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_encode_as_resource_1032() {
        let mut doc = Document::new(10, 10);
        doc.guides = vec![Guide::vertical(12.5), Guide::horizontal(-3.0)];
        let bytes = image_resources(&doc);
        // Section length, then 8BIM, id 1032, empty name, size 26.
        assert_eq!(&bytes[0..4], &38u32.to_be_bytes());
        assert_eq!(&bytes[4..8], b"8BIM");
        assert_eq!(&bytes[8..10], &1032u16.to_be_bytes());
        assert_eq!(&bytes[10..12], &[0, 0]);
        assert_eq!(&bytes[12..16], &26u32.to_be_bytes());
        // 12.5 px = 400 / 32, vertical; −3 px = −96 / 32, horizontal.
        assert_eq!(&bytes[32..37], &[0, 0, 1, 144, 0]);
        assert_eq!(&bytes[37..42], &[0xFF, 0xFF, 0xFF, 0xA0, 1]);
        assert_eq!(bytes.len(), 4 + 38);
        assert_eq!(read_guides(&bytes[4..]), doc.guides);
        assert_eq!(image_resources(&Document::new(1, 1)), 0u32.to_be_bytes());
    }

    #[test]
    fn other_resources_and_junk_are_skipped() {
        let mut res = Vec::new();
        // A resource with a 3-letter name (4 bytes with its length byte)
        // and odd-sized data, before the guides.
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&1005u16.to_be_bytes());
        res.extend_from_slice(&[3, b'a', b'b', b'c']);
        res.extend_from_slice(&3u32.to_be_bytes());
        res.extend_from_slice(&[1, 2, 3, 0]);
        let mut doc = Document::new(1, 1);
        doc.guides = vec![Guide::horizontal(7.0)];
        res.extend_from_slice(&image_resources(&doc)[4..]);
        assert_eq!(read_guides(&res), vec![Guide::horizontal(7.0)]);
        // Truncated input yields what was read, never a panic.
        for n in 0..res.len() {
            let _ = read_guides(&res[..n]);
        }
        // A count larger than the data is capped by the data.
        let mut bad = image_resources(&doc)[4..].to_vec();
        bad[12 + 15] = 200;
        assert_eq!(read_guides(&bad).len(), 1);
    }

    #[test]
    fn guides_round_trip_through_a_psd_file() {
        let mut doc = Document::new(64, 48);
        let id = doc.add_pixel_layer("p");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(3, 3, lumenply_tiles::Rgba::WHITE);
        doc.guides = vec![
            Guide::vertical(16.0),
            Guide::horizontal(24.25),
            Guide::vertical(63.0),
        ];
        let dir = std::env::temp_dir().join("lumenply-psd-guides");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("guides.psd");
        crate::psd::save(&path, &doc).unwrap();
        let back = crate::psd::load(&path).unwrap().value;
        assert_eq!(back.guides, doc.guides);
        assert_eq!(back.layer_count(), 1, "the layers still load after the resource");
        // 16-bit files carry them too.
        let path16 = dir.join("guides16.psd");
        crate::psd::save_16(&path16, &doc).unwrap();
        assert_eq!(crate::psd::load(&path16).unwrap().value.guides, doc.guides);
    }
}
