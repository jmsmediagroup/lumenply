//! `lumenply`: a headless front end for the engine. Useful for batch jobs, for
//! testing without a GUI, and for the CI performance gates.

use std::path::PathBuf;
use std::time::Instant;

mod batch;

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
    /// Lower a project or PSD to its edit graph (ADR 0025): print a
    /// summary, write the graph's JSON with --out, and with --check render
    /// it both ways and compare.
    Graph {
        project: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        check: bool,
    },
    /// Time the compositor on a synthetic document (the Phase 1 gate).
    Bench {
        #[arg(long, default_value_t = 4096)]
        size: u32,
        #[arg(long, default_value_t = 20)]
        layers: u32,
        #[arg(long, default_value_t = 5)]
        runs: u32,
        /// Blend every layer with Normal instead of cycling all modes.
        #[arg(long, default_value_t = false)]
        normal: bool,
    },
    /// Write a project (or the demo) as a layered Photoshop PSD.
    ExportPsd {
        /// An .lumen project, or omit for the demo document.
        project: Option<PathBuf>,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// List the supported blend modes (name, then Photoshop's label), in
    /// Photoshop's menu order with a blank line between its groups.
    Blends,
    /// Convert many files at once (Photoshop's Image Processor): images,
    /// camera RAW, PSD and projects in; PNG, JPEG, WebP, GIF or PDF out.
    ///
    /// e.g. `lumenply batch --out web --format jpeg --resize 2048 --auto *.CR3`
    Batch {
        /// Files to convert.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Output folder (created if missing).
        #[arg(short, long)]
        out: PathBuf,
        /// png, jpeg, webp (lossless), gif or pdf (one page at the document's resolution).
        #[arg(long, default_value = "jpeg")]
        format: String,
        /// JPEG quality, 1-100.
        #[arg(long, default_value_t = 90)]
        quality: u8,
        /// 50% (scale), 2048 (long edge) or 1920x1080 (fit inside); the last
        /// two never enlarge.
        #[arg(long)]
        resize: Option<String>,
        /// Camera RAW: set exposure, whites and blacks automatically.
        #[arg(long, default_value_t = false)]
        auto: bool,
        /// PNG/WebP: place on white instead of keeping transparency.
        #[arg(long, default_value_t = false)]
        flatten: bool,
        /// Play an action on each file first (before --resize): a built-in
        /// name ("Web export prep", "Black & white contrast", "Vintage
        /// fade") or a .json file holding one action.
        #[arg(long)]
        action: Option<String>,
        /// Look the --action name up in this JSON set (such as the app's
        /// actions.json in ~/.lumenply).
        #[arg(long)]
        action_file: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Batch {
            inputs,
            out,
            format,
            quality,
            resize,
            auto,
            flatten,
            action,
            action_file,
        } => {
            let action = action
                .as_deref()
                .map(|a| batch::resolve_action(a, action_file.as_deref()))
                .transpose()?;
            let resize = resize.as_deref().map(batch::Resize::parse).transpose()?;
            let flatten = flatten || matches!(format.to_ascii_lowercase().as_str(), "jpg" | "jpeg");
            let opts = batch::Options {
                out,
                format,
                quality: quality.clamp(1, 100),
                resize,
                auto,
                flatten,
                action,
            };
            let failed = batch::run(&inputs, &opts)?;
            if failed > 0 {
                bail!("{failed} file(s) could not be converted");
            }
            Ok(())
        }
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
            lumenply_io::resolution::save_png(&out, &flat, doc.resolution)?;
            println!("wrote {}", out.display());
            Ok(())
        }
        Cmd::Graph { project, out, check } => graph_cmd(&project, out.as_ref(), check),
        Cmd::Info { project } => {
            let doc = load_any(&project)?;
            let (w_in, h_in) = doc.print_size_inches();
            println!(
                "{}x{} px, {} layers (bottom to top):",
                doc.width,
                doc.height,
                doc.layer_count()
            );
            println!("resolution {} ppi ({w_in:.2} x {h_in:.2} in)", doc.resolution);
            print_tree(doc.layers(), 1);
            Ok(())
        }
        Cmd::Bench {
            size,
            layers,
            runs,
            normal,
        } => bench(size, layers, runs, normal),
        Cmd::Blends => {
            for (i, group) in BlendMode::GROUPS.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                for m in group.iter() {
                    println!("{:<14} {}", m.name(), m.label());
                }
            }
            Ok(())
        }
    }
}

/// Open a .lumen project or a .psd/.psb file.
fn graph_cmd(path: &PathBuf, out: Option<&PathBuf>, check: bool) -> Result<()> {
    use std::time::Instant;
    let mut doc = load_any(path)?;
    lumenply_render::fill::refresh_stale(&mut doc);
    let renderer = lumenply_graph::Renderer::new();
    let mut blobs = lumenply_graph::BlobStore::new();
    let t = Instant::now();
    let lowered = lumenply_graph::lower(&doc, &mut blobs, &renderer.hasher);
    let graph = lowered.graph;
    let lower_ms = t.elapsed().as_secs_f64() * 1e3;
    let json = graph.to_json();
    let mut by_type: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, n) in graph.nodes() {
        *by_type.entry(n.op.type_name()).or_default() += 1;
    }
    let types: Vec<String> = by_type.iter().map(|(t, n)| format!("{n} {t}")).collect();
    println!(
        "{}x{} px: {} nodes ({}), {} blobs, JSON {:.1} KB, lowered in {lower_ms:.1} ms",
        graph.width,
        graph.height,
        graph.len(),
        types.join(", "),
        blobs.len(),
        json.len() as f64 / 1024.0,
    );
    if let Some(out) = out {
        std::fs::write(out, &json)?;
        println!("wrote {}", out.display());
    }
    if check {
        let canvas = doc.canvas();
        let t = Instant::now();
        let reference = lumenply_render::composite(&doc);
        let tree_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let cold = renderer.render_canvas(&graph, &blobs);
        let cold_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let warm = renderer.render_canvas(&graph, &blobs);
        let warm_ms = t.elapsed().as_secs_f64() * 1e3;
        let mut worst = 0f32;
        for y in canvas.y..canvas.bottom() {
            for x in canvas.x..canvas.right() {
                let (p, q) = (reference.get_pixel(x, y), cold.get_pixel(x, y));
                for (a, b) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
                    worst = worst.max((a - b).abs());
                }
            }
        }
        let same_warm = canvas.tiles().iter().all(|c| warm.tile(*c) == cold.tile(*c));
        let stats = renderer.cache.stats();
        println!(
            "layer tree {tree_ms:.1} ms; graph cold {cold_ms:.1} ms, warm {warm_ms:.2} ms; max difference {worst:.2e}; \
             cache {} tiles, {:.1} MB",
            stats.tiles,
            stats.bytes as f64 / 1048576.0
        );
        if worst > 1e-5 || !same_warm {
            anyhow::bail!("the graph renders differently from the layer tree (max difference {worst})");
        }
    }
    Ok(())
}

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

/// One line about a text layer: string, font, size, colour, placement.
fn text_info(t: &lumenply_doc::TextLayer) -> String {
    let hex = |v: f32| (lumenply_io::linear_to_srgb_f(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
    let mut s = format!(
        "text: {:?} ({}px, font {:?}{}{}, #{:02x}{:02x}{:02x}, at {},{}, {:?}",
        t.text,
        t.size,
        lumenply_render::text::font_display_name(&t.font),
        if t.bold { " bold" } else { "" },
        if t.italic { " italic" } else { "" },
        hex(t.color[0]),
        hex(t.color[1]),
        hex(t.color[2]),
        t.x,
        t.y,
        t.align
    );
    if let Some([w, h]) = t.box_size {
        s += &format!(", box {w}x{h}");
    }
    if !t.runs.is_empty() {
        s += &format!(", {} runs", t.runs.len());
    }
    s + ")"
}

fn print_tree(layers: &[Layer], depth: usize) {
    for l in layers {
        let kind = match &l.content {
            LayerContent::Pixel(s) => format!("pixel, {} tiles", s.len()),
            LayerContent::Group(c) => format!("group, {} children", c.len()),
            LayerContent::Adjustment(a) => format!("adjustment: {}", a.name()),
            LayerContent::Filter(f) => format!("live filter: {}", f.name()),
            LayerContent::Text(t) => text_info(t),
            LayerContent::Smart(s) => format!("smart object, {} source tiles", s.source.len()),
            LayerContent::Fill(f) => format!("fill: {}", f.fill.name()),
            LayerContent::Shape(sh) => format!("shape: {}", sh.geometry.name()),
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

fn bench(size: u32, layers: u32, runs: u32, normal: bool) -> Result<()> {
    let mut doc = Document::new(size, size);
    for i in 0..layers {
        let id = doc.add_pixel_layer(format!("L{i}"));
        let shade = (i + 1) as f32 / layers as f32;
        let fill = Raster::filled(size, size, Rgba::from_straight(shade, 1.0 - shade, 0.5, 0.6));
        let layer = doc.layer_mut(id).unwrap();
        layer.blend = if normal {
            BlendMode::Normal
        } else {
            BlendMode::ALL[i as usize % BlendMode::ALL.len()]
        };
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
