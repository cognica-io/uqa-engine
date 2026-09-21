//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated graph definition requirements and autonomous identity observations.

use super::graph_view::GraphRead;
use super::{single_str_key, string_value, TAG_METADATA, TAG_NAMED_GRAPH};
use crate::catalog::graph_guards::GraphRecordGuard;
use crate::catalog::graph_identifiers::GraphIdentifierNamespace;
use crate::{GraphEntityKind, KeyValueBatch, StorageBackendResult};

impl GraphRead<'_> {
    pub(super) fn observe_entity_write(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<()> {
        if batch.serializable_participant().is_some() {
            crate::catalog::graph_observations::GraphEntityKey::new(
                self.identifier_namespace()?,
                kind,
                id,
            )
            .observe_write(batch)?;
        }
        Ok(())
    }
    pub(super) fn guard_entity_reference(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: GraphEntityKind,
        id: u64,
        graph: Option<&str>,
    ) -> StorageBackendResult<()> {
        if !self.identifiers {
            return Ok(());
        }
        let guard = GraphRecordGuard::new(kind, id);
        batch.require_unchanged(&single_str_key(TAG_METADATA, &guard.lifetime())?)?;
        batch.touch_marker(&single_str_key(TAG_METADATA, &guard.references())?, b"1")?;
        if let Some(graph) = graph {
            batch.require_unchanged(&super::graph_membership_key(kind.as_str(), id, graph)?)?;
            batch.touch_marker(
                &single_str_key(TAG_METADATA, &guard.membership_references(graph))?,
                b"1",
            )?;
        }
        Ok(())
    }

    pub(super) fn fence_entity_lifetime(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<()> {
        if self.identifiers {
            let guard = GraphRecordGuard::new(kind, id);
            batch.fence_record(&single_str_key(TAG_METADATA, &guard.lifetime())?)?;
            batch.fence_record(&single_str_key(TAG_METADATA, &guard.references())?)?;
        }
        Ok(())
    }

    pub(super) fn fence_membership_references(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: &str,
        id: u64,
        graph: &str,
    ) -> StorageBackendResult<()> {
        if self.identifiers {
            if let Some(guard) = GraphRecordGuard::from_kind_name(kind, id) {
                batch.fence_record(&single_str_key(
                    TAG_METADATA,
                    &guard.membership_references(graph),
                )?)?;
            }
        }
        Ok(())
    }

    pub(super) fn identifier_namespace(&self) -> StorageBackendResult<GraphIdentifierNamespace> {
        if !self.identifiers {
            return Ok(GraphIdentifierNamespace::new(None, [0; 16]));
        }
        let generation = self
            .read
            .get(&single_str_key(
                TAG_METADATA,
                "graph_identifier_generation",
            )?)?
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?
            .unwrap_or([0; 16]);
        Ok(GraphIdentifierNamespace::new(None, generation))
    }

    pub(super) fn guard_definition(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: Option<&str>,
    ) -> StorageBackendResult<()> {
        if !self.identifiers {
            return Ok(());
        }
        batch.require_unchanged(&single_str_key(
            TAG_METADATA,
            "graph_identifier_generation",
        )?)?;
        batch.touch_marker(
            &single_str_key(TAG_METADATA, "graph_identifier_data_revision")?,
            &string_value("1"),
        )?;
        if let Some(graph) = graph {
            batch.require_unchanged(&single_str_key(TAG_NAMED_GRAPH, graph)?)?;
            batch.require_unchanged(&single_str_key(
                TAG_METADATA,
                &format!("graph_label_registry::{graph}"),
            )?)?;
            batch.touch_marker(
                &single_str_key(
                    TAG_METADATA,
                    &format!("graph_definition_data_revision::{graph}"),
                )?,
                &string_value("1"),
            )?;
        }
        Ok(())
    }

    pub(super) fn fence_definition(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.guard_definition(batch, None)?;
        if self.identifiers {
            batch.fence_record(&single_str_key(
                TAG_METADATA,
                &format!("graph_definition_data_revision::{graph}"),
            )?)?;
        }
        Ok(())
    }

    pub(super) fn observe_entity(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<()> {
        if self.identifiers {
            self.identifier_namespace()?
                .observe_entity(batch, kind, id)?;
        }
        Ok(())
    }

    pub(super) fn observe_registry(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
        source: &str,
    ) -> StorageBackendResult<()> {
        if self.identifiers {
            self.identifier_namespace()?
                .observe_registry(batch, graph, source)?;
        }
        Ok(())
    }
}
