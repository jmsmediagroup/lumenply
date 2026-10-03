//! Undo and redo as graph history: every edit is a new version of the
//! graph, and undo moves back to the previous one. Versions share the nodes
//! they have in common, so a step costs only the nodes it changed.
//!
//! Each version can carry a payload `P` next to its graph: whatever else
//! the owner keeps per version (the editor keeps the document state that
//! isn't pixels, see [`crate::sync::DocState`]).

use std::sync::Arc;

use crate::model::Graph;

/// One kept version: the graph, the label of the step that made it ("Open"
/// for the first) and the owner's payload.
#[derive(Clone)]
pub struct Version<P = ()> {
    pub graph: Arc<Graph>,
    pub label: String,
    pub payload: P,
}

pub struct History<P = ()> {
    versions: Vec<Version<P>>,
    /// Index of the current version.
    cursor: usize,
    /// Most undo steps kept; the oldest go first.
    pub limit: usize,
    coalesce: Option<String>,
}

impl History<()> {
    pub fn new(graph: Graph) -> Self {
        History::with_payload(graph, ())
    }

    /// Make `graph` the new current version, dropping anything redoable.
    pub fn commit(&mut self, graph: Graph, label: impl Into<String>) {
        self.commit_with(graph, label, ());
    }

    /// Like [`History::commit`], but consecutive commits with the same key
    /// replace each other, so a slider drag is one undo step.
    pub fn commit_coalescing(&mut self, key: &str, graph: Graph, label: impl Into<String>) {
        self.commit_coalescing_with(key, graph, label, ());
    }
}

impl<P> History<P> {
    /// A history whose only version is `graph`, labelled "Open".
    pub fn with_payload(graph: impl Into<Arc<Graph>>, payload: P) -> Self {
        History {
            versions: vec![Version {
                graph: graph.into(),
                label: "Open".into(),
                payload,
            }],
            cursor: 0,
            limit: 100,
            coalesce: None,
        }
    }

    pub fn current(&self) -> &Graph {
        &self.versions[self.cursor].graph
    }

    pub fn current_arc(&self) -> Arc<Graph> {
        self.versions[self.cursor].graph.clone()
    }

    pub fn current_version(&self) -> &Version<P> {
        &self.versions[self.cursor]
    }

    pub fn current_version_mut(&mut self) -> &mut Version<P> {
        &mut self.versions[self.cursor]
    }

    /// Version `i` (0 is the oldest kept).
    pub fn version(&self, i: usize) -> Option<&Version<P>> {
        self.versions.get(i)
    }

    /// Every kept version, oldest first.
    pub fn versions(&self) -> &[Version<P>] {
        &self.versions
    }

    /// Index of the current version: also the number of steps undo can take.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Make `graph` the new current version, dropping anything redoable and
    /// then the oldest versions beyond [`History::limit`] undo steps.
    pub fn commit_with(&mut self, graph: impl Into<Arc<Graph>>, label: impl Into<String>, payload: P) {
        self.coalesce = None;
        self.push(graph.into(), label.into(), payload);
    }

    /// Like [`History::commit_with`], but consecutive commits with the same
    /// key replace each other, so a slider drag is one undo step.
    pub fn commit_coalescing_with(
        &mut self,
        key: &str,
        graph: impl Into<Arc<Graph>>,
        label: impl Into<String>,
        payload: P,
    ) {
        if self.coalesce.as_deref() == Some(key) && self.cursor > 0 && self.cursor + 1 == self.versions.len()
        {
            self.versions[self.cursor] = Version {
                graph: graph.into(),
                label: label.into(),
                payload,
            };
            return;
        }
        self.push(graph.into(), label.into(), payload);
        self.coalesce = Some(key.to_string());
    }

    fn push(&mut self, graph: Arc<Graph>, label: String, payload: P) {
        self.versions.truncate(self.cursor + 1);
        self.versions.push(Version {
            graph,
            label,
            payload,
        });
        self.cursor += 1;
        let excess = self.versions.len().saturating_sub(self.limit.saturating_add(1));
        if excess > 0 {
            self.versions.drain(..excess);
            self.cursor -= excess;
        }
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.versions.len()
    }

    pub fn undo(&mut self) -> bool {
        self.coalesce = None;
        if !self.can_undo() {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        self.coalesce = None;
        if !self.can_redo() {
            return false;
        }
        self.cursor += 1;
        true
    }

    /// Go straight to version `i` (0 is the oldest kept).
    pub fn jump(&mut self, i: usize) -> bool {
        self.coalesce = None;
        if i >= self.versions.len() {
            return false;
        }
        self.cursor = i;
        true
    }

    /// Forget the oldest version (the oldest undo step goes with it).
    /// Refused, returning `None`, when it is the current one.
    pub fn drop_oldest(&mut self) -> Option<Version<P>> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(self.versions.remove(0))
    }

    /// Merge the newest `n` steps up to the current version into one
    /// labelled `label`: the versions between are dropped and the current
    /// one is relabelled. Returns the dropped versions, oldest first.
    pub fn squash(&mut self, n: usize, label: impl Into<String>) -> Vec<Version<P>> {
        self.coalesce = None;
        let n = n.min(self.cursor);
        if n == 0 {
            return Vec::new();
        }
        let dropped: Vec<Version<P>> = self.versions.drain(self.cursor + 1 - n..self.cursor).collect();
        self.cursor -= dropped.len();
        self.versions[self.cursor].label = label.into();
        dropped
    }

    /// Take back the newest `n` steps up to the current version, leaving
    /// nothing to redo: the version `n` steps back becomes current and
    /// every version after it is dropped. Returns the dropped versions,
    /// oldest first.
    pub fn rollback(&mut self, n: usize) -> Vec<Version<P>> {
        self.coalesce = None;
        let n = n.min(self.cursor);
        if n == 0 {
            return Vec::new();
        }
        self.cursor -= n;
        self.versions.drain(self.cursor + 1..).collect()
    }

    /// Labels of every kept version, oldest first, and the current index.
    pub fn labels(&self) -> (Vec<&str>, usize) {
        (
            self.versions.iter().map(|v| v.label.as_str()).collect(),
            self.cursor,
        )
    }

    /// Every kept version, for collecting the blobs still in use.
    pub fn graphs(&self) -> impl Iterator<Item = &Graph> {
        self.versions.iter().map(|v| v.graph.as_ref())
    }
}
