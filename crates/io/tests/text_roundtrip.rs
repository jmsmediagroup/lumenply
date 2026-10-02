//! Text layers through the file formats: `.lumen` keeps every text
//! property (font, bold, italic, alignment, tracking) and re-renders the
//! same pixels; PSD export rasterises text and names the font it used.

use std::path::PathBuf;

use lumenply_doc::{Document, Layer, LayerId, TextAlign, TextLayer};
use lumenply_render::text::{missing_fonts, refresh_cache};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-text-{}-{name}", std::process::id()))
}

fn add_text(doc: &mut Document, t: TextLayer) -> LayerId {
    let mut t = t;
    refresh_cache(&mut t);
    let id = doc.alloc_id();
    doc.add_layer(Layer::text(id, t));
    id
}

#[test]
fn lumen_round_trip_keeps_font_bold_italic_and_layout() {
    const FAMILY: &str = "Lumenply Test Family";
    let mut doc = Document::new(400, 300);
    let styled = TextLayer {
        font: FAMILY.into(),
        bold: true,
        italic: true,
        align: TextAlign::Center,
        tracking: 120.0,
        line_height: 1.4,
        ..TextLayer::new("Styled\nsecond line", 200.0, 120.0, 48.0, [0.2, 0.4, 0.6, 1.0])
    };
    let bundled = TextLayer {
        font: "DejaVu Sans".into(),
        italic: true,
        align: TextAlign::Right,
        ..TextLayer::new("Plain", 380.0, 260.0, 30.0, [0.0, 0.0, 0.0, 1.0])
    };
    let a = add_text(&mut doc, styled.clone());
    let b = add_text(&mut doc, bundled.clone());

    let path = temp("styled.lumen");
    lumenply_io::project::save(&path, &doc).expect("save");
    let back = lumenply_io::project::load(&path).expect("load");
    let _ = std::fs::remove_file(&path);

    let ta = back.layer(a).unwrap().text_layer().unwrap();
    assert_eq!(ta.text, "Styled\nsecond line");
    assert_eq!(ta.font, FAMILY);
    assert!(ta.bold && ta.italic, "bold and italic survive");
    assert_eq!(ta.align, TextAlign::Center);
    assert_eq!(ta.tracking, 120.0);
    assert_eq!(ta.line_height, 1.4);
    assert_eq!((ta.x, ta.y, ta.size), (200.0, 120.0, 48.0));
    assert_eq!(ta.color, [0.2, 0.4, 0.6, 1.0]);
    assert_eq!(*ta, styled, "every editable field matches");

    let tb = back.layer(b).unwrap().text_layer().unwrap();
    assert_eq!(tb.font, "DejaVu Sans");
    assert!(!tb.bold && tb.italic);
    assert_eq!(tb.align, TextAlign::Right);
    assert_eq!(*tb, bundled);

    // The cache is rebuilt on load and matches the original pixels.
    let orig = doc.layer(a).unwrap().text_layer().unwrap().cache.clone().unwrap();
    let again = ta.cache.clone().expect("re-rasterised on load");
    let bounds = orig.content_bounds().unwrap();
    assert_eq!(again.content_bounds(), Some(bounds));
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            assert_eq!(again.get_pixel(x, y), orig.get_pixel(x, y), "({x}, {y})");
        }
    }

    // The uninstalled family is what an app reports after opening.
    assert_eq!(missing_fonts(back.layers()), vec![FAMILY.to_string()]);
}

#[test]
fn psd_export_names_the_font_of_rasterised_text() {
    let mut doc = Document::new(200, 100);
    add_text(
        &mut doc,
        TextLayer {
            bold: true,
            italic: true,
            ..TextLayer::new("Title", 10.0, 60.0, 36.4, [0.0, 0.0, 0.0, 1.0])
        },
    );
    add_text(
        &mut doc,
        TextLayer {
            font: "Georgia".into(),
            ..TextLayer::new("Body", 10.0, 90.0, 18.0, [0.0, 0.0, 0.0, 1.0])
        },
    );
    let path = temp("text.psd");
    let rep = lumenply_io::psd::save(&path, &doc).expect("export");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        rep.warnings,
        vec![
            "text layer 'Title' was exported as pixels (font: DejaVu Sans Bold Italic, 36 px)".to_string(),
            "text layer 'Body' was exported as pixels (font: Georgia, 18 px)".to_string(),
        ]
    );
}
