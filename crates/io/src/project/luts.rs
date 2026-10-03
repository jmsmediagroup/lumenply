//! Color Lookup tables in a `.lumen` project: each distinct table is one
//! `.cube` file, `luts/<content hash>.cube`, written once however many
//! layers use it; a layer's manifest record names its entry. Tables are
//! always embedded — a project never points at the user's LUT file.

use std::collections::HashSet;
use std::io::{Read, Seek, Write};
use std::sync::Arc;

use lumenply_doc::{Adjustment, Document, LayerContent, Lut3D};
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

use super::ProjectError;
use crate::lut_files;

/// Largest table entry we read back (a 65³ cube is about 8 MB of text).
const MAX_LUT_ENTRY: u64 = 32 << 20;

/// The zip entry holding `lut`.
pub(super) fn entry_name(lut: &Lut3D) -> String {
    format!("luts/{:016x}.cube", lut.content_hash())
}

/// Write every distinct table the document's Color Lookup layers use.
pub(super) fn write_all<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    doc: &Document,
    opts: FileOptions,
) -> Result<(), ProjectError> {
    let mut tables = Vec::new();
    doc.for_each_layer(|l| {
        if let LayerContent::Adjustment(Adjustment::ColorLookup { lut, .. }) = &l.content {
            tables.push(lut.clone());
        }
    });
    let mut written = HashSet::new();
    for lut in tables {
        let name = entry_name(&lut);
        if written.insert(name.clone()) {
            zip.start_file(name, opts)?;
            zip.write_all(lut_files::write_cube(&lut).as_bytes())?;
        }
    }
    Ok(())
}

/// Read the table a layer record names.
pub(super) fn read<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Arc<Lut3D>, ProjectError> {
    if !name.starts_with("luts/") {
        return Err(ProjectError::Corrupt(format!("'{name}' is not a LUT entry")));
    }
    let f = zip
        .by_name(name)
        .map_err(|_| ProjectError::Corrupt(format!("manifest references missing entry {name}")))?;
    let mut buf = Vec::new();
    f.take(MAX_LUT_ENTRY + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_LUT_ENTRY {
        return Err(ProjectError::Corrupt(format!(
            "entry {name} is implausibly large"
        )));
    }
    let lut = lut_files::parse_cube(&String::from_utf8_lossy(&buf))
        .map_err(|e| ProjectError::Corrupt(format!("{name}: {e}")))?;
    Ok(Arc::new(lut))
}

/// Make layers whose tables are equal share one copy again after loading.
pub(super) fn share(doc: &mut Document) {
    let mut seen: Vec<(u64, Arc<Lut3D>)> = Vec::new();
    doc.for_each_layer_mut(|l| {
        if let LayerContent::Adjustment(Adjustment::ColorLookup { lut, .. }) = &mut l.content {
            let h = lut.content_hash();
            match seen.iter().find(|(k, t)| *k == h && **t == **lut) {
                Some((_, t)) => *lut = t.clone(),
                None => seen.push((h, lut.clone())),
            }
        }
    });
}
