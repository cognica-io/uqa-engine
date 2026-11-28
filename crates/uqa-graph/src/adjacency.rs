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
    vertex_index(
        vertices,
        edge_endpoint(store, operation, eid, endpoint)?,
        graph,
        eid,
    )
}

pub(super) fn edge_endpoint(
    store: &impl GraphStore,
    operation: &str,
    eid: EdgeId,
    endpoint: fn(&Edge) -> VertexId,
) -> GraphStoreResult<VertexId> {
    let edge = store
        .get_edge(eid)?
        .ok_or_else(|| GraphStoreError::CorruptGraph(format!("missing {operation} edge {eid}")))?;
    Ok(endpoint(&edge))
}

pub(super) fn vertex_index(
    vertices: &[VertexId],
    id: VertexId,
    graph: &str,
    eid: EdgeId,
) -> GraphStoreResult<usize> {
    vertices.binary_search(&id).map_err(|_| {
        GraphStoreError::CorruptGraph(format!(
            "edge {eid} references vertex {id} outside graph {graph:?}"
        ))
    })
}
