//! Photoshop's Color Lookup adjustment (`clrL`): a version-1 header and a
//! version-16 action descriptor.
//!
//! A "3DLUT" lookup embeds the LUT file itself — `LUT3DFileData` holds the
//! raw `.cube` / `.3dl` bytes, `LUT3DFileName` the path it was loaded from,
//! `LUTFormat` its format — beside `profile`, an ICC device link Photoshop
//! derives from it. We read the file bytes (they are the source of truth)
//! and write the table back as `.cube` text; abstract-profile and
//! device-link lookups carry only an ICC profile, which we don't evaluate,
//! so they import as an empty lookup with a warning. A fresh Color Lookup
//! with nothing chosen is an empty descriptor: the identity.

use lumenply_doc::{Adjustment, Lut3D};
use std::sync::Arc;

use super::extra::{descriptor_block, parse_descriptor, Desc, Val};
use crate::lut_files;

fn text<'a>(desc: &'a Desc, key: &[u8]) -> Option<&'a str> {
    match desc.get(key)? {
        Val::Text(s) => Some(s.trim_end_matches('\0')),
        _ => None,
    }
}

fn enum_value<'a>(desc: &'a Desc, key: &[u8]) -> Option<&'a [u8]> {
    match desc.get(key)? {
        Val::Enum(_, v) => Some(v),
        _ => None,
    }
}

/// The file name at the end of a Windows or POSIX path.
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// What the layer is called in the UI: the file name without extension.
fn display_name(path: &str) -> String {
    let f = file_name(path);
    match f.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => f.to_string(),
    }
}

/// Decode a `clrL` block. Never fails: an unreadable or unsupported
/// lookup becomes the identity, with the reason as a warning.
pub(super) fn parse(data: &[u8]) -> (Adjustment, Option<String>) {
    let empty = |name: String, why: String| {
        (
            Adjustment::ColorLookup {
                lut: Arc::new(Lut3D::identity(2)),
                name,
            },
            Some(why),
        )
    };
    let Some(desc) = data.get(2..).and_then(parse_descriptor) else {
        return empty(
            String::new(),
            "settings not readable; imported as an empty lookup".into(),
        );
    };
    let file = text(&desc, b"LUT3DFileName")
        .or_else(|| text(&desc, b"Nm  "))
        .unwrap_or("");
    let name = display_name(file);
    let kind = enum_value(&desc, b"lookupType");
    if kind.is_none() && desc.get(b"LUT3DFileData").is_none() {
        // Nothing chosen yet: Photoshop renders it as no change too.
        return (Adjustment::color_lookup_default(), None);
    }
    if kind.is_some_and(|k| k != b"3DLUT") {
        return empty(
            name,
            "abstract-profile and device-link lookups are not supported; imported as an empty lookup".into(),
        );
    }
    let Some(Val::Raw(bytes)) = desc.get(b"LUT3DFileData") else {
        return empty(
            name,
            "the LUT file is not embedded; imported as an empty lookup".into(),
        );
    };
    // The format enum wins over the file name, which may be missing.
    let format_name = match enum_value(&desc, b"LUTFormat") {
        Some(b"LUTFormatCUBE") => "x.cube",
        Some(b"LUTFormat3DL") => "x.3dl",
        Some(b"LUTFormatLOOK") => {
            return empty(
                name,
                "the .look LUT format is not supported; imported as an empty lookup".into(),
            )
        }
        _ => file_name(file),
    };
    match lut_files::parse_lut(format_name, bytes) {
        Ok(mut lut) => {
            if lut.title.is_empty() {
                lut.title = name.clone();
            }
            (
                Adjustment::ColorLookup {
                    lut: Arc::new(lut),
                    name,
                },
                None,
            )
        }
        Err(e) => empty(
            name,
            format!("the embedded LUT is unreadable ({e}); imported as an empty lookup"),
        ),
    }
}

/// The `clrL` block body for a table: Photoshop's descriptor layout, the
/// table embedded as `.cube` text. A pure 1D or 3D table is written as is;
/// a shaper plus a cube is baked into one 65³ cube first (Adobe's `.cube`
/// holds one table). An identity lookup with no name writes the empty
/// descriptor of a fresh, unchosen Color Lookup.
pub(super) fn block_body(lut: &Lut3D, name: &str) -> Vec<u8> {
    let mut d = Vec::new();
    super::put_u16(&mut d, 1);
    if name.is_empty() && lut.is_identity() {
        d.extend(descriptor_block(&Desc::new(b"null")));
        return d;
    }
    let only_1d = lut.shaper.is_some()
        && lut.size == 2
        && lut.domain_min == [0.0; 3]
        && lut.domain_max == [1.0; 3]
        && {
            let id = Lut3D::identity(2);
            id.data == lut.data
        };
    let cube = if lut.shaper.is_some() && !only_1d {
        lut_files::write_cube(&lut.baked(65))
    } else {
        lut_files::write_cube(lut)
    };
    let base = if name.is_empty() { "Lookup" } else { name };
    let file = format!("{base}.cube");
    let order = |v: &[u8]| Val::Enum(b"colorLookupOrder".to_vec(), v.to_vec());
    let desc = Desc::new(b"null")
        .with(
            b"lookupType",
            Val::Enum(b"colorLookupType".to_vec(), b"3DLUT".to_vec()),
        )
        .with(b"Nm  ", Val::Text(file.clone()))
        .with(b"Dthr", Val::Bool(true))
        .with(
            b"LUTFormat",
            Val::Enum(b"LUTFormatType".to_vec(), b"LUTFormatCUBE".to_vec()),
        )
        .with(b"dataOrder", order(b"rgbOrder"))
        .with(b"tableOrder", order(b"bgrOrder"))
        .with(b"LUT3DFileData", Val::Raw(cube.into_bytes()))
        .with(b"LUT3DFileName", Val::Text(file));
    d.extend(descriptor_block(&desc));
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(lut: Lut3D, name: &str) -> Adjustment {
        Adjustment::ColorLookup {
            lut: Arc::new(lut),
            name: name.into(),
        }
    }

    #[test]
    fn a_table_survives_the_block_exactly() {
        let lut = Lut3D::from_fn(5, |[r, g, b]| [b, g * 0.9, r]).with_title("Swap");
        let body = block_body(&lut, "Swap");
        let (adj, warn) = parse(&body);
        assert_eq!(warn, None);
        assert_eq!(adj, lookup(lut, "Swap"));
    }

    #[test]
    fn photoshops_empty_lookup_is_the_identity_and_writes_back_empty() {
        // The 24-byte block of a fresh Color Lookup in Photoshop files.
        let ps = b"\x00\x01\x00\x00\x00\x10\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00null\x00\x00\x00\x00";
        let (adj, warn) = parse(ps);
        assert_eq!(warn, None);
        assert_eq!(adj, Adjustment::color_lookup_default());
        assert_eq!(block_body(&Lut3D::identity(2), ""), ps.to_vec());
    }

    #[test]
    fn names_come_from_windows_paths_and_profiles_warn() {
        let lut = Lut3D::from_fn(2, |[r, g, b]| [g, b, r]);
        let mut desc = Desc::new(b"null")
            .with(
                b"lookupType",
                Val::Enum(b"colorLookupType".to_vec(), b"3DLUT".to_vec()),
            )
            .with(
                b"LUTFormat",
                Val::Enum(b"LUTFormatType".to_vec(), b"LUTFormatCUBE".to_vec()),
            )
            .with(
                b"LUT3DFileData",
                Val::Raw(lut_files::write_cube(&lut).into_bytes()),
            )
            .with(
                b"LUT3DFileName",
                Val::Text("C:\\Presets\\3DLUTs\\Candlelight.CUBE\0".into()),
            );
        let mut body = vec![0, 1];
        body.extend(descriptor_block(&desc));
        let (adj, warn) = parse(&body);
        assert_eq!(warn, None);
        let Adjustment::ColorLookup { lut: got, name } = adj else {
            panic!()
        };
        assert_eq!(name, "Candlelight");
        assert_eq!(got.data, lut.data);
        // An abstract-profile lookup keeps the layer as an empty lookup.
        desc.items[0].1 = Val::Enum(b"colorLookupType".to_vec(), b"abstractProfile".to_vec());
        let mut body = vec![0, 1];
        body.extend(descriptor_block(&desc));
        let (adj, warn) = parse(&body);
        assert!(warn.unwrap().contains("not supported"));
        assert!(matches!(adj, Adjustment::ColorLookup { lut, .. } if lut.is_identity()));
        // Garbage never panics.
        for cut in 0..body.len() {
            let _ = parse(&body[..cut]);
        }
    }
}
