//! The newer adjustments (and fill layers) through `.lumen`: every
//! parameter survives a save and load exactly.

use std::path::PathBuf;

use lumenply_doc::{Adjustment, Document, Gradient, GradientStop, LayerContent};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-adjfill-{}-{name}", std::process::id()))
}

#[test]
fn lumen_keeps_every_new_adjustment_exactly() {
    let mut doc = Document::new(16, 16);
    doc.add_pixel_layer("Background");
    let mut colors = [[0.0f32; 4]; 9];
    colors[2] = [0.1, -0.2, 0.3, -0.4];
    let adjs = vec![
        Adjustment::GradientMap {
            gradient: Gradient {
                stops: vec![
                    GradientStop::srgb8(0.0, [10, 20, 30]),
                    GradientStop::srgb8(0.4, [200, 100, 50]),
                    GradientStop::srgb8(1.0, [250, 240, 230]),
                ],
            },
            reverse: true,
        },
        Adjustment::ChannelMixer {
            red: [1.1, -0.1, 0.0, 0.02],
            green: [0.0, 0.9, 0.1, 0.0],
            blue: [0.05, 0.0, 0.95, -0.03],
            monochrome: false,
            gray: [0.3, 0.6, 0.1, 0.0],
        },
        Adjustment::PhotoFilter {
            color: [0.1, 0.2, 0.9],
            density: 0.37,
            preserve_luminosity: false,
        },
        Adjustment::SelectiveColor {
            colors,
            absolute: true,
        },
    ];
    for a in &adjs {
        doc.add_adjustment(a.clone());
    }
    let path = temp("adjust.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let back = lumenply_io::project::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let got: Vec<Adjustment> = back
        .layers()
        .iter()
        .filter_map(|l| match &l.content {
            LayerContent::Adjustment(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(got, adjs);
}
