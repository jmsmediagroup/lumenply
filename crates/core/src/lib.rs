//! The app core: every edit is a [`Command`] applied through an [`Editor`],
//! which keeps undo/redo history as versions of the edit graph (ADR 0025).
//!
//! Each version is a [`Graph`] plus the document state that isn't pixels
//! ([`DocState`]); the document the app reads is its projection. Versions
//! share every node an edit didn't change, and pixels live in
//! content-addressed blobs whose tiles are shared copy-on-write, so an undo
//! step costs only the tiles the command actually touched. Tools, scripts
//! and plugins all go through this one path, so undo, macros and the
//! headless CLI behave identically.

pub mod actions;
pub mod adjust_cmds;
pub mod ai_masks;
pub mod align;
pub mod brush_tip;
pub mod canvas_ops;
pub mod channels;
pub mod commands;
mod content_aware;
pub mod content_aware_scale;
pub mod crop;
pub mod demo;
#[cfg(test)]
mod develop_filter_tests;
mod erasers;
pub mod everyday;
pub mod fill_cmds;
pub mod fill_opacity;
pub mod gradient_tool;
pub mod guides;
pub mod layer_ops;
pub mod liquify;
pub mod locks;
pub mod paste;
pub mod path_ops;
pub mod pattern_cmds;
pub mod perspective_crop;
pub mod puppet;
pub mod quick_select;
pub mod refine;
pub mod resolution;
mod retouch;
mod retouch_brush;
mod select_ops;
pub mod shape_cmds;
pub mod smart_contents;
pub mod smart_filter_cmds;
pub mod snap;

#[cfg(test)]
mod graph_history_tests;

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use lumenply_doc::{Document, LayerId};
use lumenply_graph::sync::{self as graph_sync, Base, BlobRefMemo};
pub use lumenply_graph::sync::{ContentEdit, DocState, EditInput};
use lumenply_graph::{BlobStore, Graph, History, Renderer};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("no layer with id {0}")]
    NoLayer(LayerId),
    #[error("layer {0} is not a pixel layer")]
    NotPixel(LayerId),
    #[error("layer {0} is not a group")]
    NotGroup(LayerId),
    #[error("{0}")]
    Invalid(String),
}

pub type EditResult<T = ()> = Result<T, EditError>;

/// How a command moves its target layer, so layer locks can judge it
/// (see [`locks::enforce`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Doesn't move the layer.
    #[default]
    None,
    /// Shifts it by whole pixels: only a position lock refuses this.
    Translate,
    /// Scales, rotates, flips, distorts or warps it: position, pixel and
    /// transparency locks all refuse this.
    Reshape,
}

/// An undoable edit. Commands must be deterministic: the editor records only
/// the resulting document, not the command itself.
pub trait Command {
    /// Short label for the history panel, e.g. "Paint stroke".
    fn label(&self) -> String;
    fn apply(&self, doc: &mut Document) -> EditResult;
    /// Canvas area whose appearance may change, computed against the
    /// document *before* the command runs. `None` means "anything".
    /// Front ends use it to recomposite only what moved.
    fn affected(&self, _doc: &Document) -> Option<lumenply_tiles::Rect> {
        None
    }
    /// The single layer whose data or properties this command changes, if it
    /// can name one. `None` means the change may touch anything (structure,
    /// several layers, the canvas). Front ends use it to keep composite
    /// caches warm across consecutive edits to the same layer.
    fn target_layer(&self) -> Option<LayerId> {
        None
    }
    /// How this command moves [`Command::target_layer`]; layer locks use
    /// it to tell a move from a pixel edit.
    fn motion(&self) -> Motion {
        Motion::None
    }
    /// The edit as an operation on one layer's content in the edit graph,
    /// for commands ported to it (ADR 0025). When this returns `Some`, the
    /// editor appends the op to the layer's content chain (and, outside
    /// float mode, a `compact` node, so the layer rests at 16 bits as after
    /// any command) and takes the layer's pixels from the graph instead of
    /// calling [`Command::apply`]. The op must therefore paint exactly what
    /// `apply` would, in a case where `apply` can't fail. Pixel data the op
    /// names (a brush tip) goes into `blobs`; pixels feeding its other
    /// ports (a selection) are [`EditInput`]s. Layer locks are still
    /// enforced on the result. When the graph doesn't hold the layer's
    /// pixels (smart filters, say) the editor runs `apply` after all.
    fn graph_edit(&self, _doc: &Document, _blobs: &mut BlobStore) -> Option<ContentEdit> {
        None
    }
}

/// What the editor keeps with each graph version besides the graph.
struct Step {
    /// The document state that isn't in the graph.
    state: Arc<DocState>,
    /// Canvas area the step that made this version changed (see
    /// [`Command::affected`]); undoing or redoing the step dirties exactly
    /// this area. `None` means "anything".
    affected: Option<lumenply_tiles::Rect>,
    /// Estimated bytes of pixel data the version before this one keeps
    /// alive that this one doesn't: what dropping that version (this
    /// step's undo) frees.
    bytes: usize,
    /// The blobs this version's graph names, found once when it is made:
    /// dropping unused blobs is then a union of these, however long the
    /// graph's chains grow.
    blob_ids: HashSet<lumenply_graph::BlobId>,
    /// The document this version stands for, projected when
    /// [`Editor::state`] first asks for it.
    doc: OnceLock<Document>,
    /// This version's identity (see [`Editor::version`]).
    serial: u64,
}

/// Serials for [`Step::serial`], unique across every editor in the process.
static NEXT_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Step {
    fn new(
        state: DocState,
        blob_ids: HashSet<lumenply_graph::BlobId>,
        affected: Option<lumenply_tiles::Rect>,
        bytes: usize,
    ) -> Self {
        Step {
            state: Arc::new(state),
            affected,
            bytes,
            blob_ids,
            doc: OnceLock::new(),
            serial: NEXT_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

fn union_opt(
    a: Option<lumenply_tiles::Rect>,
    b: Option<lumenply_tiles::Rect>,
) -> Option<lumenply_tiles::Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(&b)),
        _ => None,
    }
}

/// Put every tile the document owns outright into compact 16-bit storage.
/// Tiles shared with history snapshots are left as they are, so this costs
/// only the tiles a command actually changed. Float-mode documents skip
/// compaction entirely, so HDR values survive every edit (ADR 0004).
pub fn compact_storage(doc: &mut Document) {
    if doc.float_mode {
        return;
    }
    doc.for_each_layer_mut(|l| {
        match &mut l.content {
            lumenply_doc::LayerContent::Pixel(store) => store.compact(),
            lumenply_doc::LayerContent::Text(t) => {
                if let Some(c) = t.cache.as_mut() {
                    c.compact();
                }
            }
            lumenply_doc::LayerContent::Smart(s) => {
                s.source.compact();
                if let Some(c) = s.cache.as_mut() {
                    c.compact();
                }
            }
            lumenply_doc::LayerContent::Fill(f) => {
                if let Some(c) = f.cache.as_mut() {
                    c.compact();
                }
            }
            lumenply_doc::LayerContent::Shape(s) => {
                if let Some(c) = s.cache.as_mut() {
                    c.compact();
                }
            }
            _ => {}
        }
        if let Some(m) = l.mask.as_mut() {
            m.tiles.compact();
        }
    });
}

/// Bytes of pixel data referenced by all layers and masks.
pub fn storage_bytes(doc: &Document) -> usize {
    let mut total = 0;
    doc.for_each_layer(|l| {
        if let Some(s) = l.content_store() {
            total += s.byte_size();
        }
        if let Some(c) = &l.smart_filters.cache {
            total += c.store.byte_size();
        }
        if let Some(m) = &l.mask {
            total += m.tiles.byte_size();
        }
    });
    total
}

/// Owns the live document and its history.
///
/// The history is a list of graph versions (ADR 0025): each successful edit
/// commits a new [`Graph`] plus the [`DocState`] beside it, and undo, redo,
/// jumps and coalescing move over those versions. The document the app
/// reads ([`Editor::doc`]) is the projection of the current version. A
/// command runs on a copy of it (or, when it offers a
/// [`Command::graph_edit`], on the graph itself) and the result is synced
/// into the next version, which reuses every node of the layers it didn't
/// change.
pub struct Editor {
    doc: Document,
    history: History<Step>,
    blobs: BlobStore,
    /// The blobs each graph node names, memoised (see [`BlobRefMemo`]).
    blob_refs: BlobRefMemo,
    renderer: Arc<Renderer>,
    pub history_limit: usize,
    /// Rough cap on the bytes the undo stack may keep alive; the oldest
    /// steps are dropped first. At least one step is always kept.
    pub history_memory_limit: usize,
    /// Key of the open coalescing run, if any (see [`Editor::execute_coalescing`]).
    coalesce_key: Option<String>,
    /// Area changed by the last successful edit/undo/redo; `None` = whole canvas.
    last_affected: Option<lumenply_tiles::Rect>,
    /// Layer targeted by the last successful edit (see [`Command::target_layer`]).
    last_target: Option<LayerId>,
    /// Time the last edit spent on the graph (see [`Editor::last_sync_time`]).
    last_sync: std::time::Duration,
}

/// The next version of a document: the document itself and its graph
/// version, not yet committed.
struct Prepared {
    doc: Document,
    graph: Graph,
    state: DocState,
    /// Time spent on the graph so far (a native edit, the sync).
    graph_time: std::time::Duration,
}

impl Editor {
    pub fn new(mut doc: Document) -> Self {
        lumenply_render::fill::refresh_stale(&mut doc);
        let renderer = Arc::new(Renderer::new());
        let mut blobs = BlobStore::new();
        let (graph, state) = graph_sync::sync(None, &doc, &mut blobs, &renderer.hasher);
        Self::starting_at(doc, graph, state, blobs, renderer)
    }

    /// An editor whose history starts at one version.
    fn starting_at(
        doc: Document,
        graph: Graph,
        state: DocState,
        blobs: BlobStore,
        renderer: Arc<Renderer>,
    ) -> Self {
        let blob_refs = BlobRefMemo::default();
        let ids = blob_refs.graph(&graph);
        Editor {
            doc,
            history: History::with_payload(graph, Step::new(state, ids, None, 0)),
            blobs,
            blob_refs,
            renderer,
            history_limit: 100,
            history_memory_limit: 1 << 30, // 1 GiB
            coalesce_key: None,
            last_affected: None,
            last_target: None,
            last_sync: std::time::Duration::ZERO,
        }
    }

    /// Open a version read from a project file (format 3: its graph, its
    /// `meta.json` and the blobs they name), ready to edit: derived pixels
    /// are rebuilt, and pixel layers keep their chains of strokes and
    /// moves. The history starts there.
    pub fn from_graph_project(
        graph: &Graph,
        meta: &serde_json::Value,
        mut blobs: BlobStore,
    ) -> Result<Self, lumenply_graph::meta::MetaError> {
        let renderer = Arc::new(Renderer::new());
        let (graph, state, doc) = lumenply_graph::meta::open(graph, meta, &mut blobs, &renderer)?;
        Ok(Self::starting_at(doc, graph, state, blobs, renderer))
    }

    /// The current version as a project file holds it (format 3): the
    /// graph, its `meta.json` and the blobs they name (the store may hold
    /// more; a save writes only what is named).
    pub fn graph_project(&self) -> (Arc<Graph>, serde_json::Value, BlobStore) {
        let mut blobs = self.blobs.clone();
        let graph = self.history.current_arc();
        let meta = self
            .doc_state()
            .to_meta(&graph, &mut blobs, &self.renderer.hasher);
        (graph, meta, blobs)
    }

    /// Time the last successful edit spent keeping the graph in step: a
    /// native graph edit, syncing the document into the new version,
    /// memory accounting and dropping unused blobs. What the graph history
    /// costs on top of running the command itself.
    pub fn last_sync_time(&self) -> std::time::Duration {
        self.last_sync
    }

    /// Canvas area touched by the most recent change (see [`Command::affected`]).
    pub fn last_affected(&self) -> Option<lumenply_tiles::Rect> {
        self.last_affected
    }

    /// Layer the most recent edit targeted, when it could name one. Reset
    /// to `None` by undo, redo and jumps.
    pub fn last_target_layer(&self) -> Option<LayerId> {
        self.last_target
    }

    /// Estimated bytes the undo stack keeps alive beyond the live document.
    pub fn history_bytes(&self) -> usize {
        let v = self.history.versions();
        v[1..=self.history.cursor()].iter().map(|v| v.payload.bytes).sum()
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// The current version of the edit graph: the document as operations.
    pub fn graph(&self) -> &Graph {
        self.history.current()
    }

    /// The current graph version, shared.
    pub fn graph_arc(&self) -> Arc<Graph> {
        self.history.current_arc()
    }

    /// What the current version holds besides its graph.
    pub fn doc_state(&self) -> &DocState {
        &self.history.current_version().payload.state
    }

    /// The pixel data the graph versions name.
    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    /// The renderer and tile cache shared by every version of this
    /// document: content keys don't change across versions for what an
    /// edit didn't touch, so its cache keeps hitting across edits and undo.
    pub fn renderer(&self) -> &Arc<Renderer> {
        &self.renderer
    }

    /// Run `cmd` against the current document and turn the result into the
    /// next graph version, changing nothing in the editor yet. `fold` lets
    /// a native edit merge into the op on top of the layer's chain (the
    /// previous tick of the same coalescing run).
    fn prepare(&mut self, cmd: &dyn Command, fold: bool) -> EditResult<Prepared> {
        let current = Base {
            graph: self.history.current(),
            state: &self.history.current_version().payload.state,
            doc: &self.doc,
        };
        let started = std::time::Instant::now();
        let edit = cmd.graph_edit(&self.doc, &mut self.blobs);
        let edited =
            edit.and_then(|e| graph_sync::content_edit(current, &e, fold, &mut self.blobs, &self.renderer));
        let mut graph_time = started.elapsed();
        let mut next = match &edited {
            Some(e) => e.doc.clone(),
            None => {
                let mut next = self.doc.clone();
                cmd.apply(&mut next)?;
                next
            }
        };
        locks::enforce(&self.doc, &mut next, cmd)?;
        // Fill layers follow canvas size changes (crop, resize, rotate).
        lumenply_render::fill::refresh_stale(&mut next);
        compact_storage(&mut next);
        let base = edited.as_ref().map_or(current, |e| e.base());
        let started = std::time::Instant::now();
        let (graph, state) = graph_sync::sync(Some(base), &next, &mut self.blobs, &self.renderer.hasher);
        graph_time += started.elapsed();
        Ok(Prepared {
            doc: next,
            graph,
            state,
            graph_time,
        })
    }

    /// Bytes version `i` keeps alive that `next` (its blob ids and state)
    /// doesn't.
    fn released(&self, i: usize, next: (&HashSet<lumenply_graph::BlobId>, &DocState)) -> usize {
        let v = &self.history.versions()[i].payload;
        graph_sync::released_bytes((&v.blob_ids, &v.state), next, &self.blobs)
    }

    /// Drop the blobs no kept version names any more, and the memoised
    /// hashes of nodes and tiles nothing uses.
    fn collect_garbage(&mut self) {
        let sets = self.history.versions().iter().map(|v| &v.payload.blob_ids);
        graph_sync::collect_blobs(&mut self.blobs, sets);
        self.blob_refs.prune();
        self.renderer.prune();
    }

    /// The current version's document.
    fn project_current(&mut self) -> Document {
        let v = self.history.current_version_mut();
        // A projection made for `state` serves again.
        if let Some(doc) = v.payload.doc.take() {
            return doc;
        }
        graph_sync::project(&v.graph, &v.payload.state, &self.blobs, &self.renderer)
    }

    /// Apply a command, recording an undo step. On error the document is
    /// left untouched.
    pub fn execute(&mut self, cmd: &dyn Command) -> EditResult {
        self.coalesce_key = None;
        self.push_command(cmd)
    }

    /// Like [`Editor::execute`], but consecutive calls with the same `key`
    /// share one undo step. Use for slider drags: every tick updates the
    /// document, yet the whole drag undoes in one go. Call
    /// [`Editor::end_coalescing`] when the drag ends.
    pub fn execute_coalescing(&mut self, cmd: &dyn Command, key: &str) -> EditResult {
        if self.coalesce_key.as_deref() == Some(key) {
            let next = self.prepare(cmd, true)?;
            self.last_affected = smart_filter_cmds::widen_affected(&next.doc, cmd.affected(&self.doc));
            self.last_target = cmd.target_layer();
            let started = std::time::Instant::now();
            let ids = self.blob_refs.graph(&next.graph);
            // The whole drag undoes in one go, so its undo step covers
            // every tick so far, and its memory estimate follows the
            // moving document.
            let cursor = self.history.cursor();
            let (affected, bytes) = match cursor {
                0 => (None, 0),
                _ => (
                    union_opt(
                        self.history.current_version().payload.affected,
                        self.last_affected,
                    ),
                    self.released(cursor - 1, (&ids, &next.state)),
                ),
            };
            let v = self.history.current_version_mut();
            v.graph = Arc::new(next.graph);
            v.payload = Step::new(next.state, ids, affected, bytes);
            self.collect_garbage();
            self.last_sync = next.graph_time + started.elapsed();
            self.doc = next.doc;
            Ok(())
        } else {
            self.push_command(cmd)?;
            self.coalesce_key = Some(key.to_string());
            Ok(())
        }
    }

    /// Close the current coalescing run; the next edit starts a new undo step.
    pub fn end_coalescing(&mut self) {
        self.coalesce_key = None;
    }

    /// Is `key` the open coalescing run (its edits still share the last
    /// undo step)?
    pub fn coalescing(&self, key: &str) -> bool {
        self.coalesce_key.as_deref() == Some(key)
    }

    /// Abandon the open coalescing run `key`: put the document back as it
    /// was before the run and forget the run's undo step (it leaves no redo
    /// entry either). For edit sessions that end with nothing worth
    /// keeping, such as new text closed before anything was typed. Returns
    /// false, changing nothing, when `key` is not the open run.
    pub fn discard_coalescing(&mut self, key: &str) -> bool {
        if !self.coalescing(key) || self.history.cursor() == 0 {
            return false;
        }
        let dropped = self.history.rollback(1);
        self.coalesce_key = None;
        self.last_target = None;
        self.last_affected = dropped.first().and_then(|v| v.payload.affected);
        self.doc = self.project_current();
        self.collect_garbage();
        true
    }

    fn push_command(&mut self, cmd: &dyn Command) -> EditResult {
        let next = self.prepare(cmd, false)?;
        self.last_affected = smart_filter_cmds::widen_affected(&next.doc, cmd.affected(&self.doc));
        self.last_target = cmd.target_layer();
        let started = std::time::Instant::now();
        let ids = self.blob_refs.graph(&next.graph);
        let bytes = self.released(self.history.cursor(), (&ids, &next.state));
        let before = self.history.versions().len();
        self.history.limit = self.history_limit;
        self.history.commit_with(
            next.graph,
            cmd.label(),
            Step::new(next.state, ids, self.last_affected, bytes),
        );
        while self.history.cursor() > 1 && self.history_bytes() > self.history_memory_limit {
            self.history.drop_oldest();
        }
        // Anything but a plain append (redo steps or old steps dropped)
        // may leave blobs unused.
        if self.history.versions().len() != before + 1 {
            self.collect_garbage();
        }
        self.last_sync = next.graph_time + started.elapsed();
        self.doc = next.doc;
        Ok(())
    }

    /// Move one version back; the label and area of the step undone.
    fn step_back(&mut self) -> Option<(String, Option<lumenply_tiles::Rect>)> {
        let v = self.history.current_version();
        let step = (v.label.clone(), v.payload.affected);
        self.history.undo().then_some(step)
    }

    /// Move one version forward; the label and area of the step redone.
    fn step_forward(&mut self) -> Option<(String, Option<lumenply_tiles::Rect>)> {
        if !self.history.redo() {
            return None;
        }
        let v = self.history.current_version();
        Some((v.label.clone(), v.payload.affected))
    }

    /// Returns the label of the undone command.
    pub fn undo(&mut self) -> Option<String> {
        self.coalesce_key = None;
        self.last_target = None;
        let (label, affected) = self.step_back()?;
        self.last_affected = affected;
        self.doc = self.project_current();
        Some(label)
    }

    /// Returns the label of the redone command.
    pub fn redo(&mut self) -> Option<String> {
        self.coalesce_key = None;
        self.last_target = None;
        let (label, affected) = self.step_forward()?;
        self.last_affected = affected;
        self.doc = self.project_current();
        Some(label)
    }

    /// Undo or redo until exactly `steps` history entries remain applied.
    /// Afterwards [`Editor::last_affected`] covers every step crossed.
    pub fn jump_to(&mut self, steps: usize) {
        let mut acc: Option<Option<lumenply_tiles::Rect>> = None;
        let mut add = |a: Option<lumenply_tiles::Rect>| {
            acc = Some(match acc {
                None => a,
                Some(prev) => union_opt(prev, a),
            });
        };
        let mut moved = false;
        while self.history.cursor() > steps {
            self.coalesce_key = None;
            self.last_target = None;
            let Some((_, a)) = self.step_back() else { break };
            add(a);
            moved = true;
        }
        while self.history.cursor() < steps {
            self.coalesce_key = None;
            self.last_target = None;
            let Some((_, a)) = self.step_forward() else { break };
            add(a);
            moved = true;
        }
        if moved {
            self.doc = self.project_current();
        }
        if let Some(a) = acc {
            self.last_affected = a;
        }
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// The document as it was after `steps` history steps: 0 is the oldest
    /// state kept (the opened image unless the history limit dropped it),
    /// `history().len()` the current one, beyond that the redo steps.
    pub fn state(&self, steps: usize) -> Option<&Document> {
        if steps == self.history.cursor() {
            return Some(&self.doc);
        }
        let v = self.history.version(steps)?;
        Some(
            v.payload
                .doc
                .get_or_init(|| graph_sync::project(&v.graph, &v.payload.state, &self.blobs, &self.renderer)),
        )
    }

    /// The current version's identity: a number no other version of any
    /// document shares. Undo and redo return to a version's own number;
    /// any edit (a coalesced slider tick too) makes a new one. Comparing it
    /// with the number at the last save tells whether the document differs
    /// from its file, which the history's length cannot (undo, then a
    /// different edit; or edits past the history limit).
    pub fn version(&self) -> u64 {
        self.history.current_version().payload.serial
    }

    /// Labels of the undo stack, oldest first.
    pub fn history(&self) -> Vec<&str> {
        let v = self.history.versions();
        v[1..=self.history.cursor()]
            .iter()
            .map(|v| v.label.as_str())
            .collect()
    }

    /// Labels of steps that were undone and can be redone, in redo order.
    pub fn redo_history(&self) -> Vec<&str> {
        let v = self.history.versions();
        v[self.history.cursor() + 1..]
            .iter()
            .map(|v| v.label.as_str())
            .collect()
    }

    /// Union of the areas of the newest `n` undo steps (`n` ≥ 1).
    fn newest_affected(&self, n: usize) -> Option<lumenply_tiles::Rect> {
        let c = self.history.cursor();
        let steps = &self.history.versions()[c + 1 - n..=c];
        steps
            .iter()
            .skip(1)
            .fold(steps[0].payload.affected, |a, v| union_opt(a, v.payload.affected))
    }

    /// Merge the newest `n` undo steps into one labelled `label`, so a
    /// played action undoes in one go. `n` of 0 or 1 only relabels.
    pub fn squash_newest(&mut self, n: usize, label: &str) {
        let n = n.min(self.history.cursor());
        if n == 0 {
            return;
        }
        self.coalesce_key = None;
        let affected = self.newest_affected(n);
        let current = self.history.current_version();
        let bytes = self.released(
            self.history.cursor() - n,
            (&current.payload.blob_ids, &current.payload.state),
        );
        let dropped = self.history.squash(n, label);
        let step = &mut self.history.current_version_mut().payload;
        step.affected = affected;
        step.bytes = bytes;
        self.last_affected = affected;
        if !dropped.is_empty() {
            self.collect_garbage();
        }
    }

    /// Throw away the newest `n` undo steps and their changes, leaving no
    /// redo entry: a failed action taking back the steps it already ran.
    pub fn rollback_newest(&mut self, n: usize) {
        let n = n.min(self.history.cursor());
        if n == 0 {
            return;
        }
        self.coalesce_key = None;
        self.last_target = None;
        self.last_affected = self.newest_affected(n);
        self.history.rollback(n);
        self.doc = self.project_current();
        self.collect_garbage();
    }
}

#[cfg(test)]
mod tests {
    use super::commands::*;
    use super::*;
    use lumenply_tiles::Rgba;

    #[test]
    fn undo_and_redo_restore_pixels() {
        let mut ed = Editor::new(Document::new(64, 64));
        ed.execute(&AddPixelLayer::new("Paint")).unwrap();
        let id = ed.doc().layers()[0].id;

        let stroke = PaintStroke {
            layer: id,
            brush: Brush {
                radius: 4.0,
                hardness: 1.0,
                color: [1.0, 0.0, 0.0, 1.0],
                spacing: 0.25,
                jitter: 0.0,
                mode: BrushMode::Paint,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(10.0, 10.0, 1.0)],
        };
        ed.execute(&stroke).unwrap();
        let red = |ed: &Editor| ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(10, 10);
        assert!(red(&ed).a > 0.99);

        assert_eq!(ed.undo().as_deref(), Some("Paint stroke"));
        assert_eq!(red(&ed), Rgba::TRANSPARENT);
        assert_eq!(ed.redo().as_deref(), Some("Paint stroke"));
        assert!(red(&ed).a > 0.99);

        assert_eq!(ed.undo().as_deref(), Some("Paint stroke"));
        assert_eq!(ed.undo().as_deref(), Some("Add layer 'Paint'"));
        assert!(ed.doc().layers().is_empty());
        assert!(ed.undo().is_none());
    }

    #[test]
    fn failed_command_leaves_document_and_history_alone() {
        let mut ed = Editor::new(Document::new(8, 8));
        let err = ed.execute(&SetOpacity {
            layer: 42,
            opacity: 0.5,
        });
        assert!(matches!(err, Err(EditError::NoLayer(42))));
        assert!(!ed.can_undo());
    }

    #[test]
    fn a_version_number_tells_whether_the_document_is_the_one_saved() {
        let mut ed = Editor::new(Document::new(16, 16));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let opacity = |o: f32| SetOpacity {
            layer: id,
            opacity: o,
        };
        let saved = ed.version();
        // A failed edit changes nothing.
        assert!(ed
            .execute(&SetOpacity {
                layer: 99,
                opacity: 0.5
            })
            .is_err());
        assert_eq!(ed.version(), saved);
        ed.execute(&opacity(0.5)).unwrap();
        let half = ed.version();
        assert_ne!(half, saved);
        // Undo returns to the saved version's own number, redo to the edit's.
        ed.undo();
        assert_eq!(ed.version(), saved);
        ed.redo();
        assert_eq!(ed.version(), half);
        // Undo, then a different edit: the history is as long as when
        // saved, but this is a version never saved.
        ed.undo();
        ed.execute(&opacity(0.25)).unwrap();
        assert_eq!(ed.history().len(), 2);
        assert_ne!(ed.version(), saved);
        assert_ne!(ed.version(), half);
        // Every tick of a coalesced drag is a new version.
        let before = ed.version();
        ed.execute_coalescing(&opacity(0.3), "drag").unwrap();
        let tick = ed.version();
        ed.execute_coalescing(&opacity(0.4), "drag").unwrap();
        assert!(before != tick && tick != ed.version());
        // At the history limit the length stops growing; versions don't.
        ed.history_limit = 2;
        let mut seen = vec![ed.version()];
        for o in [0.6, 0.7, 0.8] {
            ed.execute(&opacity(o)).unwrap();
            assert!(!seen.contains(&ed.version()));
            seen.push(ed.version());
        }
        assert_eq!(ed.history().len(), 2);
        // Another editor never shares a number.
        let other = Editor::new(Document::new(16, 16));
        assert!(!seen.contains(&other.version()) && other.version() != saved);
    }

    #[test]
    fn discarding_a_coalesced_run_leaves_no_trace() {
        let mut ed = Editor::new(Document::new(64, 64));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let steps = ed.history().len();
        let text = |s: &str| lumenply_doc::TextLayer::new(s, 4.0, 40.0, 20.0, [0.0, 0.0, 0.0, 1.0]);
        let id = ed.doc().next_id();
        ed.execute_coalescing(
            &AddTextLayer {
                text: text(""),
                above: None,
            },
            "type",
        )
        .unwrap();
        ed.execute_coalescing(
            &SetText {
                layer: id,
                text: text("ab"),
            },
            "type",
        )
        .unwrap();
        assert!(ed.coalescing("type") && !ed.coalescing("other"));
        assert_eq!(ed.history().len(), steps + 1, "add + typing is one step");
        assert_eq!(ed.doc().layer_count(), 2);

        // Another key does nothing.
        assert!(!ed.discard_coalescing("other"));
        assert_eq!(ed.doc().layer_count(), 2);
        // The open run goes away entirely: no layer, no step, no redo.
        assert!(ed.discard_coalescing("type"));
        assert_eq!(ed.doc().layer_count(), 1);
        assert_eq!(ed.history().len(), steps);
        assert!(!ed.can_redo());
        assert!(!ed.coalescing("type"));
        assert!(!ed.discard_coalescing("type"), "only once");

        // A closed run cannot be discarded.
        ed.execute_coalescing(
            &AddTextLayer {
                text: text("x"),
                above: None,
            },
            "type",
        )
        .unwrap();
        ed.end_coalescing();
        assert!(!ed.discard_coalescing("type"));
        assert_eq!(ed.doc().layer_count(), 2);
    }

    #[test]
    fn coalescing_merges_a_drag_into_one_undo_step() {
        let mut ed = Editor::new(Document::new(8, 8));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        for i in 1..=10 {
            ed.execute_coalescing(
                &SetOpacity {
                    layer: id,
                    opacity: 1.0 - i as f32 * 0.05,
                },
                "opacity",
            )
            .unwrap();
        }
        assert_eq!(
            ed.history().len(),
            2,
            "one step for the layer, one for the whole drag"
        );
        assert!((ed.doc().layer(id).unwrap().opacity - 0.5).abs() < 1e-6);

        ed.end_coalescing();
        ed.execute_coalescing(
            &SetOpacity {
                layer: id,
                opacity: 0.25,
            },
            "opacity",
        )
        .unwrap();
        assert_eq!(ed.history().len(), 3, "a new run starts a new step");

        ed.undo();
        assert!((ed.doc().layer(id).unwrap().opacity - 0.5).abs() < 1e-6);
        ed.undo();
        assert_eq!(ed.doc().layer(id).unwrap().opacity, 1.0);
    }

    #[test]
    fn jump_to_walks_the_history_both_ways() {
        let mut ed = Editor::new(Document::new(8, 8));
        for i in 0..4 {
            ed.execute(&AddPixelLayer::new(format!("L{i}"))).unwrap();
        }
        ed.jump_to(1);
        assert_eq!(ed.doc().layer_count(), 1);
        assert_eq!(
            ed.redo_history(),
            vec!["Add layer 'L1'", "Add layer 'L2'", "Add layer 'L3'"]
        );
        ed.jump_to(3);
        assert_eq!(ed.doc().layer_count(), 3);
        ed.jump_to(10);
        assert_eq!(ed.doc().layer_count(), 4);
    }

    #[test]
    fn paint_reports_its_affected_area() {
        let mut ed = Editor::new(Document::new(500, 500));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        assert!(
            ed.last_affected().is_none(),
            "adding a layer: anything may change"
        );
        let id = ed.doc().layers()[0].id;
        ed.execute(&PaintStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                ..Brush::default()
            },
            points: vec![
                StrokePoint::new(100.0, 100.0, 1.0),
                StrokePoint::new(140.0, 120.0, 1.0),
            ],
        })
        .unwrap();
        let r = ed.last_affected().unwrap();
        assert!(r.contains(100, 100) && r.contains(140, 120));
        assert!(r.contains(89, 89) && !r.contains(60, 60), "{r:?}");
    }

    #[test]
    fn edits_are_stored_compactly_but_read_back_exactly_enough() {
        let mut ed = Editor::new(Document::new(300, 300));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        ed.execute(&Fill {
            layer: id,
            color: [0.2, 0.4, 0.6, 1.0],
        })
        .unwrap();
        let store = ed.doc().layer(id).unwrap().pixels().unwrap();
        assert!(store.coords().all(|c| store.tile(c).unwrap().is_compact()));
        assert_eq!(storage_bytes(ed.doc()), 4 * lumenply_tiles::TILE_PIXELS * 8);
        let p = store.get_pixel(150, 150).to_straight();
        assert!((p[0] - 0.2).abs() < 1e-4 && (p[2] - 0.6).abs() < 1e-4);
        // The undo snapshot still composites correctly after compaction.
        ed.undo();
        assert!(ed.doc().layer(id).unwrap().pixels().unwrap().is_empty());
        ed.redo();
        assert!(
            (ed.doc()
                .layer(id)
                .unwrap()
                .pixels()
                .unwrap()
                .get_pixel(10, 10)
                .to_straight()[1]
                - 0.4)
                .abs()
                < 1e-4
        );
    }

    #[test]
    fn undo_redo_and_jump_report_affected_areas() {
        let mut ed = Editor::new(Document::new(500, 500));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let dab = |x: f32, y: f32| PaintStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(x, y, 1.0)],
        };
        ed.execute(&dab(100.0, 100.0)).unwrap();
        ed.execute(&dab(300.0, 300.0)).unwrap();

        // Undoing a step dirties exactly that step's area.
        ed.undo();
        let r = ed.last_affected().expect("undo of a stroke is local");
        assert!(r.contains(300, 300) && !r.contains(100, 100), "{r:?}");
        ed.redo();
        let r = ed.last_affected().expect("redo of a stroke is local");
        assert!(r.contains(300, 300) && !r.contains(100, 100), "{r:?}");

        // Jumping across several steps unions their areas.
        ed.jump_to(1);
        let r = ed.last_affected().expect("both strokes are local");
        assert!(r.contains(100, 100) && r.contains(300, 300), "{r:?}");

        // A step whose command cannot bound its change poisons to "anything".
        ed.jump_to(3);
        ed.undo(); // stroke at (300, 300)
        ed.undo(); // stroke at (100, 100)
        ed.undo(); // AddPixelLayer: affected() is None
        assert!(ed.last_affected().is_none());

        // A coalesced drag undoes as the union of its ticks' areas.
        let mut ed = Editor::new(Document::new(500, 500));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let dab = |x: f32, y: f32| PaintStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(x, y, 1.0)],
        };
        ed.execute_coalescing(&dab(50.0, 50.0), "drag").unwrap();
        ed.execute_coalescing(&dab(400.0, 400.0), "drag").unwrap();
        ed.undo();
        let r = ed.last_affected().expect("coalesced strokes are local");
        assert!(r.contains(50, 50) && r.contains(400, 400), "{r:?}");
    }

    #[test]
    fn set_pass_through_is_group_only_and_undoable() {
        let mut ed = Editor::new(Document::new(8, 8));
        ed.execute(&AddPixelLayer::new("px")).unwrap();
        let px = ed.doc().layers()[0].id;
        assert!(matches!(
            ed.execute(&SetPassThrough {
                layer: px,
                pass_through: true
            }),
            Err(EditError::NotGroup(_))
        ));
        ed.execute(&GroupLayers {
            layers: vec![px],
            name: "g".into(),
        })
        .unwrap();
        let g = ed.doc().layers()[0].id;
        ed.execute(&SetPassThrough {
            layer: g,
            pass_through: true,
        })
        .unwrap();
        assert!(ed.doc().layer(g).unwrap().pass_through);
        ed.undo();
        assert!(!ed.doc().layer(g).unwrap().pass_through);
    }

    #[test]
    fn history_limit_is_enforced() {
        let mut ed = Editor::new(Document::new(8, 8));
        ed.history_limit = 3;
        for i in 0..5 {
            ed.execute(&AddPixelLayer::new(format!("L{i}"))).unwrap();
        }
        assert_eq!(ed.history().len(), 3);

        // Lowering the limit takes full effect on the next edit, not one
        // entry at a time.
        ed.history_limit = 10;
        for i in 5..12 {
            ed.execute(&AddPixelLayer::new(format!("L{i}"))).unwrap();
        }
        assert_eq!(ed.history().len(), 10);
        ed.history_limit = 2;
        ed.execute(&AddPixelLayer::new("last")).unwrap();
        assert_eq!(ed.history().len(), 2);
    }

    #[test]
    fn history_memory_limit_drops_oldest_steps() {
        // Each fill rewrites the full 600×600 canvas: 9 compact tiles, about
        // 4.5 MB per undo step.
        let mut ed = Editor::new(Document::new(600, 600));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let step = 9 * lumenply_tiles::TILE_PIXELS * 8;
        ed.history_memory_limit = 3 * step + step / 2; // room for ~3 fills
        for i in 0..6 {
            ed.execute(&Fill {
                layer: id,
                color: [i as f32 / 10.0, 0.5, 0.5, 1.0],
            })
            .unwrap();
        }
        assert!(
            ed.history_bytes() <= ed.history_memory_limit,
            "{} > {}",
            ed.history_bytes(),
            ed.history_memory_limit
        );
        let len = ed.history().len();
        assert!((2..=4).contains(&len), "kept {len} steps");
        // The newest steps survive; undo still works.
        assert_eq!(ed.undo().as_deref(), Some("Fill"));

        // A tiny limit still keeps one undo step, even when it is heavy.
        // (The first two steps cost nothing: their snapshots hold empty
        // states. Only the second fill makes the first fill's tiles unique
        // to a snapshot.)
        let mut ed = Editor::new(Document::new(600, 600));
        ed.history_memory_limit = 1;
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        for c in [0.3, 0.7] {
            ed.execute(&Fill {
                layer: id,
                color: [c, 0.2, 0.3, 1.0],
            })
            .unwrap();
        }
        assert_eq!(ed.history().len(), 1);
        assert!(ed.can_undo());
    }
}
