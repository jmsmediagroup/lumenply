//! The real models on real pixels. They need the downloaded models, so they
//! run only when `LUMENPLY_AI_MODELS` names a store holding them (as the
//! CLI's `lumenply ai --models DIR download NAME` leaves it), and print a
//! notice otherwise:
//!
//! ```sh
//! lumenply ai --models /tmp/models download mobile-sam
//! lumenply ai --models /tmp/models download birefnet-lite
//! LUMENPLY_AI_MODELS=/tmp/models cargo test --release -p lumenply-ai --test real_models -- --nocapture
//! ```
//!
//! Model output differs slightly between providers and machines (ADR 0028),
//! so these assert coverage over regions, never exact pixels.

use std::path::PathBuf;
use std::time::Instant;

use lumenply_ai::{Matter, ModelId, ModelStore, Prompt, RefineOptions, Runtime, Segmenter};
use lumenply_tiles::{Raster, Rgba};

const ENV: &str = "LUMENPLY_AI_MODELS";

/// The store, or `None` (with a notice) when the models aren't there.
fn store(id: ModelId) -> Option<ModelStore> {
    let Some(dir) = std::env::var_os(ENV) else {
        println!("skipped: set {ENV} to a folder holding the downloaded models to run this");
        return None;
    };
    let store = ModelStore::new(PathBuf::from(dir));
    if !store.installed(id) {
        println!("skipped: {id} is not installed in {}", store.dir().display());
        return None;
    }
    Some(store)
}

/// The CC0 demo photo (1800×1205, Aoraki/Mount Cook at sunrise).
fn demo_photo() -> Raster {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/assets/demo-photo.jpg");
    lumenply_io::load(&path).expect("demo photo")
}

/// Fraction of a hard mask's pixels where two mattes agree.
fn agreement(a: &lumenply_ai::Matte, b: &lumenply_ai::Matte) -> f32 {
    let same = a
        .alpha
        .iter()
        .zip(&b.alpha)
        .filter(|(x, y)| (**x >= 0.5) == (**y >= 0.5))
        .count();
    same as f32 / a.alpha.len() as f32
}

#[test]
fn mobile_sam_selects_the_mountain_not_the_sky() {
    let Some(store) = store(ModelId::MobileSam) else {
        return;
    };
    let img = demo_photo();
    let rt = Runtime::new().unwrap();
    let t = Instant::now();
    let seg = Segmenter::load(&rt, &store).unwrap();
    println!("MobileSAM loaded in {:?} on {}", t.elapsed(), seg.provider());
    let t = Instant::now();
    let emb = seg.embed(&img).unwrap();
    println!("embedded in {:?}", t.elapsed());
    // A click on the snow of Aoraki's summit ridge.
    let click = [Prompt::Point {
        x: 300.0,
        y: 600.0,
        positive: true,
    }];
    let t = Instant::now();
    let m = seg.segment(&emb, &click).unwrap();
    println!("selected in {:?}", t.elapsed());
    assert_eq!((m.width, m.height), (1800, 1205));
    // The mountains, the lake and the valley floor are in…
    let peak = m.coverage(240, 560, 360, 640);
    let range = m.coverage(0, 780, 1800, 1100);
    // …the sky is out, both the clouds and the bright band at the horizon.
    let clouds = m.coverage(0, 0, 1800, 400);
    let horizon = m.coverage(500, 470, 1500, 590);
    println!("peak {peak:.4}, range {range:.4}, clouds {clouds:.4}, horizon {horizon:.4}");
    assert!(peak > 0.97, "peak {peak}");
    assert!(range > 0.97, "range {range}");
    assert!(clouds < 0.005, "clouds {clouds}");
    assert!(horizon < 0.02, "horizon {horizon}");

    // The same click on the CPU selects the same pixels.
    let cpu = Segmenter::load(&Runtime::cpu().unwrap(), &store).unwrap();
    let m_cpu = cpu.segment(&cpu.embed(&img).unwrap(), &click).unwrap();
    let agree = agreement(&m, &m_cpu);
    println!(
        "{} vs CPU: {:.4} % of pixels agree",
        seg.provider(),
        agree * 100.0
    );
    assert!(agree > 0.998, "{agree}");

    // An exclude click on the lake's valley keeps the mountains but no
    // longer the whole lower half: the selection changes, the sky stays out.
    let two = seg
        .segment_with(
            &emb,
            &[
                click[0],
                Prompt::Point {
                    x: 600.0,
                    y: 940.0,
                    positive: false,
                },
            ],
            RefineOptions::OFF,
        )
        .unwrap();
    assert!(two.coverage(0, 0, 1800, 400) < 0.005);
    assert!(two.coverage(240, 560, 360, 640) > 0.9);
}

/// The demo photo with a shaded orange ball in front: a clear subject.
fn ball_photo() -> (Raster, f32, f32, f32) {
    let mut img = demo_photo();
    let (w, h) = (img.width as f32, img.height as f32);
    let (cx, cy, r) = (0.5 * w, 0.55 * h, 0.28 * h);
    let light = {
        let l = [-0.5f32, -0.6, 0.62];
        let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        l.map(|v| v / n)
    };
    for y in 0..img.height {
        for x in 0..img.width {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / r, (y as f32 + 0.5 - cy) / r);
            let d2 = dx * dx + dy * dy;
            if d2 < 1.0 {
                let nz = (1.0 - d2).sqrt();
                let shade = (dx * light[0] + dy * light[1] + nz * light[2]).clamp(0.0, 1.0);
                let k = 0.15 + 0.85 * shade;
                img.set(x, y, Rgba::new(0.8 * k, 0.26 * k, 0.01 * k, 1.0));
            }
        }
    }
    (img, cx, cy, r)
}

/// Mean of `m` over the pixels whose centre lies in the ring `r0..r1`
/// around `(cx, cy)`.
fn ring(m: &lumenply_ai::Matte, cx: f32, cy: f32, r0: f32, r1: f32) -> f32 {
    let (mut sum, mut n) = (0f64, 0usize);
    for y in 0..m.height {
        for x in 0..m.width {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            if d >= r0 && d < r1 {
                sum += m.get(x, y) as f64;
                n += 1;
            }
        }
    }
    (sum / n.max(1) as f64) as f32
}

#[test]
fn birefnet_mattes_the_subject_and_drops_the_background() {
    let Some(store) = store(ModelId::BiRefNetLite) else {
        return;
    };
    let (img, cx, cy, r) = ball_photo();
    let rt = Runtime::new().unwrap();
    let t = Instant::now();
    let matter = Matter::load(&rt, &store).unwrap();
    println!("BiRefNet loaded in {:?} on {}", t.elapsed(), matter.provider());
    let t = Instant::now();
    let m = matter.matte(&img).unwrap();
    println!("matted in {:?}", t.elapsed());
    let inside = ring(&m, cx, cy, 0.0, r - 4.0);
    let edge_in = ring(&m, cx, cy, r - 4.0, r);
    let edge_out = ring(&m, cx, cy, r, r + 4.0);
    let outside = ring(&m, cx, cy, r + 4.0, 1e9);
    println!("inside {inside:.4}, edge in {edge_in:.4}, edge out {edge_out:.4}, outside {outside:.5}");
    assert!(inside > 0.99, "inside {inside}");
    assert!(edge_in > 0.8, "edge inside {edge_in}");
    assert!(edge_out < 0.2, "edge outside {edge_out}");
    assert!(outside < 0.005, "outside {outside}");
    // Far from the ball the matte is exactly clear.
    assert_eq!(m.get(5, 5), 0.0);
    assert_eq!(m.get(cx as u32, cy as u32), 1.0);
}
