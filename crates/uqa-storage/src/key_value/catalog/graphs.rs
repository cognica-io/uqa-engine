//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named graph mutations evaluate entities, memberships and definitions on one fixed view.

use super::graph_access::reverse_membership_key;
use super::graph_view::GraphRead;
use super::{
    decode_value, edge_key, encode_value, graph_membership_key, graph_membership_prefix,
    key_with_tag, read_str, read_u64, single_str_key, string_value, vertex_key, EdgeRow,
    GraphSnapshot, KeyValueBatch, KeyValueCatalog, StorageBackendResult, StoredEdge, StoredVertex,
    TAG_EDGE, TAG_METADATA, TAG_NAMED_GRAPH, TAG_PATH_INDEX, TAG_VERTEX,
};
use crate::key_value::view::for_each_key;
use crate::GraphEntityKind;
use uqa_core::memory::BudgetedVec;

impl KeyValueCatalog {
    pub(super) fn guard_graph_definition_impl(
        &self,
        graph: Option<&str>,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| read.guard_definition(batch, graph))
    }

    pub(super) fn save_named_graph_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.guard_definition(batch, None)?;
            let key = single_str_key(TAG_NAMED_GRAPH, name)?;
            if read.read.get(&key)?.is_none() {
                batch.put(&key, &[])?;
            }
            Ok(())
        })
    }

    pub(super) fn drop_named_graph_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.fence_definition(batch, name)?;
            batch.delete(&single_str_key(TAG_NAMED_GRAPH, name)?)?;
            read.delete_graph_memberships_into(batch, name, |_, _| false)
        })
    }

    pub(super) fn load_named_graphs_impl(&self) -> StorageBackendResult<Vec<String>> {
        self.with_graph_read(|read| {
            let mut names = Vec::new();
            for_each_key(read.read, &[TAG_NAMED_GRAPH], &mut |key| {
                names.push(read_str(key, &mut 1)?);
                Ok(true)
            })?;
            names.sort();
            Ok(names)
        })
    }

    pub(super) fn save_vertex_impl(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.save_vertex(batch, vertex_id, label, properties_json)
        })
    }

    pub(super) fn delete_vertex_impl(&self, vertex_id: u64) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.replace_vertex_lookup(batch, vertex_id, None)?;
            batch.delete(&vertex_key(vertex_id))
        })
    }

    pub(super) fn load_vertices_impl(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        self.with_graph_read(|read| {
            let mut rows = Vec::new();
            read.read
                .visit_prefix(&key_with_tag(TAG_VERTEX), &mut |key, value| {
                    let id = read_u64(key, &mut 1)?;
                    let row: StoredVertex = decode_value(value)?;
                    rows.push((id, row.label, row.properties_json));
                    Ok(())
                })?;
            Ok(rows)
        })
    }

    pub(super) fn save_edge_impl(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.save_edge(
                batch,
                &EdgeRow {
                    edge_id,
                    source_id,
                    target_id,
                    label: label.to_owned(),
                    properties_json: properties_json.to_owned(),
                },
            )
        })
    }

    pub(super) fn delete_edge_impl(&self, edge_id: u64) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.replace_edge_lookup(batch, edge_id, None)?;
            batch.delete(&edge_key(edge_id))
        })
    }

    pub(super) fn load_edges_impl(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        self.with_graph_read(|read| {
            let mut rows = Vec::new();
            read.read
                .visit_prefix(&key_with_tag(TAG_EDGE), &mut |key, value| {
                    let edge_id = read_u64(key, &mut 1)?;
                    let row: StoredEdge = decode_value(value)?;
                    rows.push(EdgeRow {
                        edge_id,
                        source_id: row.source_id,
                        target_id: row.target_id,
                        label: row.label,
                        properties_json: row.properties_json,
                    });
                    Ok(())
                })?;
            Ok(rows)
        })
    }

    pub(super) fn save_graph_membership_impl(
        &self,
        kind: &str,
        id: u64,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| read.membership(batch, kind, id, graph, true, None))
    }

    pub(super) fn delete_graph_membership_impl(
        &self,
        kind: &str,
        id: u64,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| read.membership(batch, kind, id, graph, false, None))
    }

    pub(super) fn delete_graph_membership_for_graph_impl(
        &self,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.fence_definition(batch, graph)?;
            batch.fence_record(&single_str_key(TAG_NAMED_GRAPH, graph)?)?;
            read.delete_graph_memberships_into(batch, graph, |_, _| false)
        })
    }

    pub(super) fn load_graph_memberships_impl(
        &self,
    ) -> StorageBackendResult<Vec<(String, u64, String)>> {
        self.with_graph_read(|read| {
            let mut rows = Vec::new();
            for_each_key(read.read, &graph_membership_prefix(), &mut |key| {
                let mut offset = 1;
                let graph = read_str(key, &mut offset)?;
                let kind = read_str(key, &mut offset)?;
                let id = read_u64(key, &mut offset)?;
                rows.push((kind, id, graph));
                Ok(true)
            })?;
            Ok(rows)
        })
    }

    pub(super) fn purge_orphan_graph_entities_impl(&self) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| read.purge(batch, None, |_, _| false))
    }

    pub(super) fn replace_named_graph_impl(
        &self,
        graph: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            let vertices = read.final_rows(&snapshot.vertices, |row| row.vertex_id)?;
            let edges = read.final_rows(&snapshot.edges, |row| row.edge_id)?;
            let keep = |kind, id| {
                let selected = if kind == GraphEntityKind::Vertex {
                    &vertices
                } else {
                    &edges
                };
                selected.binary_search_by_key(&id, |entry| entry.0).is_ok()
            };
            read.fence_definition(batch, graph)?;
            batch.put(&single_str_key(TAG_NAMED_GRAPH, graph)?, &[])?;
            read.delete_graph_memberships_into(batch, graph, |kind, id| match kind {
                "vertex" => keep(GraphEntityKind::Vertex, id),
                "edge" => keep(GraphEntityKind::Edge, id),
                _ => false,
            })?;
            for &(id, slot) in vertices.iter() {
                let row = &snapshot.vertices[slot];
                read.save_vertex(batch, id, &row.label, &row.properties_json)?;
                read.membership(
                    batch,
                    "vertex",
                    id,
                    graph,
                    true,
                    Some(
                        crate::catalog::graph_observations::GraphEntityTopology::Vertex {
                            label: &row.label,
                        },
                    ),
                )?;
            }
            for &(_, slot) in edges.iter() {
                let row = &snapshot.edges[slot];
                read.save_edge(batch, row)?;
                read.membership(
                    batch,
                    "edge",
                    row.edge_id,
                    graph,
                    true,
                    Some(
                        crate::catalog::graph_observations::GraphEntityTopology::Edge {
                            label: &row.label,
                            source: row.source_id,
                            target: row.target_id,
                        },
                    ),
                )?;
            }
            batch.put(
                &single_str_key(TAG_METADATA, &format!("graph_label_registry::{graph}"))?,
                &string_value(&snapshot.label_registry_json),
            )?;
            read.observe_registry(batch, graph, &snapshot.label_registry_json)?;
            read.purge(batch, Some(graph), keep)?;
            read.remove_paths(batch, graph)
        })
    }

    pub(super) fn drop_named_graph_data_impl(&self, graph: &str) -> StorageBackendResult<()> {
        self.with_graph_mutation(|read, batch| {
            read.fence_definition(batch, graph)?;
            batch.delete(&single_str_key(TAG_NAMED_GRAPH, graph)?)?;
            read.delete_graph_memberships_into(batch, graph, |_, _| false)?;
            batch.delete(&single_str_key(
                TAG_METADATA,
                &format!("graph_label_registry::{graph}"),
            )?)?;
            read.purge(batch, Some(graph), |_, _| false)?;
            read.remove_paths(batch, graph)
        })
    }
}

impl GraphRead<'_> {
    fn save_vertex(
        &self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let row = StoredVertex {
            label: label.to_owned(),
            properties_json: properties_json.to_owned(),
        };
        self.replace_vertex_lookup(batch, id, Some(&row))?;
        self.observe_entity(batch, GraphEntityKind::Vertex, id)?;
        batch.put(&vertex_key(id), &encode_value(&row)?)
    }

    fn save_edge(&self, batch: &mut dyn KeyValueBatch, edge: &EdgeRow) -> StorageBackendResult<()> {
        let row = StoredEdge {
            source_id: edge.source_id,
            target_id: edge.target_id,
            label: edge.label.clone(),
            properties_json: edge.properties_json.clone(),
        };
        self.replace_edge_lookup(batch, edge.edge_id, Some(&row))?;
        self.observe_entity(batch, GraphEntityKind::Edge, edge.edge_id)?;
        batch.put(&edge_key(edge.edge_id), &encode_value(&row)?)
    }

    fn membership(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: &str,
        id: u64,
        graph: &str,
        present: bool,
        evaluated: Option<crate::catalog::graph_observations::GraphEntityTopology<'_>>,
    ) -> StorageBackendResult<()> {
        self.guard_definition(batch, Some(graph))?;
        let forward = graph_membership_key(kind, id, graph)?;
        let reverse = reverse_membership_key(kind, id, graph)?;
        if present {
            let entity_kind = match kind {
                "vertex" => Some(GraphEntityKind::Vertex),
                "edge" => Some(GraphEntityKind::Edge),
                _ => None,
            };
            if let Some(entity_kind) = entity_kind {
                self.guard_entity_reference(batch, entity_kind, id, None)?;
                if entity_kind == GraphEntityKind::Edge {
                    if let Some(edge) = self.edge(id)? {
                        for endpoint in [edge.source_id, edge.target_id] {
                            self.guard_entity_reference(
                                batch,
                                GraphEntityKind::Vertex,
                                endpoint,
                                Some(graph),
                            )?;
                        }
                    }
                }
            }
        } else {
            self.fence_membership_references(batch, kind, id, graph)?;
        }
        if self.read.get(&forward)?.is_some() == present {
            return Ok(());
        }
        self.observe_membership_change(batch, kind, id, graph, evaluated)?;
        if present {
            batch.put(&forward, &[])?;
            batch.put(&reverse, &[])?;
        } else {
            batch.delete(&forward)?;
            batch.delete(&reverse)?;
        }
        KeyValueCatalog::invalidate_graph_path_data(self.read, batch, graph)
    }

    fn final_rows<T>(
        &self,
        rows: &[T],
        id: impl Fn(&T) -> u64,
    ) -> StorageBackendResult<BudgetedVec<(u64, usize)>> {
        let mut order = BudgetedVec::new(self.read.control().memory());
        for (slot, row) in rows.iter().enumerate() {
            self.read.control().check()?;
            order.push((id(row), slot))?;
        }
        order.sort_unstable();
        let mut length = 0;
        for read in 0..order.len() {
            if read + 1 == order.len() || order[read].0 != order[read + 1].0 {
                order[length] = order[read];
                length += 1;
            }
        }
        order.truncate(length);
        Ok(order)
    }

    fn purge(
        &self,
        batch: &mut dyn KeyValueBatch,
        removed_graph: Option<&str>,
        keep: impl Fn(GraphEntityKind, u64) -> bool,
    ) -> StorageBackendResult<()> {
        for (kind, tag) in [
            (GraphEntityKind::Vertex, TAG_VERTEX),
            (GraphEntityKind::Edge, TAG_EDGE),
        ] {
            for_each_key(self.read, &[tag], &mut |key| {
                let id = read_u64(key, &mut 1)?;
                if !keep(kind, id)
                    && !self
                        .memberships(kind, id)?
                        .iter()
                        .any(|graph| Some(graph.as_str()) != removed_graph)
                {
                    match kind {
                        GraphEntityKind::Vertex => self.replace_vertex_lookup(batch, id, None)?,
                        GraphEntityKind::Edge => self.replace_edge_lookup(batch, id, None)?,
                    }
                    batch.delete(key)?;
                }
                Ok(true)
            })?;
        }
        Ok(())
    }

    fn remove_paths(&self, batch: &mut dyn KeyValueBatch, graph: &str) -> StorageBackendResult<()> {
        for_each_key(self.read, &[TAG_PATH_INDEX], &mut |key| {
            let name = read_str(key, &mut 1)?;
            if name
                .strip_prefix(graph)
                .is_some_and(|suffix| suffix.starts_with("::"))
            {
                KeyValueCatalog::clear_path_index_data_into(self.read, batch, &name)?;
                batch.delete(key)?;
            }
            Ok(true)
        })
    }

    pub(super) fn snapshot(&self, graph: &str) -> StorageBackendResult<Option<GraphSnapshot>> {
        let exists = self
            .read
            .get(&single_str_key(TAG_NAMED_GRAPH, graph)?)?
            .is_some();
        let registry = self.read.get(&single_str_key(
            TAG_METADATA,
            &format!("graph_label_registry::{graph}"),
        )?)?;
        let mut snapshot = GraphSnapshot {
            label_registry_json: registry
                .map(|bytes| String::from_utf8(bytes.to_vec()))
                .transpose()
                .map_err(|error| super::StorageBackendError::Other(error.to_string()))?
                .unwrap_or_default(),
            vertices: Vec::new(),
            edges: Vec::new(),
        };
        let prefix = super::graph_membership_graph_prefix(graph)?;
        for_each_key(self.read, &prefix, &mut |key| {
            let mut offset = prefix.len();
            let kind = read_str(key, &mut offset)?;
            let id = read_u64(key, &mut offset)?;
            let missing = || {
                super::StorageBackendError::Other(format!(
                    "graph {graph:?} references missing {kind} {id}"
                ))
            };
            if !exists {
                return Err(super::StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph}`"
                )));
            }
            match kind.as_str() {
                "vertex" => snapshot
                    .vertices
                    .push(self.vertex(id)?.ok_or_else(missing)?),
                "edge" => snapshot.edges.push(self.edge(id)?.ok_or_else(missing)?),
                _ => {
                    return Err(super::StorageBackendError::Other(format!(
                        "graph `{graph}` has invalid membership type `{kind}`"
                    )))
                }
            }
            Ok(true)
        })?;
        Ok(exists.then_some(snapshot))
    }
}
