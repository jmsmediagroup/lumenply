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

#[test]
fn lumen_round_trip_keeps_paragraph_boxes_and_character_options() {
    let mut doc = Document::new(400, 300);
    let para = TextLayer {
        box_size: Some([180.0, 120.0]),
        align: TextAlign::Justify,
        baseline_shift: -3.5,
        all_caps: true,
        line_height: 1.6,
        ..TextLayer::new(
            "wraps inside its box when it is long",
            20.0,
            30.0,
            20.0,
            [0.0, 0.0, 0.0, 1.0],
        )
    };
    let id = add_text(&mut doc, para.clone());
    let path = temp("paragraph.lumen");
    lumenply_io::project::save(&path, &doc).expect("save");
    let back = lumenply_io::project::load(&path).expect("load");
    let _ = std::fs::remove_file(&path);
    let t = back.layer(id).unwrap().text_layer().unwrap();
    assert_eq!(t.box_size, Some([180.0, 120.0]));
    assert_eq!(t.align, TextAlign::Justify);
    assert_eq!((t.baseline_shift, t.all_caps, t.line_height), (-3.5, true, 1.6));
    assert_eq!(*t, para);
    // Wrapped glyphs come back where they were.
    let orig = doc
        .layer(id)
        .unwrap()
        .text_layer()
        .unwrap()
        .cache
        .clone()
        .unwrap();
    assert_eq!(
        t.cache.as_ref().unwrap().content_bounds(),
        orig.content_bounds(),
        "same wrapped layout after loading"
    );
    let b = orig.content_bounds().unwrap();
    assert!(b.x >= 20 && b.right() <= 201, "inside the 180 px box: {b:?}");
    assert!(b.h > 60, "several wrapped lines: {b:?}");
}

#[test]
fn text_saved_before_paragraphs_existed_still_loads() {
    // A text layer as older versions wrote it: no box, shift or caps keys.
    let old = r#"{"text":"Old","x":10.0,"y":40.0,"size":24.0,"color":[0.0,0.0,0.0,1.0],
        "bold":false,"line_height":1.2,"font":"","italic":false,"align":"center","tracking":0.0}"#;
    let t: TextLayer = serde_json::from_str(old).expect("old text layer JSON");
    assert_eq!(t.box_size, None);
    assert_eq!((t.baseline_shift, t.all_caps), (0.0, false));
    assert_eq!(t.align, TextAlign::Center);
    let new = r#"{"text":"J","x":0.0,"y":0.0,"size":9.0,"color":[0.0,0.0,0.0,1.0],
        "bold":false,"line_height":1.2,"align":"justify","box_size":[50.0,20.0]}"#;
    let j: TextLayer = serde_json::from_str(new).expect("new keys");
    assert_eq!((j.align, j.box_size), (TextAlign::Justify, Some([50.0, 20.0])));
}
