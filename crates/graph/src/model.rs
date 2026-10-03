//! The graph itself: nodes (an operation, its parameters and its input
//! references) keyed by stable ids, and the node whose output is the image.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ops::Op;

/// A node's stable identity. Edits, the UI and the Layers panel refer to
/// nodes by id; the render cache uses content keys instead (see [`crate::key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "n{}", self.0)
    }
}

impl Serialize for NodeId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.strip_prefix('n')
            .and_then(|n| n.parse().ok())
            .map(NodeId)
            .ok_or_else(|| serde::de::Error::custom(format!("bad node id {s:?}, expected n<number>")))
    }
}

/// One operation: its type and parameters, and the nodes feeding its input
/// ports in the order [`Op::ports`] names them (`None`: port left empty).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(flatten)]
    pub op: Op,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Option<NodeId>>,
    /// What the UI calls it (a layer name, say); never affects pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Node {
    pub fn new(op: Op, inputs: Vec<Option<NodeId>>) -> Self {
        Node {
            op,
            inputs,
            name: None,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The node feeding port `i`, if connected.
    pub fn input(&self, i: usize) -> Option<NodeId> {
        self.inputs.get(i).copied().flatten()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GraphError {
    #[error("no node {0}")]
    Missing(NodeId),
    #[error("{node} refers to missing input {input}")]
    DanglingInput { node: NodeId, input: NodeId },
    #[error("{0} is part of a cycle")]
    Cycle(NodeId),
    #[error("{node} ({op}) takes at most {max} inputs, got {got}")]
    Arity {
        node: NodeId,
        op: &'static str,
        max: usize,
        got: usize,
    },
    #[error("bad graph file: {0}")]
    Json(String),
    #[error("graph format {0} is newer than this version of Lumenply understands ({FORMAT})")]
    TooNew(u32),
}

/// Version of the JSON layout written by [`Graph::to_json`].
pub const FORMAT: u32 = 3;

/// A document: canvas size, nodes and the node whose output is the image.
/// Cloning is cheap (nodes are shared), which is what makes every edit a
/// new version for the history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub format: u32,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<NodeId>,
    nodes: BTreeMap<NodeId, Arc<Node>>,
    #[serde(default)]
    next_id: u64,
}

impl Graph {
    pub fn new(width: u32, height: u32) -> Self {
        Graph {
            format: FORMAT,
            width,
            height,
            output: None,
            nodes: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub fn canvas(&self) -> lumenply_tiles::Rect {
        lumenply_tiles::Rect::new(0, 0, self.width, self.height)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id).map(|n| n.as_ref())
    }

    /// The shared node itself: unchanged nodes keep the same `Arc` across
    /// versions, which lets content keys be memoised (see [`crate::key`]).
    pub fn node_arc(&self, id: NodeId) -> Option<&Arc<Node>> {
        self.nodes.get(&id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().map(|(id, n)| (*id, n.as_ref()))
    }

    /// Add a node and return its new id.
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.next_id.max(1));
        self.next_id = id.0 + 1;
        self.nodes.insert(id, Arc::new(node));
        id
    }

    /// Replace a node's operation, inputs and name, keeping its id.
    pub fn replace(&mut self, id: NodeId, node: Node) -> Result<(), GraphError> {
        let slot = self.nodes.get_mut(&id).ok_or(GraphError::Missing(id))?;
        *slot = Arc::new(node);
        Ok(())
    }

    /// Change one node in place (copy on write: other versions keep theirs).
    pub fn update(&mut self, id: NodeId, f: impl FnOnce(&mut Node)) -> Result<(), GraphError> {
        let slot = self.nodes.get_mut(&id).ok_or(GraphError::Missing(id))?;
        f(Arc::make_mut(slot));
        Ok(())
    }

    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        self.nodes.remove(&id).map(Arc::unwrap_or_clone)
    }

    /// Every node `id` depends on, itself included.
    pub fn upstream(&self, id: NodeId) -> BTreeSet<NodeId> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![id];
        while let Some(n) = todo.pop() {
            if !seen.insert(n) {
                continue;
            }
            if let Some(node) = self.node(n) {
                todo.extend(node.inputs.iter().flatten());
            }
        }
        seen
    }

    /// Every node whose output depends on `id`, itself included: what an
    /// edit of `id` invalidates.
    pub fn downstream(&self, id: NodeId) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::from([id]);
        loop {
            let before = out.len();
            for (n, node) in &self.nodes {
                if node.inputs.iter().flatten().any(|i| out.contains(i)) {
                    out.insert(*n);
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }

    /// Drop the nodes the output no longer needs. Returns how many went.
    pub fn collect_garbage(&mut self) -> usize {
        let keep = match self.output {
            Some(o) => self.upstream(o),
            None => BTreeSet::new(),
        };
        let before = self.nodes.len();
        self.nodes.retain(|id, _| keep.contains(id));
        before - self.nodes.len()
    }

    /// Nodes in an order where every input comes before its consumers.
    pub fn topo_order(&self) -> Result<Vec<NodeId>, GraphError> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Visiting,
            Done,
        }
        let mut marks: BTreeMap<NodeId, Mark> = BTreeMap::new();
        let mut order = Vec::with_capacity(self.nodes.len());
        for &root in self.nodes.keys() {
            if marks.contains_key(&root) {
                continue;
            }
            // Iterative depth-first search: (node, next input port to visit).
            let mut stack = vec![(root, 0usize)];
            marks.insert(root, Mark::Visiting);
            while let Some(&(n, port)) = stack.last() {
                let inputs = &self.nodes[&n].inputs;
                if port == inputs.len() {
                    marks.insert(n, Mark::Done);
                    order.push(n);
                    stack.pop();
                    continue;
                }
                stack.last_mut().unwrap().1 += 1;
                let Some(next) = inputs[port] else {
                    continue;
                };
                match marks.get(&next) {
                    Some(Mark::Visiting) => return Err(GraphError::Cycle(next)),
                    Some(Mark::Done) => {}
                    None => {
                        if !self.nodes.contains_key(&next) {
                            return Err(GraphError::DanglingInput { node: n, input: next });
                        }
                        marks.insert(next, Mark::Visiting);
                        stack.push((next, 0));
                    }
                }
            }
        }
        Ok(order)
    }

    /// Structural checks: inputs exist, no cycles, no op gets more inputs
    /// than it has ports, the output exists.
    pub fn validate(&self) -> Result<(), GraphError> {
        for (&id, node) in &self.nodes {
            let ports = node.op.ports();
            if node.inputs.len() > ports.len() && !node.op.variadic() {
                return Err(GraphError::Arity {
                    node: id,
                    op: node.op.type_name(),
                    max: ports.len(),
                    got: node.inputs.len(),
                });
            }
        }
        self.topo_order()?;
        if let Some(o) = self.output {
            if !self.nodes.contains_key(&o) {
                return Err(GraphError::Missing(o));
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("graphs always serialise")
    }

    pub fn from_json(s: &str) -> Result<Graph, GraphError> {
        let g: Graph = serde_json::from_str(s).map_err(|e| GraphError::Json(e.to_string()))?;
        if g.format > FORMAT {
            return Err(GraphError::TooNew(g.format));
        }
        let mut g = g;
        let max = g.nodes.keys().map(|n| n.0).max().unwrap_or(0);
        g.next_id = g.next_id.max(max + 1);
        g.validate()?;
        Ok(g)
    }
}
