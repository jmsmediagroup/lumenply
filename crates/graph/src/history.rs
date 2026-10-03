//! Undo and redo as graph history: every edit is a new version of the
//! graph, and undo moves back to the previous one. Versions share the nodes
//! they have in common, so a step costs only the nodes it changed.

use std::sync::Arc;

use crate::model::Graph;

#[derive(Clone)]
struct Version {
    graph: Arc<Graph>,
    label: String,
}

pub struct History {
    versions: Vec<Version>,
    /// Index of the current version.
    cursor: usize,
    /// Most undo steps kept; the oldest go first.
    pub limit: usize,
    coalesce: Option<String>,
}

impl History {
    pub fn new(graph: Graph) -> Self {
        History {
            versions: vec![Version {
                graph: Arc::new(graph),
                label: "Open".into(),
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

    /// Make `graph` the new current version, dropping anything redoable.
    pub fn commit(&mut self, graph: Graph, label: impl Into<String>) {
        self.coalesce = None;
        self.push(graph, label.into());
    }

    /// Like [`History::commit`], but consecutive commits with the same key
    /// replace each other, so a slider drag is one undo step.
    pub fn commit_coalescing(&mut self, key: &str, graph: Graph, label: impl Into<String>) {
        if self.coalesce.as_deref() == Some(key) && self.cursor > 0 && self.cursor + 1 == self.versions.len()
        {
            self.versions[self.cursor] = Version {
                graph: Arc::new(graph),
                label: label.into(),
            };
            return;
        }
        self.push(graph, label.into());
        self.coalesce = Some(key.to_string());
    }

    fn push(&mut self, graph: Graph, label: String) {
        self.versions.truncate(self.cursor + 1);
        self.versions.push(Version {
            graph: Arc::new(graph),
            label,
        });
        self.cursor += 1;
        let excess = self.versions.len().saturating_sub(self.limit + 1);
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
