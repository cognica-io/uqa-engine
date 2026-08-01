//! Graph entity, membership, and AGE label-registry restoration.

use super::{BTreeMap, CatalogFacade, Engine, StorageBackendError, StorageBackendResult};

impl Engine {
    pub(super) fn restore_graphs_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Step 1: register every named graph (the registry table is
        // authoritative for empty graphs).
        let names = catalog.load_named_graphs()?;
        let mut graphs = self.graphs.write();
        for name in &names {
            graphs.entry(name.clone()).or_default();
            if let Some(store) = graphs.get_mut(name) {
                if !store.has_graph(name) {
                    store.create_graph(name);
                }
            }
        }

        // Step 2: load every entity into side tables. Memberships, rather
        // than the global entity rows, determine each graph partition.
        let (vertex_by_id, edge_by_id) = Self::load_graph_entities(catalog)?;
        let memberships = catalog.load_graph_memberships()?;
        Self::restore_graph_memberships(&mut graphs, &memberships, &vertex_by_id, &edge_by_id)?;
        Self::restore_graph_label_registries(&mut graphs, catalog)
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
        graphs: &mut BTreeMap<String, uqa_graph::MemoryGraphStore>,
        memberships: &[(String, u64, String)],
        vertex_by_id: &BTreeMap<u64, uqa_core::Vertex>,
        edge_by_id: &BTreeMap<u64, uqa_core::Edge>,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Validate every membership before mutating a graph, then
        // replay all vertex memberships before any edge membership. Catalog
        // ordering is not part of the persistence contract, and add_edge
        // correctly requires both endpoints to already belong to the target
        // graph.
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
            store
                .add_vertex(vertex.clone(), graph_name)
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
            store
                .add_edge(edge.clone(), graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
    }

    fn restore_graph_label_registries(
        graphs: &mut BTreeMap<String, uqa_graph::MemoryGraphStore>,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Restore the per-graph AGE label registries. The
        // persisted metadata is authoritative (it survives deletion of
        // every entity of a label); deriving label ids from existing
        // entity ids (`id >> 48`) self-heals missing metadata.
        for (graph_name, store) in graphs.iter_mut() {
            let vertices = store
                .vertex_ids_in_graph(graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            for edge in store
                .edges_in_graph(graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?
            {
                if !vertices.contains(&edge.source_id) || !vertices.contains(&edge.target_id) {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` edge {} references missing endpoint {} -> {}",
                        edge.edge_id, edge.source_id, edge.target_id
                    )));
                }
            }
            let key = format!("{}{graph_name}", super::GRAPH_LABELS_METADATA_PREFIX);
            if let Some(json) = catalog.get_metadata(&key)? {
                if !json.is_empty() {
                    let registry = serde_json::from_str::<uqa_graph::GraphLabelRegistry>(&json)?;
                    store.import_label_registry(graph_name, &registry);
                }
            }
            store.rebuild_label_registry_from_ids(graph_name);
        }
        Ok(())
    }
}
