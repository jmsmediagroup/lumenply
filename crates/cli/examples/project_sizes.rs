//! File size and save/load time of a document as a layer-tree project
//! (format 1) and as a graph project (format 3, ADR 0026).
//!
//! ```sh
//! cargo run --release -p lumenply-cli --example project_sizes -- [--demo] [FILE.psd ...]
//! ```
//!
//! Each document is measured twice: as loaded (PSD pixels arrive as f32
//! tiles) and compacted to 16-bit, as the editor keeps tiles at rest after
//! an edit (ADR 0004). Files go to a temporary folder and are deleted.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use lumenply_doc::Document;
use lumenply_graph::{RenderHints, Renderer};
use lumenply_io::{graph_project, project};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Best of three runs of `f`, in milliseconds, with its last result.
fn best<T>(mut f: impl FnMut() -> Result<T>) -> Result<(f64, T)> {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..3 {
        let t = Instant::now();
        let v = f()?;
        best = best.min(ms(t));
        out = Some(v);
    }
    Ok((best, out.unwrap()))
}

fn kb(path: &Path) -> f64 {
    std::fs::metadata(path).map_or(0.0, |m| m.len() as f64 / 1024.0)
}

fn measure(name: &str, doc: &Document, dir: &Path) -> Result<()> {
    let v1 = dir.join("tree.lumen");
    let v3 = dir.join("graph.lumen");
    let v3h = dir.join("graph-hints.lumen");
    let (v1_save, ()) = best(|| Ok(project::save(&v1, doc)?))?;
    let (v1_load, _) = best(|| Ok(project::load(&v1)?))?;

    let t = Instant::now();
    let p = graph_project::document_to_graph(doc, project::FORMAT_VERSION);
    let lower = ms(t);
    let (v3_save, stats) = best(|| {
        Ok(graph_project::save_graph_project(
            &v3, &p.graph, &p.blobs, &p.meta, None,
        )?)
    })?;
    let (v3_load, back) = best(|| Ok(graph_project::load_graph_project(&v3)?))?;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let r = Renderer::new();
    let out: Vec<_> = p.graph.output.into_iter().collect();
    let hints = RenderHints::capture(&r, &p.graph, &p.blobs, &out);
    graph_project::save_graph_project(&v3h, &p.graph, &p.blobs, &p.meta, Some(&hints))?;
    let (open_hinted, _) = best(|| {
        let p = graph_project::load_graph_project(&v3h)?;
        let r = Renderer::new();
        p.hints.seed(&r.cache);
        Ok(r.render_canvas(&p.graph, &p.blobs))
    })?;
    let (open_cold, _) = best(|| {
        let p = graph_project::load_graph_project(&v3)?;
        Ok(Renderer::new().render_canvas(&p.graph, &p.blobs))
    })?;
    println!(
        "{name:<34} {:>5}x{:<5} {:>3} layers | v1 {:>9.1} KB save {:>7.1} ms load {:>7.1} ms | \
         v3 {:>9.1} KB ({} blobs, {} tiles) lower {:>6.1} ms save {:>7.1} ms load {:>7.1} ms | \
         +hints {:>9.1} KB | open+render cold {:>7.1} ms, hinted {:>7.1} ms",
        doc.width,
        doc.height,
        doc.layer_count(),
        kb(&v1),
        v1_save,
        v1_load,
        kb(&v3),
        stats.blobs,
        stats.blob_tiles,
        lower,
        v3_save,
        v3_load,
        kb(&v3h),
        open_cold,
        open_hinted,
    );
    Ok(())
}

fn main() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("lumenply-project-sizes-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let mut docs: Vec<(String, Document)> = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--demo" {
            docs.push((
                "demo 1200x800".into(),
                lumenply_core::demo::build(1200, 800)?.doc().clone(),
            ));
            continue;
        }
        let path = PathBuf::from(&arg);
        let doc = lumenply_io::psd::load(&path)?.value;
        let name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or(arg.clone());
        let name = match path.parent().and_then(|p| p.file_name()) {
            Some(parent) => format!("{}/{name}", parent.to_string_lossy()),
            None => name,
        };
        docs.push((name, doc));
    }
    for (name, mut doc) in docs {
        // Owned outright (not shared with `docs`), so compaction converts every tile.
        lumenply_render::fill::refresh_stale(&mut doc);
        measure(&format!("{name} (as loaded)"), &doc, &dir)?;
        lumenply_core::compact_storage(&mut doc);
        measure(&format!("{name} (16-bit)"), &doc, &dir)?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
