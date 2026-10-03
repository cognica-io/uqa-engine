//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchical Navigable Small World vector index.

mod access;
mod canonical;
mod consistency;
mod construction;
mod index;
mod metric;
mod mutation;
mod neighbors;
mod persistence;
mod prepare;
mod query;
mod queue;
mod restore;
mod search;
mod store;
mod types;
mod validation;
mod visited;

pub use consistency::HNSWCanonicalValidator;
pub use metric::MAX_HNSW_LEVEL;
pub use persistence::{HNSWDeltaNodes, HNSWGraphDelta};
pub use prepare::{HNSWCanonicalBuilder, HNSWMutation};
pub use restore::HNSWRestoreBuilder;
pub use types::HNSWIndex;
pub use types::{HNSWGraphMeta, HNSWNodeSnapshot, HNSWPersistenceDelta};

#[cfg(test)]
mod tests;
