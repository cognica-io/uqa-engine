//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Point graph reads and selective identity pages use one retained native record view.

pub(in crate::catalog) mod lifecycle;
pub(in crate::catalog) mod paths;
mod restore;
mod write;
pub(in crate::catalog) use write::{membership, named_graph, source};

#[cfg(test)]
mod tests;

pub(in crate::catalog) use restore::{
    load_edges, load_memberships, load_names, load_snapshot, load_vertices,
};

use rusqlite::types::ValueRef;
use uqa_storage::{EdgeRow, GraphEntityFilter, GraphEntityKind, GraphVertexRow};

use super::{string, text, Family, NativeRecordOwner, NativeSnapshot, Result};
use crate::catalog::{decode_catalog_id, encode_catalog_id};

fn entity_family(kind: GraphEntityKind) -> Family {
    match kind {
        GraphEntityKind::Vertex => Family::GraphVertices,
        GraphEntityKind::Edge => Family::GraphEdges,
    }
}

pub(in crate::catalog) fn named_graph_exists(
    snapshot: &NativeSnapshot,
    name: &str,
) -> Result<bool> {
    snapshot.contains_row(Family::NamedGraphs, owner(snapshot), &[text(name)])
}

pub(in crate::catalog) fn vertex(
    snapshot: &NativeSnapshot,
    id: u64,
) -> Result<Option<GraphVertexRow>> {
    let id = encode_catalog_id("vertex", id)?;
    snapshot.read_row(
        Family::GraphVertices,
        owner(snapshot),
        &[ValueRef::Integer(id)],
        vertex_row,
    )
}

fn vertex_row(row: &[ValueRef<'_>]) -> Result<GraphVertexRow> {
    Ok(GraphVertexRow {
        vertex_id: decode_catalog_id("vertex", integer(row[0])?)?,
        label: string(row[1])?,
        properties_json: string(row[2])?,
    })
}

pub(in crate::catalog) fn edge(snapshot: &NativeSnapshot, id: u64) -> Result<Option<EdgeRow>> {
    let id = encode_catalog_id("edge", id)?;
    snapshot.read_row(
        Family::GraphEdges,
        owner(snapshot),
        &[ValueRef::Integer(id)],
        edge_row,
    )
}

fn edge_row(row: &[ValueRef<'_>]) -> Result<EdgeRow> {
    let id = |column: usize, kind| decode_catalog_id(kind, integer(row[column])?);
    Ok(EdgeRow {
        edge_id: id(0, "edge")?,
        source_id: id(1, "edge source")?,
        target_id: id(2, "edge target")?,
        label: string(row[3])?,
        properties_json: string(row[4])?,
    })
}

pub(in crate::catalog) fn ids(
    snapshot: &NativeSnapshot,
    filter: GraphEntityFilter<'_>,
    after: Option<u64>,
    limit: usize,
) -> Result<Vec<u64>> {
    uqa_storage::catalog::validate_graph_page(limit).map_err(crate::SQLiteError::from)?;
    let mut ids = Vec::new();
    snapshot.visit_graph_ids(None, filter, after, |id| {
        ids.push(decode_catalog_id("graph entity", id)?);
        Ok(ids.len() < limit)
    })?;
    Ok(ids)
}

pub(in crate::catalog) fn count(
    snapshot: &NativeSnapshot,
    filter: GraphEntityFilter<'_>,
) -> Result<u64> {
    let mut count = 0_u64;
    snapshot.visit_graph_ids(None, filter, None, |_| {
        count = count.checked_add(1).ok_or_else(|| {
            crate::SQLiteError::StorageBackend("graph entity count overflow".into())
        })?;
        Ok(true)
    })?;
    Ok(count)
}

pub(in crate::catalog) fn max_id(
    snapshot: &NativeSnapshot,
    kind: GraphEntityKind,
) -> Result<Option<u64>> {
    let mut maximum = None;
    snapshot.visit_graph_ids(None, GraphEntityFilter::new(kind, None), None, |id| {
        maximum = Some(id);
        Ok(true)
    })?;
    maximum
        .map(|id| decode_catalog_id("graph entity", id))
        .transpose()
}

pub(in crate::catalog) fn memberships(
    snapshot: &NativeSnapshot,
    kind: GraphEntityKind,
    id: u64,
) -> Result<Vec<String>> {
    let id = encode_catalog_id("graph entity", id)?;
    let mut graphs = Vec::new();
    snapshot.visit_rows(
        Family::GraphMembership,
        Some(owner(snapshot)),
        &[text(kind.as_str()), ValueRef::Integer(id)],
        |row| {
            graphs.push(string(row[2])?);
            Ok(())
        },
    )?;
    Ok(graphs)
}

pub(in crate::catalog) fn has_membership(
    snapshot: &NativeSnapshot,
    kind: GraphEntityKind,
    id: u64,
    graph: &str,
) -> Result<bool> {
    let id = encode_catalog_id("graph entity", id)?;
    snapshot.contains_row(
        Family::GraphMembership,
        owner(snapshot),
        &[text(kind.as_str()), ValueRef::Integer(id), text(graph)],
    )
}

fn owner(snapshot: &NativeSnapshot) -> NativeRecordOwner {
    NativeRecordOwner::Database(snapshot.database)
}

fn integer(value: ValueRef<'_>) -> Result<i64> {
    value.as_i64().map_err(|_| {
        uqa_storage::mvcc::VersionError::InvalidEncoding("native graph identity is not an integer")
            .into()
    })
}
