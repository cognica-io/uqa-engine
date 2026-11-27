//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense endpoint lookup for per-vertex analytical state.

use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};
use uqa_core::{Edge, EdgeId, VertexId};

pub(super) fn endpoint_index(
    store: &impl GraphStore,
    graph: &str,
    vertices: &[VertexId],
    operation: &str,
    eid: EdgeId,
    endpoint: fn(&Edge) -> VertexId,
) -> GraphStoreResult<usize> {
    let edge = store
        .get_edge(eid)?
        .ok_or_else(|| GraphStoreError::CorruptGraph(format!("missing {operation} edge {eid}")))?;
    let id = endpoint(&edge);
    vertices.binary_search(&id).map_err(|_| {
        GraphStoreError::CorruptGraph(format!(
            "edge {eid} references vertex {id} outside graph {graph:?}"
        ))
    })
}
