//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original and evaluated graph source fields supply logical observations without replaying graph operations.

use rusqlite::types::ValueRef;
use uqa_storage::catalog::graph_observations::{
    GraphEntityKey, GraphEntityTopology, GraphMembershipKey,
};
use uqa_storage::{GraphEntityKind, KeyValueBatch};

use super::{Family, NativeRecordOwner, NativeSnapshot, Result};
use crate::SQLiteError;

impl NativeSnapshot {
    pub(super) fn observe_graph_row(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        components: &[ValueRef<'_>],
        old: Option<&[ValueRef<'_>]>,
        new: Option<&[ValueRef<'_>]>,
        evaluated: Option<GraphEntityTopology<'_>>,
    ) -> Result<()> {
        if batch.serializable_participant().is_none() || (old.is_none() && new.is_none()) {
            return Ok(());
        }
        let (kind, membership) = match family {
            Family::GraphVertices | Family::StandaloneGraphVertices => {
                (GraphEntityKind::Vertex, false)
            }
            Family::GraphEdges | Family::StandaloneGraphEdges => (GraphEntityKind::Edge, false),
            Family::GraphMembership | Family::StandaloneGraphMembership => {
                if old.is_some() == new.is_some() {
                    return Ok(());
                }
                let offset = usize::from(family.is_standalone_graph());
                let kind = match string(components[offset])? {
                    "vertex" => GraphEntityKind::Vertex,
                    "edge" => GraphEntityKind::Edge,
                    _ => return Err(invalid()),
                };
                (kind, true)
            }
            _ => return Ok(()),
        };
        let offset = usize::from(family.is_standalone_graph());
        let scope = (offset != 0).then(|| string(components[0])).transpose()?;
        let namespace = self.graph_identifier_namespace(scope)?;
        let encoded_id = components[offset + usize::from(membership)];
        let id = identifier(encoded_id)?;
        if membership {
            let graph = string(components[offset + 2])?;
            batch.observe_serializable_write(
                GraphMembershipKey::new(namespace, kind, id, Some(graph)).predicate(),
            )?;
            if let Some(topology) = evaluated {
                topology.observe_write(namespace, id, Some(graph), batch)?;
            } else {
                let entity_family = match (kind, offset != 0) {
                    (GraphEntityKind::Vertex, false) => Family::GraphVertices,
                    (GraphEntityKind::Edge, false) => Family::GraphEdges,
                    (GraphEntityKind::Vertex, true) => Family::StandaloneGraphVertices,
                    (GraphEntityKind::Edge, true) => Family::StandaloneGraphEdges,
                };
                let mut key = [ValueRef::Null; 2];
                key[..offset].copy_from_slice(&components[..offset]);
                key[offset] = encoded_id;
                self.read_row(
                    entity_family,
                    NativeRecordOwner::Database(self.database),
                    &key[..=offset],
                    |row| {
                        topology(kind, &row[offset..])?.observe_write(
                            namespace,
                            id,
                            Some(graph),
                            batch,
                        )?;
                        Ok(())
                    },
                )?;
            }
            return Ok(());
        }
        GraphEntityKey::new(namespace, kind, id).observe_write(batch)?;
        let old = old.map(|row| topology(kind, &row[offset..])).transpose()?;
        let new = new.map(|row| topology(kind, &row[offset..])).transpose()?;
        if old == new {
            return Ok(());
        }
        for value in old.into_iter().chain(new) {
            value.observe_write(namespace, id, None, batch)?;
        }
        let mut prefix = [ValueRef::Null; 3];
        prefix[..offset].copy_from_slice(&components[..offset]);
        prefix[offset] = ValueRef::Text(kind.as_str().as_bytes());
        prefix[offset + 1] = encoded_id;
        self.visit_paged_rows(
            if offset == 0 {
                Family::GraphMembership
            } else {
                Family::StandaloneGraphMembership
            },
            &prefix[..offset + 2],
            |row| {
                let graph = string(row[offset + 2])?;
                for value in old.into_iter().chain(new) {
                    value.observe_write(namespace, id, Some(graph), batch)?;
                }
                Ok(true)
            },
        )
    }
}

fn topology<'a>(kind: GraphEntityKind, row: &[ValueRef<'a>]) -> Result<GraphEntityTopology<'a>> {
    Ok(match kind {
        GraphEntityKind::Vertex => GraphEntityTopology::Vertex {
            label: string(row[1])?,
        },
        GraphEntityKind::Edge => GraphEntityTopology::Edge {
            label: string(row[3])?,
            source: identifier(row[1])?,
            target: identifier(row[2])?,
        },
    })
}

fn string(value: ValueRef<'_>) -> Result<&str> {
    value.as_str().map_err(|_| invalid())
}

fn identifier(value: ValueRef<'_>) -> Result<u64> {
    value
        .as_i64()
        .ok()
        .and_then(|id| id.try_into().ok())
        .ok_or_else(invalid)
}

fn invalid() -> SQLiteError {
    SQLiteError::StorageBackend("invalid evaluated graph observation input".into())
}
