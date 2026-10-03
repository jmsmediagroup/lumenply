//! Projects on disk for the app: `.lumen` format 3 (ADR 0026), the edit
//! graph with the pixels its operations name. Opening still reads layer-tree
//! projects (format 1 and legacy `.nge`); saving, autosave and crash
//! recovery always write format 3.

use std::path::Path;
use std::sync::Arc;

use lumenply_graph::{BlobStore, Graph};
use lumenply_io::graph_project::{self, GRAPH_FORMAT_VERSION};

use super::*;

/// A project opened for editing.
pub(crate) struct Opened {
    pub editor: Editor,
    /// What was lost or repaired while loading (damaged blobs, hints).
    pub warnings: Vec<String>,
}

/// Open a project of any format: a graph project through
/// [`Editor::from_graph_project`] (its render hints seeded into the
/// editor's cache), a layer-tree project as before.
pub(crate) fn open_project(path: &Path) -> Result<Opened, String> {
    if graph_project::is_graph_project(path) {
        let p = graph_project::load_graph_project(path).map_err(|e| e.to_string())?;
        let editor = Editor::from_graph_project(&p.graph, &p.meta, p.blobs).map_err(|e| e.to_string())?;
        p.hints.seed(&editor.renderer().cache);
        return Ok(Opened {
            editor,
            warnings: p.warnings,
        });
    }
    let doc = project::load(path).map_err(|e| e.to_string())?;
    Ok(Opened {
        editor: Editor::new(doc),
        warnings: Vec::new(),
    })
}

/// Whether `path` holds a project older than format 3 (a layer-tree
/// `.lumen` or `.nge`). False for a missing file or anything else.
pub(crate) fn is_layer_tree_project(path: &Path) -> bool {
    graph_project::project_version(path).is_ok_and(|v| v < GRAPH_FORMAT_VERSION)
}

/// An editor's current version as a project file holds it, taken on the UI
/// thread and written anywhere (the autosave writes it on another).
pub(crate) struct ProjectSnapshot {
    graph: Arc<Graph>,
    meta: serde_json::Value,
    blobs: BlobStore,
}

impl ProjectSnapshot {
    pub(crate) fn of(editor: &Editor) -> Self {
        let (graph, meta, blobs) = editor.graph_project();
        ProjectSnapshot { graph, meta, blobs }
    }

    /// Write it as a format 3 project, atomically. No render hints: the
    /// output's tiles cost about the flattened image again in the file,
    /// and replaying a graph is cheap until its history grows long.
    pub(crate) fn save(&self, path: &Path) -> Result<graph_project::SaveStats, String> {
        graph_project::save_graph_project(path, &self.graph, &self.blobs, &self.meta, None)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::{Rgba, TileStore};

    /// A layer-tree document with paint, a mask, an adjustment, text and a
    /// saved selection: what a format 1 project holds.
    fn v1_document() -> Document {
        let mut doc = Document::new(300, 200);
        let bg = doc.add_pixel_layer("Background");
        let mut r = Raster::new(300, 200);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let v = (i % 251) as f32 / 251.0;
            *p = Rgba::from_straight(v, 1.0 - v, 0.5, 1.0);
        }
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
        let adj = doc.add_adjustment(Adjustment::Invert);
        let mut m = lumenply_doc::Mask::hide_all();
        m.set_value(10, 10, 1.0);
        doc.layer_mut(adj).unwrap().mask = Some(m);
        doc.layer_mut(adj).unwrap().opacity = 0.5;
        let id = doc.alloc_id();
        let mut t = TextLayer::new("Hi", 20.0, 60.0, 40.0, [1.0, 1.0, 1.0, 1.0]);
        lumenply_render::text::refresh_cache(&mut t);
        doc.add_layer(Layer::text(id, t));
        doc.resolution = 240.0;
        doc
    }

    #[test]
    fn a_format_1_project_saved_from_the_app_reopens_as_format_3_unchanged() {
        let dir = std::env::temp_dir().join(format!("lumenply-project-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = dir.join("old.lumen");
        project::save(&v1, &v1_document()).unwrap();
        assert!(is_layer_tree_project(&v1));

        let opened = open_project(&v1).unwrap();
        assert!(opened.warnings.is_empty());
        let reference = lumenply_render::composite_raster(opened.editor.doc());
        let v3 = dir.join("new.lumen");
        let stats = ProjectSnapshot::of(&opened.editor).save(&v3).unwrap();
        assert!(stats.blobs >= 2, "{stats:?}");
        assert_eq!(graph_project::project_version(&v3).unwrap(), 3);
        assert!(!is_layer_tree_project(&v3));

        let back = open_project(&v3).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let (a, b) = (opened.editor.doc(), back.editor.doc());
        assert_eq!((b.width, b.height, b.resolution), (300, 200, 240.0));
        let names = |d: &Document| d.layers().iter().map(|l| l.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(b), names(a));
        assert_eq!(b.layers()[1].opacity, 0.5);
        assert_eq!(b.layers()[1].mask.as_ref().unwrap().value(10, 10), 1.0);
        // The same image, bit for bit, from the document and from the graph.
        assert_eq!(lumenply_render::composite_raster(b), reference);
        let ed = &back.editor;
        let shown = ed.renderer().render_canvas(ed.graph(), ed.blobs());
        assert_eq!(shown.to_raster(b.canvas()), reference);
        // And again: a format 3 project saves and reopens as itself.
        let again = dir.join("again.lumen");
        ProjectSnapshot::of(&back.editor).save(&again).unwrap();
        let third = open_project(&again).unwrap();
        // The same content: equal output keys (a text layer's op is lowered
        // again from its parameters on opening, under a new node id).
        let output_key = |ed: &Editor| {
            let g = ed.graph();
            let o = g.output.unwrap();
            ed.renderer().keys(g, o)[&o]
        };
        assert_eq!(output_key(&third.editor), output_key(&back.editor));
        assert_eq!(lumenply_render::composite_raster(third.editor.doc()), reference);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_app_saves_backs_up_and_recovers_format_3() {
        let dir = std::env::temp_dir().join(format!("lumenply-project-io-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("poster.lumen");
        project::save(&file, &v1_document()).unwrap();
        let name = file.to_string_lossy().into_owned();

        let mut app = crate::a11y_tests::launch(&[]);
        app.open_path(&name);
        assert_eq!(app.status, format!("Opened {name}"));
        let adj = app.editor.doc().layers()[1].id;
        app.run(&SetOpacity {
            layer: adj,
            opacity: 0.25,
        });
        let shown = lumenply_render::composite_raster(app.editor.doc());

        // An unsaved document is backed up as format 3 and recovers as it was.
        session::remove_autosave();
        session::write_backups(&app.unsaved_docs());
        let backups = session::autosave_backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(graph_project::project_version(&backups[0].0).unwrap(), 3);
        assert_eq!(backups[0].1.as_deref(), Some(file.as_path()));
        let mut other = crate::a11y_tests::launch(&[]);
        other.recover_autosave();
        assert_eq!(other.status, "Recovered the autosaved document");
        assert_eq!(lumenply_render::composite_raster(other.editor.doc()), shown);
        session::remove_autosave();

        // Saving replaces the layer-tree file with format 3, once said.
        app.save_path(&name);
        assert_eq!(app.status, format!("Saved {name} in the new project format"));
        assert_eq!(graph_project::project_version(&file).unwrap(), 3);
        app.save_path(&name);
        assert_eq!(app.status, format!("Saved {name}"));

        // Reopened elsewhere: the same document.
        let mut again = crate::a11y_tests::launch(&[]);
        again.open_path(&name);
        assert_eq!(again.status, format!("Opened {name}"));
        assert_eq!(again.editor.doc().layers()[1].opacity, 0.25);
        assert_eq!(lumenply_render::composite_raster(again.editor.doc()), shown);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
