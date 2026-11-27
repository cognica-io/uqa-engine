//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional agtype materialization for a matched path.

use super::{agtype, CypherError, Edge, Value, Vertex};

#[derive(Clone, Default)]
pub(super) struct Walk {
    pub(super) edges: Vec<Edge>,
    pub(super) trail: Vec<Value>,
}

pub(super) fn push_trail_vertex(
    trail: &mut Vec<Value>,
    vertex: &Vertex,
    needed: bool,
) -> Result<(), CypherError> {
    if needed {
        trail.push(agtype::vertex_to_value(vertex)?);
    } else {
        agtype::graph_id(vertex.vertex_id)?;
    }
    Ok(())
}

pub(super) fn push_trail_edge(
    trail: &mut Vec<Value>,
    edge: &Edge,
    vertex: &Vertex,
    needed: bool,
) -> Result<(), CypherError> {
    if needed {
        trail.push(agtype::edge_to_value(edge)?);
    } else {
        agtype::edge_ids(edge)?;
    }
    push_trail_vertex(trail, vertex, needed)
}
