//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded graph cursors over durable label, adjacency, and membership keys.

use super::graph_access::{
    edge_lookup_keys, endpoint_prefix, identity_key, label_prefix, membership_prefix,
    reverse_membership_key, vertex_lookup_keys, INDEX_VERSION,
};
use super::{
    decode_value, edge_key, graph_membership_graph_prefix, graph_membership_key, key_with_tag,
    push_str, read_str, read_u64, single_str_key, vertex_key, EdgeRow, KeyValueBatch,
    KeyValueCatalog, StorageBackendError, StorageBackendResult, StoredEdge, StoredVertex, TAG_EDGE,
    TAG_METADATA, TAG_VERTEX,
};
use crate::catalog::GraphEntitySelector;
use crate::key_value::KeyValueRead;
use crate::{GraphEntityFilter, GraphEntityKind, GraphVertexRow};
use uqa_core::memory::BudgetedVec;

pub(super) struct GraphRead<'a> {
    pub(super) read: &'a dyn KeyValueRead,
    pub(super) identifiers: bool,
}

pub(super) fn graph_lookup_indexes_ready(read: &dyn KeyValueRead) -> StorageBackendResult<bool> {
    match read.get(&single_str_key(TAG_METADATA, INDEX_VERSION)?)? {
        Some(version) if &*version == b"1" => Ok(true),
        None => Ok(false),
        Some(_) => Err(StorageBackendError::Other(
            "unsupported graph lookup index version".into(),
        )),
    }
}

impl GraphRead<'_> {
    fn keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<BudgetedVec<BudgetedVec<u8>>> {
        let mut keys = BudgetedVec::new(self.read.control().memory());
        self.read
            .visit_keys_after(prefix, after, limit, self.read.control(), &mut |key| {
                let mut owned = BudgetedVec::new(self.read.control().memory());
                owned.extend_from_slice(key)?;
                keys.push(owned)?;
                Ok(())
            })?;
        Ok(keys)
    }

    fn contains(&self, key: &[u8]) -> StorageBackendResult<bool> {
        let mut found = false;
        self.read
            .visit_keys_after(key, None, 1, self.read.control(), &mut |candidate| {
                found = candidate == key;
                Ok(())
            })?;
        Ok(found)
    }
    fn require_graph_lookup_indexes(&self) -> StorageBackendResult<()> {
        if graph_lookup_indexes_ready(self.read)? {
            return Ok(());
        }
        for prefix in [
            key_with_tag(TAG_VERTEX),
            key_with_tag(TAG_EDGE),
            super::graph_membership_prefix(),
        ] {
            if !self.keys(&prefix, None, 1)?.is_empty() {
                return Err(StorageBackendError::Other(
                    "graph lookup indexes require an explicit catalog migration".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn delete_graph_memberships_into(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
    ) -> StorageBackendResult<()> {
        KeyValueCatalog::invalidate_graph_path_data(self.read, batch, graph)?;
        let prefix = graph_membership_graph_prefix(graph)?;
        let mut after = None;
        loop {
            let keys = self.keys(&prefix, after.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            after = keys.last().map(|key| key.to_vec());
            for key in keys.iter() {
                let mut offset = prefix.len();
                let kind = read_str(key, &mut offset)?;
                let id = read_u64(key, &mut offset)?;
                batch.delete(&reverse_membership_key(&kind, id, graph)?)?;
            }
        }
        batch.delete_prefix(&prefix)
    }

    pub(super) fn vertex(&self, id: u64) -> StorageBackendResult<Option<GraphVertexRow>> {
        self.read
            .get(&vertex_key(id))?
            .map(|value| {
                let row: StoredVertex = decode_value(&value)?;
                Ok(GraphVertexRow {
                    vertex_id: id,
                    label: row.label,
                    properties_json: row.properties_json,
                })
            })
            .transpose()
    }

    pub(super) fn edge(&self, id: u64) -> StorageBackendResult<Option<EdgeRow>> {
        self.read
            .get(&edge_key(id))?
            .map(|value| {
                let row: StoredEdge = decode_value(&value)?;
                Ok(EdgeRow {
                    edge_id: id,
                    source_id: row.source_id,
                    target_id: row.target_id,
                    label: row.label,
                    properties_json: row.properties_json,
                })
            })
            .transpose()
    }

    pub(super) fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> StorageBackendResult<Vec<u64>> {
        let selector = filter.selector()?;
        crate::catalog::validate_graph_page(limit)?;
        if filter.source.is_some() || filter.target.is_some() || filter.label.is_some() {
            self.require_graph_lookup_indexes()?;
        }
        let prefix = match selector {
            GraphEntitySelector::Source(source) => endpoint_prefix(true, source),
            GraphEntitySelector::Target(target) => endpoint_prefix(false, target),
            GraphEntitySelector::Label(label) => label_prefix(filter.kind, label)?,
            GraphEntitySelector::Graph(graph) => {
                let mut key = graph_membership_graph_prefix(graph)?;
                push_str(&mut key, filter.kind.as_str())?;
                key
            }
            GraphEntitySelector::All => key_with_tag(if filter.kind == GraphEntityKind::Vertex {
                TAG_VERTEX
            } else {
                TAG_EDGE
            }),
        };
        let mut cursor = after.map(|id| identity_key(prefix.clone(), id));
        let mut result = Vec::new();
        loop {
            let keys = self.keys(&prefix, cursor.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            cursor = keys.last().map(|key| key.to_vec());
            for key in keys.iter() {
                let mut offset = prefix.len();
                let id = read_u64(key, &mut offset)?;
                if let Some(graph) = filter.graph {
                    if !self.contains(&graph_membership_key(filter.kind.as_str(), id, graph)?)? {
                        continue;
                    }
                }
                let matches = match filter.kind {
                    GraphEntityKind::Vertex => {
                        if filter.label.is_none() {
                            self.contains(&vertex_key(id))?
                        } else {
                            self.vertex(id)?.is_some_and(|row| {
                                filter.label.is_none_or(|label| row.label == label)
                            })
                        }
                    }
                    GraphEntityKind::Edge => {
                        if filter.label.is_none()
                            && filter.source.is_none()
                            && filter.target.is_none()
                        {
                            self.contains(&edge_key(id))?
                        } else {
                            self.edge(id)?.is_some_and(|row| {
                                filter.label.is_none_or(|label| row.label == label)
                                    && filter.source.is_none_or(|source| row.source_id == source)
                                    && filter.target.is_none_or(|target| row.target_id == target)
                            })
                        }
                    }
                };
                if !matches {
                    // A surviving index/membership must never conceal an absent entity.
                    let exists = self.contains(&if filter.kind == GraphEntityKind::Vertex {
                        vertex_key(id)
                    } else {
                        edge_key(id)
                    })?;
                    if !exists {
                        return Err(StorageBackendError::Other(format!(
                            "graph lookup references missing {} {id}",
                            filter.kind.as_str()
                        )));
                    }
                    continue;
                }
                result.push(id);
                if result.len() == limit {
                    return Ok(result);
                }
            }
        }
        Ok(result)
    }

    pub(super) fn memberships(
        &self,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<Vec<String>> {
        self.require_graph_lookup_indexes()?;
        let prefix = membership_prefix(kind.as_str(), id)?;
        let mut after = None;
        let mut graphs = Vec::new();
        loop {
            let keys = self.keys(&prefix, after.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            after = keys.last().map(|key| key.to_vec());
            for key in keys.iter() {
                let mut offset = prefix.len();
                graphs.push(read_str(key, &mut offset)?);
            }
        }
        graphs.sort();
        Ok(graphs)
    }

    pub(super) fn replace_vertex_lookup(
        &self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
        row: Option<&StoredVertex>,
    ) -> StorageBackendResult<()> {
        self.guard_definition(batch, None)?;
        batch.graph_mutation(crate::mvcc::GraphMutation::InvalidateEntity(
            GraphEntityKind::Vertex,
            id,
        ))?;
        for graph in self.memberships(GraphEntityKind::Vertex, id)? {
            self.guard_definition(batch, Some(&graph))?;
            KeyValueCatalog::invalidate_graph_path_data(self.read, batch, &graph)?;
        }
        if let Some(old) = self.read.get(&vertex_key(id))? {
            for key in vertex_lookup_keys(id, &decode_value(&old)?)? {
                batch.delete(&key)?;
            }
        }
        if let Some(row) = row {
            for key in vertex_lookup_keys(id, row)? {
                batch.put(&key, &[])?;
            }
        }
        Ok(())
    }

    pub(super) fn replace_edge_lookup(
        &self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
        row: Option<&StoredEdge>,
    ) -> StorageBackendResult<()> {
        self.guard_definition(batch, None)?;
        batch.graph_mutation(crate::mvcc::GraphMutation::InvalidateEntity(
            GraphEntityKind::Edge,
            id,
        ))?;
        for graph in self.memberships(GraphEntityKind::Edge, id)? {
            self.guard_definition(batch, Some(&graph))?;
            KeyValueCatalog::invalidate_graph_path_data(self.read, batch, &graph)?;
        }
        if let Some(old) = self.read.get(&edge_key(id))? {
            for key in edge_lookup_keys(id, &decode_value(&old)?)? {
                batch.delete(&key)?;
            }
        }
        if let Some(row) = row {
            for key in edge_lookup_keys(id, row)? {
                batch.put(&key, &[])?;
            }
        }
        Ok(())
    }
}
