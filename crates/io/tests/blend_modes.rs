//! Every blend mode through every format: PSD (Photoshop's 4-character
//! keys, checked with the independent psd-tools reader when it is
//! installed), OpenRaster (`svg:*` composite ops, normal + warning for the
//! modes the spec lacks) and `.lumen` (serde names).

use std::path::PathBuf;
use std::process::Command;

use lumenply_doc::{BlendMode, Document};
use lumenply_tiles::Rgba;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-blends-{}-{name}", std::process::id()))
}

/// One small pixel layer per mode, named after the mode, plus a group in
/// Color Burn (groups carry their mode in the section divider too).
fn every_mode_doc() -> Document {
    let mut doc = Document::new(32, 32);
    for (i, m) in BlendMode::ALL.into_iter().enumerate() {
        let id = doc.add_pixel_layer(m.name());
        let l = doc.layer_mut(id).unwrap();
        l.blend = m;
        l.pixels_mut()
            .unwrap()
            .set_pixel(i as i32, i as i32, Rgba::from_straight(0.8, 0.4, 0.2, 1.0));
    }
    let g = doc.add_group("group-color-burn");
    doc.layer_mut(g).unwrap().blend = BlendMode::ColorBurn;
    doc
}

fn modes_by_name(doc: &Document) -> Vec<(String, BlendMode)> {
    let mut out = Vec::new();
    doc.for_each_layer(|l| out.push((l.name.clone(), l.blend)));
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// psd-tools' `BlendMode` enum names for our modes.
fn psd_tools_name(m: BlendMode) -> &'static str {
    match m {
        BlendMode::Normal => "NORMAL",
        BlendMode::Multiply => "MULTIPLY",
        BlendMode::Screen => "SCREEN",
        BlendMode::Overlay => "OVERLAY",
        BlendMode::Darken => "DARKEN",
        BlendMode::Lighten => "LIGHTEN",
        BlendMode::Difference => "DIFFERENCE",
        BlendMode::Add => "LINEAR_DODGE",
        BlendMode::HardLight => "HARD_LIGHT",
        BlendMode::SoftLight => "SOFT_LIGHT",
        BlendMode::Dissolve => "DISSOLVE",
        BlendMode::ColorBurn => "COLOR_BURN",
        BlendMode::LinearBurn => "LINEAR_BURN",
        BlendMode::DarkerColor => "DARKER_COLOR",
        BlendMode::ColorDodge => "COLOR_DODGE",
        BlendMode::LighterColor => "LIGHTER_COLOR",
        BlendMode::VividLight => "VIVID_LIGHT",
        BlendMode::LinearLight => "LINEAR_LIGHT",
        BlendMode::PinLight => "PIN_LIGHT",
        BlendMode::HardMix => "HARD_MIX",
        BlendMode::Exclusion => "EXCLUSION",
        BlendMode::Subtract => "SUBTRACT",
        BlendMode::Divide => "DIVIDE",
        BlendMode::Hue => "HUE",
        BlendMode::Saturation => "SATURATION",
        BlendMode::Color => "COLOR",
        BlendMode::Luminosity => "LUMINOSITY",
    }
}

/// Write → psd-tools reads the right `BlendMode` → our reader reads it
/// back. Leaves `<tmp>/lumenply-psd-extra/blend-modes.psd` for manual
/// inspection.
#[test]
fn psd_round_trips_every_blend_mode() {
    let doc = every_mode_doc();
    let dir = std::env::temp_dir().join("lumenply-psd-extra");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("blend-modes.psd");
    let report = lumenply_io::psd::save(&path, &doc).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let back = lumenply_io::psd::load(&path).unwrap();
    assert!(
        !back.warnings.iter().any(|w| w.contains("blend")),
        "{:?}",
        back.warnings
    );
    assert_eq!(modes_by_name(&back.value), modes_by_name(&doc));

    // The independent reader (ADR 0003), when installed.
    let script = "import sys\n\
        from psd_tools import PSDImage\n\
        psd = PSDImage.open(sys.argv[1])\n\
        for l in psd.descendants():\n\
        \x20   print(l.name + '=' + l.blend_mode.name)\n";
    match Command::new("python3").arg("-c").arg(script).arg(&path).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let got: std::collections::HashMap<&str, &str> =
                text.lines().filter_map(|l| l.split_once('=')).collect();
            for m in BlendMode::ALL {
                assert_eq!(got.get(m.name()).copied(), Some(psd_tools_name(m)), "{m:?}");
            }
            assert_eq!(got.get("group-color-burn").copied(), Some("COLOR_BURN"));
        }
        Ok(out) => eprintln!(
            "psd-tools not usable, skipping the independent check: {}",
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")
        ),
        Err(e) => eprintln!("python3 not found, skipping the psd-tools check: {e}"),
    }
}

#[test]
fn psd_reads_photoshop_keys() {
    // Spot-check the less obvious keys straight from bytes Photoshop
    // writes: the trailing spaces matter.
    let doc = every_mode_doc();
    let path = temp("keys.psd");
    lumenply_io::psd::save(&path, &doc).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    for key in [
        b"8BIMdiss",
        b"8BIMidiv",
        b"8BIMlbrn",
        b"8BIMdkCl",
        b"8BIMdiv ",
        b"8BIMlgCl",
        b"8BIMvLit",
        b"8BIMlLit",
        b"8BIMpLit",
        b"8BIMhMix",
        b"8BIMsmud",
        b"8BIMfsub",
        b"8BIMfdiv",
        b"8BIMhue ",
        b"8BIMsat ",
        b"8BIMcolr",
        b"8BIMlum ",
    ] {
        assert!(
            bytes.windows(8).any(|w| w == key),
            "{} missing",
            String::from_utf8_lossy(key)
        );
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn ora_carries_the_svg_modes_and_warns_for_the_rest() {
    let doc = every_mode_doc();
    let path = temp("modes.ora");
    let report = lumenply_io::ora::save(&path, &doc).unwrap();
    let back = lumenply_io::ora::load(&path).unwrap().value;
    let back = modes_by_name(&back);
    let svg = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Add,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::ColorBurn,
        BlendMode::ColorDodge,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];
    for m in BlendMode::ALL {
        let got = back.iter().find(|(n, _)| n == m.name()).map(|(_, b)| *b);
        let warned = report
            .warnings
            .iter()
            .any(|w| w.contains(&format!("'{}'", m.name())) && w.contains(m.label()));
        if svg.contains(&m) {
            assert_eq!(got, Some(m), "{m:?} keeps its mode");
            assert!(!warned, "{m:?}");
        } else {
            assert_eq!(got, Some(BlendMode::Normal), "{m:?} exports as normal");
            assert!(warned, "{m:?} warns: {:?}", report.warnings);
        }
    }
    // Group modes travel too.
    assert!(back.contains(&("group-color-burn".into(), BlendMode::ColorBurn)));
    // The exact composite-op strings.
    let file = std::fs::File::open(&path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("stack.xml").unwrap(), &mut xml).unwrap();
    for op in [
        "svg:color-burn",
        "svg:color-dodge",
        "svg:exclusion",
        "svg:hue",
        "svg:saturation",
        "svg:color\"",
        "svg:luminosity",
    ] {
        assert!(xml.contains(op), "{op} in stack.xml");
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn lumen_projects_keep_every_mode() {
    let doc = every_mode_doc();
    let path = temp("modes.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let back = lumenply_io::project::load(&path).unwrap();
    assert_eq!(modes_by_name(&back), modes_by_name(&doc));
    std::fs::remove_file(&path).ok();
}
