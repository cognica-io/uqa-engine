//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute consumed graph catalog projections to the retained query participant.

use super::CatalogReadView;
use std::{borrow::Cow, sync::Arc};
use uqa_core::CancellationToken;
use uqa_graph::{GraphStore, GraphStoreError, GraphStoreHandle};
use uqa_sql::SQLError;
use uqa_storage::{
    catalog::graph_observations::GraphDefinitionKind, mvcc::SerializableReadContext,
    StorageBackendError,
};

pub(super) struct GraphCatalogRead {
    template: GraphStoreHandle,
    context: SerializableReadContext,
    cancellation: CancellationToken,
}

fn graph_error(error: GraphStoreError) -> SQLError {
    crate::storage_errors::storage_error(
        "read graph catalog",
        &StorageBackendError::backend("graph", error),
    )
}

impl CatalogReadView {
    /// Retain the original participant independently from the catalog's immutable definitions. Binding alone records no reads.
    pub fn with_graph_reads(
        mut self,
        template: GraphStoreHandle,
        context: SerializableReadContext,
        cancellation: &CancellationToken,
    ) -> Self {
        self.graph_reads = Some(Arc::new(GraphCatalogRead {
            template: template.with_serializable_read(context.clone(), cancellation),
            context,
            cancellation: cancellation.clone(),
        }));
        self
    }

    fn graph_read(&self, name: &str) -> Result<Option<Cow<'_, GraphStoreHandle>>, SQLError> {
        if let Some(read) = &self.graph_reads {
            read.template
                .observe_definition(GraphDefinitionKind::NamedGraph, Some(name))
                .map_err(graph_error)?;
        }
        Ok(self.snapshot.definitions.graphs.get(name).map(|store| {
            match (&self.graph_reads, store.as_ref()) {
                (Some(read), GraphStoreHandle::Persistent(store)) => Cow::Owned(
                    GraphStoreHandle::Persistent(store.clone())
                        .with_serializable_read(read.context.clone(), &read.cancellation),
                ),
                _ => Cow::Borrowed(store.as_ref()),
            }
        }))
    }

    /// Catalog row consumption observes the complete name set, including an empty result.
    pub fn read_graph_names(&self) -> Result<Vec<String>, SQLError> {
        self.observe_graph_names()?;
        Ok(self.graph_names())
    }

    pub(super) fn observe_graph_names(&self) -> Result<(), SQLError> {
        if let Some(read) = &self.graph_reads {
            read.template
                .observe_definition(GraphDefinitionKind::NamedGraph, None)
                .map_err(graph_error)?;
        }
        Ok(())
    }

    pub fn graph_labels(
        &self,
        graph: &str,
    ) -> Result<Option<Vec<uqa_graph::GraphLabelInfo>>, SQLError> {
        self.graph_read(graph)?
            .map(|store| store.graph_labels(graph))
            .transpose()
            .map_err(graph_error)
    }

    /// Unobserved names for binding and restoration; row consumers use `read_graph_names`.
    pub fn graph_names(&self) -> Vec<String> {
        self.snapshot.definitions.graphs.keys().cloned().collect()
    }

    pub fn graph_next_label_id(&self, graph: &str) -> Result<Option<u32>, SQLError> {
        self.graph_read(graph)?
            .map(|store| {
                store
                    .label_registry(graph)
                    .map(|registry| registry.next_label_id)
            })
            .transpose()
            .map_err(graph_error)
    }

    pub fn graph_label_count(
        &self,
        graph: &str,
        label: &str,
        kind: uqa_graph::LabelKind,
    ) -> Result<Option<usize>, SQLError> {
        self.graph_read(graph)?
            .map(|store| match kind {
                uqa_graph::LabelKind::Vertex => store
                    .vertex_ids_by_label(label, graph)
                    .map(|identities| identities.len()),
                uqa_graph::LabelKind::Edge => store
                    .edge_ids_by_label(label, graph)
                    .map(|identities| identities.len()),
            })
            .transpose()
            .map_err(graph_error)
    }

    pub fn graph_vertices(&self, graph: &str) -> Result<Option<Vec<uqa_core::Vertex>>, SQLError> {
        self.graph_read(graph)?
            .map(|store| store.vertices_in_graph(graph))
            .transpose()
            .map_err(graph_error)
    }

    pub fn graph_edges(&self, graph: &str) -> Result<Option<Vec<uqa_core::Edge>>, SQLError> {
        self.graph_read(graph)?
            .map(|store| store.edges_in_graph(graph))
            .transpose()
            .map_err(graph_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_graph_reads_preserve_cancellation_resource_and_serialization_diagnostics() {
        let memory = uqa_core::memory::MemoryBudget::new(0)
            .reserve(1)
            .unwrap_err();
        for (error, state) in [
            (
                GraphStoreError::from(StorageBackendError::Cancelled(uqa_core::QueryCancelled)),
                "57014",
            ),
            (
                GraphStoreError::from(StorageBackendError::Memory(memory)),
                "53200",
            ),
            (
                GraphStoreError::SerializationFailure("completed reader".into()),
                "40001",
            ),
        ] {
            assert_eq!(graph_error(error).sqlstate(), Some(state));
        }
    }
}
