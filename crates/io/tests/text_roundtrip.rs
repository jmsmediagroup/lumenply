//! Text layers through the file formats: `.lumen` keeps every text
//! property (font, bold, italic, alignment, tracking) and re-renders the
//! same pixels; PSD keeps it as editable Photoshop type (`TySh`).

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
fn psd_text_layers_round_trip_as_editable_type() {
    let mut doc = Document::new(200, 100);
    let title = TextLayer {
        bold: true,
        italic: true,
        align: TextAlign::Center,
        tracking: 40.0,
        kerning: true,
        ..TextLayer::new("Title", 100.0, 40.0, 36.0, [0.0, 0.0, 0.0, 1.0])
    };
    let mut body = TextLayer {
        font: "NoSuchFontXYZ-Regular".into(),
        box_size: Some([180.0, 30.0]),
        align: TextAlign::Justify,
        line_height: 1.5,
        underline: true,
        ..TextLayer::new("Body text that wraps", 10.0, 55.0, 18.0, [0.2, 0.4, 0.6, 1.0])
    };
    body.apply_style(
        5,
        9,
        lumenply_doc::text_runs::CharStyle {
            bold: Some(true),
            size: Some(24.0),
            ..Default::default()
        },
    );
    let a = add_text(&mut doc, title.clone());
    let b = add_text(&mut doc, body.clone());
    let path = temp("text.psd");
    let rep = lumenply_io::psd::save(&path, &doc).expect("export");
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    let back = lumenply_io::psd::load(&path).expect("import");
    let _ = std::fs::remove_file(&path);
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let layers = back.value.layers();
    assert_eq!(layers.len(), 2);
    let same = |got: &TextLayer, want: &TextLayer| {
        // Colours pass through 0-1 sRGB with five decimals.
        for (g, w) in got.color.iter().zip(want.color) {
            assert!((g - w).abs() < 1e-4, "{:?} vs {:?}", got.color, want.color);
        }
        let mut got = got.clone();
        got.color = want.color;
        assert_eq!(&got, want);
    };
    let ta = layers[0].text_layer().expect("Title is text again");
    assert_eq!(layers[0].name, doc.layer(a).unwrap().name);
    same(ta, &title);
    let tb = layers[1].text_layer().expect("Body is text again");
    assert_eq!(layers[1].name, doc.layer(b).unwrap().name);
    same(tb, &body);
    assert_eq!(tb.runs.len(), 1);
    assert_eq!((tb.runs[0].start, tb.runs[0].end), (5, 9));
    // Re-rendered on import.
    assert!(ta.cache.as_ref().and_then(|c| c.content_bounds()).is_some());
}

#[test]
fn lumen_round_trip_keeps_paragraph_boxes_and_character_options() {
    let mut doc = Document::new(400, 300);
    let para = TextLayer {
        box_size: Some([180.0, 120.0]),
        align: TextAlign::Justify,
        baseline_shift: -3.5,
        all_caps: true,
        kerning: true,
        underline: true,
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
    assert!(t.kerning && t.underline && !t.strikethrough);
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
    assert_eq!((t.baseline_shift, t.all_caps, t.kerning), (0.0, false, false));
    assert!(t.runs.is_empty());
    assert_eq!(t.align, TextAlign::Center);
    let new = r#"{"text":"J","x":0.0,"y":0.0,"size":9.0,"color":[0.0,0.0,0.0,1.0],
        "bold":false,"line_height":1.2,"align":"justify","box_size":[50.0,20.0]}"#;
    let j: TextLayer = serde_json::from_str(new).expect("new keys");
    assert_eq!((j.align, j.box_size), (TextAlign::Justify, Some([50.0, 20.0])));
}
