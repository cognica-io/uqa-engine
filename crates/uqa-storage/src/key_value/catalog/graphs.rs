//! Named graph entities, memberships, snapshots, and cleanup.

use super::{
    decode_value, edge_key, encode_value, graph_membership_graph_prefix, graph_membership_key,
    graph_membership_prefix, key_with_tag, load_single_keys, read_str, read_u64, single_str_key,
    string_value, vertex_key, BTreeSet, CatalogFacade, EdgeRow, GraphSnapshot, KeyValueCatalog,
    StorageBackendResult, StoredEdge, StoredVertex, TAG_EDGE, TAG_METADATA, TAG_NAMED_GRAPH,
    TAG_PATH_INDEX, TAG_VERTEX,
};

impl KeyValueCatalog {
    pub(super) fn save_named_graph_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.store.put(&single_str_key(TAG_NAMED_GRAPH, name)?, &[])
    }

    pub(super) fn drop_named_graph_impl(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_NAMED_GRAPH, name)?)?;
        batch.delete_prefix(&graph_membership_graph_prefix(name)?)?;
        batch.commit()
    }

    pub(super) fn load_named_graphs_impl(&self) -> StorageBackendResult<Vec<String>> {
        load_single_keys(self.store.as_ref(), TAG_NAMED_GRAPH)
    }

    pub(super) fn save_vertex_impl(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let key = vertex_key(vertex_id);
        self.store.put(
            &key,
            &encode_value(&StoredVertex {
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    pub(super) fn delete_vertex_impl(&self, vertex_id: u64) -> StorageBackendResult<()> {
        let key = vertex_key(vertex_id);
        self.store.delete(&key)
    }

    pub(super) fn load_vertices_impl(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_VERTEX))? {
            let mut offset = 1;
            let vertex_id = read_u64(&key, &mut offset)?;
            let stored: StoredVertex = decode_value(&value)?;
            rows.push((vertex_id, stored.label, stored.properties_json));
        }
        Ok(rows)
    }

    pub(super) fn save_edge_impl(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let key = edge_key(edge_id);
        self.store.put(
            &key,
            &encode_value(&StoredEdge {
                source_id,
                target_id,
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    pub(super) fn delete_edge_impl(&self, edge_id: u64) -> StorageBackendResult<()> {
        let key = edge_key(edge_id);
        self.store.delete(&key)
    }

    pub(super) fn load_edges_impl(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_EDGE))? {
            let mut offset = 1;
            let edge_id = read_u64(&key, &mut offset)?;
            let stored: StoredEdge = decode_value(&value)?;
            rows.push(EdgeRow {
                edge_id,
                source_id: stored.source_id,
                target_id: stored.target_id,
                label: stored.label,
                properties_json: stored.properties_json,
            });
        }
        Ok(rows)
    }

    pub(super) fn save_graph_membership_impl(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &graph_membership_key(entity_type, entity_id, graph_name)?,
            &[],
        )
    }

    pub(super) fn delete_graph_membership_impl(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete(&graph_membership_key(entity_type, entity_id, graph_name)?)
    }

    pub(super) fn delete_graph_membership_for_graph_impl(
        &self,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&graph_membership_graph_prefix(graph_name)?)?;
        Ok(())
    }

    pub(super) fn load_graph_memberships_impl(
        &self,
    ) -> StorageBackendResult<Vec<(String, u64, String)>> {
        let mut rows = Vec::new();
        for (key, _) in self.store.scan_prefix(&graph_membership_prefix())? {
            let mut offset = 1;
            let graph_name = read_str(&key, &mut offset)?;
            let entity_type = read_str(&key, &mut offset)?;
            let entity_id = read_u64(&key, &mut offset)?;
            rows.push((entity_type, entity_id, graph_name));
        }
        Ok(rows)
    }

    pub(super) fn purge_orphan_graph_entities_impl(&self) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let vertex_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let edge_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut batch = self.store.batch();
        for (id, _, _) in self.load_vertices()? {
            if !vertex_ids.contains(&id) {
                batch.delete(&vertex_key(id))?;
            }
        }
        for edge in self.load_edges()? {
            if !edge_ids.contains(&edge.edge_id) {
                batch.delete(&edge_key(edge.edge_id))?;
            }
        }
        batch.commit()
    }

    pub(super) fn replace_named_graph_impl(
        &self,
        graph_name: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let mut surviving_vertices = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut surviving_edges = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        surviving_vertices.extend(snapshot.vertices.iter().map(|row| row.vertex_id));
        surviving_edges.extend(snapshot.edges.iter().map(|row| row.edge_id));
        let mut batch = self.store.batch();
        batch.put(&single_str_key(TAG_NAMED_GRAPH, graph_name)?, &[])?;
        batch.delete_prefix(&graph_membership_graph_prefix(graph_name)?)?;
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_PATH_INDEX))? {
            let mut offset = 1;
            let key_name = read_str(&key, &mut offset)?;
            if key_name.starts_with(&format!("{graph_name}::")) {
                batch.delete(&key)?;
            }
        }
        for vertex in &snapshot.vertices {
            batch.put(
                &vertex_key(vertex.vertex_id),
                &encode_value(&StoredVertex {
                    label: vertex.label.clone(),
                    properties_json: vertex.properties_json.clone(),
                })?,
            )?;
            batch.put(
                &graph_membership_key("vertex", vertex.vertex_id, graph_name)?,
                &[],
            )?;
        }
        for edge in &snapshot.edges {
            batch.put(
                &edge_key(edge.edge_id),
                &encode_value(&StoredEdge {
                    source_id: edge.source_id,
                    target_id: edge.target_id,
                    label: edge.label.clone(),
                    properties_json: edge.properties_json.clone(),
                })?,
            )?;
            batch.put(
                &graph_membership_key("edge", edge.edge_id, graph_name)?,
                &[],
            )?;
        }
        batch.put(
            &single_str_key(TAG_METADATA, &format!("graph_label_registry::{graph_name}"))?,
            &string_value(&snapshot.label_registry_json),
        )?;
        for (id, _, _) in self.load_vertices()? {
            if !surviving_vertices.contains(&id) {
                batch.delete(&vertex_key(id))?;
            }
        }
        for edge in self.load_edges()? {
            if !surviving_edges.contains(&edge.edge_id) {
                batch.delete(&edge_key(edge.edge_id))?;
            }
        }
        batch.commit()
    }

    pub(super) fn drop_named_graph_data_impl(&self, graph_name: &str) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let surviving_vertices = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let surviving_edges = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_NAMED_GRAPH, graph_name)?)?;
        batch.delete_prefix(&graph_membership_graph_prefix(graph_name)?)?;
        batch.delete(&single_str_key(
            TAG_METADATA,
            &format!("graph_label_registry::{graph_name}"),
        )?)?;
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_PATH_INDEX))? {
            let mut offset = 1;
            let key_name = read_str(&key, &mut offset)?;
            if key_name.starts_with(&format!("{graph_name}::")) {
                batch.delete(&key)?;
            }
        }
        for (id, _, _) in self.load_vertices()? {
            if !surviving_vertices.contains(&id) {
                batch.delete(&vertex_key(id))?;
            }
        }
        for edge in self.load_edges()? {
            if !surviving_edges.contains(&edge.edge_id) {
                batch.delete(&edge_key(edge.edge_id))?;
            }
        }
        batch.commit()
    }
}
