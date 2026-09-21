//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated graph selectors and membership changes share their original source view and batch.

use super::graph_view::GraphRead;
use super::{StoredEdge, StoredVertex};
use crate::catalog::graph_observations::{GraphEntityTopology, GraphMembershipKey};
use crate::{GraphEntityKind, KeyValueBatch, StorageBackendResult};

pub(super) fn vertex(row: &StoredVertex) -> GraphEntityTopology<'_> {
    GraphEntityTopology::Vertex { label: &row.label }
}

pub(super) fn edge(row: &StoredEdge) -> GraphEntityTopology<'_> {
    GraphEntityTopology::Edge {
        label: &row.label,
        source: row.source_id,
        target: row.target_id,
    }
}

impl GraphRead<'_> {
    pub(super) fn observe_topology_change(
        &self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
        graph: Option<&str>,
        old: Option<GraphEntityTopology<'_>>,
        new: Option<GraphEntityTopology<'_>>,
    ) -> StorageBackendResult<()> {
        if batch.serializable_participant().is_none() || old == new {
            return Ok(());
        }
        let namespace = self.identifier_namespace()?;
        for topology in old.into_iter().chain(new) {
            topology.observe_write(namespace, id, graph, batch)?;
        }
        Ok(())
    }

    pub(super) fn observe_membership_change(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: &str,
        id: u64,
        graph: &str,
        evaluated: Option<GraphEntityTopology<'_>>,
    ) -> StorageBackendResult<()> {
        if batch.serializable_participant().is_none() {
            return Ok(());
        }
        let kind = match kind {
            "vertex" => GraphEntityKind::Vertex,
            "edge" => GraphEntityKind::Edge,
            _ => {
                return Err(crate::StorageBackendError::Other(
                    "invalid graph membership kind".into(),
                ))
            }
        };
        let namespace = self.identifier_namespace()?;
        batch.observe_serializable_write(
            GraphMembershipKey::new(namespace, kind, id, Some(graph)).predicate(),
        )?;
        if let Some(topology) = evaluated {
            return topology.observe_write(namespace, id, Some(graph), batch);
        }
        match kind {
            GraphEntityKind::Vertex => {
                if let Some(row) = self.vertex(id)? {
                    GraphEntityTopology::Vertex { label: &row.label }.observe_write(
                        namespace,
                        id,
                        Some(graph),
                        batch,
                    )?;
                }
            }
            GraphEntityKind::Edge => {
                if let Some(row) = self.edge(id)? {
                    GraphEntityTopology::Edge {
                        label: &row.label,
                        source: row.source_id,
                        target: row.target_id,
                    }
                    .observe_write(namespace, id, Some(graph), batch)?;
                }
            }
        }
        Ok(())
    }
}
