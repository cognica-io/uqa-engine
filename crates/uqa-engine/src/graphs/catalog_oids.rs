//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The catalog OIDs of graphs and labels, drawn from the database's counter when a graph write publishes them and recorded with the catalog. A graph created before OIDs were recorded keeps deriving the OIDs of its schema and every label from their names.

use std::collections::BTreeSet;

use uqa_execution::catalog::identity::LabelShape;
use uqa_graph::{GraphStore as _, GraphStoreHandle, LabelKind};
use uqa_sql::catalog::graph_oids::GraphCatalogOids;
use uqa_storage::StorageBackendResult;

use crate::Engine;

impl Engine {
    /// Allocate and record the OIDs of the graphs and labels `candidate` holds that the published graphs do not, in the order AGE creates them: a new graph's schema, label id sequence and default labels, and then each label it gained by ascending label id.
    pub(super) fn record_created_graph_oids(
        &self,
        candidate: &GraphStoreHandle,
    ) -> StorageBackendResult<()> {
        let published = self.published_graph_names();
        let mut recorded = self.durable.graph_catalog_oids.read().clone();
        let mut changed = BTreeSet::new();
        for graph in candidate.graph_names().map_err(super::graph_store_error)? {
            if !published.contains(&graph) && !recorded.contains_key(&graph) {
                let oids = self
                    .catalog_identity_reservation_context()
                    .allocator(uqa_execution::catalog::identity::allocate_catalog_object_id)
                    .allocate_graph_oids(|oid| {
                        Ok(
                            uqa_execution::schema::namespaces::identity::namespace_oid_in_use(
                                self, oid,
                            ),
                        )
                    })
                    .map_err(|error| {
                        uqa_storage::StorageBackendError::backend("graph OIDs", error)
                    })?;
                recorded.insert(graph.clone(), oids);
                changed.insert(graph.clone());
            }
            let Some(oids) = recorded.get_mut(&graph) else {
                continue;
            };
            for label in candidate
                .graph_labels(&graph)
                .map_err(super::graph_store_error)?
            {
                if oids.labels.contains_key(&label.id) {
                    continue;
                }
                let label_oids = self
                    .catalog_identity_reservation_context()
                    .allocator(uqa_execution::catalog::identity::allocate_catalog_object_id)
                    .allocate_label_oids(LabelShape {
                        edge: label.kind == LabelKind::Edge,
                        default: false,
                    })
                    .map_err(|error| {
                        uqa_storage::StorageBackendError::backend("label OIDs", error)
                    })?;
                oids.labels.insert(label.id, label_oids);
                changed.insert(graph.clone());
            }
        }
        for graph in &changed {
            self.persist_graph_catalog_oids(graph, recorded.get(graph))?;
        }
        if !changed.is_empty() {
            *self.durable.graph_catalog_oids.write() = recorded;
        }
        Ok(())
    }

    /// Forget a removed label's OIDs; a graph created before OIDs were recorded has none.
    pub(super) fn forget_graph_label_oids(
        &self,
        graph: &str,
        label_ids: &[u32],
    ) -> StorageBackendResult<()> {
        let mut recorded = self.durable.graph_catalog_oids.read().clone();
        let Some(oids) = recorded.get_mut(graph) else {
            return Ok(());
        };
        for id in label_ids {
            oids.labels.remove(id);
        }
        self.persist_graph_catalog_oids(graph, recorded.get(graph))?;
        *self.durable.graph_catalog_oids.write() = recorded;
        Ok(())
    }

    /// Forget a removed graph's OIDs.
    pub(super) fn forget_graph_oids(&self, graph: &str) -> StorageBackendResult<()> {
        if self.durable.graph_catalog_oids.read().contains_key(graph) {
            self.persist_graph_catalog_oids(graph, None)?;
            self.durable.graph_catalog_oids.write().remove(graph);
        }
        Ok(())
    }

    /// A renamed graph keeps its OIDs under its new name.
    pub(super) fn rename_graph_oids(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        let Some(oids) = self.durable.graph_catalog_oids.read().get(from).cloned() else {
            return Ok(());
        };
        self.persist_graph_catalog_oids(from, None)?;
        self.persist_graph_catalog_oids(to, Some(&oids))?;
        let mut recorded = self.durable.graph_catalog_oids.write();
        recorded.remove(from);
        recorded.insert(to.to_string(), oids);
        Ok(())
    }

    fn persist_graph_catalog_oids(
        &self,
        graph: &str,
        oids: Option<&GraphCatalogOids>,
    ) -> StorageBackendResult<()> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        match oids {
            Some(oids) => uqa_execution::catalog::graph_oids::record(catalog.as_ref(), graph, oids),
            None => uqa_execution::catalog::graph_oids::forget(catalog.as_ref(), graph),
        }
    }

    /// The graphs visible before the write being published: the session's transaction overlay, or the published registry.
    fn published_graph_names(&self) -> BTreeSet<String> {
        if let Some(overlay) = &self.session.state.read().graph_overlay {
            return overlay.names.iter().cloned().collect();
        }
        self.durable.graphs.read().keys().cloned().collect()
    }
}
