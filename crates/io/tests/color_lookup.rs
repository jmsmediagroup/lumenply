//! Color Lookup layers through the file formats: `.lumen` embeds each
//! distinct table once as `luts/<hash>.cube`; PSD embeds it as `.cube`
//! text in the `clrL` descriptor, as Photoshop does.

use std::path::PathBuf;
use std::sync::Arc;

use lumenply_doc::lut::looks::Look;
use lumenply_doc::{Adjustment, BlendMode, Document, Layer, LayerContent, Lut3D};
use lumenply_tiles::Rgba;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-lut-{}-{name}", std::process::id()))
}

fn lookups(doc: &Document) -> Vec<(Arc<Lut3D>, String)> {
    let mut v = Vec::new();
    doc.for_each_layer(|l| {
        if let LayerContent::Adjustment(Adjustment::ColorLookup { lut, name }) = &l.content {
            v.push((lut.clone(), name.clone()));
        }
    });
    v
}

/// A grey-ramp background under three Color Lookups: Teal & Orange twice
/// (one table shared) and a red/blue swap at 50% in Multiply.
fn lut_doc() -> Document {
    let mut doc = Document::new(64, 32);
    let bg = doc.add_pixel_layer("Background");
    if let Some(LayerContent::Pixel(store)) = doc.layer_mut(bg).map(|l| &mut l.content) {
        for x in 0..64 {
            for y in 0..32 {
                let v = x as f32 / 63.0;
                store.set_pixel(x, y, Rgba::new(v, v * 0.5, 1.0 - v, 1.0));
            }
        }
    }
    let teal = Arc::new(Look::TealOrange.table());
    for n in 0..2 {
        let id = doc.alloc_id();
        let mut l = Layer::adjustment(
            id,
            Adjustment::ColorLookup {
                lut: teal.clone(),
                name: "Teal & Orange".into(),
            },
        );
        l.name = format!("Look {n}");
        doc.add_layer(l);
    }
    let id = doc.alloc_id();
    let mut swap = Layer::adjustment(
        id,
        Adjustment::ColorLookup {
            lut: Arc::new(Lut3D::from_fn(17, |[r, g, b]| [b, g, r]).with_title("Swap")),
            name: "Swap".into(),
        },
    );
    swap.opacity = 0.5;
    swap.blend = BlendMode::Multiply;
    doc.add_layer(swap);
    doc
}

#[test]
fn lumen_embeds_each_table_once_and_restores_it_exactly() {
    let doc = lut_doc();
    let path = temp("looks.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    // Two distinct tables, however many layers use them.
    let zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let entries: Vec<String> = zip
        .file_names()
        .filter(|n| n.starts_with("luts/"))
        .map(String::from)
        .collect();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries.iter().all(|n| n.ends_with(".cube")));
    let back = lumenply_io::project::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let (a, b) = (lookups(&doc), lookups(&back));
    assert_eq!(a, b, "tables and names come back value for value");
    assert!(Arc::ptr_eq(&b[0].0, &b[1].0), "equal tables share one copy again");
    assert_eq!(back.layers()[3].blend, BlendMode::Multiply);
    let (ca, cb) = (
        lumenply_render::composite(&doc),
        lumenply_render::composite(&back),
    );
    for (x, y) in [(0, 0), (20, 10), (63, 31)] {
        assert_eq!(ca.get_pixel(x, y), cb.get_pixel(x, y), "({x}, {y})");
    }
}

#[test]
fn lumen_refuses_a_missing_or_foreign_table_entry() {
    let doc = lut_doc();
    let path = temp("broken.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    // Re-pack the archive without its LUT entries.
    let mut zin = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let out = temp("broken2.lumen");
    {
        let mut zout = zip::ZipWriter::new(std::fs::File::create(&out).unwrap());
        for i in 0..zin.len() {
            let f = zin.by_index_raw(i).unwrap();
            if !f.name().starts_with("luts/") {
                zout.raw_copy_file(f).unwrap();
            }
        }
        zout.finish().unwrap();
    }
    let err = lumenply_io::project::load(&out).unwrap_err().to_string();
    let _ = (std::fs::remove_file(&path), std::fs::remove_file(&out));
    assert!(err.contains("missing entry luts/"), "{err}");
}

/// Also leaves `<tmp>/lumenply-psd-extra/color_lookup.psd` for psd-tools.
#[test]
fn psd_embeds_the_table_as_cube_text_and_reads_it_back() {
    let mut doc = lut_doc();
    // Plus a fresh, unchosen lookup: Photoshop's empty descriptor.
    let id = doc.alloc_id();
    doc.add_layer(Layer::adjustment(id, Adjustment::color_lookup_default()));
    let dir = std::env::temp_dir().join("lumenply-psd-extra");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("color_lookup.psd");
    let report = lumenply_io::psd::save(&path, &doc).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let back = lumenply_io::psd::load(&path).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let (a, b) = (lookups(&doc), lookups(&back.value));
    assert_eq!(b.len(), 4);
    for i in 0..4 {
        assert_eq!(a[i].1, b[i].1, "name of lookup {i}");
        assert_eq!(a[i].0.data, b[i].0.data, "table of lookup {i} is exact");
        assert_eq!(a[i].0.size, b[i].0.size);
    }
    let swap = &back.value.layers()[3];
    assert_eq!(swap.blend, BlendMode::Multiply);
    assert!((swap.opacity - 0.5).abs() < 3e-3);
}
