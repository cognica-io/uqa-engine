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
use uqa_core::memory::MemoryBudget;
use uqa_core::{Edge, EdgeId, Vertex, VertexId};

use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};
use crate::types::Direction;

mod graphid;
mod image;
mod labels;
mod native;
mod partition;
mod publication;
mod store;
mod trait_impl;

pub use graphid::{
    graphid_label_id, graphid_sequence, make_graphid, GraphLabelInfo, GraphLabelRegistry,
    LabelKind, EDGE_DEFAULT_LABEL_ID, FIRST_USER_LABEL_ID, GRAPHID_LABEL_SHIFT,
    VERTEX_DEFAULT_LABEL_ID,
};

pub(crate) use graphid::{usize_to_f64_exact, MAX_GRAPHID_LABEL_ID, MAX_GRAPHID_SEQUENCE};
use labels::NativeLabels;
use native::{NativeMap, NativeSet};
use partition::Partition;

#[derive(Debug, Clone)]
pub struct MemoryGraphStore {
    vertices: NativeMap<VertexId, Vertex>,
    edges: NativeMap<EdgeId, Edge>,
    graphs: NativeMap<String, Partition>,
    vertex_membership: NativeMap<VertexId, NativeSet<String>>,
    edge_membership: NativeMap<EdgeId, NativeSet<String>>,
    label_registries: NativeMap<String, NativeLabels>,
    next_vertex_id: VertexId,
    next_edge_id: EdgeId,
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod tests;
