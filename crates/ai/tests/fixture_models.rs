//! The pipelines end to end through tiny stand-in models with the real
//! models' interfaces (`tests/fixtures/make_fixtures.py` documents what
//! each computes), so session creation, provider choice and tensor
//! plumbing run in CI without downloads.

use std::path::PathBuf;

use lumenply_ai::{AiError, Matter, ModelStore, Prompt, RefineOptions, Runtime, Segmenter};
use lumenply_tiles::{Raster, Rgba};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn segmenter() -> Segmenter {
    let rt = Runtime::new().unwrap();
    Segmenter::load_files(&rt, &fixture("sam_encoder.onnx"), &fixture("sam_decoder.onnx")).unwrap()
}

/// Linear light for sRGB ½.
const HALF: f32 = 0.214_041_14;

fn click(x: f32, y: f32) -> Prompt {
    Prompt::Point { x, y, positive: true }
}

#[test]
fn the_encoder_sees_the_resized_image_padded_with_sams_mean() {
    let seg = segmenter();
    println!("stand-in MobileSAM runs on: {}", seg.provider());
    // 2048×1024 sRGB ½ grey: the frame holds it in its top 512 rows.
    let img = Raster::filled(2048, 1024, Rgba::new(HALF, HALF, HALF, 1.0));
    let emb = seg.embed(&img).unwrap();
    assert_eq!((emb.width(), emb.height(), emb.cell()), (2048, 1024, 8.0));
    let f = emb.features();
    assert_eq!(f.len(), 256 * 64 * 64);
    // Channel c holds colour c % 3 of each 16×16 block's mean / 255.
    let at = |c: usize, y: usize, x: usize| f[(c * 64 + y) * 64 + x];
    let close = |a: f32, b: f32| (a - b).abs() < 2e-3;
    assert!(close(at(0, 10, 10), 0.5), "{}", at(0, 10, 10));
    assert!(close(at(4, 31, 63), 0.5), "{}", at(4, 31, 63));
    // Below the image: SAM's mean colour (123.675, 116.28, 103.53).
    assert!(close(at(0, 40, 10), 0.485), "{}", at(0, 40, 10));
    assert!(close(at(1, 63, 0), 0.456), "{}", at(1, 63, 0));
    assert!(close(at(5, 32, 5), 0.406), "{}", at(5, 32, 5));

    // The same pixels again: the cached embedding, not a second run.
    let again = seg.embed(&img.clone()).unwrap();
    assert!(again.is_same(&emb));
    assert_eq!(again.key(), emb.key());
    let mut other = img.clone();
    other.set(0, 0, Rgba::BLACK);
    let third = seg.embed(&other).unwrap();
    assert!(!third.is_same(&emb));
    assert_ne!(third.key(), emb.key());
    // The first image is still cached behind the second.
    assert!(seg.embed(&img).unwrap().is_same(&emb));
}

#[test]
fn a_click_selects_the_mask_around_it_at_full_resolution() {
    let seg = segmenter();
    let img = Raster::filled(2048, 1024, Rgba::new(HALF, HALF, HALF, 1.0));
    let emb = seg.embed(&img).unwrap();
    // One click at image (1000, 500) = frame (500, 250). The decoder's
    // candidates are cones of radius 96, 32, 64, 160 frame pixels scored
    // 0.5, 0.6, 0.9, 0.7: SAM picks the best multimask output, index 2.
    let pred = seg.predict(&emb, &[click(1000.0, 500.0)]).unwrap();
    assert_eq!((pred.index, pred.score), (2, 0.9));
    assert_eq!(pred.logits.len(), 256 * 256);
    // Grid cell (125, 62) has its centre at frame (502, 250): 2 px away.
    assert!((pred.logits[62 * 256 + 125] - 62.0).abs() < 1e-4);

    // At full resolution: a disc of 64 frame = 128 image pixels.
    let m = seg
        .segment_with(&emb, &[click(1000.0, 500.0)], RefineOptions::OFF)
        .unwrap();
    assert_eq!((m.width, m.height), (2048, 1024));
    for (dx, dy, inside) in [
        (0, 0, 1.0),
        (124, 0, 1.0),
        (132, 0, 0.0),
        (0, -124, 1.0),
        (0, 132, 0.0),
        (-87, 87, 1.0),  // 123 px away
        (-96, -96, 0.0), // 136 px away
    ] {
        let (x, y) = ((1000 + dx) as u32, (500 + dy) as u32);
        assert_eq!(m.get(x, y), inside, "({dx}, {dy})");
    }
    let area: f32 = m.alpha.iter().sum();
    let disc = std::f32::consts::PI * 128.0 * 128.0;
    assert!((area / disc - 1.0).abs() < 0.01, "{area} vs {disc}");
    // The same mask from the prediction already decoded.
    assert_eq!(pred.matte(&emb, RefineOptions::OFF), m);

    // Two clicks: SAM's single-mask output (index 0, 96 frame px) around
    // the first.
    let two = seg
        .segment_with(
            &emb,
            &[
                click(1000.0, 500.0),
                Prompt::Point {
                    x: 1800.0,
                    y: 900.0,
                    positive: false,
                },
            ],
            RefineOptions::OFF,
        )
        .unwrap();
    assert_eq!((two.get(1180, 500), two.get(1200, 500)), (1.0, 0.0));

    // No prompt is an error, not an empty selection.
    assert!(matches!(seg.segment(&emb, &[]), Err(AiError::Invalid(_))));
}

#[test]
fn refinement_puts_the_selection_on_the_objects_edge() {
    let seg = segmenter();
    // An orange disc of radius 120 around (1000, 500) on grey: the stand-in
    // decoder's disc (128 px) overshoots the object by 8 px all round, as
    // an upscaled model mask is off by a few pixels.
    let (w, h) = (2048u32, 1024u32);
    let mut img = Raster::filled(w, h, Rgba::new(HALF, HALF, HALF, 1.0));
    for y in 300..700 {
        for x in 800..1200 {
            let (dx, dy) = (x as f32 + 0.5 - 1000.0, y as f32 + 0.5 - 500.0);
            if dx * dx + dy * dy < 120.0 * 120.0 {
                img.set(x, y, Rgba::new(0.8, 0.25, 0.02, 1.0));
            }
        }
    }
    let emb = seg.embed(&img).unwrap();
    let raw = seg
        .segment_with(&emb, &[click(1000.0, 500.0)], RefineOptions::OFF)
        .unwrap();
    let m = seg.segment(&emb, &[click(1000.0, 500.0)]).unwrap();
    // Along each axis: 116 px out is the object, 124 px is grey. The raw
    // mask holds both; the refined one only the object.
    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let at = |m: &lumenply_ai::Matte, d: i32| m.get((1000 + dx * d) as u32, (500 + dy * d) as u32);
        assert_eq!((at(&raw, 116), at(&raw, 124)), (1.0, 1.0), "raw ({dx}, {dy})");
        assert_eq!((at(&m, 116), at(&m, 124)), (1.0, 0.0), "refined ({dx}, {dy})");
    }
    let area: f32 = m.alpha.iter().sum();
    let disc = std::f32::consts::PI * 120.0 * 120.0;
    assert!((area / disc - 1.0).abs() < 0.01, "{area} vs {disc}");
}

#[test]
fn the_matter_mattes_the_subject_at_full_resolution() {
    let rt = Runtime::new().unwrap();
    // The stand-in reads red as subject, blue as background, at 64².
    let matter = Matter::load_file(&rt, &fixture("matter.onnx"), 64).unwrap();
    println!("stand-in BiRefNet runs on: {}", matter.provider());
    // A red disc of radius 60 on blue, 300×200.
    let (w, h) = (300u32, 200u32);
    let mut img = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - 150.0, y as f32 + 0.5 - 100.0);
            let c = if dx * dx + dy * dy < 60.0 * 60.0 {
                Rgba::new(0.6, 0.02, 0.01, 1.0)
            } else {
                Rgba::new(0.01, 0.03, 0.5, 1.0)
            };
            img.set(x, y, c);
        }
    }
    let small = matter.predict(&img).unwrap();
    assert_eq!(small.len(), 64 * 64);
    assert!(small[32 * 64 + 32] > 0.999 && small[0] < 0.001);

    let m = matter.matte(&img).unwrap();
    assert_eq!((m.width, m.height), (w, h));
    assert!(m.coverage(130, 80, 170, 120) > 0.999);
    assert!(m.coverage(0, 0, 40, 40) < 0.001);
    assert!(m.coverage(260, 160, 300, 200) < 0.001);
    // Along the row through the centre the disc ends after x = 209. The
    // model's 64² matte, upscaled 4.7×, smears that over 209..213; the
    // guided upsampling puts it on the image's edge.
    let row = |m: &lumenply_ai::Matte| {
        (204..216)
            .map(|x| (m.get(x, 100) * 100.0).round() / 100.0)
            .collect::<Vec<_>>()
    };
    let raw = matter.matte_with(&img, RefineOptions::OFF).unwrap();
    assert_eq!(
        row(&raw),
        vec![1.0, 1.0, 1.0, 1.0, 1.0, 0.81, 0.59, 0.38, 0.17, 0.0, 0.0, 0.0]
    );
    assert_eq!(
        row(&m),
        vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.02, 0.01, 0.0, 0.0, 0.0, 0.0]
    );
    // Down the column the disc ends after y = 159 (that pixel only partly
    // in the disc's colour band of the 64² model).
    assert!(
        m.get(150, 158) > 0.99 && m.get(150, 159) > 0.9,
        "{}",
        m.get(150, 159)
    );
    assert_eq!((m.get(150, 160), m.get(150, 161)), (0.0, 0.0));
}

/// A model the accelerator fails to compile still loads, on the CPU, and
/// says why (macOS: CoreML rejects a convolution without explicit pads,
/// as it rejects BiRefNet's).
#[test]
fn a_model_the_accelerator_cant_compile_falls_back_to_the_cpu() {
    let rt = Runtime::new().unwrap();
    let matter = Matter::load_file(&rt, &fixture("coreml_refuses.onnx"), 64).unwrap();
    let report = matter.report();
    println!("coreml_refuses.onnx: {} {:?}", matter.provider(), report.fallback);
    if cfg!(target_os = "macos") && std::env::var_os(lumenply_ai::PROVIDER_ENV).is_none() {
        assert_eq!(report.provider, lumenply_ai::Provider::Cpu);
        assert_eq!(report.fallback.len(), 1, "{:?}", report.fallback);
        assert!(
            report.fallback[0].starts_with("CoreML: "),
            "{:?}",
            report.fallback
        );
        assert!(report.fallback[0].contains("pad"), "{:?}", report.fallback);
    }
    // Whatever it runs on, it computes the same.
    let img = Raster::filled(64, 64, Rgba::new(0.6, 0.02, 0.01, 1.0));
    assert!(matter.predict(&img).unwrap().iter().all(|&v| v > 0.999));
}

#[test]
fn missing_models_say_so() {
    let dir = std::env::temp_dir().join(format!("lumenply-ai-empty-store-{}", std::process::id()));
    let store = ModelStore::new(&dir);
    let rt = Runtime::cpu().unwrap();
    assert!(matches!(
        Segmenter::load(&rt, &store),
        Err(AiError::NotInstalled(_))
    ));
    assert!(matches!(Matter::load(&rt, &store), Err(AiError::NotInstalled(_))));
    assert!(!dir.exists(), "loading must not create the store");
}
