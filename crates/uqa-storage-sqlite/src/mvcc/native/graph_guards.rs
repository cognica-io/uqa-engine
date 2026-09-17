//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native addresses for graph definition/data revision guards.

use rusqlite::types::ValueRef;
use uqa_storage::{
    catalog::graph_identifiers::GraphIdentifierNamespace, GraphEntityKind, KeyValueBatch,
};

use super::{
    NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
    NativeSnapshot,
};
use crate::Result;

fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}

impl NativeSnapshot {
    pub(crate) fn graph_identifier_namespace(
        &self,
        scope: Option<&str>,
    ) -> Result<GraphIdentifierNamespace> {
        let owner = NativeRecordOwner::Database(self.database);
        let value = match scope {
            None => self.read_row(
                Family::Metadata,
                owner,
                &[text("graph_identifier_generation")],
                |row| {
                    Ok(serde_json::from_str(row[1].as_str().map_err(|_| {
                        crate::SQLiteError::StorageBackend(
                            "invalid graph allocation generation".into(),
                        )
                    })?)?)
                },
            )?,
            Some(scope) => self.read_row(
                Family::StandaloneGraphMetadata,
                owner,
                &[text(scope), text("identifier_generation")],
                |row| {
                    Ok(serde_json::from_str(row[2].as_str().map_err(|_| {
                        crate::SQLiteError::StorageBackend(
                            "invalid graph allocation generation".into(),
                        )
                    })?)?)
                },
            )?,
        };
        Ok(GraphIdentifierNamespace::new(
            scope,
            value.unwrap_or([0; 16]),
        ))
    }

    pub(crate) fn observe_graph_entity(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
        kind: GraphEntityKind,
        id: u64,
    ) -> Result<()> {
        self.graph_identifier_namespace(scope)?
            .observe_entity(batch, kind, id)?;
        Ok(())
    }

    pub(crate) fn observe_graph_registry(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
        graph: &str,
        registry: &str,
    ) -> Result<()> {
        self.graph_identifier_namespace(scope)?
            .observe_registry(batch, graph, registry)?;
        Ok(())
    }

    fn graph_guard_metadata(&self, scope: Option<&str>, key: &str) -> Result<NativeRecord> {
        let owner = NativeRecordOwner::Database(self.database);
        Ok(match scope {
            None => NativeRecord::encode(
                Family::Metadata,
                owner,
                &[text(key), text("1")],
                &self.control,
            )?,
            Some(scope) => NativeRecord::encode(
                Family::StandaloneGraphMetadata,
                owner,
                &[text(scope), text(key), text("1")],
                &self.control,
            )?,
        })
    }

    pub(crate) fn guard_graph_definition(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
        graph: Option<&str>,
    ) -> Result<()> {
        let generation = if scope.is_some() {
            "identifier_generation"
        } else {
            "graph_identifier_generation"
        };
        batch.require_unchanged(self.graph_guard_metadata(scope, generation)?.key())?;
        let marker = self.graph_guard_metadata(scope, "graph_identifier_data_revision")?;
        batch.touch_marker(marker.key(), marker.row())?;
        if let Some(graph) = graph {
            let owner = NativeRecordOwner::Database(self.database);
            let key = match scope {
                Some(scope) => NativeRecordIdentity::new(Family::StandaloneGraphCatalog, owner)?
                    .encode_key(&[text(scope), text(graph)], &self.control)?,
                None => NativeRecordIdentity::new(Family::NamedGraphs, owner)?
                    .encode_key(&[text(graph)], &self.control)?,
            };
            batch.require_unchanged(&key)?;
            if scope.is_none() {
                batch.require_unchanged(
                    self.graph_guard_metadata(None, &format!("graph_label_registry::{graph}"))?
                        .key(),
                )?;
            }
            let marker = self
                .graph_guard_metadata(scope, &format!("graph_definition_data_revision::{graph}"))?;
            batch.touch_marker(marker.key(), marker.row())?;
        }
        Ok(())
    }

    pub(crate) fn fence_graph_definition(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
        graph: &str,
    ) -> Result<()> {
        self.guard_graph_definition(batch, scope, None)?;
        batch.fence_record(
            self.graph_guard_metadata(scope, &format!("graph_definition_data_revision::{graph}"))?
                .key(),
        )?;
        Ok(())
    }

    pub(crate) fn fence_graph_identifier_scope(
        &self,
        batch: &mut dyn KeyValueBatch,
        scope: Option<&str>,
    ) -> Result<()> {
        batch.fence_record(
            self.graph_guard_metadata(scope, "graph_identifier_data_revision")?
                .key(),
        )?;
        Ok(())
    }
}
