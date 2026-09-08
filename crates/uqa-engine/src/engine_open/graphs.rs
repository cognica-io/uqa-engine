//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph entity, membership, and AGE label-registry restoration.

use super::{BTreeMap, CatalogFacade, Engine, StorageBackendError, StorageBackendResult};
use std::collections::BTreeSet;
use std::sync::Arc;

impl Engine {
    pub(super) fn restore_graphs_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        // Rollback recovery and load-only session construction can arrive
        // without a backend transaction. A shared version must always pair
        // its generation and contents from one pinned storage snapshot.
        let owned_snapshot = self
            .storage
            .backend
            .as_ref()
            .filter(|backend| !backend.in_transaction());
        if let Some(backend) = owned_snapshot {
            backend.begin_read_transaction()?;
        }
        let result = self.restore_pinned_graphs_from_catalog(catalog);
        let cleanup = owned_snapshot.map_or(Ok(()), |backend| backend.rollback_transaction());
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(StorageBackendError::Other(format!(
                "graph restoration failed: {error}; snapshot cleanup failed: {cleanup}"
            ))),
        }
    }

    fn restore_pinned_graphs_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        if let Some(current) = catalog
            .cache_revisions()?
            .and_then(|revisions| revisions.graphs)
        {
            let previous = self.epochs.storage_cache_revisions.lock().clone();
            self.refresh_graph_snapshots(
                catalog,
                previous
                    .as_ref()
                    .and_then(|revisions| revisions.graphs.as_ref()),
                &current,
            )?;
            return Ok(());
        }

        // Providers without graph generations keep the full, validated load.
        // Replace the outer allocation instead of copy-on-writing every graph
        // just to immediately throw the copies away.
        self.durable.graphs.restore(&Arc::new(BTreeMap::new()));

        // Step 1: register every named graph (the registry table is
        // authoritative for empty graphs).
        let names = catalog.load_named_graphs()?;
        let mut graphs = self.durable.graphs.write();
        for name in &names {
            graphs.entry(name.clone()).or_default();
            if let Some(store) = graphs.get_mut(name) {
                if !store.has_graph(name) {
                    Arc::make_mut(store).create_graph(name);
                }
            }
        }

        // Label tombstones must be installed before edge memberships: AGE
        // keeps edge rows whose endpoints belonged to a dropped vertex-label
        // relation, and those dangling endpoints are valid broken-graph state.
        Self::import_graph_label_registries(&mut graphs, catalog)?;

        // Step 2: load every entity into side tables. Memberships, rather
        // than the global entity rows, determine each graph partition.
        let (vertex_by_id, edge_by_id) = Self::load_graph_entities(catalog)?;
        let memberships = catalog.load_graph_memberships()?;
        Self::restore_graph_memberships(&mut graphs, &memberships, &vertex_by_id, &edge_by_id)?;
        Self::restore_graph_label_registries(&mut graphs)
    }

    fn import_graph_label_registries(
        graphs: &mut BTreeMap<String, Arc<uqa_graph::MemoryGraphStore>>,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (graph_name, store) in graphs.iter_mut() {
            let key = format!("{}{graph_name}", super::GRAPH_LABELS_METADATA_PREFIX);
            if let Some(json) = catalog.get_metadata(&key)? {
                if !json.is_empty() {
                    let registry = serde_json::from_str::<uqa_graph::GraphLabelRegistry>(&json)?;
                    Arc::make_mut(store).import_label_registry(graph_name, &registry);
                }
            }
        }
        Ok(())
    }

    fn load_graph_entities(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<(
        BTreeMap<u64, uqa_core::Vertex>,
        BTreeMap<u64, uqa_core::Edge>,
    )> {
        let vertex_rows = catalog.load_vertices()?;
        let mut vertex_by_id: BTreeMap<u64, uqa_core::Vertex> = BTreeMap::new();
        for (id, label, props_json) in vertex_rows {
            let properties: BTreeMap<String, uqa_core::Value> = serde_json::from_str(&props_json)?;
            vertex_by_id.insert(
                id,
                uqa_core::Vertex {
                    vertex_id: id,
                    label,
                    properties,
                },
            );
        }
        let edge_rows = catalog.load_edges()?;
        let mut edge_by_id: BTreeMap<u64, uqa_core::Edge> = BTreeMap::new();
        for row in edge_rows {
            let properties: BTreeMap<String, uqa_core::Value> =
                serde_json::from_str(&row.properties_json)?;
            edge_by_id.insert(
                row.edge_id,
                uqa_core::Edge {
                    edge_id: row.edge_id,
                    source_id: row.source_id,
                    target_id: row.target_id,
                    label: row.label,
                    properties,
                },
            );
        }
        Ok((vertex_by_id, edge_by_id))
    }

    fn restore_graph_memberships(
        graphs: &mut BTreeMap<String, Arc<uqa_graph::MemoryGraphStore>>,
        memberships: &[(String, u64, String)],
        vertex_by_id: &BTreeMap<u64, uqa_core::Vertex>,
        edge_by_id: &BTreeMap<u64, uqa_core::Edge>,
    ) -> StorageBackendResult<()> {
        // Validate every membership before mutating a graph, then
        // hydrate all vertex memberships before edge memberships. Catalog row
        // order is not part of the persistence contract; edge attachment uses
        // the imported label tombstones to admit AGE's persisted dangling
        // endpoints without weakening normal add_edge validation.
        for (entity_type, entity_id, graph_name) in memberships {
            if !graphs.contains_key(graph_name) {
                return Err(StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph_name}`"
                )));
            }
            match entity_type.as_str() {
                "vertex" if vertex_by_id.contains_key(entity_id) => {}
                "vertex" => {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` references missing vertex {entity_id}"
                    )));
                }
                "edge" if edge_by_id.contains_key(entity_id) => {}
                "edge" => {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` references missing edge {entity_id}"
                    )));
                }
                other => {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` has invalid membership type `{other}`"
                    )));
                }
            }
        }
        for (entity_type, entity_id, graph_name) in memberships {
            if entity_type != "vertex" {
                continue;
            }
            let store = graphs.get_mut(graph_name).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph_name}`"
                ))
            })?;
            let vertex = vertex_by_id.get(entity_id).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph `{graph_name}` references missing vertex {entity_id}"
                ))
            })?;
            let store = Arc::make_mut(store);
            store
                .insert_raw_vertex(vertex.clone())
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            store
                .attach_vertex(*entity_id, graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        for (entity_type, entity_id, graph_name) in memberships {
            if entity_type != "edge" {
                continue;
            }
            let store = graphs.get_mut(graph_name).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph_name}`"
                ))
            })?;
            let edge = edge_by_id.get(entity_id).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph `{graph_name}` references missing edge {entity_id}"
                ))
            })?;
            let store = Arc::make_mut(store);
            store
                .insert_raw_edge(edge.clone())
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            store
                .attach_edge(*entity_id, graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
    }

    fn restore_graph_label_registries(
        graphs: &mut BTreeMap<String, Arc<uqa_graph::MemoryGraphStore>>,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Persisted metadata was imported before memberships. Validate the
        // resulting partitions, then derive any labels missing from legacy
        // metadata from entity ids (`id >> 48`).
        for (graph_name, store) in graphs.iter_mut() {
            store
                .vertex_ids_in_graph(graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            store
                .edges_in_graph(graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            Arc::make_mut(store).rebuild_label_registry_from_ids(graph_name);
        }
        Ok(())
    }

    pub(super) fn refresh_graph_snapshots(
        &self,
        catalog: &dyn CatalogFacade,
        previous: Option<&BTreeMap<String, u64>>,
        current: &BTreeMap<String, u64>,
    ) -> StorageBackendResult<BTreeSet<String>> {
        let changed = match previous {
            Some(previous) => previous
                .keys()
                .chain(current.keys())
                .filter(|name| previous.get(*name) != current.get(*name))
                .cloned()
                .collect(),
            None => current
                .keys()
                .chain(self.durable.graphs.read().keys())
                .cloned()
                .collect::<BTreeSet<_>>(),
        };
        if changed.is_empty() {
            return Ok(changed);
        }
        let private = self
            .storage
            .backend
            .as_ref()
            .map(|backend| backend.transaction_has_written())
            .transpose()?
            .unwrap_or(false);
        // Load without holding the session's graph map. A failed load leaves
        // the old immutable snapshot untouched and the generation unobserved.
        let mut replacements = Vec::new();
        for name in &changed {
            let load = || Self::load_named_graph(catalog, name);
            let graph = if private {
                load()?.map(Arc::new)
            } else {
                self.row_locks.graph_snapshots.load(
                    name,
                    current.get(name).copied().unwrap_or_default(),
                    load,
                )?
            };
            replacements.push((name, graph));
        }
        let mut graphs = self.durable.graphs.write();
        for (name, graph) in replacements {
            if let Some(graph) = graph {
                graphs.insert(name.clone(), graph);
            } else {
                graphs.remove(name);
            }
        }
        Ok(changed)
    }

    fn load_named_graph(
        catalog: &dyn CatalogFacade,
        name: &str,
    ) -> StorageBackendResult<Option<uqa_graph::MemoryGraphStore>> {
        use uqa_graph::GraphStore as _;
        let Some(snapshot) = catalog.load_named_graph_snapshot(name)? else {
            return Ok(None);
        };
        let mut graph = uqa_graph::MemoryGraphStore::new();
        graph.create_graph(name);
        if !snapshot.label_registry_json.is_empty() {
            graph
                .import_label_registry(name, &serde_json::from_str(&snapshot.label_registry_json)?);
        }
        for vertex in snapshot.vertices {
            let id = vertex.vertex_id;
            graph
                .insert_raw_vertex(uqa_core::Vertex {
                    vertex_id: id,
                    label: vertex.label,
                    properties: serde_json::from_str(&vertex.properties_json)?,
                })
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            graph
                .attach_vertex(id, name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        for edge in snapshot.edges {
            let id = edge.edge_id;
            graph
                .insert_raw_edge(uqa_core::Edge {
                    edge_id: id,
                    source_id: edge.source_id,
                    target_id: edge.target_id,
                    label: edge.label,
                    properties: serde_json::from_str(&edge.properties_json)?,
                })
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            graph
                .attach_edge(id, name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        graph
            .vertex_ids_in_graph(name)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        graph
            .edges_in_graph(name)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        graph.rebuild_label_registry_from_ids(name);
        Ok(Some(graph))
    }
}
