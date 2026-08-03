//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchical Navigable Small World vector index.

mod construction;
mod index;
mod metric;
mod mutation;
mod neighbors;
mod persistence;
mod restore;
mod search;
mod types;
mod validation;

pub(crate) use metric::MAX_HNSW_LEVEL;
pub use types::HNSWIndex;
pub(crate) use types::{HNSWGraphMeta, HNSWNodeSnapshot, HNSWPersistenceDelta};

#[cfg(test)]
mod tests;
