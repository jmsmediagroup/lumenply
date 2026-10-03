//! Content keys: a hash of a node's operation, parameters and the content
//! keys of its inputs. Equal keys mean equal output, so the render cache
//! keyed by them never needs explicit invalidation: editing a node changes
//! its key and every key downstream of it, and nothing else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::blob::Hash;
use crate::model::{Graph, Node, NodeId};

pub type Key = Hash;

/// Memoises the hash of each node's operation by the node's address, so a
/// new graph version re-hashes only the nodes that changed (unchanged nodes
/// are the same `Arc` in every version).
#[derive(Default)]
pub struct KeyMemo {
    ops: Mutex<HashMap<usize, (Arc<Node>, Hash)>>,
}

impl KeyMemo {
    fn op_hash(&self, node: &Arc<Node>) -> Hash {
        let addr = Arc::as_ptr(node) as usize;
        if let Some((kept, h)) = self.ops.lock().unwrap().get(&addr) {
            if Arc::ptr_eq(kept, node) {
                return *h;
            }
        }
        // The name is for people; it never changes pixels.
        let json = serde_json::to_vec(&node.op).expect("ops always serialise");
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"op");
        hasher.update(&json);
        let h = Hash(*hasher.finalize().as_bytes());
        self.ops.lock().unwrap().insert(addr, (node.clone(), h));
        h
    }

    /// Content keys of `root` and everything it depends on.
    pub fn keys(&self, graph: &Graph, root: NodeId) -> HashMap<NodeId, Key> {
        let mut keys: HashMap<NodeId, Key> = HashMap::new();
        // Iterative post-order over the inputs of `root`.
        let mut stack = vec![(root, false)];
        while let Some((id, ready)) = stack.pop() {
            if keys.contains_key(&id) {
                continue;
            }
            let Some(node) = graph.node_arc(id) else {
                continue;
            };
            if !ready {
                stack.push((id, true));
                for i in node.inputs.iter().flatten() {
                    if !keys.contains_key(i) {
                        stack.push((*i, false));
                    }
                }
                continue;
            }
            let mut h = blake3::Hasher::new();
            h.update(b"node");
            h.update(&self.op_hash(node).0);
            for i in &node.inputs {
                match i.and_then(|i| keys.get(&i)) {
                    Some(k) => h.update(&k.0),
                    None => h.update(&[0u8; 32]),
                };
            }
            keys.insert(id, Hash(*h.finalize().as_bytes()));
        }
        keys
    }

    /// Forget nodes no graph version uses any more.
    pub fn prune(&self) {
        self.ops
            .lock()
            .unwrap()
            .retain(|_, (n, _)| Arc::strong_count(n) > 1);
    }
}
