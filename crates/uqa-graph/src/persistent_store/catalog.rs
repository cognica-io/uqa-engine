//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-bound catalog access; transaction ownership stays in the backend.

use std::sync::Arc;

use uqa_core::{Edge, Vertex};
use uqa_storage::{CatalogFacade, GraphEntityFilter, GraphEntityKind, PersistentStorageBackend};

use super::storage::{GraphStorage, GraphWriteTransaction};
use crate::{GraphLabelRegistry, GraphStoreError, GraphStoreResult};

pub(super) struct CatalogGraphStorage {
    pub catalog: Arc<dyn CatalogFacade>,
    pub backend: Arc<dyn PersistentStorageBackend>,
}

fn registry_key(graph: &str) -> String {
    format!("graph_label_registry::{graph}")
}
fn counter_key(kind: GraphEntityKind) -> String {
    format!("graph_next_{}_id", kind.as_str())
}
fn json_error(error: &serde_json::Error) -> GraphStoreError {
    GraphStoreError::CorruptGraph(error.to_string())
}

impl GraphStorage for CatalogGraphStorage {
    fn begin_write(&self) -> GraphStoreResult<Box<dyn GraphWriteTransaction>> {
        super::storage::begin_graph_write(Arc::clone(&self.backend))
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        Ok(self.catalog.load_named_graphs()?)
    }
    fn has_graph(&self, graph: &str) -> GraphStoreResult<bool> {
        Ok(self.catalog.named_graph_exists(graph)?)
    }
    fn create_graph(&self, graph: &str) -> GraphStoreResult<()> {
        Ok(self.catalog.save_named_graph(graph)?)
    }
    fn delete_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.catalog.drop_named_graph(graph)?;
        self.catalog.set_metadata(&registry_key(graph), "")?;
        let prefix = format!("{graph}::");
        for (key, _) in self.catalog.load_path_indexes()? {
            if key.starts_with(&prefix) {
                self.catalog.drop_path_index(&key)?;
            }
        }
        Ok(())
    }
    fn registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        self.catalog
            .get_metadata(&registry_key(graph))?
            .filter(|json| !json.is_empty())
            .map(|json| serde_json::from_str(&json).map_err(|error| json_error(&error)))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    fn save_registry(&self, graph: &str, registry: &GraphLabelRegistry) -> GraphStoreResult<()> {
        Ok(self.catalog.set_metadata(
            &registry_key(graph),
            &serde_json::to_string(registry).map_err(|error| json_error(&error))?,
        )?)
    }
    fn counter(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.catalog
            .get_metadata(&counter_key(kind))?
            .map(|text| {
                text.parse::<u64>()
                    .map_err(|error| GraphStoreError::CorruptGraph(error.to_string()))
            })
            .transpose()
    }
    fn save_counter(&self, kind: GraphEntityKind, next: u64) -> GraphStoreResult<()> {
        Ok(self
            .catalog
            .set_metadata(&counter_key(kind), &next.to_string())?)
    }
    fn vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>> {
        self.catalog
            .graph_vertex(id)?
            .map(|row| {
                Ok(Vertex {
                    vertex_id: id,
                    label: row.label,
                    properties: serde_json::from_str(&row.properties_json)
                        .map_err(|error| json_error(&error))?,
                })
            })
            .transpose()
    }
    fn edge(&self, id: u64) -> GraphStoreResult<Option<Edge>> {
        self.catalog
            .graph_edge(id)?
            .map(|row| {
                Ok(Edge {
                    edge_id: id,
                    source_id: row.source_id,
                    target_id: row.target_id,
                    label: row.label,
                    properties: serde_json::from_str(&row.properties_json)
                        .map_err(|error| json_error(&error))?,
                })
            })
            .transpose()
    }
    fn save_vertex(&self, vertex: &Vertex) -> GraphStoreResult<()> {
        Ok(self.catalog.save_vertex(
            vertex.vertex_id,
            &vertex.label,
            &serde_json::to_string(&vertex.properties).map_err(|error| json_error(&error))?,
        )?)
    }
    fn save_edge(&self, edge: &Edge) -> GraphStoreResult<()> {
        Ok(self.catalog.save_edge(
            edge.edge_id,
            edge.source_id,
            edge.target_id,
            &edge.label,
            &serde_json::to_string(&edge.properties).map_err(|error| json_error(&error))?,
        )?)
    }
    fn delete_vertex(&self, id: u64) -> GraphStoreResult<()> {
        Ok(self.catalog.delete_vertex(id)?)
    }
    fn delete_edge(&self, id: u64) -> GraphStoreResult<()> {
        Ok(self.catalog.delete_edge(id)?)
    }
    fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        Ok(self.catalog.graph_entity_ids(filter, after, limit)?)
    }
    fn count(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<u64> {
        Ok(self.catalog.graph_entity_count(filter)?)
    }
    fn max_id(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        Ok(self.catalog.graph_entity_max_id(kind)?)
    }
    fn memberships(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<Vec<String>> {
        Ok(self.catalog.graph_entity_memberships(kind, id)?)
    }
    fn has_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> GraphStoreResult<bool> {
        Ok(self.catalog.graph_has_membership(kind, id, graph)?)
    }
    fn attach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        Ok(self
            .catalog
            .save_graph_membership(kind.as_str(), id, graph)?)
    }
    fn detach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        Ok(self
            .catalog
            .delete_graph_membership(kind.as_str(), id, graph)?)
    }
}
