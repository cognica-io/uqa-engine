//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit graph hydration preserves one committed/private boundary and visits only the selected graph's members.

use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, GraphSnapshot};

use super::{
    decode_catalog_id, edge, edge_row, integer, named_graph_exists, owner, string, text, vertex,
    vertex_row, EdgeRow, Family, GraphEntityKind, NativeSnapshot, Result, ValueRef,
};
use crate::{mvcc::native::NativeRecordIdentity, SQLiteError};

pub(in crate::catalog) fn load_names(snapshot: &NativeSnapshot) -> Result<Vec<String>> {
    let mut names = Vec::new();
    snapshot.visit_rows(Family::NamedGraphs, Some(owner(snapshot)), &[], |row| {
        names.push(string(row[0])?);
        Ok(())
    })?;
    Ok(names)
}

pub(in crate::catalog) fn load_vertices(
    snapshot: &NativeSnapshot,
) -> Result<Vec<(u64, String, String)>> {
    let mut rows = Vec::new();
    snapshot.visit_rows(Family::GraphVertices, Some(owner(snapshot)), &[], |row| {
        let row = vertex_row(row)?;
        rows.push((row.vertex_id, row.label, row.properties_json));
        Ok(())
    })?;
    Ok(rows)
}

pub(in crate::catalog) fn load_edges(snapshot: &NativeSnapshot) -> Result<Vec<EdgeRow>> {
    let mut rows = Vec::new();
    snapshot.visit_rows(Family::GraphEdges, Some(owner(snapshot)), &[], |row| {
        rows.push(edge_row(row)?);
        Ok(())
    })?;
    Ok(rows)
}

pub(in crate::catalog) fn load_memberships(
    snapshot: &NativeSnapshot,
) -> Result<Vec<(String, u64, String)>> {
    let mut members = Vec::new();
    snapshot.visit_rows(
        Family::GraphLookups,
        Some(owner(snapshot)),
        &[text("member")],
        |row| {
            members.push((
                string(row[3])?,
                decode_catalog_id("graph membership entity", integer(row[4])?)?,
                string(row[1])?,
            ));
            Ok(())
        },
    )?;
    Ok(members)
}

pub(in crate::catalog) fn load_snapshot(
    snapshot: &NativeSnapshot,
    name: &str,
) -> Result<Option<GraphSnapshot>> {
    let exists = named_graph_exists(snapshot, name)?;
    let registry = format!("graph_label_registry::{name}");
    let label_registry_json = snapshot
        .read_row(
            Family::Metadata,
            owner(snapshot),
            &[text(&registry)],
            |row| string(row[1]),
        )?
        .unwrap_or_default();
    let mut result = GraphSnapshot {
        vertices: Vec::new(),
        edges: Vec::new(),
        label_registry_json,
    };
    visit_members(snapshot, name, |kind, id| {
        if !exists {
            return Err(SQLiteError::StorageBackend(format!(
                "graph membership references unregistered graph `{name}`"
            )));
        }
        let id = decode_catalog_id("graph membership entity", id)?;
        let missing = || {
            SQLiteError::StorageBackend(format!(
                "graph `{name}` references missing {} {id}",
                kind.as_str()
            ))
        };
        match kind {
            GraphEntityKind::Vertex => result
                .vertices
                .push(vertex(snapshot, id)?.ok_or_else(missing)?),
            GraphEntityKind::Edge => result.edges.push(edge(snapshot, id)?.ok_or_else(missing)?),
        }
        Ok(())
    })?;
    Ok(exists.then_some(result))
}

fn visit_members(
    snapshot: &NativeSnapshot,
    graph: &str,
    mut visit: impl FnMut(GraphEntityKind, i64) -> Result<()>,
) -> Result<()> {
    let prefix = NativeRecordIdentity::new(Family::GraphLookups, owner(snapshot))?.encode_prefix(
        &[text("member"), text(graph), ValueRef::Integer(0)],
        &snapshot.control,
    )?;
    let mut after: Option<BudgetedVec<u8>> = None;
    loop {
        let mut members = BudgetedVec::new(snapshot.control.memory());
        let mut last = BudgetedVec::new(snapshot.control.memory());
        snapshot.view.visit_keys(
            &prefix,
            after.as_deref(),
            64,
            &snapshot.control,
            &mut |key, record| {
                last.clear();
                last.extend_from_slice(key)?;
                if record.live {
                    members.push(decode_member(snapshot, graph, key)?)?;
                }
                Ok(true)
            },
        )?;
        if last.is_empty() {
            break;
        }
        after = Some(last);
        for &(kind, id) in members.iter() {
            visit(kind, id)?;
        }
    }
    Ok(())
}

fn decode_member(
    snapshot: &NativeSnapshot,
    graph: &str,
    key: &[u8],
) -> uqa_storage::mvcc::VersionResult<(GraphEntityKind, i64)> {
    let mut kind = None;
    let mut id = None;
    NativeRecordIdentity::visit_key_components(key, &snapshot.control, |column, value| {
        match column {
            3 => {
                let name = value.as_str().map_err(|_| {
                    VersionError::InvalidEncoding("graph membership kind is not text")
                })?;
                kind = Some(match name {
                    "vertex" => GraphEntityKind::Vertex,
                    "edge" => GraphEntityKind::Edge,
                    _ => {
                        return Err(VersionError::Storage(
                            SQLiteError::StorageBackend(format!(
                                "graph `{graph}` has invalid membership type `{name}`"
                            ))
                            .into(),
                        ));
                    }
                });
            }
            4 => {
                id = Some(value.as_i64().map_err(|_| {
                    VersionError::InvalidEncoding("graph membership identity is not an integer")
                })?);
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok((
        kind.expect("complete member key"),
        id.expect("complete member key"),
    ))
}
