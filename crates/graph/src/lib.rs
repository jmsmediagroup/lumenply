//! The document as a graph of operations (ADR 0025).
//!
//! A [`Graph`] holds nodes — an [`Op`] (type and parameters) plus input
//! references — and the node whose output is the image. It serialises to
//! JSON; pixel data an op needs lives in a content-addressed [`BlobStore`].
//! A [`Renderer`] evaluates it tile by tile, caching every node's tiles
//! under the node's content key (a hash of its op, parameters and inputs'
//! keys), so an edit recomputes exactly what is downstream of it and undo
//! finds the previous results still cached. [`History`] keeps graph
//! versions for undo and redo.

pub mod blob;
pub mod cache;
pub mod eval;
pub mod history;
pub mod key;
pub mod lower;
pub mod model;
pub mod ops;
pub mod ops_content;
mod pixel_ops;
pub mod sync;
pub mod whole;

pub use blob::{BlobId, BlobStore, Hash, TileHasher};
pub use cache::{CacheStats, TileCache};
pub use eval::{Ctx, Renderer};
pub use history::{History, Version};
pub use key::{Key, KeyMemo};
pub use lower::{lower, Lowered};
pub use model::{Graph, GraphError, Node, NodeId, FORMAT};
pub use ops::{ClipMember, LayerProps, Op};
pub use ops_content::PatternPixels;
pub use sync::{project, sync, Base, ContentEdit, DocState, EditInput, LayerRecord};
pub use whole::WholeCache;

#[cfg(test)]
mod sync_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_content;
