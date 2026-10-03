//! `lumenply ai`: the local AI models (ADR 0028) from the command line.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use lumenply_ai::{models, Matte, Matter, ModelId, ModelStore, Prompt, RefineOptions, Runtime, Segmenter};
use lumenply_tiles::Raster;

#[derive(Subcommand)]
pub enum AiCmd {
    /// List the models: installed or not, size, licence and source.
    Models,
    /// Download a model (mobile-sam, birefnet-lite), checking its SHA-256.
    Download { name: String },
    /// Delete a downloaded model.
    Remove { name: String },
    /// Cut out the main subject (BiRefNet lite): write IN with the
    /// subject's matte as alpha.
    RemoveBg {
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Also write the matte as a greyscale PNG.
        #[arg(long)]
        mask: Option<PathBuf>,
        /// Run this ONNX file instead of the installed model (another
        /// export with the same interface, for comparisons).
        #[arg(long, hide = true)]
        model_file: Option<PathBuf>,
        #[command(flatten)]
        tune: Tune,
        /// Run the model this many times and report each time.
        #[arg(long, default_value_t = 1)]
        runs: usize,
    },
    /// Select an object (MobileSAM) by clicks and/or a box; write the
    /// selection as a greyscale PNG mask.
    Select {
        input: PathBuf,
        /// X,Y to include, X,Y,neg to exclude (image pixels); repeatable.
        #[arg(long = "point")]
        points: Vec<String>,
        /// X0,Y0,X1,Y1 around the object.
        #[arg(long = "box")]
        bbox: Option<String>,
        #[arg(short, long)]
        out: PathBuf,
        /// Also write the image with the selection tinted, for a look.
        #[arg(long)]
        overlay: Option<PathBuf>,
        #[command(flatten)]
        tune: Tune,
        /// Run encoder and decoder this many times and report each time.
        #[arg(long, default_value_t = 1)]
        runs: usize,
    },
}

/// Edge refinement overrides (defaults: `RefineOptions::selection` or
/// `::matte` for the image's size).
#[derive(Args, Clone, Copy, Debug, Default)]
pub struct Tune {
    /// No edge refinement: the model's mask, upscaled.
    #[arg(long)]
    raw: bool,
    /// Colour matting in the band: on or off.
    #[arg(long)]
    matting: Option<bool>,
    /// Half-width of the refined band, image pixels.
    #[arg(long)]
    radius: Option<f32>,
    /// Guided filter window radius, image pixels.
    #[arg(long)]
    window: Option<u32>,
    /// Guided filter epsilon (sRGB variance).
    #[arg(long)]
    eps: Option<f32>,
    /// Contrast after the filter, 0-100.
    #[arg(long)]
    contrast: Option<f32>,
}

impl Tune {
    fn apply(&self, base: RefineOptions) -> RefineOptions {
        if self.raw {
            return RefineOptions::OFF;
        }
        RefineOptions {
            radius: self.radius.unwrap_or(base.radius),
            window: self.window.unwrap_or(base.window),
            eps: self.eps.unwrap_or(base.eps),
            contrast: self.contrast.unwrap_or(base.contrast),
            matting: self.matting.unwrap_or(base.matting),
        }
    }
}

/// Run `cmd` with the store in `models` (default: the app's) on the
/// platform's providers, or the CPU alone with `cpu`.
pub fn run(cmd: AiCmd, models_dir: Option<PathBuf>, cpu: bool) -> Result<()> {
    let dir = match models_dir {
        Some(d) => d,
        None => ModelStore::default_dir().ok_or_else(|| anyhow!("no home folder: pass --models DIR"))?,
    };
    let store = ModelStore::new(dir);
    let runtime = || -> Result<Runtime> { Ok(if cpu { Runtime::cpu()? } else { Runtime::new()? }) };
    match cmd {
        AiCmd::Models => list(&store),
        AiCmd::Download { name } => download(&store, model(&name)?),
        AiCmd::Remove { name } => {
            let id = model(&name)?;
            store.remove(id)?;
            println!("removed {id} from {}", store.dir().display());
            Ok(())
        }
        AiCmd::RemoveBg {
            input,
            out,
            mask,
            model_file,
            tune,
            runs,
        } => {
            let rt = runtime()?;
            let t = Instant::now();
            let matter = match model_file {
                Some(f) => Matter::load_file(&rt, &f, 1024)?,
                None => Matter::load(&rt, &store)
                    .with_context(|| format!("loading BiRefNet from {}", store.dir().display()))?,
            };
            eprintln!("load {:.0} ms: {}", ms(t), matter.provider());
            remove_bg(&matter, &input, &out, mask.as_deref(), tune, runs)
        }
        AiCmd::Select {
            input,
            points,
            bbox,
            out,
            overlay,
            tune,
            runs,
        } => {
            let prompts = prompts(&points, bbox.as_deref())?;
            select(
                &store,
                &runtime()?,
                &input,
                &prompts,
                &out,
                overlay.as_deref(),
                tune,
                runs,
            )
        }
    }
}

fn model(name: &str) -> Result<ModelId> {
    ModelId::parse(name).ok_or_else(|| {
        let names: Vec<&str> = ModelId::ALL.iter().map(|m| m.key()).collect();
        anyhow!("no model called {name:?} (one of: {})", names.join(", "))
    })
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

fn list(store: &ModelStore) -> Result<()> {
    println!("models in {}", store.dir().display());
    for m in models() {
        let state = if store.installed(m.id) {
            format!("installed ({} on disk)", mb(store.disk_bytes(m.id)))
        } else {
            "not installed".to_string()
        };
        println!("\n{} [{}]: {state}", m.name, m.id.key());
        println!("  {}", m.task);
        println!("  download {}, input {}²", mb(m.total_bytes), m.input_size);
        println!("  licence: {}", m.licence);
        println!("  source: {}", m.source);
        for f in m.files {
            println!("    {} ({}, sha256 {})", f.name, mb(f.bytes), f.sha256);
            if !f.avoid.is_empty() {
                let names: Vec<&str> = f.avoid.iter().map(|p| p.name()).collect();
                println!("      not run on {}", names.join(", "));
            }
        }
        if let Some(note) = m.note {
            println!("  note: {note}");
        }
    }
    Ok(())
}

fn download(store: &ModelStore, id: ModelId) -> Result<()> {
    let info = id.info();
    if store.installed(id) {
        println!("{id} is already installed in {}", store.model_dir(id).display());
        return Ok(());
    }
    eprintln!(
        "downloading {id} ({}, {}) from {}",
        mb(info.total_bytes),
        info.licence,
        info.source
    );
    let t = Instant::now();
    let last = std::cell::Cell::new(0u64);
    let progress = |done: u64, total: u64| {
        let pct = done * 100 / total.max(1);
        if pct >= last.get() + 5 || done == total {
            last.set(pct);
            eprint!("\r  {pct:3}% ({} of {})", mb(done), mb(total));
        }
    };
    store.download(id, &progress, &AtomicBool::new(false))?;
    eprintln!();
    println!(
        "installed {id} in {} ({:.1} s, SHA-256 checked)",
        store.model_dir(id).display(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

/// `--point X,Y[,neg]` and `--box X0,Y0,X1,Y1`.
fn prompts(points: &[String], bbox: Option<&str>) -> Result<Vec<Prompt>> {
    let mut out = Vec::new();
    for p in points {
        let parts: Vec<&str> = p.split(',').map(str::trim).collect();
        let num = |s: &str| {
            s.parse::<f32>()
                .with_context(|| format!("--point {p}: {s:?} is not a number"))
        };
        match parts.as_slice() {
            [x, y] => out.push(Prompt::Point {
                x: num(x)?,
                y: num(y)?,
                positive: true,
            }),
            [x, y, kind] => {
                let positive = match kind.to_ascii_lowercase().as_str() {
                    "neg" | "-" | "0" | "exclude" => false,
                    "pos" | "+" | "1" | "include" => true,
                    _ => bail!("--point {p}: the third value is neg or pos"),
                };
                out.push(Prompt::Point {
                    x: num(x)?,
                    y: num(y)?,
                    positive,
                });
            }
            _ => bail!("--point {p}: expected X,Y or X,Y,neg"),
        }
    }
    if let Some(b) = bbox {
        let v: Vec<f32> = b
            .split(',')
            .map(|s| s.trim().parse::<f32>())
            .collect::<Result<_, _>>()
            .with_context(|| format!("--box {b}"))?;
        let [x0, y0, x1, y1] = v[..] else {
            bail!("--box {b}: expected X0,Y0,X1,Y1");
        };
        out.push(Prompt::Box { x0, y0, x1, y1 });
    }
    if out.is_empty() {
        bail!("give at least one --point or a --box");
    }
    Ok(out)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn load_image(path: &Path) -> Result<Raster> {
    lumenply_io::load(path).with_context(|| format!("loading {}", path.display()))
}

fn save_mask(path: &Path, m: &Matte) -> Result<()> {
    let img = image::GrayImage::from_fn(m.width, m.height, |x, y| {
        image::Luma([(m.get(x, y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8])
    });
    img.save(path)
        .with_context(|| format!("writing {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
fn select(
    store: &ModelStore,
    rt: &Runtime,
    input: &Path,
    prompts: &[Prompt],
    out: &Path,
    overlay: Option<&Path>,
    tune: Tune,
    runs: usize,
) -> Result<()> {
    let img = load_image(input)?;
    let t = Instant::now();
    let seg = Segmenter::load(rt, store)
        .with_context(|| format!("loading MobileSAM from {}", store.dir().display()))?;
    eprintln!("load {:.0} ms: {}", ms(t), seg.provider());
    for r in seg.reports() {
        for f in &r.fallback {
            eprintln!("  fell back from {f}");
        }
    }
    if runs > 1 {
        let (enc, dec) = seg.bench(&img, prompts, runs)?;
        eprintln!("encoder ms per run: {}", list_ms(&enc));
        eprintln!("decoder ms per run: {}", list_ms(&dec));
    }
    let t = Instant::now();
    let emb = seg.embed(&img)?;
    let embed_ms = ms(t);
    let t = Instant::now();
    let pred = seg.predict(&emb, prompts)?;
    let decode_ms = ms(t);
    let opts = tune.apply(RefineOptions::selection(emb.cell()));
    let t = Instant::now();
    let m = pred.matte(&emb, opts);
    let refine_ms = ms(t);
    eprintln!(
        "{}×{}: embed {embed_ms:.0} ms, decode {decode_ms:.1} ms, upscale + refine {refine_ms:.0} ms; \
         mask {} (score {:.3}), covers {:.1} %",
        img.width,
        img.height,
        pred.index,
        pred.score,
        m.coverage(0, 0, m.width, m.height) * 100.0
    );
    save_mask(out, &m)?;
    println!("wrote {}", out.display());
    if let Some(path) = overlay {
        let mut tinted = img.clone();
        for (p, &a) in tinted.pixels.iter_mut().zip(&m.alpha) {
            // Outside the selection: darkened and tinted red.
            let k = 1.0 - a;
            p.r = p.r * (1.0 - 0.4 * k) + 0.25 * k * p.a;
            p.g *= 1.0 - 0.6 * k;
            p.b *= 1.0 - 0.6 * k;
        }
        lumenply_io::save_png(path, &tinted)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn list_ms(v: &[f64]) -> String {
    v.iter().map(|t| format!("{t:.1}")).collect::<Vec<_>>().join(", ")
}

#[allow(clippy::too_many_arguments)]
fn remove_bg(
    matter: &Matter,
    input: &Path,
    out: &Path,
    mask: Option<&Path>,
    tune: Tune,
    runs: usize,
) -> Result<()> {
    let img = load_image(input)?;
    for f in &matter.report().fallback {
        eprintln!("  fell back from {f}");
    }
    let mut times = Vec::new();
    for _ in 1..runs.max(1) {
        let t = Instant::now();
        matter.predict(&img)?;
        times.push(ms(t));
    }
    if !times.is_empty() {
        eprintln!("model ms per run: {}", list_ms(&times));
    }
    let scale = img.width.max(img.height) as f32 / 1024.0;
    let opts = tune.apply(RefineOptions::matte(scale));
    let t = Instant::now();
    let m = matter.matte_with(&img, opts)?;
    eprintln!(
        "{}×{}: model + upscale + refine {:.0} ms, subject covers {:.1} %",
        img.width,
        img.height,
        ms(t),
        m.coverage(0, 0, m.width, m.height) * 100.0
    );
    let mut cut = img.clone();
    for (p, &a) in cut.pixels.iter_mut().zip(&m.alpha) {
        *p = p.scale(a);
    }
    lumenply_io::save_png(out, &cut)?;
    println!("wrote {}", out.display());
    if let Some(path) = mask {
        save_mask(path, &m)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_parse_points_and_a_box() {
        let p = prompts(&["10,20".into(), "30.5, 40, neg".into()], Some("1,2,3,4")).unwrap();
        assert_eq!(
            p,
            vec![
                Prompt::Point {
                    x: 10.0,
                    y: 20.0,
                    positive: true
                },
                Prompt::Point {
                    x: 30.5,
                    y: 40.0,
                    positive: false
                },
                Prompt::Box {
                    x0: 1.0,
                    y0: 2.0,
                    x1: 3.0,
                    y1: 4.0
                },
            ]
        );
        assert!(prompts(&[], None).is_err());
        assert!(prompts(&["1".into()], None).is_err());
        assert!(prompts(&["1,2,maybe".into()], None).is_err());
        assert!(prompts(&[], Some("1,2,3")).is_err());
        assert_eq!(model("BiRefNet").unwrap(), ModelId::BiRefNetLite);
        assert!(model("sam2").is_err());
    }
}
