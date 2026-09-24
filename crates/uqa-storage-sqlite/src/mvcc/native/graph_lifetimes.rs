//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Entity and membership references retain topology without fencing property-only writes.

use rusqlite::types::ValueRef;
use uqa_storage::{catalog::graph_guards::GraphRecordGuard, GraphEntityKind, KeyValueBatch};

use super::{
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot,
};
use crate::{Result, SQLiteError};

fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}

fn string(value: ValueRef<'_>) -> Result<&str> {
    value
        .as_str()
        .map_err(|_| SQLiteError::StorageBackend("invalid graph reference text".into()))
}

fn identifier(value: ValueRef<'_>) -> Result<u64> {
    value
        .as_i64()
        .ok()
        .and_then(|id| u64::try_from(id).ok())
        .ok_or_else(|| SQLiteError::StorageBackend("invalid graph reference identity".into()))
}

impl NativeSnapshot {
    fn graph_reference(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
        kind: GraphEntityKind,
        id: u64,
        graph: Option<&str>,
    ) -> Result<()> {
        let guard = GraphRecordGuard::new(kind, id);
        batch.require_unchanged(self.graph_guard_metadata(scope, &guard.lifetime())?.key())?;
        let marker = self.graph_guard_metadata(scope, &guard.references())?;
        batch.touch_marker(marker.key(), marker.row())?;
        if let Some(graph) = graph {
            let id = ValueRef::Integer(i64::try_from(id).map_err(|_| {
                SQLiteError::StorageBackend("graph reference identity is out of range".into())
            })?);
            let components = [
                text(scope.unwrap_or_default()),
                text(kind.as_str()),
                id,
                text(graph),
            ];
            let family = if scope.is_some() {
                Family::StandaloneGraphMembership
            } else {
                Family::GraphMembership
            };
            let key =
                NativeRecordIdentity::new(family, NativeRecordOwner::Database(self.database))?
                    .encode_key(&components[usize::from(scope.is_none())..], &self.control)?;
            batch.require_unchanged(&key)?;
            let marker = self.graph_guard_metadata(scope, &guard.membership_references(graph))?;
            batch.touch_marker(marker.key(), marker.row())?;
        }
        Ok(())
    }

    fn graph_endpoint_references(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
        edge: &[ValueRef<'_>],
        graph: Option<&str>,
    ) -> Result<()> {
        for endpoint in &edge[1..3] {
            self.graph_reference(
                batch,
                scope,
                GraphEntityKind::Vertex,
                identifier(*endpoint)?,
                graph,
            )?;
        }
        Ok(())
    }

    pub(crate) fn guard_graph_row_lifetimes(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        components: &[ValueRef<'_>],
        replacement: Option<&[ValueRef<'_>]>,
    ) -> Result<()> {
        let kind = match family {
            Family::GraphVertices | Family::StandaloneGraphVertices => {
                Some(GraphEntityKind::Vertex)
            }
            Family::GraphEdges | Family::StandaloneGraphEdges => Some(GraphEntityKind::Edge),
            Family::GraphMembership | Family::StandaloneGraphMembership => None,
            _ => return Ok(()),
        };
        let offset = usize::from(family.is_standalone_graph());
        let scope = if offset == 0 {
            None
        } else {
            Some(string(components[0])?)
        };
        let owner = NativeRecordOwner::Database(self.database);
        let row = replacement.map(|row| &row[offset..]);
        if let Some(kind) = kind {
            let id = identifier(components[offset])?;
            let unchanged = self
                .read_row(family, owner, components, |previous| {
                    Ok(row.is_some_and(|row| {
                        kind == GraphEntityKind::Vertex
                            || previous[offset + 1..offset + 3] == row[1..3]
                    }))
                })?
                .unwrap_or(false);
            if !unchanged {
                let guard = GraphRecordGuard::new(kind, id);
                batch.fence_record(self.graph_guard_metadata(scope, &guard.lifetime())?.key())?;
                batch.fence_record(self.graph_guard_metadata(scope, &guard.references())?.key())?;
            }
            if let Some(row) = row.filter(|_| kind == GraphEntityKind::Edge) {
                self.graph_endpoint_references(batch, scope, row, None)?;
                let prefix = [
                    text(scope.unwrap_or_default()),
                    text("edge"),
                    components[offset],
                ];
                let membership = if scope.is_some() {
                    Family::StandaloneGraphMembership
                } else {
                    Family::GraphMembership
                };
                self.visit_paged_rows(
                    membership,
                    &prefix[usize::from(scope.is_none())..],
                    |member| {
                        self.graph_endpoint_references(
                            batch,
                            scope,
                            row,
                            Some(string(member[offset + 2])?),
                        )?;
                        Ok(true)
                    },
                )?;
            }
        } else {
            let name = string(components[offset])?;
            let id = identifier(components[offset + 1])?;
            let graph = string(components[offset + 2])?;
            let Some(guard) = GraphRecordGuard::from_kind_name(name, id) else {
                return Ok(());
            };
            if replacement.is_some() {
                let kind = if name == "vertex" {
                    GraphEntityKind::Vertex
                } else {
                    GraphEntityKind::Edge
                };
                self.graph_reference(batch, scope, kind, id, None)?;
                if kind == GraphEntityKind::Edge {
                    let edge_family = if scope.is_some() {
                        Family::StandaloneGraphEdges
                    } else {
                        Family::GraphEdges
                    };
                    let key = [text(scope.unwrap_or_default()), components[offset + 1]];
                    self.read_row(
                        edge_family,
                        owner,
                        &key[usize::from(scope.is_none())..],
                        |edge| {
                            self.graph_endpoint_references(
                                batch,
                                scope,
                                &edge[offset..],
                                Some(graph),
                            )
                        },
                    )?;
                }
            } else {
                batch.fence_record(
                    self.graph_guard_metadata(scope, &guard.membership_references(graph))?
                        .key(),
                )?;
            }
        }
        Ok(())
    }
}
