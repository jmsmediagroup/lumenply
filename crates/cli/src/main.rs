//! `lumenply`: a headless front end for the engine. Useful for batch jobs, for
//! testing without a GUI, and for the CI performance gates.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use lumenply_core::commands::AddPixelLayer;
use lumenply_core::Editor;
use lumenply_doc::{BlendMode, Document, Layer, LayerContent};
use lumenply_io::project;
use lumenply_tiles::{Raster, Rgba};

#[derive(Parser)]
#[command(
    name = "lumenply",
    version,
    about = "Lumenply — free photo editor (headless CLI)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stack image files as layers and write the composite as PNG.
    ///
    /// Each --layer is PATH[:BLEND[:OPACITY]], bottom first, e.g.
    /// `--layer photo.jpg --layer grain.png:overlay:0.4`.
    Composite {
        #[arg(short, long)]
        out: PathBuf,
        #[arg(short, long = "layer", required = true)]
        layers: Vec<String>,
        /// Also save the layered document as a .lumen project.
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Paint a demo stroke, add a masked adjustment layer, and save.
    Paint {
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1024)]
        width: u32,
        #[arg(long, default_value_t = 768)]
        height: u32,
        /// Also save the layered document as a .lumen project.
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Render a .lumen project to PNG.
    Render {
        project: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Print the layer tree of a .lumen project.
    Info { project: PathBuf },
    /// Time the compositor on a synthetic document (the Phase 1 gate).
    Bench {
        #[arg(long, default_value_t = 4096)]
        size: u32,
        #[arg(long, default_value_t = 20)]
        layers: u32,
        #[arg(long, default_value_t = 5)]
        runs: u32,
    },
    /// Write a project (or the demo) as a layered Photoshop PSD.
    ExportPsd {
        /// An .lumen project, or omit for the demo document.
        project: Option<PathBuf>,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// List the supported blend modes.
    Blends,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Composite { out, layers, save } => composite(out, layers, save),
        Cmd::Paint {
            out,
            width,
            height,
            save,
        } => paint(out, width, height, save),
        Cmd::ExportPsd { project, out } => {
            let doc = match project {
                Some(p) => load_any(&p)?,
                None => lumenply_core::demo::build(1200, 800)?.doc().clone(),
            };
            let rep = lumenply_io::psd::save(&out, &doc)?;
            for w in &rep.warnings {
                eprintln!("warning: {w}");
            }
            println!("wrote {} ({} layers)", out.display(), doc.layer_count());
            Ok(())
        }
        Cmd::Render { project, out } => {
            let doc = load_any(&project)?;
            let t = Instant::now();
            let flat = lumenply_render::composite_raster(&doc);
            eprintln!(
                "rendered {} layers in {:.1} ms",
                doc.layer_count(),
                t.elapsed().as_secs_f64() * 1e3
            );
            lumenply_io::save_png(&out, &flat)?;
            println!("wrote {}", out.display());
            Ok(())
        }
        Cmd::Info { project } => {
            let doc = load_any(&project)?;
            println!(
                "{}x{} px, {} layers (bottom to top):",
                doc.width,
                doc.height,
                doc.layer_count()
            );
            print_tree(doc.layers(), 1);
            Ok(())
        }
        Cmd::Bench { size, layers, runs } => bench(size, layers, runs),
        Cmd::Blends => {
            for m in BlendMode::ALL {
                println!("{}", m.name());
            }
            Ok(())
        }
    }
}

/// Open a .lumen project or a .psd/.psb file.
fn load_any(path: &PathBuf) -> Result<Document> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".psd") || lower.ends_with(".psb") {
        let rep = lumenply_io::psd::load(path)?;
        for w in &rep.warnings {
            eprintln!("warning: {w}");
        }
        Ok(rep.value)
    } else {
        Ok(project::load(path)?)
    }
}

fn print_tree(layers: &[Layer], depth: usize) {
    for l in layers {
        let kind = match &l.content {
            LayerContent::Pixel(s) => format!("pixel, {} tiles", s.len()),
            LayerContent::Group(c) => format!("group, {} children", c.len()),
            LayerContent::Adjustment(a) => format!("adjustment: {}", a.name()),
            LayerContent::Filter(f) => format!("live filter: {}", f.name()),
            LayerContent::Text(t) => format!("text: {:?} ({}px)", t.text, t.size),
        };
        let mask = l
            .mask
            .as_ref()
            .map_or(String::new(), |m| format!(", mask ({} tiles)", m.tiles.len()));
        println!(
            "{}#{} {:<16} {:>4.0}% {:<10} {}{}{}",
            "  ".repeat(depth),
            l.id,
            l.name,
            l.opacity * 100.0,
            l.blend.name(),
            kind,
            mask,
            if l.visible { "" } else { " (hidden)" }
        );
        if let Some(c) = l.children() {
            print_tree(c, depth + 1);
        }
    }
}

fn composite(out: PathBuf, specs: Vec<String>, save: Option<PathBuf>) -> Result<()> {
    let mut editor: Option<Editor> = None;
    for spec in &specs {
        let mut parts = spec.splitn(3, ':');
        let path = parts.next().unwrap();
        let blend: BlendMode = match parts.next() {
            Some(b) => b.parse().map_err(anyhow::Error::msg)?,
            None => BlendMode::Normal,
        };
        let opacity: f32 = match parts.next() {
            Some(o) => o.parse().with_context(|| format!("bad opacity in '{spec}'"))?,
            None => 1.0,
        };
        let raster = lumenply_io::load(path).with_context(|| format!("loading {path}"))?;
        let ed = editor.get_or_insert_with(|| Editor::new(Document::new(raster.width, raster.height)));
        let mut cmd = AddPixelLayer::from_raster(path, raster, 0, 0);
        cmd.blend = blend;
        cmd.opacity = opacity;
        ed.execute(&cmd)?;
    }
    let Some(ed) = editor else {
        bail!("no layers given")
    };

    let t = Instant::now();
    let flat = lumenply_render::composite_raster(ed.doc());
    eprintln!(
        "composited {} layers at {}x{} in {:.1} ms",
        specs.len(),
        flat.width,
        flat.height,
        t.elapsed().as_secs_f64() * 1e3
    );
    lumenply_io::save_png(&out, &flat)?;
    println!("wrote {}", out.display());
    if let Some(p) = save {
        project::save(&p, ed.doc())?;
        println!("saved {}", p.display());
    }
    Ok(())
}

fn paint(out: PathBuf, width: u32, height: u32, save: Option<PathBuf>) -> Result<()> {
    let t = Instant::now();
    let ed = lumenply_core::demo::build(width, height)?;
    eprintln!("built demo document in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    lumenply_io::save_png(&out, &lumenply_render::composite_raster(ed.doc()))?;
    println!("wrote {} (history: {:?})", out.display(), ed.history());
    if let Some(p) = save {
        project::save(&p, ed.doc())?;
        println!("saved {}", p.display());
    }
    Ok(())
}

fn bench(size: u32, layers: u32, runs: u32) -> Result<()> {
    let mut doc = Document::new(size, size);
    for i in 0..layers {
        let id = doc.add_pixel_layer(format!("L{i}"));
        let shade = (i + 1) as f32 / layers as f32;
        let fill = Raster::filled(size, size, Rgba::from_straight(shade, 1.0 - shade, 0.5, 0.6));
        let layer = doc.layer_mut(id).unwrap();
        layer.blend = BlendMode::ALL[i as usize % BlendMode::ALL.len()];
        *layer.pixels_mut().unwrap() = lumenply_tiles::TileStore::from_raster(&fill, 0, 0);
    }
    lumenply_core::compact_storage(&mut doc);
    let mp = (size as f64 * size as f64) / 1e6;
    println!(
        "{layers} full {size}x{size} layers ({mp:.1} MP), mixed blend modes, {} threads, {:.0} MB of layer pixels",
        rayon_threads(),
        lumenply_core::storage_bytes(&doc) as f64 / 1e6
    );
    let mut best = f64::MAX;
    for run in 1..=runs {
        let t = Instant::now();
        let out = lumenply_render::composite(&doc);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        println!("run {run}: {ms:.1} ms ({} tiles)", out.len());
    }
    println!(
        "best: {best:.1} ms  →  {:.1} MP·layers/s",
        mp * layers as f64 / (best / 1e3)
    );
    Ok(())
}

fn rayon_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}
