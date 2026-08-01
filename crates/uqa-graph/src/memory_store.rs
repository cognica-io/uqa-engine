//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory implementation of [`GraphStore`].
//!
//! Vertex and edge records live in a global map, while each named graph owns a
//! partition of membership and adjacency indexes. AGE-compatible graph ids and
//! their per-graph label registries are managed separately.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_core::{Edge, EdgeId, Vertex, VertexId};

use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};
use crate::types::Direction;

mod graphid;
mod partition;
mod store;
mod trait_impl;

pub use graphid::{
    graphid_label_id, graphid_sequence, make_graphid, GraphLabelRegistry, EDGE_DEFAULT_LABEL_ID,
    FIRST_USER_LABEL_ID, GRAPHID_LABEL_SHIFT, VERTEX_DEFAULT_LABEL_ID,
};

use graphid::usize_to_f64_exact;
#[cfg(test)]
use graphid::{MAX_GRAPHID_LABEL_ID, MAX_GRAPHID_SEQUENCE};
use partition::Partition;

#[derive(Debug, Default, Clone)]
pub struct MemoryGraphStore {
    vertices: BTreeMap<VertexId, Vertex>,
    edges: BTreeMap<EdgeId, Edge>,
    graphs: BTreeMap<String, Partition>,
    vertex_membership: BTreeMap<VertexId, BTreeSet<String>>,
    edge_membership: BTreeMap<EdgeId, BTreeSet<String>>,
    label_registries: BTreeMap<String, GraphLabelRegistry>,
    next_vertex_id: VertexId,
    next_edge_id: EdgeId,
}

#[cfg(test)]
mod tests;
