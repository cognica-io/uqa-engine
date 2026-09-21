//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated graph selectors and membership changes share their original source view and batch.

use super::graph_view::GraphRead;
use super::{StoredEdge, StoredVertex};
use crate::catalog::graph_observations::{
    observe_label_registry_change, GraphDefinitionKey, GraphDefinitionKind, GraphEntityTopology,
    GraphMembershipKey,
};
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
    pub(super) fn observe_definition_change(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: GraphDefinitionKind,
        name: &str,
        new: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        if batch.serializable_participant().is_none() {
            return Ok(());
        }
        let key = match kind {
            GraphDefinitionKind::NamedGraph => super::single_str_key(super::TAG_NAMED_GRAPH, name)?,
            GraphDefinitionKind::PathIndex => super::single_str_key(super::TAG_PATH_INDEX, name)?,
            GraphDefinitionKind::LabelRegistry => super::single_str_key(
                super::TAG_METADATA,
                &format!("graph_label_registry::{name}"),
            )?,
        };
        let old = self.read.get(&key)?;
        if old.as_deref() != new {
            batch.observe_serializable_write(
                GraphDefinitionKey::new(self.identifier_namespace()?, kind, Some(name)).predicate(),
            )?;
        }
        Ok(())
    }

    pub(super) fn observe_label_registry_change(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
        new: Option<&str>,
    ) -> StorageBackendResult<()> {
        if batch.serializable_participant().is_none() {
            return Ok(());
        }
        let old = self.read.get(&super::single_str_key(
            super::TAG_METADATA,
            &format!("graph_label_registry::{graph}"),
        )?)?;
        let old = old
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|error| {
                crate::StorageBackendError::Other(format!("invalid UTF-8 graph registry: {error}"))
            })?;
        observe_label_registry_change(
            self.identifier_namespace()?,
            graph,
            old,
            new,
            batch,
            self.read.control(),
        )
    }

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
